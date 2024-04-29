// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mem/cpp/natural_types.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/internal/result.h>
#include <lib/fit/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <optional>
#include <utility>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

namespace fad = fuchsia_audio_device;

// static
std::shared_ptr<ControlServer> ControlServer::Create(std::shared_ptr<const FidlThread> thread,
                                                     fidl::ServerEnd<fad::Control> server_end,
                                                     std::shared_ptr<AudioDeviceRegistry> parent,
                                                     std::shared_ptr<Device> device) {
  ADR_LOG_STATIC(kLogControlServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), std::move(parent),
                                std::move(device));
}

ControlServer::ControlServer(std::shared_ptr<AudioDeviceRegistry> parent,
                             std::shared_ptr<Device> device)
    : parent_(std::move(parent)), device_(std::move(device)) {
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

  for (auto& [_, weak_ring_buffer_server] : ring_buffer_servers_) {
    if (auto sh_ring_buffer_server = weak_ring_buffer_server.lock(); sh_ring_buffer_server) {
      sh_ring_buffer_server->ClientDroppedControl();
    }
  }
  ring_buffer_servers_.clear();
}

// Called when Device drops its RingBuffer FIDL. Tell RingBufferServer and drop our reference.
void ControlServer::DeviceDroppedRingBuffer(ElementId element_id) {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods);

  auto ring_buffer_server_iter = ring_buffer_servers_.find(element_id);
  if (ring_buffer_server_iter != ring_buffer_servers_.end()) {
    if (auto sh_ring_buffer_server = ring_buffer_server_iter->second.lock();
        sh_ring_buffer_server) {
      sh_ring_buffer_server->DeviceDroppedRingBuffer();
    }
    ring_buffer_servers_.erase(element_id);
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

  for (auto& [_, weak_ring_buffer_server] : ring_buffer_servers_) {
    if (auto sh_ring_buffer_server = weak_ring_buffer_server.lock(); sh_ring_buffer_server) {
      sh_ring_buffer_server->ClientDroppedControl();
    }
  }
  ring_buffer_servers_.clear();

  // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.

  Shutdown(ZX_ERR_PEER_CLOSED);
}

std::shared_ptr<RingBufferServer> ControlServer::TryGetRingBufferServer(ElementId element_id) {
  ADR_LOG_METHOD(kLogControlServerMethods);
  auto ring_buffer_server_iter = ring_buffer_servers_.find(element_id);
  if (ring_buffer_server_iter != ring_buffer_servers_.end()) {
    if (auto sh_ring_buffer_server = ring_buffer_server_iter->second.lock();
        sh_ring_buffer_server) {
      return sh_ring_buffer_server;
    }
    ring_buffer_servers_.erase(element_id);
  }
  return nullptr;
}

// fuchsia.audio.device.Control implementation
void ControlServer::SetGain(SetGainRequest& request, SetGainCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlSetGainError::kDeviceError));
    return;
  }

  if (!device_->is_stream_config()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlSetGainError::kWrongDeviceType));
    return;
  }

  if (!request.target_state()) {
    ADR_WARN_METHOD() << "required field 'target_state' is missing";
    completer.Reply(fit::error(fad::ControlSetGainError::kInvalidGainState));
    return;
  }

  auto& gain_caps = *device_->info()->gain_caps();
  if (!request.target_state()->gain_db()) {
    ADR_WARN_METHOD() << "required field `target_state.gain_db` is missing";
    completer.Reply(fit::error(fad::ControlSetGainError::kInvalidGainDb));
    return;
  }

  if (*request.target_state()->gain_db() > *gain_caps.max_gain_db() ||
      *request.target_state()->gain_db() < *gain_caps.min_gain_db()) {
    ADR_WARN_METHOD() << "gain_db (" << *request.target_state()->gain_db() << ") is out of range ["
                      << *gain_caps.min_gain_db() << ", " << *gain_caps.max_gain_db() << "]";
    completer.Reply(fit::error(fad::ControlSetGainError::kGainOutOfRange));
    return;
  }

  if (request.target_state()->muted().value_or(false) && !(*gain_caps.can_mute())) {
    ADR_WARN_METHOD() << "device cannot MUTE";
    completer.Reply(fit::error(fad::ControlSetGainError::kMuteUnavailable));
    return;
  }

  if (request.target_state()->agc_enabled().value_or(false) && !(*gain_caps.can_agc())) {
    ADR_WARN_METHOD() << "device cannot AGC";
    completer.Reply(fit::error(fad::ControlSetGainError::kAgcUnavailable));
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

  completer.Reply(fit::success(fad::ControlSetGainResponse{}));
}

void ControlServer::CreateRingBuffer(CreateRingBufferRequest& request,
                                     CreateRingBufferCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  // Fail if device has error.
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kDeviceError));
    return;
  }

  if (!device_->is_stream_config() && !device_->is_composite()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kWrongDeviceType));
    return;
  }
  ElementId element_id = fad::kDefaultRingBufferElementId;
  // Fail on missing parameters.
  if (device_->is_composite()) {
    if (!request.element_id()) {
      ADR_WARN_METHOD() << "required field 'element_id' is missing";
      completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidElementId));
      return;
    }
    element_id = *request.element_id();
    auto& rb_ids = device_->ring_buffer_endpoint_ids();
    if (rb_ids.find(element_id) == rb_ids.end()) {
      ADR_WARN_METHOD() << "required field 'element_id' (" << element_id
                        << ") does not refer to an ENDPOINT of type RING_BUFFER";
      completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidElementId));
      return;
    }
  }

  if (create_ring_buffer_completers_.find(element_id) != create_ring_buffer_completers_.end()) {
    ADR_WARN_METHOD() << "(element_id " << element_id
                      << ") previous `CreateRingBuffer` request has not completed";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kAlreadyPending));
    return;
  }

  if (!request.options()) {
    ADR_WARN_METHOD() << "(element_id " << element_id << ") required field 'options' is missing";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidOptions));
    return;
  }
  if (!request.options()->format() || !request.options()->format()->sample_type() ||
      !request.options()->format()->channel_count() ||
      !request.options()->format()->frames_per_second()) {
    ADR_WARN_METHOD() << "(element_id " << element_id
                      << ") required 'options.format' (or one of its required members) is missing";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidFormat));
    return;
  }
  if (!request.options()->ring_buffer_min_bytes()) {
    ADR_WARN_METHOD() << "(element_id " << element_id
                      << ") required field 'options.ring_buffer_min_bytes' is missing";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidMinBytes));
    return;
  }
  if (!request.ring_buffer_server()) {
    ADR_WARN_METHOD() << "(element_id " << element_id
                      << ") required field 'ring_buffer_server' is missing";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kInvalidRingBuffer));
    return;
  }
  if (TryGetRingBufferServer(element_id)) {
    ADR_WARN_METHOD() << "(element_id " << element_id << ") device RingBuffer already exists";
    completer.Reply(fit::error(fad::wire::ControlCreateRingBufferError::kAlreadyAllocated));
    return;
  }

  auto driver_format =
      device_->SupportedDriverFormatForClientFormat(element_id, *request.options()->format());
  // Fail if device cannot satisfy the requested format.
  if (!driver_format) {
    ADR_WARN_METHOD() << "(element_id " << element_id
                      << ") device does not support the specified options";
    completer.Reply(fit::error(fad::ControlCreateRingBufferError::kFormatMismatch));
    return;
  }

  create_ring_buffer_completers_.insert_or_assign(element_id, completer.ToAsync());
  bool created = device_->CreateRingBuffer(
      element_id, *driver_format, *request.options()->ring_buffer_min_bytes(),
      [this,
       element_id](fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo> result) {
        // If we have no async completer, maybe we're shutting down. Just exit.
        if (create_ring_buffer_completers_.find(element_id) ==
            create_ring_buffer_completers_.end()) {
          ADR_WARN_OBJECT()
              << "(element_id " << element_id
              << ") create_ring_buffer_completer_ gone by the time the CreateRingBuffer callback ran";
          device_->DropRingBuffer(element_id);  //  Let this unwind the RingBufferServer.
          return;
        }

        auto completer = std::move(create_ring_buffer_completers_.find(element_id)->second);
        create_ring_buffer_completers_.erase(element_id);

        if (result.is_error()) {
          // Based on the driver response, set our ControlServer response
          completer.Reply(fit::error(fad::ControlCreateRingBufferError::kDeviceError));
          return;
        }

        completer.Reply(fit::success(fad::ControlCreateRingBufferResponse{{
            .properties = result.value().properties,
            .ring_buffer = std::move(result.value().ring_buffer),
        }}));
      });

  if (!created) {
    ADR_WARN_METHOD() << "device cannot create a ring buffer with the specified options";
    auto completer = std::move(create_ring_buffer_completers_.find(element_id)->second);
    create_ring_buffer_completers_.erase(element_id);
    completer.Reply(fidl::Response<fad::Control::CreateRingBuffer>(
        fit::error(fad::ControlCreateRingBufferError::kBadRingBufferOption)));
    return;
  }
  auto ring_buffer_server =
      RingBufferServer::Create(thread_ptr(), std::move(*request.ring_buffer_server()),
                               shared_from_this(), device_, element_id);
  AddChildServer(ring_buffer_server);
  ring_buffer_servers_.insert_or_assign(element_id, ring_buffer_server);
}

// This is only here because ControlNotify includes the methods from ObserverNotify. It might be
// helpful for ControlServer to know when its SetGain call took effect, but this isn't needed.
// ControlServer also has no gain-related hanging-get to complete.
void ControlServer::GainStateChanged(const fad::GainState&) { ADR_LOG_METHOD(kLogNotifyMethods); }

// This is only here because ControlNotify includes the methods from ObserverNotify. ControlServer
// doesn't have a role to play in plug state changes, nor a client hanging-get to complete.
void ControlServer::PlugStateChanged(const fad::PlugState& new_plug_state,
                                     zx::time plug_change_time) {
  ADR_LOG_METHOD(kLogNotifyMethods);
}

// We receive delay values for the first time during the configuration process. Once we have these
// values, we can calculate the required ring-buffer size and request the VMO.
void ControlServer::DelayInfoChanged(ElementId element_id, const fad::DelayInfo& delay_info) {
  ADR_LOG_METHOD(kLogControlServerResponses || kLogNotifyMethods);

  // Initialization is complete, so this represents a delay update.
  // If this is eventually exposed to Observers or any other watcher, notify them.
  if (auto ring_buffer_server = TryGetRingBufferServer(element_id); ring_buffer_server) {
    ring_buffer_server->DelayInfoChanged(delay_info);
  }
}

void ControlServer::SetDaiFormat(SetDaiFormatRequest& request,
                                 SetDaiFormatCompleter::Sync& completer) {
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlSetDaiFormatError::kDeviceError));
    return;
  }

  ElementId element_id = fad::kDefaultDaiInterconnectElementId;
  // Fail on missing parameters.
  if (device_->is_composite()) {
    if (!request.element_id().has_value()) {
      ADR_WARN_METHOD() << "required field 'element_id' is missing";
      completer.Reply(fit::error(fad::ControlSetDaiFormatError::kInvalidElementId));
      return;
    }
    element_id = *request.element_id();
  } else if (!device_->is_codec()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlSetDaiFormatError::kWrongDeviceType));
    return;
  }
  if (set_dai_format_completers_.find(element_id) != set_dai_format_completers_.end()) {
    ADR_WARN_METHOD() << "previous `SetDaiFormat` request has not yet completed";
    completer.Reply(fit::error(fad::ControlSetDaiFormatError::kAlreadyPending));
    return;
  }

  if (!request.dai_format().has_value() || !ValidateDaiFormat(*request.dai_format())) {
    ADR_WARN_METHOD() << "required field 'dai_format' is missing or invalid";
    completer.Reply(fit::error(fad::ControlSetDaiFormatError::kInvalidDaiFormat));
    return;
  }

  set_dai_format_completers_.insert_or_assign(element_id, completer.ToAsync());
  device_->SetDaiFormat(element_id, *request.dai_format());

  // We need the CodecFormatInfo to complete this, so we wait for the Device to notify us. Besides,
  // it's also possible that the underlying driver will reject the request (e.g. format mismatch).
}

// The Device's DaiFormat has changed. If `dai_format` is set, this resulted from `SetDaiFormat`
// being called. Otherwise, the Device is newly-initialized or `Reset` was called, so
// SetDaiFormat must be called again.
void ControlServer::DaiFormatChanged(
    ElementId element_id, const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
    const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods) << "(" << element_id << ")";

  auto completer_match = set_dai_format_completers_.find(element_id);

  // Device must be newly-initialized or Reset was called. Either way we don't expect a completion.
  if (!dai_format.has_value()) {
    FX_DCHECK(!codec_format_info.has_value());
    // If there's a completer, it must have been in-progress when Reset  was called; cancel it.
    if (completer_match != set_dai_format_completers_.end()) {
      auto completer = std::move(completer_match->second);
      set_dai_format_completers_.erase(element_id);
      completer.Reply(fit::error(fad::ControlSetDaiFormatError::kOther));
    }
    return;
  }

  // SetDaiFormat was called and succeeded, but now we don't have a completer.
  if (completer_match == set_dai_format_completers_.end()) {
    ADR_WARN_METHOD() << "received Device notification, but completer is gone.";
    return;
  }

  auto completer = std::move(set_dai_format_completers_.find(element_id)->second);
  set_dai_format_completers_.erase(element_id);
  if (codec_format_info.has_value()) {
    completer.Reply(fit::success(fad::ControlSetDaiFormatResponse{{.state = *codec_format_info}}));
  } else {
    completer.Reply(fit::success(fad::ControlSetDaiFormatResponse{{.state = std::nullopt}}));
  }
}

void ControlServer::DaiFormatNotSet(ElementId element_id,
                                    const fuchsia_hardware_audio::DaiFormat& dai_format,
                                    fad::ControlSetDaiFormatError error) {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods)
      << "(" << element_id << ", error " << fidl::ToUnderlying(error) << ") for dai_format:";
  LogDaiFormat(dai_format);

  // SetDaiFormat was called, but now we don't have a completer.
  if (set_dai_format_completers_.find(element_id) == set_dai_format_completers_.end()) {
    ADR_WARN_METHOD()
        << "SetDaiFormat was called, did not result in change, but completer is gone.";
    return;
  }

  auto completer = std::move(set_dai_format_completers_.find(element_id)->second);
  set_dai_format_completers_.erase(element_id);
  // If `error` is 0, SetDaiFormat was not an error but resulted in no change, so succeed that call.
  if (error == fad::ControlSetDaiFormatError(0)) {
    completer.Reply(fit::success(
        fad::ControlSetDaiFormatResponse{{.state = device_->codec_format_info(element_id)}}));
  } else {
    completer.Reply(fit::error(error));
  }
}

void ControlServer::CodecStart(CodecStartCompleter::Sync& completer) {
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlCodecStartError::kDeviceError));
    return;
  }

  if (!device_->is_codec()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlCodecStartError::kWrongDeviceType));
    return;
  }

  // Check for already pending
  if (codec_start_completer_.has_value()) {
    ADR_WARN_METHOD() << "previous `CodecStart` request has not yet completed";
    completer.Reply(fit::error(fad::ControlCodecStartError::kAlreadyPending));
    return;
  }

  if (!device_->dai_format_is_set()) {
    ADR_WARN_METHOD() << "CodecStart called before DaiFormat was set";
    completer.Reply(fit::error(fad::ControlCodecStartError::kDaiFormatNotSet));
    return;
  }

  // Check for already started
  if (device_->codec_is_started()) {
    ADR_WARN_METHOD() << "Codec is already started";
    completer.Reply(fit::error(fad::ControlCodecStartError::kAlreadyStarted));
    return;
  }

  codec_start_completer_ = completer.ToAsync();

  // Call into the Device to Start.
  if (!device_->CodecStart()) {
    auto start_completer = std::move(*codec_start_completer_);
    codec_start_completer_.reset();
    start_completer.Reply(fit::error(fad::ControlCodecStartError::kDeviceError));
  }

  // We need `start_time` to complete this, so we wait for the Device to notify us. Besides,
  // it's also possible that the underlying driver will reject the request.
}

void ControlServer::CodecStarted(const zx::time& start_time) {
  ADR_LOG_METHOD(kLogNotifyMethods) << "(" << start_time.get() << ")";

  if (!codec_start_completer_.has_value()) {
    ADR_WARN_METHOD() << "received notification from Device, but completer is gone";
    return;
  }

  auto completer = std::move(*codec_start_completer_);
  codec_start_completer_.reset();
  completer.Reply(fit::success(fad::ControlCodecStartResponse{{.start_time = start_time.get()}}));
}

void ControlServer::CodecNotStarted() {
  ADR_LOG_METHOD(kLogNotifyMethods);

  if (!codec_start_completer_.has_value()) {
    ADR_WARN_METHOD()
        << "received Device notification (CodecStart rejected), but completer is gone.";
    return;
  }

  auto completer = std::move(*codec_start_completer_);
  codec_start_completer_.reset();
  completer.Reply(fit::error(fad::ControlCodecStartError::kOther));
}

void ControlServer::CodecStop(CodecStopCompleter::Sync& completer) {
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlCodecStopError::kDeviceError));
    return;
  }

  if (!device_->is_codec()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlCodecStopError::kWrongDeviceType));
    return;
  }

  // Check for already pending
  if (codec_stop_completer_.has_value()) {
    ADR_WARN_METHOD() << "previous `CodecStop` request has not yet completed";
    completer.Reply(fit::error(fad::ControlCodecStopError::kAlreadyPending));
    return;
  }

  if (!device_->dai_format_is_set()) {
    ADR_WARN_METHOD() << "CodecStop called before DaiFormat was set";
    completer.Reply(fit::error(fad::ControlCodecStopError::kDaiFormatNotSet));
    return;
  }

  // Check for already stopped
  if (!device_->codec_is_started()) {
    ADR_WARN_METHOD() << "Codec is already stopped";
    completer.Reply(fit::error(fad::ControlCodecStopError::kAlreadyStopped));
    return;
  }

  codec_stop_completer_ = completer.ToAsync();

  // Call into the Device to Stop.
  if (!device_->CodecStop()) {
    auto stop_completer = std::move(*codec_stop_completer_);
    codec_stop_completer_.reset();
    stop_completer.Reply(fit::error(fad::ControlCodecStopError::kDeviceError));
    return;
  }

  // We need `stop_time` to complete this, so we wait for the Device to notify us. Besides,
  // it's also possible that the underlying driver will reject the request.
}

void ControlServer::CodecStopped(const zx::time& stop_time) {
  ADR_LOG_METHOD(kLogNotifyMethods) << "(" << stop_time.get() << ")";

  if (!codec_stop_completer_.has_value()) {
    ADR_LOG_METHOD(kLogNotifyMethods)
        << "received Device notification, but completer is gone. Reset occurred?";
    return;
  }

  auto completer = std::move(*codec_stop_completer_);
  codec_stop_completer_.reset();
  completer.Reply(fit::success(fad::ControlCodecStopResponse{{.stop_time = stop_time.get()}}));
}

void ControlServer::CodecNotStopped() {
  ADR_LOG_METHOD(kLogNotifyMethods);

  if (!codec_stop_completer_.has_value()) {
    ADR_WARN_METHOD()
        << "received Device notification (CodecStop rejected), but completer is gone.";
    return;
  }

  auto completer = std::move(*codec_stop_completer_);
  codec_stop_completer_.reset();
  completer.Reply(fit::error(fad::ControlCodecStopError::kOther));
}

void ControlServer::Reset(ResetCompleter::Sync& completer) {
  if (device_has_error_) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fad::ControlResetError::kDeviceError));
    return;
  }

  if (!device_->is_codec() && !device_->is_composite()) {
    ADR_WARN_METHOD() << "Unsupported method for device_type " << device_->device_type();
    completer.Reply(fit::error(fad::ControlResetError::kWrongDeviceType));
    return;
  }

  if (!device_->Reset()) {
    ADR_WARN_METHOD() << "device had an error during Device::Reset";
    completer.Reply(fit::error(fad::ControlResetError::kDeviceError));
    return;
  }

  completer.Reply(fit::success(fad::ControlResetResponse{}));
}

// fuchsia.hardware.audio.signalprocessing.SignalProcessing support
//
void ControlServer::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Reply(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "This device_type does not support " << __func__;
    completer.Reply(zx::error(ZX_ERR_WRONG_TYPE));
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  FX_CHECK(device_->info().has_value() &&
           device_->info()->signal_processing_topologies().has_value() &&
           !device_->info()->signal_processing_topologies()->empty());
  completer.Reply(zx::ok(*device_->info()->signal_processing_topologies()));
}

void ControlServer::WatchTopology(WatchTopologyCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "This device_type does not support " << __func__;
    completer.Close(ZX_ERR_WRONG_TYPE);
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (watch_topology_completer_) {
    ADR_WARN_METHOD() << "previous `WatchTopology` request has not yet completed";
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  FX_CHECK(!watch_topology_completer_.has_value());
  watch_topology_completer_ = completer.ToAsync();
  FX_CHECK(watch_topology_completer_.has_value());
  MaybeCompleteWatchTopology();
}

void ControlServer::SetTopology(SetTopologyRequest& request,
                                SetTopologyCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Reply(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "unsupported method for this device_type";
    completer.Reply(zx::error(ZX_ERR_WRONG_TYPE));
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  if (device_->topology_ids().find(request.topology_id()) == device_->topology_ids().end()) {
    ADR_WARN_METHOD() << "Unknown topology_id " << request.topology_id();
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (auto status = device_->SetTopology(request.topology_id()); status == ZX_OK) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "SetTopology succeeded";
    completer.Reply(zx::ok());
  } else {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "SetTopology failed: " << status;
    completer.Reply(zx::error(status));
  }
}

void ControlServer::TopologyChanged(TopologyId topology_id) {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods)
      << "(topology_id " << topology_id << ")";

  topology_id_to_notify_ = topology_id;
  MaybeCompleteWatchTopology();
}

void ControlServer::MaybeCompleteWatchTopology() {
  if (watch_topology_completer_ && topology_id_to_notify_) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods)
        << "completing(" << *topology_id_to_notify_ << ")";
    auto completer = std::move(*watch_topology_completer_);
    watch_topology_completer_.reset();

    auto new_topology_id = *topology_id_to_notify_;
    topology_id_to_notify_.reset();

    completer.Reply({new_topology_id});
  } else {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods) << "NOT completing";
  }
}

void ControlServer::GetElements(GetElementsCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogObserverServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Reply(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "This device_type does not support " << __func__;
    completer.Reply(zx::error(ZX_ERR_WRONG_TYPE));
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  FX_CHECK(device_->info().has_value() &&
           device_->info()->signal_processing_elements().has_value() &&
           !device_->info()->signal_processing_elements()->empty());
  completer.Reply(zx::ok(*device_->info()->signal_processing_elements()));
}

void ControlServer::WatchElementState(WatchElementStateRequest& request,
                                      WatchElementStateCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods);

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "This device_type does not support " << __func__;
    completer.Close(ZX_ERR_WRONG_TYPE);
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  if (device_->element_ids().find(request.processing_element_id()) ==
      device_->element_ids().end()) {
    ADR_WARN_METHOD() << "Unknown element_id " << request.processing_element_id();
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  ElementId element_id = request.processing_element_id();
  if (watch_element_state_completers_.find(element_id) != watch_element_state_completers_.end()) {
    ADR_WARN_METHOD() << "previous `WatchElementState(" << element_id
                      << ")` request has not yet completed";
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  watch_element_state_completers_.insert({element_id, completer.ToAsync()});
  MaybeCompleteWatchElementState(element_id);
}

void ControlServer::SetElementState(SetElementStateRequest& request,
                                    SetElementStateCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogControlServerMethods)
      << "(element_id " << request.processing_element_id() << ")";

  if (device_has_error_) {
    ADR_WARN_METHOD() << "Device has error";
    completer.Reply(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  FX_CHECK(device_);
  if (!device_->is_codec() && !device_->is_composite() && !device_->is_stream_config()) {
    ADR_WARN_METHOD() << "unsupported method for this device_type";
    completer.Reply(zx::error(ZX_ERR_WRONG_TYPE));
    return;
  }

  if (!device_->supports_signalprocessing()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogSignalProcessingFidlCalls)
        << "This driver does not support signalprocessing";
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  if (device_->element_ids().find(request.processing_element_id()) ==
      device_->element_ids().end()) {
    ADR_WARN_METHOD() << "Unknown element_id " << request.processing_element_id();
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (auto status = device_->SetElementState(request.processing_element_id(), request.state());
      status == ZX_OK) {
    completer.Reply(zx::ok());
  } else {
    completer.Reply(zx::error(status));
  }
}

void ControlServer::ElementStateChanged(
    ElementId element_id, fuchsia_hardware_audio_signalprocessing::ElementState element_state) {
  ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods)
      << "(element_id " << element_id << ")";

  element_states_to_notify_.insert_or_assign(element_id, element_state);
  MaybeCompleteWatchElementState(element_id);
}

// If we have an outstanding hanging-get and a state-change, respond with the state change.
void ControlServer::MaybeCompleteWatchElementState(ElementId element_id) {
  if (watch_element_state_completers_.find(element_id) != watch_element_state_completers_.end() &&
      element_states_to_notify_.find(element_id) != element_states_to_notify_.end()) {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods) << element_id << ": completing";
    auto completer = std::move(watch_element_state_completers_.find(element_id)->second);
    watch_element_state_completers_.erase(element_id);

    auto new_element_state = element_states_to_notify_.find(element_id)->second;
    element_states_to_notify_.erase(element_id);

    completer.Reply({new_element_state});
  } else {
    ADR_LOG_METHOD(kLogControlServerMethods || kLogNotifyMethods)
        << element_id << ": NOT completing";
  }
}

}  // namespace media_audio
