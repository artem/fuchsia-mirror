// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

class AudioDeviceRegistryServerWarningTest : public AudioDeviceRegistryServerTestBase {};

// Device-less tests
TEST_F(AudioDeviceRegistryServerWarningTest, FindDeviceByTokenIdUnknown) {
  EXPECT_EQ(adr_service_->FindDeviceByTokenId(-1).first,
            AudioDeviceRegistry::DevicePresence::Unknown);
}

// Tests that use multiple device types
TEST_F(AudioDeviceRegistryServerWarningTest, UnhealthyDevices) {
  auto fake_stream_config = CreateFakeStreamConfigOutput();
  auto fake_codec = CreateFakeCodecOutput();

  fake_stream_config->set_health_state(false);
  fake_codec->set_health_state(false);

  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_stream_config->Enable());
  auto codec_client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_codec->Enable());

  AddDeviceForDetection(
      "test output", fuchsia_audio_device::DeviceType::kOutput,
      fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(stream_config_client)));
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(codec_client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 2u);
}

/////////////////////
// StreamConfig cases
TEST_F(AudioDeviceRegistryServerWarningTest, FindStreamConfigByTokenIdError) {
  auto fake_driver = CreateFakeStreamConfigInput();
  fake_driver->set_health_state(false);
  auto client = fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test input", fuchsia_audio_device::DeviceType::kInput,
                        fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 1u);
  auto token_id = adr_service_->unhealthy_devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Error);
}

TEST_F(AudioDeviceRegistryServerWarningTest, FindStreamConfigByTokenIdRemoved) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto client = fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Unknown);
}

/////////////////////
// Codec cases
TEST_F(AudioDeviceRegistryServerWarningTest, FindCodecByTokenIdError) {
  auto fake_driver = CreateFakeCodecInput();
  fake_driver->set_health_state(false);
  auto client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable());
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 1u);
  auto token_id = adr_service_->unhealthy_devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Error);
}

TEST_F(AudioDeviceRegistryServerWarningTest, FindCodecByTokenIdRemoved) {
  auto fake_driver = CreateFakeCodecNoDirection();
  auto client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable());
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  fake_driver->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Unknown);
}

}  // namespace
}  // namespace media_audio
