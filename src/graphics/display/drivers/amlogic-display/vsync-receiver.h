// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/irq.h>
#include <lib/ddk/device.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>

#include <memory>
#include <optional>

#include <fbl/mutex.h>

namespace amlogic_display {

// Receives Vertical Sync (Vsync) interrupts triggered by the display engine
// indicating that the display engine finishes presenting a frame to the
// display device.
class VsyncReceiver {
 public:
  // Internal state size for the function called when a Vsync interrupt is
  // triggered.
  static constexpr size_t kOnVsyncTargetSize = 16;

  // The type of the function called when a Vsync interrupt is triggered.
  using VsyncHandler = fit::inline_function<void(zx::time timestamp), kOnVsyncTargetSize>;

  // Factory method intended for production use.
  //
  // `parent` must be non-null.
  //
  // `platform_device` must be valid.
  //
  // `on_vsync` is called when the display engine finishes presenting a frame
  // to the display device and triggers a Vsync interrupt. Must be non-null.
  static zx::result<std::unique_ptr<VsyncReceiver>> Create(
      zx_device_t* parent,
      fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device,
      VsyncHandler on_vsync);

  // Production code should prefer the factory method `Create()`.
  //
  // `vsync_irq` must be valid.
  // `on_vsync` must be non-null.
  explicit VsyncReceiver(zx::interrupt vsync_irq, VsyncHandler on_vsync);

  VsyncReceiver(const VsyncReceiver&) = delete;
  VsyncReceiver& operator=(const VsyncReceiver&) = delete;

  ~VsyncReceiver();

  // Initialization work that is not suitable for the constructor.
  //
  // Called by Create().
  //
  // `parent` must be non-null.
  zx::result<> Init(zx_device_t* parent);

 private:
  void OnVsync(zx::time timestamp);

  void InterruptHandler(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                        const zx_packet_interrupt_t* interrupt);

  const zx::interrupt vsync_irq_;

  const VsyncHandler on_vsync_;

  const async_loop_config_t irq_handler_loop_config_;

  // The `irq_handler_loop_`, `irq_handler_` and `irq_handler_thread_` are
  // constant between Init() and instance destruction. Only accessed on the
  // threads used for class initialization and destruction.
  async::Loop irq_handler_loop_;
  async::IrqMethod<VsyncReceiver, &VsyncReceiver::InterruptHandler> irq_handler_{this};
  thrd_t irq_handler_thread_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_
