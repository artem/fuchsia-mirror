// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_inspect_args::{InspectCommand, InspectSubCommand};
use fho::{deferred, toolbox_or, Deferred, FfxMain, FfxTool, MachineWriter, ToolIO};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_diagnostics_host::ArchiveAccessorProxy;
use iquery::commands::{Command, ListAccessorsResult, ListResult, SelectorsResult, ShowResult};
use serde::Serialize;
use std::{fmt, io::Write};

mod accessor_provider;
mod apply_selectors;
#[cfg(test)]
pub(crate) mod tests;

pub use accessor_provider::HostArchiveReader;

fho::embedded_plugin!(InspectTool);

#[derive(FfxTool)]
pub struct InspectTool {
    #[command]
    cmd: InspectCommand,
    #[with(deferred(toolbox_or("bootstrap/archivist")))]
    archive_accessor: Deferred<ArchiveAccessorProxy>,
    rcs: Deferred<RemoteControlProxy>,
}

#[async_trait(?Send)]
impl FfxMain for InspectTool {
    type Writer = MachineWriter<InspectOutput>;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let Self { rcs, archive_accessor, cmd } = self;
        let (rcs, archive_accessor) = match futures::future::join(rcs, archive_accessor).await {
            (Ok(rcs), Ok(archive_accessor)) => (rcs, archive_accessor),
            (rcs_res, accessor_res) => {
                let mut msg =
                    "Failed to connect to necessary Remote Control protocols.".to_string();
                if let Err(rcs_err) = rcs_res {
                    msg.push_str(&format!("\nRemoteControl: {rcs_err:?}"));
                }
                if let Err(accessor_err) = accessor_res {
                    msg.push_str(&format!("\nArchiveAccessor: {accessor_err:?}"));
                }
                ffx_bail!("{msg}");
            }
        };
        match cmd.sub_command {
            InspectSubCommand::ApplySelectors(cmd) => {
                apply_selectors::execute(rcs, archive_accessor, cmd).await?;
            }
            InspectSubCommand::Show(cmd) => {
                run_command(rcs, archive_accessor, cmd, &mut writer).await?;
            }
            InspectSubCommand::List(cmd) => {
                run_command(rcs, archive_accessor, cmd, &mut writer).await?;
            }
            InspectSubCommand::ListAccessors(cmd) => {
                run_command(rcs, archive_accessor, cmd, &mut writer).await?;
            }
            InspectSubCommand::Selectors(cmd) => {
                run_command(rcs, archive_accessor, cmd, &mut writer).await?;
            }
        }
        Ok(())
    }
}

pub(crate) async fn run_command<C, O>(
    rcs_proxy: RemoteControlProxy,
    diagnostics_proxy: ArchiveAccessorProxy,
    cmd: C,
    writer: &mut MachineWriter<InspectOutput>,
) -> anyhow::Result<()>
where
    C: Command<Result = O>,
    InspectOutput: From<O>,
{
    let provider = HostArchiveReader::new(diagnostics_proxy, rcs_proxy);
    let result = cmd.execute(&provider).await.map_err(|e| anyhow!(ffx_error!("{}", e)))?;
    let result = InspectOutput::from(result);
    if writer.is_machine() {
        writer.machine(&result)?;
    } else {
        writeln!(writer, "{}", result)?;
    }
    Ok(())
}

macro_rules! impl_inspect_output {
    ($($variant:tt),*) => {
        #[derive(Serialize)]
        #[serde(untagged)]
        pub enum InspectOutput {
            $($variant( $variant ),)*
        }

        $(
            impl From<$variant> for InspectOutput {
                fn from(item: $variant) -> Self {
                    Self::$variant(item)
                }
            }
        )*

        impl fmt::Display for InspectOutput {
             fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(
                        Self::$variant(value) => value.fmt(f),
                    )*
                }
            }
        }
    };
}

impl_inspect_output!(ShowResult, ListAccessorsResult, ListResult, SelectorsResult);
