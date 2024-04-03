// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <memory>
#include <optional>
#include <string_view>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

inline void LogFidlClientError(fidl::UnbindInfo error, std::string tag = "") {
  if (error.status() != ZX_OK && error.status() != ZX_ERR_PEER_CLOSED) {
    FX_LOGS(WARNING) << tag << ":" << error;
  } else {
    FX_LOGS(DEBUG) << tag << ":" << error;
  }
}

// This provides shared unittest functions for AudioDeviceRegistry and the six FIDL server classes.
class AudioDeviceRegistryServerTestBase : public gtest::TestLoopFixture {
 protected:
  // Create a FakeCodec that can mock a real device that has been detected, using default settings.
  // From here, the fake Codec can be customized before it is enabled.
  std::unique_ptr<FakeCodec> CreateFakeCodecInput() { return CreateFakeCodec(true); }
  std::unique_ptr<FakeCodec> CreateFakeCodecOutput() { return CreateFakeCodec(false); }
  std::unique_ptr<FakeCodec> CreateFakeCodecNoDirection() { return CreateFakeCodec(std::nullopt); }

  // Create a FakeComposite that can mock a real device that has been detected, using default
  // settings. From here, the fake Composite can be customized before it is enabled.
  std::unique_ptr<FakeComposite> CreateFakeComposite() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    auto composite_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::Composite>();
    EXPECT_TRUE(composite_endpoints.is_ok());
    return std::make_unique<FakeComposite>(composite_endpoints->server.TakeChannel(),
                                           composite_endpoints->client.TakeChannel(), dispatcher());
  }

  // Create a FakeStreamConfig that can mock a real device that has been detected, using default
  // settings. From here, the fake StreamConfig can be customized before it is enabled.
  std::unique_ptr<FakeStreamConfig> CreateFakeStreamConfigInput() {
    return CreateFakeStreamConfig(true);
  }
  std::unique_ptr<FakeStreamConfig> CreateFakeStreamConfigOutput() {
    return CreateFakeStreamConfig(false);
  }

  // Device
  // Create a Device object (backed by a fake driver); insert it to ADR as if it had been detected.
  // Through the stream_config connection, this will communicate with the fake driver.
  void AddDeviceForDetection(std::string_view name, fuchsia_audio_device::DeviceType device_type,
                             fuchsia_audio_device::DriverClient driver_client) {
    ASSERT_TRUE(ClientIsValidForDeviceType(device_type, driver_client));
    adr_service_->AddDevice(
        Device::Create(adr_service_, dispatcher(), name, device_type, std::move(driver_client)));
  }

  class FidlHandler {
   public:
    explicit FidlHandler(AudioDeviceRegistryServerTestBase* parent) : parent_(parent) {}

   protected:
    AudioDeviceRegistryServerTestBase* parent() const { return parent_; }

   private:
    AudioDeviceRegistryServerTestBase* parent_;
  };

  // Provider support
  std::unique_ptr<TestServerAndNaturalAsyncClient<ProviderServer>> CreateTestProviderServer() {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Provider>();
    auto server = adr_service_->CreateProviderServer(std::move(server_end));
    auto client = fidl::Client<fuchsia_audio_device::Provider>(std::move(client_end), dispatcher(),
                                                               provider_fidl_handler_.get());
    return std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
        test_loop(), std::move(server), std::move(client));
  }
  class ProviderFidlHandler : public fidl::AsyncEventHandler<fuchsia_audio_device::Provider>,
                              public FidlHandler {
   public:
    explicit ProviderFidlHandler(AudioDeviceRegistryServerTestBase* parent) : FidlHandler(parent) {}
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "Provider");
      parent()->provider_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<ProviderFidlHandler> provider_fidl_handler_ =
      std::make_unique<ProviderFidlHandler>(static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> provider_fidl_error_status_;

  // Registry support
  std::unique_ptr<TestServerAndNaturalAsyncClient<RegistryServer>> CreateTestRegistryServer() {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Registry>();
    auto server = adr_service_->CreateRegistryServer(std::move(server_end));
    auto client = fidl::Client<fuchsia_audio_device::Registry>(std::move(client_end), dispatcher(),
                                                               registry_fidl_handler_.get());
    return std::make_unique<TestServerAndNaturalAsyncClient<RegistryServer>>(
        test_loop(), std::move(server), std::move(client));
  }
  class RegistryFidlHandler : public fidl::AsyncEventHandler<fuchsia_audio_device::Registry>,
                              public FidlHandler {
   public:
    explicit RegistryFidlHandler(AudioDeviceRegistryServerTestBase* parent) : FidlHandler(parent) {}
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "Registry");
      parent()->registry_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<RegistryFidlHandler> registry_fidl_handler_ =
      std::make_unique<RegistryFidlHandler>(static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> registry_fidl_error_status_;

  // ControlCreator support
  class ControlCreatorFidlHandler
      : public fidl::AsyncEventHandler<fuchsia_audio_device::ControlCreator>,
        public FidlHandler {
   public:
    explicit ControlCreatorFidlHandler(AudioDeviceRegistryServerTestBase* parent)
        : FidlHandler(parent) {}
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "ControlCreator");
      parent()->control_creator_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<ControlCreatorFidlHandler> control_creator_fidl_handler_ =
      std::make_unique<ControlCreatorFidlHandler>(
          static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> control_creator_fidl_error_status_;

  std::unique_ptr<TestServerAndNaturalAsyncClient<ControlCreatorServer>>
  CreateTestControlCreatorServer() {
    auto [client_end, server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::ControlCreator>();
    auto server = adr_service_->CreateControlCreatorServer(std::move(server_end));
    auto client = fidl::Client<fuchsia_audio_device::ControlCreator>(
        std::move(client_end), dispatcher(), control_creator_fidl_handler_.get());
    return std::make_unique<TestServerAndNaturalAsyncClient<ControlCreatorServer>>(
        test_loop(), std::move(server), std::move(client));
  }

  // Observer support
  std::unique_ptr<TestServerAndNaturalAsyncClient<ObserverServer>> CreateTestObserverServer(
      std::shared_ptr<Device> observed_device) {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
    auto server = adr_service_->CreateObserverServer(std::move(server_end), observed_device);
    auto client = fidl::Client<fuchsia_audio_device::Observer>(std::move(client_end), dispatcher(),
                                                               observer_fidl_handler_.get());
    return std::make_unique<TestServerAndNaturalAsyncClient<ObserverServer>>(
        test_loop(), std::move(server), std::move(client));
  }
  class ObserverFidlHandler : public fidl::AsyncEventHandler<fuchsia_audio_device::Observer>,
                              public FidlHandler {
   public:
    explicit ObserverFidlHandler(AudioDeviceRegistryServerTestBase* parent) : FidlHandler(parent) {}
    // Invoked when the underlying driver disconnects its Codec/StreamConfig.
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "Observer");
      parent()->observer_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<ObserverFidlHandler> observer_fidl_handler_ =
      std::make_unique<ObserverFidlHandler>(static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> observer_fidl_error_status_;

  // Control support
  std::unique_ptr<TestServerAndNaturalAsyncClient<ControlServer>> CreateTestControlServer(
      std::shared_ptr<Device> device_to_control) {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Control>();
    auto server = adr_service_->CreateControlServer(std::move(server_end), device_to_control);
    FX_CHECK(server) << "ControlServer is NULL";
    auto client = fidl::Client<fuchsia_audio_device::Control>(std::move(client_end), dispatcher(),
                                                              control_fidl_handler_.get());
    return std::make_unique<TestServerAndNaturalAsyncClient<ControlServer>>(
        test_loop(), std::move(server), std::move(client));
  }
  class ControlFidlHandler : public fidl::AsyncEventHandler<fuchsia_audio_device::Control>,
                             public FidlHandler {
   public:
    explicit ControlFidlHandler(AudioDeviceRegistryServerTestBase* parent) : FidlHandler(parent) {}
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "Control");
      parent()->control_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<ControlFidlHandler> control_fidl_handler_ =
      std::make_unique<ControlFidlHandler>(static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> control_fidl_error_status_;

  // RingBuffer support
  class RingBufferFidlHandler : public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer>,
                                public FidlHandler {
   public:
    explicit RingBufferFidlHandler(AudioDeviceRegistryServerTestBase* parent)
        : FidlHandler(parent) {}
    void on_fidl_error(fidl::UnbindInfo error) override {
      LogFidlClientError(error, "RingBuffer");
      parent()->ring_buffer_fidl_error_status_ = error.status();
    }
  };
  std::unique_ptr<RingBufferFidlHandler> ring_buffer_fidl_handler_ =
      std::make_unique<RingBufferFidlHandler>(
          static_cast<AudioDeviceRegistryServerTestBase*>(this));
  std::optional<zx_status_t> ring_buffer_fidl_error_status_;

  // General members
  std::shared_ptr<FidlThread> server_thread_ =
      FidlThread::CreateFromCurrentThread("test_server_thread", dispatcher());

  std::shared_ptr<media_audio::AudioDeviceRegistry> adr_service_ =
      std::make_shared<media_audio::AudioDeviceRegistry>(server_thread_);

 private:
  // Create a FakeStreamConfig that can mock a real device that has been detected, using default
  // settings. From here, the fake StreamConfig can be customized before it is enabled.
  std::unique_ptr<FakeStreamConfig> CreateFakeStreamConfig(bool is_input = false) {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    auto stream_config_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::StreamConfig>();
    EXPECT_TRUE(stream_config_endpoints.is_ok());
    auto fake_stream = std::make_unique<FakeStreamConfig>(
        stream_config_endpoints->server.TakeChannel(),
        stream_config_endpoints->client.TakeChannel(), dispatcher());
    fake_stream->set_is_input(is_input);
    return fake_stream;
  }

  std::unique_ptr<FakeCodec> CreateFakeCodec(std::optional<bool> is_input = false) {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    auto codec_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::Codec>();
    EXPECT_TRUE(codec_endpoints.is_ok());
    auto fake_codec = std::make_unique<FakeCodec>(
        codec_endpoints->server.TakeChannel(), codec_endpoints->client.TakeChannel(), dispatcher());
    fake_codec->set_is_input(is_input);
    return fake_codec;
  }
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_
