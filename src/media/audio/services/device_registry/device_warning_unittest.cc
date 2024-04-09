// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fidl/cpp/enum.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

class CodecWarningTest : public CodecTest {};
class CompositeWarningTest : public CompositeTest {
 protected:
  // Creating a RingBuffer should fail with `expected_error`.
  void ExpectCreateRingBufferError(
      const std::shared_ptr<Device>& device, ElementId element_id,
      fuchsia_audio_device::ControlCreateRingBufferError expected_error,
      const fuchsia_hardware_audio::Format& format, uint32_t requested_ring_buffer_bytes = 1024) {
    std::stringstream stream;
    stream << "Validating CreateRingBuffer on element_id " << element_id << " with format "
           << *format.pcm_format();
    SCOPED_TRACE(stream.str());

    auto response_received = false;
    auto error_received = fuchsia_audio_device::ControlCreateRingBufferError(0);

    EXPECT_FALSE(device->CreateRingBuffer(
        element_id, format, requested_ring_buffer_bytes,
        [&response_received, &error_received](
            fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>
                result) {
          ASSERT_TRUE(result.is_error());
          error_received = result.error_value();
          response_received = true;
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(response_received);
    EXPECT_EQ(error_received, expected_error);
  }
};
class StreamConfigWarningTest : public StreamConfigTest {};

////////////////////
// Codec tests
//
TEST_F(CodecWarningTest, UnhealthyIsError) {
  auto fake_driver = MakeFakeCodecOutput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_driver);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, UnhealthyCanBeRemoved) {
  auto fake_driver = MakeFakeCodecInput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_driver);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_error_count(), 1u);
}

TEST_F(CodecWarningTest, UnhealthyFailsSetControl) {
  auto fake_driver = MakeFakeCodecNoDirection();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(SetControl(device));
}

TEST_F(CodecWarningTest, UnhealthyFailsAddObserver) {
  auto fake_driver = MakeFakeCodecOutput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(CodecWarningTest, AlreadyControlledFailsSetControl) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(SetControl(device));

  EXPECT_FALSE(SetControl(device));
  EXPECT_TRUE(IsControlled(device));

  // Even though SetControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, AlreadyObservedFailsAddObserver) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  EXPECT_FALSE(AddObserver(device));

  // Even though AddObserver failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, CannotDropUnknownCodecControl) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, CannotDropCodecControlTwice) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);

  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(DropControl(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, WithoutControlFailsCodecCalls) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_FALSE(notify()->dai_format());
  ASSERT_FALSE(notify()->codec_is_started());
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(device->Reset());
  EXPECT_FALSE(device->CodecStop());
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));
  EXPECT_FALSE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_FALSE(notify()->codec_is_started());
}

// SetDaiFormat with invalid formats: expect a warning.
TEST_F(CodecWarningTest, SetInvalidDaiFormat) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  auto invalid_dai_format = SecondDaiFormatFromDaiFormatSets(dai_formats);
  invalid_dai_format.bits_per_sample() = invalid_dai_format.bits_per_slot() + 1;

  device->SetDaiFormat(dai_element_id(), invalid_dai_format);

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  // TODO: Expect a NotSet notification here.

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, SetUnsupportedDaiFormat) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& format_sets) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        for (const auto& format_set : format_sets) {
          dai_formats.push_back(format_set);
        }
      });

  RunLoopUntilIdle();

  // Format is valid but unsupported. The call should fail, but the device should remain healthy.
  device->SetDaiFormat(dai_element_id(), UnsupportedDaiFormatFromDaiFormatSets(dai_formats));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  // TODO: Expect a NotSet notification here.

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, StartBeforeSetDaiFormat) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  // We expect this to fail, because the DaiFormat has not yet been set.
  EXPECT_FALSE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->codec_is_started());
  // We do expect the device to remain healthy and usable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, StopBeforeSetDaiFormat) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  // We expect this to fail, because the DaiFormat has not yet been set.
  EXPECT_FALSE(device->CodecStop());

  RunLoopUntilIdle();
  // We do expect the device to remain healthy and usable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, CreateRingBufferWrongDeviceType) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  auto format = fuchsia_hardware_audio::Format{{
      fuchsia_hardware_audio::PcmFormat{{
          .number_of_channels = 1,
          .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2u,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }};
  int32_t min_bytes = 100;
  bool callback_received = false;
  auto received_error = fuchsia_audio_device::ControlCreateRingBufferError(0);

  // We expect this to fail, because the DaiFormat has not yet been set.
  EXPECT_FALSE(device->CreateRingBuffer(
      fuchsia_audio_device::kDefaultRingBufferElementId, format, min_bytes,
      [&callback_received, &received_error](
          fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>
              result) {
        callback_received = true;
        ASSERT_TRUE(result.is_error());
        received_error = result.error_value();
      }));

  RunLoopUntilIdle();
  EXPECT_TRUE(callback_received);
  EXPECT_EQ(received_error, fuchsia_audio_device::ControlCreateRingBufferError::kWrongDeviceType);
  // We do expect the device to remain healthy and usable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

////////////////////
// Composite tests
//
TEST_F(CompositeWarningTest, UnhealthyIsError) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeComposite(fake_driver);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CompositeWarningTest, CanRemoveUnhealthy) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeComposite(fake_driver);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropComposite();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_error_count(), 1u);
}

TEST_F(CompositeWarningTest, SetControlHealthy) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(SetControl(device));
}

TEST_F(CompositeWarningTest, AddObserverUnhealthy) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(CompositeWarningTest, SetControlAlreadyControlled) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(SetControl(device));

  EXPECT_FALSE(SetControl(device));
  EXPECT_TRUE(IsControlled(device));

  // Even though SetControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CompositeWarningTest, AddObserverAlreadyObserved) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  EXPECT_FALSE(AddObserver(device));

  // Even though AddObserver failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CompositeWarningTest, DropControlUnknown) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CompositeWarningTest, DropControlTwice) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);

  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(DropControl(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CompositeWarningTest, MakeControlCallsWithoutControl) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(notify()->dai_formats().empty());
  ASSERT_TRUE(notify()->codec_format_infos().empty());
  ASSERT_TRUE(notify()->dai_format_errors().empty());
  uint32_t requested_ring_buffer_bytes = 4000;
  ASSERT_TRUE(notify()->dai_formats().empty());
  ASSERT_TRUE(notify()->codec_format_infos().empty());
  ASSERT_TRUE(notify()->dai_format_errors().empty());

  // All three of the primary Control methods (Reset, SetDaiFormat, CreateRingBuffer) should fail,
  // if the Device is not controlled.
  EXPECT_FALSE(device->Reset());

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    const auto dai_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    device->SetDaiFormat(dai_element_id, dai_format);
  }
  EXPECT_TRUE(notify()->dai_formats().empty());
  EXPECT_TRUE(notify()->codec_format_infos().empty());
  EXPECT_TRUE(notify()->dai_format_errors().empty());

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    auto callback_received = false;
    auto received_error = fuchsia_audio_device::ControlCreateRingBufferError(0);
    EXPECT_FALSE(device->CreateRingBuffer(
        ring_buffer_element_id,
        SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
            ring_buffer_element_id, ElementDriverRingBufferFormatSets(device)),
        requested_ring_buffer_bytes,
        [&callback_received, &received_error](
            fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>
                result) {
          callback_received = true;
          ASSERT_TRUE(result.is_error());
          received_error = result.error_value();
        }));

    RunLoopUntilIdle();
    EXPECT_TRUE(callback_received);
    EXPECT_EQ(received_error, fuchsia_audio_device::ControlCreateRingBufferError::kOther);
  }
}

TEST_F(CompositeWarningTest, SetDaiFormatWrongElementType) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto dai_element_id = *device->dai_endpoint_ids().begin();
  auto safe_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
  notify()->clear_dai_formats();
  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    device->SetDaiFormat(ring_buffer_element_id, safe_format);

    RunLoopUntilIdle();
    EXPECT_TRUE(notify()->dai_formats().empty());
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(ExpectDaiFormatError(
        ring_buffer_element_id, fuchsia_audio_device::ControlSetDaiFormatError::kInvalidElementId));
  }
}

// SetDaiFormat with invalid formats: expect a warning.
TEST_F(CompositeWarningTest, SetDaiFormatInvalidFormat) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    notify()->clear_dai_formats();
    auto bad_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    bad_format.channels_to_use_bitmask() = (1u << bad_format.number_of_channels());  // too high

    device->SetDaiFormat(dai_element_id, bad_format);

    RunLoopUntilIdle();
    EXPECT_TRUE(notify()->dai_formats().empty());
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(ExpectDaiFormatError(
        dai_element_id, fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat));
  }
}

TEST_F(CompositeWarningTest, SetDaiFormatUnsupportedFormat) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    notify()->clear_dai_formats();
    auto unsupported_format =
        UnsupportedDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    ASSERT_EQ(ValidateDaiFormat(unsupported_format), ZX_OK);

    device->SetDaiFormat(dai_element_id, unsupported_format);

    RunLoopUntilIdle();
    EXPECT_TRUE(notify()->dai_formats().empty());
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(ExpectDaiFormatError(
        dai_element_id, fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch));
  }
}

TEST_F(CompositeWarningTest, CreateRingBufferInvalidElementId) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  auto ring_buffer_element_id = *device->ring_buffer_endpoint_ids().begin();
  fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
  auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
      ring_buffer_element_id, ring_buffer_format_sets_by_element);

  ExpectCreateRingBufferError(device, -1,
                              fuchsia_audio_device::ControlCreateRingBufferError::kInvalidElementId,
                              safe_format);
}

TEST_F(CompositeWarningTest, CreateRingBufferWrongElementType) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  auto ring_buffer_element_id = *device->ring_buffer_endpoint_ids().begin();
  fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
  auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
      ring_buffer_element_id, ring_buffer_format_sets_by_element);

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    ExpectCreateRingBufferError(
        device, dai_element_id,
        fuchsia_audio_device::ControlCreateRingBufferError::kInvalidElementId, safe_format);
  }
}

TEST_F(CompositeWarningTest, CreateRingBufferInvalidFormat) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
    auto invalid_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        ring_buffer_element_id, ring_buffer_format_sets_by_element);
    invalid_format.pcm_format()->number_of_channels(0);

    ExpectCreateRingBufferError(device, ring_buffer_element_id,
                                fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat,
                                invalid_format);
  }
}

TEST_F(CompositeWarningTest, CreateRingBufferUnsupportedFormat) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
    auto unsupported_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        ring_buffer_element_id, ring_buffer_format_sets_by_element);
    unsupported_format.pcm_format()->frame_rate(unsupported_format.pcm_format()->frame_rate() - 1);

    ExpectCreateRingBufferError(device, ring_buffer_element_id,
                                fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch,
                                unsupported_format);
  }
}

// CreateRingBufferSizeTooSmall test?

// CreateRingBufferSizeTooLarge test?

////////////////////
// StreamConfig tests
//
TEST_F(StreamConfigWarningTest, UnhealthyIsError) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(StreamConfigWarningTest, UnhealthyCanBeRemoved) {
  auto fake_driver = MakeFakeStreamConfigInput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_error_count(), 1u);
}

TEST_F(StreamConfigWarningTest, AlreadyControlledFailsSetControl) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(SetControl(device));

  EXPECT_FALSE(SetControl(device));
  EXPECT_TRUE(IsControlled(device));
}

TEST_F(StreamConfigWarningTest, UnhealthyFailsSetControl) {
  auto fake_driver = MakeFakeStreamConfigInput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(SetControl(device));
}

TEST_F(StreamConfigWarningTest, NoMatchForSupportedRingBufferFormatForClientFormat) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  fake_driver->set_frame_rates(0, {48000});
  fake_driver->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_driver->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kInt16, 2,
                      47999);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kInt16, 2,
                      48001);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kInt16, 1,
                      48000);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kInt16, 3,
                      48000);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kUint8, 2,
                      48000);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kFloat32, 2,
                      48000);
  ExpectNoFormatMatch(device, ring_buffer_element_id(), fuchsia_audio::SampleType::kFloat64, 2,
                      48000);
}

TEST_F(StreamConfigWarningTest, CannotAddSameObserverTwice) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(StreamConfigWarningTest, UnhealthyFailsAddObserver) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  fake_driver->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(StreamConfigWarningTest, CannotDropUnknownControl) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);

  EXPECT_FALSE(DropControl(device));
}

TEST_F(StreamConfigWarningTest, CannotDropControlTwice) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(DropControl(device));

  EXPECT_FALSE(DropControl(device));
}

TEST_F(StreamConfigWarningTest, WithoutControlFailsSetGain) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  RunLoopUntilIdle();
  auto gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());

  constexpr float kNewGainDb = -2.0f;
  EXPECT_FALSE(SetDeviceGain(device, {{
                                         .muted = true,
                                         .agc_enabled = true,
                                         .gain_db = kNewGainDb,
                                     }}));

  RunLoopUntilIdle();
  gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
}

TEST_F(StreamConfigWarningTest, CodecDeviceCallsFail) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  ASSERT_TRUE(IsInitialized(device));

  device->SetDaiFormat(1, fuchsia_hardware_audio::DaiFormat{{}});
  EXPECT_FALSE(device->Reset());
  EXPECT_FALSE(device->CodecStart());
  EXPECT_FALSE(device->CodecStop());
}

// TODO(https://fxbug.dev/42069012): CreateRingBuffer with bad format.

// Also test (StreamConfig) device->CreateRingBuffer with a bad element_id.

// TODO(https://fxbug.dev/42069012): GetVmo size too large; min_frames too large

// Validate that Device can reopen the driver's RingBuffer FIDL channel after closing it.
//
// We perform this positive test here because a potential race can produce a WARNING.
// Controls are also Observers, and this test drops and then immediately re-adds a Control.
// Observers are not explicitly dropped; they are weakly held and allowed to self-invalidate,
// which may not occur before it is re-added, causing a WARNING.
TEST_F(StreamConfigWarningTest, CreateRingBufferTwice) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(IsInitialized(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>
             result) {
        ASSERT_TRUE(result.is_ok());
        auto& info = result.value();
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  ASSERT_TRUE(connected_to_ring_buffer_fidl);
  ExpectRingBufferReady(device, ring_buffer_element_id());
  StartAndExpectValid(device, ring_buffer_element_id());
  StopAndExpectValid(device, ring_buffer_element_id());

  device->DropRingBuffer(ring_buffer_element_id());
  ASSERT_TRUE(device->DropControl());

  ASSERT_TRUE(SetControl(device));
  auto reconnected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>
             result) {
        ASSERT_TRUE(result.is_ok());
        auto& info = result.value();
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(reconnected_to_ring_buffer_fidl);
  ExpectRingBufferReady(device, ring_buffer_element_id());
  StartAndExpectValid(device, ring_buffer_element_id());
  StopAndExpectValid(device, ring_buffer_element_id());
}

// Additional test cases for badly-behaved drivers:
//
// TODO(https://fxbug.dev/42069012): test non-compliant driver behavior (e.g. min_gain>max_gain).
//
// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test:
//    * device becomes unhealthy before any Device method. Expect method-specific failure +
//      State::Error notif. Would include Reset, SetDaiFormat, Start, Stop.
//    * device becomes unhealthy after being Observed / Controlled. Expect both to drop.
// For this reason, the only "UnhealthyDevice" tests needed at this time are
// SetControl/AddObserver.
//
// GetDaiFormats for FakeDriver that fails the GetDaiFormats call
// GetDaiFormats for FakeDriver that returns bad dai_format_sets

}  // namespace media_audio
