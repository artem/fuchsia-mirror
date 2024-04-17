// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <string>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

using Control = fuchsia_audio_device::Control;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::Control>,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
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
        std::move(control_client_end), dispatcher(), control_fidl_handler().get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server = std::move(control_server_end),
        }})
        .Then([&received_callback](
                  fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_TRUE(control_client.is_valid());
    return control_client;
  }

  static ElementId ring_buffer_element_id() { return kRingBufferElementId; }
  static ElementId dai_element_id() { return kDaiElementId; }

 private:
  static constexpr ElementId kRingBufferElementId =
      fuchsia_audio_device::kDefaultRingBufferElementId;
  static constexpr ElementId kDaiElementId = fuchsia_audio_device::kDefaultDaiInterconnectElementId;
};

class ControlServerCodecWarningTest : public ControlServerWarningTest {
 protected:
  std::shared_ptr<FakeCodec> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeCodecInput();

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
                                            fuchsia_audio_device::DeviceType::kCodec,
                                            DriverClient::WithCodec(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

class ControlServerCompositeWarningTest : public ControlServerWarningTest {
 protected:
  std::shared_ptr<FakeComposite> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeComposite();

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
                                            fuchsia_audio_device::DeviceType::kComposite,
                                            DriverClient::WithComposite(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  void TestCreateRingBufferBadOptions(
      const std::optional<fuchsia_audio_device::RingBufferOptions>& bad_options,
      fuchsia_audio_device::ControlCreateRingBufferError expected_error) {
    auto fake_driver = CreateAndEnableDriverWithDefaults();
    auto registry = CreateTestRegistryServer();

    auto added_id = WaitForAddedDeviceTokenId(registry->client());
    auto control_creator = CreateTestControlCreatorServer();
    auto control_client = ConnectToControl(control_creator->client(), *added_id);

    RunLoopUntilIdle();
    ASSERT_EQ(ControlServer::count(), 1u);
    auto device = *adr_service()->devices().begin();

    for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
      fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
      auto [ring_buffer_client_end, ring_buffer_server_end] =
          CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
      auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
          std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
      bool received_callback = false;

      control_client
          ->CreateRingBuffer({{
              ring_buffer_element_id,
              bad_options,
              std::move(ring_buffer_server_end),
          }})
          .Then([&received_callback,
                 expected_error](fidl::Result<Control::CreateRingBuffer>& result) {
            received_callback = true;
            ASSERT_TRUE(result.is_error());
            ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
            EXPECT_EQ(result.error_value().domain_error(), expected_error) << result.error_value();
          });

      RunLoopUntilIdle();
      EXPECT_TRUE(received_callback);
      EXPECT_EQ(ControlServer::count(), 1u);
    }
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
    EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
  }
};

class ControlServerStreamConfigWarningTest : public ControlServerWarningTest {
 protected:
  std::shared_ptr<FakeStreamConfig> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeStreamConfigOutput();

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
                                            fuchsia_audio_device::DeviceType::kOutput,
                                            DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  // Device error causes ControlNotify->DeviceHasError
  // Driver drops StreamConfig causes ControlNotify->DeviceIsRemoved
  // Client closes RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError
  // Driver drops RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError

  void TestSetGainBadState(const std::optional<fuchsia_audio_device::GainState>& bad_state,
                           fuchsia_audio_device::ControlSetGainError expected_error) {
    auto fake_driver = CreateFakeStreamConfigOutput();
    fake_driver->set_can_mute(false);
    fake_driver->set_can_agc(false);
    fake_driver->AllocateRingBuffer(8192);
    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
                                            fuchsia_audio_device::DeviceType::kOutput,
                                            DriverClient::WithStreamConfig(fake_driver->Enable())));

    RunLoopUntilIdle();
    auto registry = CreateTestRegistryServer();

    auto added_id = WaitForAddedDeviceTokenId(registry->client());
    auto control_creator = CreateTestControlCreatorServer();
    auto control_client = ConnectToControl(control_creator->client(), *added_id);

    RunLoopUntilIdle();
    ASSERT_EQ(ControlServer::count(), 1u);
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
    bool received_callback = false;

    control_client
        ->SetGain({{
            .target_state = bad_state,
        }})
        .Then([&received_callback, expected_error](fidl::Result<Control::SetGain>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(), expected_error) << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
    EXPECT_FALSE(control_creator_fidl_error_status().has_value())
        << *control_creator_fidl_error_status();
  }

  void TestCreateRingBufferBadOptions(
      const std::optional<fuchsia_audio_device::RingBufferOptions>& bad_options,
      fuchsia_audio_device::ControlCreateRingBufferError expected_error) {
    auto fake_driver = CreateAndEnableDriverWithDefaults();
    fake_driver->AllocateRingBuffer(8192);

    auto registry = CreateTestRegistryServer();
    auto added_id = WaitForAddedDeviceTokenId(registry->client());
    auto control_creator = CreateTestControlCreatorServer();
    auto control_client = ConnectToControl(control_creator->client(), *added_id);

    RunLoopUntilIdle();
    ASSERT_EQ(ControlServer::count(), 1u);
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
    bool received_callback = false;

    control_client
        ->CreateRingBuffer({{
            .options = bad_options,
            .ring_buffer_server = std::move(ring_buffer_server_end),
        }})
        .Then([&received_callback,
               expected_error](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(), expected_error) << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
    EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
    EXPECT_FALSE(control_creator_fidl_error_status().has_value())
        << *control_creator_fidl_error_status();
  }
};

/////////////////////
// Codec tests
//
// SetDaiFormat when already pending
TEST_F(ControlServerCodecWarningTest, SetDaiFormatWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto dai_format2 =
      SecondDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;
  auto received_callback2 = false;

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback2](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat invalid
TEST_F(ControlServerCodecWarningTest, SetDaiFormatInvalidFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto invalid_dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  invalid_dai_format.bits_per_sample() = invalid_dai_format.bits_per_slot() + 1;
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = invalid_dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat unsupported
TEST_F(ControlServerCodecWarningTest, SetDaiFormatUnsupportedFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto unsupported_dai_format =
      UnsupportedDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = unsupported_dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Start when already pending
TEST_F(ControlServerCodecWarningTest, CodecStartWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  auto received_callback2 = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()->CodecStart().Then(
      [&received_callback2](fidl::Result<Control::CodecStart>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Start before SetDaiFormat
TEST_F(ControlServerCodecWarningTest, CodecStartBeforeSetDaiFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kDaiFormatNotSet)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Start when Started
TEST_F(ControlServerCodecWarningTest, CodecStartWhenStarted) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kAlreadyStarted)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Stop when already pending
TEST_F(ControlServerCodecWarningTest, CodecStopWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  auto received_callback2 = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()->CodecStop().Then(
      [&received_callback2](fidl::Result<Control::CodecStop>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Stop before SetDaiFormat
TEST_F(ControlServerCodecWarningTest, CodecStopBeforeSetDaiFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kDaiFormatNotSet)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Stop when Stopped
TEST_F(ControlServerCodecWarningTest, CodecStopWhenStopped) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id(), device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kAlreadyStopped)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetGain WRONG_DEVICE_TYPE
TEST_F(ControlServerCodecWarningTest, SetGainWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->SetGain({{
          .target_state = fuchsia_audio_device::GainState{{
              .gain_db = 0.0f,
              .muted = false,
              .agc_enabled = false,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetGainError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCodecWarningTest, CreateRingBufferWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
  auto received_callback = false;

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
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain),
// including in the FakeCodec test fixture. Then add negative test cases for
// GetTopologies/GetElements/WatchTopology/WatchElementState, as are in Composite, as well as
// negative cases for SetTopology/SetElementState.

// Verify WatchTopology if the driver has an error.

// Verify WatchTopology if the driver does not support signalprocessing.
TEST_F(ControlServerCodecWarningTest, WatchTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->WatchTopology().Then(
      [&received_callback](fidl::Result<Control::WatchTopology>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After this failing call, the binding should not be usable.
  control->client()->Reset().Then([&received_callback](fidl::Result<Control::Reset>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_framework_error());
    EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_NOT_SUPPORTED);
}

// Verify WatchElementState if the driver has an error.

// Verify WatchElementState if the driver does not support signalprocessing.
TEST_F(ControlServerCodecWarningTest, WatchElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->WatchElementState(fuchsia_audio_device::kDefaultDaiInterconnectElementId)
      .Then([&received_callback](fidl::Result<Control::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After this failing call, the binding should not be usable.
  control->client()->Reset().Then([&received_callback](fidl::Result<Control::Reset>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_framework_error());
    EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_NOT_SUPPORTED);
}

// Verify SetTopology if the driver has an error.

// Verify SetTopology if the driver does not support signalprocessing.
TEST_F(ControlServerCodecWarningTest, SetTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->SetTopology(0).Then([&received_callback](
                                             fidl::Result<Control::SetTopology>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Verify SetElementState if the driver has an error.

// Verify SetElementState if the driver does not support signalprocessing.
TEST_F(ControlServerCodecWarningTest, SetElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->SetElementState({fuchsia_audio_device::kDefaultDaiInterconnectElementId, {}})
      .Then([&received_callback](fidl::Result<Control::SetElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

/////////////////////
// Composite tests
//
// SetDaiFormat when already pending
TEST_F(ControlServerCompositeWarningTest, SetDaiFormatWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    auto dai_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    auto dai_format2 =
        SecondDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    auto received_callback = false;
    auto received_callback2 = false;

    control->client()
        ->SetDaiFormat({{
            dai_element_id,
            dai_format,
        }})
        .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
          received_callback = true;
          EXPECT_TRUE(result.is_ok()) << result.error_value();
        });
    control->client()
        ->SetDaiFormat({{
            dai_element_id,
            dai_format2,
        }})
        .Then([&received_callback2](fidl::Result<Control::SetDaiFormat>& result) {
          received_callback2 = true;
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlSetDaiFormatError::kAlreadyPending)
              << result.error_value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback && received_callback2);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat invalid
TEST_F(ControlServerCompositeWarningTest, SetDaiFormatInvalidFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    auto invalid_dai_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    invalid_dai_format.bits_per_sample() = invalid_dai_format.bits_per_slot() + 1;
    auto received_callback = false;

    control->client()
        ->SetDaiFormat({{
            dai_element_id,
            invalid_dai_format,
        }})
        .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat)
              << result.error_value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat unsupported
TEST_F(ControlServerCompositeWarningTest, SetDaiFormatUnsupportedFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    auto unsupported_dai_format =
        UnsupportedDaiFormatFromElementDaiFormatSets(dai_element_id, device->dai_format_sets());
    auto received_callback = false;

    control->client()
        ->SetDaiFormat({{
            dai_element_id,
            unsupported_dai_format,
        }})
        .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch)
              << result.error_value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat on RingBuffer element
TEST_F(ControlServerCompositeWarningTest, SetDaiFormatWrongElementType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto ring_buffer_element_id = *device->ring_buffer_endpoint_ids().begin();
  auto dai_element_id_unused = *device->dai_endpoint_ids().begin();
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id_unused, device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{
          ring_buffer_element_id,
          dai_format,
      }})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kInvalidElementId)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// SetDaiFormat on unknown element_id
TEST_F(ControlServerCompositeWarningTest, SetDaiFormatUnknownElementId) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  ElementId ring_buffer_element_id = -1;
  auto dai_element_id_unused = *device->dai_endpoint_ids().begin();
  auto dai_format =
      SafeDaiFormatFromElementDaiFormatSets(dai_element_id_unused, device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{
          ring_buffer_element_id,
          dai_format,
      }})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kInvalidElementId)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, SetGainWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->SetGain({{
          fuchsia_audio_device::GainState{{
              .gain_db = 0.0f,
              .muted = false,
              .agc_enabled = false,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetGainError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CodecStartWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStart().Then([&received_callback](
                                           fidl::Result<Control::CodecStart>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStartError::kWrongDeviceType)
        << result.error_value();
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CodecStopWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStop().Then([&received_callback](
                                          fidl::Result<Control::CodecStop>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStopError::kWrongDeviceType)
        << result.error_value();
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferWrongElementType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service()->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();

    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());

    control->client()
        ->CreateRingBuffer({{
            dai_element_id,
            fuchsia_audio_device::RingBufferOptions{{
                .format = fuchsia_audio::Format{{
                    .sample_type = fuchsia_audio::SampleType::kInt16,
                    .channel_count = 2,
                    .frames_per_second = 48000,
                }},
                .ring_buffer_min_bytes = 2000,
            }},
            std::move(ring_buffer_server_end),
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlCreateRingBufferError::kInvalidElementId)
              << result.error_value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingOptions) {
  TestCreateRingBufferBadOptions(
      std::nullopt,  // entirely missing table
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferEmptyOptions) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions(),  // entirely empty table
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = std::nullopt,  // missing
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferEmptyFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format(),  // empty
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              // missing sample_type
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferBadSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kFloat64,  // bad value
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              // missing channel_count
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferBadChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 7,  // bad value
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16, .channel_count = 2,
              // missing frames_per_second
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferBadFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 97531,  // bad value
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingRingBufferMinBytes) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          // missing ring_buffer_min_bytes
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes);
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  auto device = *adr_service()->devices().begin();
  ASSERT_EQ(ControlServer::count(), 1u);

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
    auto [ring_buffer_client_end1, ring_buffer_server_end1] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto [ring_buffer_client_end2, ring_buffer_server_end2] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto options = fuchsia_audio_device::RingBufferOptions{{
        .format = SafeRingBufferFormatFromElementRingBufferFormatSets(
            ring_buffer_element_id, device->ring_buffer_format_sets()),
        .ring_buffer_min_bytes = 4096,
    }};
    bool received_callback_1 = false, received_callback_2 = false;

    control_client
        ->CreateRingBuffer({{
            ring_buffer_element_id,
            options,
            std::move(ring_buffer_server_end1),
        }})
        .Then([&received_callback_1](fidl::Result<Control::CreateRingBuffer>& result) {
          received_callback_1 = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          EXPECT_TRUE(result->properties().has_value());
          EXPECT_TRUE(result->ring_buffer().has_value());
        });
    control_client
        ->CreateRingBuffer({{
            ring_buffer_element_id,
            options,
            std::move(ring_buffer_server_end2),
        }})
        .Then([&received_callback_2](fidl::Result<Control::CreateRingBuffer>& result) {
          received_callback_2 = true;
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlCreateRingBufferError::kAlreadyPending)
              << result.error_value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback_1 && received_callback_2);
    EXPECT_EQ(ControlServer::count(), 1u);
    EXPECT_TRUE(control_client.is_valid());
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferUnknownElementId) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  auto device = *adr_service()->devices().begin();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto ring_buffer_element_id_unused = *device->ring_buffer_endpoint_ids().begin();
  // fake_driver->ReserveRingBufferSize(ring_buffer_element_id_unused, 8192);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto options = fuchsia_audio_device::RingBufferOptions{{
      .format = SafeRingBufferFormatFromElementRingBufferFormatSets(
          ring_buffer_element_id_unused, device->ring_buffer_format_sets()),
      .ring_buffer_min_bytes = 2000,
  }};
  ElementId unknown_element_id = -1;
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          unknown_element_id,
          options,
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kInvalidElementId)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerCompositeWarningTest, CreateRingBufferMissingRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto device = *adr_service()->devices().begin();
  bool received_callback = false;

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
    control_client
        ->CreateRingBuffer({{
            .element_id = ring_buffer_element_id,
            .options = fuchsia_audio_device::RingBufferOptions{{
                .format = fuchsia_audio::Format{{
                    .sample_type = fuchsia_audio::SampleType::kInt16,
                    .channel_count = 2,
                    .frames_per_second = 48000,
                }},
                .ring_buffer_min_bytes = 8192,
            }},
            // missing server_end
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(),
                    fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer)
              << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

// If the ServerEnd<RingBuffer> passed to CreateRingBuffer is invalid, the Control will disconnect.
// We recreate it for each RING_BUFFER element so we can probe each one.
TEST_F(ControlServerCompositeWarningTest, CreateRingBufferBadRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto device = *adr_service()->devices().begin();

  for (auto ring_buffer_element_id : device->ring_buffer_endpoint_ids()) {
    auto control_client = ConnectToControl(control_creator->client(), *added_id);

    RunLoopUntilIdle();
    ASSERT_EQ(ControlServer::count(), 1u);
    bool received_callback = false;

    fake_driver->ReserveRingBufferSize(ring_buffer_element_id, 8192);
    control_client
        ->CreateRingBuffer({{
            ring_buffer_element_id,
            fuchsia_audio_device::RingBufferOptions{{
                .format = fuchsia_audio::Format{{
                    .sample_type = fuchsia_audio::SampleType::kInt16,
                    .channel_count = 2,
                    .frames_per_second = 48000,
                }},
                .ring_buffer_min_bytes = 8192,
            }},
            fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(),  // bad value
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
          EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
              << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 0u);
    ASSERT_TRUE(control_fidl_error_status().has_value());
    EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_INVALID_ARGS);
  }
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
}

// TODO(https://fxbug.dev/42069012): Create a unittest to test the upper limit of VMO size (4Gb).
//     This is not high-priority since even at the service's highest supported bitrate (192kHz,
//     8-channel, float64), a 4Gb ring-buffer would be 5.8 minutes long!
//     Note the disabled attempt (for StreamConfig below) to create this.
// TEST_F(ControlServerCompositeWarningTest, DISABLED_CreateRingBufferHugeRingBufferMinBytes) {}

// Verify WatchTopology if the driver has an error.

TEST_F(ControlServerCompositeWarningTest, WatchTopologyWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback1 = false, received_callback2 = false;

  control->client()->WatchTopology().Then(
      [&received_callback1](fidl::Result<Control::WatchTopology>& result) {
        received_callback1 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  control->client()->WatchTopology().Then(
      [&received_callback1](fidl::Result<Control::WatchTopology>& result) {
        // This should pend until the subsequent WatchTopology fails, causing a disconnect.
        // The epitaph of that disconnect is ZX_ERR_BAD_STATE.
        received_callback1 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE);
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback1);

  control->client()->WatchTopology().Then(
      [&received_callback2](fidl::Result<Control::WatchTopology>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback2);
  // After a failing WatchTopology call, the binding should not be usable, so the previous
  // WatchElementState will complete with a failure.
  EXPECT_TRUE(received_callback1);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_BAD_STATE);
}

// Verify WatchElementState if the driver has an error.

TEST_F(ControlServerCompositeWarningTest, WatchElementStateUnknownElementId) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto& elements_from_device = element_map(device);
  ElementId unknown_element_id = 0;
  while (true) {
    if (elements_from_device.find(unknown_element_id) == elements_from_device.end()) {
      break;
    }
    ++unknown_element_id;
  }
  auto received_callback = false;

  control->client()
      ->WatchElementState(unknown_element_id)
      .Then([&received_callback](fidl::Result<Control::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_INVALID_ARGS);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  // After a failing WatchElementState call, the binding should not be usable.
  control->client()->Reset().Then([&received_callback](fidl::Result<Control::Reset>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_framework_error());
    EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_INVALID_ARGS);
}

TEST_F(ControlServerCompositeWarningTest, WatchElementStateWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto& elements_from_device = element_map(device);

  auto element_id = elements_from_device.begin()->first;
  auto received_callback1 = false, received_callback2 = false;

  control->client()
      ->WatchElementState(element_id)
      .Then([&received_callback1](fidl::Result<Control::WatchElementState>& result) {
        received_callback1 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  control->client()
      ->WatchElementState(element_id)
      .Then([&received_callback1](fidl::Result<Control::WatchElementState>& result) {
        received_callback1 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback1);

  control->client()
      ->WatchElementState(element_id)
      .Then([&received_callback2](fidl::Result<Control::WatchElementState>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback2);
  // After a failing WatchElementState call, the binding should not be usable, so the previous
  // WatchElementState will complete with a failure.
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  control->client()->Reset().Then([&received_callback1](fidl::Result<Control::Reset>& result) {
    received_callback1 = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
    EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_BAD_STATE)
        << result.error_value().framework_error();
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_BAD_STATE);
}

// Verify SetTopology if the driver has an error.

TEST_F(ControlServerCompositeWarningTest, SetTopologyUnknownId) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  const auto& topologies = topology_map(device);
  TopologyId unknown_topology_id = 0;
  bool found_an_unknown_topology_id = false;
  do {
    if (topologies.find(unknown_topology_id) == topologies.end()) {
      found_an_unknown_topology_id = true;
    } else {
      ++unknown_topology_id;
    }
  } while (!found_an_unknown_topology_id);
  auto received_callback = false;

  control->client()
      ->SetTopology(unknown_topology_id)
      .Then([&received_callback](fidl::Result<Control::SetTopology>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_INVALID_ARGS);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Verify SetTopology if the driver does not support signalprocessing.

// Verify SetElementState if the driver has an error.

// Verify SetElementState if the ElementId is unknown.

// Verify SetElementState if the ElementState is invalid.
//   (missing fields, wrong element type, internally inconsistent values, read-only)

/////////////////////
// StreamConfig tests
//
TEST_F(ControlServerStreamConfigWarningTest, SetGainMissingState) {
  TestSetGainBadState(std::nullopt, fuchsia_audio_device::ControlSetGainError::kInvalidGainState);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainEmptyState) {
  TestSetGainBadState(fuchsia_audio_device::GainState(),
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainMissingGainDb) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainTooHigh) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = +200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainTooLow) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = -200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainBadMute) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = true,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kMuteUnavailable);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainBadAgc) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = false,
                          .agc_enabled = true,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kAgcUnavailable);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingOptions) {
  TestCreateRingBufferBadOptions(
      std::nullopt, fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferEmptyOptions) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions(),
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = std::nullopt,
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferEmptyFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format(),
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kFloat64,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 7,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 97531,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingRingBufferMinBytes) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end1, ring_buffer_server_end1] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto [ring_buffer_client_end2, ring_buffer_server_end2] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto options = fuchsia_audio_device::RingBufferOptions{{
      .format = fuchsia_audio::Format{{
          .sample_type = fuchsia_audio::SampleType::kInt16,
          .channel_count = 2,
          .frames_per_second = 48000,
      }},
      .ring_buffer_min_bytes = 8192,
  }};
  bool received_callback_1 = false, received_callback_2 = false;

  control_client
      ->CreateRingBuffer({{
          .options = options,
          .ring_buffer_server = std::move(ring_buffer_server_end1),
      }})
      .Then([&received_callback_1](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback_1 = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_TRUE(result->properties().has_value());
        EXPECT_TRUE(result->ring_buffer().has_value());
      });
  control_client
      ->CreateRingBuffer({{
          .options = options,
          .ring_buffer_server = std::move(ring_buffer_server_end2),
      }})
      .Then([&received_callback_2](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback_2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_creator_fidl_error_status().has_value())
      << *control_creator_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// TODO(https://fxbug.dev/42069012): Enable this unittest to test the upper limit of VMO size
// (4Gb).
//   This is not high-priority since even at the service's highest supported bitrate (192kHz,
//   8-channel, float64), a 4Gb ring-buffer would be 5.8 minutes long!
TEST_F(ControlServerStreamConfigWarningTest, DISABLED_CreateRingBufferHugeRingBufferMinBytes) {
  auto fake_driver = CreateFakeStreamConfigOutput();

  fake_driver->clear_formats();
  std::vector<fuchsia::hardware::audio::ChannelAttributes> channel_vector;
  channel_vector.push_back(fuchsia::hardware::audio::ChannelAttributes());
  fake_driver->set_channel_sets(0, 0, std::move(channel_vector));
  fake_driver->set_sample_formats(0, {fuchsia::hardware::audio::SampleFormat::PCM_UNSIGNED});
  fake_driver->set_bytes_per_sample(0, {1});
  fake_driver->set_valid_bits_per_sample(0, {8});
  fake_driver->set_frame_rates(0, {48000});

  adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
                                          fuchsia_audio_device::DeviceType::kOutput,
                                          DriverClient::WithStreamConfig(fake_driver->Enable())));
  fake_driver->AllocateRingBuffer(8192);

  RunLoopUntilIdle();
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler().get());
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kUint8,
                  .channel_count = 1,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = -1u,
          }},
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        if (result.is_ok()) {
          FX_LOGS(ERROR) << "RingBufferProperties";
          FX_LOGS(ERROR) << "    valid_bits_per_sample: "
                         << static_cast<int16_t>(
                                result->properties()->valid_bits_per_sample().value_or(-1));
          FX_LOGS(ERROR) << "    turn_on_delay:         "
                         << result->properties()->turn_on_delay().value_or(-1);
          FX_LOGS(ERROR) << "fuchsia.audio.RingBuffer";
          FX_LOGS(ERROR) << "    buffer";
          FX_LOGS(ERROR) << "        vmo:               0x" << std::hex
                         << result->ring_buffer()->buffer()->vmo().get() << " (handle)";
          FX_LOGS(ERROR) << "        size:              0x" << std::hex
                         << result->ring_buffer()->buffer()->size();
          FX_LOGS(ERROR) << "    format";
          FX_LOGS(ERROR) << "        sample_type:       "
                         << fidl::ToUnderlying(*result->ring_buffer()->format()->sample_type());
          FX_LOGS(ERROR) << "        channel_count:     "
                         << *result->ring_buffer()->format()->channel_count();
          FX_LOGS(ERROR) << "        frames_per_second: "
                         << *result->ring_buffer()->format()->frames_per_second();
          FX_LOGS(ERROR)
              << "        channel_layout:    "
              << (result->ring_buffer()->format()->channel_layout().has_value()
                      ? std::to_string(fidl::ToUnderlying(
                            result->ring_buffer()->format()->channel_layout()->config().value()))
                      : "NONE");
          FX_LOGS(ERROR) << "    producer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->producer_bytes();
          FX_LOGS(ERROR) << "    consumer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->consumer_bytes();
          FX_LOGS(ERROR) << "    reference_clock:       0x" << std::hex
                         << result->ring_buffer()->reference_clock()->get() << " (handle)";
          FX_LOGS(ERROR) << "    ref_clock_domain:      "
                         << (result->ring_buffer()->reference_clock_domain().has_value()
                                 ? std::to_string(*result->ring_buffer()->reference_clock_domain())
                                 : "NONE");
        }
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kBadRingBufferOption)
            << result.error_value();

        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);

  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
          .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 0u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_INVALID_ARGS);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before SetGain. Expect Control/Control/RingBuffer to drop, Reg/WatchRemoved.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before CreateRingBuffer. Expect Obs/Ctl to drop, Reg/WatchRemoved.

TEST_F(ControlServerStreamConfigWarningTest, SetDaiFormatWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  fuchsia_hardware_audio::DaiFormat dai_format{{
      .number_of_channels = 1,
      .channels_to_use_bitmask = 1,
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kNone),
      .frame_rate = 48000,
      .bits_per_slot = 16,
      .bits_per_sample = 16,
  }};

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kWrongDeviceType)
            << result.error_value();
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerStreamConfigWarningTest, CodecStartWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStart().Then([&received_callback](
                                           fidl::Result<Control::CodecStart>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStartError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerStreamConfigWarningTest, CodecStopWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStop().Then([&received_callback](
                                          fidl::Result<Control::CodecStop>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStopError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

TEST_F(ControlServerStreamConfigWarningTest, ResetWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service()->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->Reset().Then([&received_callback](fidl::Result<Control::Reset>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlResetError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// TODO(https://fxbug.dev/323270827): implement signalprocessing, including in the FakeStreamConfig
// test fixture. Then add those types of negative test cases for
// GetTopologies/GetElements/WatchTopology/WatchElementState, as are in Composite, as well as for
// SetTopology/SetElementState.
//
// Verify WatchTopology if the driver does not support signalprocessing.
TEST_F(ControlServerStreamConfigWarningTest, WatchTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->WatchTopology().Then(
      [&received_callback](fidl::Result<Control::WatchTopology>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After a failing WatchTopology call, the binding should not be usable.
  control->client()
      ->SetGain({{fuchsia_audio_device::GainState{{.gain_db = 0}}}})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_NOT_SUPPORTED);
}

// Verify WatchElementState if the driver does not support signalprocessing.
TEST_F(ControlServerStreamConfigWarningTest, WatchElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->WatchElementState(fuchsia_audio_device::kDefaultDaiInterconnectElementId)
      .Then([&received_callback](fidl::Result<Control::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After a failing WatchElementState call, the binding should not be usable.
  control->client()
      ->SetGain({{fuchsia_audio_device::GainState{{.gain_db = 0}}}})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  ASSERT_TRUE(control_fidl_error_status().has_value());
  EXPECT_EQ(*control_fidl_error_status(), ZX_ERR_NOT_SUPPORTED);
}

// Verify SetTopology if the driver does not support signalprocessing.
TEST_F(ControlServerStreamConfigWarningTest, SetTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->SetTopology(0).Then([&received_callback](
                                             fidl::Result<Control::SetTopology>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

// Verify SetElementState if the driver does not support signalprocessing.
TEST_F(ControlServerStreamConfigWarningTest, SetElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->SetElementState({fuchsia_audio_device::kDefaultDaiInterconnectElementId, {}})
      .Then([&received_callback](fidl::Result<Control::SetElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(registry_fidl_error_status().has_value()) << *registry_fidl_error_status();
  EXPECT_FALSE(control_fidl_error_status().has_value()) << *control_fidl_error_status();
}

}  // namespace
}  // namespace media_audio
