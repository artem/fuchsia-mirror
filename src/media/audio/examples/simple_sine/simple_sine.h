// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_SINE_SIMPLE_SINE_H_
#define SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_SINE_SIMPLE_SINE_H_

#include <fuchsia/media/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/sys/cpp/component_context.h>

namespace examples {

class MediaApp {
 public:
  explicit MediaApp(fit::closure quit_callback);

  void Run(sys::ComponentContext* app_context);

 private:
  void AcquireAudioRenderer(sys::ComponentContext* app_context);
  void AcceptAudioRendererDefaultClock();
  void SetStreamType();

  zx_status_t CreateMemoryMapping();

  void WriteAudioIntoBuffer();

  fuchsia::media::StreamPacket CreatePacket(uint32_t packet_num) const;
  void SendPacket(fuchsia::media::StreamPacket packet);
  void OnSendPacketComplete();

  void Shutdown();

  fit::closure quit_callback_;

  fuchsia::media::AudioRendererPtr audio_renderer_;

  fzl::VmoMapper payload_buffer_;
  uint32_t payload_size_;
  uint32_t total_mapping_size_;

  uint32_t num_packets_sent_ = 0u;
  uint32_t num_packets_completed_ = 0u;
};

}  // namespace examples

#endif  // SRC_MEDIA_AUDIO_EXAMPLES_SIMPLE_SINE_SIMPLE_SINE_H_
