// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "include/lib/virtio/driver_utils.h"

#include <lib/device-protocol/pci.h>
#include <lib/zx/result.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <memory>

#include "include/lib/virtio/backends/pci.h"

namespace virtio {

zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> GetBtiAndBackend(
    zx_device_t* bus_device) {
  ddk::Pci pci(bus_device, "pci");
  if (!pci.is_valid()) {
    pci = ddk::Pci(bus_device);
  }

  if (!pci.is_valid()) {
    zxlogf(ERROR, "virtio failed to find PciProtocol");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  return GetBtiAndBackend(std::move(pci));
}

zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> GetBtiAndBackend(ddk::Pci pci) {
  zx_status_t status;
  if (!pci.is_valid()) {
    zxlogf(ERROR, "ddk::Pci invalid");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  fuchsia_hardware_pci::wire::DeviceInfo info;
  status = pci.GetDeviceInfo(&info);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  zx::bti bti;
  status = pci.GetBti(0, &bti);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // Due to the similarity between Virtio 0.9.5 legacy devices and Virtio 1.0
  // transitional devices we need to check whether modern capabilities exist.
  // If no vendor capabilities are found then we will default to the legacy
  // interface.
  std::unique_ptr<virtio::Backend> backend = nullptr;
  uint8_t offset = 0;
  bool is_modern =
      (pci.GetFirstCapability(fuchsia_hardware_pci::CapabilityId::kVendor, &offset) == ZX_OK);
  if (is_modern) {
    backend = std::make_unique<virtio::PciModernBackend>(std::move(pci), info);
  } else {
    backend = std::make_unique<virtio::PciLegacyBackend>(std::move(pci), info);
  }
  zxlogf(TRACE, "virtio %02x:%02x.%1x using %s PCI backend", info.bus_id, info.dev_id, info.func_id,
         (is_modern) ? "modern" : "legacy");

  status = backend->Bind();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::make_pair(std::move(bti), std::move(backend)));
}

}  // namespace virtio
