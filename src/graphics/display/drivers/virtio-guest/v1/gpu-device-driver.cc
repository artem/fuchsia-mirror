// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-device-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstring>
#include <memory>
#include <utility>

#include <ddktl/device.h>
#include <ddktl/init-txn.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/virtio-guest/v1/display-engine.h"

namespace virtio_display {

// static
zx_status_t GpuDeviceDriver::Create(zx_device_t* parent) {
  char flag[32];
  zx_status_t status =
      device_get_variable(parent, "driver.virtio-gpu.disable", flag, sizeof(flag), nullptr);
  // If gpu disabled:
  if (status == ZX_OK && (!std::strncmp(flag, "1", 2) || !std::strncmp(flag, "true", 5) ||
                          !std::strncmp(flag, "on", 3))) {
    zxlogf(INFO, "driver.virtio-gpu.disabled=1, not binding to the GPU");
    return ZX_ERR_NOT_FOUND;
  }

  zx::result<std::unique_ptr<DisplayEngine>> display_engine_result = DisplayEngine::Create(parent);
  if (display_engine_result.is_error()) {
    // DisplayEngine::Create() logs on error.
    return display_engine_result.error_value();
  }

  fbl::AllocChecker alloc_checker;
  auto driver = fbl::make_unique_checked<GpuDeviceDriver>(&alloc_checker, parent,
                                                          std::move(display_engine_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for GpuDeviceDriver");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result<> init_result = driver->Init();
  if (init_result.is_error()) {
    // DisplayEngine::Init() logs on error.
    return init_result.error_value();
  }

  // devmgr now owns the memory for `device`.
  [[maybe_unused]] auto device_ptr = driver.release();
  return ZX_OK;
}

GpuDeviceDriver::GpuDeviceDriver(zx_device_t* bus_device,
                                 std::unique_ptr<DisplayEngine> display_engine)
    : DdkDeviceType(bus_device), display_engine_(std::move(display_engine)) {
  ZX_DEBUG_ASSERT(display_engine_);
}

GpuDeviceDriver::~GpuDeviceDriver() {
  if (start_thread_.joinable()) {
    start_thread_.join();
  }
}

zx::result<> GpuDeviceDriver::Init() {
  zx_status_t status = DdkAdd(
      ddk::DeviceAddArgs("virtio-gpu-display").set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device node: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t GpuDeviceDriver::DdkGetProtocol(uint32_t proto_id, void* out) {
  return display_engine_->DdkGetProtocol(proto_id, out);
}

void GpuDeviceDriver::DdkInit(ddk::InitTxn txn) {
  start_thread_ = std::thread([this, init_txn = std::move(txn)]() mutable {
    zx_status_t status = display_engine_->Start();
    init_txn.Reply(status);
  });
}

void GpuDeviceDriver::DdkRelease() { delete this; }

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return GpuDeviceDriver::Create(parent); },
};

}  // namespace

}  // namespace virtio_display

ZIRCON_DRIVER(virtio_gpu, virtio_display::kDriverOps, "zircon", "0.1");
