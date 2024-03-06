// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/audio_device_registry.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

class AudioDeviceRegistryServerTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(AudioDeviceRegistryServerTest, DeviceInitialization) {
  auto fake_stream_config = CreateFakeStreamConfigOutput();
  auto fake_codec = CreateFakeCodecOutput();

  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_stream_config->Enable());
  auto codec_client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_codec->Enable());

  AddDeviceForDetection(
      "test output", fuchsia_audio_device::DeviceType::kOutput,
      fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(stream_config_client)));
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(codec_client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 2u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(AudioDeviceRegistryServerTest, DeviceRemoval) {
  auto fake_input = CreateFakeStreamConfigInput();
  auto fake_codec = CreateFakeCodecInput();

  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_input->Enable());
  auto codec_client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_codec->Enable());

  AddDeviceForDetection(
      "test input", fuchsia_audio_device::DeviceType::kInput,
      fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(stream_config_client)));
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(codec_client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 2u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  fake_input->DropStreamConfig();
  fake_codec->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

/////////////////////
// StreamConfig cases
TEST_F(AudioDeviceRegistryServerTest, FindStreamConfigByTokenId) {
  auto fake_driver = CreateFakeStreamConfigInput();

  auto client = fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test input", fuchsia_audio_device::DeviceType::kInput,
                        fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Active);
}

/////////////////////
// Codec cases
TEST_F(AudioDeviceRegistryServerTest, FindCodecByTokenId) {
  auto fake_driver = CreateFakeCodecNoDirection();

  auto client = fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable());
  AddDeviceForDetection("test codec", fuchsia_audio_device::DeviceType::kCodec,
                        fuchsia_audio_device::DriverClient::WithCodec(std::move(client)));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Active);
}

}  // namespace
}  // namespace media_audio
