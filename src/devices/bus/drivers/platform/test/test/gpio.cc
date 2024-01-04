// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>

#include <memory>

#include <ddktl/device.h>

#define DRIVER_NAME "test-gpio"

namespace gpio {

class TestGpioDevice;
using DeviceType = ddk::Device<TestGpioDevice>;

class TestGpioDevice : public DeviceType,
                       public ddk::GpioImplProtocol<TestGpioDevice, ddk::base_protocol> {
 public:
  static zx_status_t Create(zx_device_t* parent);

  explicit TestGpioDevice(zx_device_t* parent) : DeviceType(parent) {}

  zx_status_t Create(std::unique_ptr<TestGpioDevice>* out);

  // Methods required by the ddk mixins
  void DdkRelease();

  zx_status_t GpioImplConfigIn(uint32_t pin, uint32_t flags);
  zx_status_t GpioImplConfigOut(uint32_t pin, uint8_t initial_value);
  zx_status_t GpioImplSetAltFunction(uint32_t pin, uint64_t function);
  zx_status_t GpioImplRead(uint32_t pin, uint8_t* out_value);
  zx_status_t GpioImplWrite(uint32_t pin, uint8_t value);
  zx_status_t GpioImplGetInterrupt(uint32_t pin, uint32_t flags, zx::interrupt* out_irq);
  zx_status_t GpioImplReleaseInterrupt(uint32_t pin);
  zx_status_t GpioImplSetPolarity(uint32_t pin, uint32_t polarity);
  zx_status_t GpioImplSetDriveStrength(uint32_t pin, uint64_t ua, uint64_t* out_actual_ua);
  zx_status_t GpioImplGetDriveStrength(uint32_t pin, uint64_t* result);

 private:
  static constexpr uint32_t PIN_COUNT = 10;

  // values for our pins
  bool pins_[PIN_COUNT] = {};
  uint64_t drive_strengths_[PIN_COUNT] = {};
};

zx_status_t TestGpioDevice::Create(zx_device_t* parent) {
  auto dev = std::make_unique<TestGpioDevice>(parent);
  pdev_protocol_t pdev;
  zx_status_t status;

  zxlogf(INFO, "TestGpioDevice::Create: %s ", DRIVER_NAME);

  status = device_get_protocol(parent, ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not get ZX_PROTOCOL_PDEV", __func__);
    return status;
  }

  status = dev->DdkAdd(
      ddk::DeviceAddArgs("test-gpio").forward_metadata(parent, DEVICE_METADATA_GPIO_PINS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed: %d", __func__, status);
    return status;
  }
  // devmgr is now in charge of dev.
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

void TestGpioDevice::DdkRelease() { delete this; }

zx_status_t TestGpioDevice::GpioImplConfigIn(uint32_t pin, uint32_t flags) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplConfigOut(uint32_t pin, uint8_t initial_value) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplSetAltFunction(uint32_t pin, uint64_t function) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplRead(uint32_t pin, uint8_t* out_value) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  *out_value = pins_[pin];
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplWrite(uint32_t pin, uint8_t value) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  pins_[pin] = value;
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplGetInterrupt(uint32_t pin, uint32_t flags,
                                                 zx::interrupt* out_irq) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplReleaseInterrupt(uint32_t pin) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplSetPolarity(uint32_t pin, uint32_t polarity) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplSetDriveStrength(uint32_t pin, uint64_t ua,
                                                     uint64_t* out_actual_ua) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }

  drive_strengths_[pin] = ua;

  if (out_actual_ua) {
    *out_actual_ua = drive_strengths_[pin];
  }
  return ZX_OK;
}

zx_status_t TestGpioDevice::GpioImplGetDriveStrength(uint32_t pin, uint64_t* result) {
  if (pin >= PIN_COUNT) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (result) {
    *result = drive_strengths_[pin];
  }

  return ZX_OK;
}

zx_status_t test_gpio_bind(void* ctx, zx_device_t* parent) {
  return TestGpioDevice::Create(parent);
}

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = test_gpio_bind;
  return driver_ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(test_gpio, gpio::driver_ops, "zircon", "0.1");
