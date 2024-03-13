// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/markers.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

using Provider = fuchsia_audio_device::Provider;
using Registry = fuchsia_audio_device::Registry;

class ProviderServerTest : public AudioDeviceRegistryServerTestBase {};
class ProviderServerCodecTest : public ProviderServerTest {};
class ProviderServerStreamConfigTest : public ProviderServerTest {};

/////////////////////
// Device-less tests
//
// A client can drop a Producer connection without hangs or WARNING logging.
TEST_F(ProviderServerTest, CleanClientDrop) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);

  provider->client() = fidl::Client<Provider>();

  // No WARNING logging should occur.
}

// A server can shutdown a Producer connection without hangs or WARNING logging.
TEST_F(ProviderServerTest, CleanServerShutdown) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);

  provider->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // No WARNING logging should occur.
}

/////////////////////
// Codec tests
//
// An added Codec lives even after the used Provider connection is dropped.
TEST_F(ProviderServerCodecTest, AddedDeviceThatOutlivesProvider) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider->client() = fidl::Client<Provider>();

  RunLoopUntilIdle();
  EXPECT_TRUE(provider->server().WaitForShutdown(zx::sec(1)));
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// An added Codec can be dropped without affecting the used Provider.
TEST_F(ProviderServerCodecTest, ProviderCanOutliveAddedDevice) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  fake_driver->DropCodec();

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(ProviderServer::count(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// For Codecs added by Provider, ensure that Add-then-Watch works as expected.
TEST_F(ProviderServerCodecTest, AddThenWatch) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto registry_wrapper = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecNoDirection();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  received_callback = false;

  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// For Codecs added by Provider, ensure that Watch-then-Add works as expected.
TEST_F(ProviderServerCodecTest, WatchThenAdd) {
  auto registry_wrapper = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback1 = false, received_callback2 = false;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&received_callback1](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback1 = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
      });

  RunLoopUntilIdle();
  ASSERT_FALSE(received_callback1);
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();

  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback2](fidl::Result<Provider::AddDevice>& result) {
        received_callback2 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  EXPECT_TRUE(received_callback2);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

/////////////////////
// StreamConfig tests
//
// An added StreamConfig lives even after the used Provider connection is dropped.
TEST_F(ProviderServerStreamConfigTest, AddedDeviceThatOutlivesProvider) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider->client() = fidl::Client<Provider>();

  RunLoopUntilIdle();
  EXPECT_TRUE(provider->server().WaitForShutdown(zx::sec(1)));
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// An added StreamConfig can be dropped without affecting the used Provider.
TEST_F(ProviderServerStreamConfigTest, ProviderCanOutliveAddedDevice) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test input",
          .device_type = fuchsia_audio_device::DeviceType::kInput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  fake_driver->DropStreamConfig();

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(ProviderServer::count(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// For StreamConfigs added by Provider, ensure that Add-then-Watch works as expected.
TEST_F(ProviderServerStreamConfigTest, AddThenWatch) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto registry_wrapper = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  received_callback = false;

  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// For StreamConfigs added by Provider, ensure that Watch-then-Add works as expected.
TEST_F(ProviderServerStreamConfigTest, WatchThenAdd) {
  auto registry_wrapper = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback1 = false, received_callback2 = false;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&received_callback1](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback1 = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
      });

  RunLoopUntilIdle();
  ASSERT_FALSE(received_callback1);
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();

  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback2](fidl::Result<Provider::AddDevice>& result) {
        received_callback2 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  EXPECT_TRUE(received_callback2);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

}  // namespace
}  // namespace media_audio
