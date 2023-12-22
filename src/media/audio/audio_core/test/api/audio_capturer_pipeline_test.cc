// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>
#include <lib/media/audio/cpp/types.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <set>
#include <vector>

#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"
#include "src/media/audio/lib/analysis/generators.h"
#include "src/media/audio/lib/test/comparators.h"

using ASF = fuchsia::media::AudioSampleFormat;
using StreamPacket = fuchsia::media::StreamPacket;

namespace media::audio::test {

namespace {
struct CapturedPacket {
  int64_t pts;
  AudioBuffer<ASF::SIGNED_16> data;
};

// Represents a pointer to a specific frame in a vector of packets.
using PacketAndFrameIdx = std::pair<std::vector<CapturedPacket>::const_iterator, int64_t>;

// Represents an AudioRenderer along with its input packets.
struct RendererHolder {
  AudioRendererShim<ASF::SIGNED_16>* renderer;
  RendererShimImpl::PacketVector input_packets;
};
}  // namespace

class AudioCapturerPipelineTest : public HermeticAudioTest {
 protected:
  static constexpr zx::duration kPacketLength = zx::msec(10);
  static constexpr int32_t kFrameRate = 48000;
  static constexpr int64_t kPacketFrames = kFrameRate * kPacketLength.to_msecs() / 1000;
  static_assert((kFrameRate * kPacketLength.to_msecs()) % 1000 == 0);

  AudioCapturerPipelineTest() : format_(Format::Create<ASF::SIGNED_16>(2, kFrameRate).value()) {}

  void SetUp() {
    HermeticAudioTest::SetUp();
    // The payloads can store exactly 1s of audio data.
    input_ = CreateInput({{0xff, 0x00}}, format_, kFrameRate);
    capturer_ = CreateAudioCapturer(format_, kFrameRate,
                                    fuchsia::media::AudioCapturerConfiguration::WithInput(
                                        fuchsia::media::InputAudioCapturerConfiguration()));
  }

  void TearDown() {
    // None of our tests should underflow.
    ExpectNoOverflowsOrUnderflows();
  }

  const TypedFormat<ASF::SIGNED_16> format_;
  VirtualInput<ASF::SIGNED_16>* input_ = nullptr;
  AudioCapturerShim<ASF::SIGNED_16>* capturer_ = nullptr;
};

TEST_F(AudioCapturerPipelineTest, CaptureWithPts) {
  constexpr auto num_packets = 1;
  constexpr auto num_frames = num_packets * kPacketFrames;
  constexpr int16_t first_sample = 1;

  // Start capturing from the input device.
  std::optional<int64_t> first_capture_pts;
  std::optional<int64_t> last_capture_pts;
  AudioBuffer<ASF::SIGNED_16> capture_buffer(format_, 0);

  capturer_->fidl().events().OnPacketProduced = [this, &first_capture_pts, &last_capture_pts,
                                                 &capture_buffer](StreamPacket p) mutable {
    EXPECT_EQ(p.payload_buffer_id, 0u);
    if (!first_capture_pts) {
      EXPECT_TRUE(p.flags & fuchsia::media::STREAM_PACKET_FLAG_DISCONTINUITY);
      first_capture_pts = p.pts;
    } else {
      EXPECT_FALSE(p.flags & fuchsia::media::STREAM_PACKET_FLAG_DISCONTINUITY);
    }
    last_capture_pts = p.pts;
    auto data = capturer_->SnapshotPacket(p);
    capture_buffer.samples().insert(capture_buffer.samples().end(), data.samples().begin(),
                                    data.samples().end());
    capturer_->fidl()->ReleasePacket(p);
  };

  capturer_->fidl()->StartAsyncCapture(kPacketFrames);

  // Write data to the input ring_buffer which should be readable 100ms from now.
  // This gives us plenty of time to start the capturer before the data will be read.
  auto start_time = zx::clock::get_monotonic() + zx::msec(100);
  auto input_frame = input_->RingBufferFrameAtTimestamp(start_time);
  auto input_buffer = GenerateSequentialAudio<ASF::SIGNED_16>(format_, num_frames, first_sample);
  input_->WriteRingBufferAt(input_frame, AudioBufferSlice(&input_buffer));

  // Wait until the last input packet has been captured.
  auto end_time = start_time + kPacketLength * num_packets;
  RunLoopUntil([&last_capture_pts, end_time]() {
    return last_capture_pts && last_capture_pts > end_time.get();
  });

  // Stop the capture so we don't overflow while running the following tests.
  capturer_->fidl().events().OnPacketProduced = nullptr;
  capturer_->fidl()->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();
  Unbind(capturer_);

  // The captured data should be silence up to start_time, followed by the input_buffer,
  // followed by more silence.
  auto data_start_frame = format_.frames_per_ns().Scale(start_time.get() - *first_capture_pts);
  if (capture_buffer.SampleAt(data_start_frame, 0) != first_sample) {
    // Since captured packets are not perfectly aligned with the input ring buffer,
    // the start frame might be off by one.
    if (capture_buffer.SampleAt(data_start_frame - 1, 0) == first_sample) {
      data_start_frame--;
    } else if (capture_buffer.SampleAt(data_start_frame + 1, 0) == first_sample) {
      data_start_frame++;
    } else {
      ADD_FAILURE() << "Start frame not found, expected at frame " << data_start_frame << " +/- 1";
    }
  }
  auto data_end_frame = data_start_frame + num_frames;

  CompareAudioBufferOptions opts;
  opts.num_frames_per_packet = kPacketFrames;
  opts.test_label = "check first silence";
  CompareAudioBuffers(AudioBufferSlice(&capture_buffer, 0, data_start_frame),
                      AudioBufferSlice<ASF::SIGNED_16>(), opts);
  opts.test_label = "check data";
  CompareAudioBuffers(AudioBufferSlice(&capture_buffer, data_start_frame, data_end_frame),
                      AudioBufferSlice(&input_buffer, 0, num_frames), opts);
  opts.test_label = "check second silence";
  CompareAudioBuffers(AudioBufferSlice(&capture_buffer, data_end_frame, capture_buffer.NumFrames()),
                      AudioBufferSlice<ASF::SIGNED_16>(), opts);
}

class AudioCapturerPipelineSWGainTest : public AudioCapturerPipelineTest {
 protected:
  static void SetUpTestSuite() {
    HermeticAudioTest::SetTestSuiteRealmOptions([] {
      return HermeticAudioRealm::Options{
          .audio_core_config_data = MakeAudioCoreConfig({
              .input_device_config = R"x(
                "device_id": "*",
                "supported_stream_types": [
                    "capture:background",
                    "capture:communications",
                    "capture:foreground",
                    "capture:system_agent"
                ],
                "rate": 48000,
                "software_gain_db": 20
              )x",
          }),
      };
    });
  }
};

TEST_F(AudioCapturerPipelineSWGainTest, CaptureWithSWGain) {
  // Fill the input ring buffer with 1s. The actual value doesn't matter as long as it is
  // non-silent but small enough to be scaled up by 20dB without clamping.
  constexpr int16_t input_sample = 1;
  auto input_buffer = GenerateConstantAudio(format_, kFrameRate, input_sample);
  input_->WriteRingBufferAt(0, AudioBufferSlice(&input_buffer));

  // Since the config applies a SW gain of 20.0, the input is scaled by DbToScale(20)*1 = 10.
  bool found_nonsilent_frame = false;
  capturer_->fidl().events().OnPacketProduced = [this,
                                                 &found_nonsilent_frame](StreamPacket p) mutable {
    EXPECT_EQ(p.payload_buffer_id, 0u);
    auto data = capturer_->SnapshotPacket(p);
    capturer_->fidl()->ReleasePacket(p);

    if (found_nonsilent_frame) {
      return;
    }
    for (auto sample : data.samples()) {
      if (sample != 0) {
        EXPECT_EQ(sample, 10);
        found_nonsilent_frame = true;
        return;
      }
    }
  };
  capturer_->fidl()->StartAsyncCapture(kPacketFrames);

  // Wait until a non-silent frame is detected.
  RunLoopWithTimeoutOrUntil([&found_nonsilent_frame]() { return found_nonsilent_frame; },
                            zx::sec(5));
  EXPECT_TRUE(found_nonsilent_frame);
  Unbind(capturer_);
}

class AudioLoopbackPipelineTest : public HermeticAudioTest {
 protected:
  static constexpr zx::duration kPacketLength = zx::msec(10);
  static constexpr int32_t kFrameRate = 48000;
  static constexpr int64_t kPacketFrames = kFrameRate * kPacketLength.to_msecs() / 1000;
  const TypedFormat<ASF::SIGNED_16> format_;

  AudioLoopbackPipelineTest() : format_(Format::Create<ASF::SIGNED_16>(2, kFrameRate).value()) {}

  void TearDown() override {
    if constexpr (kEnableAllOverflowAndUnderflowChecksInRealtimeTests) {
      ExpectNoOverflowsOrUnderflows();
    } else {
      // We expect no renderer underflows: we pre-submit the whole signal. Keep that check enabled.
      ExpectNoRendererUnderflows();
    }

    HermeticAudioTest::TearDown();
  }

  std::optional<PacketAndFrameIdx> FindFirstFrame(const std::vector<CapturedPacket>& packets,
                                                  int16_t first_sample_value) {
    for (auto p = packets.begin(); p != packets.end(); p++) {
      for (int64_t f = 0; f < p->data.NumFrames(); f++) {
        if (p->data.SampleAt(f, 0) == first_sample_value) {
          return std::make_pair(p, f);
        }
      }
    }
    return std::nullopt;
  }

  // Start one renderer for each input and have each renderer play their inputs simulaneously,
  // then validate that the captured output matches the given expected_output.
  void RunTest(std::vector<AudioBuffer<ASF::SIGNED_16>> inputs,
               AudioBuffer<ASF::SIGNED_16> expected_output,
               zx::duration delay_before_data = zx::msec(0)) {
    FX_CHECK(inputs.size() > 0);

    // The output device, renderers, and capturer can each store exactly 1s of audio data.
    CreateOutput({{0xff, 0x00}}, format_, kFrameRate);
    auto capturer = CreateAudioCapturer(format_, kFrameRate,
                                        fuchsia::media::AudioCapturerConfiguration::WithLoopback(
                                            fuchsia::media::LoopbackAudioCapturerConfiguration()));

    // Create one renderer per input.
    std::vector<RendererHolder> renderers;
    int64_t num_input_frames = 0;
    for (auto& i : inputs) {
      auto r = CreateAudioRenderer(format_, kFrameRate);
      renderers.push_back({r, r->AppendSlice(i, kPacketFrames)});
      num_input_frames = std::max(num_input_frames, i.NumFrames());
    }

    // Collect all captured packets.
    std::vector<CapturedPacket> captured_packets;
    capturer->fidl().events().OnPacketProduced = [capturer, &captured_packets](StreamPacket p) {
      EXPECT_EQ(p.payload_buffer_id, 0u);
      captured_packets.push_back({
          .pts = p.pts,
          .data = capturer->SnapshotPacket(p),
      });
      capturer->fidl()->ReleasePacket(p);
    };
    capturer->fidl()->StartAsyncCapture(kPacketFrames);

    // Play inputs starting at `now + min_lead_time + tolerance`, where tolerance estimates
    // the maximum scheduling delay between reading the clock and the last call to Play.
    // The tolerance is somewhat large to reduce flakes on debug builds.
    zx::duration min_lead_time;
    for (auto& r : renderers) {
      min_lead_time = std::max(min_lead_time, r.renderer->min_lead_time());
    }
    const auto tolerance = zx::msec(10);
    auto start_time = zx::clock::get_monotonic() + min_lead_time + tolerance;
    for (auto& r : renderers) {
      r.renderer->Play(this, start_time, 0);
    }
    for (auto& r : renderers) {
      r.renderer->WaitForPackets(this, r.input_packets);
    }

    // Wait until we've captured a packet with pts > start_time + audio duration + 1 packet. The
    // extra packet is included to ensure there is silence after the captured audio -- this helps
    // verify that we capture the correct amount of data. Note that PTS is relative to the
    // capturer's clock, which defaults to the system mono clock.
    //
    // We add an extra frame to "audio duration + 1 packet" because in practice the actual start
    // time might be misaligned by a fractional frame.
    auto ns_per_frame = format_.frames_per_ns().Inverse();
    auto end_time = start_time + zx::nsec(ns_per_frame.Scale(num_input_frames + kPacketFrames + 1));

    RunLoopUntil([&captured_packets, end_time]() {
      return captured_packets.size() > 0 && captured_packets.back().pts > end_time.get();
    });

    // Stop the capture so we don't overflow while running the following tests.
    capturer->fidl().events().OnPacketProduced = nullptr;
    capturer->fidl()->StopAsyncCapture(AddCallback("StopAsyncCapture"));
    ExpectCallbacks();
    Unbind(capturer);
    for (auto& r : renderers) {
      Unbind(r.renderer);
    }

    // Find the first output frame.
    auto first_output_value = expected_output.samples()[0];
    auto first_output_frame = FindFirstFrame(captured_packets, first_output_value);
    ASSERT_FALSE(first_output_frame == std::nullopt)
        << "could not find first data sample 0x" << std::hex << first_output_value
        << " in the captured output";

    // The first output frame should have occurred at start_time, although in practice
    // the actual time may be off by a fractional frame.
    auto [packet_it, frame] = *first_output_frame;
    auto first_output_time = zx::time(packet_it->pts) + zx::nsec(ns_per_frame.Scale(frame));
    EXPECT_LT(std::abs((start_time - first_output_time + delay_before_data).get()),
              ns_per_frame.Scale(1))
        << "first frame output at unexpected time:" << "\n  expected time = " << start_time.get()
        << "\n       got time = " << first_output_time.get()
        << "\n    packet time = " << packet_it->pts;

    // Gather the full captured audio into a buffer and compare vs the expected output.
    AudioBuffer<ASF::SIGNED_16> capture_buffer(format_, 0);
    capture_buffer.samples().insert(capture_buffer.samples().end(),
                                    packet_it->data.samples().begin() + frame * format_.channels(),
                                    packet_it->data.samples().end());

    for (packet_it++; packet_it != captured_packets.end(); packet_it++) {
      capture_buffer.samples().insert(capture_buffer.samples().end(),
                                      packet_it->data.samples().begin(),
                                      packet_it->data.samples().end());
    }

    CompareAudioBufferOptions opts;
    opts.num_frames_per_packet = kPacketFrames;
    opts.test_label = "check data";
    CompareAudioBuffers(AudioBufferSlice(&capture_buffer, 0, expected_output.NumFrames()),
                        AudioBufferSlice(&expected_output), opts);
    opts.test_label = "check silence";
    CompareAudioBuffers(
        AudioBufferSlice(&capture_buffer, expected_output.NumFrames(), capture_buffer.NumFrames()),
        AudioBufferSlice<ASF::SIGNED_16>(), opts);
  }
};

TEST_F(AudioLoopbackPipelineTest, OneRenderer) {
  // With one renderer, the output should match exactly.
  auto num_padding_frames = kPacketFrames;
  auto num_signal_frames = 3 * kPacketFrames;

  auto input = GenerateSilentAudio<ASF::SIGNED_16>(format_, num_padding_frames);
  auto signal = GenerateSequentialAudio<ASF::SIGNED_16>(format_, num_signal_frames, 0x40);
  input.Append(AudioBufferSlice(&signal));

  RunTest({input}, signal, kPacketLength);
}

TEST_F(AudioLoopbackPipelineTest, TwoRenderers) {
  // With two renderers, the output should mix the two inputs.
  auto num_padding_frames = kPacketFrames;
  auto num_signal_frames = 3 * kPacketFrames;

  auto input0 = GenerateSilentAudio<ASF::SIGNED_16>(format_, num_padding_frames);
  auto signal0 = GenerateSequentialAudio<ASF::SIGNED_16>(format_, num_signal_frames, 0x40);
  input0.Append(AudioBufferSlice(&signal0));

  auto input1 = GenerateSilentAudio<ASF::SIGNED_16>(format_, num_padding_frames);
  auto signal1 = GenerateSequentialAudio<ASF::SIGNED_16>(format_, num_signal_frames, 0x1000);
  input1.Append(AudioBufferSlice(&signal1));

  AudioBuffer<ASF::SIGNED_16> out(format_, num_signal_frames);
  for (int64_t f = 0; f < out.NumFrames(); f++) {
    for (int32_t c = 0; c < format_.channels(); c++) {
      out.samples()[out.SampleIndex(f, c)] = signal0.SampleAt(f, c) + signal1.SampleAt(f, c);
    }
  }

  RunTest({input0, input1}, out, kPacketLength);
}

// Although these tests don't look at packet data, they look at timestamps and rely on
// deadline scheduling, hence this test must be executed on real hardware.
class AudioCapturerReleaseTest : public HermeticAudioTest {
 protected:
  // VMO size is rounded up to the nearest multiple of 4096. These numbers are
  // designed so that kNumPackets * kFramesPerPacket rounds up to 4096.
  const int64_t kNumPackets = 5;
  const int64_t kFramesPerPacket = (4096 / sizeof(int16_t)) / kNumPackets;
  const int64_t kBytesPerPacket = kFramesPerPacket * sizeof(int16_t);
  const int32_t kFrameRate = 8000;
  const zx::duration kPacketDuration = zx::nsec(1'000'000'000 * kFramesPerPacket / kFrameRate);

  void SetUp() {
    HermeticAudioTest::SetUp();

    auto format = Format::Create<ASF::SIGNED_16>(1, kFrameRate).value();
    auto num_frames = kNumPackets * kFramesPerPacket;
    capturer_ = CreateAudioCapturer(format, num_frames,
                                    fuchsia::media::AudioCapturerConfiguration::WithInput(
                                        fuchsia::media::InputAudioCapturerConfiguration()));
  }

  AudioCapturerShim<ASF::SIGNED_16>* capturer_;
};

TEST_F(AudioCapturerReleaseTest, AsyncCapture_PacketsManuallyReleased) {
  zx::time start_pts;
  int64_t count = 0;
  capturer_->fidl().events().OnPacketProduced = [this, &count,
                                                 &start_pts](fuchsia::media::StreamPacket p) {
    SCOPED_TRACE(fxl::StringPrintf("packet %lu", count));

    // Check that we're receiving the expected packets.
    auto pts = zx::time(p.pts);
    if (count == 0) {
      start_pts = zx::time(p.pts);
    } else {
      auto got = pts - start_pts;
      auto want = kPacketDuration * count;
      EXPECT_LT(std::abs(got.get() - want.get()), zx::msec(1).get())
          << "\n  expected time: " << want.get() << "\n       got time: " << got.get();
    }

    EXPECT_EQ(p.payload_buffer_id, 0u);
    EXPECT_EQ(p.payload_offset, static_cast<uint64_t>((count % kNumPackets) * kBytesPerPacket));
    EXPECT_EQ(p.payload_size, static_cast<uint64_t>(kBytesPerPacket));
    EXPECT_EQ((count == 0), (p.flags & fuchsia::media::STREAM_PACKET_FLAG_DISCONTINUITY) != 0)
        << "\nflags: " << std::hex << p.flags;
    count++;

    // Manually release.
    capturer_->fidl()->ReleasePacket(p);
  };

  capturer_->fidl()->StartAsyncCapture(static_cast<uint32_t>(kFramesPerPacket));

  // To verify that we're automatically recycling packets, we need to loop
  // through the payload buffer at least twice.
  const zx::duration kLoopTimeout = zx::sec(10);
  RunLoopWithTimeoutOrUntil([this, &count]() { return ErrorOccurred() || count > 2 * kNumPackets; },
                            kLoopTimeout);
  Unbind(capturer_);

  ASSERT_FALSE(ErrorOccurred());
  ASSERT_GT(count, 2 * kNumPackets);
  ExpectNoOverflowsOrUnderflows();
}

TEST_F(AudioCapturerReleaseTest, AsyncCapture_PacketsNotManuallyReleased) {
  std::vector<fuchsia::media::StreamPacket> packets;

  // Do NOT manually release any packets.
  zx::time start_pts;
  int64_t count = 0;
  capturer_->fidl().events().OnPacketProduced = [this, &count, &start_pts,
                                                 &packets](fuchsia::media::StreamPacket p) {
    SCOPED_TRACE(fxl::StringPrintf("packet %lu", count));

    // Check that we're receiving the expected packets.
    auto pts = zx::time(p.pts);
    if (count == 0) {
      start_pts = zx::time(p.pts);
    } else {
      auto got = pts - start_pts;
      auto want = kPacketDuration * count;
      EXPECT_LT(std::abs(got.get() - want.get()), zx::msec(1).get())
          << "\n  expected time: " << want.get() << "\n       got time: " << got.get();
    }

    EXPECT_EQ(p.payload_buffer_id, 0u);
    EXPECT_EQ(p.payload_offset, static_cast<uint64_t>((count % kNumPackets) * kBytesPerPacket));
    EXPECT_EQ(p.payload_size, static_cast<uint64_t>(kBytesPerPacket));
    EXPECT_EQ((count == 0), (p.flags & fuchsia::media::STREAM_PACKET_FLAG_DISCONTINUITY) != 0)
        << "\nflags: " << std::hex << p.flags;
    count++;

    // Save so we can release these later.
    packets.push_back(p);
  };

  capturer_->fidl()->StartAsyncCapture(static_cast<uint32_t>(kFramesPerPacket));

  // We expect exactly kNumPackets.
  const zx::duration kLoopTimeout = zx::sec(10);
  RunLoopWithTimeoutOrUntil([this, &count]() { return ErrorOccurred() || count >= kNumPackets; },
                            kLoopTimeout);

  // To verify that we don't get additional packets, wait for the duration
  // of one more loop through the payload buffer.
  RunLoopWithTimeoutOrUntil([this]() { return ErrorOccurred(); }, kPacketDuration * kNumPackets);

  ASSERT_FALSE(ErrorOccurred());
  ASSERT_EQ(count, kNumPackets);

  // After releasing all packets, we should get at least one more packet.
  // This packet has a discontinuous timestamp.
  count = 0;
  capturer_->fidl().events().OnPacketProduced = [this, &count,
                                                 &start_pts](fuchsia::media::StreamPacket p) {
    SCOPED_TRACE(fxl::StringPrintf("after release, packet %lu", count));

    // All further packets should be some time after the endpoint of the last released packet.
    auto pts = zx::time(p.pts) - start_pts;
    auto last_end_pts = kPacketDuration * kNumPackets;
    EXPECT_GT(pts.get(), last_end_pts.get());
    EXPECT_EQ(p.payload_buffer_id, 0u);
    EXPECT_EQ(p.payload_size, static_cast<uint64_t>(kBytesPerPacket));
    EXPECT_EQ((count == 0), (p.flags & fuchsia::media::STREAM_PACKET_FLAG_DISCONTINUITY) != 0)
        << "\nflags: " << std::hex << p.flags;
    count++;
  };

  for (auto& p : packets) {
    capturer_->fidl()->ReleasePacket(p);
  }
  RunLoopWithTimeoutOrUntil([this, &count]() { return ErrorOccurred() || count > 0; },
                            kLoopTimeout);
  ASSERT_FALSE(ErrorOccurred());
  ASSERT_GT(count, 0u);

  // Since we did not release any of the initial captured packets in a timely manner,
  // there should be at least one overflow.
  ExpectInspectMetrics(capturer_, {
                                      .children =
                                          {
                                              {"overflows", {.nonzero_uints = {"count"}}},
                                          },
                                  });
  Unbind(capturer_);
}

////// Need to add similar tests for the Capture pipeline
// TODO(mpuryear): validate signal gets bit-for-bit from driver to capturer
// TODO(mpuryear): test OnPacketProduced timing etc.
// TODO(mpuryear): test OnEndOfStream
// TODO(mpuryear): test ReleasePacket
// TODO(mpuryear): test DiscardAllPackets timing etc.
// TODO(mpuryear): test DiscardAllPacketsNoReply timing etc.
// Also: correct routing of loopback

}  // namespace media::audio::test
