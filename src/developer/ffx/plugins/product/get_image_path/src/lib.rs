// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for get a path of the image inside product bundle.
use assembly_manifest::Image;
use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use ffx_config::EnvironmentContext;
use ffx_product_get_image_path_args::{GetImagePathCommand, ImageType, Slot};
use fho::{
    bug, return_user_error, user_error, Error, FfxMain, FfxTool, Result, VerifiedMachineWriter,
};
use schemars::JsonSchema;
use sdk_metadata::ProductBundle;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utf8_path::path_relative_from;

/// CommandStatus is returned to indicate exit status of
/// a command. The Ok variant returns the list of artifacts.
#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { path: String },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

/// This plugin will get the path of image from the product bundle, based on the slot and image_type passed in.
#[derive(FfxTool)]
pub struct PbGetImagePathTool {
    #[command]
    pub cmd: GetImagePathCommand,
    env: EnvironmentContext,
}

fho::embedded_plugin!(PbGetImagePathTool);

#[async_trait(?Send)]
impl FfxMain for PbGetImagePathTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;

    async fn main(mut self, mut writer: Self::Writer) -> fho::Result<()> {
        // Set the product bundle path from config if it was not passed in.
        if self.cmd.product_bundle.is_none() {
            if let Some(default_path) = self
                .env
                .query("product.path")
                .get()
                .await
                .map(|p: PathBuf| p.into())
                .map_err(|e| bug!(e))?
            {
                let pb_path: Utf8PathBuf =
                    Utf8PathBuf::try_from(default_path).map_err(|e| bug!(e))?;
                self.cmd.product_bundle = Some(pb_path);
            } else {
                let message = String::from("no product bundle specified nor configured.");
                writer
                    .machine(&CommandStatus::UserError { message: message.clone() })
                    .map_err(Into::<Error>::into)?;
                return_user_error!(message);
            }
        }
        match self.pb_get_image_path().await {
            Ok(Some(image_path)) => writer
                .machine_or(
                    &CommandStatus::Ok { path: image_path.to_string() },
                    image_path.to_string(),
                )
                .map_err(Into::into),
            Ok(None) => {
                let e = user_error!("No image found based on the specified options");
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(e)
            }
            Err(Error::User(e)) => {
                writer.machine(&CommandStatus::UserError { message: e.to_string() })?;
                Err(Error::User(e))
            }
            Err(err) => {
                writer.machine(&CommandStatus::UnexpectedError { message: err.to_string() })?;
                Err(err)
            }
        }
    }
}

impl PbGetImagePathTool {
    pub async fn pb_get_image_path(&self) -> Result<Option<Utf8PathBuf>> {
        // It is expected that self.cmd.product_bundle has been validated to be some() by this point.
        let product_bundle =
            ProductBundle::try_load_from(&self.cmd.product_bundle.as_ref().unwrap())
                .map_err(Into::<fho::Error>::into)?;
        self.extract_image_path(product_bundle)
    }

    fn compute_path(&self, artifact_path: &Utf8Path) -> Result<Utf8PathBuf> {
        if self.cmd.relative_path {
            path_relative_from(artifact_path, &self.cmd.product_bundle.as_ref().unwrap())
                .map_err(Into::into)
        } else {
            Ok(artifact_path.into())
        }
    }

    fn extract_image_path(&self, product_bundle: ProductBundle) -> Result<Option<Utf8PathBuf>> {
        let product_bundle = match product_bundle {
            ProductBundle::V2(pb) => pb,
        };

        if self.cmd.bootloader.is_some() && self.cmd.slot.is_some() {
            return_user_error!("Cannot pass in both --bootloader and --slot parameters");
        }

        if let Some(bootloader) = &self.cmd.bootloader {
            let bootloader_type = bootloader
                .strip_prefix("firmware")
                .ok_or(user_error!("Bootloader name must begin with 'firmware'"))?
                .trim_start_matches("_");
            return product_bundle
                .partitions
                .bootloader_partitions
                .iter()
                .find_map(|b| {
                    if b.partition_type == bootloader_type {
                        Some(self.compute_path(&b.image))
                    } else {
                        None
                    }
                })
                .transpose();
        }

        let system = match self.cmd.slot {
            Some(Slot::A) => product_bundle.system_a,
            Some(Slot::B) => product_bundle.system_b,
            Some(Slot::R) => product_bundle.system_r,
            _ => return_user_error!("No valid slot passed in {:#?}", self.cmd.slot),
        }
        .ok_or(user_error!("No image found in slot"))?;

        system
            .iter()
            .find_map(|i| match (i, self.cmd.image_type) {
                (Image::ZBI { path, .. }, Some(ImageType::Zbi))
                | (Image::VBMeta(path), Some(ImageType::VBMeta))
                | (Image::FVM(path), Some(ImageType::Fvm))
                | (Image::Fxfs { path, .. }, Some(ImageType::Fxfs))
                | (Image::QemuKernel(path), Some(ImageType::QemuKernel)) => {
                    Some(self.compute_path(path))
                }
                _ => None,
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_partitions_config::PartitionsConfig;
    use ffx_config::ConfigLevel;
    use fho::{Format, TestBuffers};
    use sdk_metadata::ProductBundleV2;
    use std::fs;

    #[fuchsia::test]
    async fn test_get_image_path() {
        let env = ffx_config::test_init().await.expect("test env");
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "bl2",
                        name: "bl2",
                        image: "/tmp/product_bundle/bootloader/path",
                    },
                    {
                        type: "",
                        name: "bootloader",
                        image: "/tmp/product_bundle/bootloader/empty/path",
                    }
                ],
                partitions: [
                    {
                        type: "ZBI",
                        name: "zircon_a",
                        slot: "A",
                    },
                    {
                        type: "VBMeta",
                        name: "vbmeta_b",
                        slot: "B",
                    },
                    {
                        type: "FVM",
                        name: "fvm",
                    },
                    {
                        type: "Fxfs",
                        name: "fxfs",
                    },
                ],
                hardware_revision: "hw",
                unlock_credentials: [
                    "credential/path",
                ],
            }
        "#;
        let mut cursor = std::io::Cursor::new(json);
        let config: PartitionsConfig = PartitionsConfig::from_reader(&mut cursor).unwrap();

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("/tmp/product_bundle/system_a/fvm.blk")),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });

        // Each test case is in its own scope
        {
            let tool = PbGetImagePathTool {
                cmd: GetImagePathCommand {
                    product_bundle: None,
                    slot: Some(Slot::A),
                    image_type: Some(ImageType::Zbi),
                    relative_path: false,
                    bootloader: None,
                },
                env: env.context.clone(),
            };
            let path = tool.extract_image_path(pb.clone()).unwrap().unwrap();
            let expected_path = Utf8PathBuf::from("zbi/path");
            assert_eq!(expected_path, path);
        }
        {
            let tool = PbGetImagePathTool {
                cmd: GetImagePathCommand {
                    product_bundle: None,
                    slot: Some(Slot::A),
                    image_type: Some(ImageType::QemuKernel),
                    relative_path: false,
                    bootloader: None,
                },
                env: env.context.clone(),
            };
            let path = tool.extract_image_path(pb.clone()).unwrap().unwrap();
            let expected_path = Utf8PathBuf::from("qemu/path");
            assert_eq!(expected_path, path);
        }
        {
            let tool = PbGetImagePathTool {
                cmd: GetImagePathCommand {
                    product_bundle: Some(Utf8PathBuf::from("/tmp/product_bundle")),
                    slot: Some(Slot::A),
                    image_type: Some(ImageType::Fvm),
                    relative_path: true,
                    bootloader: None,
                },
                env: env.context.clone(),
            };
            let path = tool.extract_image_path(pb.clone()).unwrap().unwrap();
            let expected_path = Utf8PathBuf::from("system_a/fvm.blk");
            assert_eq!(expected_path, path);
        }
        {
            let tool = PbGetImagePathTool {
                cmd: GetImagePathCommand {
                    product_bundle: Some(Utf8PathBuf::from("/tmp/product_bundle")),
                    slot: None,
                    image_type: None,
                    relative_path: true,
                    bootloader: Some(String::from("firmware_bl2")),
                },
                env: env.context.clone(),
            };
            let path = tool.extract_image_path(pb.clone()).unwrap().unwrap();
            let expected_path = Utf8PathBuf::from("bootloader/path");
            assert_eq!(expected_path, path);
        }
        {
            let tool = PbGetImagePathTool {
                cmd: GetImagePathCommand {
                    product_bundle: Some(Utf8PathBuf::from("/tmp/product_bundle")),
                    slot: None,
                    image_type: None,
                    relative_path: true,
                    bootloader: Some(String::from("firmware")),
                },
                env: env.context.clone(),
            };
            let path = tool.extract_image_path(pb.clone()).unwrap().unwrap();
            let expected_path = Utf8PathBuf::from("bootloader/empty/path");
            assert_eq!(expected_path, path);
        }
    }

    #[fuchsia::test]
    async fn test_get_image_path_not_found() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("fvm/path")),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let tool = PbGetImagePathTool {
            cmd: GetImagePathCommand {
                product_bundle: None,
                slot: Some(Slot::A),
                image_type: Some(ImageType::VBMeta),
                relative_path: false,
                bootloader: None,
            },
            env: env.context.clone(),
        };
        let path = tool.extract_image_path(pb).unwrap();
        assert_eq!(None, path);
    }

    #[fuchsia::test]
    async fn test_get_image_path_not_found_machine() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb_path = env.isolate_root.path().join("test_bundle");
        fs::create_dir_all(&pb_path).expect("create test bundle dir");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(pb_path.join("zbi/path")).expect("utf8 path"),
                    signed: false,
                },
                Image::FVM(
                    Utf8PathBuf::from_path_buf(pb_path.join("fvm/path")).expect("utf8 path"),
                ),
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(pb_path.join("qemu/path")).expect("utf8 path"),
                ),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(Utf8Path::from_path(&pb_path).expect("temp dir to utf8 path"))
            .expect("temp test product bundle");

        let tool = PbGetImagePathTool {
            cmd: GetImagePathCommand {
                product_bundle: Some(Utf8PathBuf::from_path_buf(pb_path).expect("utf8 path")),
                slot: Some(Slot::A),
                image_type: Some(ImageType::VBMeta),
                relative_path: false,
                bootloader: None,
            },
            env: env.context.clone(),
        };
        let test_buffers = TestBuffers::default();
        let writer: <PbGetImagePathTool as FfxMain>::Writer =
            <PbGetImagePathTool as FfxMain>::Writer::new_test(
                Some(Format::JsonPretty),
                &test_buffers,
            );

        let result = tool.main(writer).await;

        assert!(result.is_err(), "Expect result to be an error");
        let raw = test_buffers.into_stdout_str();

        let got: CommandStatus = serde_json::from_str(&raw).expect("parse output");
        let want = CommandStatus::UserError {
            message: "No image found based on the specified options".to_string(),
        };
        assert_eq!(got, want);
        let data: serde_json::Value = serde_json::from_str(&raw).expect("serde value");
        match <PbGetImagePathTool as FfxMain>::Writer::verify_schema(&data) {
            Ok(_) => (),
            Err(e) => {
                assert_eq!("", format!("Error verifying schema: {e:?}\n{data:?}"));
            }
        };
    }

    #[fuchsia::test]
    async fn test_get_image_path_machine() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb_path = env.isolate_root.path().join("test_bundle");
        fs::create_dir_all(&pb_path).expect("create test bundle dir");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(pb_path.join("zbi/path")).expect("utf8 path"),
                    signed: false,
                },
                Image::FVM(
                    Utf8PathBuf::from_path_buf(pb_path.join("fvm/path")).expect("utf8 path"),
                ),
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(pb_path.join("qemu/path")).expect("utf8 path"),
                ),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(Utf8Path::from_path(&pb_path).expect("temp dir to utf8 path"))
            .expect("temp test product bundle");

        let tool = PbGetImagePathTool {
            cmd: GetImagePathCommand {
                product_bundle: Some(Utf8Path::from_path(&pb_path).expect("utf8 path").into()),
                slot: Some(Slot::A),
                image_type: Some(ImageType::Zbi),
                relative_path: false,
                bootloader: None,
            },
            env: env.context.clone(),
        };
        let test_buffers = TestBuffers::default();
        let writer: <PbGetImagePathTool as FfxMain>::Writer =
            <PbGetImagePathTool as FfxMain>::Writer::new_test(
                Some(Format::JsonPretty),
                &test_buffers,
            );

        let result = tool.main(writer).await;

        assert!(result.is_ok(), "Expect result to be ok");
        let raw = test_buffers.into_stdout_str();

        let got: CommandStatus = serde_json::from_str(&raw).expect("parse output");
        let want =
            CommandStatus::Ok { path: pb_path.join("zbi/path").to_string_lossy().to_string() };
        assert_eq!(got, want);
        let data: serde_json::Value = serde_json::from_str(&raw).expect("serde value");
        match <PbGetImagePathTool as FfxMain>::Writer::verify_schema(&data) {
            Ok(_) => (),
            Err(e) => {
                assert_eq!("", format!("Error verifying schema: {e:?}\n{data:?}"));
            }
        }
    }

    #[fuchsia::test]
    async fn test_get_image_path_from_config() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb_path = env.isolate_root.path().join("test_bundle");
        fs::create_dir_all(&pb_path).expect("create test bundle dir");

        env.context
            .query("product.path")
            .level(Some(ConfigLevel::User))
            .set(pb_path.to_string_lossy().into())
            .await
            .expect("setting default path");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(pb_path.join("zbi/path")).expect("utf8 path"),
                    signed: false,
                },
                Image::FVM(
                    Utf8PathBuf::from_path_buf(pb_path.join("fvm/path")).expect("utf8 path"),
                ),
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(pb_path.join("qemu/path")).expect("utf8 path"),
                ),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(Utf8Path::from_path(&pb_path).expect("temp dir to utf8 path"))
            .expect("temp test product bundle");

        let tool = PbGetImagePathTool {
            cmd: GetImagePathCommand {
                product_bundle: None,
                slot: Some(Slot::A),
                image_type: Some(ImageType::Zbi),
                relative_path: false,
                bootloader: None,
            },
            env: env.context.clone(),
        };
        let test_buffers = TestBuffers::default();
        let writer: <PbGetImagePathTool as FfxMain>::Writer =
            <PbGetImagePathTool as FfxMain>::Writer::new_test(
                Some(Format::JsonPretty),
                &test_buffers,
            );

        let result = tool.main(writer).await;

        assert!(result.is_ok(), "Expect result to be ok: {}", test_buffers.into_stderr_str());
        let raw = test_buffers.into_stdout_str();

        let got: CommandStatus = serde_json::from_str(&raw).expect("parse output");
        let want =
            CommandStatus::Ok { path: pb_path.join("zbi/path").to_string_lossy().to_string() };
        assert_eq!(got, want);
    }
}
