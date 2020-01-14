// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/testing/fake_audio_renderer.h"

#include <lib/async/cpp/time.h>

#include "src/media/audio/audio_core/audio_output.h"
#include "src/media/audio/audio_core/mixer/constants.h"
#include "src/media/audio/audio_core/packet.h"

namespace media::audio::testing {
namespace {

const fuchsia::media::AudioStreamType kDefaultStreamType{
    .sample_format = fuchsia::media::AudioSampleFormat::FLOAT,
    .channels = 2,
    .frames_per_second = 48000,
};

}

// static
std::shared_ptr<FakeAudioRenderer> FakeAudioRenderer::CreateWithDefaultFormatInfo(
    async_dispatcher_t* dispatcher) {
  return FakeAudioRenderer::Create(dispatcher, Format::Create(kDefaultStreamType),
                                   fuchsia::media::AudioRenderUsage::MEDIA);
}

FakeAudioRenderer::FakeAudioRenderer(async_dispatcher_t* dispatcher, fbl::RefPtr<Format> format,
                                     fuchsia::media::AudioRenderUsage usage)
    : AudioObject(AudioObject::Type::AudioRenderer),
      dispatcher_(dispatcher),
      format_(format),
      usage_(usage),
      packet_factory_(dispatcher, *format, 2 * PAGE_SIZE) {}

void FakeAudioRenderer::EnqueueAudioPacket(float sample, zx::duration duration,
                                           fit::closure callback) {
  FX_CHECK(format_valid());

  auto packet_ref = packet_factory_.CreatePacket(sample, duration, std::move(callback));
  if (packet_ref->start() == FractionalFrames<int64_t>(0)) {
    zx::duration min_lead_time = FindMinLeadTime();
    auto now = async::Now(dispatcher_) + min_lead_time;
    auto frac_fps = FractionalFrames<int32_t>(format()->frames_per_second());
    auto rate = TimelineRate(frac_fps.raw_value(), zx::sec(1).to_nsecs());
    timeline_function_->Update(TimelineFunction(0, now.get(), rate));
  }

  for (auto& [_, packet_queue] : packet_queues_) {
    packet_queue->PushPacket(packet_ref);
  }
}

zx::duration FakeAudioRenderer::FindMinLeadTime() {
  TRACE_DURATION("audio", "AudioRendererImpl::RecomputeMinLeadTime");
  zx::duration cur_lead_time;

  ForEachDestLink([&cur_lead_time](AudioLink& link) {
    if (link.GetDest().is_output()) {
      const auto& output = static_cast<const AudioOutput&>(link.GetDest());
      cur_lead_time = std::max(cur_lead_time, output.min_lead_time());
    }
  });

  return cur_lead_time;
}

fit::result<std::shared_ptr<Stream>, zx_status_t> FakeAudioRenderer::InitializeDestLink(
    const AudioObject& dest) {
  auto queue = std::make_shared<PacketQueue>(*format(), timeline_function_);
  packet_queues_.insert({&dest, queue});
  return fit::ok(std::move(queue));
}

void FakeAudioRenderer::CleanupDestLink(const AudioObject& dest) {
  auto it = packet_queues_.find(&dest);
  FX_CHECK(it != packet_queues_.end());
  packet_queues_.erase(it);
}

}  // namespace media::audio::testing
