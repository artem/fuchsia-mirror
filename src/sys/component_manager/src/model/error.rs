// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        events::error::EventsError,
        policy::PolicyError,
        resolver::ResolverError,
        routing::{OpenResourceError, RoutingError},
        runner::RunnerError,
        storage::StorageError,
    },
    ::routing::component_id_index::ComponentIdIndexError,
    ::routing::error::ComponentInstanceError,
    anyhow::Error,
    clonable_error::ClonableError,
    fuchsia_inspect, fuchsia_zircon as zx,
    moniker::{AbsoluteMonikerBase, PartialAbsoluteMoniker, PartialChildMoniker},
    std::{ffi::OsString, path::PathBuf},
    thiserror::Error,
};

/// Errors produced by `Model`.
#[derive(Debug, Error, Clone)]
pub enum ModelError {
    #[error("component instance {} not found in realm {}", child, moniker)]
    InstanceNotFoundInRealm { moniker: PartialAbsoluteMoniker, child: PartialChildMoniker },
    #[error("component instance {} in realm {} already exists", child, moniker)]
    InstanceAlreadyExists { moniker: PartialAbsoluteMoniker, child: PartialChildMoniker },
    #[error("component instance with moniker {} has shut down", moniker)]
    InstanceShutDown { moniker: PartialAbsoluteMoniker },
    #[error("component collection not found with name {}", name)]
    CollectionNotFound { name: String },
    #[error("collection {} does not allow dynamic offers", collection_name)]
    DynamicOffersNotAllowed { collection_name: String },
    #[error("dynamic offer not valid: {}", err)]
    DynamicOfferInvalid {
        #[source]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("context not found")]
    ContextNotFound,
    #[error("{} is not supported", feature)]
    Unsupported { feature: String },
    #[error("package URL missing")]
    PackageUrlMissing,
    #[error("package directory handle missing")]
    PackageDirectoryMissing,
    #[error("path is not utf-8: {:?}", path)]
    PathIsNotUtf8 { path: PathBuf },
    #[error("path is not valid: {:?}", path)]
    PathInvalid { path: String },
    #[error("filename is not utf-8: {:?}", name)]
    NameIsNotUtf8 { name: OsString },
    #[error("expected a component instance moniker")]
    UnexpectedComponentManagerMoniker,
    #[error("ComponentDecl invalid {}: {}", url, err)]
    ComponentDeclInvalid {
        url: String,
        #[source]
        err: cm_rust::Error,
    },
    #[error("The model is not available")]
    ModelNotAvailable,
    #[error("Namespace creation failed: {}", err)]
    NamespaceCreationFailed {
        #[source]
        err: ClonableError,
    },
    #[error("calling Admin/Reboot failed: {}", err)]
    RebootFailed {
        #[source]
        err: ClonableError,
    },
    #[error("failed to resolve \"{}\": {}", url, err)]
    ResolverError {
        url: String,
        #[source]
        err: ResolverError,
    },
    #[error("Routing error: {}", err)]
    RoutingError {
        #[from]
        err: RoutingError,
    },
    #[error("Failed to open resource: {}", err)]
    OpenResourceError {
        #[from]
        err: OpenResourceError,
    },
    #[error("Runner error: {}", err)]
    RunnerError {
        #[from]
        err: RunnerError,
    },
    #[error("storage error: {}", err)]
    StorageError {
        #[from]
        err: StorageError,
    },
    #[error("component instance error: {}", err)]
    ComponentInstanceError {
        #[from]
        err: ComponentInstanceError,
    },
    #[error(
        "Component {} is trying to use a storage capability which is restricted to the component ID index.",
        moniker
    )]
    ComponentNotInIdIndex { moniker: PartialAbsoluteMoniker },
    #[error("failed to add entry {} to {}", entry_name, moniker)]
    AddEntryError { moniker: PartialAbsoluteMoniker, entry_name: String },
    #[error("failed to remove entry {}", entry_name)]
    RemoveEntryError { entry_name: String },
    #[error("failed to open directory '{}' for component '{}'", relative_path, moniker)]
    OpenDirectoryError { moniker: PartialAbsoluteMoniker, relative_path: String },
    #[error("failed to clone node '{}' for '{}'", relative_path, moniker)]
    CloneNodeError { moniker: PartialAbsoluteMoniker, relative_path: String },
    #[error("failed to create stream from channel")]
    StreamCreationError {
        #[source]
        err: ClonableError,
    },
    #[error("insufficient resources to complete operation")]
    InsufficientResources,
    #[error("failed to send {} to runner for component {}", operation, moniker)]
    RunnerCommunicationError {
        moniker: PartialAbsoluteMoniker,
        operation: String,
        #[source]
        err: ClonableError,
    },
    #[error("events error: {}", err)]
    EventsError {
        #[from]
        err: EventsError,
    },
    #[error("policy error: {}", err)]
    PolicyError {
        #[from]
        err: PolicyError,
    },
    #[error("inspect error: {}", err)]
    Inspect {
        #[from]
        err: fuchsia_inspect::Error,
    },
    #[error("component id index error: {}", err)]
    ComponentIdIndexError {
        #[from]
        err: ComponentIdIndexError,
    },
}

impl ModelError {
    pub fn instance_not_found_in_realm(
        moniker: PartialAbsoluteMoniker,
        child: PartialChildMoniker,
    ) -> ModelError {
        ModelError::InstanceNotFoundInRealm { moniker, child }
    }

    pub fn instance_already_exists(
        moniker: PartialAbsoluteMoniker,
        child: PartialChildMoniker,
    ) -> ModelError {
        ModelError::InstanceAlreadyExists { moniker, child }
    }

    pub fn instance_shut_down(moniker: PartialAbsoluteMoniker) -> ModelError {
        ModelError::InstanceShutDown { moniker }
    }

    pub fn instance_not_found(moniker: PartialAbsoluteMoniker) -> ModelError {
        ModelError::from(ComponentInstanceError::instance_not_found(moniker.to_partial()))
    }

    pub fn collection_not_found(name: impl Into<String>) -> ModelError {
        ModelError::CollectionNotFound { name: name.into() }
    }

    pub fn dynamic_offers_not_allowed(collection_name: impl Into<String>) -> ModelError {
        ModelError::DynamicOffersNotAllowed { collection_name: collection_name.into() }
    }

    pub fn dynamic_offer_invalid(err: cm_fidl_validator::error::ErrorList) -> ModelError {
        ModelError::DynamicOfferInvalid { err }
    }

    pub fn context_not_found() -> ModelError {
        ModelError::ContextNotFound {}
    }

    pub fn unsupported(feature: impl Into<String>) -> ModelError {
        ModelError::Unsupported { feature: feature.into() }
    }

    pub fn path_is_not_utf8(path: PathBuf) -> ModelError {
        ModelError::PathIsNotUtf8 { path }
    }

    pub fn path_invalid(path: impl Into<String>) -> ModelError {
        ModelError::PathInvalid { path: path.into() }
    }

    pub fn name_is_not_utf8(name: OsString) -> ModelError {
        ModelError::NameIsNotUtf8 { name }
    }

    pub fn namespace_creation_failed(err: impl Into<Error>) -> ModelError {
        ModelError::NamespaceCreationFailed { err: err.into().into() }
    }

    pub fn reboot_failed(err: impl Into<Error>) -> ModelError {
        ModelError::RebootFailed { err: err.into().into() }
    }

    pub fn component_decl_invalid(url: impl Into<String>, err: cm_rust::Error) -> ModelError {
        ModelError::ComponentDeclInvalid { url: url.into(), err }
    }

    pub fn model_not_available() -> ModelError {
        ModelError::ModelNotAvailable
    }

    pub fn add_entry_error(
        moniker: PartialAbsoluteMoniker,
        entry_name: impl Into<String>,
    ) -> ModelError {
        ModelError::AddEntryError { moniker, entry_name: entry_name.into() }
    }

    pub fn remove_entry_error(entry_name: impl Into<String>) -> ModelError {
        ModelError::RemoveEntryError { entry_name: entry_name.into() }
    }

    pub fn open_directory_error(
        moniker: PartialAbsoluteMoniker,
        relative_path: impl Into<String>,
    ) -> ModelError {
        ModelError::OpenDirectoryError { moniker, relative_path: relative_path.into() }
    }

    pub fn clone_node_error(
        moniker: PartialAbsoluteMoniker,
        relative_path: impl Into<String>,
    ) -> ModelError {
        ModelError::CloneNodeError { moniker, relative_path: relative_path.into() }
    }

    pub fn stream_creation_error(err: impl Into<Error>) -> ModelError {
        ModelError::StreamCreationError { err: err.into().into() }
    }

    pub fn as_zx_status(&self) -> zx::Status {
        match self {
            ModelError::RoutingError { err } => err.as_zx_status(),
            ModelError::PolicyError { err } => err.as_zx_status(),
            ModelError::ComponentInstanceError {
                err: ComponentInstanceError::InstanceNotFound { .. },
            } => zx::Status::NOT_FOUND,
            ModelError::Unsupported { .. } => zx::Status::NOT_SUPPORTED,
            // Any other type of error is not expected.
            _ => zx::Status::INTERNAL,
        }
    }
}
