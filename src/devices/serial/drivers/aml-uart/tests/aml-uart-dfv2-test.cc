// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv2.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/serial/drivers/aml-uart/tests/device_state.h"
#include "src/devices/serial/drivers/aml-uart/tests/fake_timer.h"

static constexpr fuchsia_hardware_serial::wire::SerialPortInfo kSerialInfo = {
    .serial_class = fuchsia_hardware_serial::Class::kBluetoothHci,
    .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
    .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
};

class FakeSystemActivityGovernor : public fidl::Server<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor() = default;

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event::create(0, &wake_handling_);
    zx::event duplicate;
    wake_handling_.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate);

    fuchsia_power_system::WakeHandling wake_handling = {
        {.active_dependency_token = std::move(duplicate)}};

    elements = {{.wake_handling = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& request,
                        RegisterListenerCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  zx::event wake_handling_;
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;
};

class FakeLeaseControl : public fidl::Server<fuchsia_power_broker::LeaseControl> {
 public:
  void WatchStatus(fuchsia_power_broker::LeaseControlWatchStatusRequest& request,
                   WatchStatusCompleter::Sync& completer) override {
    completer.Reply(lease_status_);
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::LeaseControl> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fuchsia_power_broker::LeaseStatus lease_status_ = fuchsia_power_broker::LeaseStatus::kSatisfied;
};

class FakeLessor : public fidl::Server<fuchsia_power_broker::Lessor> {
 public:
  void Lease(fuchsia_power_broker::LessorLeaseRequest& request,
             LeaseCompleter::Sync& completer) override {
    auto lease_control_endpoints = fidl::CreateEndpoints<fuchsia_power_broker::LeaseControl>();
    lease_control_binding_.emplace(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(),
        std::move(lease_control_endpoints->server), &lease_control_,
        [this](fidl::UnbindInfo info) mutable { lease_requested_ = false; });
    lease_requested_ = true;
    completer.Reply(fit::success(std::move(lease_control_endpoints->client)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  bool GetLeaseRequested() { return lease_requested_; }

 private:
  bool lease_requested_ = false;
  FakeLeaseControl lease_control_;
  std::optional<fidl::ServerBinding<fuchsia_power_broker::LeaseControl>> lease_control_binding_;
};

class FakeRequiredLevel : public fidl::Server<fuchsia_power_broker::RequiredLevel> {
 public:
  void Watch(WatchCompleter::Sync& completer) override {
    ZX_ASSERT(!completer_);
    completer_.emplace(completer.ToAsync());
  }

  void SetRequiredLevel(fuchsia_power_broker::PowerLevel level) {
    // Make sure driver is watching the required level before calling this function.
    ZX_ASSERT(completer_);
    completer_->Reply(fit::success(level));
    completer_.reset();
  }

  bool WatchReceived() { return completer_.has_value(); }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::RequiredLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  std::optional<WatchCompleter::Async> completer_;
};

class FakeCurrentLevel : public fidl::Server<fuchsia_power_broker::CurrentLevel> {
 public:
  void Update(fuchsia_power_broker::CurrentLevelUpdateRequest& request,
              UpdateCompleter::Sync& completer) override {
    current_level_ = request.current_level();
    completer.Reply(fit::success());
  }

  fuchsia_power_broker::PowerLevel current_level() { return current_level_; }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::CurrentLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fuchsia_power_broker::PowerLevel current_level_ = serial::AmlUart::kPowerLevelOff;
};

class FakePowerBroker : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  fidl::ProtocolHandler<fuchsia_power_broker::Topology> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void AddElement(fuchsia_power_broker::ElementSchema& request,
                  AddElementCompleter::Sync& completer) override {
    auto element_control = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();
    element_control_server_ = std::move(element_control->server);
    if (request.lessor_channel()) {
      fidl::BindServer<fuchsia_power_broker::Lessor>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(*request.lessor_channel()),
          &wake_lessor_);
    }

    if (request.level_control_channels()) {
      fidl::BindServer<fuchsia_power_broker::RequiredLevel>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(),
          std::move(request.level_control_channels()->required()), &required_level_);
      fidl::BindServer<fuchsia_power_broker::CurrentLevel>(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(),
          std::move(request.level_control_channels()->current()), &current_level_);
    }

    fuchsia_power_broker::TopologyAddElementResponse result{
        {.element_control_channel = std::move(element_control->client)},
    };

    completer.Reply(fit::success(std::move(result)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fidl::ServerEnd<fuchsia_power_broker::ElementControl>& element_control_server() {
    return element_control_server_;
  }

  FakeRequiredLevel& required_level() { return required_level_; }
  FakeCurrentLevel& current_level() { return current_level_; }
  bool GetLeaseRequested() { return wake_lessor_.GetLeaseRequested(); }

 private:
  FakeLessor wake_lessor_;
  FakeRequiredLevel required_level_;
  FakeCurrentLevel current_level_;
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_server_;
  fidl::ServerBindingGroup<fuchsia_power_broker::Topology> bindings_;
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    // Configure pdev.
    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    EXPECT_EQ(ZX_OK,
              zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    state_.set_irq_signaller(config.irqs[0].borrow());
    config.mmios[0] = state_.GetMmio();

    // Add power configuration to config.
    constexpr uint8_t kPowerLevelOff =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOff);
    constexpr uint8_t kPowerLevelHandling =
        static_cast<uint8_t>(fuchsia_power_broker::BinaryPowerLevel::kOn);
    constexpr char kPowerElementName[] = "aml-uart-wake-on-interrupt";
    fuchsia_hardware_power::LevelTuple wake_handling_on = {{
        .child_level = kPowerLevelHandling,
        .parent_level = static_cast<uint8_t>(fuchsia_power_system::WakeHandlingLevel::kActive),
    }};
    fuchsia_hardware_power::PowerDependency wake_handling = {{
        .child = kPowerElementName,
        .parent = fuchsia_hardware_power::ParentElement::WithSag(
            fuchsia_hardware_power::SagElement::kWakeHandling),
        .level_deps = {{std::move(wake_handling_on)}},
        .strength = fuchsia_hardware_power::RequirementType::kActive,
    }};
    fuchsia_hardware_power::PowerLevel off = {{.level = kPowerLevelOff, .name = "off"}};
    fuchsia_hardware_power::PowerLevel on = {{.level = kPowerLevelHandling, .name = "on"}};
    fuchsia_hardware_power::PowerElement element = {
        {.name = kPowerElementName, .levels = {{std::move(off), std::move(on)}}}};
    fuchsia_hardware_power::PowerElementConfiguration wake_config = {
        {.element = std::move(element), .dependencies = {{std::move(wake_handling)}}}};
    config.power_elements.push_back(wake_config);

    pdev_server_.SetConfig(std::move(config));

    // Add pdev.
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    constexpr std::string_view kInstanceName = "pdev";
    zx::result add_service_result =
        to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
            pdev_server_.GetInstanceHandler(dispatcher), kInstanceName);
    ZX_ASSERT(add_service_result.is_ok());

    // Add power protocols.
    auto result_sag =
        to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_power_system::ActivityGovernor>(
            system_activity_governor_.CreateHandler());
    EXPECT_EQ(ZX_OK, result_sag.status_value());
    auto result_broker =
        to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_power_broker::Topology>(
            power_broker_.CreateHandler());
    EXPECT_EQ(ZX_OK, result_broker.status_value());

    // Configure and add compat.
    compat_server_.Init("default", "topo");

    fit::result encoded = fidl::Persist(kSerialInfo);
    ZX_ASSERT(encoded.is_ok());

    compat_server_.AddMetadata(DEVICE_METADATA_SERIAL_PORT_INFO, encoded->data(), encoded->size());
    return zx::make_result(compat_server_.Serve(dispatcher, &to_driver_vfs));
  }

  DeviceState& device_state() { return state_; }
  FakePowerBroker& power_broker() { return power_broker_; }

 private:
  DeviceState state_;
  fake_pdev::FakePDevFidl pdev_server_;
  FakeSystemActivityGovernor system_activity_governor_;
  FakePowerBroker power_broker_;
  compat::DeviceServer compat_server_;
};

class AmlUartTestConfig {
 public:
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = serial::AmlUartV2;
  using EnvironmentType = Environment;
};

class AmlUartAsyncTestConfig {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = serial::AmlUartV2;
  using EnvironmentType = Environment;
};

class AmlUartHarness : public fdf_testing::DriverTestFixture<AmlUartTestConfig> {
 public:
  void SetUp() override {
    zx::result result = StartDriverCustomized([&](fdf::DriverStartArgs& args) {
      aml_uart_dfv2_config::Config fake_config;
      fake_config.enable_suspend() = false;
      args.config(fake_config.ToVmo());
    });

    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf::WireSyncClient<fuchsia_hardware_serialimpl::Device> CreateClient() {
    zx::result driver_connect_result =
        Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
    if (driver_connect_result.is_error()) {
      return {};
    }
    return fdf::WireSyncClient(std::move(driver_connect_result.value()));
  }
};

class AmlUartAsyncHarness : public fdf_testing::DriverTestFixture<AmlUartAsyncTestConfig> {
 public:
  void SetUp() override {
    zx::result result = StartDriverCustomized([&](fdf::DriverStartArgs& args) {
      aml_uart_dfv2_config::Config fake_config;
      fake_config.enable_suspend() = false;
      args.config(fake_config.ToVmo());
    });

    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf::WireClient<fuchsia_hardware_serialimpl::Device> CreateClient() {
    zx::result driver_connect_result =
        Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
    if (driver_connect_result.is_error()) {
      return {};
    }
    return fdf::WireClient(std::move(driver_connect_result.value()),
                           fdf::Dispatcher::GetCurrent()->get());
  }

  serial::AmlUart& Device() { return driver()->aml_uart_for_testing(); }
};

class AmlUartHarnessWithPower : public AmlUartHarness {
 public:
  void SetUp() override {
    zx::result result = StartDriverCustomized([&](fdf::DriverStartArgs& args) {
      aml_uart_dfv2_config::Config fake_config;
      fake_config.enable_suspend() = true;
      args.config(fake_config.ToVmo());
    });

    ASSERT_EQ(ZX_OK, result.status_value());
  }
};

class AmlUartAsyncHarnessWithPower : public AmlUartAsyncHarness {
 public:
  void SetUp() override {
    zx::result result = StartDriverCustomized([&](fdf::DriverStartArgs& args) {
      aml_uart_dfv2_config::Config fake_config;
      fake_config.enable_suspend() = true;
      args.config(fake_config.ToVmo());
    });

    ASSERT_EQ(ZX_OK, result.status_value());
  }
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');
  auto result = client.buffer(arena)->GetInfo();
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());

  const auto& info = result->value()->info;
  ASSERT_EQ(info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
  ASSERT_EQ(info.serial_pid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
  ASSERT_EQ(info.serial_vid, bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
}

TEST_F(AmlUartHarness, SerialImplAsyncGetInfoFromDriverService) {
  zx::result driver_connect_result =
      Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('INFO');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());
  device_client.buffer(arena)->GetInfo().Then(
      [quit = runtime().QuitClosure()](
          fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::GetInfo>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        ASSERT_EQ(res->info.serial_class, fuchsia_hardware_serial::Class::kBluetoothHci);
        ASSERT_EQ(res->info.serial_pid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458);
        ASSERT_EQ(res->info.serial_vid,
                  bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM);
        quit();
      });
  runtime().Run();
}

TEST_F(AmlUartHarness, SerialImplAsyncConfig) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  {
    auto result = client.buffer(arena)->Enable(false);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  static constexpr uint32_t serial_test_config = fuchsia_hardware_serialimpl::kSerialDataBits6 |
                                                 fuchsia_hardware_serialimpl::kSerialStopBits2 |
                                                 fuchsia_hardware_serialimpl::kSerialParityEven |
                                                 fuchsia_hardware_serialimpl::kSerialFlowCtrlCtsRts;
  {
    auto result = client.buffer(arena)->Config(20, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result =
        client.buffer(arena)->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result = client.buffer(arena)->Config(0, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  {
    auto result = client.buffer(arena)->Config(UINT32_MAX, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  {
    auto result = client.buffer(arena)->Config(1, serial_test_config);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  {
    auto result =
        client.buffer(arena)->Config(40, fuchsia_hardware_serialimpl::kSerialSetBaudRateOnly);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), fuchsia_hardware_serialimpl::kSerialDataBits6);
    ASSERT_EQ(env.device_state().StopBits(), fuchsia_hardware_serialimpl::kSerialStopBits2);
    ASSERT_EQ(env.device_state().Parity(), fuchsia_hardware_serialimpl::kSerialParityEven);
    ASSERT_TRUE(env.device_state().FlowControl());
  });
}

TEST_F(AmlUartHarness, SerialImplAsyncEnable) {
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  {
    auto result = client.buffer(arena)->Enable(false);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  {
    auto result = client.buffer(arena)->Enable(true);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  }

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 1u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 1u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
    ASSERT_TRUE(env.device_state().PortResetRX());
    ASSERT_TRUE(env.device_state().PortResetTX());
    ASSERT_FALSE(env.device_state().Control().rst_rx());
    ASSERT_FALSE(env.device_state().Control().rst_tx());
    ASSERT_TRUE(env.device_state().Control().tx_interrupt_enable());
    ASSERT_TRUE(env.device_state().Control().rx_interrupt_enable());
  });
}

TEST_F(AmlUartHarness, SerialImplReadDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('READ');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = runtime().QuitClosure()](auto& res) { quit(); });
  runtime().Run();
  runtime().ResetQuit();

  device_client.buffer(arena)->Read().Then(
      [quit = runtime().QuitClosure(),
       data](fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Read>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());

        auto res = result.value().value();
        EXPECT_EQ(res->data.count(), kDataLen);
        EXPECT_EQ(memcmp(data, res->data.data(), res->data.count()), 0);
        quit();
      });

  RunInEnvironmentTypeContext(
      [&data](Environment& env) { env.device_state().Inject(data, kDataLen); });
  runtime().Run();
}

TEST_F(AmlUartHarness, SerialImplWriteDriverService) {
  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('WRIT');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = runtime().QuitClosure()](auto& res) { quit(); });
  runtime().Run();
  runtime().ResetQuit();

  device_client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(data, kDataLen))
      .Then([quit = runtime().QuitClosure()](
                fdf::WireUnownedResult<fuchsia_hardware_serialimpl::Device::Write>& result) {
        ASSERT_EQ(ZX_OK, result.status());
        ASSERT_TRUE(result.value().is_ok());
        quit();
      });
  runtime().Run();

  RunInEnvironmentTypeContext([&data](Environment& env) {
    auto buf = env.device_state().TxBuf();
    ASSERT_EQ(buf.size(), kDataLen);
    ASSERT_EQ(memcmp(buf.data(), data, buf.size()), 0);
  });
}

TEST_F(AmlUartAsyncHarness, SerialImplAsyncWriteDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  bool write_complete = false;
  client.buffer(arena)
      ->Write(fidl::VectorView<uint8_t>::FromExternal(expected_data.data(), kDataLen))
      .ThenExactlyOnce([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        write_complete = true;
      });
  runtime().RunUntilIdle();
  Device().HandleTXRaceForTest();
  runtime().RunUntil([&]() { return write_complete; });

  RunInEnvironmentTypeContext([expected_data = std::move(expected_data)](Environment& env) {
    EXPECT_EQ(expected_data, env.device_state().TxBuf());
  });
}

TEST_F(AmlUartAsyncHarness, SerialImplAsyncReadDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  auto client = CreateClient();

  fdf::Arena arena('TEST');

  std::vector<uint8_t> expected_data;
  for (size_t i = 0; i < kDataLen; i++) {
    expected_data.push_back(static_cast<uint8_t>(i));
  }

  client.buffer(arena)->Read().ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    const std::vector actual_data(result->value()->data.cbegin(), result->value()->data.cend());
    EXPECT_EQ(expected_data, actual_data);
    runtime().Quit();
  });
  runtime().RunUntilIdle();

  RunInEnvironmentTypeContext(
      [&](Environment& env) { env.device_state().Inject(expected_data.data(), kDataLen); });
  Device().HandleRXRaceForTest();
  runtime().Run();
}

TEST_F(AmlUartHarnessWithPower, PowerElementControl) {
  zx_info_handle_basic_t broker_element_control, driver_element_control;

  RunInDriverContext([&](serial::AmlUartV2& driver) {
    zx_status_t status =
        driver.aml_uart_for_testing().element_control_for_testing().channel().get_info(
            ZX_INFO_HANDLE_BASIC, &driver_element_control, sizeof(zx_info_handle_basic_t), nullptr,
            nullptr);
    ASSERT_EQ(status, ZX_OK);
  });

  RunInEnvironmentTypeContext([&](Environment& env) {
    zx_status_t status = env.power_broker().element_control_server().channel().get_info(
        ZX_INFO_HANDLE_BASIC, &broker_element_control, sizeof(zx_info_handle_basic_t), nullptr,
        nullptr);
    ASSERT_EQ(status, ZX_OK);
  });
  ASSERT_EQ(broker_element_control.koid, driver_element_control.related_koid);
}

TEST_F(AmlUartHarnessWithPower, AcquireWakeLeaseWithRead) {
  FakeTimer fake_timer;

  // Inject the fake timer handle to driver. FakeTimer::timer_handle_ will replace the handle
  // under the driver timer.
  RunInDriverContext([&](serial::AmlUartV2& driver) {
    driver.aml_uart_for_testing().InjectTimerForTest(FakeTimer::timer_handle_);
  });

  uint8_t data[kDataLen];
  for (size_t i = 0; i < kDataLen; i++) {
    data[i] = static_cast<uint8_t>(i);
  }

  zx::result driver_connect_result =
      Connect<fuchsia_hardware_serialimpl::Service::Device>("aml-uart");
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());
  fdf::Arena arena('READ');
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> device_client(
      std::move(driver_connect_result.value()), fdf::Dispatcher::GetCurrent()->get());

  device_client.buffer(arena)->Enable(true).Then(
      [quit = runtime().QuitClosure()](auto& res) { quit(); });
  runtime().Run();
  runtime().ResetQuit();

  // Verify that no lease has been acquired. Trigger an interrupt.
  RunInEnvironmentTypeContext([&data](Environment& env) {
    ASSERT_FALSE(env.power_broker().GetLeaseRequested());
    env.device_state().Inject(data, kDataLen);
  });

  // Verify that the timer deadline has been set by the driver. Save the deadline for later
  // comparison.
  runtime().RunUntil([&]() { return FakeTimer::current_deadline_ != 0; });
  zx_time_t last_deadline = FakeTimer::current_deadline_;

  RunInEnvironmentTypeContext(
      [](Environment& env) { ASSERT_TRUE(env.power_broker().GetLeaseRequested()); });

  RunInEnvironmentTypeContext(
      [&data](Environment& env) { env.device_state().Inject(data, kDataLen); });

  // // Verify that the timer was canceled and set again by driver, also the wake lease was not
  // // dropped.
  runtime().RunUntil([&]() { return FakeTimer::cancel_called_; });
  runtime().RunUntil([&]() {
    return FakeTimer::current_deadline_ != 0 && FakeTimer::current_deadline_ != last_deadline;
  });

  RunInEnvironmentTypeContext(
      [](Environment& env) { ASSERT_TRUE(env.power_broker().GetLeaseRequested()); });

  // Fire the timer and verify that the wake lease has been dropped.
  fake_timer.FireTimer();
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<bool>(
               [](Environment& env) { return env.power_broker().GetLeaseRequested(); }) == false;
  });
}

TEST_F(AmlUartHarnessWithPower, PowerLevelUpdate) {
  // Wait until we have received the Watch.
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<bool>(
        [](Environment& env) { return env.power_broker().required_level().WatchReceived(); });
  });

  // The driver sets the CurrentLevel to the RequiredLevel kPowerLevelHandling.
  RunInEnvironmentTypeContext([](Environment& env) {
    env.power_broker().required_level().SetRequiredLevel(serial::AmlUart::kPowerLevelHandling);
  });
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<fuchsia_power_broker::PowerLevel>([](Environment& env) {
             return env.power_broker().current_level().current_level();
           }) == serial::AmlUart::kPowerLevelHandling;
  });

  // Wait until we have received the Watch.
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<bool>(
        [](Environment& env) { return env.power_broker().required_level().WatchReceived(); });
  });

  // The driver sets the CurrentLevel to the RequiredLevel kPowerLevelOff.
  RunInEnvironmentTypeContext([](Environment& env) {
    env.power_broker().required_level().SetRequiredLevel(serial::AmlUart::kPowerLevelOff);
  });
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<fuchsia_power_broker::PowerLevel>([](Environment& env) {
             return env.power_broker().current_level().current_level();
           }) == 0;
  });

  // Wait until we have received the Watch.
  runtime().RunUntil([&]() {
    return RunInEnvironmentTypeContext<bool>(
        [](Environment& env) { return env.power_broker().required_level().WatchReceived(); });
  });
}
