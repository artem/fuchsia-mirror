// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Destinations in this file are used to force developers to list all their assembly-generated
//! destination files in this central location. The resulting enums are iterated over in order to
//! generate scrutiny golden files.

use crate::named_map::Key;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

/// A destination path for an input file.
pub trait Destination: Key + Clone + std::fmt::Display {}

/// A mapping between a file source and destination.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct FileEntry<D: Destination> {
    /// The path of the source file.
    pub source: Utf8PathBuf,

    /// The destination path to put the file.
    pub destination: D,
}

impl<D: Destination> std::fmt::Display for FileEntry<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(src={}, dest={})", self.source, self.destination)
    }
}

/// A bootfs file that assembly is allowed to include.
#[derive(Debug, Clone, EnumIter, Serialize)]
#[serde(into = "String")]
pub enum BootfsDestination {
    /// List of additional boot arguments to add to the ZBI.
    AdditionalBootArgs,
    /// The list of bootfs packages.
    BootfsPackageIndex,
    /// The component id index config for the Storage subsystem.
    ComponentIdIndex,
    /// The component manager policy for the Component subsystem.
    ComponentManagerConfig,
    /// The cpu manager node config.
    CpuManagerNodeConfig,
    /// The power manager node config.
    PowerManagerNodeConfig,
    /// The power manager thermal config.
    PowerManagerThermalConfig,
    /// SSH keys for development access.
    SshAuthorizedKeys,
    /// The zxcrypt config for the Storage subsystem.
    Zxcrypt,
    /// Variant specifically for making tests easier.
    ForTest,
    /// Any file that came from an AIB.
    FromAIB(String),
}

impl std::fmt::Display for BootfsDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::FromAIB(s) => return write!(f, "{}", s),
                Self::AdditionalBootArgs => "config/additional_boot_args",
                Self::BootfsPackageIndex => "data/bootfs_packages",
                Self::ComponentIdIndex => "config/component_id_index",
                Self::ComponentManagerConfig => "config/component_manager",
                Self::CpuManagerNodeConfig => "config/cpu_manager/node_config.json",
                Self::PowerManagerNodeConfig => "config/power_manager/node_config.json",
                Self::PowerManagerThermalConfig => "config/power_manager/thermal_config.json",
                Self::SshAuthorizedKeys => "data/ssh/authorized_keys",
                Self::Zxcrypt => "config/zxcrypt",
                Self::ForTest => "for-test",
            }
        )
    }
}

/// A package that assembly is allowed to include.
#[derive(Debug, Clone, EnumIter, Serialize)]
#[serde(into = "String")]
pub enum PackageDestination {
    /// The build-info package for the BuildInfo subsystem.
    BuildInfo,
    /// A sensor config for the UI subsystem.
    SensorConfig,
    /// The base package.
    Base,
    /// The config data package.
    ConfigData,
    /// The shell commands package.
    ShellCommands,
    /// The network provisioning configuration.
    NetcfgConfig,
    /// Variant specifically for making tests easier.
    ForTest,
    /// Any package that came from an AIB.
    FromAIB(String),
    /// Any package that came from the board
    FromBoard(String),
    /// Any package that came from the product.
    FromProduct(String),
}

impl std::fmt::Display for PackageDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::FromAIB(s) | Self::FromBoard(s) | Self::FromProduct(s) =>
                    return write!(f, "{}", s),
                Self::BuildInfo => "build-info",
                Self::SensorConfig => "sensor-config",
                Self::Base => "system_image",
                Self::ConfigData => "config-data",
                Self::ShellCommands => "shell-commands",
                Self::NetcfgConfig => "netcfg-config",
                Self::ForTest => "for-test",
            }
        )
    }
}

/// A bootfs package that assembly is allowed to include.
#[derive(Debug, Clone, EnumIter, Serialize)]
#[serde(into = "String")]
pub enum BootfsPackageDestination {
    /// The archivist pipelines configuration for the Diagnostics subsystem.
    ArchivistPipelines,
    /// The component config package.
    Config,
    /// Variant specifically for making tests easier.
    ForTest,
    /// Any package that came from an AIB.
    FromAIB(String),
    /// Any package that came from a board
    FromBoard(String),
}

impl std::fmt::Display for BootfsPackageDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::FromAIB(s) | Self::FromBoard(s) => return write!(f, "{}", s),
                Self::ArchivistPipelines => "archivist-pipelines",
                Self::Config => "config",
                Self::ForTest => "for-test",
            }
        )
    }
}

/// A destination key for a package set which can be either for blobfs or bootfs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(into = "String")]
pub enum PackageSetDestination {
    /// A package destined for blobfs.
    Blob(PackageDestination),
    /// A package destined for bootfs.
    Boot(BootfsPackageDestination),
}

impl std::fmt::Display for PackageSetDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blob(d) => write!(f, "{}", d),
            Self::Boot(d) => write!(f, "{}", d),
        }
    }
}

/// A package that assembly is allowed to compile then include.
/// Deserialize is implemented so that AIBs can continue to list the package
/// name as a string, and we can convert it into this enum.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CompiledPackageDestination {
    /// A blobfs compiled package.
    Blob(BlobfsCompiledPackageDestination),
    /// A bootfs compiled package.
    Boot(BootfsCompiledPackageDestination),
    /// Test variants.
    Test(TestCompiledPackageDestination),
}

/// A blobfs package that assembly is allowed to compiled then include.
#[derive(Debug, Clone, EnumIter, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlobfsCompiledPackageDestination {
    /// The compiled core package.
    Core,
    /// The compiled diagnostics package.
    Diagnostics,
    /// The compiled network package.
    Network,
    /// The compiled system update realm package.
    SystemUpdateRealm,
    /// The compiled toolbox package.
    Toolbox,
}

/// A blobfs package that assembly is allowed to compiled then include.
#[derive(Debug, Clone, EnumIter, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootfsCompiledPackageDestination {
    /// The compiled fshost package.
    Fshost,
    /// The compiled bootstrap realm package.
    Bootstrap,
}

/// Test variants.
#[derive(Debug, Clone, EnumIter, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestCompiledPackageDestination {
    /// Variant specifically for making tests easier.
    ForTest,
    /// Variant specifically for making tests easier.
    ForTest2,
}

// Compare using the string representation to ensure that we do not add a
// package from an AIB that was already added by assembly.
impl Eq for BootfsDestination {}
impl PartialEq for BootfsDestination {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}
impl PartialOrd for BootfsDestination {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.to_string().cmp(&other.to_string()))
    }
}
impl Ord for BootfsDestination {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}
impl Eq for PackageDestination {}
impl PartialEq for PackageDestination {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}
impl PartialOrd for PackageDestination {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.to_string().cmp(&other.to_string()))
    }
}
impl Ord for PackageDestination {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}
impl Eq for BootfsPackageDestination {}
impl PartialEq for BootfsPackageDestination {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}
impl PartialOrd for BootfsPackageDestination {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.to_string().cmp(&other.to_string()))
    }
}
impl Ord for BootfsPackageDestination {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

// Be able to convert to a string.
impl From<BootfsDestination> for String {
    fn from(b: BootfsDestination) -> Self {
        b.to_string()
    }
}
impl From<PackageDestination> for String {
    fn from(p: PackageDestination) -> Self {
        p.to_string()
    }
}
impl From<BootfsPackageDestination> for String {
    fn from(p: BootfsPackageDestination) -> Self {
        p.to_string()
    }
}
impl From<PackageSetDestination> for String {
    fn from(p: PackageSetDestination) -> Self {
        p.to_string()
    }
}

impl std::fmt::Display for CompiledPackageDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).expect("serialize enum");
        write!(f, "{}", value.as_str().expect("enum is str"))
    }
}

impl std::fmt::Display for BlobfsCompiledPackageDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).expect("serialize enum");
        write!(f, "{}", value.as_str().expect("enum is str"))
    }
}

impl std::fmt::Display for BootfsCompiledPackageDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).expect("serialize enum");
        write!(f, "{}", value.as_str().expect("enum is str"))
    }
}

impl Key for BootfsDestination {}
impl Key for PackageDestination {}
impl Key for BootfsPackageDestination {}
impl Key for PackageSetDestination {}
impl Key for BlobfsCompiledPackageDestination {}
impl Key for BootfsCompiledPackageDestination {}
impl Destination for String {}
impl Destination for BootfsDestination {}
impl Destination for PackageDestination {}
impl Destination for BootfsPackageDestination {}
impl Destination for PackageSetDestination {}
impl Destination for BlobfsCompiledPackageDestination {}
impl Destination for BootfsCompiledPackageDestination {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_string() {
        let dest = BootfsDestination::AdditionalBootArgs;
        assert_eq!("config/additional_boot_args", &dest.to_string());
        assert_eq!("for-test", &BootfsDestination::ForTest.to_string());
    }

    #[test]
    fn test_serialize() {
        let dest = BootfsDestination::AdditionalBootArgs;
        assert_eq!("\"config/additional_boot_args\"", &serde_json::to_string(&dest).unwrap());
        assert_eq!("\"for-test\"", &serde_json::to_string(&BootfsDestination::ForTest).unwrap());
    }
}
