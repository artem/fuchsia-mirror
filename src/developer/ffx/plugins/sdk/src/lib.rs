// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use camino::Utf8Path;
use ffx_config::{ConfigLevel, EnvironmentContext};
use ffx_sdk_args::{RunCommand, SdkCommand, SetCommand, SetRootCommand, SetSubCommand, SubCommand};
use fho::{exit_with_code, user_error, FfxContext, FfxMain, FfxTool, SimpleWriter};
use sdk::{in_tree_sdk_version, metadata::ElementType, Sdk, SdkRoot, SdkVersion};
use std::io::{ErrorKind, Write};

#[derive(FfxTool)]
pub struct SdkTool {
    context: EnvironmentContext,
    sdk_root: fho::Result<SdkRoot>,
    #[command]
    cmd: SdkCommand,
}

fho::embedded_plugin!(SdkTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for SdkTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        match &self.cmd.sub {
            SubCommand::Version(_) => {
                exec_version(self.sdk_root?.get_sdk()?, writer).await.map_err(Into::into)
            }
            SubCommand::Set(cmd) => exec_set(self.context, cmd).await.map_err(Into::into),
            SubCommand::Run(cmd) => exec_run(self.sdk_root?.get_sdk()?, cmd).await,
            SubCommand::PopulatePath(cmd) => exec_populate_path(writer, self.sdk_root?, &cmd.path),
        }
    }
}

async fn exec_run(sdk: Sdk, cmd: &RunCommand) -> fho::Result<()> {
    let status = sdk
        .get_host_tool_command(&cmd.name)
        .with_user_message(|| {
            format!("Could not find the host tool `{}` in the currently active sdk", cmd.name)
        })?
        .args(&cmd.args)
        .status()
        .map_err(|e| {
            user_error!(
                "Failed to spawn host tool `{}` from the sdk with system error: {e}",
                cmd.name
            )
        })?;

    if status.success() {
        Ok(())
    } else {
        exit_with_code!(status.code().unwrap_or(1))
    }
}

async fn exec_version<W: Write>(sdk: Sdk, mut writer: W) -> Result<()> {
    match sdk.get_version() {
        SdkVersion::Version(v) => writeln!(writer, "{}", v)?,
        SdkVersion::InTree => writeln!(writer, "{}", in_tree_sdk_version())?,
        SdkVersion::Unknown => writeln!(writer, "<unknown>")?,
    }

    Ok(())
}

async fn exec_set(context: EnvironmentContext, cmd: &SetCommand) -> Result<()> {
    match &cmd.sub {
        SetSubCommand::Root(SetRootCommand { path }) => {
            let abs_path =
                path.canonicalize().with_context(|| format!("making path absolute: {:?}", path))?;
            context
                .query("sdk.root")
                .level(Some(ConfigLevel::User))
                .set(abs_path.to_string_lossy().into())
                .await?;
            Ok(())
        }
    }
}

fn exec_populate_path(
    mut writer: impl Write,
    sdk_root: SdkRoot,
    bin_path: &Utf8Path,
) -> fho::Result<()> {
    let inner_bin_path = bin_path.join("fuchsia-sdk");
    let full_fuchsia_sdk_run_path = inner_bin_path.join("fuchsia-sdk-run");
    tracing::debug!("Installing host tool stubs to {bin_path:?} (and `fuchsia-sdk-run` to {inner_bin_path:?}) from SDK {sdk_root:?}");

    let sdk = sdk_root.get_sdk()?;
    let sdk_run_tool = sdk.get_host_tool("fuchsia-sdk-run").user_message("SDK does not contain `fuchsia-sdk-run` host tool. You may need to update your SDK to use this command.")?;
    tracing::debug!("Found `fuchsia-sdk-run` in the SDK at {sdk_run_tool:?}");

    std::fs::create_dir_all(&inner_bin_path).with_user_message(|| {
        format!("Could not create {inner_bin_path:?}. Do you have write access to {bin_path:?}?")
    })?;
    std::fs::copy(&sdk_run_tool, &full_fuchsia_sdk_run_path).with_user_message(|| format!("Could not install `fuchsia-sdk-run` tool to {inner_bin_path:?}. Do you have write access to {bin_path:?}"))?;

    writeln!(writer, "Installing host tool stubs to {bin_path:?}").bug()?;
    for tool in sdk.get_all_host_tools_metadata() {
        let tool_name = &tool.name;
        if tool.kind != ElementType::HostTool && tool_name != "fuchsia-sdk-run" {
            tracing::trace!("Skipping companion tool (or other kind of tool) {tool_name}");
        }

        writeln!(writer, "Installing {tool_name}").bug()?;
        match std::os::unix::fs::symlink("fuchsia-sdk/fuchsia-sdk-run", bin_path.join(tool_name)) {
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            other => other.with_user_message(|| {
                format!("Could not create symlink for `{tool_name}` in {bin_path:?}")
            })?,
        }
    }

    writeln!(writer, "\n\
        All tools installed in {bin_path:?}. Add the following to your environment to use project local configuration to run SDK host tools:\n\
        \n\
        PATH={bin_path}:$PATH").bug()?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_with_string() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::Version("Test.0".to_owned()));

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!("Test.0\n", out);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_in_tree() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::InTree);

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        let re = regex::Regex::new(r"^\d+.99991231.0.1\n$").expect("creating regex");
        assert!(re.is_match(&out));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_unknown() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::Unknown);

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!("<unknown>\n", out);
    }
}
