// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

using Control = fuchsia_audio_device::Control;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlServerTest : public AudioDeviceRegistryServerTestBase {
 protected:
  std::unique_ptr<FakeStreamConfig> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeStreamConfigOutput();

    adr_service_->AddDevice(Device::Create(
        adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
        DriverClient::WithStreamConfig(
            fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable()))));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }

  // Obtain a control via ControlCreator/Create (not the synthetic CreateTestControlServer method).
  fidl::Client<fuchsia_audio_device::Control> ConnectToControl(
      fidl::Client<fuchsia_audio_device::ControlCreator>& control_creator_client,
      TokenId token_id) {
    auto [control_client_end, control_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Control>();
    auto control_client = fidl::Client<fuchsia_audio_device::Control>(
        fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(control_client_end)), dispatcher(),
        control_fidl_handler_.get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server =
                fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(control_server_end)),
        }})
        .Then([&received_callback](
                  fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_TRUE(control_client.is_valid());
    return control_client;
  }
};

TEST_F(ControlServerTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->client() = fidl::Client<fuchsia_audio_device::Control>();

  // If Control client doesn't drop cleanly, ControlServer will emit a WARNING, causing a failure.
}

TEST_F(ControlServerTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // If ControlServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

// Same as "CleanClientDrop" test case, but the Control is created "properly" through a
// ControlCreator rather than directly via AudioDeviceRegistry::CreateControlServer.
TEST_F(ControlServerTest, BasicClose) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  RunLoopUntilIdle();
  EXPECT_TRUE(control_client.is_valid());
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, ControlCreatorServerShutdownDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  control_creator->server().Shutdown(ZX_ERR_PEER_CLOSED);
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator->server().WaitForShutdown(zx::sec(1)));

  EXPECT_TRUE(control_client.is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, SetGain) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto received_callback = false;

  control->client()
      ->SetGain({{
          .target_state = fuchsia_audio_device::GainState{{.gain_db = -1.0f}},
      }})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the Control lives, even if the client drops its child RingBuffer.
TEST_F(ControlServerTest, ClientRingBufferDropDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  {
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    bool received_callback = false;

    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler_.get());

    control->client()
        ->CreateRingBuffer({{
            .options = fuchsia_audio_device::RingBufferOptions{{
                .format = fuchsia_audio::Format{{
                    .sample_type = fuchsia_audio::SampleType::kInt16,
                    .channel_count = 2,
                    .frames_per_second = 48000,
                }},
                .ring_buffer_min_bytes = 2000,
            }},
            .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                std::move(ring_buffer_server_end)),
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);

    // Let our RingBuffer client connection drop.
    ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  }

  // Wait for the RingBufferServer to destruct.
  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }

  // Allow the ControlServer to destruct, if it (erroneously) wants to.
  RunLoopUntilIdle();
  EXPECT_TRUE(control->client().is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the Control lives, even if the driver drops its RingBuffer connection.
TEST_F(ControlServerTest, DriverRingBufferDropDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto [ring_buffer_client, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  bool received_callback = false;

  control->client()
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 2000,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_EQ(RingBufferServer::count(), 1u);
  EXPECT_TRUE(received_callback);

  // Drop the driver RingBuffer connection.
  fake_driver->DropRingBuffer();

  // Wait for the RingBufferServer to destruct.
  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }

  // Allow the ControlServer to destruct, if it (erroneously) wants to.
  RunLoopUntilIdle();
  EXPECT_TRUE(control->client().is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the ControlServer shuts down cleanly if the driver drops its StreamConfig.
TEST_F(ControlServerTest, StreamConfigDropCausesCleanControlServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto [ring_buffer_client, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  bool received_callback = false;

  control->client()
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 2000,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_EQ(RingBufferServer::count(), 1u);
  EXPECT_TRUE(received_callback);

  // Drop the driver StreamConfig connection.
  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }
  EXPECT_TRUE(control->server().WaitForShutdown(zx::sec(5)));
}

// TODO(https://fxbug.dev/42069012): unittest GetCurrentlyPermittedFormats

}  // namespace
}  // namespace media_audio
