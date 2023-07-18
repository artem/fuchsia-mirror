// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {cm_types, thiserror::Error};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Errors produced by `MonikerEnvironment`.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MonikerError {
    #[error("invalid moniker: {}", rep)]
    InvalidMoniker { rep: String },
    #[error(transparent)]
    InvalidMonikerPart(#[from] cm_types::ParseError),
    #[error("moniker {} does not have prefix {}", moniker, prefix)]
    MonikerDoesNotHavePrefix { moniker: String, prefix: String },
}

impl MonikerError {
    pub fn invalid_moniker(rep: impl Into<String>) -> MonikerError {
        MonikerError::InvalidMoniker { rep: rep.into() }
    }
}
