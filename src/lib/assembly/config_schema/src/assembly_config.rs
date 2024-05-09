// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::image_assembly_config::PartialKernelConfig;
use crate::platform_config::PlatformConfig;
use crate::PackageDetails;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_package_utils::PackageInternalPathBuf;
use assembly_util::{CompiledPackageDestination, FileEntry};
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::common::{DriverDetails, PackageName};
use crate::product_config::ProductConfig;

/// Configuration for a Product Assembly operation.  This is a high-level operation
/// that takes a more abstract description of what is desired in the assembled
/// product images, and then generates the complete Image Assembly configuration
/// (`crate::config::ImageAssemblyConfig`) from that.
#[derive(Debug, Deserialize, Serialize, JsonSchema, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct AssemblyConfig {
    #[file_relative_paths]
    pub platform: PlatformConfig,
    #[file_relative_paths]
    pub product: ProductConfig,
    #[serde(default)]
    pub file_relative_paths: bool,
}

/// Configuration for Product Assembly, when developer overrides are in use.
///
/// This deserializes to intermediate types that can be manipulated in order to
/// apply developer overrides, before being parsed into the PlatformConfig
/// and ProductConfig types.
#[derive(Debug, Deserialize, Serialize, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct AssemblyConfigWrapperForOverrides {
    // The platform config is deserialized as a Value before it is parsed into
    // a 'PlatformConfig``
    pub platform: serde_json::Value,
    pub product: ProductConfig,
    #[serde(default)]
    pub file_relative_paths: bool,
}

/// A typename to represent a package that contains shell command binaries,
/// and the paths to those binaries
pub type ShellCommands = BTreeMap<PackageName, BTreeSet<PackageInternalPathBuf>>;

/// A bundle of inputs to be used in the assembly of a product.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
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
    #[file_relative_paths]
    #[serde(default)]
    pub packages_to_compile: Vec<CompiledPackageDefinition>,

    /// A package that includes files to include in bootfs.
    #[serde(default)]
    pub bootfs_files_package: Option<Utf8PathBuf>,
}

/// Contents of a compiled package. The contents provided by all
/// selected AIBs are merged by `name` into a single package
/// at assembly time.
#[derive(Debug, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct CompiledPackageDefinition {
    /// Name of the package to compile.
    pub name: CompiledPackageDestination,

    /// Components to compile and add to the package.
    #[file_relative_paths]
    #[serde(default)]
    pub components: Vec<CompiledComponentDefinition>,

    /// Non-component files to add to the package.
    #[serde(default)]
    pub contents: Vec<FileEntry<String>>,

    /// CML files included by the component cml.
    #[serde(default)]
    pub includes: Vec<Utf8PathBuf>,

    /// Whether the contents of this package should go into bootfs.
    /// Gated by allowlist -- please use this as a base package if possible.
    #[serde(default)]
    pub bootfs_package: Option<bool>,
}

/// Contents of a compiled component. The contents provided by all
/// selected AIBs are merged by `name` into a single package
/// at assembly time.
#[derive(Debug, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct CompiledComponentDefinition {
    /// The name of the component to compile.
    pub component_name: String,

    /// CML file shards to include in the compiled component manifest.
    #[file_relative_paths]
    pub shards: Vec<FileRelativePathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::PackageSet;
    use crate::platform_config::media_config::{AudioConfig, PlatformMediaConfig};
    use crate::platform_config::swd_config::{OtaConfigs, UpdateChecker};
    use crate::platform_config::{BuildType, FeatureSupportLevel};
    use crate::product_config::ProductPackageDetails;
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::Eng);
        assert_eq!(platform.feature_set_level, FeatureSupportLevel::Standard);
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::Eng);
        assert_eq!(platform.feature_set_level, FeatureSupportLevel::Bootstrap);
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::Eng);
        assert_eq!(platform.feature_set_level, FeatureSupportLevel::Standard);
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::Eng);
        assert_eq!(platform.feature_set_level, FeatureSupportLevel::Empty);
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::UserDebug);
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::User);
    }

    #[test]
    fn test_product_assembly_config_with_product_provided_parts() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
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
        let platform = config.platform;
        assert_eq!(platform.build_type, BuildType::Eng);
        assert_eq!(
            config.product.packages.base,
            vec![ProductPackageDetails {
                manifest: FileRelativePathBuf::FileRelative(
                    "path/to/base/package_manifest.json".into()
                ),
                config_data: Vec::default()
            }]
        );
        assert_eq!(
            config.product.packages.cache,
            vec![ProductPackageDetails {
                manifest: FileRelativePathBuf::FileRelative(
                    "path/to/cache/package_manifest.json".into()
                ),
                config_data: Vec::default()
            }]
        );
        assert_eq!(
            config.product.base_drivers,
            vec![DriverDetails {
                package: "path/to/base/driver/package_manifest.json".into(),
                components: vec!["meta/path/to/component.cml".into()]
            }]
        )
    }

    #[test]
    fn test_product_assembly_config_with_relative_paths() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
            connectivity: {
              network: {
                netcfg_config_path: "a/b/c",
                netstack_config_path: "a/b/c",
                google_maps_api_key_path: "a/b/c",
              },
              mdns: {
                config: "a/b/c",
              },
            },
            development_support: {
              authorized_ssh_keys_path: "a/b/c",
              authorized_ssh_ca_certs_path: "a/b/c",
            },
            software_delivery: {
              update_checker: {
                omaha_client: {
                  channels_path: "a/b/c",
                },
              },
              tuf_config_paths: [
                "a/b/c"
              ],
            },
            storage: {
              component_id_index: {
                product_index: "component/id/index",
              },
            },
          },
          product: {
            packages: {
              base: [
                { manifest: "base/package_manifest.json" }
              ],
              cache: [
                { manifest: "cache/package_manifest.json" }
              ],
            },
            component_policy: {
              product_policies: [
                "component/policy",
              ],
            },
          },
          file_relative_paths: true,
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: AssemblyConfig = util::from_reader(&mut cursor).unwrap();
        let config = config.resolve_paths_from_file("path/to/assembly_config.json").unwrap();
        assert_eq!(config.file_relative_paths, true);
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
        assert_eq!(
            config.platform.connectivity.network.netcfg_config_path.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.connectivity.network.netstack_config_path.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.connectivity.network.google_maps_api_key_path.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.connectivity.mdns.config.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.development_support.authorized_ssh_ca_certs_path.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.development_support.authorized_ssh_keys_path.unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.software_delivery.tuf_config_paths[0],
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            match config.platform.software_delivery.update_checker {
                Some(UpdateChecker::OmahaClient(OtaConfigs { channels_path, .. })) => channels_path,
                Some(_) => None,
                None => None,
            }
            .unwrap(),
            FileRelativePathBuf::Resolved("path/to/a/b/c".into())
        );
        assert_eq!(
            config.platform.storage.component_id_index.product_index.unwrap(),
            FileRelativePathBuf::Resolved("path/to/component/id/index".into())
        );
        assert_eq!(
            config.product.component_policy.product_policies[0],
            FileRelativePathBuf::Resolved("path/to/component/policy".into())
        );
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
                    components: [
                      {
                        component_name: "component1",
                        shards: ["path/to/component1.cml"],
                      },
                      {
                        component_name: "component2",
                        shards: ["path/to/component2.cml"],
                      },
                    ],
                    contents: [
                        {
                            source: "path/to/source",
                            destination: "path/to/destination",
                        }
                    ],
                    includes: [ "src/path/to/include.cml" ]
                },
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

    #[test]
    fn test_assembly_config_wrapper_for_overrides() {
        let json5 = r#"
        {
          platform: {
            build_type: "eng",
          },
          product: {},
        }
        "#;

        let overrides = serde_json::json!({
            "media": {
                "audio": {
                    "partial_stack": {}
                }
            }
        });

        let mut cursor = std::io::Cursor::new(json5);
        let AssemblyConfigWrapperForOverrides { platform, product: _, .. } =
            util::from_reader(&mut cursor).unwrap();

        // serde_json and serde_json5 have an incompatible handling of how they
        // serialize / deserialize enums.  So this test validates both the
        // value merging method but also that the problematic enum syntax is
        // correctly parsed when bounced through a string as it's done in the
        // product assembly binary itself.

        // 1. Merge to a 'value', not to the final type, as we need serde_json5
        //    to do the parsing, not serde_json.
        let merged_platform_value: serde_json::Value =
            crate::try_merge_into(platform, overrides).unwrap();

        // 2. Write the value out to a string, using pretty-printing so that
        // line numbers and such are all sensical.
        let merged_platform_string = serde_json::to_string_pretty(&merged_platform_value).unwrap();

        // 3. Parse the string using serde_json5, so that enums are handled
        //    consistently.
        let merged_platform: PlatformConfig =
            serde_json5::from_str(&merged_platform_string).unwrap();

        assert_eq!(
            merged_platform.media,
            PlatformMediaConfig { audio: Some(AudioConfig::PartialStack), ..Default::default() },
        );
    }
}
