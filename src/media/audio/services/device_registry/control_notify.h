// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_NOTIFY_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_NOTIFY_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/time.h>
#include <zircon/types.h>

#include "src/media/audio/services/device_registry/observer_notify.h"

namespace media_audio {

// A ControlServer exposes this interface, to the Device that it controls. The Device uses it for
// asynchronous notification. Note that ControlNotify includes the entirety of the ObserverNotify
// interface, including methods such as DeviceIsRemoved and DeviceHasError. Also note that the
// Device stores this interface as a weak_ptr, since the ControlServer can be destroyed at any time.
class ControlNotify : public ObserverNotify {
 public:
  virtual void DeviceDroppedRingBuffer() = 0;
  virtual void DelayInfoChanged(const fuchsia_audio_device::DelayInfo&) = 0;

  virtual void DaiFormatChanged(
      const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
      const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) = 0;
  virtual void DaiFormatNotSet(const fuchsia_hardware_audio::DaiFormat& dai_format,
                               zx_status_t driver_error) = 0;
  virtual void CodecStarted(const zx::time& start_time) = 0;
  virtual void CodecNotStarted() = 0;
  virtual void CodecStopped(const zx::time& stop_time) = 0;
  virtual void CodecNotStopped() = 0;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_CONTROL_NOTIFY_H_
