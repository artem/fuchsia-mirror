// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::image_assembly_config::PartialKernelConfig;
use crate::PackageDetails;
use assembly_package_utils::PackageInternalPathBuf;
use assembly_util::{CompiledPackageDestination, FileEntry};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::common::{DriverDetails, PackageName};
use crate::platform_config::PlatformConfig;
use crate::product_config::ProductConfig;

/// Configuration for a Product Assembly operation.  This is a high-level operation
/// that takes a more abstract description of what is desired in the assembled
/// product images, and then generates the complete Image Assembly configuration
/// (`crate::config::ImageAssemblyConfig`) from that.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssemblyConfig {
    pub platform: PlatformConfig,
    pub product: ProductConfig,
}

/// A typename to represent a package that contains shell command binaries,
/// and the paths to those binaries
pub type ShellCommands = BTreeMap<PackageName, BTreeSet<PackageInternalPathBuf>>;

/// A bundle of inputs to be used in the assembly of a product.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AssemblyInputBundle {
    /// The parameters that specify which kernel to put into the ZBI.
    pub kernel: Option<PartialKernelConfig>,

    /// The qemu kernel to use when starting the emulator.
    #[serde(default)]
    pub qemu_kernel: Option<Utf8PathBuf>,

    /// The list of additional boot args to add.
    #[serde(default)]
    pub boot_args: Vec<String>,

    /// The packages that are in the bootfs package list, which are
    /// added to the BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_packages: Vec<Utf8PathBuf>,

    /// The set of files to be placed in BOOTFS in the ZBI.
    #[serde(default)]
    pub bootfs_files: Vec<FileEntry<String>>,

    /// Package entries that internally specify their package set, instead of being grouped
    /// separately.
    #[serde(default)]
    pub packages: Vec<PackageDetails>,

    /// Entries for the `config_data` package.
    #[serde(default)]
    pub config_data: BTreeMap<String, Vec<FileEntry<String>>>,

    /// The blobs index of the AIB.  This currently isn't used by product
    /// assembly, as the package manifests contain the same information.
    #[serde(default)]
    pub blobs: Vec<Utf8PathBuf>,

    /// Configuration of base driver packages. Driver packages should not be
    /// listed in the base package list and will be included automatically.
    #[serde(default)]
    pub base_drivers: Vec<DriverDetails>,

    /// Configuration of boot driver packages. Driver packages should not be
    /// listed in the bootfs package list and will be included automatically.
    #[serde(default)]
    pub boot_drivers: Vec<DriverDetails>,

    /// Map of the names of packages that contain shell commands to the list of
    /// commands within each.
    #[serde(default)]
    pub shell_commands: ShellCommands,

    /// Packages to create dynamically as part of the Assembly process.
    #[serde(default)]
    pub packages_to_compile: Vec<CompiledPackageDefinition>,

    /// A package that includes files to include in bootfs.
    #[serde(default)]
    pub bootfs_files_package: Option<Utf8PathBuf>,
}

/// Contents of a compiled package. The contents provided by all
/// selected AIBs are merged by `name` into a single package
/// at assembly time.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub enum CompiledPackageDefinition {
    MainDefinition(MainPackageDefinition),
    Additional(AdditionalPackageContents),
}

/// Primary definition of a compiled package. Only a single AIB should
/// contain the main definition for a given package.
#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct MainPackageDefinition {
    pub name: CompiledPackageDestination,
    /// Components to add to the package, mapping component name to
    /// the primary cml definition of the component.
    pub components: BTreeMap<String, Utf8PathBuf>,
    /// Non-component files to add to the package.
    #[serde(default)]
    pub contents: Vec<FileEntry<String>>,
    /// CML files included by the component cml.
    #[serde(default)]
    pub includes: Vec<Utf8PathBuf>,
    /// Whether the contents of this package should go into bootfs.
    /// Gated by allowlist -- please use this as a base package if possible.
    #[serde(default)]
    pub bootfs_package: bool,
}

/// Additional contents of the package to be defined in
/// secondary AssemblyInputBundles. There can be many of these, and
/// they will be merged into the final compiled package.
#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct AdditionalPackageContents {
    /// Name of the package to which these contents are associated.
    /// This package should have a corresponding MainDefinition in another
    /// AIB.
    pub name: CompiledPackageDestination,
    /// Additional component shards to combine with the primary manifest.
    pub component_shards: BTreeMap<String, Vec<Utf8PathBuf>>,
}

impl CompiledPackageDefinition {
    pub fn name(&self) -> &CompiledPackageDestination {
        match self {
            CompiledPackageDefinition::MainDefinition(MainPackageDefinition { name, .. }) => name,
            CompiledPackageDefinition::Additional(AdditionalPackageContents { name, .. }) => name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{FeatureControl, PackageSet};
    use crate::platform_config::{BuildType, FeatureSupportLevel};
    use crate::product_config::ProductPackageDetails;
    use assembly_file_relative_path::FileRelativePathBuf;
    use assembly_util as util;

    #[test]
    fn test_product_assembly_config_from_json5() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::Eng);
        assert_eq!(config.platform.feature_set_level, FeatureSupportLevel::Standard);
    }

    #[test]
    fn test_bringup_product_assembly_config_from_json5() {
        let json5 = r#"
        {
          platform: {
            feature_set_level: "bootstrap",
            build_type: "eng",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::Eng);
        assert_eq!(config.platform.feature_set_level, FeatureSupportLevel::Bootstrap);
    }

    #[test]
    fn test_minimal_product_assembly_config_from_json5() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::Eng);
        assert_eq!(config.platform.feature_set_level, FeatureSupportLevel::Standard);
    }

    #[test]
    fn test_empty_product_assembly_config_from_json5() {
        let json5 = r#"
        {
          platform: {
            feature_set_level: "empty",
            build_type: "eng",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::Eng);
        assert_eq!(config.platform.feature_set_level, FeatureSupportLevel::Empty);
    }

    #[test]
    fn test_buildtype_deserialization_userdebug() {
        let json5 = r#"
        {
          platform: {
            build_type: "userdebug",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::UserDebug);
    }

    #[test]
    fn test_buildtype_deserialization_user() {
        let json5 = r#"
        {
          platform: {
            build_type: "user",
          },
          product: {},
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::User);
    }

    #[test]
    fn test_product_assembly_config_with_product_provided_parts() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
            identity : {
                "password_pinweaver": "allowed",
            }
          },
          product: {
              packages: {
                  base: [
                      { manifest: "path/to/base/package_manifest.json" }
                  ],
                  cache: [
                      { manifest: "path/to/cache/package_manifest.json" }
                  ]
              },
              base_drivers: [
                {
                  package: "path/to/base/driver/package_manifest.json",
                  components: [ "meta/path/to/component.cml" ]
                }
              ]
          },
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.platform.build_type, BuildType::Eng);
        assert_eq!(
            config.product.packages.base,
            vec![ProductPackageDetails {
                manifest: "path/to/base/package_manifest.json".into(),
                config_data: Vec::default()
            }]
        );
        assert_eq!(
            config.product.packages.cache,
            vec![ProductPackageDetails {
                manifest: "path/to/cache/package_manifest.json".into(),
                config_data: Vec::default()
            }]
        );
        assert_eq!(config.platform.identity.password_pinweaver, FeatureControl::Allowed);
        assert_eq!(
            config.product.base_drivers,
            vec![DriverDetails {
                package: "path/to/base/driver/package_manifest.json".into(),
                components: vec!["meta/path/to/component.cml".into()]
            }]
        )
    }

    #[test]
    fn test_assembly_input_bundle_from_json5() {
        let json5 = r#"
            {
              // json5 files can have comments in them.
              packages: [
                {
                    package: "package5",
                    set: "base",
                },
                {
                    package: "package6",
                    set: "cache",
                },
              ],
              kernel: {
                path: "path/to/kernel",
                args: ["arg1", "arg2"],
              },
              // and lists can have trailing commas
              boot_args: ["arg1", "arg2", ],
              bootfs_files: [
                {
                  source: "path/to/source",
                  destination: "path/to/destination",
                }
              ],
              config_data: {
                "package1": [
                  {
                    source: "path/to/source.json",
                    destination: "config.json"
                  }
                ]
              },
              base_drivers: [
                {
                  package: "path/to/driver",
                  components: ["path/to/1234", "path/to/5678"]
                }
              ],
              shell_commands: {
                "package1": ["path/to/binary1", "path/to/binary2"]
              },
              packages_to_compile: [
                {
                    name: "core",
                    components: {
                        "component1": "path/to/component1.cml",
                        "component2": "path/to/component2.cml",
                    },
                    contents: [
                        {
                            source: "path/to/source",
                            destination: "path/to/destination",
                        }
                    ],
                    includes: [ "src/path/to/include.cml" ]
                },
                {
                   name: "core",
                   component_shards: {
                        "component1": [
                            "path/to/shard1.cml",
                            "path/to/shard2.cml"
                        ]
                   }
                }
              ]
            }
        "#;
        let bundle =
            util::from_reader::<_, AssemblyInputBundle>(&mut std::io::Cursor::new(json5)).unwrap();
        assert_eq!(
            bundle.packages,
            vec!(
                PackageDetails {
                    package: FileRelativePathBuf::FileRelative(Utf8PathBuf::from("package5")),
                    set: PackageSet::Base,
                },
                PackageDetails {
                    package: FileRelativePathBuf::FileRelative(Utf8PathBuf::from("package6")),
                    set: PackageSet::Cache,
                },
            )
        );
        let expected_kernel = PartialKernelConfig {
            path: Some(Utf8PathBuf::from("path/to/kernel")),
            args: vec!["arg1".to_string(), "arg2".to_string()],
        };
        assert_eq!(bundle.kernel, Some(expected_kernel));
        assert_eq!(bundle.boot_args, vec!("arg1".to_string(), "arg2".to_string()));
        assert_eq!(
            bundle.bootfs_files,
            vec!(FileEntry {
                source: Utf8PathBuf::from("path/to/source"),
                destination: "path/to/destination".to_string()
            })
        );
        assert_eq!(
            bundle.config_data.get("package1").unwrap(),
            &vec!(FileEntry {
                source: Utf8PathBuf::from("path/to/source.json"),
                destination: "config.json".to_string()
            })
        );
        assert_eq!(
            bundle.base_drivers[0],
            DriverDetails {
                package: Utf8PathBuf::from("path/to/driver"),
                components: vec!(
                    Utf8PathBuf::from("path/to/1234"),
                    Utf8PathBuf::from("path/to/5678")
                )
            }
        );
        assert_eq!(
            bundle.shell_commands.get("package1").unwrap(),
            &BTreeSet::from([
                PackageInternalPathBuf::from("path/to/binary1"),
                PackageInternalPathBuf::from("path/to/binary2"),
            ])
        );
    }
}
