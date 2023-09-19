// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/audio_core/v1/audio_capturer.h"

#include <fuchsia/media/cpp/fidl.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>

#include <gtest/gtest.h>

#include "src/media/audio/audio_core/shared/audio_admin.h"
#include "src/media/audio/audio_core/shared/stream_volume_manager.h"
#include "src/media/audio/audio_core/v1/audio_device_manager.h"
#include "src/media/audio/audio_core/v1/audio_driver.h"
#include "src/media/audio/audio_core/v1/audio_input.h"
#include "src/media/audio/audio_core/v1/testing/fake_audio_driver.h"
#include "src/media/audio/audio_core/v1/testing/threading_model_fixture.h"
#include "src/media/audio/lib/clock/testing/clock_test.h"

namespace media::audio {
namespace {

constexpr uint32_t kAudioCapturerUnittestFrameRate = 48000;
constexpr size_t kAudioCapturerUnittestVmarSize = 16ull * 1024;

class AudioCapturerTest : public testing::ThreadingModelFixture {
 public:
  AudioCapturerTest() : testing::ThreadingModelFixture(WithRealClocks) {
    FX_CHECK(vmo_mapper_.CreateAndMap(kAudioCapturerUnittestVmarSize,
                                      /*flags=*/0, nullptr, &vmo_) == ZX_OK);
  }

 protected:
  void SetUp() override {
    testing::ThreadingModelFixture::SetUp();

    auto format = Format::Create(stream_type_).take_value();
    fuchsia::media::InputAudioCapturerConfiguration input_configuration;
    input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
    auto capturer = AudioCapturer::Create(
        fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)),
        {format}, fidl_capturer_.NewRequest(), &context());
    capturer_ = capturer.get();
    EXPECT_NE(capturer_, nullptr);

    fidl_capturer_.set_error_handler(
        [](auto status) { EXPECT_TRUE(status == ZX_OK) << "Capturer disconnected: " << status; });

    context().route_graph().AddCapturer(std::move(capturer));
  }

  void TearDown() override {
    // Dropping the channel queues up a reference to the Capturer through its error handler, which
    // will not work since the rest of this class is destructed before the loop and its
    // queued functions are. Here, we ensure the error handler runs before this class' destructors
    // run.
    { auto r = std::move(fidl_capturer_); }
    RunLoopUntilIdle();

    testing::ThreadingModelFixture::TearDown();
  }

  zx::clock GetReferenceClock() {
    zx::clock fidl_clock;
    fidl_capturer_->GetReferenceClock(
        [&fidl_clock](zx::clock ref_clock) { fidl_clock = std::move(ref_clock); });
    RunLoopUntilIdle();

    EXPECT_TRUE(fidl_clock.is_valid());
    return fidl_clock;
  }

  AudioCapturer* capturer_;
  fuchsia::media::AudioCapturerPtr fidl_capturer_;

  fzl::VmoMapper vmo_mapper_;
  zx::vmo vmo_;

  fuchsia::media::AudioStreamType stream_type_ = {
      .sample_format = fuchsia::media::AudioSampleFormat::FLOAT,
      .channels = 1,
      .frames_per_second = kAudioCapturerUnittestFrameRate,
  };
};

TEST_F(AudioCapturerTest, CanShutdownWithUnusedBuffer) {
  zx::vmo duplicate;
  ASSERT_EQ(
      vmo_.duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_WRITE | ZX_RIGHT_READ | ZX_RIGHT_MAP, &duplicate),
      ZX_OK);
  fidl_capturer_->AddPayloadBuffer(0, std::move(duplicate));
  RunLoopUntilIdle();
}

TEST_F(AudioCapturerTest, RegistersWithRouteGraphIfHasUsageStreamTypeAndBuffersDriver) {
  EXPECT_EQ(context().link_matrix().SourceLinkCount(*capturer_), 0u);

  zx::vmo duplicate;
  ASSERT_EQ(
      vmo_.duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_WRITE | ZX_RIGHT_READ | ZX_RIGHT_MAP, &duplicate),
      ZX_OK);

  zx::channel c1, c2;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &c1, &c2));
  fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> stream_config = {};
  stream_config.set_channel(zx::channel());
  auto input = AudioInput::Create(
      "", context().process_config().device_config(), std::move(stream_config), &threading_model(),
      &context().device_manager(), &context().link_matrix(), context().clock_factory());
  auto fake_driver =
      testing::FakeAudioDriver(std::move(c1), threading_model().FidlDomain().dispatcher());

  auto vmo = fake_driver.CreateRingBuffer(zx_system_get_page_size());

  input->driver()->Init(std::move(c2));
  fake_driver.Start();
  input->driver()->GetDriverInfo();
  RunLoopUntilIdle();

  input->driver()->Start();

  context().route_graph().AddDeviceToRoutes(input.get());
  RunLoopUntilIdle();

  fidl_capturer_->AddPayloadBuffer(0, std::move(duplicate));

  RunLoopUntilIdle();
  EXPECT_EQ(context().link_matrix().SourceLinkCount(*capturer_), 1u);
}

TEST_F(AudioCapturerTest, CanReleasePacketWithoutDroppingConnection) {
  bool channel_dropped = false;
  fidl_capturer_.set_error_handler([&channel_dropped](auto _) { channel_dropped = true; });
  fidl_capturer_->ReleasePacket(fuchsia::media::StreamPacket{});
  RunLoopUntilIdle();

  // The RouteGraph should still own our capturer.
  EXPECT_FALSE(channel_dropped);
}

TEST_F(AudioCapturerTest, ReferenceClockIsAdvancing) {
  auto fidl_clock = GetReferenceClock();

  clock::testing::VerifyAdvances(fidl_clock);
  clock::testing::VerifyAdvances(*capturer_->reference_clock());
}

TEST_F(AudioCapturerTest, DefaultReferenceClockIsReadOnly) {
  auto fidl_clock = GetReferenceClock();

  clock::testing::VerifyCannotBeRateAdjusted(fidl_clock);

  // Within audio_core, the default clock is rate-adjustable.
  clock::testing::VerifyCanBeRateAdjusted(*capturer_->reference_clock());
}

TEST_F(AudioCapturerTest, DefaultClockIsClockMonotonic) {
  auto fidl_clock = GetReferenceClock();

  clock::testing::VerifyIsSystemMonotonic(fidl_clock);
  clock::testing::VerifyIsSystemMonotonic(*capturer_->reference_clock());
}

class AudioCapturerBadFormatTest : public testing::ThreadingModelFixture {
 protected:
  void TearDown() override {
    // Dropping the channel queues a reference to the Capturer through its error handler, which
    // won't work since the rest of the class is destructed before the loop and its queued
    // functions. Here, we ensure the error handler runs before this class' destructors run.
    { auto r = std::move(fidl_capturer_); }
    RunLoopUntilIdle();

    testing::ThreadingModelFixture::TearDown();
  }

  AudioCapturer* capturer_;
  fuchsia::media::AudioCapturerPtr fidl_capturer_;

  fuchsia::media::AudioStreamType stream_type_ = {
      .sample_format = fuchsia::media::AudioSampleFormat::FLOAT,
      .channels = 1,
      .frames_per_second = kAudioCapturerUnittestFrameRate,
  };
};

// If given a malformed format (channels == 0), an AudioCapturer should disconnect.
TEST_F(AudioCapturerBadFormatTest, ChannelsTooLowShouldDisconnect) {
  fuchsia::media::InputAudioCapturerConfiguration input_configuration;
  input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
  std::optional<Format> format;
  auto capturer = AudioCapturer::Create(
      fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)), format,
      fidl_capturer_.NewRequest(), &context());
  context().route_graph().AddCapturer(std::move(capturer));

  stream_type_.channels = fuchsia::media::MIN_PCM_CHANNEL_COUNT - 1;
  std::optional<zx_status_t> received_status;
  fidl_capturer_.set_error_handler([&received_status](auto status) { received_status = status; });

  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_TRUE(received_status.has_value());
  EXPECT_EQ(*received_status, ZX_ERR_PEER_CLOSED);
}

// AudioCapturers are limited to a maximum channel count of 4.
TEST_F(AudioCapturerBadFormatTest, ChannelsTooHighShouldDisconnect) {
  fuchsia::media::InputAudioCapturerConfiguration input_configuration;
  input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
  std::optional<Format> format;
  auto capturer = AudioCapturer::Create(
      fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)), format,
      fidl_capturer_.NewRequest(), &context());
  context().route_graph().AddCapturer(std::move(capturer));

  stream_type_.channels = 5;  // AudioCapturers limit this to 4 instead of MAX_PCM_CHANNEL_COUNT
  std::optional<zx_status_t> received_status;
  fidl_capturer_.set_error_handler([&received_status](auto status) { received_status = status; });

  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_TRUE(received_status.has_value());
  EXPECT_EQ(*received_status, ZX_ERR_PEER_CLOSED);
}

// AudioCapturers are limited to a minimum frame rate of 1000 Hz.
TEST_F(AudioCapturerBadFormatTest, FrameRateTooLowShouldDisconnect) {
  fuchsia::media::InputAudioCapturerConfiguration input_configuration;
  input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
  std::optional<Format> format;
  auto capturer = AudioCapturer::Create(
      fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)), format,
      fidl_capturer_.NewRequest(), &context());
  context().route_graph().AddCapturer(std::move(capturer));

  stream_type_.frames_per_second = fuchsia::media::MIN_PCM_FRAMES_PER_SECOND - 1;
  std::optional<zx_status_t> received_status;
  fidl_capturer_.set_error_handler([&received_status](auto status) { received_status = status; });

  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_TRUE(received_status.has_value());
  EXPECT_EQ(*received_status, ZX_ERR_PEER_CLOSED);
}

// AudioCapturers are limited to a maximum frame rate of 192000 Hz.
TEST_F(AudioCapturerBadFormatTest, FrameRateTooHighShouldDisconnect) {
  fuchsia::media::InputAudioCapturerConfiguration input_configuration;
  input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
  std::optional<Format> format;
  auto capturer = AudioCapturer::Create(
      fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)), format,
      fidl_capturer_.NewRequest(), &context());
  context().route_graph().AddCapturer(std::move(capturer));

  stream_type_.frames_per_second = fuchsia::media::MAX_PCM_FRAMES_PER_SECOND + 1;
  std::optional<zx_status_t> received_status;
  fidl_capturer_.set_error_handler([&received_status](auto status) { received_status = status; });

  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_TRUE(received_status.has_value());
  EXPECT_EQ(*received_status, ZX_ERR_PEER_CLOSED);
}

// AudioCapturers cannot call SetPcmStreamType whiole a payload buffer has been added.
TEST_F(AudioCapturerBadFormatTest, SetFormatAfterAddPayloadBufferShouldDisconnect) {
  fuchsia::media::InputAudioCapturerConfiguration input_configuration;
  input_configuration.set_usage(fuchsia::media::AudioCaptureUsage::BACKGROUND);
  std::optional<Format> format;
  auto capturer = AudioCapturer::Create(
      fuchsia::media::AudioCapturerConfiguration::WithInput(std::move(input_configuration)), format,
      fidl_capturer_.NewRequest(), &context());
  std::optional<zx_status_t> received_status;
  fidl_capturer_.set_error_handler([&received_status](auto status) { received_status = status; });

  context().route_graph().AddCapturer(std::move(capturer));
  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_FALSE(received_status.has_value());

  fzl::VmoMapper vmo_mapper;
  zx::vmo vmo, duplicate;
  ASSERT_EQ(vmo_mapper.CreateAndMap(kAudioCapturerUnittestVmarSize,
                                    /*flags=*/0, nullptr, &vmo),
            ZX_OK);
  ASSERT_EQ(
      vmo.duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_WRITE | ZX_RIGHT_READ | ZX_RIGHT_MAP, &duplicate),
      ZX_OK);
  fidl_capturer_->AddPayloadBuffer(0, std::move(duplicate));
  RunLoopUntilIdle();
  ASSERT_FALSE(received_status.has_value());

  fidl_capturer_->SetPcmStreamType(stream_type_);
  RunLoopUntilIdle();
  ASSERT_TRUE(received_status.has_value());
  EXPECT_EQ(*received_status, ZX_ERR_PEER_CLOSED);
}

}  // namespace
}  // namespace media::audio
