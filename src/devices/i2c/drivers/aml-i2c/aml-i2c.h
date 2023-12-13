// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/time.h>
#include <threads.h>

#include "soc/aml-common/aml-i2c.h"

namespace aml_i2c {

class AmlI2c : public ddk::I2cImplProtocol<AmlI2c> {
 public:
  // Create an `AmlI2c` object and return a pointer to it. The return type is a
  // pointer to `AmlI2c` and not just `AmlI2c` because `AmlI2c` is not copyable.
  static zx::result<std::unique_ptr<AmlI2c>> Create(ddk::PDevFidl& pdev,
                                                    const aml_i2c_delay_values& delay);

  AmlI2c(zx::interrupt irq, zx::event event, fdf::MmioBuffer regs_iobuff)
      : irq_(std::move(irq)), event_(std::move(event)), regs_iobuff_(std::move(regs_iobuff)) {
    StartIrqThread();
  }

  ~AmlI2c() {
    irq_.destroy();
    if (irqthrd_) {
      thrd_join(irqthrd_, nullptr);
    }
  }

  zx_status_t I2cImplGetMaxTransferSize(uint64_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bitrate);
  zx_status_t I2cImplTransact(const i2c_impl_op_t* rws, size_t count);

  thrd_t irqthrd() const { return irqthrd_; }

  void* get_ops() { return &i2c_impl_protocol_ops_; }

  void SetTimeout(zx::duration timeout) { timeout_ = timeout; }

 private:
  static zx_status_t SetClockDelay(const aml_i2c_delay_values& delay,
                                   const fdf::MmioBuffer& regs_iobuff);

  void SetTargetAddr(uint16_t addr) const;
  void StartXfer() const;
  zx_status_t WaitTransferComplete() const;

  zx_status_t Read(uint8_t* buff, uint32_t len, bool stop) const;
  zx_status_t Write(const uint8_t* buff, uint32_t len, bool stop) const;

  void StartIrqThread();
  int IrqThread() const;

  const zx::interrupt irq_;
  const zx::event event_;
  const fdf::MmioBuffer regs_iobuff_;
  zx::duration timeout_ = zx::sec(1);
  thrd_t irqthrd_{};
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
