// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::FsManagementFilesystemInstance,
    async_trait::async_trait,
    storage_benchmarks::{BlockDeviceConfig, BlockDeviceFactory, FilesystemConfig},
};

/// Config object for starting Minfs instances.
#[derive(Clone)]
pub struct Minfs;

#[async_trait]
impl FilesystemConfig for Minfs {
    type Filesystem = FsManagementFilesystemInstance;

    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> FsManagementFilesystemInstance {
        let block_device = block_device_factory
            .create_block_device(&BlockDeviceConfig { use_zxcrypt: true, fvm_volume_size: None })
            .await;
        FsManagementFilesystemInstance::new(
            fs_management::Minfs::default(),
            block_device,
            /*as_blob=*/ false,
        )
        .await
    }

    fn name(&self) -> String {
        "minfs".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use {super::Minfs, crate::filesystems::testing::check_filesystem};

    #[fuchsia::test]
    async fn start_minfs() {
        check_filesystem(Minfs).await;
    }
}
