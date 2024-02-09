// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{AnyCapability, AnyCast, Open};
use fidl_fuchsia_component_sandbox as fsandbox;
use std::fmt::Debug;
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ConversionError {
    #[error("could not get handle: {err:?}")]
    Handle { err: fsandbox::HandleCapabilityError },
    #[error("`{0}` is not a valid `fuchsia.io` node name")]
    ParseName(#[from] vfs::name::ParseNameError),
    #[error("conversion to type is not supported")]
    NotSupported,
    #[error("value at `{key}` could not be converted")]
    Nested {
        key: String,
        #[source]
        err: Box<ConversionError>,
    },
}

/// Errors arising from conversion between Rust and FIDL types.
#[derive(Error, Debug)]
pub enum RemoteError {
    #[error("unknown FIDL variant")]
    UnknownVariant,

    #[error("unregistered capability; only capabilities created by sandbox are allowed")]
    Unregistered,
}

/// The capability trait, implemented by all capabilities.
pub trait Capability:
    AnyCast + Into<fsandbox::Capability> + TryFrom<AnyCapability> + Clone + Debug + Send + Sync
{
    /// Attempt to convert `self` to a capability of type [Open].
    ///
    /// The default implementation always returns an error
    fn try_into_open(self) -> Result<Open, ConversionError> {
        Err(ConversionError::NotSupported)
    }

    fn into_fidl(self) -> fsandbox::Capability {
        self.into()
    }
}
