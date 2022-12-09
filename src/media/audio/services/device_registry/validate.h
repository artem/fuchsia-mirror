// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// Utility functions
std::vector<fuchsia_audio_device::PcmFormatSet> TranslateFormatSets(
    std::vector<fuchsia_hardware_audio::SupportedFormats>& formats);

zx_status_t ValidateStreamProperties(
    const fuchsia_hardware_audio::StreamProperties& props,
    std::optional<const fuchsia_hardware_audio::GainState> gain_state = std::nullopt,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state = std::nullopt);
zx_status_t ValidateSupportedFormats(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& formats);
zx_status_t ValidateGainState(
    const fuchsia_hardware_audio::GainState& gain_state,
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_properties = std::nullopt);
zx_status_t ValidatePlugState(
    const fuchsia_hardware_audio::PlugState& plug_state,
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_properties = std::nullopt);
bool ValidateDeviceInfo(const fuchsia_audio_device::Info& device_info);

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_VALIDATE_H_
