// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V1_USAGE_GAIN_REPORTER_IMPL_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V1_USAGE_GAIN_REPORTER_IMPL_H_

#include <fuchsia/media/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>

#include <unordered_set>

#include "src/media/audio/audio_core/shared/device_lister.h"
#include "src/media/audio/audio_core/shared/loudness_transform.h"
#include "src/media/audio/audio_core/shared/process_config.h"
#include "src/media/audio/audio_core/shared/stream_volume_manager.h"

namespace media::audio {

class UsageGainReporterImpl : public fuchsia::media::UsageGainReporter {
 public:
  UsageGainReporterImpl(DeviceLister& device_lister, StreamVolumeManager& stream_volume_manager,
                        const ProcessConfig& process_config)
      : device_lister_(device_lister),
        stream_volume_manager_(stream_volume_manager),
        process_config_(process_config) {}

  fidl::InterfaceRequestHandler<fuchsia::media::UsageGainReporter> GetFidlRequestHandler();

  // |fuchsia::media::UsageGainReporter|
  void RegisterListener(
      std::string device_unique_id, fuchsia::media::Usage usage,
      fidl::InterfaceHandle<fuchsia::media::UsageGainListener> usage_gain_listener_handler) final;

 private:
  friend class UsageGainReporterTest;

  class Listener final : public StreamVolume {
   public:
    Listener(UsageGainReporterImpl& parent,
             const DeviceConfig::OutputDeviceProfile& output_device_profile,
             fuchsia::media::Usage usage, fuchsia::media::UsageGainListenerPtr usage_gain_listener);

   private:
    // |media::audio::StreamVolume|
    fuchsia::media::Usage GetStreamUsage() const final { return fidl::Clone(usage_); }
    void RealizeVolume(VolumeCommand volume_command) final;

    UsageGainReporterImpl& parent_;
    std::shared_ptr<LoudnessTransform> loudness_transform_;
    bool independent_volume_control_;
    fuchsia::media::Usage usage_;
    fuchsia::media::UsageGainListenerPtr usage_gain_listener_;
    size_t unacked_messages_ = 0;
  };

  // TODO(https://fxbug.dev/50074): Queue a function on the async loop to periodically execute and
  // clean up any listeners with too many unacked messages

  // TODO(https://fxbug.dev/50596): Disconnect listeners upon device removal

  DeviceLister& device_lister_;
  StreamVolumeManager& stream_volume_manager_;
  const ProcessConfig& process_config_;
  std::unordered_map<Listener*, std::unique_ptr<Listener>> listeners_;
  fidl::BindingSet<fuchsia::media::UsageGainReporter, UsageGainReporterImpl*> bindings_;
};

}  // namespace media::audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V1_USAGE_GAIN_REPORTER_IMPL_H_
