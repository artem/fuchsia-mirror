// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
#include <endian.h>
#include <fuchsia/hardware/pciroot/c/banjo.h>
#include <inttypes.h>
#include <lib/ddk/debug.h>
#include <lib/pci/pciroot.h>
#include <lib/pci/pio.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/resource.h>
#include <zircon/types.h>

#include <array>
#include <memory>

#include <acpica/acpi.h>
#include <ddktl/device.h>

#include "src/devices/board/lib/acpi/device.h"
#include "src/devices/board/lib/acpi/pci-internal.h"
#include "src/devices/lib/iommu/iommu.h"

zx_status_t AcpiPciroot::PcirootGetBti(uint32_t bdf, uint32_t index, zx::bti* bti) {
  // x86 uses PCI BDFs as hardware identifiers, and ARM uses PCI root complexes. There will be at
  // most one BTI per device.
  if (index != 0) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto iommu = context_.iommu->IommuForPciDevice(bdf);
  const zx_status_t status = zx::bti::create(*iommu, 0, bdf, bti);
  if (status == ZX_OK) {
    char name[ZX_MAX_NAME_LEN]{};
    snprintf(name, std::size(name) - 1, "acpi bti %02x:%02x.%1x", (bdf >> 8) & 0xFF,
             (bdf >> 3) & 0x1F, bdf & 0x7);
    const zx_status_t name_status = bti->set_property(ZX_PROP_NAME, name, std::size(name));
    if (name_status != ZX_OK) {
      zxlogf(WARNING, "Couldn't set name for BTI '%s': %s", name,
             zx_status_get_string(name_status));
    }
  }

  return status;
}

zx_status_t AcpiPciroot::PcirootGetPciPlatformInfo(pci_platform_info_t* info) {
  *info = context_.info;
  info->irq_routing_list = context_.routing.data();
  info->irq_routing_count = context_.routing.size();
  info->acpi_bdfs_list = acpi_bdfs_.data();
  info->acpi_bdfs_count = acpi_bdfs_.size();

  return ZX_OK;
}

zx_status_t AcpiPciroot::PcirootReadConfig8(const pci_bdf_t* address, uint16_t offset,
                                            uint8_t* value) {
  return pci_pio_read8(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::PcirootReadConfig16(const pci_bdf_t* address, uint16_t offset,
                                             uint16_t* value) {
  return pci_pio_read16(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::PcirootReadConfig32(const pci_bdf_t* address, uint16_t offset,
                                             uint32_t* value) {
  return pci_pio_read32(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::PcirootWriteConfig8(const pci_bdf_t* address, uint16_t offset,
                                             uint8_t value) {
  return pci_pio_write8(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::PcirootWriteConfig16(const pci_bdf_t* address, uint16_t offset,
                                              uint16_t value) {
  return pci_pio_write16(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::PcirootWriteConfig32(const pci_bdf_t* address, uint16_t offset,
                                              uint32_t value) {
  return pci_pio_write32(*address, static_cast<uint8_t>(offset), value);
}

zx_status_t AcpiPciroot::Create(PciRootHost* root_host, AcpiPciroot::Context ctx,
                                zx_device_t* parent, const char* name,
                                std::vector<pci_bdf_t> acpi_bdfs) {
  auto pciroot = new AcpiPciroot(root_host, std::move(ctx), parent, name, std::move(acpi_bdfs));
  return pciroot->DdkAdd(
      ddk::DeviceAddArgs(name).set_inspect_vmo(pciroot->inspect().DuplicateVmo()));
}
