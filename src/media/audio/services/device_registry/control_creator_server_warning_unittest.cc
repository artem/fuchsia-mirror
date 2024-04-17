// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"

namespace media_audio {
namespace {

using ControlCreator = fuchsia_audio_device::ControlCreator;
using Registry = fuchsia_audio_device::Registry;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlCreatorServerWarningTest : public AudioDeviceRegistryServerTestBase {};
class ControlCreatorServerCodecWarningTest : public ControlCreatorServerWarningTest {};
class ControlCreatorServerCompositeWarningTest : public ControlCreatorServerWarningTest {};
class ControlCreatorServerStreamConfigWarningTest : public ControlCreatorServerWarningTest {};

/////////////////////
// Device-less tests
//
// ControlCreator/Create with a missing ID should fail.
TEST_F(ControlCreatorServerWarningTest, MissingId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          // Missing token_id
          .control_server = std::move(server),
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
  EXPECT_FALSE(control_creator_fidl_error_status().has_value());
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
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
                                          fuchsia_audio_device::DeviceType::kCodec,
                                          DriverClient::WithCodec(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = std::move(server),
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerCodecWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
                                          fuchsia_audio_device::DeviceType::kCodec,
                                          DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerCodecWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
                                          fuchsia_audio_device::DeviceType::kCodec,
                                          DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  ASSERT_TRUE(control_creator_fidl_error_status().has_value());
  EXPECT_EQ(*control_creator_fidl_error_status(), ZX_ERR_INVALID_ARGS);
}

TEST_F(ControlCreatorServerCodecWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
                                          fuchsia_audio_device::DeviceType::kCodec,
                                          DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  ASSERT_EQ(ControlServer::count(), 0u);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_1 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client_1.is_valid());
  auto [client2, server2] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_2 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client2), dispatcher(), control_fidl_handler().get());
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server2),
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
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before ControlCreator/Create. Expect Obs/Ctl/RingBuf to drop + Reg/WatcDevRemoved.

/////////////////////
// Composite tests
//
// ControlCreator/Create with an unknown ID should fail.
TEST_F(ControlCreatorServerCompositeWarningTest, BadId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
                                          fuchsia_audio_device::DeviceType::kComposite,
                                          DriverClient::WithComposite(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = std::move(server),
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerCompositeWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
                                          fuchsia_audio_device::DeviceType::kComposite,
                                          DriverClient::WithComposite(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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

  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerCompositeWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
                                          fuchsia_audio_device::DeviceType::kComposite,
                                          DriverClient::WithComposite(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  ASSERT_TRUE(control_creator_fidl_error_status().has_value());
  EXPECT_EQ(*control_creator_fidl_error_status(), ZX_ERR_INVALID_ARGS);
}

TEST_F(ControlCreatorServerCompositeWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
                                          fuchsia_audio_device::DeviceType::kComposite,
                                          DriverClient::WithComposite(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  ASSERT_EQ(ControlServer::count(), 0u);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_1 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client_1.is_valid());
  auto [client2, server2] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_2 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client2), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server2),
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
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
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
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
                                          fuchsia_audio_device::DeviceType::kOutput,
                                          DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = std::move(server),
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test input name",
                                          fuchsia_audio_device::DeviceType::kInput,
                                          DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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

  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
                                          fuchsia_audio_device::DeviceType::kOutput,
                                          DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
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
  ASSERT_TRUE(control_creator_fidl_error_status().has_value());
  EXPECT_EQ(*control_creator_fidl_error_status(), ZX_ERR_INVALID_ARGS);
}

TEST_F(ControlCreatorServerStreamConfigWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test input name",
                                          fuchsia_audio_device::DeviceType::kInput,
                                          DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
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
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  }

  ASSERT_TRUE(added_device_id);
  ASSERT_EQ(ControlServer::count(), 0u);
  auto [client, server] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_1 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client_1.is_valid());
  auto [client2, server2] = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client_2 = fidl::Client<fuchsia_audio_device::Control>(
      std::move(client2), dispatcher(), control_fidl_handler().get());
  received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = std::move(server2),
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
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before ControlCreator/Create. Expect Obs/Ctl/RingBuf to drop + Reg/WatcDevRemoved.

}  // namespace
}  // namespace media_audio
