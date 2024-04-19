// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the kernel area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformKernelConfig {
    #[serde(default)]
    pub memory_compression: bool,
    #[serde(default)]
    pub lru_memory_compression: bool,
    #[serde(default)]
    pub continuous_eviction: bool,
}
