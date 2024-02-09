// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use input_device_constants::InputDeviceType as PlatformInputDeviceType;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the UI area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformUiConfig {
    /// Whether UI should be enabled on the product.
    #[serde(default)]
    pub enabled: bool,

    /// The sensor config to provide to the input pipeline.
    #[serde(default)]
    pub sensor_config: Option<Utf8PathBuf>,

    /// The minimum frame duration for frame scheduler.
    #[serde(default)]
    pub frame_scheduler_min_predicted_frame_duration_in_us: u64,

    /// Scenic shifts focus from view to view as the user interacts with the UI.
    /// Set to false for Smart displays, as they use a different programmatic focus change scheme.
    #[serde(default)]
    pub pointer_auto_focus: bool,

    /// Scenic attempts to delegate composition of client images to the display controller, with
    /// GPU/Vulkan composition as the fallback. If false, GPU/Vulkan composition is always used.
    #[serde(default)]
    pub display_composition: bool,

    /// The relevant input device bindings from which to install appropriate
    /// input handlers. Default to an empty set.
    #[serde(default)]
    pub supported_input_devices: Vec<InputDeviceType>,

    // The rotation of the display, counter-clockwise, in 90-degree increments.
    #[serde(default)]
    pub display_rotation: u64,

    // TODO(132584): change to float when supported in structured config.
    // The density of the display, in pixels per mm.
    #[serde(default)]
    pub display_pixel_density: String,

    // The expected viewing distance for the display.
    #[serde(default)]
    pub viewing_distance: ViewingDistance,

    /// Whether to include brightness manager, and the relevant configs.
    #[serde(default)]
    pub brightness_manager: Option<BrightnessManager>,

    /// Set with_synthetic_device_support true to include input-helper to ui.
    #[serde(default)]
    pub with_synthetic_device_support: bool,

    /// The renderer Scenic should use.
    #[serde(default)]
    pub renderer: RendererType,

    /// The constraints on the display mode horizontal resolution, in pixels.
    #[serde(default)]
    pub display_mode_horizontal_resolution_px_range: UnsignedIntegerRangeInclusive,

    /// The constraints on the display mode vertical resolution, in pixels.
    #[serde(default)]
    pub display_mode_vertical_resolution_px_range: UnsignedIntegerRangeInclusive,

    /// The constraints on the display mode refresh rate, in millihertz (10^-3 Hz).
    #[serde(default)]
    pub display_mode_refresh_rate_millihertz_range: UnsignedIntegerRangeInclusive,
}

impl Default for PlatformUiConfig {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            sensor_config: Default::default(),
            frame_scheduler_min_predicted_frame_duration_in_us: Default::default(),
            pointer_auto_focus: true,
            display_composition: false,
            supported_input_devices: Default::default(),
            display_rotation: Default::default(),
            display_pixel_density: Default::default(),
            viewing_distance: Default::default(),
            brightness_manager: Default::default(),
            with_synthetic_device_support: Default::default(),
            renderer: Default::default(),
            display_mode_horizontal_resolution_px_range: Default::default(),
            display_mode_vertical_resolution_px_range: Default::default(),
            display_mode_refresh_rate_millihertz_range: Default::default(),
        }
    }
}

// LINT.IfChange
/// Options for input devices that may be supported.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum InputDeviceType {
    Button,
    Keyboard,
    LightSensor,
    Mouse,
    Touchscreen,
}

// This impl verifies that the platform and assembly enums are kept in sync.
impl From<InputDeviceType> for PlatformInputDeviceType {
    fn from(src: InputDeviceType) -> PlatformInputDeviceType {
        match src {
            InputDeviceType::Button => PlatformInputDeviceType::ConsumerControls,
            InputDeviceType::Keyboard => PlatformInputDeviceType::Keyboard,
            InputDeviceType::LightSensor => PlatformInputDeviceType::LightSensor,
            InputDeviceType::Mouse => PlatformInputDeviceType::Mouse,
            InputDeviceType::Touchscreen => PlatformInputDeviceType::Touch,
        }
    }
}
// LINT.ThenChange(/src/ui/lib/input-device-constants/src/lib.rs)

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ViewingDistance {
    Handheld,
    Close,
    Near,
    Midrange,
    Far,
    #[default]
    Unknown,
}

impl AsRef<str> for ViewingDistance {
    fn as_ref(&self) -> &str {
        match &self {
            Self::Handheld => "handheld",
            Self::Close => "close",
            Self::Near => "near",
            Self::Midrange => "midrange",
            Self::Far => "far",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub struct BrightnessManager {
    pub with_display_power: bool,
}

// LINT.IfChange
/// Options for Scenic renderers that may be supported.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum RendererType {
    Cpu,
    Null,
    #[default]
    Vulkan,
}
// LINT.ThenChange(/src/ui/scenic/bin/app.h)

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct UnsignedIntegerRangeInclusive {
    /// The inclusive lower bound of the range. If None, the range is unbounded.
    pub start: Option<u32>,

    /// The inclusive upper bound of the range. If None, the range is unbounded.
    pub end: Option<u32>,
}
