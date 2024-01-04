// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use version_history_shared::Status;

use std::{
    array::TryFromSliceError,
    convert::{TryFrom, TryInto},
    fmt,
};

/// VERSION_HISTORY is an array of all the known SDK versions.  It is guaranteed
/// (at compile-time) by the proc_macro to be non-empty.
pub const VERSION_HISTORY: &[Version] = &version_history_macro::declare_version_history!();

/// LATEST_VERSION is the latest known SDK version.
pub const LATEST_VERSION: &Version = &version_history_macro::latest_sdk_version!();

/// SUPPORTED_API_LEVELS are the supported API levels.
pub const SUPPORTED_API_LEVELS: &[Version] = &version_history_macro::get_supported_versions!();

/// An `AbiRevision` represents the ABI revision of a Fuchsia Package.
/// https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0135_package_abi_revision?#design
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Clone)]
pub struct AbiRevision(pub u64);

impl AbiRevision {
    pub const PATH: &'static str = "meta/fuchsia.abi/abi-revision";

    pub fn new(u: u64) -> AbiRevision {
        AbiRevision(u)
    }

    /// Parse the ABI revision from little-endian bytes.
    pub fn from_bytes(b: [u8; 8]) -> Self {
        AbiRevision(u64::from_le_bytes(b))
    }

    /// Encode the ABI revision into little-endian bytes.
    pub fn as_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl fmt::Display for AbiRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl From<u64> for AbiRevision {
    fn from(abi_revision: u64) -> AbiRevision {
        AbiRevision(abi_revision)
    }
}

impl From<&u64> for AbiRevision {
    fn from(abi_revision: &u64) -> AbiRevision {
        AbiRevision(*abi_revision)
    }
}

impl From<AbiRevision> for u64 {
    fn from(abi_revision: AbiRevision) -> u64 {
        abi_revision.0
    }
}

impl From<[u8; 8]> for AbiRevision {
    fn from(abi_revision: [u8; 8]) -> AbiRevision {
        AbiRevision::from_bytes(abi_revision)
    }
}

impl TryFrom<&[u8]> for AbiRevision {
    type Error = TryFromSliceError;

    fn try_from(abi_revision: &[u8]) -> Result<AbiRevision, Self::Error> {
        let abi_revision: [u8; 8] = abi_revision.try_into()?;
        Ok(AbiRevision::from_bytes(abi_revision))
    }
}

impl std::ops::Deref for AbiRevision {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Version is a mapping between the supported API level and the ABI revisions.
///
/// See https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0002_platform_versioning for more
/// details.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
pub struct Version {
    /// The API level denotes a set of APIs available when building an application for a given
    /// release of the FUCHSIA IDK.
    pub api_level: u64,

    /// The ABI revision denotes the semantics of the Fuchsia System Interface that an application
    /// expects the platform to provide.
    pub abi_revision: AbiRevision,

    /// The Status denotes the current status of the API level, either unsupported, supported, in-development.
    pub status: Status,
}

impl Version {
    /// Returns true if this version is supported - that is, whether components
    /// targeting this version will be able to run on this device.
    pub fn is_supported(&self) -> bool {
        match self.status {
            Status::InDevelopment | Status::Supported => true,
            Status::Unsupported => false,
        }
    }
}

pub fn version_from_abi_revision(abi_revision: AbiRevision) -> Option<Version> {
    // TODO(https://fxbug.dev/117262): Store APIs and ABIs in a map instead of a list.
    VERSION_HISTORY.iter().find(|v| v.abi_revision == abi_revision).cloned()
}

/// Returns true if the given abi_revision is listed in the VERSION_HISTORY of
/// known SDK versions.
pub fn is_valid_abi_revision(abi_revision: AbiRevision) -> bool {
    version_from_abi_revision(abi_revision).is_some()
}

/// Returns true if the given abi_revision is listed in SUPPORTED_API_LEVELS.
pub fn is_supported_abi_revision(abi_revision: AbiRevision) -> bool {
    if let Some(version) = version_from_abi_revision(abi_revision) {
        version.is_supported()
    } else {
        false
    }
}

/// Returns a vector of the API levels in SUPPORTED_API_LEVELS.
pub fn get_supported_api_levels() -> Vec<u64> {
    let mut api_levels: Vec<u64> = Vec::new();
    for version in SUPPORTED_API_LEVELS {
        api_levels.push(version.api_level);
    }

    return api_levels;
}

/// Returns a vector of the ABI revisions in SUPPORTED_API_LEVELS.
pub fn get_supported_abi_revisions() -> Vec<u64> {
    let mut abi_revisions: Vec<u64> = Vec::new();
    for version in SUPPORTED_API_LEVELS {
        abi_revisions.push(version.abi_revision.0);
    }

    return abi_revisions;
}

pub fn get_latest_abi_revision() -> u64 {
    return LATEST_VERSION.abi_revision.0;
}

pub fn check_abi_revision(abi_revision: Option<AbiRevision>) -> Result<(), AbiRevisionError> {
    let abi_revision = abi_revision.ok_or(AbiRevisionError::Absent)?;

    if let Some(version) = version_from_abi_revision(abi_revision) {
        if version.is_supported() {
            Ok(())
        } else {
            Err(AbiRevisionError::Unsupported {
                version,
                supported_versions: SUPPORTED_API_LEVELS.to_vec(),
            })
        }
    } else {
        Err(AbiRevisionError::Unknown {
            abi_revision,
            supported_versions: SUPPORTED_API_LEVELS.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiRevisionError {
    /// A component tried to run, but it presented no ABI revision.
    Absent,

    /// A component tried to run, but its ABI revision was not recognized.
    Unknown { abi_revision: AbiRevision, supported_versions: Vec<Version> },

    /// A component tried to run, but the ABI revision it presented is not
    /// supported by this system.
    Unsupported { version: Version, supported_versions: Vec<Version> },
}

impl std::error::Error for AbiRevisionError {}

impl std::fmt::Display for AbiRevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let write_supported_versions =
            |f: &mut std::fmt::Formatter<'_>, supported_versions: &[Version]| -> std::fmt::Result {
                write!(f, "The following API levels are supported: ")?;

                for (idx, version) in supported_versions.iter().enumerate() {
                    write!(f, "{} ({})", version.api_level, version.abi_revision)?;
                    if idx != supported_versions.len() - 1 {
                        write!(f, ", ")?;
                    }
                }
                Ok(())
            };

        match self {
            AbiRevisionError::Absent => write!(f, "Missing target ABI revision."),
            AbiRevisionError::Unknown { abi_revision, supported_versions } => {
                write!(
                    f,
                    "Unknown target ABI revision: {}. The OS may be too old to support it? ",
                    abi_revision
                )?;
                write_supported_versions(f, supported_versions)
            }
            AbiRevisionError::Unsupported { version, supported_versions } => {
                write!(
                    f,
                    "This ABI targets API {} ({}), which is no longer supported. ",
                    version.api_level, version.abi_revision
                )?;
                write_supported_versions(f, supported_versions)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Helper to convert from the shared crate's Version struct to this crate's
    // struct.
    fn version_history_from_shared() -> Vec<Version> {
        version_history_shared::version_history()
            .unwrap()
            .into_iter()
            .map(|v| Version {
                api_level: v.api_level,
                abi_revision: AbiRevision::new(v.abi_revision.value),
                status: v.status,
            })
            .collect::<Vec<_>>()
    }

    #[test]
    fn test_proc_macro_worked() {
        let expected = version_history_from_shared();
        assert_eq!(expected, VERSION_HISTORY);
    }

    #[test]
    fn test_latest_version_proc_macro() {
        let shared_versions = version_history_from_shared();
        let expected = shared_versions.last().expect("version_history_shared was empty");
        assert_eq!(expected, &version_history_macro::latest_sdk_version!());
    }

    #[test]
    fn test_valid_abi_revision() {
        for version in VERSION_HISTORY {
            assert!(is_valid_abi_revision(version.abi_revision));
        }
    }

    #[test]
    fn test_is_supported_abi_revision() {
        for version in SUPPORTED_API_LEVELS {
            assert!(is_supported_abi_revision(version.abi_revision));
        }
    }

    // To ensure this test doesn't flake, proptest is used to generate u64
    // values which are not in the current VERSION_HISTORY list.
    proptest! {
        #[test]
        fn test_invalid_abi_revision(u in any::<u64>().prop_filter("using u64 that isn't in VERSION_HISTORY", |u|
            // The randomly chosen 'abi_revision' must not equal any of the
            // abi_revisions in the VERSION_HISTORY list.
            VERSION_HISTORY.iter().all(|v| v.abi_revision != AbiRevision::new(*u))
        )) {
            assert!(!is_valid_abi_revision(AbiRevision::new(u)))
        }
    }
}
