// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    async_trait::async_trait,
    errors::ffx_error,
    ffx_profile_heapdump_common::{check_snapshot_error, connect_to_collector, export_to_pprof},
    ffx_profile_heapdump_download_args::DownloadCommand,
    fho::{AvailabilityFlag, FfxMain, FfxTool, SimpleWriter},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fidl_fuchsia_memory_heapdump_client as fheapdump_client,
};

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_heapdump"))]
pub struct DownloadTool {
    #[command]
    cmd: DownloadCommand,
    remote_control: RemoteControlProxy,
}

fho::embedded_plugin!(DownloadTool);

#[async_trait(?Send)]
impl FfxMain for DownloadTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        download(self.remote_control, self.cmd).await?;
        Ok(())
    }
}

async fn download(remote_control: RemoteControlProxy, cmd: DownloadCommand) -> Result<()> {
    let (receiver_client, receiver_stream) = create_request_stream()?;
    let request = fheapdump_client::CollectorDownloadStoredSnapshotRequest {
        snapshot_id: Some(cmd.snapshot_id),
        receiver: Some(receiver_client),
        ..Default::default()
    };

    let collector = connect_to_collector(&remote_control, cmd.collector).await?;
    collector.download_stored_snapshot(request)?;
    let snapshot =
        check_snapshot_error(heapdump_snapshot::Snapshot::receive_from(receiver_stream).await)?;

    export_to_pprof(
        &snapshot,
        &mut std::fs::File::create(&cmd.output_file).map_err(|err| {
            ffx_error!("Failed to create output file: {}: {}", cmd.output_file, err)
        })?,
        cmd.with_tags,
    )?;

    Ok(())
}
