// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_AML_SPI_TESTS_AML_SPI_TEST_ENV_H_
#define SRC_DEVICES_SPI_DRIVERS_AML_SPI_TESTS_AML_SPI_TEST_ENV_H_

#include <endian.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
#include <lib/fake-bti/bti.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>

#include <fake-mmio-reg/fake-mmio-reg.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/spi/drivers/aml-spi/aml-spi.h"
#include "src/devices/spi/drivers/aml-spi/registers.h"
#include "src/lib/testing/predicates/status.h"

namespace spi {

class TestAmlSpiDriver : public AmlSpiDriver {
 public:
  TestAmlSpiDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : AmlSpiDriver(std::move(start_args), std::move(dispatcher)),
        mmio_region_(sizeof(uint32_t), 17) {}

  static DriverRegistration GetDriverRegistration() {
    // Use a custom DriverRegistration to create the DUT. Without this, the non-test
    // implementation will be used by default.
    return FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<TestAmlSpiDriver>::initialize,
                                          fdf_internal::DriverServer<TestAmlSpiDriver>::destroy);
  }

  ddk_fake::FakeMmioRegRegion& mmio() { return mmio_region_; }

  uint32_t conreg() const { return conreg_; }
  uint32_t enhance_cntl() const { return enhance_cntl_; }
  uint32_t testreg() const { return testreg_; }

 protected:
  fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) override {
    return fpromise::make_promise([this]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
      // Set the transfer complete bit so the driver doesn't get stuck waiting on the interrupt.
      mmio_region_[AML_SPI_STATREG].SetReadCallback(
          []() { return StatReg::Get().FromValue(0).set_tc(1).set_te(1).set_rr(1).reg_value(); });

      mmio_region_[AML_SPI_CONREG].SetWriteCallback([this](uint32_t value) { conreg_ = value; });
      mmio_region_[AML_SPI_CONREG].SetReadCallback([this]() { return conreg_; });
      mmio_region_[AML_SPI_ENHANCE_CNTL].SetWriteCallback(
          [this](uint32_t value) { enhance_cntl_ = value; });
      mmio_region_[AML_SPI_TESTREG].SetWriteCallback([this](uint32_t value) { testreg_ = value; });

      return fpromise::ok(mmio_region_.GetMmioBuffer());
    });
  }

 private:
  ddk_fake::FakeMmioRegRegion mmio_region_;
  uint32_t conreg_{};
  uint32_t enhance_cntl_{};
  uint32_t testreg_{};
};

class FakePDev : public fidl::testing::WireTestBase<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler(
      async_dispatcher_t* dispatcher) {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
    });
  }

  void set_interrupt(zx::interrupt interrupt) { interrupt_ = std::move(interrupt); }

  void set_bti(zx::bti bti) { bti_ = std::move(bti); }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}

  void GetInterruptById(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByIdRequest* request,
      GetInterruptByIdCompleter::Sync& completer) override {
    if (request->index != 0 || !interrupt_) {
      return completer.ReplyError(ZX_ERR_NOT_FOUND);
    }

    zx::interrupt out_interrupt;
    zx_status_t status = interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_interrupt);
    if (status == ZX_OK) {
      completer.ReplySuccess(std::move(out_interrupt));
    } else {
      completer.ReplyError(status);
    }
  }

  void GetBtiById(fuchsia_hardware_platform_device::wire::DeviceGetBtiByIdRequest* request,
                  GetBtiByIdCompleter::Sync& completer) override {
    if (request->index != 0 || !bti_) {
      return completer.ReplyError(ZX_ERR_NOT_FOUND);
    }

    zx::bti out_bti;
    zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_bti);
    if (status == ZX_OK) {
      completer.ReplySuccess(std::move(out_bti));
    } else {
      completer.ReplyError(status);
    }
  }

  zx::interrupt interrupt_;
  zx::bti bti_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;
};

class BaseTestEnvironment : public fdf_testing::Environment {
 public:
  BaseTestEnvironment()
      : fdf_testing::Environment(), registers_(fdf::Dispatcher::GetCurrent()->async_dispatcher()) {}

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    SetUpInterrupt();
    SetUpBti();

    auto result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_server_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()), "pdev");
    if (!result.is_ok()) {
      return result.take_error();
    }

    SetMetadata(compat_);
    compat_.Init("pdev", {});
    EXPECT_OK(compat_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));

    // Serve a second compat instance at default in order to satisfy AmlSpiDriver's compat
    // server. Without this, metadata doesn't get forwarded.
    compat_default_.Init("default", {});
    EXPECT_OK(
        compat_default_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));

    result = to_driver_vfs.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                      "gpio-cs-2");
    if (result.is_error()) {
      return result.take_error();
    }

    result = to_driver_vfs.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                      "gpio-cs-3");
    if (result.is_error()) {
      return result.take_error();
    }

    result = to_driver_vfs.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                      "gpio-cs-5");
    if (result.is_error()) {
      return result.take_error();
    }

    gpio_.SetCurrentState(fake_gpio::State{.polarity = fuchsia_hardware_gpio::GpioPolarity::kHigh,
                                           .sub_state = fake_gpio::WriteSubState{.value = 0}});
    gpio_.SetWriteCallback([this](fake_gpio::FakeGpio& gpio) {
      if (gpio_writes_.empty()) {
        EXPECT_FALSE(gpio_writes_.empty());
        return ZX_ERR_INTERNAL;
      }
      auto [status, value] = gpio_writes_.front();
      gpio_writes_.pop();
      if (status != ZX_OK) {
        EXPECT_EQ(value, gpio_.GetWriteValue());
      }
      return status;
    });

    if (SetupResetRegister()) {
      auto result = to_driver_vfs.AddService<fuchsia_hardware_registers::Service>(
          registers_.GetInstanceHandler(), "reset");
      if (result.is_error()) {
        return result.take_error();
      }
    }

    registers_.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
    return zx::ok();
  }

  virtual void SetUpInterrupt() {
    ASSERT_OK(zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &interrupt_));
    zx::interrupt dut_interrupt;
    ASSERT_OK(interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dut_interrupt));
    pdev_server_.set_interrupt(std::move(dut_interrupt));
    interrupt_.trigger(0, zx::clock::get_monotonic());
  }

  virtual void SetUpBti() {}

  virtual bool SetupResetRegister() { return true; }

  virtual void SetMetadata(compat::DeviceServer& compat) {
    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }

  void ExpectGpioWrite(zx_status_t status, uint8_t value) { gpio_writes_.emplace(status, value); }

  void VerifyGpioAndClear() {
    EXPECT_EQ(gpio_writes_.size(), 0u);
    gpio_writes_ = {};
  }

  bool ControllerReset() {
    zx_status_t status = registers_.VerifyAll();
    if (status == ZX_OK) {
      // Always keep a single expectation in the queue, that way we can verify when the controller
      // is not reset.
      registers_.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
    }

    return status == ZX_OK;
  }

 protected:
  static constexpr amlogic_spi::amlspi_config_t kSpiConfig = {
      .bus_id = 0,
      .cs_count = 3,
      .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
      .clock_divider_register_value = 0,
      .use_enhanced_clock_mode = false,
  };

  FakePDev pdev_server_;
  zx::interrupt interrupt_;

  mock_registers::MockRegisters registers_;
  std::queue<std::pair<zx_status_t, uint8_t>> gpio_writes_;
  fake_gpio::FakeGpio gpio_;

  compat::DeviceServer compat_;
  compat::DeviceServer compat_default_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_AML_SPI_TESTS_AML_SPI_TEST_ENV_H_
