// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::storage_config::StorageConfig;
use assembly_images_config::{
    BlobfsLayout, DataFilesystemFormat, DataFvmVolumeConfig, FilesystemImageMode, FvmVolumeConfig,
    VolumeConfig,
};

pub(crate) struct StorageSubsystemConfig;
impl DefineSubsystemConfiguration<StorageConfig> for StorageSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        storage_config: &StorageConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if storage_config.live_usb_enabled {
            builder.platform_bundle("live_usb");
        } else {
            builder.platform_bundle("empty_live_usb");
        }

        if storage_config.configure_fshost {
            // Collect the arguments from the board.
            let blobfs_max_bytes =
                context.board_info.filesystems.fvm.blobfs.maximum_bytes.unwrap_or(0);
            let blobfs_initial_inodes =
                context.board_info.filesystems.fvm.blobfs.minimum_inodes.unwrap_or(0);
            let data_max_bytes =
                context.board_info.filesystems.fvm.minfs.maximum_bytes.unwrap_or(0);
            let fvm_slice_size = context.board_info.filesystems.fvm.slice_size.0;
            let gpt_all = context.board_info.filesystems.gpt_all;

            // Collect the arguments from the product.
            let ramdisk_image =
                storage_config.filesystems.image_mode == FilesystemImageMode::Ramdisk;
            let no_zxcrypt = storage_config.filesystems.no_zxcrypt;
            let format_data_on_corruption = storage_config.filesystems.format_data_on_corruption.0;
            let nand = storage_config.filesystems.watch_for_nand;

            // Prepare some default arguments that may get overriden by the product config.
            let mut blob_deprecated_padded = false;
            let mut use_disk_migration = false;
            let mut data_filesystem_format_str = "fxfs";
            let mut fxfs_blob = false;

            // Add all the AIBs and collect some argument values.
            builder.platform_bundle("fshost_common");
            builder.platform_bundle("fshost_storage");
            match &storage_config.filesystems.volume {
                VolumeConfig::Fxfs => {
                    builder.platform_bundle("fshost_fxfs");
                    fxfs_blob = true;
                }
                VolumeConfig::Fvm(FvmVolumeConfig { blob, data, .. }) => {
                    if let Some(blob) = blob {
                        builder.platform_bundle("fshost_fvm_blobfs");
                        blob_deprecated_padded = blob.blob_layout == BlobfsLayout::DeprecatedPadded;
                    }
                    if let Some(DataFvmVolumeConfig {
                        use_disk_based_minfs_migration,
                        data_filesystem_format,
                    }) = data
                    {
                        match data_filesystem_format {
                            DataFilesystemFormat::Fxfs => {
                                builder.platform_bundle("fshost_fvm_fxfs")
                            }
                            DataFilesystemFormat::F2fs => {
                                data_filesystem_format_str = "f2fs";
                                builder.platform_bundle("fshost_fvm_f2fs");
                            }
                            DataFilesystemFormat::Minfs => {
                                data_filesystem_format_str = "minfs";
                                if *use_disk_based_minfs_migration {
                                    use_disk_migration = true;
                                    builder.platform_bundle("fshost_fvm_minfs_migration");
                                } else {
                                    builder.platform_bundle("fshost_fvm_minfs");
                                }
                            }
                        }
                    }
                }
            }

            let mut fshost_config_builder = builder.bootfs().component("meta/fshost.cm")?;
            fshost_config_builder
                .field("blobfs", true)?
                .field("blobfs_allow_delivery_blobs", true)?
                .field("blobfs_max_bytes", blobfs_max_bytes)?
                .field("bootpart", true)?
                .field("check_filesystems", true)?
                .field("data", true)?
                .field("data_max_bytes", data_max_bytes)?
                .field("disable_block_watcher", false)?
                .field("factory", false)?
                .field("fvm", true)?
                .field("ramdisk_image", ramdisk_image)?
                .field("gpt", true)?
                .field("gpt_all", gpt_all)?
                .field("mbr", false)?
                .field("netboot", false)?
                .field("no_zxcrypt", no_zxcrypt)?
                .field("format_data_on_corruption", format_data_on_corruption)?
                .field("blobfs_initial_inodes", blobfs_initial_inodes)?
                .field("blobfs_use_deprecated_padded_format", blob_deprecated_padded)?
                .field("use_disk_migration", use_disk_migration)?
                .field("nand", nand)?
                .field("fxfs_blob", fxfs_blob)?
                .field("fvm_slice_size", fvm_slice_size)?;

            fshost_config_builder.field("data_filesystem_format", data_filesystem_format_str)?;
        }

        Ok(())
    }
}
