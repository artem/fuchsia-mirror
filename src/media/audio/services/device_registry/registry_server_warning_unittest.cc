// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/registry_server.h"

namespace media_audio {
namespace {

using DriverClient = fuchsia_audio_device::DriverClient;

class RegistryServerWarningTest : public AudioDeviceRegistryServerTestBase {};
class RegistryServerStreamConfigWarningTest : public RegistryServerWarningTest {};

/////////////////////
// Device-less tests
//
// A subsequent call to WatchDevicesAdded before the previous one completes should fail.
TEST_F(RegistryServerWarningTest, WatchDevicesAddedWhilePending) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  // The first `WatchDevicesAdded` call should pend indefinitely (even after the second one fails).
  registry->client()->WatchDevicesAdded().Then(
      [&received_callback_1](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        received_callback_1 = true;
        ADD_FAILURE() << "Unexpected completion for initial WatchDevicesAdded call";
      });

  RunLoopUntilIdle();
  ASSERT_FALSE(received_callback_1);

  // The second `WatchDevicesAdded` call should fail immediately with domain error
  // ALREADY_PENDING, since the first call has not yet completed.
  registry->client()->WatchDevicesAdded().Then(
      [&received_callback_2](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        received_callback_2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryWatchDevicesAddedError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback_1);
  EXPECT_TRUE(received_callback_2);
  EXPECT_EQ(RegistryServer::count(), 1u);
}

// A subsequent call to WatchDeviceRemoved before the previous one completes should fail.
TEST_F(RegistryServerWarningTest, WatchDeviceRemovedWhilePending) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  // The first `WatchDeviceRemoved` call should pend indefinitely (even after the second one fails).
  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback_1](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        received_callback_1 = true;
        ADD_FAILURE() << "Unexpected completion for initial WatchDeviceRemoved call";
      });

  RunLoopUntilIdle();
  ASSERT_FALSE(received_callback_1);

  // The second `WatchDeviceRemoved` call should fail immediately with domain error
  // ALREADY_PENDING, since the first call has not yet completed.
  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback_2](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        received_callback_2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryWatchDeviceRemovedError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback_1);
  EXPECT_TRUE(received_callback_2);
  EXPECT_EQ(RegistryServer::count(), 1u);
}

// If the required 'id' field is not set, CreateObserver should fail with kInvalidTokenId.
TEST_F(RegistryServerWarningTest, CreateObserverMissingToken) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;

  registry->client()
      ->CreateObserver({{
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kInvalidTokenId)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// If 'id' doesn't identify an initialized device, CreateObserver should fail with kDeviceNotFound.
TEST_F(RegistryServerWarningTest, CreateObserverBadToken) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;

  registry->client()
      ->CreateObserver({{
          .token_id = 0,  // no device is present yet.
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kDeviceNotFound)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

/////////////////////
// StreamConfig tests
//
// If the required 'observer_server' field is not set, we should fail.
TEST_F(RegistryServerStreamConfigWarningTest, CreateObserverMissingObserver) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      DriverClient::WithStreamConfig(
          fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable()))));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_id;
  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_id](fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_id);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  received_callback = false;

  registry->client()
      ->CreateObserver({{
          .token_id = *added_id,
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kInvalidObserver)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// If 'observer_server' is not set to a valid handle, we should fail.
TEST_F(RegistryServerStreamConfigWarningTest, CreateObserverBadObserver) {
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test input name", fuchsia_audio_device::DeviceType::kInput,
      DriverClient::WithStreamConfig(
          fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable()))));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_id;
  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_id](fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_id);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  received_callback = false;

  registry->client()
      ->CreateObserver({{
          .token_id = *added_id,
          .observer_server = fidl::ServerEnd<fuchsia_audio_device::Observer>(zx::channel()),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, add a test case for:
//   CreateObserver with token of StreamConfig that was ready & Added, but then became unhealthy.

}  // namespace
}  // namespace media_audio
