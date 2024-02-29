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

TEST_F(AudioDeviceRegistryServerWarningTest, UnhealthyDevice) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  fake_driver->set_health_state(false);
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 1u);
}

TEST_F(AudioDeviceRegistryServerWarningTest, FindDeviceByTokenIdError) {
  auto fake_driver = CreateFakeStreamConfigInput();
  fake_driver->set_health_state(false);
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test input", fuchsia_audio_device::DeviceType::kInput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 1u);
  auto token_id = adr_service_->unhealthy_devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Error);
}

TEST_F(AudioDeviceRegistryServerWarningTest, FindDeviceByTokenIdUnknown) {
  EXPECT_EQ(adr_service_->FindDeviceByTokenId(-1).first,
            AudioDeviceRegistry::DevicePresence::Unknown);
}

TEST_F(AudioDeviceRegistryServerWarningTest, FindDeviceByTokenIdRemoved) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Unknown);
}

}  // namespace
}  // namespace media_audio
