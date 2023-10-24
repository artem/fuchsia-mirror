// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Duration;
use errors::ffx_bail;
use ffx_fastboot::common::{
    cmd::OemFile,
    fastboot::{tcp_proxy, udp_proxy, usb_proxy},
    fastboot_interface::FastbootInterface,
    from_manifest,
};
use ffx_flash_args::FlashCommand;
use ffx_ssh::SshKeyFiles;
use fho::FfxContext;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{
    FastbootInterface as FidlFastbootInterface, TargetProxy, TargetRebootState,
};
use fidl_fuchsia_developer_ffx::{FastbootProxy, TargetState};
use fuchsia_async::Timer;
use std::io::Write;
use std::net::SocketAddr;
use termion::{color, style};

const SSH_OEM_COMMAND: &str = "add-staged-bootloader-file ssh.authorized_keys";

#[derive(FfxTool)]
pub struct FlashTool {
    #[command]
    cmd: FlashCommand,
    fastboot_proxy: fho::Deferred<FastbootProxy>,
    target_proxy: fho::Deferred<TargetProxy>,
}

fho::embedded_plugin!(FlashTool);

#[async_trait(?Send)]
impl FfxMain for FlashTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        // Checks
        preflight_checks(&self.cmd, &mut writer)?;

        // Massage FlashCommand
        let cmd = preprocess_flash_cmd(self.cmd).await?;

        if cmd.daemon {
            flash_plugin_with_daemon(self.fastboot_proxy.await?, cmd, &mut writer).await
        } else {
            flash_plugin_no_daemon(self.target_proxy.await?, cmd, &mut writer).await
        }
    }
}

fn preflight_checks<W: Write>(cmd: &FlashCommand, mut writer: W) -> Result<()> {
    if cmd.manifest_path.is_some() {
        // TODO(fxb/125854)
        writeln!(writer, "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (fxb/125854)", color::Fg(color::Red), style::Reset)
.with_context(||"writing warning to users")
.map_err(fho::Error::from)?;
    }
    if cmd.manifest_path.is_some() && cmd.manifest.is_some() {
        ffx_bail!("Error: the manifest must be specified either by positional argument or the --manifest flag")
    }
    Ok(())
}

async fn preprocess_flash_cmd(mut cmd: FlashCommand) -> Result<FlashCommand> {
    match cmd.authorized_keys.as_ref() {
        Some(ssh) => {
            let ssh_file = match std::fs::canonicalize(ssh) {
                Ok(path) => path,
                Err(err) => {
                    ffx_bail!("Cannot find SSH key \"{}\": {}", ssh, err);
                }
            };
            if cmd.oem_stage.iter().any(|f| f.command() == SSH_OEM_COMMAND) {
                ffx_bail!("Both the SSH key and the SSH OEM Stage flags were set. Only use one.");
            }
            cmd.oem_stage.push(OemFile::new(
                SSH_OEM_COMMAND.to_string(),
                ssh_file
                    .into_os_string()
                    .into_string()
                    .map_err(|s| anyhow!("Cannot convert OsString \"{:?}\" to String", s))?,
            ));
        }
        None => {
            if !cmd.oem_stage.iter().any(|f| f.command() == SSH_OEM_COMMAND) {
                let ssh_keys =
                    SshKeyFiles::load().await.context("finding ssh authorized_keys file.")?;
                ssh_keys.create_keys_if_needed().context("creating ssh keys if needed")?;
                if ssh_keys.authorized_keys.exists() {
                    let k = ssh_keys.authorized_keys.display().to_string();
                    eprintln!("No `--authorized-keys` flag, using {}", k);
                    cmd.oem_stage.push(OemFile::new(SSH_OEM_COMMAND.to_string(), k));
                } else {
                    // Since the key will be initialized, this should never happen.
                    ffx_bail!("Warning: flashing without a SSH key is not advised.");
                }
            }
        }
    };
    Ok(cmd)
}

#[tracing::instrument(skip(fastboot_proxy, writer))]
async fn flash_plugin_with_daemon<W: Write>(
    mut fastboot_proxy: impl FastbootInterface,
    cmd: FlashCommand,
    mut writer: W,
) -> fho::Result<()> {
    from_manifest(&mut writer, cmd, &mut fastboot_proxy).await.map_err(fho::Error::from)
}

#[tracing::instrument(skip(target_proxy, writer))]
async fn flash_plugin_no_daemon<W: Write>(
    target_proxy: TargetProxy,
    cmd: FlashCommand,
    mut writer: W,
) -> fho::Result<()> {
    let mut info = target_proxy.identity().await.user_message("Error getting Target's identity")?;

    match info.target_state {
        Some(TargetState::Fastboot) => {
            // Nothing to do
        }
        Some(_) => {
            // Tell the target to reboot to the bootloader
            target_proxy
                .reboot(TargetRebootState::Bootloader)
                .await
                .user_message("Got error rebooting")?
                .map_err(|e| anyhow!("Got error rebooting target: {:#?}", e))
                .user_message("Got an error rebooting")?;

            // Sleep for 10 seconds to let the target reboot to bootloader
            write!(writer, "Waiting for 10 seconds while the target reboots to bootloader...")
                .user_message("Error writing to buffer")?;

            Timer::new(
                Duration::seconds(10)
                    .to_std()
                    .user_message("Error converting 10 second duration to standard duration")?,
            )
            .await;
            // Get the info again since the target changed state
            info = target_proxy.identity().await.user_message("Error getting Target's identity")?;

            if !matches!(info.target_state, Some(TargetState::Fastboot)) {
                ffx_bail!("Target was requested to reboot to the bootloader, but was found in {:#?} state", info.target_state)
            }
        }
        None => {
            ffx_bail!("Target had an unknown, non-existant state")
        }
    };

    match info.fastboot_interface {
        None => ffx_bail!("Unknown fastboot interface state"),
        Some(FidlFastbootInterface::Usb) => {
            let serial_num = info.serial_number.ok_or_else(|| {
                anyhow!("Target was detected in Fastboot USB but did not have a serial number")
            })?;
            let mut proxy = usb_proxy(serial_num).await?;
            from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
        }
        Some(FidlFastbootInterface::Udp) => {
            // We take the first address as when a target is in Fastboot mode and over
            // UDP it only exposes one address
            if let Some(addr) = info.addresses.unwrap().into_iter().take(1).next() {
                let target_addr: TargetAddr = addr.into();
                let socket_addr: SocketAddr = target_addr.into();
                let mut proxy = udp_proxy(&socket_addr).await?;
                from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
            } else {
                ffx_bail!("Could not get a valid address for target");
            }
        }
        Some(FidlFastbootInterface::Tcp) => {
            // We take the first address as when a target is in Fastboot mode and over
            // TCP it only exposes one address
            if let Some(addr) = info.addresses.unwrap().into_iter().take(1).next() {
                let target_addr: TargetAddr = addr.into();
                let socket_addr: SocketAddr = target_addr.into();
                let mut proxy = tcp_proxy(&socket_addr).await?;
                from_manifest(&mut writer, cmd, &mut proxy).await.map_err(fho::Error::from)
            } else {
                ffx_bail!("Could not get a valid address for target");
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::{default::Default, path::PathBuf};
    use tempfile::NamedTempFile;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_file_throws_err() {
        assert!(preprocess_flash_cmd(FlashCommand {
            manifest_path: Some(PathBuf::from("ffx_test_does_not_exist")),
            ..Default::default()
        })
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_nonexistent_ssh_file_throws_err() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        assert!(preprocess_flash_cmd(FlashCommand {
            manifest_path: Some(PathBuf::from(tmp_file_name)),
            authorized_keys: Some("ssh_does_not_exist".to_string()),
            ..Default::default()
        },)
        .await
        .is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_specify_manifest_twice_throws_error() {
        let tmp_file = NamedTempFile::new().expect("tmp access failed");
        let tmp_file_name = tmp_file.path().to_string_lossy().to_string();
        let mut w = Vec::new();
        assert!(preflight_checks(
            &FlashCommand {
                manifest: Some(PathBuf::from(tmp_file_name.clone())),
                manifest_path: Some(PathBuf::from(tmp_file_name)),
                ..Default::default()
            },
            &mut w
        )
        .is_err());
        // Additionally, check that the warning was printed
        assert_eq!(
            std::str::from_utf8(&w).expect("UTF8 String"),
            format!(
                "{}WARNING:{} specifying the flash manifest via a positional argument is deprecated. Use the --manifest flag instead (fxb/125854)\n",
                color::Fg(color::Red),
                style::Reset
        ));
    }
}
