// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_GPIO_TEST_GPIO_TEST_H_
#define SRC_DEVICES_GPIO_DRIVERS_GPIO_TEST_GPIO_TEST_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/zx/interrupt.h>
#include <threads.h>

#include <ddktl/device.h>
#include <fbl/array.h>

namespace gpio_test {

class GpioTest;
using GpioTestType = ddk::Device<GpioTest>;

class GpioTest : public GpioTestType {
 public:
 public:
  explicit GpioTest(zx_device_t* parent) : GpioTestType(parent) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation.
  void DdkRelease();

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(GpioTest);

  // GPIO indices
  enum {
    GPIO_LED,
    GPIO_BUTTON,
  };

  zx_status_t Init();
  int OutputThread();
  zx_status_t InterruptThread();

  fbl::Array<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpios_;

  uint32_t gpio_count_;
  thrd_t output_thread_;
  thrd_t interrupt_thread_;
  bool done_;
  zx::interrupt interrupt_;
};

}  // namespace gpio_test

#endif  // SRC_DEVICES_GPIO_DRIVERS_GPIO_TEST_GPIO_TEST_H_
