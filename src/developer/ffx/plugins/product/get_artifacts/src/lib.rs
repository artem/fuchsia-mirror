// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for the paths of a group of artifacts inside product bundle.
use assembly_manifest::Image;
use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use ffx_config::EnvironmentContext;
use ffx_product_get_artifacts_args::GetArtifactsCommand;
use fho::{bug, return_user_error, Error, FfxMain, FfxTool, Result, VerifiedMachineWriter};
use schemars::JsonSchema;
use sdk_metadata::{ProductBundle, Type, VirtualDeviceManifest};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utf8_path::path_relative_from;

/// CommandStatus is returned to indicate exit status of
/// a command. The Ok variant returns the list of artifacts.
#[derive(Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { paths: Vec<String> },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}
#[derive(FfxTool)]
pub struct PbGetArtifactsTool {
    #[command]
    pub cmd: GetArtifactsCommand,
    env: EnvironmentContext,
}

fho::embedded_plugin!(PbGetArtifactsTool);

#[async_trait(?Send)]
impl FfxMain for PbGetArtifactsTool {
    type Writer = VerifiedMachineWriter<CommandStatus>;

    async fn main(mut self, mut writer: Self::Writer) -> fho::Result<()> {
        // Set the product bundle path from config if it was not passed in.
        if self.cmd.product_bundle.is_none() {
            if let Some(default_path) = self
                .env
                .query("product.path")
                .get()
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
        match self.pb_get_artifacts().await {
            Ok(artifacts) => writer
                .machine_or(&CommandStatus::Ok { paths: artifacts.clone() }, artifacts.join("\n"))
                .map_err(Into::into),
            Err(Error::User(e)) => {
                writer
                    .machine(&CommandStatus::UserError { message: e.to_string() })
                    .map_err(Into::<Error>::into)?;
                Err(Error::User(e))
            }
            Err(err) => {
                writer
                    .machine(&CommandStatus::UnexpectedError { message: err.to_string() })
                    .map_err(Into::<Error>::into)?;
                Err(err)
            }
        }
    }
}

impl PbGetArtifactsTool {
    /// This plugin will get the paths of a group of artifacts from
    /// the product bundle. This group can be used for flashing,
    /// emulator or updating the device depends on the
    /// artifact_group parameter passed in.
    pub async fn pb_get_artifacts(&self) -> Result<Vec<String>> {
        // It is expected that self.cmd.product_bundle has been validated to be some() by this point.
        let product_bundle =
            ProductBundle::try_load_from(&self.cmd.product_bundle.as_ref().unwrap())
                .map_err(Into::<fho::Error>::into)?;

        let artifacts = match self.cmd.artifacts_group {
            Type::Flash => self.extract_flashing_artifacts(product_bundle)?,
            Type::Bootloader => self.extract_bootloaders(product_bundle)?,
            Type::Emu => self.extract_emu_artifacts(product_bundle)?,
            Type::None => return_user_error!("invalid --artifact-group value specified."),
            _ => return_user_error!(
                "{:?} artifacts is not supported as of now",
                self.cmd.artifacts_group
            ),
        };
        Ok(artifacts)
    }

    fn compute_path(&self, artifact_path: &Utf8Path) -> Result<Utf8PathBuf> {
        if self.cmd.relative_path {
            path_relative_from(artifact_path, &self.cmd.product_bundle.as_ref().unwrap())
                .map_err(Into::into)
        } else {
            Ok(artifact_path.into())
        }
    }

    /// Extract bootloaders will list all the bootloaders used by this product. It
    /// will list the bootloader type along with the path. Example output:
    ///
    ///   firmware_b12:path/to/bootloader
    ///   firmware:path/to/bootloader2
    ///
    fn extract_bootloaders(&self, product_bundle: ProductBundle) -> Result<Vec<String>> {
        let mut product_bundle = match product_bundle {
            ProductBundle::V2(pb) => pb,
        };

        let mut artifacts = Vec::new();
        for part in &mut product_bundle.partitions.bootloader_partitions {
            let path = self.compute_path(&part.image)?;
            let bootloader_string = if part.partition_type == "" {
                format!("firmware:{}", &path)
            } else {
                format!("firmware_{}:{}", part.partition_type, &path)
            };

            artifacts.push(bootloader_string);
        }
        Ok(artifacts)
    }

    fn extract_flashing_artifacts(&self, product_bundle: ProductBundle) -> Result<Vec<String>> {
        let mut product_bundle = match product_bundle {
            ProductBundle::V2(pb) => pb,
        };

        let mut artifacts = vec![Utf8PathBuf::from("product_bundle.json")];
        for part in &mut product_bundle.partitions.bootstrap_partitions {
            artifacts.push(self.compute_path(&part.image)?);
        }
        for part in &mut product_bundle.partitions.bootloader_partitions {
            artifacts.push(self.compute_path(&part.image)?);
        }
        for cred in &mut product_bundle.partitions.unlock_credentials {
            artifacts.push(self.compute_path(&cred)?);
        }

        // Collect the systems artifacts.
        let mut collect_system_artifacts = |system: &mut Option<Vec<Image>>| -> Result<()> {
            if let Some(system) = system {
                for image in system.iter_mut() {
                    match image {
                        Image::ZBI { path: _, signed: _ }
                        | Image::VBMeta(_)
                        | Image::FVMFastboot(_)
                        | Image::Fxfs { path: _, contents: _ }
                        | Image::FxfsSparse { path: _, contents: _ } => {
                            artifacts.push(self.compute_path(&image.source())?)
                        }
                        _ => continue,
                    }
                }
            }
            Ok(())
        };
        collect_system_artifacts(&mut product_bundle.system_a)?;
        collect_system_artifacts(&mut product_bundle.system_b)?;
        collect_system_artifacts(&mut product_bundle.system_r)?;
        Ok(artifacts.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    }

    fn extract_emu_artifacts(&self, product_bundle: ProductBundle) -> Result<Vec<String>> {
        let mut product_bundle = match product_bundle {
            ProductBundle::V2(pb) => pb,
        };

        let mut artifacts = vec![Utf8PathBuf::from("product_bundle.json")];

        // Collect the systems artifacts.
        let mut collect_system_artifacts = |system: &mut Option<Vec<Image>>| -> Result<()> {
            if let Some(system) = system {
                for image in system.iter_mut() {
                    match image {
                        Image::ZBI { path: _, signed: _ }
                        | Image::QemuKernel(_)
                        | Image::FVM(_)
                        | Image::Fxfs { path: _, contents: _ }
                        | Image::FxfsSparse { path: _, contents: _ } => {
                            artifacts.push(self.compute_path(&image.source())?)
                        }
                        _ => continue,
                    }
                }
            }
            Ok(())
        };

        collect_system_artifacts(&mut product_bundle.system_a)?;
        collect_system_artifacts(&mut product_bundle.system_b)?;
        collect_system_artifacts(&mut product_bundle.system_r)?;

        if let Some(path) = product_bundle.virtual_devices_path.clone() {
            artifacts.push(self.compute_path(&path)?);

            // Also append the virtual device paths mentioned in manifest
            let virtual_device =
                VirtualDeviceManifest::from_path(&product_bundle.virtual_devices_path)?;
            let devices = virtual_device.device_paths.values().cloned();
            for device in devices {
                artifacts.push(
                    self.compute_path(
                        &path
                            .parent()
                            .ok_or(bug!(format!("Path does not have a parent: {}", &path)))?
                            .join(&device),
                    )?,
                );
            }
        }
        Ok(artifacts.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_partitions_config::PartitionsConfig;
    use ffx_config::ConfigLevel;
    use fho::{Format, TestBuffers};
    use sdk_metadata::ProductBundleV2;
    use std::{
        fs::{self, File},
        io::Write,
    };
    use tempfile::TempDir;

    const VIRTUAL_DEVICE_VALID: &str =
        include_str!("../../../../../../../build/sdk/meta/test_data/single_vd_manifest.json");

    #[fuchsia::test]
    async fn test_get_flashing_artifacts() {
        let env = ffx_config::test_init().await.expect("test env");
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "tpl",
                        name: "firmware_tpl",
                        image: "bootloader/path",
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
                Image::FVMFastboot(Utf8PathBuf::from(
                    "/tmp/product_bundle/system_a/fvm_fastboot.blk",
                )),
                Image::Fxfs {
                    path: Utf8PathBuf::from("/tmp/product_bundle/system_a/fxfs.blk"),
                    contents: Default::default(),
                },
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let tool = PbGetArtifactsTool {
            cmd: GetArtifactsCommand {
                product_bundle: None,
                relative_path: false,
                artifacts_group: Type::Flash,
            },
            env: env.context.clone(),
        };
        let artifacts = tool.extract_flashing_artifacts(pb.clone()).unwrap();
        let expected_artifacts = vec![
            String::from("product_bundle.json"),
            String::from("bootloader/path"),
            String::from("credential/path"),
            String::from("zbi/path"),
            String::from("/tmp/product_bundle/system_a/fvm_fastboot.blk"),
            String::from("/tmp/product_bundle/system_a/fxfs.blk"),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }

    #[fuchsia::test]
    async fn test_get_emu_artifacts() {
        let env = ffx_config::test_init().await.expect("test env");
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "tpl",
                        name: "firmware_tpl",
                        image: "bootloader/path",
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

        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap().canonicalize_utf8().unwrap();

        let virtual_device = tempdir.join("manifest.json");
        let mut vd_file1 = File::create(&virtual_device).unwrap();
        vd_file1.write_all(VIRTUAL_DEVICE_VALID.as_bytes()).unwrap();

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("/tmp/product_bundle/system_a/fvm.blk")),
                Image::FVMFastboot(Utf8PathBuf::from(
                    "/tmp/product_bundle/system_a/fvm_fastboot.blk",
                )),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
                Image::Fxfs {
                    path: Utf8PathBuf::from("/tmp/product_bundle/system_a/fxfs.blk"),
                    contents: Default::default(),
                },
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some(virtual_device.clone()),
        });
        let tool = PbGetArtifactsTool {
            cmd: GetArtifactsCommand {
                product_bundle: None,
                relative_path: false,
                artifacts_group: Type::Emu,
            },
            env: env.context.clone(),
        };
        let artifacts = tool.extract_emu_artifacts(pb.clone()).unwrap();
        let expected_artifacts = vec![
            String::from("product_bundle.json"),
            String::from("zbi/path"),
            String::from("/tmp/product_bundle/system_a/fvm.blk"),
            String::from("qemu/path"),
            String::from("/tmp/product_bundle/system_a/fxfs.blk"),
            virtual_device.to_string(),
            tempdir.join("device.json").to_string(),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }

    #[fuchsia::test]
    async fn test_get_bootloaders() {
        let env = ffx_config::test_init().await.expect("test env");
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "",
                        name: "firmware_tpl",
                        image: "bootloader/path",
                    },
                    {
                        type: "bl2",
                        name: "firmware_tpl",
                        image: "bootloader/path2",
                    }
                ],
                partitions: [],
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
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let tool = PbGetArtifactsTool {
            cmd: GetArtifactsCommand {
                product_bundle: None,
                relative_path: false,
                artifacts_group: Type::Bootloader,
            },
            env: env.context.clone(),
        };
        let artifacts = tool.extract_bootloaders(pb.clone()).unwrap();
        let expected_artifacts = vec![
            String::from("firmware:bootloader/path"),
            String::from("firmware_bl2:bootloader/path2"),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }

    #[fuchsia::test]
    async fn test_get_emu_artifacts_machine() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb_path = env.isolate_root.path().join("test_bundle");
        fs::create_dir_all(&pb_path).expect("create test bundle dir");

        let virtual_device = pb_path.join("manifest.json");
        let mut vd_file1 = File::create(&virtual_device).expect("virtual_device manifest");
        vd_file1
            .write_all(VIRTUAL_DEVICE_VALID.as_bytes())
            .expect("virtual device contents written");

        // write out some files so that canonicalizing does not error out.
        let zbi_path = pb_path.join("fuchsia.zbi");
        fs::write(&zbi_path, vec![0x20, 0x10]).expect("zbi write");
        let fvm_path = pb_path.join("fvm.blk");
        fs::write(&fvm_path, vec![0x20, 0x10]).expect("zbi write");
        let fvm_fast_path = pb_path.join("fastboot_fvm.blk");
        fs::write(&fvm_fast_path, vec![0x20, 0x10]).expect("zbi write");
        let qemu_path = pb_path.join("qemu-boot.bin");
        fs::write(&qemu_path, vec![0x20, 0x10]).expect("zbi write");
        let fxfs_path = pb_path.join("fxfs.blk");
        fs::write(&fxfs_path, vec![0x20, 0x10]).expect("zbi write");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(zbi_path.clone()).expect("utf8 path"),
                    signed: false,
                },
                Image::FVM(Utf8PathBuf::from_path_buf(fvm_path.clone()).expect("utf8 path")),
                Image::FVMFastboot(
                    Utf8PathBuf::from_path_buf(fvm_fast_path.clone()).expect("utf8 path"),
                ),
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(qemu_path.clone()).expect("utf8 path"),
                ),
                Image::Fxfs {
                    path: Utf8PathBuf::from_path_buf(fxfs_path.clone()).expect("utf8 path"),
                    contents: Default::default(),
                },
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some(
                Utf8Path::from_path(&virtual_device).expect("utf8 path").into(),
            ),
        });
        pb.write(Utf8Path::from_path(&pb_path).expect("temp dir to utf8 path"))
            .expect("temp test product bundle");

        let tool = PbGetArtifactsTool {
            cmd: GetArtifactsCommand {
                product_bundle: Some(Utf8Path::from_path(&pb_path).expect("utf8 path").into()),
                relative_path: false,
                artifacts_group: Type::Emu,
            },
            env: env.context.clone(),
        };

        let test_buffers = TestBuffers::default();
        let writer: <PbGetArtifactsTool as FfxMain>::Writer =
            <PbGetArtifactsTool as FfxMain>::Writer::new_test(
                Some(Format::JsonPretty),
                &test_buffers,
            );

        let result = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(result.is_ok(), "Expect result to be ok: {stdout} {stderr}");

        let got: CommandStatus = match serde_json::from_str(&stdout) {
            Ok(c) => c,
            Err(e) => panic!("Error parsing result: {e} {stdout}"),
        };
        let want = CommandStatus::Ok {
            paths: vec![
                "product_bundle.json".into(),
                zbi_path.to_string_lossy().into(),
                fvm_path.to_string_lossy().into(),
                qemu_path.to_string_lossy().into(),
                fxfs_path.to_string_lossy().into(),
                virtual_device.to_string_lossy().into(),
                pb_path.join("device.json").to_string_lossy().into(),
            ],
        };

        assert_eq!(got, want);
        let data: serde_json::Value = serde_json::from_str(&stdout).expect("serde value");
        match <PbGetArtifactsTool as FfxMain>::Writer::verify_schema(&data) {
            Ok(_) => (),
            Err(e) => {
                assert_eq!("", format!("Error verifying schema: {e:?}\n{data:?}"));
            }
        };
    }

    #[fuchsia::test]
    async fn test_get_use_default_bundle() {
        let env = ffx_config::test_init().await.expect("test env");
        let pb_path = env.isolate_root.path().join("test_bundle");
        fs::create_dir_all(&pb_path).expect("create test bundle dir");

        env.context
            .query("product.path")
            .level(Some(ConfigLevel::User))
            .set(pb_path.to_string_lossy().into())
            .await
            .expect("set pb path");

        let virtual_device = pb_path.join("manifest.json");
        let mut vd_file1 = File::create(&virtual_device).expect("virtual_device manifest");
        vd_file1
            .write_all(VIRTUAL_DEVICE_VALID.as_bytes())
            .expect("virtual device contents written");

        // write out some files so that canonicalizing does not error out.
        let zbi_path = pb_path.join("fuchsia.zbi");
        fs::write(&zbi_path, vec![0x20, 0x10]).expect("zbi write");
        let fvm_path = pb_path.join("fvm.blk");
        fs::write(&fvm_path, vec![0x20, 0x10]).expect("zbi write");
        let fvm_fast_path = pb_path.join("fastboot_fvm.blk");
        fs::write(&fvm_fast_path, vec![0x20, 0x10]).expect("zbi write");
        let qemu_path = pb_path.join("qemu-boot.bin");
        fs::write(&qemu_path, vec![0x20, 0x10]).expect("zbi write");
        let fxfs_path = pb_path.join("fxfs.blk");
        fs::write(&fxfs_path, vec![0x20, 0x10]).expect("zbi write");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI {
                    path: Utf8PathBuf::from_path_buf(zbi_path.clone()).expect("utf8 path"),
                    signed: false,
                },
                Image::FVM(Utf8PathBuf::from_path_buf(fvm_path.clone()).expect("utf8 path")),
                Image::FVMFastboot(
                    Utf8PathBuf::from_path_buf(fvm_fast_path.clone()).expect("utf8 path"),
                ),
                Image::QemuKernel(
                    Utf8PathBuf::from_path_buf(qemu_path.clone()).expect("utf8 path"),
                ),
                Image::Fxfs {
                    path: Utf8PathBuf::from_path_buf(fxfs_path.clone()).expect("utf8 path"),
                    contents: Default::default(),
                },
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some(
                Utf8Path::from_path(&virtual_device).expect("utf8 path").into(),
            ),
        });
        pb.write(Utf8Path::from_path(&pb_path).expect("temp dir to utf8 path"))
            .expect("temp test product bundle");

        let tool = PbGetArtifactsTool {
            cmd: GetArtifactsCommand {
                product_bundle: None,
                relative_path: false,
                artifacts_group: Type::Emu,
            },
            env: env.context.clone(),
        };

        let test_buffers = TestBuffers::default();
        let writer: <PbGetArtifactsTool as FfxMain>::Writer =
            <PbGetArtifactsTool as FfxMain>::Writer::new_test(
                Some(Format::JsonPretty),
                &test_buffers,
            );

        let result = tool.main(writer).await;
        let (stdout, stderr) = test_buffers.into_strings();
        assert!(result.is_ok(), "Expect result to be ok: {stdout} {stderr}");

        let got: CommandStatus = match serde_json::from_str(&stdout) {
            Ok(c) => c,
            Err(e) => panic!("Error parsing result: {e} {stdout}"),
        };
        let want = CommandStatus::Ok {
            paths: vec![
                "product_bundle.json".into(),
                zbi_path.to_string_lossy().into(),
                fvm_path.to_string_lossy().into(),
                qemu_path.to_string_lossy().into(),
                fxfs_path.to_string_lossy().into(),
                virtual_device.to_string_lossy().into(),
                pb_path.join("device.json").to_string_lossy().into(),
            ],
        };

        assert_eq!(got, want);
    }
}
