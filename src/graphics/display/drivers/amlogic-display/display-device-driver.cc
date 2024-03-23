// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-device-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

namespace amlogic_display {

// static
zx_status_t DisplayDeviceDriver::Create(zx_device_t* parent) {
  zx::result<std::unique_ptr<AmlogicDisplay>> amlogic_display_result =
      AmlogicDisplay::Create(parent);
  if (amlogic_display_result.is_error()) {
    zxlogf(ERROR, "Failed to create AmlogicDisplay instance: %s",
           amlogic_display_result.status_string());
    return amlogic_display_result.status_value();
  }
  std::unique_ptr<AmlogicDisplay> amlogic_display = std::move(amlogic_display_result).value();
  auto cleanup =
      fit::defer([amlogic_display = amlogic_display.get()]() { amlogic_display->Deinitialize(); });

  fbl::AllocChecker alloc_checker;
  auto display_device_driver = fbl::make_unique_checked<DisplayDeviceDriver>(
      &alloc_checker, parent, std::move(amlogic_display));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayDeviceDriver");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result<> init_result = display_device_driver->Init();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to add device: %s", init_result.status_string());
    return init_result.status_value();
  }
  // `display_device_driver` is managed by the device manager.
  [[maybe_unused]] DisplayDeviceDriver* display_device_driver_ptr = display_device_driver.release();

  cleanup.cancel();
  return ZX_OK;
}

DisplayDeviceDriver::DisplayDeviceDriver(zx_device_t* parent,
                                         std::unique_ptr<AmlogicDisplay> amlogic_display)
    : DeviceType(parent), amlogic_display_(std::move(amlogic_display)) {}

DisplayDeviceDriver::~DisplayDeviceDriver() = default;

zx::result<> DisplayDeviceDriver::Init() {
  const ddk::DeviceAddArgs args =
      ddk::DeviceAddArgs("amlogic-display")
          .set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL)
          .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
          .set_inspect_vmo(amlogic_display_->inspector().DuplicateVmo());
  return zx::make_result(DdkAdd(args));
}

void DisplayDeviceDriver::DdkRelease() {
  amlogic_display_->Deinitialize();
  delete this;
}

zx_status_t DisplayDeviceDriver::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  ZX_DEBUG_ASSERT(amlogic_display_ != nullptr);
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = amlogic_display_.get();
  switch (proto_id) {
    case ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL:
      proto->ops = amlogic_display_->display_controller_impl_protocol_ops();
      return ZX_OK;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return DisplayDeviceDriver::Create(parent); },
};

}  // namespace

}  // namespace amlogic_display

// clang-format off
ZIRCON_DRIVER(amlogic_display, amlogic_display::kDriverOps, "zircon", "0.1");
