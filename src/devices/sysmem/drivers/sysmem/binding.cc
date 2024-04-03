// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <inttypes.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/io-buffer.h>
#include <lib/ddk/platform-defs.h>
#include <string.h>

#include <limits>
#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "device.h"
#include "driver.h"
#include "macros.h"

namespace sysmem_driver {
zx_status_t sysmem_init(void** out_driver_ctx) {
  auto driver = std::make_unique<Driver>();

  // For now at least, sysmem doesn't unload, so just release() the pointer
  // into *out_driver_ctx to remain allocated for life of this devhost
  // process.
  *out_driver_ctx = reinterpret_cast<void*>(driver.release());
  return ZX_OK;
}

zx_status_t sysmem_bind(void* driver_ctx, zx_device_t* parent_device) {
  DRIVER_DEBUG("sysmem_bind()");
  Driver* driver = reinterpret_cast<Driver*>(driver_ctx);

  auto device = std::make_unique<Device>(parent_device, driver);

  zx_status_t status = Device::Bind(std::move(device));
  if (status != ZX_OK) {
    DRIVER_ERROR("Bind() failed: %s\n", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

// -Werror=missing-field-initializers is more paranoid than I want here.
zx_driver_ops_t sysmem_driver_ops = [] {
  zx_driver_ops_t tmp{};
  tmp.version = DRIVER_OPS_VERSION;
  tmp.init = sysmem_init;
  tmp.bind = sysmem_bind;
  return tmp;
}();

}  // namespace sysmem_driver

ZIRCON_DRIVER(sysmem, sysmem_driver::sysmem_driver_ops, "zircon", "0.1");
