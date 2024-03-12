// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

using ::testing::Optional;

class DeviceTest : public DeviceTestBase {
 protected:
  static inline const fuchsia_hardware_audio::Format kDefaultRingBufferFormat{{
      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = static_cast<uint8_t>(16),
          .frame_rate = 48000,
      }},
  }};
  // Accessor for a Device private member.
  static const std::optional<fuchsia_hardware_audio::DelayInfo>& DeviceDelayInfo(
      const std::shared_ptr<Device>& device) {
    return device->delay_info_;
  }
};
class CodecTest : public DeviceTestBase {};
class StreamConfigTest : public DeviceTest {};

// Validate that a fake codec with default values is initialized successfully.
TEST_F(CodecTest, Initialization) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  EXPECT_EQ(device->device_type(), fuchsia_audio_device::DeviceType::kCodec);
  EXPECT_TRUE(device->ring_buffer_format_sets().empty());
  EXPECT_EQ(device->dai_format_sets(), FakeCodec::kDefaultDaiFormatSets);

  ASSERT_TRUE(device->info().has_value());
  EXPECT_TRUE(device->info()->is_input().has_value());
  EXPECT_FALSE(*device->info()->is_input());

  fake_device_presence_watcher_.reset();
}

TEST_F(CodecTest, InitializationNoDirection) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  ASSERT_TRUE(device->info().has_value());
  EXPECT_FALSE(device->info()->is_input().has_value());

  fake_device_presence_watcher_.reset();
}

// Validate that a fake codec with default values is initialized successfully.
TEST_F(CodecTest, DeviceInfoValues) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);

  ASSERT_TRUE(device->info().has_value());
  auto info = *device->info();
  EXPECT_TRUE(info.token_id().has_value());
  EXPECT_TRUE(info.device_type().has_value());
  EXPECT_TRUE(info.device_name().has_value());
  EXPECT_TRUE(info.manufacturer().has_value());
  EXPECT_TRUE(info.product().has_value());
  EXPECT_TRUE(info.unique_instance_id().has_value());
  EXPECT_TRUE(info.is_input().has_value());
  EXPECT_FALSE(info.ring_buffer_format_sets().has_value());
  EXPECT_TRUE(info.dai_format_sets().has_value());
  EXPECT_FALSE(info.gain_caps().has_value());
  EXPECT_TRUE(info.plug_detect_caps().has_value());
  EXPECT_FALSE(info.clock_domain().has_value());
  EXPECT_FALSE(info.signal_processing_elements().has_value());
  EXPECT_FALSE(info.signal_processing_topologies().has_value());

  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kCodec);
  EXPECT_TRUE(!info.device_name()->empty());
  EXPECT_EQ(*info.manufacturer(), FakeCodec::kDefaultManufacturer);
  EXPECT_EQ(*info.product(), FakeCodec::kDefaultProduct);
  // EXPECT_EQ(*info.unique_instance_id(), FakeCodec::kDefaultUniqueInstanceId);
  EXPECT_TRUE(!info.is_input().value());
  EXPECT_EQ(*info.dai_format_sets(), FakeCodec::kDefaultDaiFormatSets);
  EXPECT_EQ(*info.plug_detect_caps(), FakeCodec::kDefaultPlugCaps);

  fake_device_presence_watcher_.reset();
}

// Validate that a driver's dropping the Codec causes a DeviceIsRemoved notification.
TEST_F(CodecTest, Disconnect) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_codec->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(CodecTest, EmptyHealthResponse) {
  auto fake_codec = MakeFakeCodecOutput();
  fake_codec->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(CodecTest, DeviceInfo) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  auto info = GetDeviceInfo(device);

  EXPECT_TRUE(info.token_id());
  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kCodec);
  EXPECT_TRUE(info.device_name());
  // manufacturer is optional, but it can't be an empty string
  EXPECT_TRUE(!info.manufacturer().has_value() || !info.manufacturer()->empty());
  // product is optional, but it can't be an empty string
  EXPECT_TRUE(!info.product().has_value() || !info.product()->empty());
  // unique_instance_id is optional
  EXPECT_FALSE(info.gain_caps());
  EXPECT_TRUE(info.plug_detect_caps());
  EXPECT_FALSE(info.clock_domain());
}

TEST_F(CodecTest, Observer) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(CodecTest, Control) {
  auto fake_codec = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

TEST_F(CodecTest, GetDaiFormats) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_FALSE(IsControlled(device));

  ASSERT_TRUE(AddObserver(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback,
       &dai_formats](const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  EXPECT_EQ(ValidateDaiFormatSets(dai_formats), ZX_OK);
}

// SetDaiFormat is tested (here and in CodecWarningTest) against all states and error cases:
// States: First set, format-change, no-change. ControlNotify::DaiFormatChanged received.
// SetDaiFormat stops Codec; ControlNotify::CodecStopped received if was started.
// Errors: StreamConfig type; Device has error, not controlled; invalid format; unsupported format.
//
// SetDaiFormat - use the first supported format
TEST_F(CodecTest, SetDaiFormat) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback,
       &dai_formats](const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  ASSERT_FALSE(notify_->dai_format());

  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  // check that notify received dai_format and codec_format_info
  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->dai_format());
  EXPECT_EQ(ValidateDaiFormat(*notify_->dai_format()), ZX_OK);
  EXPECT_TRUE(notify_->codec_format_info());
  EXPECT_EQ(ValidateCodecFormatInfo(*notify_->codec_format_info()), ZX_OK);
}

// Start and Stop
TEST_F(CodecTest, InitialStop) {
  auto fake_codec = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback,
       &dai_formats](const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_is_stopped());

  // This should do nothing since we are already stopped.
  EXPECT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_stopped());
  // This demonstrates that there was no change as a result of the Stop call.
  EXPECT_EQ(notify_->codec_stop_time()->get(), zx::time::infinite_past().get());
}

TEST_F(CodecTest, Start) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_is_stopped());
  auto time_before_start = zx::clock::get_monotonic();

  EXPECT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_started());
  EXPECT_GT(notify_->codec_start_time()->get(), time_before_start.get());
}

TEST_F(CodecTest, SetDaiFormatChange) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->codec_is_started());
  auto dai_format2 = SecondDaiFormatFromDaiSupportedFormats(dai_formats);
  auto time_before_format_change = zx::clock::get_monotonic();

  // Change the DaiFormat
  EXPECT_TRUE(device->CodecSetDaiFormat(dai_format2));

  // Expect codec to be stopped, and format to be changed.
  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_stopped());
  EXPECT_GT(notify_->codec_stop_time()->get(), time_before_format_change.get());
  EXPECT_TRUE(notify_->dai_format());
  EXPECT_EQ(*notify_->dai_format(), dai_format2);
  EXPECT_TRUE(notify_->codec_format_info());
}

TEST_F(CodecTest, SetDaiFormatNoChange) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  auto safe_format = SafeDaiFormatFromDaiSupportedFormats(dai_formats);
  ASSERT_TRUE(device->CodecSetDaiFormat(safe_format));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->codec_is_started());
  auto time_after_started = zx::clock::get_monotonic();
  // Clear out our notify's DaiFormat so we can detect a new notification of the same DaiFormat.
  notify_->clear_dai_format();

  EXPECT_TRUE(device->CodecSetDaiFormat(safe_format));

  RunLoopUntilIdle();
  // We do not expect this to reset our Start state.
  EXPECT_TRUE(notify_->codec_is_started());
  EXPECT_LT(notify_->codec_start_time()->get(), time_after_started.get());
  // We do not expect to be notified of a format change -- even a "change" to the same format.
  EXPECT_FALSE(notify_->dai_format());
}

TEST_F(CodecTest, StartStop) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  auto time_before_stop = zx::clock::get_monotonic();
  ASSERT_TRUE(notify_->codec_is_started());
  ASSERT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_stopped());
  EXPECT_GT(notify_->codec_stop_time()->get(), time_before_stop.get());
}

// Start when already started: no notification; old start_time.
TEST_F(CodecTest, StartStart) {
  auto fake_codec = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->codec_is_started());
  auto previous_start_time = *notify_->codec_start_time();
  ASSERT_TRUE(device->CodecStart());

  // Should not get a new notification here.
  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_started());
  EXPECT_EQ(notify_->codec_start_time()->get(), previous_start_time.get());
}

// Stop when already stopped: no notification; old stop_time.
TEST_F(CodecTest, StopStop) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_is_stopped());
  // First Start the Codec so we can explicitly transition into the Stop state.
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->codec_is_started());
  ASSERT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->codec_is_stopped());
  auto previous_stop_time = *notify_->codec_stop_time();

  // Since we are already stopped, this should have no effect.
  EXPECT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_stopped());
  // This demonstrates that there was no change as a result of the Stop call.
  EXPECT_EQ(notify_->codec_stop_time()->get(), previous_stop_time.get());
}

// Reset stops the Codec and resets DaiFormat, Elements and Topology
TEST_F(CodecTest, Reset) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify_->dai_format());
  ASSERT_TRUE(notify_->codec_format_info());
  ASSERT_TRUE(notify_->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
  // When implemented in FakeCodec, set a signalprocessing Topology, and change a signalprocessing
  // Element, and observe that both of these changes are reverted by the call to Reset.

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_started());
  // Verify that the signalprocessing Topology has in fact changed.
  // Verify that the signalprocessing Element has in fact changed.
  auto time_before_reset = zx::clock::get_monotonic();

  EXPECT_TRUE(device->CodecReset());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify_->codec_is_stopped());
  // We were notified that the Codec stopped.
  EXPECT_GT(notify_->codec_stop_time()->get(), time_before_reset.get());
  // We were notified that the Codec reset its DaiFormat (none is now set).
  EXPECT_FALSE(notify_->dai_format());
  EXPECT_FALSE(notify_->codec_format_info());
  // Observe the signalprocessing Elements - were they reset?
  // Observe the signalprocessing Topology - was it reset?
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(CodecTest, InitialPlugState) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), zx::time::infinite_past().get());
}

// This tests the driver's ability to originate plug changes, such as from jack detection. It also
// validates that plug notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(CodecTest, DynamicPlugUpdate) {
  auto fake_codec = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), zx::time::infinite_past().get());
  notify_->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_codec->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), unplug_time.get());
}

// Validate that a fake stream_config with default values is initialized successfully.
TEST_F(StreamConfigTest, Initialization) {
  auto device = InitializeDeviceForFakeStreamConfig(MakeFakeStreamConfigOutput());
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_device_presence_watcher_.reset();
}

// Validate that a driver's dropping the StreamConfig causes a DeviceIsRemoved notification.
TEST_F(StreamConfigTest, Disconnect) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_stream_config->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(StreamConfigTest, EmptyHealthResponse) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(DeviceTest, DistinctTokenIds) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  // Set up a second, entirely distinct fake device.
  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));

  auto fake_driver2 = std::make_unique<FakeStreamConfig>(std::move(server_end),
                                                         std::move(client_end), dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeStreamConfig(fake_driver2);
  EXPECT_TRUE(InInitializedState(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 2u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DefaultClock) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_FALSE(device_clock(device)->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, ClockInOtherDomain) {
  const uint32_t kNonMonotonicClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic + 1;
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_clock_domain(kNonMonotonicClockDomain);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock(device)->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DeviceInfo) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  auto info = GetDeviceInfo(device);

  EXPECT_TRUE(info.token_id());
  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kOutput);
  EXPECT_TRUE(info.device_name());
  // manufacturer is optional, but it can't be an empty string
  EXPECT_TRUE(!info.manufacturer().has_value() || !info.manufacturer()->empty());
  // product is optional, but it can't be an empty string
  EXPECT_TRUE(!info.product().has_value() || !info.product()->empty());
  // unique_instance_id is optional
  EXPECT_TRUE(info.gain_caps());
  EXPECT_TRUE(info.plug_detect_caps());
  EXPECT_TRUE(info.clock_domain());
  EXPECT_EQ(*info.clock_domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
}

TEST_F(StreamConfigTest, SupportedDriverFormatForClientFormat) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_frame_rates(0, {48000, 48001});
  fake_stream_config->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_stream_config->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  auto valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt16, 2, 48001);
  EXPECT_EQ(valid_bits, 15u);

  valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt32, 2, 48000);
  EXPECT_EQ(valid_bits, 20u);
}

TEST_F(StreamConfigTest, Observer) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(StreamConfigTest, Control) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests the driver's ability to inform its ObserverNotify of initial gain state.
TEST_F(StreamConfigTest, InitialGainState) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  ASSERT_TRUE(notify_->gain_state()->gain_db());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
}

// This tests the driver's ability to originate gain changes, such as from hardware buttons. It also
// validates that gain notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(StreamConfigTest, DynamicGainUpdate) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  fake_stream_config->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }});

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify_->gain_state());
  ASSERT_TRUE(notify_->gain_state()->gain_db());
  ASSERT_TRUE(notify_->gain_state()->muted());
  ASSERT_TRUE(notify_->gain_state()->agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(*notify_->gain_state()->muted());
  EXPECT_TRUE(*notify_->gain_state()->agc_enabled());
}

// This tests the ability to set gain to the driver, such as from GUI volume controls.
TEST_F(StreamConfigTest, SetGain) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  EXPECT_TRUE(SetDeviceGain(device, {{
                                        .muted = true,
                                        .agc_enabled = true,
                                        .gain_db = kNewGainDb,
                                    }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify_->gain_state() && notify_->gain_state()->gain_db() &&
              notify_->gain_state()->muted() && notify_->gain_state()->agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(notify_->gain_state()->muted().value_or(false));        // Must be present and true.
  EXPECT_TRUE(notify_->gain_state()->agc_enabled().value_or(false));  // Must be present and true.
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(StreamConfigTest, InitialPlugState) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), zx::time(0).get());
}

TEST_F(StreamConfigTest, DynamicPlugUpdate) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), zx::time(0).get());
  notify_->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_stream_config->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify_->plug_state()->second.get(), unplug_time.get());
}

// Validate that Device can open the driver's RingBuffer FIDL channel.
TEST_F(StreamConfigTest, CreateRingBuffer) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
}

TEST_F(StreamConfigTest, RingBufferProperties) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetRingBufferProperties(device);
}

// TODO(https://fxbug.dev/42069012): Unittest CalculateRequiredRingBufferSizes

TEST_F(StreamConfigTest, RingBufferGetVmo) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetDriverVmoAndExpectValid(device);
}

TEST_F(StreamConfigTest, BasicStartAndStop) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);

  StartAndExpectValid(device);
  StopAndExpectValid(device);
}

TEST_F(StreamConfigTest, InitialDelay) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  // Validate that the device received the expected values.
  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(DeviceDelayInfo(device));
  ASSERT_TRUE(DeviceDelayInfo(device)->internal_delay());
  EXPECT_FALSE(DeviceDelayInfo(device)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device)->internal_delay(), 0);

  // Validate that the ControlNotify was sent the expected values.
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
}

TEST_F(StreamConfigTest, DynamicDelay) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
  notify_->delay_info().reset();

  RunLoopUntilIdle();
  EXPECT_FALSE(notify_->delay_info());

  fake_stream_config->InjectDelayUpdate(zx::nsec(123'456), zx::nsec(654'321));
  RunLoopUntilIdle();
  ASSERT_TRUE(DeviceDelayInfo(device));
  ASSERT_TRUE(DeviceDelayInfo(device)->internal_delay());
  ASSERT_TRUE(DeviceDelayInfo(device)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device)->internal_delay(), 123'456);
  EXPECT_EQ(*DeviceDelayInfo(device)->external_delay(), 654'321);

  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified with updated delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  ASSERT_TRUE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 123'456);
  EXPECT_EQ(*notify_->delay_info()->external_delay(), 654'321);
}

TEST_F(StreamConfigTest, ReportsThatItSupportsSetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);
  EXPECT_THAT(device->supports_set_active_channels(), Optional(true));
}

TEST_F(StreamConfigTest, ReportsThatItDoesNotSupportSetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->set_active_channels_supported(false);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);
  EXPECT_THAT(device->supports_set_active_channels(), Optional(false));
}

TEST_F(StreamConfigTest, SetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  fake_stream_config->set_active_channels_supported(true);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  ExpectActiveChannels(device, 0x0003);

  SetActiveChannelsAndExpect(device, 0x0002);
}

// TODO(https://fxbug.dev/42069012): SetActiveChannel no change => no callback (no set_time change).

}  // namespace media_audio
