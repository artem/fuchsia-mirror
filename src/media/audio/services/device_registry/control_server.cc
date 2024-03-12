// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mem/cpp/natural_types.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <iomanip>
#include <limits>
#include <optional>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {

// static
std::shared_ptr<ControlServer> ControlServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::Control> server_end,
    std::shared_ptr<AudioDeviceRegistry> parent, std::shared_ptr<Device> device) {
  ADR_LOG_STATIC(kLogControlServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), parent, device);
}

ControlServer::ControlServer(std::shared_ptr<AudioDeviceRegistry> parent,
                             std::shared_ptr<Device> device)
    : parent_(parent), device_(device) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

ControlServer::~ControlServer() {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

// Called when the client shuts down first.
void ControlServer::OnShutdown(fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_METHOD() << "shutdown with unexpected status: " << info;
  } else {
    ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogObjectLifetimes) << "with status: " << info;
  }

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->ClientDroppedControl();
    ring_buffer_server_.reset();
  }
}

// Called when Device drops its RingBuffer FIDL. Tell RingBufferServer and drop our reference.
void ControlServer::DeviceDroppedRingBuffer() {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods);

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->DeviceDroppedRingBuffer();
    ring_buffer_server_.reset();
  }
}

void ControlServer::DeviceHasError() {
  ADR_LOG_METHOD(kLogControlServerMethods);

  device_has_error_ = true;
  DeviceIsRemoved();
}

// Upon exiting this method, we drop our connection to the client.
void ControlServer::DeviceIsRemoved() {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->ClientDroppedControl();
    ring_buffer_server_.reset();

    // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  }
  Shutdown(ZX_ERR_PEER_CLOSED);
}

std::shared_ptr<RingBufferServer> ControlServer::GetRingBufferServer() {
  ADR_LOG_METHOD(kLogControlServerMethods);
  if (ring_buffer_server_) {
    if (auto sh_ptr_ring_buffer_server = ring_buffer_server_->lock(); sh_ptr_ring_buffer_server) {
      return sh_ptr_ring_buffer_server;
    }
    ring_buffer_server_.reset();
  }
  return nullptr;
}

// fuchsia.audio.device.Control implementation
void ControlServer::SetGain(SetGainRequest& request, SetGainCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kDeviceError));
    return;
  }

  if (!request.target_state()) {
    ADR_WARN_METHOD() << "required field 'target_state' is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kInvalidGainState));
    return;
  }

  auto& gain_caps = *device_->info()->gain_caps();
  if (!request.target_state()->gain_db()) {
    ADR_WARN_METHOD() << "required field `target_state.gain_db` is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kInvalidGainDb));
    return;
  }

  if (*request.target_state()->gain_db() > *gain_caps.max_gain_db() ||
      *request.target_state()->gain_db() < *gain_caps.min_gain_db()) {
    ADR_WARN_METHOD() << "gain_db (" << *request.target_state()->gain_db() << ") is out of range ["
                      << *gain_caps.min_gain_db() << ", " << *gain_caps.max_gain_db() << "]";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kGainOutOfRange));
    return;
  }

  if (request.target_state()->muted().value_or(false) && !(*gain_caps.can_mute())) {
    ADR_WARN_METHOD() << "device cannot MUTE";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kMuteUnavailable));
    return;
  }

  if (request.target_state()->agc_enabled().value_or(false) && !(*gain_caps.can_agc())) {
    ADR_WARN_METHOD() << "device cannot AGC";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kAgcUnavailable));
    return;
  }

  fuchsia_hardware_audio::GainState gain_state{{.gain_db = *request.target_state()->gain_db()}};
  if (request.target_state()->muted()) {
    gain_state.muted(*request.target_state()->muted());
  }
  if (request.target_state()->agc_enabled()) {
    gain_state.agc_enabled(*request.target_state()->agc_enabled());
  }
  device_->SetGain(gain_state);

  completer.Reply(fit::success(fuchsia_audio_device::ControlSetGainResponse{}));
}

void ControlServer::CreateRingBuffer(CreateRingBufferRequest& request,
                                     CreateRingBufferCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  // Fail if device has error.
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kDeviceError));
    return;
  }

  if (create_ring_buffer_completer_) {
    ADR_WARN_METHOD() << "previous `CreateRingBuffer` request has not yet completed";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kAlreadyPending));
    return;
  }

  // Fail on missing parameters.
  if (!request.options()) {
    ADR_WARN_METHOD() << "required field 'options' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions));
    return;
  }
  if (!request.options()->format() || !request.options()->format()->sample_type() ||
      !request.options()->format()->channel_count() ||
      !request.options()->format()->frames_per_second()) {
    ADR_WARN_METHOD() << "required 'options.format' (or one of its required members) is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat));
    return;
  }
  if (!request.options()->ring_buffer_min_bytes()) {
    ADR_WARN_METHOD() << "required field 'options.ring_buffer_min_bytes' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes));
    return;
  }
  if (!request.ring_buffer_server()) {
    ADR_WARN_METHOD() << "required field 'ring_buffer_server' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer));
    return;
  }
  if (GetRingBufferServer()) {
    ADR_WARN_METHOD() << "device RingBuffer already exists";
    completer.Reply(
        fit::error(fuchsia_audio_device::wire::ControlCreateRingBufferError::kAlreadyAllocated));
  }

  auto driver_format = device_->SupportedDriverFormatForClientFormat(*request.options()->format());
  // Fail if device cannot satisfy the requested format.
  if (!driver_format) {
    ADR_WARN_METHOD() << "device does not support the specified options";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch));
    return;
  }

  create_ring_buffer_completer_ = completer.ToAsync();

  bool created = device_->CreateRingBuffer(
      *driver_format, *request.options()->ring_buffer_min_bytes(),
      [this](Device::RingBufferInfo info) {
        // If we have no async completer, maybe we're shutting down. Just exit.
        if (!create_ring_buffer_completer_) {
          ADR_WARN_METHOD()
              << "create_ring_buffer_completer_ gone by the time the CreateRingBuffer callback ran";
          if (auto ring_buffer_server = GetRingBufferServer(); ring_buffer_server) {
            ring_buffer_server_.reset();
          }
          return;
        }

        auto completer = std::move(*create_ring_buffer_completer_);
        create_ring_buffer_completer_.reset();

        completer.Reply(fit::success(fuchsia_audio_device::ControlCreateRingBufferResponse{{
            .properties = info.properties,
            .ring_buffer = std::move(info.ring_buffer),
        }}));
      });

  if (!created) {
    ADR_WARN_METHOD() << "device cannot create a ring buffer with the specified options";
    ring_buffer_server_.reset();
    create_ring_buffer_completer_->Reply(
        fidl::Response<fuchsia_audio_device::Control::CreateRingBuffer>(
            fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kBadRingBufferOption)));
    return;
  }
  auto ring_buffer_server = RingBufferServer::Create(
      thread_ptr(), std::move(*request.ring_buffer_server()), shared_from_this(), device_);
  AddChildServer(ring_buffer_server);
  ring_buffer_server_ = ring_buffer_server;
}

// This is only here because ControlNotify includes the methods from ObserverNotify. It might be
// helpful for ControlServer to know when its SetGain call took effect, but this isn't needed.
// ControlServer also has no gain-related hanging-get to complete.
void ControlServer::GainStateChanged(const fuchsia_audio_device::GainState&) {
  ADR_LOG_METHOD(kLogNotifyMethods);
}

// This is only here because ControlNotify includes the methods from ObserverNotify. ControlServer
// doesn't have a role to play in plug state changes, nor a client hanging-get to complete.
void ControlServer::PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                                     zx::time plug_change_time) {
  ADR_LOG_METHOD(kLogNotifyMethods);
}

// We receive delay values for the first time during the configuration process. Once we have these
// values, we can calculate the required ring-buffer size and request the VMO.
void ControlServer::DelayInfoChanged(const fuchsia_audio_device::DelayInfo& delay_info) {
  ADR_LOG_METHOD(kLogControlServerResponses || kLogNotifyMethods);

  // Initialization is complete, so this represents a delay update.
  // If this is eventually exposed to Observers or any other watcher, notify them.
  if (auto ring_buffer_server = GetRingBufferServer(); ring_buffer_server) {
    ring_buffer_server->DelayInfoChanged(delay_info);
  }
  delay_info_ = delay_info;
}

// For now don't do anything on receiving this. Eventually we'll complete a pending `SetDaiFormat`.
void ControlServer::DaiFormatChanged(
    const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
    const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) {
  ADR_LOG_METHOD(kLogNotifyMethods);
  LogDaiFormat(dai_format);
  LogCodecFormatInfo(codec_format_info);
}

// For now don't do anything on receiving this. Eventually we'll fail a pending `SetDaiFormat`.
void ControlServer::DaiFormatNotSet(const fuchsia_hardware_audio::DaiFormat& dai_format,
                                    zx_status_t driver_error) {
  ADR_WARN_METHOD() << "(err " << driver_error << ")";
  LogDaiFormat(dai_format);
}

// For now don't do anything on receiving this. Eventually we'll complete a pending `Start`.
void ControlServer::CodecStarted(const zx::time& start_time) {
  ADR_LOG_METHOD(kLogNotifyMethods) << "(" << start_time.get() << ")";
}

// For now don't do anything on receiving this. Eventually we'll fail a pending `Start`.
void ControlServer::CodecNotStarted() { ADR_WARN_METHOD(); }

// For now don't do anything on receiving this. Eventually we'll complete a pending `Stop`.
void ControlServer::CodecStopped(const zx::time& stop_time) {
  ADR_LOG_METHOD(kLogNotifyMethods) << "(" << stop_time.get() << ")";
}

// For now don't do anything on receiving this. Eventually we'll fail a pending `Stop`.
void ControlServer::CodecNotStopped() { ADR_WARN_METHOD(); }

// fuchsia.hardware.audio.signalprocessing support
//
void ControlServer::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
}

void ControlServer::GetElements(GetElementsCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
}

void ControlServer::WatchTopology(WatchTopologyCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void ControlServer::WatchElementState(WatchElementStateRequest& request,
                                      WatchElementStateCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void ControlServer::SetTopology(SetTopologyRequest& request,
                                SetTopologyCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
}

void ControlServer::SetElementState(SetElementStateRequest& request,
                                    SetElementStateCompleter::Sync& completer) {
  ADR_WARN_METHOD() << kClassName << "(" << this << ")::" << __func__
                    << ": signalprocessing not supported";
  completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace media_audio
