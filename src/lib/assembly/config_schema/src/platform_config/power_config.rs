// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PowerConfig {
    /// Whether power suspend/resume is supported.
    #[serde(default)]
    pub suspend_enabled: bool,

    /// Whether the testing SAG with testing based controls
    /// should be used. This will only work when |suspend_enabled|
    /// is also true, as there is no SAG when suspend support is disabled.
    /// TODO(https://fxbug.dev/335526423): Remove when no longer needed.
    #[serde(default)]
    pub testing_sag_enabled: bool,
}
