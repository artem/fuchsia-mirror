// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[allow(dead_code)]
pub mod convert;

use {
    cm_fidl_validator,
    cm_rust_derive::{
        CapabilityDeclCommon, ExposeDeclCommon, FidlDecl, OfferDeclCommon, UseDeclCommon,
    },
    cm_types, fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata,
    fidl_fuchsia_io2 as fio2, fidl_fuchsia_process as fprocess, fidl_fuchsia_sys2 as fsys,
    from_enum::FromEnum,
    lazy_static::lazy_static,
    std::collections::HashMap,
    std::convert::{From, TryFrom},
    std::fmt,
    std::path::PathBuf,
    std::str::FromStr,
    thiserror::Error,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "serde")]
mod serde_ext;

lazy_static! {
    static ref DATA_TYPENAME: CapabilityName = CapabilityName("Data".to_string());
    static ref CACHE_TYPENAME: CapabilityName = CapabilityName("Cache".to_string());
    static ref META_TYPENAME: CapabilityName = CapabilityName("Meta".to_string());
}

/// Converts a fidl object into its corresponding native representation.
pub trait FidlIntoNative<T> {
    fn fidl_into_native(self) -> T;
}

pub trait NativeIntoFidl<T> {
    fn native_into_fidl(self) -> T;
}

/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations that leaves the input unchanged.
macro_rules! fidl_translations_identical {
    ($into_type:ty) => {
        impl FidlIntoNative<$into_type> for $into_type {
            fn fidl_into_native(self) -> $into_type {
                self
            }
        }
        impl NativeIntoFidl<$into_type> for $into_type {
            fn native_into_fidl(self) -> Self {
                self
            }
        }
    };
}

/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations that
/// delegate to existing `Into` implementations.
macro_rules! fidl_translations_from_into {
    ($native_type:ty, $fidl_type:ty) => {
        impl FidlIntoNative<$native_type> for $fidl_type {
            fn fidl_into_native(self) -> $native_type {
                self.into()
            }
        }
        impl NativeIntoFidl<$fidl_type> for $native_type {
            fn native_into_fidl(self) -> $fidl_type {
                self.into()
            }
        }
    };
}

/// DO NOT SUBMIT: Add comment.
/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations for
/// an symmetrical enum types.
/// `fidl_type` should be the FIDL type while `native_type` should be
/// the Rust native type defined elsewhere in this file.
/// Each field of the enums must be provided in the `variant` fieldset.
macro_rules! fidl_translations_symmetrical_enums {
($fidl_type:ty , $native_type:ty, $($variant: ident),*) => {
        impl FidlIntoNative<$native_type> for $fidl_type {
            fn fidl_into_native(self) -> $native_type {
                match self {
                    $( <$fidl_type>::$variant => <$native_type>::$variant,  )*
                }
            }
        }
        impl NativeIntoFidl<$fidl_type> for $native_type {
            fn native_into_fidl(self) -> $fidl_type {
                match self {
                    $( <$native_type>::$variant => <$fidl_type>::$variant,  )*
                }
            }
        }
    };
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Default)]
#[fidl_decl(fidl_table = "fsys::ComponentDecl, fdecl::Component")]
pub struct ComponentDecl {
    pub program: Option<ProgramDecl>,
    pub uses: Vec<UseDecl>,
    pub exposes: Vec<ExposeDecl>,
    pub offers: Vec<OfferDecl>,
    pub capabilities: Vec<CapabilityDecl>,
    pub children: Vec<ChildDecl>,
    pub collections: Vec<CollectionDecl>,
    pub facets: Option<fdata::Dictionary>,
    pub environments: Vec<EnvironmentDecl>,
    pub config: Option<ConfigDecl>,
}

impl ComponentDecl {
    /// Returns the runner used by this component, or `None` if this is a non-executable component.
    pub fn get_runner(&self) -> Option<&CapabilityName> {
        self.program.as_ref().and_then(|p| p.runner.as_ref())
    }

    /// Returns the `StorageDecl` corresponding to `storage_name`.
    pub fn find_storage_source<'a>(
        &'a self,
        storage_name: &CapabilityName,
    ) -> Option<&'a StorageDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Storage(s) if &s.name == storage_name => Some(s),
            _ => None,
        })
    }

    /// Returns the `ProtocolDecl` corresponding to `protocol_name`.
    pub fn find_protocol_source<'a>(
        &'a self,
        protocol_name: &CapabilityName,
    ) -> Option<&'a ProtocolDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Protocol(r) if &r.name == protocol_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `DirectoryDecl` corresponding to `directory_name`.
    pub fn find_directory_source<'a>(
        &'a self,
        directory_name: &CapabilityName,
    ) -> Option<&'a DirectoryDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Directory(r) if &r.name == directory_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `RunnerDecl` corresponding to `runner_name`.
    pub fn find_runner_source<'a>(
        &'a self,
        runner_name: &CapabilityName,
    ) -> Option<&'a RunnerDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Runner(r) if &r.name == runner_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `ResolverDecl` corresponding to `resolver_name`.
    pub fn find_resolver_source<'a>(
        &'a self,
        resolver_name: &CapabilityName,
    ) -> Option<&'a ResolverDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Resolver(r) if &r.name == resolver_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `CollectionDecl` corresponding to `collection_name`.
    pub fn find_collection<'a>(&'a self, collection_name: &str) -> Option<&'a CollectionDecl> {
        self.collections.iter().find(|c| c.name == collection_name)
    }

    /// Indicates whether the capability specified by `target_name` is exposed to the framework.
    pub fn is_protocol_exposed_to_framework(&self, in_target_name: &CapabilityName) -> bool {
        self.exposes.iter().any(|expose| match expose {
            ExposeDecl::Protocol(ExposeProtocolDecl { target, target_name, .. })
                if target == &ExposeTarget::Framework =>
            {
                target_name == in_target_name
            }
            _ => false,
        })
    }

    /// Indicates whether the capability specified by `source_name` is requested.
    pub fn uses_protocol(&self, source_name: &CapabilityName) -> bool {
        self.uses.iter().any(|use_decl| match use_decl {
            UseDecl::Protocol(ls) => &ls.source_name == source_name,
            _ => false,
        })
    }
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::UseDecl, fdecl::Use")]
pub enum UseDecl {
    Service(UseServiceDecl),
    Protocol(UseProtocolDecl),
    Directory(UseDirectoryDecl),
    Storage(UseStorageDecl),
    Event(UseEventDecl),
    EventStream(UseEventStreamDecl),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseServiceDecl, fdecl::UseService")]
pub struct UseServiceDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
    pub dependency_type: DependencyType,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseProtocolDecl, fdecl::UseProtocol")]
pub struct UseProtocolDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
    pub dependency_type: DependencyType,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseDirectoryDecl, fdecl::UseDirectory")]
pub struct UseDirectoryDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_fio2_operations",
            serialize_with = "serde_ext::serialize_fio2_operations"
        )
    )]
    pub rights: fio2::Operations,

    pub subdir: Option<PathBuf>,
    pub dependency_type: DependencyType,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseStorageDecl, fdecl::UseStorage")]
pub struct UseStorageDecl {
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
}

impl SourceName for UseStorageDecl {
    fn source_name(&self) -> &CapabilityName {
        &self.source_name
    }
}

impl UseDeclCommon for UseStorageDecl {
    fn source(&self) -> &UseSource {
        &UseSource::Parent
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseEventDecl, fdecl::UseEvent")]
pub struct UseEventDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_name: CapabilityName,
    pub filter: Option<HashMap<String, DictionaryValue>>,
    pub mode: EventMode,
    pub dependency_type: DependencyType,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::EventSubscription, fdecl::EventSubscription")]
pub struct EventSubscription {
    pub event_name: String,
    pub mode: EventMode,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventMode {
    Async,
    Sync,
}

fidl_translations_symmetrical_enums!(fsys::EventMode, EventMode, Sync, Async);
fidl_translations_symmetrical_enums!(fdecl::EventMode, EventMode, Sync, Async);

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::UseEventStreamDecl, fdecl::UseEventStream")]
pub struct UseEventStreamDecl {
    pub name: CapabilityName,
    pub subscriptions: Vec<EventSubscription>,
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::OfferDecl, fdecl::Offer")]
pub enum OfferDecl {
    Service(OfferServiceDecl),
    Protocol(OfferProtocolDecl),
    Directory(OfferDirectoryDecl),
    Storage(OfferStorageDecl),
    Runner(OfferRunnerDecl),
    Resolver(OfferResolverDecl),
    Event(OfferEventDecl),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferServiceDecl, fdecl::OfferService")]
pub struct OfferServiceDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferProtocolDecl, fdecl::OfferProtocol")]
pub struct OfferProtocolDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub dependency_type: DependencyType,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferDirectoryDecl, fdecl::OfferDirectory")]
pub struct OfferDirectoryDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub dependency_type: DependencyType,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_opt_fio2_operations",
            serialize_with = "serde_ext::serialize_opt_fio2_operations"
        )
    )]
    pub rights: Option<fio2::Operations>,

    pub subdir: Option<PathBuf>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferStorageDecl, fdecl::OfferStorage")]
pub struct OfferStorageDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferRunnerDecl, fdecl::OfferRunner")]
pub struct OfferRunnerDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferResolverDecl, fdecl::OfferResolver")]
pub struct OfferResolverDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::OfferEventDecl, fdecl::OfferEvent")]
pub struct OfferEventDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub filter: Option<HashMap<String, DictionaryValue>>,
    pub mode: EventMode,
}

impl SourceName for OfferDecl {
    fn source_name(&self) -> &CapabilityName {
        match &self {
            OfferDecl::Service(o) => o.source_name(),
            OfferDecl::Protocol(o) => o.source_name(),
            OfferDecl::Directory(o) => o.source_name(),
            OfferDecl::Storage(o) => o.source_name(),
            OfferDecl::Runner(o) => o.source_name(),
            OfferDecl::Resolver(o) => o.source_name(),
            OfferDecl::Event(o) => o.source_name(),
        }
    }
}

impl OfferDeclCommon for OfferDecl {
    fn target_name(&self) -> &CapabilityName {
        match &self {
            OfferDecl::Service(o) => o.target_name(),
            OfferDecl::Protocol(o) => o.target_name(),
            OfferDecl::Directory(o) => o.target_name(),
            OfferDecl::Storage(o) => o.target_name(),
            OfferDecl::Runner(o) => o.target_name(),
            OfferDecl::Resolver(o) => o.target_name(),
            OfferDecl::Event(o) => o.target_name(),
        }
    }

    fn target(&self) -> &OfferTarget {
        match &self {
            OfferDecl::Service(o) => o.target(),
            OfferDecl::Protocol(o) => o.target(),
            OfferDecl::Directory(o) => o.target(),
            OfferDecl::Storage(o) => o.target(),
            OfferDecl::Runner(o) => o.target(),
            OfferDecl::Resolver(o) => o.target(),
            OfferDecl::Event(o) => o.target(),
        }
    }

    fn source(&self) -> &OfferSource {
        match &self {
            OfferDecl::Service(o) => o.source(),
            OfferDecl::Protocol(o) => o.source(),
            OfferDecl::Directory(o) => o.source(),
            OfferDecl::Storage(o) => o.source(),
            OfferDecl::Runner(o) => o.source(),
            OfferDecl::Resolver(o) => o.source(),
            OfferDecl::Event(o) => o.source(),
        }
    }
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::ExposeDecl, fdecl::Expose")]
pub enum ExposeDecl {
    Service(ExposeServiceDecl),
    Protocol(ExposeProtocolDecl),
    Directory(ExposeDirectoryDecl),
    Runner(ExposeRunnerDecl),
    Resolver(ExposeResolverDecl),
}

impl SourceName for ExposeDecl {
    fn source_name(&self) -> &CapabilityName {
        match self {
            Self::Service(e) => e.source_name(),
            Self::Protocol(e) => e.source_name(),
            Self::Directory(e) => e.source_name(),
            Self::Runner(e) => e.source_name(),
            Self::Resolver(e) => e.source_name(),
        }
    }
}

impl ExposeDeclCommon for ExposeDecl {
    fn source(&self) -> &ExposeSource {
        match self {
            Self::Service(e) => e.source(),
            Self::Protocol(e) => e.source(),
            Self::Directory(e) => e.source(),
            Self::Runner(e) => e.source(),
            Self::Resolver(e) => e.source(),
        }
    }

    fn target(&self) -> &ExposeTarget {
        match self {
            Self::Service(e) => e.target(),
            Self::Protocol(e) => e.target(),
            Self::Directory(e) => e.target(),
            Self::Runner(e) => e.target(),
            Self::Resolver(e) => e.target(),
        }
    }

    fn target_name(&self) -> &CapabilityName {
        match self {
            Self::Service(e) => e.target_name(),
            Self::Protocol(e) => e.target_name(),
            Self::Directory(e) => e.target_name(),
            Self::Runner(e) => e.target_name(),
            Self::Resolver(e) => e.target_name(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ExposeServiceDecl, fdecl::ExposeService")]
pub struct ExposeServiceDecl {
    pub source: ExposeSource,

    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ExposeProtocolDecl, fdecl::ExposeProtocol")]
pub struct ExposeProtocolDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ExposeDirectoryDecl, fdecl::ExposeDirectory")]
pub struct ExposeDirectoryDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_opt_fio2_operations",
            serialize_with = "serde_ext::serialize_opt_fio2_operations"
        )
    )]
    pub rights: Option<fio2::Operations>,

    pub subdir: Option<PathBuf>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ExposeRunnerDecl, fdecl::ExposeRunner")]
pub struct ExposeRunnerDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ExposeResolverDecl, fdecl::ExposeResolver")]
pub struct ExposeResolverDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::CapabilityDecl, fdecl::Capability")]
pub enum CapabilityDecl {
    Service(ServiceDecl),
    Protocol(ProtocolDecl),
    Directory(DirectoryDecl),
    Storage(StorageDecl),
    Runner(RunnerDecl),
    Resolver(ResolverDecl),
    Event(EventDecl),
}

impl CapabilityDeclCommon for CapabilityDecl {
    fn name(&self) -> &CapabilityName {
        match self {
            Self::Service(c) => c.name(),
            Self::Protocol(c) => c.name(),
            Self::Directory(c) => c.name(),
            Self::Storage(c) => c.name(),
            Self::Runner(c) => c.name(),
            Self::Resolver(c) => c.name(),
            Self::Event(c) => c.name(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ServiceDecl, fdecl::Service")]
pub struct ServiceDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ProtocolDecl, fdecl::Protocol")]
pub struct ProtocolDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::DirectoryDecl, fdecl::Directory")]
pub struct DirectoryDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_fio2_operations",
            serialize_with = "serde_ext::serialize_fio2_operations"
        )
    )]
    pub rights: fio2::Operations,
}

// The native type here is `fsys::StorageId`. We do this because the StorageDecl
// uses this type directly instead of using a "native" type.
// This pattern holds for other cases where a "native" struct used a field
// with a `fsys` type.
fidl_translations_symmetrical_enums!(
    fdecl::StorageId,
    fsys::StorageId,
    StaticInstanceId,
    StaticInstanceIdOrMoniker
);

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::StorageDecl, fdecl::Storage")]
pub struct StorageDecl {
    pub name: CapabilityName,
    pub source: StorageDirectorySource,
    pub backing_dir: CapabilityName,
    pub subdir: Option<PathBuf>,
    #[cfg_attr(feature = "serde", serde(with = "serde_ext::StorageId"))]
    pub storage_id: fsys::StorageId,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::RunnerDecl, fdecl::Runner")]
pub struct RunnerDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ResolverDecl, fdecl::Resolver")]
pub struct ResolverDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::EventDecl, fdecl::Event")]
pub struct EventDecl {
    pub name: CapabilityName,
}

impl CapabilityDecl {
    pub fn name(&self) -> &CapabilityName {
        match self {
            CapabilityDecl::Directory(decl) => decl.name(),
            CapabilityDecl::Protocol(decl) => decl.name(),
            CapabilityDecl::Resolver(decl) => decl.name(),
            CapabilityDecl::Runner(decl) => decl.name(),
            CapabilityDecl::Service(decl) => decl.name(),
            CapabilityDecl::Storage(decl) => decl.name(),
            CapabilityDecl::Event(decl) => decl.name(),
        }
    }

    pub fn path(&self) -> Option<&CapabilityPath> {
        match self {
            CapabilityDecl::Directory(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Protocol(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Resolver(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Runner(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Service(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Storage(_) => None,
            CapabilityDecl::Event(_) => None,
        }
    }
}

fidl_translations_symmetrical_enums!(fdecl::OnTerminate, fsys::OnTerminate, Reboot, None);
fidl_translations_symmetrical_enums!(fdecl::StartupMode, fsys::StartupMode, Eager, Lazy);

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ChildDecl, fdecl::Child")]
pub struct ChildDecl {
    pub name: String,
    pub url: String,
    pub startup: fsys::StartupMode,
    pub on_terminate: Option<fsys::OnTerminate>,
    pub environment: Option<String>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChildRef {
    pub name: String,
    pub collection: Option<String>,
}

impl FidlIntoNative<ChildRef> for fsys::ChildRef {
    fn fidl_into_native(self) -> ChildRef {
        ChildRef { name: self.name, collection: self.collection }
    }
}

impl NativeIntoFidl<fsys::ChildRef> for ChildRef {
    fn native_into_fidl(self) -> fsys::ChildRef {
        fsys::ChildRef { name: self.name, collection: self.collection }
    }
}

impl FidlIntoNative<ChildRef> for fdecl::ChildRef {
    fn fidl_into_native(self) -> ChildRef {
        ChildRef { name: self.name, collection: self.collection }
    }
}

impl NativeIntoFidl<fdecl::ChildRef> for ChildRef {
    fn native_into_fidl(self) -> fdecl::ChildRef {
        fdecl::ChildRef { name: self.name, collection: self.collection }
    }
}

fidl_translations_symmetrical_enums!(
    fdecl::Durability,
    fsys::Durability,
    Persistent,
    Transient,
    SingleRun
);
fidl_translations_symmetrical_enums!(
    fdecl::AllowedOffers,
    cm_types::AllowedOffers,
    StaticOnly,
    StaticAndDynamic
);

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::CollectionDecl, fdecl::Collection")]
pub struct CollectionDecl {
    pub name: String,
    pub durability: fsys::Durability,

    #[fidl_decl(default)]
    pub allowed_offers: cm_types::AllowedOffers,
    pub environment: Option<String>,
}

fidl_translations_symmetrical_enums!(
    fdecl::EnvironmentExtends,
    fsys::EnvironmentExtends,
    Realm,
    None
);

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::EnvironmentDecl, fdecl::Environment")]
pub struct EnvironmentDecl {
    pub name: String,
    pub extends: fsys::EnvironmentExtends,
    pub runners: Vec<RunnerRegistration>,
    pub resolvers: Vec<ResolverRegistration>,
    pub debug_capabilities: Vec<DebugRegistration>,
    pub stop_timeout_ms: Option<u32>,
}

pub enum DependencySource<'a> {
    Runner { registry: &'a Vec<RunnerRegistration> },
    Resolver { registry: &'a Vec<ResolverRegistration> },
    Debug { registry: &'a Vec<DebugRegistration> },
}

impl EnvironmentDecl {
    pub fn get_dependency_sources<'a>(&'a self) -> Vec<DependencySource<'a>> {
        vec![
            DependencySource::Runner { registry: &self.runners },
            DependencySource::Resolver { registry: &self.resolvers },
            DependencySource::Debug { registry: &self.debug_capabilities },
        ]
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigDecl, fdecl::Config")]
pub struct ConfigDecl {
    pub fields: Vec<ConfigField>,
    pub declaration_checksum: Vec<u8>,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigField, fdecl::ConfigField")]
pub struct ConfigField {
    pub key: String,
    pub value_type: ConfigValueType,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigStringType, fdecl::ConfigStringType")]
pub struct ConfigStringType {
    pub max_size: u32,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigBooleanType, fdecl::ConfigBooleanType")]
pub struct ConfigBooleanType {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigUnsigned8Type, fdecl::ConfigUnsigned8Type")]
pub struct ConfigUnsigned8Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigSigned8Type, fdecl::ConfigSigned8Type")]
pub struct ConfigSigned8Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigUnsigned16Type, fdecl::ConfigUnsigned16Type")]
pub struct ConfigUnsigned16Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigSigned16Type, fdecl::ConfigSigned16Type")]
pub struct ConfigSigned16Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigUnsigned32Type, fdecl::ConfigUnsigned32Type")]
pub struct ConfigUnsigned32Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigSigned32Type, fdecl::ConfigSigned32Type")]
pub struct ConfigSigned32Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigUnsigned64Type, fdecl::ConfigUnsigned64Type")]
pub struct ConfigUnsigned64Type {}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigSigned64Type, fdecl::ConfigSigned64Type")]
pub struct ConfigSigned64Type {}

#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::ConfigVectorElementType, fdecl::ConfigVectorElementType")]
pub enum ConfigVectorElementType {
    Bool(ConfigBooleanType),
    Uint8(ConfigUnsigned8Type),
    Int8(ConfigSigned8Type),
    Uint16(ConfigUnsigned16Type),
    Int16(ConfigSigned16Type),
    Uint32(ConfigUnsigned32Type),
    Int32(ConfigSigned32Type),
    Uint64(ConfigUnsigned64Type),
    Int64(ConfigSigned64Type),
    String(ConfigStringType),
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ConfigVectorType, fdecl::ConfigVectorType")]
pub struct ConfigVectorType {
    pub max_count: u32,
    pub element_type: ConfigVectorElementType,
}

#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::ConfigValueType, fdecl::ConfigValueType")]
pub enum ConfigValueType {
    Bool(ConfigBooleanType),
    Uint8(ConfigUnsigned8Type),
    Int8(ConfigSigned8Type),
    Uint16(ConfigUnsigned16Type),
    Int16(ConfigSigned16Type),
    Uint32(ConfigUnsigned32Type),
    Int32(ConfigSigned32Type),
    Uint64(ConfigUnsigned64Type),
    Int64(ConfigSigned64Type),
    String(ConfigStringType),
    Vector(ConfigVectorType),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::RunnerRegistration, fdecl::RunnerRegistration")]
pub struct RunnerRegistration {
    pub source_name: CapabilityName,
    pub target_name: CapabilityName,
    pub source: RegistrationSource,
}

impl SourceName for RunnerRegistration {
    fn source_name(&self) -> &CapabilityName {
        &self.source_name
    }
}

impl RegistrationDeclCommon for RunnerRegistration {
    const TYPE: &'static str = "runner";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::ResolverRegistration, fdecl::ResolverRegistration")]
pub struct ResolverRegistration {
    pub resolver: CapabilityName,
    pub source: RegistrationSource,
    pub scheme: String,
}

impl SourceName for ResolverRegistration {
    fn source_name(&self) -> &CapabilityName {
        &self.resolver
    }
}

impl RegistrationDeclCommon for ResolverRegistration {
    const TYPE: &'static str = "resolver";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fsys::DebugRegistration, fdecl::DebugRegistration")]
pub enum DebugRegistration {
    Protocol(DebugProtocolRegistration),
}

impl RegistrationDeclCommon for DebugRegistration {
    const TYPE: &'static str = "debug_protocol";

    fn source(&self) -> &RegistrationSource {
        match self {
            DebugRegistration::Protocol(protocol_reg) => &protocol_reg.source,
        }
    }
}

impl SourceName for DebugRegistration {
    fn source_name(&self) -> &CapabilityName {
        match self {
            DebugRegistration::Protocol(protocol_reg) => &protocol_reg.source_name,
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fsys::DebugProtocolRegistration, fdecl::DebugProtocolRegistration")]
pub struct DebugProtocolRegistration {
    pub source_name: CapabilityName,
    pub source: RegistrationSource,
    pub target_name: CapabilityName,
}

#[derive(FidlDecl, Debug, Clone, PartialEq)]
#[fidl_decl(fidl_table = "fsys::ProgramDecl, fdecl::Program")]
pub struct ProgramDecl {
    pub runner: Option<CapabilityName>,
    pub info: fdata::Dictionary,
}

impl Default for ProgramDecl {
    fn default() -> Self {
        Self { runner: None, info: fdata::Dictionary::EMPTY.clone() }
    }
}

fidl_translations_identical!(u8);
fidl_translations_identical!(u32);
fidl_translations_identical!(bool);
fidl_translations_identical!(String);
fidl_translations_identical!(fsys::StartupMode);
fidl_translations_identical!(fsys::OnTerminate);
fidl_translations_identical!(fsys::Durability);
fidl_translations_identical!(fdata::Dictionary);
fidl_translations_identical!(fio2::Operations);
fidl_translations_identical!(fsys::EnvironmentExtends);
fidl_translations_identical!(fsys::StorageId);
fidl_translations_identical!(Vec<fprocess::HandleInfo>);

fidl_translations_from_into!(cm_types::AllowedOffers, fsys::AllowedOffers);
fidl_translations_from_into!(CapabilityName, String);

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyType {
    Strong,
    Weak,
    WeakForMigration,
}

fidl_translations_symmetrical_enums!(
    fsys::DependencyType,
    DependencyType,
    Strong,
    Weak,
    WeakForMigration
);
fidl_translations_symmetrical_enums!(
    fdecl::DependencyType,
    DependencyType,
    Strong,
    Weak,
    WeakForMigration
);

/// A path to a capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityPath {
    /// The directory containing the last path element, e.g. `/svc/foo` in `/svc/foo/bar`.
    pub dirname: String,
    /// The last path element: e.g. `bar` in `/svc/foo/bar`.
    pub basename: String,
}

impl CapabilityPath {
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(self.to_string())
    }

    /// Splits the path according to "/", ignoring empty path components
    pub fn split(&self) -> Vec<String> {
        self.to_string().split("/").map(|s| s.to_string()).filter(|s| !s.is_empty()).collect()
    }
}

impl FromStr for CapabilityPath {
    type Err = Error;

    fn from_str(path: &str) -> Result<CapabilityPath, Error> {
        cm_types::Path::validate(path)
            .map_err(|_| Error::InvalidCapabilityPath { raw: path.to_string() })?;
        let idx = path.rfind('/').expect("path validation is wrong");
        Ok(CapabilityPath {
            dirname: if idx == 0 { "/".to_string() } else { path[0..idx].to_string() },
            basename: path[idx + 1..].to_string(),
        })
    }
}

impl TryFrom<&str> for CapabilityPath {
    type Error = Error;

    fn try_from(path: &str) -> Result<CapabilityPath, Error> {
        Self::from_str(path)
    }
}

impl fmt::Display for CapabilityPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if &self.dirname == "/" {
            write!(f, "/{}", self.basename)
        } else {
            write!(f, "{}/{}", self.dirname, self.basename)
        }
    }
}

impl UseDecl {
    pub fn path(&self) -> Option<&CapabilityPath> {
        match self {
            UseDecl::Service(d) => Some(&d.target_path),
            UseDecl::Protocol(d) => Some(&d.target_path),
            UseDecl::Directory(d) => Some(&d.target_path),
            UseDecl::Storage(d) => Some(&d.target_path),
            UseDecl::Event(_) | UseDecl::EventStream(_) => None,
        }
    }

    pub fn name(&self) -> Option<&CapabilityName> {
        match self {
            UseDecl::Event(event_decl) => Some(&event_decl.source_name),
            UseDecl::Storage(storage_decl) => Some(&storage_decl.source_name),
            UseDecl::EventStream(event_stream_decl) => Some(&event_stream_decl.name),
            UseDecl::Service(_) | UseDecl::Protocol(_) | UseDecl::Directory(_) => None,
        }
    }
}

/// A named capability.
///
/// Unlike a `CapabilityPath`, a `CapabilityName` doesn't encode any form
/// of hierarchy.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityName(pub String);

impl CapabilityName {
    pub fn str(&self) -> &str {
        &self.0
    }
}

impl From<CapabilityName> for String {
    fn from(name: CapabilityName) -> String {
        name.0
    }
}

impl From<&str> for CapabilityName {
    fn from(name: &str) -> CapabilityName {
        CapabilityName(name.to_string())
    }
}

impl From<String> for CapabilityName {
    fn from(name: String) -> CapabilityName {
        CapabilityName(name)
    }
}

impl From<cm_types::Name> for CapabilityName {
    fn from(name: cm_types::Name) -> CapabilityName {
        name.as_str().into()
    }
}

impl<'a> PartialEq<&'a str> for CapabilityName {
    fn eq(&self, other: &&'a str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The trait for all declarations that have a source name.
pub trait SourceName {
    fn source_name(&self) -> &CapabilityName;
}

/// The common properties of a [Use](fsys2::UseDecl) declaration.
pub trait UseDeclCommon: SourceName + Send + Sync {
    fn source(&self) -> &UseSource;
}

/// The common properties of a Registration-with-environment declaration.
pub trait RegistrationDeclCommon: SourceName + Send + Sync {
    /// The name of the registration type, for error messages.
    const TYPE: &'static str;
    fn source(&self) -> &RegistrationSource;
}

/// The common properties of an [Offer](fsys2::OfferDecl) declaration.
pub trait OfferDeclCommon: SourceName + Send + Sync {
    fn target_name(&self) -> &CapabilityName;
    fn target(&self) -> &OfferTarget;
    fn source(&self) -> &OfferSource;
}

/// The common properties of an [Expose](fsys2::ExposeDecl) declaration.
pub trait ExposeDeclCommon: SourceName + Send + Sync {
    fn target_name(&self) -> &CapabilityName;
    fn target(&self) -> &ExposeTarget;
    fn source(&self) -> &ExposeSource;
}

/// The common properties of a [Capability](fsys2::CapabilityDecl) declaration.
pub trait CapabilityDeclCommon: Send + Sync {
    fn name(&self) -> &CapabilityName;
}

/// A named capability type.
///
/// `CapabilityTypeName` provides a user friendly type encoding for a capability.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CapabilityTypeName {
    Directory,
    Event,
    EventStream,
    Protocol,
    Resolver,
    Runner,
    Service,
    Storage,
}

impl fmt::Display for CapabilityTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let display_name = match &self {
            CapabilityTypeName::Directory => "directory",
            CapabilityTypeName::Event => "event",
            CapabilityTypeName::EventStream => "event_stream",
            CapabilityTypeName::Protocol => "protocol",
            CapabilityTypeName::Resolver => "resolver",
            CapabilityTypeName::Runner => "runner",
            CapabilityTypeName::Service => "service",
            CapabilityTypeName::Storage => "storage",
        };
        write!(f, "{}", display_name)
    }
}

impl From<&UseDecl> for CapabilityTypeName {
    fn from(use_decl: &UseDecl) -> Self {
        match use_decl {
            UseDecl::Service(_) => Self::Service,
            UseDecl::Protocol(_) => Self::Protocol,
            UseDecl::Directory(_) => Self::Directory,
            UseDecl::Storage(_) => Self::Storage,
            UseDecl::Event(_) => Self::Event,
            UseDecl::EventStream(_) => Self::EventStream,
        }
    }
}

impl From<&ExposeDecl> for CapabilityTypeName {
    fn from(expose_decl: &ExposeDecl) -> Self {
        match expose_decl {
            ExposeDecl::Service(_) => Self::Service,
            ExposeDecl::Protocol(_) => Self::Protocol,
            ExposeDecl::Directory(_) => Self::Directory,
            ExposeDecl::Runner(_) => Self::Runner,
            ExposeDecl::Resolver(_) => Self::Resolver,
        }
    }
}

// TODO: Runners and third parties can use this to parse `facets`.
impl FidlIntoNative<HashMap<String, DictionaryValue>> for fdata::Dictionary {
    fn fidl_into_native(self) -> HashMap<String, DictionaryValue> {
        from_fidl_dict(self)
    }
}

impl NativeIntoFidl<fdata::Dictionary> for HashMap<String, DictionaryValue> {
    fn native_into_fidl(self) -> fdata::Dictionary {
        to_fidl_dict(self)
    }
}

impl FidlIntoNative<CapabilityPath> for String {
    fn fidl_into_native(self) -> CapabilityPath {
        self.as_str().parse().unwrap()
    }
}

impl NativeIntoFidl<String> for CapabilityPath {
    fn native_into_fidl(self) -> String {
        self.to_string()
    }
}

impl FidlIntoNative<PathBuf> for String {
    fn fidl_into_native(self) -> PathBuf {
        PathBuf::from(self)
    }
}

impl NativeIntoFidl<String> for PathBuf {
    fn native_into_fidl(self) -> String {
        self.into_os_string().into_string().expect("invalid utf8")
    }
}

#[derive(Debug, PartialEq)]
pub enum Value {
    Bit(bool),
    Inum(i64),
    Fnum(f64),
    Str(String),
    Vec(Vec<Value>),
    Obj(HashMap<String, Value>),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictionaryValue {
    Str(String),
    StrVec(Vec<String>),
    Null,
}

impl FidlIntoNative<DictionaryValue> for Option<Box<fdata::DictionaryValue>> {
    fn fidl_into_native(self) -> DictionaryValue {
        match self {
            Some(v) => match *v {
                fdata::DictionaryValue::Str(s) => DictionaryValue::Str(s),
                fdata::DictionaryValue::StrVec(ss) => DictionaryValue::StrVec(ss),
            },
            None => DictionaryValue::Null,
        }
    }
}

impl NativeIntoFidl<Option<Box<fdata::DictionaryValue>>> for DictionaryValue {
    fn native_into_fidl(self) -> Option<Box<fdata::DictionaryValue>> {
        match self {
            DictionaryValue::Str(s) => Some(Box::new(fdata::DictionaryValue::Str(s))),
            DictionaryValue::StrVec(ss) => Some(Box::new(fdata::DictionaryValue::StrVec(ss))),
            DictionaryValue::Null => None,
        }
    }
}

fn from_fidl_dict(dict: fdata::Dictionary) -> HashMap<String, DictionaryValue> {
    match dict.entries {
        Some(entries) => entries.into_iter().map(|e| (e.key, e.value.fidl_into_native())).collect(),
        _ => HashMap::new(),
    }
}

fn to_fidl_dict(dict: HashMap<String, DictionaryValue>) -> fdata::Dictionary {
    fdata::Dictionary {
        entries: Some(
            dict.into_iter()
                .map(|(key, value)| fdata::DictionaryEntry { key, value: value.native_into_fidl() })
                .collect(),
        ),
        ..fdata::Dictionary::EMPTY
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseSource {
    Parent,
    Framework,
    Debug,
    Self_,
    Capability(CapabilityName),
    Child(String),
}

impl FidlIntoNative<UseSource> for fsys::Ref {
    fn fidl_into_native(self) -> UseSource {
        match self {
            fsys::Ref::Parent(_) => UseSource::Parent,
            fsys::Ref::Framework(_) => UseSource::Framework,
            fsys::Ref::Debug(_) => UseSource::Debug,
            fsys::Ref::Self_(_) => UseSource::Self_,
            fsys::Ref::Capability(c) => UseSource::Capability(c.name.into()),
            fsys::Ref::Child(c) => UseSource::Child(c.name),
            _ => panic!("invalid UseSource variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for UseSource {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            UseSource::Parent => fsys::Ref::Parent(fsys::ParentRef {}),
            UseSource::Framework => fsys::Ref::Framework(fsys::FrameworkRef {}),
            UseSource::Debug => fsys::Ref::Debug(fsys::DebugRef {}),
            UseSource::Self_ => fsys::Ref::Self_(fsys::SelfRef {}),
            UseSource::Capability(name) => {
                fsys::Ref::Capability(fsys::CapabilityRef { name: name.to_string() })
            }
            UseSource::Child(name) => fsys::Ref::Child(fsys::ChildRef { name, collection: None }),
        }
    }
}

impl FidlIntoNative<UseSource> for fdecl::Ref {
    fn fidl_into_native(self) -> UseSource {
        match self {
            fdecl::Ref::Parent(_) => UseSource::Parent,
            fdecl::Ref::Framework(_) => UseSource::Framework,
            fdecl::Ref::Debug(_) => UseSource::Debug,
            fdecl::Ref::Self_(_) => UseSource::Self_,
            fdecl::Ref::Capability(c) => UseSource::Capability(c.name.into()),
            fdecl::Ref::Child(c) => UseSource::Child(c.name),
            _ => panic!("invalid UseSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for UseSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            UseSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            UseSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            UseSource::Debug => fdecl::Ref::Debug(fdecl::DebugRef {}),
            UseSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            UseSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
            UseSource::Child(name) => fdecl::Ref::Child(fdecl::ChildRef { name, collection: None }),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferSource {
    Framework,
    Parent,
    Child(ChildRef),
    Collection(String),
    Self_,
    Capability(CapabilityName),
}

impl OfferSource {
    pub fn static_child(name: String) -> Self {
        Self::Child(ChildRef { name, collection: None })
    }
}

impl FidlIntoNative<OfferSource> for fsys::Ref {
    fn fidl_into_native(self) -> OfferSource {
        match self {
            fsys::Ref::Parent(_) => OfferSource::Parent,
            fsys::Ref::Self_(_) => OfferSource::Self_,
            fsys::Ref::Child(c) => OfferSource::Child(c.fidl_into_native()),
            fsys::Ref::Collection(c) => OfferSource::Collection(c.name),
            fsys::Ref::Framework(_) => OfferSource::Framework,
            fsys::Ref::Capability(c) => OfferSource::Capability(c.name.into()),
            _ => panic!("invalid OfferSource variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for OfferSource {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            OfferSource::Parent => fsys::Ref::Parent(fsys::ParentRef {}),
            OfferSource::Self_ => fsys::Ref::Self_(fsys::SelfRef {}),
            OfferSource::Child(c) => fsys::Ref::Child(c.native_into_fidl()),
            OfferSource::Collection(name) => fsys::Ref::Collection(fsys::CollectionRef { name }),
            OfferSource::Framework => fsys::Ref::Framework(fsys::FrameworkRef {}),
            OfferSource::Capability(name) => {
                fsys::Ref::Capability(fsys::CapabilityRef { name: name.to_string() })
            }
        }
    }
}

impl FidlIntoNative<OfferSource> for fdecl::Ref {
    fn fidl_into_native(self) -> OfferSource {
        match self {
            fdecl::Ref::Parent(_) => OfferSource::Parent,
            fdecl::Ref::Self_(_) => OfferSource::Self_,
            fdecl::Ref::Child(c) => OfferSource::Child(c.fidl_into_native()),
            fdecl::Ref::Collection(c) => OfferSource::Collection(c.name),
            fdecl::Ref::Framework(_) => OfferSource::Framework,
            fdecl::Ref::Capability(c) => OfferSource::Capability(c.name.into()),
            _ => panic!("invalid OfferSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for OfferSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            OfferSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            OfferSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            OfferSource::Child(c) => fdecl::Ref::Child(c.native_into_fidl()),
            OfferSource::Collection(name) => fdecl::Ref::Collection(fdecl::CollectionRef { name }),
            OfferSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            OfferSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExposeSource {
    Self_,
    Child(String),
    Collection(String),
    Framework,
    Capability(CapabilityName),
}

impl FidlIntoNative<ExposeSource> for fsys::Ref {
    fn fidl_into_native(self) -> ExposeSource {
        match self {
            fsys::Ref::Self_(_) => ExposeSource::Self_,
            fsys::Ref::Child(c) => ExposeSource::Child(c.name),
            fsys::Ref::Collection(c) => ExposeSource::Collection(c.name),
            fsys::Ref::Framework(_) => ExposeSource::Framework,
            fsys::Ref::Capability(c) => ExposeSource::Capability(c.name.into()),
            _ => panic!("invalid ExposeSource variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for ExposeSource {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            ExposeSource::Self_ => fsys::Ref::Self_(fsys::SelfRef {}),
            ExposeSource::Child(child_name) => {
                fsys::Ref::Child(fsys::ChildRef { name: child_name, collection: None })
            }
            ExposeSource::Collection(name) => fsys::Ref::Collection(fsys::CollectionRef { name }),
            ExposeSource::Framework => fsys::Ref::Framework(fsys::FrameworkRef {}),
            ExposeSource::Capability(name) => {
                fsys::Ref::Capability(fsys::CapabilityRef { name: name.to_string() })
            }
        }
    }
}

impl FidlIntoNative<ExposeSource> for fdecl::Ref {
    fn fidl_into_native(self) -> ExposeSource {
        match self {
            fdecl::Ref::Self_(_) => ExposeSource::Self_,
            fdecl::Ref::Child(c) => ExposeSource::Child(c.name),
            fdecl::Ref::Collection(c) => ExposeSource::Collection(c.name),
            fdecl::Ref::Framework(_) => ExposeSource::Framework,
            fdecl::Ref::Capability(c) => ExposeSource::Capability(c.name.into()),
            _ => panic!("invalid ExposeSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for ExposeSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            ExposeSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            ExposeSource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
            ExposeSource::Collection(name) => fdecl::Ref::Collection(fdecl::CollectionRef { name }),
            ExposeSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            ExposeSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExposeTarget {
    Parent,
    Framework,
}

impl FidlIntoNative<ExposeTarget> for fsys::Ref {
    fn fidl_into_native(self) -> ExposeTarget {
        match self {
            fsys::Ref::Parent(_) => ExposeTarget::Parent,
            fsys::Ref::Framework(_) => ExposeTarget::Framework,
            _ => panic!("invalid ExposeTarget variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for ExposeTarget {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            ExposeTarget::Parent => fsys::Ref::Parent(fsys::ParentRef {}),
            ExposeTarget::Framework => fsys::Ref::Framework(fsys::FrameworkRef {}),
        }
    }
}

impl FidlIntoNative<ExposeTarget> for fdecl::Ref {
    fn fidl_into_native(self) -> ExposeTarget {
        match self {
            fdecl::Ref::Parent(_) => ExposeTarget::Parent,
            fdecl::Ref::Framework(_) => ExposeTarget::Framework,
            _ => panic!("invalid ExposeTarget variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for ExposeTarget {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            ExposeTarget::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            ExposeTarget::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
        }
    }
}

/// A source for a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSource<T> {
    /// The provider of the service, relative to a component.
    pub source: T,
    /// The name of the service.
    pub source_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageDirectorySource {
    Parent,
    Self_,
    Child(String),
}

impl FidlIntoNative<StorageDirectorySource> for fsys::Ref {
    fn fidl_into_native(self) -> StorageDirectorySource {
        match self {
            fsys::Ref::Parent(_) => StorageDirectorySource::Parent,
            fsys::Ref::Self_(_) => StorageDirectorySource::Self_,
            fsys::Ref::Child(c) => StorageDirectorySource::Child(c.name),
            _ => panic!("invalid OfferDirectorySource variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for StorageDirectorySource {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            StorageDirectorySource::Parent => fsys::Ref::Parent(fsys::ParentRef {}),
            StorageDirectorySource::Self_ => fsys::Ref::Self_(fsys::SelfRef {}),
            StorageDirectorySource::Child(child_name) => {
                fsys::Ref::Child(fsys::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

impl FidlIntoNative<StorageDirectorySource> for fdecl::Ref {
    fn fidl_into_native(self) -> StorageDirectorySource {
        match self {
            fdecl::Ref::Parent(_) => StorageDirectorySource::Parent,
            fdecl::Ref::Self_(_) => StorageDirectorySource::Self_,
            fdecl::Ref::Child(c) => StorageDirectorySource::Child(c.name),
            _ => panic!("invalid OfferDirectorySource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for StorageDirectorySource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            StorageDirectorySource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            StorageDirectorySource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            StorageDirectorySource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationSource {
    Parent,
    Self_,
    Child(String),
}

impl FidlIntoNative<RegistrationSource> for fsys::Ref {
    fn fidl_into_native(self) -> RegistrationSource {
        match self {
            fsys::Ref::Parent(_) => RegistrationSource::Parent,
            fsys::Ref::Self_(_) => RegistrationSource::Self_,
            fsys::Ref::Child(c) => RegistrationSource::Child(c.name),
            _ => panic!("invalid RegistrationSource variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for RegistrationSource {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            RegistrationSource::Parent => fsys::Ref::Parent(fsys::ParentRef {}),
            RegistrationSource::Self_ => fsys::Ref::Self_(fsys::SelfRef {}),
            RegistrationSource::Child(child_name) => {
                fsys::Ref::Child(fsys::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

impl FidlIntoNative<RegistrationSource> for fdecl::Ref {
    fn fidl_into_native(self) -> RegistrationSource {
        match self {
            fdecl::Ref::Parent(_) => RegistrationSource::Parent,
            fdecl::Ref::Self_(_) => RegistrationSource::Self_,
            fdecl::Ref::Child(c) => RegistrationSource::Child(c.name),
            _ => panic!("invalid RegistrationSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for RegistrationSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            RegistrationSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            RegistrationSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            RegistrationSource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OfferTarget {
    Child(ChildRef),
    Collection(String),
}
impl OfferTarget {
    pub fn static_child(name: String) -> Self {
        Self::Child(ChildRef { name, collection: None })
    }
}

impl FidlIntoNative<OfferTarget> for fsys::Ref {
    fn fidl_into_native(self) -> OfferTarget {
        match self {
            fsys::Ref::Child(c) => OfferTarget::Child(c.fidl_into_native()),
            fsys::Ref::Collection(c) => OfferTarget::Collection(c.name),
            _ => panic!("invalid OfferTarget variant"),
        }
    }
}

impl NativeIntoFidl<fsys::Ref> for OfferTarget {
    fn native_into_fidl(self) -> fsys::Ref {
        match self {
            OfferTarget::Child(c) => fsys::Ref::Child(c.native_into_fidl()),
            OfferTarget::Collection(collection_name) => {
                fsys::Ref::Collection(fsys::CollectionRef { name: collection_name })
            }
        }
    }
}

impl FidlIntoNative<OfferTarget> for fdecl::Ref {
    fn fidl_into_native(self) -> OfferTarget {
        match self {
            fdecl::Ref::Child(c) => OfferTarget::Child(c.fidl_into_native()),
            fdecl::Ref::Collection(c) => OfferTarget::Collection(c.name),
            _ => panic!("invalid OfferTarget variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for OfferTarget {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            OfferTarget::Child(c) => fdecl::Ref::Child(c.native_into_fidl()),
            OfferTarget::Collection(collection_name) => {
                fdecl::Ref::Collection(fdecl::CollectionRef { name: collection_name })
            }
        }
    }
}

/// Converts the contents of a CM-FIDL declaration and produces the equivalent CM-Rust
/// struct.
/// This function applies cm_fidl_validator to check correctness.
impl TryFrom<fsys::ComponentDecl> for ComponentDecl {
    type Error = Error;

    fn try_from(decl: fsys::ComponentDecl) -> Result<Self, Self::Error> {
        cm_fidl_validator::fsys::validate(&decl).map_err(|err| Error::Validate { err })?;
        Ok(decl.fidl_into_native())
    }
}

// Converts the contents of a CM-Rust declaration into a CM_FIDL declaration
impl TryFrom<ComponentDecl> for fsys::ComponentDecl {
    type Error = Error;
    fn try_from(decl: ComponentDecl) -> Result<Self, Self::Error> {
        Ok(decl.native_into_fidl())
    }
}

/// Errors produced by cm_rust.
#[derive(Debug, Error, Clone)]
pub enum Error {
    #[error("Fidl validation failed: {}", err)]
    Validate {
        #[source]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("Invalid capability path: {}", raw)]
    InvalidCapabilityPath { raw: String },
}

#[cfg(test)]
mod tests {
    use {super::*, maplit::hashmap, std::convert::TryInto};

    macro_rules! test_try_from_decl {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    {
                        let res = ComponentDecl::try_from($input).expect("try_from failed");
                        assert_eq!(res, $result);
                    }
                    {
                        let res = fsys::ComponentDecl::try_from($result).expect("try_from failed");
                        assert_eq!(res, $input);
                    }
                }
            )+
        }
    }

    macro_rules! test_fidl_into_and_from {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    input_type = $input_type:ty,
                    result = $result:expr,
                    result_type = $result_type:ty,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    {
                        let res: Vec<$result_type> =
                            $input.into_iter().map(|e| e.fidl_into_native()).collect();
                        assert_eq!(res, $result);
                    }
                    {
                        let res: Vec<$input_type> =
                            $result.into_iter().map(|e| e.native_into_fidl()).collect();
                        assert_eq!(res, $input);
                    }
                }
            )+
        }
    }

    macro_rules! test_fidl_into {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    test_fidl_into_helper($input, $result);
                }
            )+
        }
    }

    fn test_fidl_into_helper<T, U>(input: T, expected_res: U)
    where
        T: FidlIntoNative<U>,
        U: std::cmp::PartialEq + std::fmt::Debug,
    {
        let res: U = input.fidl_into_native();
        assert_eq!(res, expected_res);
    }

    macro_rules! test_capability_path {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    test_capability_path_helper($input, $result);
                }
            )+
        }
    }

    fn test_capability_path_helper(input: &str, result: Result<CapabilityPath, Error>) {
        let res = CapabilityPath::try_from(input);
        assert_eq!(format!("{:?}", res), format!("{:?}", result));
        if let Ok(p) = res {
            assert_eq!(&p.to_string(), input);
        }
    }

    test_try_from_decl! {
        try_from_empty => {
            input = fsys::ComponentDecl {
                program: None,
                uses: None,
                exposes: None,
                offers: None,
                capabilities: None,
                children: None,
                collections: None,
                facets: None,
                environments: None,
                ..fsys::ComponentDecl::EMPTY
            },
            result = ComponentDecl {
                program: None,
                uses: vec![],
                exposes: vec![],
                offers: vec![],
                capabilities: vec![],
                children: vec![],
                collections: vec![],
                facets: None,
                environments: vec![],
                config: None,
            },
        },
        try_from_all => {
            input = fsys::ComponentDecl {
                program: Some(fsys::ProgramDecl {
                    runner: Some("elf".to_string()),
                    info: Some(fdata::Dictionary {
                        entries: Some(vec![
                            fdata::DictionaryEntry {
                                key: "args".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["foo".to_string(), "bar".to_string()]))),
                            },
                            fdata::DictionaryEntry {
                                key: "binary".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::Str("bin/app".to_string()))),
                            },
                        ]),
                        ..fdata::Dictionary::EMPTY
                    }),
                    ..fsys::ProgramDecl::EMPTY
                }),
                uses: Some(vec![
                    fsys::UseDecl::Service(fsys::UseServiceDecl {
                        dependency_type: Some(fsys::DependencyType::Strong),
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("netstack".to_string()),
                        target_path: Some("/svc/mynetstack".to_string()),
                        ..fsys::UseServiceDecl::EMPTY
                    }),
                    fsys::UseDecl::Protocol(fsys::UseProtocolDecl {
                        dependency_type: Some(fsys::DependencyType::Strong),
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("legacy_netstack".to_string()),
                        target_path: Some("/svc/legacy_mynetstack".to_string()),
                        ..fsys::UseProtocolDecl::EMPTY
                    }),
                    fsys::UseDecl::Protocol(fsys::UseProtocolDecl {
                        dependency_type: Some(fsys::DependencyType::Strong),
                        source: Some(fsys::Ref::Child(fsys::ChildRef { name: "echo".to_string(), collection: None})),
                        source_name: Some("echo_service".to_string()),
                        target_path: Some("/svc/echo_service".to_string()),
                        ..fsys::UseProtocolDecl::EMPTY
                    }),
                    fsys::UseDecl::Directory(fsys::UseDirectoryDecl {
                        dependency_type: Some(fsys::DependencyType::Strong),
                        source: Some(fsys::Ref::Framework(fsys::FrameworkRef {})),
                        source_name: Some("dir".to_string()),
                        target_path: Some("/data".to_string()),
                        rights: Some(fio2::Operations::Connect),
                        subdir: Some("foo/bar".to_string()),
                        ..fsys::UseDirectoryDecl::EMPTY
                    }),
                    fsys::UseDecl::Storage(fsys::UseStorageDecl {
                        source_name: Some("cache".to_string()),
                        target_path: Some("/cache".to_string()),
                        ..fsys::UseStorageDecl::EMPTY
                    }),
                    fsys::UseDecl::Storage(fsys::UseStorageDecl {
                        source_name: Some("temp".to_string()),
                        target_path: Some("/temp".to_string()),
                        ..fsys::UseStorageDecl::EMPTY
                    }),
                    fsys::UseDecl::Event(fsys::UseEventDecl {
                        dependency_type: Some(fsys::DependencyType::Strong),
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("directory_ready".to_string()),
                        target_name: Some("diagnostics_ready".to_string()),
                        filter: Some(fdata::Dictionary{
                            entries: Some(vec![
                               fdata::DictionaryEntry {
                                   key: "path".to_string(),
                                   value: Some(Box::new(fdata::DictionaryValue::Str("/diagnostics".to_string()))),
                               },
                            ]),
                            ..fdata::Dictionary::EMPTY
                        }),
                        mode: Some(fsys::EventMode::Sync),
                        ..fsys::UseEventDecl::EMPTY
                    }),
                ]),
                exposes: Some(vec![
                    fsys::ExposeDecl::Protocol(fsys::ExposeProtocolDecl {
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("legacy_netstack".to_string()),
                        target_name: Some("legacy_mynetstack".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        ..fsys::ExposeProtocolDecl::EMPTY
                    }),
                    fsys::ExposeDecl::Directory(fsys::ExposeDirectoryDecl {
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("dir".to_string()),
                        target_name: Some("data".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        rights: Some(fio2::Operations::Connect),
                        subdir: Some("foo/bar".to_string()),
                        ..fsys::ExposeDirectoryDecl::EMPTY
                    }),
                    fsys::ExposeDecl::Runner(fsys::ExposeRunnerDecl {
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("elf".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        target_name: Some("elf".to_string()),
                        ..fsys::ExposeRunnerDecl::EMPTY
                    }),
                    fsys::ExposeDecl::Resolver(fsys::ExposeResolverDecl{
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("pkg".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef{})),
                        target_name: Some("pkg".to_string()),
                        ..fsys::ExposeResolverDecl::EMPTY
                    }),
                    fsys::ExposeDecl::Service(fsys::ExposeServiceDecl {
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("netstack1".to_string()),
                        target_name: Some("mynetstack".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        ..fsys::ExposeServiceDecl::EMPTY
                    }),
                    fsys::ExposeDecl::Service(fsys::ExposeServiceDecl {
                        source: Some(fsys::Ref::Child(fsys::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("netstack2".to_string()),
                        target_name: Some("mynetstack".to_string()),
                        target: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        ..fsys::ExposeServiceDecl::EMPTY
                    }),
                ]),
                offers: Some(vec![
                    fsys::OfferDecl::Protocol(fsys::OfferProtocolDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("legacy_netstack".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("legacy_mynetstack".to_string()),
                        dependency_type: Some(fsys::DependencyType::WeakForMigration),
                        ..fsys::OfferProtocolDecl::EMPTY
                    }),
                    fsys::OfferDecl::Directory(fsys::OfferDirectoryDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("dir".to_string()),
                        target: Some(fsys::Ref::Collection(
                            fsys::CollectionRef { name: "modular".to_string() }
                        )),
                        target_name: Some("data".to_string()),
                        rights: Some(fio2::Operations::Connect),
                        subdir: None,
                        dependency_type: Some(fsys::DependencyType::Strong),
                        ..fsys::OfferDirectoryDecl::EMPTY
                    }),
                    fsys::OfferDecl::Storage(fsys::OfferStorageDecl {
                        source_name: Some("cache".to_string()),
                        source: Some(fsys::Ref::Self_(fsys::SelfRef {})),
                        target: Some(fsys::Ref::Collection(
                            fsys::CollectionRef { name: "modular".to_string() }
                        )),
                        target_name: Some("cache".to_string()),
                        ..fsys::OfferStorageDecl::EMPTY
                    }),
                    fsys::OfferDecl::Runner(fsys::OfferRunnerDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("elf".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("elf2".to_string()),
                        ..fsys::OfferRunnerDecl::EMPTY
                    }),
                    fsys::OfferDecl::Resolver(fsys::OfferResolverDecl{
                        source: Some(fsys::Ref::Parent(fsys::ParentRef{})),
                        source_name: Some("pkg".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                              name: "echo".to_string(),
                              collection: None,
                           }
                        )),
                        target_name: Some("pkg".to_string()),
                        ..fsys::OfferResolverDecl::EMPTY
                    }),
                    fsys::OfferDecl::Event(fsys::OfferEventDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("started".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mystarted".to_string()),
                        filter: Some(fdata::Dictionary {
                            entries: Some(vec![
                               fdata::DictionaryEntry {
                                   key: "path".to_string(),
                                   value: Some(Box::new(fdata::DictionaryValue::Str("/a".to_string()))),
                               },
                            ]),
                            ..fdata::Dictionary::EMPTY
                        }),
                        mode: Some(fsys::EventMode::Sync),
                        ..fsys::OfferEventDecl::EMPTY
                    }),
                    fsys::OfferDecl::Service(fsys::OfferServiceDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("netstack1".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mynetstack".to_string()),
                        ..fsys::OfferServiceDecl::EMPTY
                    }),
                    fsys::OfferDecl::Service(fsys::OfferServiceDecl {
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        source_name: Some("netstack2".to_string()),
                        target: Some(fsys::Ref::Child(
                           fsys::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mynetstack".to_string()),
                        ..fsys::OfferServiceDecl::EMPTY
                    }),
                ]),
                capabilities: Some(vec![
                    fsys::CapabilityDecl::Service(fsys::ServiceDecl {
                        name: Some("netstack".to_string()),
                        source_path: Some("/netstack".to_string()),
                        ..fsys::ServiceDecl::EMPTY
                    }),
                    fsys::CapabilityDecl::Protocol(fsys::ProtocolDecl {
                        name: Some("netstack2".to_string()),
                        source_path: Some("/netstack2".to_string()),
                        ..fsys::ProtocolDecl::EMPTY
                    }),
                    fsys::CapabilityDecl::Directory(fsys::DirectoryDecl {
                        name: Some("data".to_string()),
                        source_path: Some("/data".to_string()),
                        rights: Some(fio2::Operations::Connect),
                        ..fsys::DirectoryDecl::EMPTY
                    }),
                    fsys::CapabilityDecl::Storage(fsys::StorageDecl {
                        name: Some("cache".to_string()),
                        backing_dir: Some("data".to_string()),
                        source: Some(fsys::Ref::Parent(fsys::ParentRef {})),
                        subdir: Some("cache".to_string()),
                        storage_id: Some(fsys::StorageId::StaticInstanceId),
                        ..fsys::StorageDecl::EMPTY
                    }),
                    fsys::CapabilityDecl::Runner(fsys::RunnerDecl {
                        name: Some("elf".to_string()),
                        source_path: Some("/elf".to_string()),
                        ..fsys::RunnerDecl::EMPTY
                    }),
                    fsys::CapabilityDecl::Resolver(fsys::ResolverDecl {
                        name: Some("pkg".to_string()),
                        source_path: Some("/pkg_resolver".to_string()),
                        ..fsys::ResolverDecl::EMPTY
                    }),
                ]),
                children: Some(vec![
                     fsys::ChildDecl {
                         name: Some("netstack".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/netstack#meta/netstack.cm"
                                   .to_string()),
                         startup: Some(fsys::StartupMode::Lazy),
                         on_terminate: None,
                         environment: None,
                         ..fsys::ChildDecl::EMPTY
                     },
                     fsys::ChildDecl {
                         name: Some("gtest".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/gtest#meta/gtest.cm".to_string()),
                         startup: Some(fsys::StartupMode::Lazy),
                         on_terminate: Some(fsys::OnTerminate::None),
                         environment: None,
                         ..fsys::ChildDecl::EMPTY
                     },
                     fsys::ChildDecl {
                         name: Some("echo".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/echo#meta/echo.cm"
                                   .to_string()),
                         startup: Some(fsys::StartupMode::Eager),
                         on_terminate: Some(fsys::OnTerminate::Reboot),
                         environment: Some("test_env".to_string()),
                         ..fsys::ChildDecl::EMPTY
                     },
                ]),
                collections: Some(vec![
                     fsys::CollectionDecl {
                         name: Some("modular".to_string()),
                         durability: Some(fsys::Durability::Persistent),
                         allowed_offers: Some(fsys::AllowedOffers::StaticOnly),
                         environment: None,
                         ..fsys::CollectionDecl::EMPTY
                     },
                     fsys::CollectionDecl {
                         name: Some("tests".to_string()),
                         durability: Some(fsys::Durability::Transient),
                         allowed_offers: Some(fsys::AllowedOffers::StaticAndDynamic),
                         environment: Some("test_env".to_string()),
                         ..fsys::CollectionDecl::EMPTY
                     },
                ]),
                facets: Some(fdata::Dictionary {
                    entries: Some(vec![
                        fdata::DictionaryEntry {
                            key: "author".to_string(),
                            value: Some(Box::new(fdata::DictionaryValue::Str("Fuchsia".to_string()))),
                        },
                    ]),
                    ..fdata::Dictionary::EMPTY
                }),
                environments: Some(vec![
                    fsys::EnvironmentDecl {
                        name: Some("test_env".to_string()),
                        extends: Some(fsys::EnvironmentExtends::Realm),
                        runners: Some(vec![
                            fsys::RunnerRegistration {
                                source_name: Some("runner".to_string()),
                                source: Some(fsys::Ref::Child(fsys::ChildRef {
                                    name: "gtest".to_string(),
                                    collection: None,
                                })),
                                target_name: Some("gtest-runner".to_string()),
                                ..fsys::RunnerRegistration::EMPTY
                            }
                        ]),
                        resolvers: Some(vec![
                            fsys::ResolverRegistration {
                                resolver: Some("pkg_resolver".to_string()),
                                source: Some(fsys::Ref::Parent(fsys::ParentRef{})),
                                scheme: Some("fuchsia-pkg".to_string()),
                                ..fsys::ResolverRegistration::EMPTY
                            }
                        ]),
                        debug_capabilities: Some(vec![
                         fsys::DebugRegistration::Protocol(fsys::DebugProtocolRegistration {
                             source_name: Some("some_protocol".to_string()),
                             source: Some(fsys::Ref::Child(fsys::ChildRef {
                                 name: "gtest".to_string(),
                                 collection: None,
                             })),
                             target_name: Some("some_protocol".to_string()),
                             ..fsys::DebugProtocolRegistration::EMPTY
                            })
                        ]),
                        stop_timeout_ms: Some(4567),
                        ..fsys::EnvironmentDecl::EMPTY
                    }
                ]),
                config: Some(fsys::ConfigDecl {
                    fields: Some(vec![
                        fsys::ConfigField {
                            key: Some("enable_logging".to_string()),
                            value_type: Some(fsys::ConfigValueType::Bool(fsys::ConfigBooleanType::EMPTY)),
                            ..fsys::ConfigField::EMPTY
                        }
                    ]),
                    declaration_checksum: Some(vec![
                        0x66, 0x3E, 0x10, 0x72, 0x26, 0xA7, 0x5F, 0x0B, 0x23, 0x1D, 0x6C, 0x8D,
                        0x03, 0x6B, 0xC3, 0x58, 0x85, 0x70, 0xB4, 0x4D, 0x8A, 0x11, 0x16, 0x20,
                        0x5B, 0x11, 0x87, 0x96, 0x5E, 0x14, 0x67, 0xF8
                    ]),
                    ..fsys::ConfigDecl::EMPTY
                }),
                ..fsys::ComponentDecl::EMPTY
            },
            result = {
                ComponentDecl {
                    program: Some(ProgramDecl {
                        runner: Some("elf".try_into().unwrap()),
                        info: fdata::Dictionary {
                            entries: Some(vec![
                                fdata::DictionaryEntry {
                                    key: "args".to_string(),
                                    value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["foo".to_string(), "bar".to_string()]))),
                                },
                                fdata::DictionaryEntry{
                                    key: "binary".to_string(),
                                    value: Some(Box::new(fdata::DictionaryValue::Str("bin/app".to_string()))),
                                },
                            ]),
                            ..fdata::Dictionary::EMPTY
                        },
                    }),
                    uses: vec![
                        UseDecl::Service(UseServiceDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Parent,
                            source_name: "netstack".try_into().unwrap(),
                            target_path: "/svc/mynetstack".try_into().unwrap(),
                        }),
                        UseDecl::Protocol(UseProtocolDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Parent,
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target_path: "/svc/legacy_mynetstack".try_into().unwrap(),
                        }),
                        UseDecl::Protocol(UseProtocolDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Child("echo".to_string()),
                            source_name: "echo_service".try_into().unwrap(),
                            target_path: "/svc/echo_service".try_into().unwrap(),
                        }),
                        UseDecl::Directory(UseDirectoryDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Framework,
                            source_name: "dir".try_into().unwrap(),
                            target_path: "/data".try_into().unwrap(),
                            rights: fio2::Operations::Connect,
                            subdir: Some("foo/bar".into()),
                        }),
                        UseDecl::Storage(UseStorageDecl {
                            source_name: "cache".into(),
                            target_path: "/cache".try_into().unwrap(),
                        }),
                        UseDecl::Storage(UseStorageDecl {
                            source_name: "temp".into(),
                            target_path: "/temp".try_into().unwrap(),
                        }),
                        UseDecl::Event(UseEventDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Parent,
                            source_name: "directory_ready".into(),
                            target_name: "diagnostics_ready".into(),
                            filter: Some(hashmap!{"path".to_string() =>  DictionaryValue::Str("/diagnostics".to_string())}),
                            mode: EventMode::Sync,
                        }),
                    ],
                    exposes: vec![
                        ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target_name: "legacy_mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                        }),
                        ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "dir".try_into().unwrap(),
                            target_name: "data".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: Some(fio2::Operations::Connect),
                            subdir: Some("foo/bar".into()),
                        }),
                        ExposeDecl::Runner(ExposeRunnerDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "elf".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            target_name: "elf".try_into().unwrap(),
                        }),
                        ExposeDecl::Resolver(ExposeResolverDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "pkg".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            target_name: "pkg".try_into().unwrap(),
                        }),
                        ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "netstack1".try_into().unwrap(),
                            target_name: "mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                        }),
                        ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "netstack2".try_into().unwrap(),
                            target_name: "mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                        }),
                    ],
                    offers: vec![
                        OfferDecl::Protocol(OfferProtocolDecl {
                            source: OfferSource::Parent,
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "legacy_mynetstack".try_into().unwrap(),
                            dependency_type: DependencyType::WeakForMigration,
                        }),
                        OfferDecl::Directory(OfferDirectoryDecl {
                            source: OfferSource::Parent,
                            source_name: "dir".try_into().unwrap(),
                            target: OfferTarget::Collection("modular".to_string()),
                            target_name: "data".try_into().unwrap(),
                            rights: Some(fio2::Operations::Connect),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                        }),
                        OfferDecl::Storage(OfferStorageDecl {
                            source_name: "cache".try_into().unwrap(),
                            source: OfferSource::Self_,
                            target: OfferTarget::Collection("modular".to_string()),
                            target_name: "cache".try_into().unwrap(),
                        }),
                        OfferDecl::Runner(OfferRunnerDecl {
                            source: OfferSource::Parent,
                            source_name: "elf".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "elf2".try_into().unwrap(),
                        }),
                        OfferDecl::Resolver(OfferResolverDecl {
                            source: OfferSource::Parent,
                            source_name: "pkg".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "pkg".try_into().unwrap(),
                        }),
                        OfferDecl::Event(OfferEventDecl {
                            source: OfferSource::Parent,
                            source_name: "started".into(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mystarted".into(),
                            filter: Some(hashmap!{"path".to_string() => DictionaryValue::Str("/a".to_string())}),
                            mode: EventMode::Sync,
                        }),
                        OfferDecl::Service(OfferServiceDecl {
                                    source: OfferSource::Parent,
                                    source_name: "netstack1".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mynetstack".try_into().unwrap(),
                        }),
                        OfferDecl::Service(OfferServiceDecl {
                                    source: OfferSource::Parent,
                                    source_name: "netstack2".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mynetstack".try_into().unwrap(),
                        }),
                    ],
                    capabilities: vec![
                        CapabilityDecl::Service(ServiceDecl {
                            name: "netstack".into(),
                            source_path: Some("/netstack".try_into().unwrap()),
                        }),
                        CapabilityDecl::Protocol(ProtocolDecl {
                            name: "netstack2".into(),
                            source_path: Some("/netstack2".try_into().unwrap()),
                        }),
                        CapabilityDecl::Directory(DirectoryDecl {
                            name: "data".into(),
                            source_path: Some("/data".try_into().unwrap()),
                            rights: fio2::Operations::Connect,
                        }),
                        CapabilityDecl::Storage(StorageDecl {
                            name: "cache".into(),
                            backing_dir: "data".try_into().unwrap(),
                            source: StorageDirectorySource::Parent,
                            subdir: Some("cache".try_into().unwrap()),
                            storage_id: fsys::StorageId::StaticInstanceId,
                        }),
                        CapabilityDecl::Runner(RunnerDecl {
                            name: "elf".into(),
                            source_path: Some("/elf".try_into().unwrap()),
                        }),
                        CapabilityDecl::Resolver(ResolverDecl {
                            name: "pkg".into(),
                            source_path: Some("/pkg_resolver".try_into().unwrap()),
                        }),
                    ],
                    children: vec![
                        ChildDecl {
                            name: "netstack".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/netstack#meta/netstack.cm".to_string(),
                            startup: fsys::StartupMode::Lazy,
                            on_terminate: None,
                            environment: None,
                        },
                        ChildDecl {
                            name: "gtest".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/gtest#meta/gtest.cm".to_string(),
                            startup: fsys::StartupMode::Lazy,
                            on_terminate: Some(fsys::OnTerminate::None),
                            environment: None,
                        },
                        ChildDecl {
                            name: "echo".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/echo#meta/echo.cm".to_string(),
                            startup: fsys::StartupMode::Eager,
                            on_terminate: Some(fsys::OnTerminate::Reboot),
                            environment: Some("test_env".to_string()),
                        },
                    ],
                    collections: vec![
                        CollectionDecl {
                            name: "modular".to_string(),
                            durability: fsys::Durability::Persistent,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            environment: None,
                        },
                        CollectionDecl {
                            name: "tests".to_string(),
                            durability: fsys::Durability::Transient,
                            allowed_offers: cm_types::AllowedOffers::StaticAndDynamic,
                            environment: Some("test_env".to_string()),
                        },
                    ],
                    facets: Some(fdata::Dictionary {
                        entries: Some(vec![
                            fdata::DictionaryEntry {
                                key: "author".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::Str("Fuchsia".to_string()))),
                            },
                        ]),
                        ..fdata::Dictionary::EMPTY
                    }),
                    environments: vec![
                        EnvironmentDecl {
                            name: "test_env".into(),
                            extends: fsys::EnvironmentExtends::Realm,
                            runners: vec![
                                RunnerRegistration {
                                    source_name: "runner".into(),
                                    source: RegistrationSource::Child("gtest".to_string()),
                                    target_name: "gtest-runner".into(),
                                }
                            ],
                            resolvers: vec![
                                ResolverRegistration {
                                    resolver: "pkg_resolver".into(),
                                    source: RegistrationSource::Parent,
                                    scheme: "fuchsia-pkg".to_string(),
                                }
                            ],
                            debug_capabilities: vec![
                                DebugRegistration::Protocol(DebugProtocolRegistration {
                                    source_name: "some_protocol".into(),
                                    source: RegistrationSource::Child("gtest".to_string()),
                                    target_name: "some_protocol".into(),
                                })
                            ],
                            stop_timeout_ms: Some(4567),
                        }
                    ],
                    config: Some(ConfigDecl {
                        fields: vec![
                            ConfigField {
                                key: "enable_logging".to_string(),
                                value_type: ConfigValueType::Bool(ConfigBooleanType {})
                            }
                        ],
                        declaration_checksum: vec![
                            0x66, 0x3E, 0x10, 0x72, 0x26, 0xA7, 0x5F, 0x0B, 0x23, 0x1D, 0x6C, 0x8D,
                            0x03, 0x6B, 0xC3, 0x58, 0x85, 0x70, 0xB4, 0x4D, 0x8A, 0x11, 0x16, 0x20,
                            0x5B, 0x11, 0x87, 0x96, 0x5E, 0x14, 0x67, 0xF8
                        ]
                    }),
                }
            },
        },
    }

    test_capability_path! {
        capability_path_one_part => {
            input = "/foo",
            result = Ok(CapabilityPath{dirname: "/".to_string(), basename: "foo".to_string()}),
        },
        capability_path_two_parts => {
            input = "/foo/bar",
            result = Ok(CapabilityPath{dirname: "/foo".to_string(), basename: "bar".to_string()}),
        },
        capability_path_many_parts => {
            input = "/foo/bar/long/path",
            result = Ok(CapabilityPath{
                dirname: "/foo/bar/long".to_string(),
                basename: "path".to_string()
            }),
        },
        capability_path_invalid_empty_part => {
            input = "/foo/bar//long/path",
            result = Err(Error::InvalidCapabilityPath{raw: "/foo/bar//long/path".to_string()}),
        },
        capability_path_invalid_empty => {
            input = "",
            result = Err(Error::InvalidCapabilityPath{raw: "".to_string()}),
        },
        capability_path_invalid_root => {
            input = "/",
            result = Err(Error::InvalidCapabilityPath{raw: "/".to_string()}),
        },
        capability_path_invalid_relative => {
            input = "foo/bar",
            result = Err(Error::InvalidCapabilityPath{raw: "foo/bar".to_string()}),
        },
        capability_path_invalid_trailing => {
            input = "/foo/bar/",
            result = Err(Error::InvalidCapabilityPath{raw: "/foo/bar/".to_string()}),
        },
    }

    test_fidl_into_and_from! {
        fidl_into_and_from_use_source => {
            input = vec![
                fsys::Ref::Parent(fsys::ParentRef{}),
                fsys::Ref::Framework(fsys::FrameworkRef{}),
                fsys::Ref::Debug(fsys::DebugRef{}),
                fsys::Ref::Capability(fsys::CapabilityRef {name: "capability".to_string()}),
                fsys::Ref::Child(fsys::ChildRef {
                    name: "foo".to_string(),
                    collection: None,
                }),
            ],
            input_type = fsys::Ref,
            result = vec![
                UseSource::Parent,
                UseSource::Framework,
                UseSource::Debug,
                UseSource::Capability(CapabilityName("capability".to_string())),
                UseSource::Child("foo".to_string()),
            ],
            result_type = UseSource,
        },
        fidl_into_and_from_expose_source => {
            input = vec![
                fsys::Ref::Self_(fsys::SelfRef {}),
                fsys::Ref::Child(fsys::ChildRef {
                    name: "foo".to_string(),
                    collection: None,
                }),
                fsys::Ref::Framework(fsys::FrameworkRef {}),
                fsys::Ref::Collection(fsys::CollectionRef { name: "foo".to_string() }),
            ],
            input_type = fsys::Ref,
            result = vec![
                ExposeSource::Self_,
                ExposeSource::Child("foo".to_string()),
                ExposeSource::Framework,
                ExposeSource::Collection("foo".to_string()),
            ],
            result_type = ExposeSource,
        },
        fidl_into_and_from_offer_source => {
            input = vec![
                fsys::Ref::Self_(fsys::SelfRef {}),
                fsys::Ref::Child(fsys::ChildRef {
                    name: "foo".to_string(),
                    collection: None,
                }),
                fsys::Ref::Framework(fsys::FrameworkRef {}),
                fsys::Ref::Capability(fsys::CapabilityRef { name: "foo".to_string() }),
                fsys::Ref::Parent(fsys::ParentRef {}),
                fsys::Ref::Collection(fsys::CollectionRef { name: "foo".to_string() }),
            ],
            input_type = fsys::Ref,
            result = vec![
                OfferSource::Self_,
                OfferSource::static_child("foo".to_string()),
                OfferSource::Framework,
                OfferSource::Capability(CapabilityName("foo".to_string())),
                OfferSource::Parent,
                OfferSource::Collection("foo".to_string()),
            ],
            result_type = OfferSource,
        },
        fidl_into_and_from_capability_without_path => {
            input = vec![
                fsys::ProtocolDecl {
                    name: Some("foo_protocol".to_string()),
                    source_path: None,
                    ..fsys::ProtocolDecl::EMPTY
                },
            ],
            input_type = fsys::ProtocolDecl,
            result = vec![
                ProtocolDecl {
                    name: "foo_protocol".into(),
                    source_path: None,
                }
            ],
            result_type = ProtocolDecl,
        },
        fidl_into_and_from_storage_capability => {
            input = vec![
                fsys::StorageDecl {
                    name: Some("minfs".to_string()),
                    backing_dir: Some("minfs".into()),
                    source: Some(fsys::Ref::Child(fsys::ChildRef {
                        name: "foo".to_string(),
                        collection: None,
                    })),
                    subdir: None,
                    storage_id: Some(fsys::StorageId::StaticInstanceIdOrMoniker),
                    ..fsys::StorageDecl::EMPTY
                },
            ],
            input_type = fsys::StorageDecl,
            result = vec![
                StorageDecl {
                    name: "minfs".into(),
                    backing_dir: "minfs".into(),
                    source: StorageDirectorySource::Child("foo".to_string()),
                    subdir: None,
                    storage_id: fsys::StorageId::StaticInstanceIdOrMoniker,
                },
            ],
            result_type = StorageDecl,
        },
        fidl_into_and_from_storage_capability_restricted => {
            input = vec![
                fsys::StorageDecl {
                    name: Some("minfs".to_string()),
                    backing_dir: Some("minfs".into()),
                    source: Some(fsys::Ref::Child(fsys::ChildRef {
                        name: "foo".to_string(),
                        collection: None,
                    })),
                    subdir: None,
                    storage_id: Some(fsys::StorageId::StaticInstanceId),
                    ..fsys::StorageDecl::EMPTY
                },
            ],
            input_type = fsys::StorageDecl,
            result = vec![
                StorageDecl {
                    name: "minfs".into(),
                    backing_dir: "minfs".into(),
                    source: StorageDirectorySource::Child("foo".to_string()),
                    subdir: None,
                    storage_id: fsys::StorageId::StaticInstanceId,
                },
            ],
            result_type = StorageDecl,
        },
    }

    test_fidl_into! {
        all_with_omitted_defaults => {
            input = fsys::ComponentDecl {
                program: Some(fsys::ProgramDecl {
                    runner: Some("elf".to_string()),
                    info: Some(fdata::Dictionary {
                        entries: Some(vec![]),
                        ..fdata::Dictionary::EMPTY
                    }),
                    ..fsys::ProgramDecl::EMPTY
                }),
                uses: Some(vec![]),
                exposes: Some(vec![]),
                offers: Some(vec![]),
                capabilities: Some(vec![]),
                children: Some(vec![]),
                collections: Some(vec![
                     fsys::CollectionDecl {
                         name: Some("modular".to_string()),
                         durability: Some(fsys::Durability::Persistent),
                         allowed_offers: None,
                         environment: None,
                         ..fsys::CollectionDecl::EMPTY
                     },
                     fsys::CollectionDecl {
                         name: Some("tests".to_string()),
                         durability: Some(fsys::Durability::Transient),
                         allowed_offers: Some(fsys::AllowedOffers::StaticOnly),
                         environment: Some("test_env".to_string()),
                         ..fsys::CollectionDecl::EMPTY
                     },
                     fsys::CollectionDecl {
                         name: Some("dyn_offers".to_string()),
                         durability: Some(fsys::Durability::Transient),
                         allowed_offers: Some(fsys::AllowedOffers::StaticAndDynamic),
                         ..fsys::CollectionDecl::EMPTY
                     },
                ]),
                facets: Some(fdata::Dictionary{
                    entries: Some(vec![]),
                    ..fdata::Dictionary::EMPTY
                }),
                environments: Some(vec![]),
                ..fsys::ComponentDecl::EMPTY
            },
            result = {
                ComponentDecl {
                    program: Some(ProgramDecl {
                        runner: Some("elf".try_into().unwrap()),
                        info: fdata::Dictionary {
                            entries: Some(vec![]),
                            ..fdata::Dictionary::EMPTY
                        },
                    }),
                    uses: vec![],
                    exposes: vec![],
                    offers: vec![],
                    capabilities: vec![],
                    children: vec![],
                    collections: vec![
                        CollectionDecl {
                            name: "modular".to_string(),
                            durability: fsys::Durability::Persistent,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            environment: None,
                        },
                        CollectionDecl {
                            name: "tests".to_string(),
                            durability: fsys::Durability::Transient,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            environment: Some("test_env".to_string()),
                        },
                        CollectionDecl {
                            name: "dyn_offers".to_string(),
                            durability: fsys::Durability::Transient,
                            allowed_offers: cm_types::AllowedOffers::StaticAndDynamic,
                            environment: None,
                        },
                    ],
                    facets: Some(fdata::Dictionary{
                        entries: Some(vec![]),
                        ..fdata::Dictionary::EMPTY
                    }),
                    environments: vec![],
                    config: None,
                }
            },
        },
    }
}
