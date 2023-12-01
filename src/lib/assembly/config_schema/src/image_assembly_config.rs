// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_images_config::ImagesConfig;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::common::FileEntry;

/// The set of information that defines a fuchsia product.  This is capable of
/// being a complete configuration (it at least has a kernel).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageAssemblyConfig {
    /// The packages whose files get added to the base package. The
    /// packages themselves are not added, but their individual files are
    /// extracted and added to the base package. These files are needed
    /// to bootstrap pkgfs.
    #[serde(default)]
    pub system: Vec<Utf8PathBuf>,

    /// The packages that are in the base package list, excluding drivers
    /// which are added to the base package (data/static_packages).
    /// These packages get updated by flashing and OTAing, and cannot
    /// be garbage collected.
    #[serde(default)]
    pub base: Vec<Utf8PathBuf>,

    /// The packages that are in the cache package list, which is added
    /// to the base package (data/cache_packages). These packages get
    /// updated by flashing and OTAing, but can be garbage collected.
    #[serde(default)]
    pub cache: Vec<Utf8PathBuf>,

    /// The parameters that specify which kernel to put into the ZBI.
    pub kernel: KernelConfig,

    /// The qemu kernel to use when starting the emulator.
    pub qemu_kernel: Utf8PathBuf,

    /// The list of additional boot args to add.
    #[serde(default)]
    pub boot_args: Vec<String>,

    /// The set of files to be placed in BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_files: Vec<FileEntry>,

    /// The packages that are in the bootfs package list, which are
    /// added to the BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_packages: Vec<Utf8PathBuf>,

    /// Which images to produce and how.
    #[serde(default)]
    pub images_config: ImagesConfig,
}

impl ImageAssemblyConfig {
    /// Helper function for constructing a ImageAssemblyConfig in tests in this
    /// and other modules within the crate.
    pub fn new_for_testing(kernel_path: impl AsRef<Utf8Path>, clock_backstop: u64) -> Self {
        Self {
            system: Vec::default(),
            base: Vec::default(),
            cache: Vec::default(),
            boot_args: Vec::default(),
            bootfs_files: Vec::default(),
            bootfs_packages: Vec::default(),
            kernel: KernelConfig {
                path: kernel_path.as_ref().into(),
                args: Vec::default(),
                clock_backstop,
            },
            qemu_kernel: "path/to/qemu/kernel".into(),
            images_config: ImagesConfig::default(),
        }
    }
}

/// The information required to specify a kernel and its arguments, all optional
/// to allow for the partial specification
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialKernelConfig {
    /// The path to the prebuilt kernel.
    pub path: Option<Utf8PathBuf>,

    /// The list of command line arguments to pass to the kernel on startup.
    #[serde(default)]
    pub args: Vec<String>,

    /// The backstop UTC time for the clock.
    pub clock_backstop: Option<u64>,
}

/// The information required to specify a kernel and its arguments.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KernelConfig {
    /// The path to the prebuilt kernel.
    pub path: Utf8PathBuf,

    /// The list of command line arguments to pass to the kernel on startup.
    #[serde(default)]
    pub args: Vec<String>,

    /// The backstop UTC time for the clock.
    /// This is kept separate from the `args` to make it clear that this is a required argument.
    pub clock_backstop: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use assembly_util::from_reader;

    fn from_json_str<T>(json: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut cursor = std::io::Cursor::new(json);
        from_reader(&mut cursor)
    }

    #[test]
    fn product_from_json_file() {
        let json = r#"
            {
              "system": ["package0"],
              "base": ["package1", "package2"],
              "cache": ["package3", "package4"],
              "kernel": {
                "path": "path/to/kernel",
                "args": ["arg1", "arg2"],
                "clock_backstop": 0
              },
              "qemu_kernel": "path/to/qemu/kernel",
              "bootfs_files": [
                {
                    "source": "path/to/source",
                    "destination": "path/to/destination"
                }
              ],
              "bootfs_packages": ["package5", "package6"]
            }
        "#;

        let config: ImageAssemblyConfig = from_json_str(json).unwrap();
        assert_eq!(
            config.kernel,
            KernelConfig {
                path: "path/to/kernel".into(),
                args: vec!["arg1".into(), "arg2".into()],
                clock_backstop: 0
            }
        );
    }

    #[test]
    fn product_from_json5_file() {
        let json = r#"
            {
              // json5 files can have comments in them.
              system: ["package0"],
              base: ["package1", "package2"],
              cache: ["package3", "package4"],
              kernel: {
                path: "path/to/kernel",
                args: ["arg1", "arg2"],
                clock_backstop: 0,
              },
              qemu_kernel: "path/to/qemu/kernel",
              // and lists can have trailing commas
              boot_args: ["arg1", "arg2", ],
              bootfs_files: [
                {
                    source: "path/to/source",
                    destination: "path/to/destination",
                }
              ],
              bootfs_packages: ["package5", "package6"],
            }
        "#;

        let config: ImageAssemblyConfig = from_json_str(json).unwrap();
        assert_eq!(
            config.kernel,
            KernelConfig {
                path: "path/to/kernel".into(),
                args: vec!["arg1".into(), "arg2".into()],
                clock_backstop: 0
            }
        );
    }

    #[test]
    fn product_from_invalid_json_file() {
        let json = r#"
            {
                "invalid": "data"
            }
        "#;

        let config: Result<ImageAssemblyConfig> = from_json_str(json);
        assert!(config.is_err());
    }
}
