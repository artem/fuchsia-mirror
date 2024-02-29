// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv2.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

#include <bind/fuchsia/broadcom/platform/cpp/bind.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/serial/drivers/aml-uart/tests/device_state.h"

static constexpr serial_port_info_t kSerialInfo = {
    .serial_class = fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci),
    .serial_vid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_VID_BROADCOM,
    .serial_pid = bind_fuchsia_broadcom_platform::BIND_PLATFORM_DEV_PID_BCM43458,
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

    pdev_server_.SetConfig(std::move(config));

    // Add pdev.
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    constexpr std::string_view kInstanceName = "pdev";
    zx::result add_service_result =
        to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
            pdev_server_.GetInstanceHandler(dispatcher), kInstanceName);
    ZX_ASSERT(add_service_result.is_ok());

    // Configure and add compat.
    compat_server_.Init("default", "topo");
    compat_server_.AddMetadata(DEVICE_METADATA_SERIAL_PORT_INFO, &kSerialInfo, sizeof(kSerialInfo));
    return zx::make_result(compat_server_.Serve(dispatcher, &to_driver_vfs));
  }

  DeviceState& device_state() { return state_; }

 private:
  DeviceState state_;
  fake_pdev::FakePDevFidl pdev_server_;
  compat::DeviceServer compat_server_;
};

class AmlUartTestConfig {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = serial::AmlUartV2;
  using EnvironmentType = Environment;
};

class AmlUartHarness : public fdf_testing::DriverTestFixture<AmlUartTestConfig> {
 public:
  serial::AmlUart& Device() { return driver()->aml_uart_for_testing(); }
};

TEST_F(AmlUartHarness, SerialImplAsyncGetInfo) {
  serial_port_info_t info;
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncGetInfo(&info));
  ASSERT_EQ(info.serial_class, fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci));
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
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncEnable(false));
  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  static constexpr uint32_t serial_test_config =
      SERIAL_DATA_BITS_6 | SERIAL_STOP_BITS_2 | SERIAL_PARITY_EVEN | SERIAL_FLOW_CTRL_CTS_RTS;
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncConfig(20, serial_test_config));

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), SERIAL_DATA_BITS_6);
    ASSERT_EQ(env.device_state().StopBits(), SERIAL_STOP_BITS_2);
    ASSERT_EQ(env.device_state().Parity(), SERIAL_PARITY_EVEN);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncConfig(40, SERIAL_SET_BAUD_RATE_ONLY));

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), SERIAL_DATA_BITS_6);
    ASSERT_EQ(env.device_state().StopBits(), SERIAL_STOP_BITS_2);
    ASSERT_EQ(env.device_state().Parity(), SERIAL_PARITY_EVEN);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  ASSERT_NE(ZX_OK, Device().SerialImplAsyncConfig(0, serial_test_config));
  ASSERT_NE(ZX_OK, Device().SerialImplAsyncConfig(UINT32_MAX, serial_test_config));
  ASSERT_NE(ZX_OK, Device().SerialImplAsyncConfig(1, serial_test_config));

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), SERIAL_DATA_BITS_6);
    ASSERT_EQ(env.device_state().StopBits(), SERIAL_STOP_BITS_2);
    ASSERT_EQ(env.device_state().Parity(), SERIAL_PARITY_EVEN);
    ASSERT_TRUE(env.device_state().FlowControl());
  });

  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncConfig(40, SERIAL_SET_BAUD_RATE_ONLY));

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().DataBits(), SERIAL_DATA_BITS_6);
    ASSERT_EQ(env.device_state().StopBits(), SERIAL_STOP_BITS_2);
    ASSERT_EQ(env.device_state().Parity(), SERIAL_PARITY_EVEN);
    ASSERT_TRUE(env.device_state().FlowControl());
  });
}

TEST_F(AmlUartHarness, SerialImplAsyncEnable) {
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncEnable(false));

  RunInEnvironmentTypeContext([](Environment& env) {
    ASSERT_EQ(env.device_state().Control().tx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().rx_enable(), 0u);
    ASSERT_EQ(env.device_state().Control().inv_cts(), 0u);
  });

  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncEnable(true));

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

TEST_F(AmlUartHarness, SerialImplReadAsync) {
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncEnable(true));
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status, const uint8_t* buffer, size_t bufsz) {
    auto context = static_cast<Context*>(ctx);
    EXPECT_EQ(bufsz, kDataLen);
    EXPECT_EQ(memcmp(buffer, context->data, bufsz), 0);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncReadAsync(cb, &context);

  RunInEnvironmentTypeContext(
      [&context](Environment& env) { env.device_state().Inject(context.data, kDataLen); });
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
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

TEST_F(AmlUartHarness, SerialImplWriteAsync) {
  ASSERT_EQ(ZX_OK, Device().SerialImplAsyncEnable(true));
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status) {
    auto context = static_cast<Context*>(ctx);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncWriteAsync(context.data, kDataLen, cb, &context);
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);

  RunInEnvironmentTypeContext([&context](Environment& env) {
    auto buf = env.device_state().TxBuf();
    ASSERT_EQ(buf.size(), kDataLen);
    ASSERT_EQ(memcmp(buf.data(), context.data, buf.size()), 0);
  });
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

TEST_F(AmlUartHarness, SerialImplAsyncWriteDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status) {
    auto context = static_cast<Context*>(ctx);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncWriteAsync(context.data, kDataLen, cb, &context);
  Device().HandleTXRaceForTest();
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);

  RunInEnvironmentTypeContext([&context](Environment& env) {
    auto buf = env.device_state().TxBuf();
    ASSERT_EQ(buf.size(), kDataLen);
    ASSERT_EQ(memcmp(buf.data(), context.data, buf.size()), 0);
  });
}

TEST_F(AmlUartHarness, SerialImplAsyncReadDoubleCallback) {
  // NOTE: we don't start the IRQ thread.  The Handle*RaceForTest() enable.
  struct Context {
    uint8_t data[kDataLen];
    sync_completion_t completion;
  } context;
  for (size_t i = 0; i < kDataLen; i++) {
    context.data[i] = static_cast<uint8_t>(i);
  }
  auto cb = [](void* ctx, zx_status_t status, const uint8_t* buffer, size_t bufsz) {
    auto context = static_cast<Context*>(ctx);
    EXPECT_EQ(bufsz, kDataLen);
    EXPECT_EQ(memcmp(buffer, context->data, bufsz), 0);
    sync_completion_signal(&context->completion);
  };
  Device().SerialImplAsyncReadAsync(cb, &context);

  RunInEnvironmentTypeContext(
      [&context](Environment& env) { env.device_state().Inject(context.data, kDataLen); });
  Device().HandleRXRaceForTest();
  sync_completion_wait(&context.completion, ZX_TIME_INFINITE);
}
