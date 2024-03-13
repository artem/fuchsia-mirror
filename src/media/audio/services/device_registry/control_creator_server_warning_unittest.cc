// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"

namespace media_audio {
namespace {

using ControlCreator = fuchsia_audio_device::ControlCreator;
using Registry = fuchsia_audio_device::Registry;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlCreatorServerWarningTest : public AudioDeviceRegistryServerTestBase {};
class ControlCreatorServerCodecWarningTest : public ControlCreatorServerWarningTest {};
class ControlCreatorServerStreamConfigWarningTest : public ControlCreatorServerWarningTest {};

/////////////////////
// Device-less tests
//
// ControlCreator/Create with a missing ID should fail.
TEST_F(ControlCreatorServerWarningTest, MissingId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          // Missing token_id
          .control_server = std::move(control_endpoints->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kInvalidTokenId)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

/////////////////////
// Codec tests
//
// ControlCreator/Create with an unknown ID should fail.
TEST_F(ControlCreatorServerCodecWarningTest, BadId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = std::move(control_endpoints->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kDeviceNotFound)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerCodecWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(added_device_id);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          // Missing server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kInvalidControl)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerCodecWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&received_callback,
         &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(received_callback);
  }

  ASSERT_TRUE(added_device_id);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(),  // Bad server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 0u);
}

TEST_F(ControlCreatorServerCodecWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&received_callback,
         &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(received_callback);
  }

  ASSERT_TRUE(added_device_id);
  ASSERT_EQ(ControlServer::count(), 0u);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_1 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(control_endpoints->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client_1.is_valid());
  auto control_endpoints2 = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_2 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints2->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(control_endpoints2->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kAlreadyAllocated)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before ControlCreator/Create. Expect Obs/Ctl/RingBuf to drop + Reg/WatcDevRemoved.

/////////////////////
// StreamConfig tests
//
// ControlCreator/Create with an unknown ID should fail.
TEST_F(ControlCreatorServerStreamConfigWarningTest, BadId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(added_device_id);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = std::move(control_endpoints->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kDeviceNotFound)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);

  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          // Missing server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kInvalidControl)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&received_callback,
         &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(received_callback);
  }

  ASSERT_TRUE(added_device_id);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(),  // Bad server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 0u);
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  std::optional<TokenId> added_device_id;
  auto received_callback = false;

  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&received_callback,
         &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(received_callback);
  }

  ASSERT_TRUE(added_device_id);
  ASSERT_EQ(ControlServer::count(), 0u);
  auto control_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints.is_ok());
  auto control_client_1 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(control_endpoints->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client_1.is_valid());
  auto control_endpoints2 = fidl::CreateEndpoints<fuchsia_audio_device::Control>();
  ASSERT_TRUE(control_endpoints2.is_ok());
  auto control_client_2 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints2->client), dispatcher(), control_fidl_handler_.get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(control_endpoints2->server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kAlreadyAllocated)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before ControlCreator/Create. Expect Obs/Ctl/RingBuf to drop + Reg/WatcDevRemoved.

}  // namespace
}  // namespace media_audio
