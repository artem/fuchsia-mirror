// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_codec.h"

#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <fidl/fuchsia.virtualaudio/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/fit/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <audio-proto-utils/format-utils.h>
#include <fbl/algorithm.h>

namespace virtual_audio {

// static
int VirtualAudioCodec::instance_count_ = 0;

// static
fuchsia_virtualaudio::Configuration VirtualAudioCodec::GetDefaultConfig(
    std::optional<bool> is_input) {
  fuchsia_virtualaudio::Configuration config = {};
  config.device_name(std::string("Virtual Audio Codec Device") +
                     (is_input ? (*is_input ? " (input)" : " (output)") : " (no direction)"));
  config.manufacturer_name("Fuchsia Virtual Audio Group");
  config.product_name("Virgil v2, a Virtual Volume Vessel");
  config.unique_id(std::array<uint8_t, 16>({1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0}));

  // Driver type is Codec.
  fuchsia_virtualaudio::Codec codec = {};
  codec.is_input(is_input);
  codec.plug_properties() = {{
      .plug_state = fuchsia_hardware_audio::PlugState{{.plugged = true, .plug_state_time = 0}},
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
  }};

  // Codec interconnect (to/from a DAI).
  fuchsia_virtualaudio::DaiInterconnect dai_interconnect = {};

  // By default, expose a single Codec format: 48kHz I2S (2 channels, 16-in-32, 8-byte frames).
  fuchsia_hardware_audio::DaiSupportedFormats dai_format_set = {};
  dai_format_set.number_of_channels(std::vector<uint32_t>{2});
  dai_format_set.sample_formats(std::vector{fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned});
  dai_format_set.frame_formats(
      std::vector{fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)});
  dai_format_set.frame_rates(std::vector<uint32_t>{48'000});
  dai_format_set.bits_per_slot(std::vector<uint8_t>{32});
  dai_format_set.bits_per_sample(std::vector<uint8_t>{16});

  dai_interconnect.dai_supported_formats(
      std::optional<std::vector<fuchsia_hardware_audio::DaiSupportedFormats>>{std::in_place,
                                                                              {dai_format_set}});
  codec.dai_interconnect(std::move(dai_interconnect));
  config.device_specific() = fuchsia_virtualaudio::DeviceSpecific::WithCodec(std::move(codec));

  return config;
}

VirtualAudioCodec::VirtualAudioCodec(fuchsia_virtualaudio::Configuration config,
                                     std::weak_ptr<VirtualAudioDeviceImpl> owner,
                                     zx_device_t* parent)
    : VirtualAudioCodecDeviceType(parent), parent_(std::move(owner)), config_(std::move(config)) {
  ddk_proto_id_ = ZX_PROTOCOL_CODEC;
  sprintf(instance_name_, "virtual-audio-codec-%d", instance_count_++);
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs(instance_name_));
  ZX_ASSERT_MSG(status == ZX_OK, "DdkAdd failed");

  if (config.device_specific()->codec()->plug_properties()->plug_state()) {
    plug_state_ = *config.device_specific()->codec()->plug_properties()->plug_state();
  }
}

void VirtualAudioCodec::ResetCodecState() {
  should_return_plug_state_ = true;
  watch_plug_state_completer_.reset();
}

void VirtualAudioCodec::Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) {
  fidl::BindServer(
      dispatcher(), std::move(request->codec_protocol), this,
      [](VirtualAudioCodec* codec_instance, fidl::UnbindInfo,
         fidl::ServerEnd<fuchsia_hardware_audio::Codec>) { codec_instance->ResetCodecState(); });
}

// FIDL natural C++ methods for fuchsia.hardware.audio.Codec.
void VirtualAudioCodec::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
}

void VirtualAudioCodec::SignalProcessingConnect(SignalProcessingConnectRequest& request,
                                                SignalProcessingConnectCompleter::Sync& completer) {
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  request.protocol().Close(ZX_ERR_NOT_SUPPORTED);
}

void VirtualAudioCodec::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Codec>::GetPropertiesCompleter::Sync& completer) {
  auto parent = parent_.lock();
  ZX_ASSERT(parent);

  fidl::Arena arena;
  fuchsia_hardware_audio::CodecProperties properties;
  properties.is_input(codec_config().is_input());
  properties.manufacturer(config_.manufacturer_name());
  properties.product(config_.product_name());
  if (config_.unique_id()) {
    properties.unique_id() = "";
    for (auto i = 0; i < 16; ++i) {
      properties.unique_id()->push_back(config_.unique_id()->at(i));
    }
  }
  if (config_.device_specific()) {
    ZX_ASSERT_MSG(
        config_.device_specific()->Which() == fuchsia_virtualaudio::DeviceSpecific::Tag::kCodec,
        "Codec::GetProperties with wrong (non-Codec) device type");

    if (config_.device_specific()->codec()->plug_properties() &&
        config_.device_specific()->codec()->plug_properties()->plug_detect_capabilities()) {
      properties.plug_detect_capabilities() =
          config_.device_specific()->codec()->plug_properties()->plug_detect_capabilities();
    }
  }
  completer.Reply(std::move(properties));
}

void VirtualAudioCodec::GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) {
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  completer.Reply(zx::ok(codec_config().dai_interconnect()->dai_supported_formats().value()));
}

void VirtualAudioCodec::SetDaiFormat(SetDaiFormatRequest& request,
                                     SetDaiFormatCompleter::Sync& completer) {
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  if (request.format().frame_rate() > 192000) {
    completer.Close(ZX_ERR_INVALID_ARGS);
  }
  fuchsia_hardware_audio::CodecFormatInfo codec_info{{
      .external_delay = 123,
      .turn_on_delay = 234,
      .turn_off_delay = 345,
  }};
  completer.Reply(fit::ok(codec_info));
}

void VirtualAudioCodec::Start(StartCompleter::Sync& completer) {
  completer.Reply(zx::clock::get_monotonic().get());
}

void VirtualAudioCodec::Stop(StopCompleter::Sync& completer) {
  completer.Reply(zx::clock::get_monotonic().get());
}

void VirtualAudioCodec::Reset(ResetCompleter::Sync& completer) { completer.Reply(); }

void VirtualAudioCodec::WatchPlugState(WatchPlugStateCompleter::Sync& completer) {
  if (should_return_plug_state_) {
    should_return_plug_state_ = false;
    completer.Reply(plug_state_);
    return;
  }

  if (watch_plug_state_completer_) {
    completer.Close(ZX_ERR_INVALID_ARGS);
    watch_plug_state_completer_.reset();
    return;
  }
  watch_plug_state_completer_ = completer.ToAsync();
}

void VirtualAudioCodec::PlugStateChanged(const fuchsia_hardware_audio::PlugState& plug_state) {
  plug_state_ = plug_state;

  if (watch_plug_state_completer_) {
    should_return_plug_state_ = false;
    watch_plug_state_completer_->Reply(plug_state_);
    return;
  }
  should_return_plug_state_ = true;
}

}  // namespace virtual_audio
