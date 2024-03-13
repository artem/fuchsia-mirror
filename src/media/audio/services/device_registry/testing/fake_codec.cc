// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_codec.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fit/result.h>
#include <zircon/errors.h>

#include <iterator>

#include <gtest/gtest.h>

namespace media_audio {
using fuchsia_hardware_audio::Codec;

const fuchsia_hardware_audio::DaiFrameFormat FakeCodec::kDefaultFrameFormat =
    fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
        fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S);

const std::vector<uint32_t> FakeCodec::kDefaultNumberOfChannelsSet{
    FakeCodec::kDefaultNumberOfChannels};
const std::vector<fuchsia_hardware_audio::DaiSampleFormat> FakeCodec::kDefaultSampleFormatsSet{
    FakeCodec::kDefaultDaiSampleFormat};
const std::vector<fuchsia_hardware_audio::DaiFrameFormat> FakeCodec::kDefaultFrameFormatsSet{
    FakeCodec::kDefaultFrameFormat};
const std::vector<uint32_t> FakeCodec::kDefaultFrameRatesSet{FakeCodec::kDefaultFrameRates};
const std::vector<uint8_t> FakeCodec::kDefaultBitsPerSlotSet{FakeCodec::kDefaultBitsPerSlot};
const std::vector<uint8_t> FakeCodec::kDefaultBitsPerSampleSet{FakeCodec::kDefaultBitsPerSample};

const fuchsia_hardware_audio::DaiSupportedFormats FakeCodec::kDefaultDaiFormatSet{{
    .number_of_channels = FakeCodec::kDefaultNumberOfChannelsSet,
    .sample_formats = FakeCodec::kDefaultSampleFormatsSet,
    .frame_formats = FakeCodec::kDefaultFrameFormatsSet,
    .frame_rates = FakeCodec::kDefaultFrameRatesSet,
    .bits_per_slot = FakeCodec::kDefaultBitsPerSlotSet,
    .bits_per_sample = FakeCodec::kDefaultBitsPerSampleSet,
}};

const std::vector<fuchsia_hardware_audio::DaiSupportedFormats> FakeCodec::kDefaultDaiFormatSets{
    FakeCodec::kDefaultDaiFormatSet};

FakeCodec::FakeCodec(zx::channel server_end, zx::channel client_end, async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher),
      server_end_(std::move(server_end)),
      client_end_(std::move(client_end)) {
  ADR_LOG_METHOD(kLogFakeCodec || kLogObjectLifetimes);

  set_stream_unique_id(kDefaultUniqueInstanceId);
  SetDefaultFormatSets();
}

FakeCodec::~FakeCodec() { ADR_LOG_METHOD(kLogFakeCodec || kLogObjectLifetimes); }

void on_unbind(FakeCodec* fake_codec, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_hardware_audio::Codec> server_end) {
  ADR_LOG(kLogFakeCodec || kLogObjectLifetimes) << "FakeCodec disconnected";
}

fidl::ClientEnd<fuchsia_hardware_audio::Codec> FakeCodec::Enable() {
  ADR_LOG_METHOD(kLogFakeCodec);
  EXPECT_TRUE(server_end_.is_valid());
  EXPECT_TRUE(client_end_.is_valid());
  EXPECT_TRUE(dispatcher_);
  EXPECT_FALSE(binding_);

  binding_ = fidl::BindServer(dispatcher_, std::move(server_end_), this);
  EXPECT_FALSE(server_end_.is_valid());

  return std::move(client_end_);
}

void FakeCodec::DropCodec() {
  ADR_LOG_METHOD(kLogFakeCodec);
  health_completer_.reset();
  watch_plug_state_completer_.reset();
  binding_->Close(ZX_ERR_PEER_CLOSED);
}

void FakeCodec::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  if (responsive_) {
    if (healthy_.has_value()) {
      completer.Reply(fuchsia_hardware_audio::HealthState{{healthy_.value()}});
    } else {
      completer.Reply({});
    }
  } else {
    health_completer_.emplace(completer.ToAsync());  // Just pend it; never respond.
  }
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::SignalProcessingConnect(SignalProcessingConnectRequest& request,
                                        SignalProcessingConnectCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);
  if (!supports_signalprocessing_) {
    request.protocol().Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
  // Not yet implemented.
  request.protocol().Close(ZX_ERR_NOT_SUPPORTED);
}

void FakeCodec::GetProperties(GetPropertiesCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);
  // Gather the properties and return them.
  fuchsia_hardware_audio::CodecProperties codec_properties{};
  if (is_input_) {
    codec_properties.is_input(*is_input_);
  }
  if (manufacturer_) {
    codec_properties.manufacturer(*manufacturer_);
  }
  if (product_) {
    codec_properties.product(*product_);
  }
  if (uid_) {
    codec_properties.unique_id(*uid_);
  }
  if (plug_detect_capabilities_) {
    codec_properties.plug_detect_capabilities(*plug_detect_capabilities_);
  }

  completer.Reply(codec_properties);
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  completer.Reply(fit::ok(std::vector<fuchsia_hardware_audio::DaiSupportedFormats>{
      {{
          .number_of_channels = number_of_channels_,
          .sample_formats = sample_formats_,
          .frame_formats = frame_formats_,
          .frame_rates = frame_rates_,
          .bits_per_slot = bits_per_slot_,
          .bits_per_sample = bits_per_sample_,
      }},
  }));
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

bool FakeCodec::CheckDaiFormatSupported(const fuchsia_hardware_audio::DaiFormat& candidate) {
  return (std::find(number_of_channels_.begin(), number_of_channels_.end(),
                    candidate.number_of_channels()) != std::end(number_of_channels_) &&
          candidate.channels_to_use_bitmask() < (1u << candidate.number_of_channels()) &&
          std::find(sample_formats_.begin(), sample_formats_.end(), candidate.sample_format()) !=
              std::end(sample_formats_) &&
          std::find(frame_formats_.begin(), frame_formats_.end(), candidate.frame_format()) !=
              std::end(frame_formats_) &&
          std::find(frame_rates_.begin(), frame_rates_.end(), candidate.frame_rate()) !=
              std::end(frame_rates_) &&
          std::find(bits_per_slot_.begin(), bits_per_slot_.end(), candidate.bits_per_slot()) !=
              std::end(bits_per_slot_) &&
          std::find(bits_per_sample_.begin(), bits_per_sample_.end(),
                    candidate.bits_per_sample()) != std::end(bits_per_sample_));
}
void FakeCodec::SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  if (!CheckDaiFormatSupported(request.format())) {
    completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  selected_format_.emplace(request.format());

  fuchsia_hardware_audio::CodecFormatInfo info;
  if (external_delay_) {
    info.external_delay(external_delay_->get());
  }
  if (turn_on_delay_) {
    info.turn_on_delay(turn_on_delay_->get());
  }
  if (turn_off_delay_) {
    info.turn_off_delay(turn_off_delay_->get());
  }

  completer.Reply(fit::ok(info));
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::Start(StartCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  EXPECT_TRUE(selected_format_);
  if (!is_running_) {
    mono_start_time_ = zx::clock::get_monotonic();
    is_running_ = true;
  }

  completer.Reply(mono_start_time_.get());
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::Stop(StopCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  if (is_running_) {
    mono_stop_time_ = zx::clock::get_monotonic();
    is_running_ = false;
  }

  completer.Reply(mono_stop_time_.get());
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::Reset(ResetCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  if (is_running_) {
    mono_stop_time_ = zx::clock::get_monotonic();
    is_running_ = false;
  }
  if (selected_format_) {
    selected_format_.reset();
    external_delay_.reset();
    turn_on_delay_.reset();
    turn_off_delay_.reset();
  }

  // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
  // Reset all signalprocessing Elements and the signalprocessing Topology.

  completer.Reply();
  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::WatchPlugState(WatchPlugStateCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeCodec);

  ASSERT_FALSE(watch_plug_state_completer_.has_value())
      << " received second WatchPlugState while first is still pending";

  // If pending plug state change, clear the dirty bit and return the state with this completer.
  if (plug_has_changed_) {
    plug_has_changed_ = false;
    completer.Reply(fuchsia_hardware_audio::PlugState{{
        .plugged = plugged_,
        .plug_state_time = plug_state_time_.get(),
    }});
  } else {
    watch_plug_state_completer_ = completer.ToAsync();
  }

  ADR_LOG_METHOD(kLogFakeCodec) << "exit";
}

void FakeCodec::InjectPluggedAt(zx::time plug_time) {
  ADR_LOG_METHOD(kLogFakeCodec) << "(plugged, " << plug_time.get() << ")";
  ASSERT_TRUE(!watch_plug_state_completer_ || !plug_has_changed_)
      << "Inconsistent state: WatchPlugState is pending, but a plug change is waiting to be reported";

  // Compare to previously-reported plug state: only report _changes_ that occurred _later_.
  if (!plugged_ && plug_time > plug_state_time_) {
    plugged_ = true;
    plug_state_time_ = plug_time;

    HandlePlugResponse();
  }
}

void FakeCodec::InjectUnpluggedAt(zx::time plug_time) {
  ADR_LOG_METHOD(kLogFakeCodec) << "(unplugged, " << plug_time.get() << ")";
  ASSERT_TRUE(!watch_plug_state_completer_ || !plug_has_changed_)
      << "Inconsistent state: WatchPlugState is pending, but a plug change is waiting to be reported";

  // Compare to previously-reported plug state: only report _changes_ that occurred _later_.
  if (plugged_ && plug_time > plug_state_time_) {
    plugged_ = false;
    plug_state_time_ = plug_time;

    HandlePlugResponse();
  }
}

void FakeCodec::HandlePlugResponse() {
  // A WatchPlugState is pending; complete it.
  if (watch_plug_state_completer_) {
    watch_plug_state_completer_->Reply(fuchsia_hardware_audio::PlugState{{
        .plugged = plugged_,
        .plug_state_time = plug_state_time_.get(),
    }});
    watch_plug_state_completer_.reset();
    plug_has_changed_ = false;
  } else {  // Pend this plug change for a WatchPlugState call in the future.
    plug_has_changed_ = true;
  }
}

}  // namespace media_audio
