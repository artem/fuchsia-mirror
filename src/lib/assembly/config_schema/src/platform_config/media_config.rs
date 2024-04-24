// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlatformMediaConfig {
    /// Enable Audio Device Registry.
    /// TODO: Remove this once all clients are using the audio config.
    #[serde(default)]
    pub audio_device_registry_enabled: bool,

    #[serde(default)]
    pub audio: Option<AudioConfig>,

    #[serde(default)]
    pub camera: CameraConfig,

    #[serde(default)]
    pub multizone_leader: MultizoneConfig,
}

/// The audio stack to use in the platform.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioConfig {
    /// Use the full AudioCore stack.
    FullStack(AudioCoreConfig),

    /// Use the partial AudioDeviceRegistry stack.
    PartialStack,
}

/// Configuration options for the AudioCore stack.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioCoreConfig {
    /// Route the ADC device to audio_core.
    #[serde(default)]
    pub use_adc_device: bool,
}

/// The camera settings for the platform.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct CameraConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// The multizone_leader settings for the platform.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct MultizoneConfig {
    /// The component url for the multizone leader component.
    /// The component should expose these capabilities:
    ///   fuchsia.media.SessionAudioConsumerFactory
    ///   google.cast.multizone.Leader
    #[serde(default)]
    pub component_url: Option<String>,
}
