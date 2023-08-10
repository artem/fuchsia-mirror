// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DRIVER_UTILS_H_
#define SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DRIVER_UTILS_H_

#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zx/result.h>
#include <stdlib.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <type_traits>

#include "device.h"

namespace virtio {
// Get the bti and virtio backend for a given pci virtio device.
zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> GetBtiAndBackend(
    zx_device_t* bus_device);

zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> GetBtiAndBackend(ddk::Pci pci);

// Creates a Virtio device by determining the backend and moving that into
// |VirtioDevice|'s constructor, then call's the device's Init() method. The
// device's Init() is expected to call DdkAdd. On success, ownership of the device
// is released to devmgr.
template <class VirtioDevice, class = typename std::enable_if<
                                  std::is_base_of<virtio::Device, VirtioDevice>::value>::type>
zx_status_t CreateAndBind(void* /*ctx*/, zx_device_t* device) {
  auto bti_and_backend = GetBtiAndBackend(device);
  if (!bti_and_backend.is_ok()) {
    return bti_and_backend.status_value();
  }
  auto dev = std::make_unique<VirtioDevice>(device, std::move(bti_and_backend.value().first),
                                            std::move(bti_and_backend.value().second));
  zx_status_t status = dev->Init();
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

}  // namespace virtio
#endif  // SRC_DEVICES_BUS_LIB_VIRTIO_INCLUDE_LIB_VIRTIO_DRIVER_UTILS_H_
