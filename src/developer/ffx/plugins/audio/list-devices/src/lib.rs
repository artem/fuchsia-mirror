// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_audio_common::ffxtool::exposed_dir;
use ffx_audio_device::{
    device_list_untagged,
    list::{get_devices, ListResult},
};
use ffx_audio_listdevices_args::ListDevicesCommand;
use fho::{FfxMain, FfxTool, MachineWriter};
use fidl_fuchsia_io as fio;

#[derive(FfxTool)]
pub struct ListDevicesTool {
    #[command]
    _cmd: ListDevicesCommand,
    #[with(exposed_dir("/bootstrap/devfs", "dev-class"))]
    dev_class: fio::DirectoryProxy,
}

fho::embedded_plugin!(ListDevicesTool);
#[async_trait(?Send)]
impl FfxMain for ListDevicesTool {
    type Writer = MachineWriter<ListResult>;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        let selectors = get_devices(&self.dev_class).await?;
        device_list_untagged(selectors, writer)
    }
}
