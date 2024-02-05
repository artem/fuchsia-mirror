// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_CODEC_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_CODEC_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.virtualaudio/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/result.h>

#include <ddktl/device.h>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_driver.h"

namespace virtual_audio {

class VirtualAudioCodec;
using VirtualAudioCodecDeviceType =
    ddk::Device<VirtualAudioCodec, ddk::Messageable<fuchsia_hardware_audio::CodecConnector>::Mixin>;

class VirtualAudioCodec : public VirtualAudioCodecDeviceType,
                          public ddk::internal::base_protocol,
                          public fidl::Server<fuchsia_hardware_audio::Codec>,
                          public VirtualAudioDriver {
 public:
  static fuchsia_virtualaudio::Configuration GetDefaultConfig(std::optional<bool> is_input);

  VirtualAudioCodec(fuchsia_virtualaudio::Configuration config,
                    std::weak_ptr<VirtualAudioDeviceImpl> owner, zx_device_t* parent);
  void ResetCodecState();

  async_dispatcher_t* dispatcher() override {
    return fdf::Dispatcher::GetCurrent()->async_dispatcher();
  }
  void ShutdownAndRemove() override { DdkAsyncRemove(); }
  void DdkRelease() {}

 protected:
  // FIDL LLCPP method for fuchsia.hardware.audio.CodecConnector.
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override;

  // FIDL natural C++ methods for fuchsia.hardware.audio.Codec.
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override;
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override;

  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void IsBridgeable(IsBridgeableCompleter::Sync& completer) override;
  void SetBridgedMode(SetBridgedModeRequest& request,
                      SetBridgedModeCompleter::Sync& completer) override;

  void GetDaiFormats(GetDaiFormatsCompleter::Sync& completer) override;
  void SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) override;

  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;

  void Reset(ResetCompleter::Sync& completer) override;
  void WatchPlugState(WatchPlugStateCompleter::Sync& completer) override;

 private:
  void PlugStateChanged(const fuchsia_hardware_audio::PlugState& plug_state);

  fuchsia_virtualaudio::Codec& codec_config() { return config_.device_specific()->codec().value(); }

  // This should never be invalid: this VirtualAudioCodec should always be destroyed before
  // its parent. This field is a weak_ptr to avoid a circular reference count.
  const std::weak_ptr<VirtualAudioDeviceImpl> parent_;
  static int instance_count_;
  char instance_name_[64];
  std::optional<fuchsia_hardware_audio::DaiFormat> dai_format_;
  fuchsia_virtualaudio::Configuration config_;

  bool should_return_plug_state_ = true;
  fuchsia_hardware_audio::PlugState plug_state_;
  std::optional<WatchPlugStateCompleter::Async> watch_plug_state_completer_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_CODEC_H_
