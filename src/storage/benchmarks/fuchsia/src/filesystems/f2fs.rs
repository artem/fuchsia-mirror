// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::filesystems::FsManagementFilesystemInstance,
    async_trait::async_trait,
    storage_benchmarks::{BlockDeviceConfig, BlockDeviceFactory, FilesystemConfig},
};

/// Config object for starting F2fs instances.
#[derive(Clone)]
pub struct F2fs;

#[async_trait]
impl FilesystemConfig for F2fs {
    type Filesystem = FsManagementFilesystemInstance;

    async fn start_filesystem(
        &self,
        block_device_factory: &dyn BlockDeviceFactory,
    ) -> FsManagementFilesystemInstance {
        let block_device = block_device_factory
            .create_block_device(&BlockDeviceConfig {
                use_zxcrypt: true,
                // f2fs requires a minimum of 100MiB volume for fsync test
                fvm_volume_size: Some(100 * 1024 * 1024),
            })
            .await;
        FsManagementFilesystemInstance::new(
            fs_management::F2fs::default(),
            block_device,
            /*as_blob=*/ false,
        )
        .await
    }

    fn name(&self) -> String {
        "f2fs".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use {super::F2fs, crate::filesystems::testing::check_filesystem};

    #[fuchsia::test]
    async fn start_f2fs() {
        check_filesystem(F2fs).await;
    }
}
