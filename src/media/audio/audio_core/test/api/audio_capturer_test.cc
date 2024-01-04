// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/media/cpp/fidl.h>
#include <lib/media/audio/cpp/types.h>
#include <lib/zx/clock.h>

#include "src/media/audio/audio_core/testing/integration/hermetic_audio_test.h"
#include "src/media/audio/lib/clock/clone_mono.h"
#include "src/media/audio/lib/clock/testing/clock_test.h"

using ASF = fuchsia::media::AudioSampleFormat;

namespace media::audio::test {

//
// AudioCapturerTestOldAPI
//
// "OldAPI" because these tests haven't been updated to use the new HermeticAudioTest Create
// functions.
class AudioCapturerTestOldAPI : public HermeticAudioTest {
 protected:
  void SetUp() override {
    HermeticAudioTest::SetUp();

    audio_core_->CreateAudioCapturer(false, audio_capturer_.NewRequest());
    AddErrorHandler(audio_capturer_, "AudioCapturer");
  }

  void TearDown() override {
    gain_control_.Unbind();
    audio_capturer_.Unbind();

    HermeticAudioTest::TearDown();
  }

  void SetFormat(int32_t frames_per_second = 16000) {
    auto t = media::CreateAudioStreamType(fuchsia::media::AudioSampleFormat::SIGNED_16, 1,
                                          frames_per_second);
    format_ = Format::Create(t).take_value();
    audio_capturer_->SetPcmStreamType(t);
  }

  void SetUpPayloadBuffer(int64_t num_frames = 16000, zx::vmo* vmo_out = nullptr) {
    zx::vmo audio_capturer_vmo;

    auto status = zx::vmo::create(num_frames * sizeof(int16_t), 0, &audio_capturer_vmo);
    ASSERT_EQ(status, ZX_OK) << "Failed to create payload buffer";

    if (vmo_out) {
      status = audio_capturer_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, vmo_out);
      ASSERT_EQ(status, ZX_OK);
    }

    audio_capturer_->AddPayloadBuffer(0, std::move(audio_capturer_vmo));
  }

  std::optional<Format> format_;
  fuchsia::media::AudioCapturerPtr audio_capturer_;
  fuchsia::media::audio::GainControlPtr gain_control_;
};

class AudioCapturerClockTestOldAPI : public AudioCapturerTestOldAPI {
 protected:
  // The clock received from GetRefClock is read-only, but the original can still be adjusted.
  static constexpr auto kClockRights = ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER | ZX_RIGHT_READ;

  zx::clock GetAndValidateReferenceClock() {
    zx::clock clock;

    audio_capturer_->GetReferenceClock(
        AddCallback("GetReferenceClock",
                    [&clock](zx::clock received_clock) { clock = std::move(received_clock); }));

    ExpectCallbacks();

    return clock;
  }

  // Wait for >1 mix job, because clock-related disconnects start in SetReferenceClock on the FIDL
  // thread, but must transfer to the mix thread to complete. Although our deadline-scheduler mix
  // period is 10ms, our mix thread will not become idle until it finishes the entire mix job.
  // Capture mix jobs can be as long as 50ms (see kMaxTimePerCapture in base_capturer.cc).
  void WaitBeforeClockRelatedDisconnect() {
    usleep(150 * 1000);  // 3 x 50ms.

    audio_capturer_->GetReferenceClock([](zx::clock) {});
  }
};

//
// Test cases
//
// AudioCapturer implements the base classes StreamBufferSet and StreamSource.

// StreamBufferSet methods
//

// TODO(mpuryear): test AddPayloadBuffer(uint32 id, handle<vmo> payload_buffer);
// Also negative testing: bad id, null or bad handle

// TODO(mpuryear): test RemovePayloadBuffer(uint32 id);
// Also negative testing: unknown or already-removed id

// TODO(mpuryear): apply same tests to AudioRenderer and AudioCapturer
// (although their implementations within AudioCore differ somewhat).

// StreamSource methods
//

// TODO(mpuryear): test -> OnPacketProduced(StreamPacket packet);
// Always received for every packet - even malformed ones?

// TODO(mpuryear): test -> OnEndOfStream();
// Also proper sequence vis-a-vis other completion and disconnect callbacks
// Also negative testing: malformed or non-submitted packet, before started
//
// Also capture StreamPacket flags

// DiscardAllPackets waits to deliver its completion callback until all packets have returned.
TEST_F(AudioCapturerTestOldAPI, DiscardAllReturnsAfterAllPackets) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_->CaptureAt(0, 0, 4000, AddCallback("CaptureAt 0"));
  audio_capturer_->CaptureAt(0, 4000, 4000, AddCallback("CaptureAt 4000"));
  audio_capturer_->CaptureAt(0, 8000, 4000, AddCallback("CaptureAt 8000"));
  audio_capturer_->CaptureAt(0, 12000, 4000, AddCallback("CaptureAt 12000"));

  // Packets should complete in strict order, with DiscardAllPackets' completion afterward.
  audio_capturer_->DiscardAllPackets(AddCallback("DiscardAllPackets"));
  ExpectCallbacks();
}

TEST_F(AudioCapturerTestOldAPI, DiscardAllWithNoVmoShouldDisconnect) {
  SetFormat();

  audio_capturer_->DiscardAllPackets(AddUnexpectedCallback("DiscardAllPackets"));
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPackets should fail, if async capture is active
TEST_F(AudioCapturerTestOldAPI, DiscardAllDuringAsyncCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->DiscardAllPackets(AddUnexpectedCallback("DiscardAllPackets"));
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPackets should fail, if async capture is in the process of stopping
TEST_F(AudioCapturerTestOldAPI, DISABLED_DiscardAllAsyncCaptureStoppingShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->StopAsyncCaptureNoReply();
  audio_capturer_->DiscardAllPackets(AddUnexpectedCallback("DiscardAllPackets"));
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPackets should succeed, if async capture is completely stopped
TEST_F(AudioCapturerTestOldAPI, DiscardAllAfterAsyncCapture) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();

  audio_capturer_->DiscardAllPackets(AddCallback("DiscardAllPackets"));
  ExpectCallbacks();
}

// TODO(mpuryear): DiscardAllPacketsNoReply() post-stop
TEST_F(AudioCapturerTestOldAPI, DiscardAllNoReplyWithNoVmoShouldDisconnect) {
  SetFormat();

  audio_capturer_->DiscardAllPacketsNoReply();
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPacketsNoReply should fail, if async capture is active
TEST_F(AudioCapturerTestOldAPI, DiscardAllNoReplyDuringAsyncCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->DiscardAllPacketsNoReply();
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPacketsNoReply should fail, if async capture is in the process of stopping
TEST_F(AudioCapturerTestOldAPI, DISABLED_DiscardAllNoReplyAsyncCaptureStoppingShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->StopAsyncCaptureNoReply();
  audio_capturer_->DiscardAllPacketsNoReply();
  ExpectDisconnect(audio_capturer_);
}

// DiscardAllPacketsNoReply should succeed, if async capture is completely stopped
TEST_F(AudioCapturerTestOldAPI, DiscardAllNoReplyAfterAsyncCapture) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();

  audio_capturer_->DiscardAllPacketsNoReply();
  RunLoopUntilIdle();
}

// Stopping an async capturer should succeed when all packets are in flight.
TEST_F(AudioCapturerTestOldAPI, StopAsyncWithAllPacketsInFlight) {
  const auto kFramesPerPacket = 1600;
  const auto kPackets = 10;
  const auto kFramesPerSecond = kFramesPerPacket * kPackets;  // below we assume 1 packet == 100ms
  SetFormat(kFramesPerSecond);
  SetUpPayloadBuffer(kFramesPerSecond);

  // Don't recycle any packets.
  int count = 0;
  audio_capturer_.events().OnPacketProduced =
      AddCallback("OnPacketProduced", [&count](auto packet) { count++; });

  // Wait until all packets are in flight.
  audio_capturer_->StartAsyncCapture(kFramesPerPacket);
  RunLoopUntil([&count]() { return count == kPackets; });

  // Wait for over one mix period (100ms). This is not necessary for the test, however it
  // increases the chance of a mix period running before our StopAsyncCapture call, which
  // increases our chance of finding bugs (e.g. https://fxbug.dev/72776).
  usleep(150 * 1000);

  audio_capturer_->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();
}

// AudioCapturer methods
//

// TODO(mpuryear): test SetPcmStreamType(AudioStreamType stream_type);
// Also when already set, when packets submitted, when started
// Also negative testing: malformed type

// TODO(mpuryear): test CaptureAt(uint32 id, uint32 offset, uint32 frames)
//                        -> (StreamPacket captured_packet);
// Also when in async capture, before format set, before packets submitted
// Also negative testing: bad id, bad offset, 0/tiny/huge num frames

// TODO(mpuryear): test StartAsyncCapture(uint32 frames_per_packet);
// Also when already started, before format set, before packets submitted
// Also negative testing: 0/tiny/huge num frames (bigger than packet)

TEST_F(AudioCapturerTestOldAPI, StopWhenStoppedShouldDisconnect) {
  audio_capturer_->StopAsyncCapture(AddUnexpectedCallback("StopAsyncCapture"));
  ExpectDisconnect(audio_capturer_);
}
// Also test before format set, before packets submitted

TEST_F(AudioCapturerTestOldAPI, StopNoReplyWhenStoppedShouldDisconnect) {
  audio_capturer_->StopAsyncCaptureNoReply();
  ExpectDisconnect(audio_capturer_);
}
// Also before format set, before packets submitted

// Test creation and interface independence of GainControl.
// In a number of tests below, we run the message loop to give the AudioCapturer
// or GainControl binding a chance to disconnect, if an error occurred.
TEST_F(AudioCapturerTestOldAPI, BindGainControl) {
  // Validate AudioCapturers can create GainControl interfaces.
  audio_capturer_->BindGainControl(gain_control_.NewRequest());
  AddErrorHandler(gain_control_, "AudioCapturer::GainControl");

  fuchsia::media::AudioCapturerPtr audio_capturer_2;
  audio_core_->CreateAudioCapturer(true, audio_capturer_2.NewRequest());
  AddErrorHandler(audio_capturer_2, "AudioCapturer2");

  fuchsia::media::audio::GainControlPtr gain_control_2;
  audio_capturer_2->BindGainControl(gain_control_2.NewRequest());
  AddErrorHandler(gain_control_2, "AudioCapturer::GainControl2");

  // What happens to a child gain_control, when a capturer is unbound?
  audio_capturer_.Unbind();

  // What happens to a parent capturer, when a gain_control is unbound?
  gain_control_2.Unbind();

  // Give audio_capturer_ a chance to disconnect gain_control_
  ExpectDisconnect(gain_control_);

  // Give time for other Disconnects to occur, if they must.
  audio_capturer_2->GetStreamType(AddCallback("GetStreamType"));
  ExpectCallbacks();
}

// Setting a payload buffer should fail, if format has not yet been set (even if it was retrieved).
TEST_F(AudioCapturerTestOldAPI, AddPayloadBufferBeforeSetFormatShouldDisconnect) {
  // Give time for Disconnect to occur, if it must.
  audio_capturer_->GetStreamType(AddCallback("GetStreamType"));
  ExpectCallbacks();

  // Calling this before SetPcmStreamType should fail
  SetUpPayloadBuffer();
  ExpectDisconnect(audio_capturer_);
}

// TODO(mpuryear): test GetStreamType() -> (StreamType stream_type);
// Also negative testing: before format set

//
// Validation of AudioCapturer reference clock methods

// In test cases below of SetReferenceClock calls that should lead to disconnect, we wait for more
// than one mix job, then call another capturer method (SetUsage) to give the capturer time to drop.

// Accept the default clock that is returned if we set no clock.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockDefault) {
  zx::clock ref_clock = GetAndValidateReferenceClock();

  clock::testing::VerifyReadOnlyRights(ref_clock);
  clock::testing::VerifyIsSystemMonotonic(ref_clock);

  clock::testing::VerifyAdvances(ref_clock);
  clock::testing::VerifyCannotBeRateAdjusted(ref_clock);
}

// Set a null clock; this represents selecting the AudioCore-generated clock.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockFlexible) {
  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));
  zx::clock provided_clock = GetAndValidateReferenceClock();

  clock::testing::VerifyReadOnlyRights(provided_clock);
  clock::testing::VerifyIsSystemMonotonic(provided_clock);

  clock::testing::VerifyAdvances(provided_clock);
  clock::testing::VerifyCannotBeRateAdjusted(provided_clock);
}

TEST_F(AudioCapturerClockTestOldAPI, SetRefClockCustom) {
  // Set a recognizable custom reference clock -- should be what we receive from GetReferenceClock.
  zx::clock dupe_clock, retained_clock, orig_clock = clock::AdjustableCloneOfMonotonic();
  zx::clock::update_args args;
  args.reset().set_rate_adjust(-100);
  ASSERT_EQ(orig_clock.update(args), ZX_OK) << "clock.update with rate_adjust failed";

  ASSERT_EQ(orig_clock.duplicate(kClockRights, &dupe_clock), ZX_OK);
  ASSERT_EQ(orig_clock.duplicate(kClockRights, &retained_clock), ZX_OK);

  audio_capturer_->SetReferenceClock(std::move(dupe_clock));
  zx::clock received_clock = GetAndValidateReferenceClock();

  clock::testing::VerifyReadOnlyRights(received_clock);
  clock::testing::VerifyIsNotSystemMonotonic(received_clock);

  clock::testing::VerifyAdvances(received_clock);
  clock::testing::VerifyCannotBeRateAdjusted(received_clock);

  // We can still rate-adjust our custom clock.
  clock::testing::VerifyCanBeRateAdjusted(orig_clock);
  clock::testing::VerifyAdvances(orig_clock);
}

// inadequate ZX_RIGHTS -- no DUPLICATE should cause GetReferenceClock to fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockNoDuplicateShouldDisconnect) {
  zx::clock dupe_clock, orig_clock = clock::CloneOfMonotonic();
  ASSERT_EQ(orig_clock.duplicate(kClockRights & ~ZX_RIGHT_DUPLICATE, &dupe_clock), ZX_OK);

  audio_capturer_->SetReferenceClock(std::move(dupe_clock));
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// inadequate ZX_RIGHTS -- no READ should cause GetReferenceClock to fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockNoReadShouldDisconnect) {
  zx::clock dupe_clock, orig_clock = clock::CloneOfMonotonic();
  ASSERT_EQ(orig_clock.duplicate(kClockRights & ~ZX_RIGHT_READ, &dupe_clock), ZX_OK);

  audio_capturer_->SetReferenceClock(std::move(dupe_clock));
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Regardless of the type of clock, calling SetReferenceClock a second time should fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockCustomThenFlexibleShouldDisconnect) {
  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());

  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Regardless of the type of clock, calling SetReferenceClock a second time should fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockSecondCustomShouldDisconnect) {
  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Regardless of the type of clock, calling SetReferenceClock a second time should fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockSecondFlexibleShouldDisconnect) {
  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));

  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Regardless of the type of clock, calling SetReferenceClock a second time should fail.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockFlexibleThenCustomShouldDisconnect) {
  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// If client-submitted clock has ZX_RIGHT_WRITE, this should be removed upon GetReferenceClock.
TEST_F(AudioCapturerClockTestOldAPI, GetRefClockRemovesWriteRight) {
  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());

  zx::clock received_clock = GetAndValidateReferenceClock();
  clock::testing::VerifyReadOnlyRights(received_clock);
}

// You can set the reference clock at any time before the payload buffer is added.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockBeforeBuffer) {
  SetFormat();

  audio_capturer_->SetReferenceClock(zx::clock(ZX_HANDLE_INVALID));
  GetAndValidateReferenceClock();
}

// Setting the reference clock should fail, once payload buffer has been added.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockAfterBufferShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Setting the reference clock should fail, any time after payload buffer has been added.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockDuringCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_->CaptureAt(0, 0, 8000, [](fuchsia::media::StreamPacket) {
    // Don't fail if this completes before SetReferenceClock can run.
    GTEST_SKIP() << "CaptureAt completed before SetReferenceClock could cancel it";
  });

  audio_capturer_->SetReferenceClock(clock::CloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Setting the reference clock should fail, even after all active capture packets have returned.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockAfterCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_->CaptureAt(0, 0, 8000, AddCallback("CaptureAt"));
  ExpectCallbacks();

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Setting the reference clock should fail, any time after capture has started (even if cancelled).
//
// TODO(https://fxbug.dev/57079): deflake and re-enable.
TEST_F(AudioCapturerClockTestOldAPI, DISABLED_SetRefClockCaptureCancelledShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_->CaptureAt(0, 0, 8000, [](fuchsia::media::StreamPacket) {});
  audio_capturer_->DiscardAllPackets(AddCallback("DiscardAllPackets"));
  ExpectCallbacks();

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Setting the reference clock should fail, if at least one capture packet is active.
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockDuringAsyncCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->SetReferenceClock(clock::CloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// Setting the reference clock should fail, any time after capture has started (even if stopped).
TEST_F(AudioCapturerClockTestOldAPI, SetRefClockAfterAsyncCaptureShouldDisconnect) {
  SetFormat();
  SetUpPayloadBuffer();

  audio_capturer_.events().OnPacketProduced = AddCallback("OnPacketProduced");
  audio_capturer_->StartAsyncCapture(1600);
  ExpectCallbacks();

  audio_capturer_->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();

  audio_capturer_->SetReferenceClock(clock::AdjustableCloneOfMonotonic());
  WaitBeforeClockRelatedDisconnect();
  ExpectDisconnect(audio_capturer_);
}

// A simple fixture that uses the new HermeticAudioTest Create methods instead of raw
// FIDL InterfacePtrs (cf. AudioCapturerTestOldAPI).
class AudioCapturerTest : public HermeticAudioTest {};

TEST_F(AudioCapturerTest, NoCrashOnChannelCloseAfterStopAsync) {
  auto format = Format::Create<ASF::SIGNED_16>(1, 48000).take_value();
  CreateInput({{0xff, 0x00}}, format, 48000);
  auto capturer = CreateAudioCapturer(format, 48000,
                                      fuchsia::media::AudioCapturerConfiguration::WithInput(
                                          fuchsia::media::InputAudioCapturerConfiguration()));

  capturer->fidl()->StartAsyncCapture(480);
  RunLoopUntilIdle();
  capturer->fidl()->StopAsyncCaptureNoReply();
  Unbind(capturer);
  RunLoopUntilIdle();
}

// Test capturing when there's no input device. We expect this to work with all the audio captured
// being completely silent.
TEST_F(AudioCapturerTest, CaptureAsyncNoDevice) {
  auto format = Format::Create<ASF::SIGNED_16>(1, 16000).take_value();
  auto capturer = CreateAudioCapturer(format, 16000,
                                      fuchsia::media::AudioCapturerConfiguration::WithInput(
                                          fuchsia::media::InputAudioCapturerConfiguration()));

  // Initialize capture buffers to non-silent values.
  capturer->payload().Memset<ASF::SIGNED_16>(0xff);

  // Capture a packet and retain it.
  std::optional<fuchsia::media::StreamPacket> capture_packet;
  capturer->fidl().events().OnPacketProduced = AddCallback(
      "OnPacketProduced", [&capture_packet](auto packet) { capture_packet = std::move(packet); });
  capturer->fidl()->StartAsyncCapture(1600);
  ExpectCallbacks();

  capturer->fidl()->StopAsyncCapture(AddCallback("StopAsyncCapture"));
  ExpectCallbacks();

  // Expect the packet to be silent. Since we initialized the buffer to non-silence we know that
  // this silence was populated by audio_core.
  EXPECT_TRUE(capture_packet);
  EXPECT_EQ(capture_packet->payload_buffer_id, 0u);
  EXPECT_NE(capture_packet->payload_size, 0u);
  auto buffer = capturer->payload().SnapshotSlice<ASF::SIGNED_16>(capture_packet->payload_offset,
                                                                  capture_packet->payload_size);
  Unbind(capturer);
  ASSERT_EQ(1, buffer.format().channels());
  for (int64_t frame = 0; frame < buffer.NumFrames(); ++frame) {
    ASSERT_EQ(buffer.SampleAt(frame, 0), 0) << "at frame " << frame;
  }
}

}  // namespace media::audio::test
