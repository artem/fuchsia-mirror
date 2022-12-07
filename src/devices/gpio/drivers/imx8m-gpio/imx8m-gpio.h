// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_IMX8M_GPIO_IMX8M_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_IMX8M_GPIO_IMX8M_GPIO_H_

#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <lib/mmio/mmio.h>
#include <threads.h>

#include <ddktl/device.h>
#include <fbl/array.h>
#include <fbl/vector.h>

#include "src/devices/lib/nxp/include/soc/imx8m/gpio.h"

namespace gpio {

class Imx8mGpio : public ddk::Device<Imx8mGpio, ddk::Unbindable>,
                  public ddk::GpioImplProtocol<Imx8mGpio, ddk::base_protocol> {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  Imx8mGpio(zx_device_t* parent, ddk::MmioBuffer pinconfig_mmio,
            fbl::Vector<ddk::MmioBuffer> gpio_mmios, fbl::Array<zx::interrupt> port_interrupts,
            const imx8m::PinConfigMetadata& pinconfig_metadata)
      : ddk::Device<Imx8mGpio, ddk::Unbindable>(parent),
        pinconfig_mmio_(std::move(pinconfig_mmio)),
        gpio_mmios_(std::move(gpio_mmios)),
        port_interrupts_(std::move(port_interrupts)),
        pinconfig_metadata_(pinconfig_metadata) {}
  virtual ~Imx8mGpio() = default;

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t GpioImplConfigIn(uint32_t index, uint32_t flags);
  zx_status_t GpioImplConfigOut(uint32_t index, uint8_t initial_value);
  zx_status_t GpioImplSetAltFunction(uint32_t index, uint64_t function);
  zx_status_t GpioImplSetDriveStrength(uint32_t index, uint64_t ua, uint64_t* out_actual_ua);
  zx_status_t GpioImplRead(uint32_t index, uint8_t* out_value);
  zx_status_t GpioImplWrite(uint32_t index, uint8_t value);
  zx_status_t GpioImplGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq);
  zx_status_t GpioImplReleaseInterrupt(uint32_t index);
  zx_status_t GpioImplSetPolarity(uint32_t index, gpio_polarity_t polarity);
  zx_status_t GpioImplGetDriveStrength(uint32_t index, uint64_t* out_value);

  zx_status_t Init();
  void Shutdown();

 protected:
  const ddk::MmioBuffer pinconfig_mmio_;
  const fbl::Vector<ddk::MmioBuffer> gpio_mmios_;

 private:
  zx_status_t Bind();
  int Thread();
  inline void SetInterruptPolarity(uint32_t index, bool is_high);
  inline void SetInterruptEdge(uint32_t index, bool is_edge);
  bool IsInterruptEnabled(const uint64_t port, const uint32_t pin);
  inline zx_status_t ValidateGpioPin(const uint32_t port, const uint32_t pin);
  thrd_t thread_;
  fbl::Array<zx::interrupt> port_interrupts_;
  fbl::Array<zx::interrupt> gpio_interrupts_;
  zx::port port_;
  const imx8m::PinConfigMetadata pinconfig_metadata_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_IMX8M_GPIO_IMX8M_GPIO_H_
