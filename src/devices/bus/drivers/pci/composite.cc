// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/pci/composite.h"

#include <lib/ddk/binding_driver.h>

#include <bind/fuchsia/acpi/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/pci/cpp/bind.h>
#include <bind/fuchsia/hardware/sysmem/cpp/bind.h>

namespace pci {

ddk::CompositeNodeSpec CreateCompositeNodeSpec(const CompositeInfo& info) {
  auto kPciBindTopo =
      static_cast<uint32_t>(BIND_PCI_TOPO_PACK(info.bus_id, info.dev_id, info.func_id));

  const ddk::BindRule kSysmemRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_sysmem::SERVICE,
                              bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
  };

  const device_bind_prop_t kSysmemProperties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_sysmem::SERVICE,
                        bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
  };

  const ddk::BindRule kAcpiRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_acpi::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::ACPI_BUS_TYPE,
                              bind_fuchsia_acpi::BIND_ACPI_BUS_TYPE_PCI),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  const device_bind_prop_t kAcpiProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_acpi::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::ACPI_BUS_TYPE, bind_fuchsia_acpi::BIND_ACPI_BUS_TYPE_PCI),
      ddk::MakeProperty(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  const ddk::BindRule kPciRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_pci::SERVICE,
                              bind_fuchsia_hardware_pci::SERVICE_ZIRCONTRANSPORT),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_VID, info.vendor_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_DID, info.device_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_CLASS, info.class_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_SUBCLASS, info.subclass),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_INTERFACE, info.program_interface),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_REVISION, info.revision_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_TOPO, kPciBindTopo),
      ddk::MakeRejectBindRule(bind_fuchsia::COMPOSITE, static_cast<uint32_t>(1)),
  };

  const device_bind_prop_t kPciProperties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_pci::SERVICE,
                        bind_fuchsia_hardware_pci::SERVICE_ZIRCONTRANSPORT),
      ddk::MakeProperty(bind_fuchsia::PCI_VID, info.vendor_id),
      ddk::MakeProperty(bind_fuchsia::PCI_DID, info.device_id),
      ddk::MakeProperty(bind_fuchsia::PCI_CLASS, info.class_id),
      ddk::MakeProperty(bind_fuchsia::PCI_SUBCLASS, info.subclass),
      ddk::MakeProperty(bind_fuchsia::PCI_INTERFACE, info.program_interface),
      ddk::MakeProperty(bind_fuchsia::PCI_REVISION, info.revision_id),
      ddk::MakeProperty(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  auto composite_node_spec = ddk::CompositeNodeSpec(kSysmemRules, kSysmemProperties)
                                 .AddParentSpec(kPciRules, kPciProperties);
  if (info.has_acpi) {
    composite_node_spec.AddParentSpec(kAcpiRules, kAcpiProperties);
  }
  return composite_node_spec;
}

}  // namespace pci
