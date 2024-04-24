// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration options for the forensics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForensicsConfig {
    #[serde(default)]
    pub feedback: FeedbackConfig,
    #[serde(default)]
    pub cobalt: CobaltConfig,
}

/// Configuration options for the feedback configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackConfig {
    #[serde(default)]
    pub low_memory: bool,
    #[serde(default)]
    pub large_disk: bool,
    #[serde(default)]
    pub remote_device_id_provider: bool,
    #[serde(default)]
    pub flash_ts_feedback_id_component_url: Option<String>,
}

/// Configuration options for the cobalt configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CobaltConfig {
    #[serde(default)]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub api_key: Option<Utf8PathBuf>,
}
