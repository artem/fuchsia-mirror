// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/pci/device.h"

#include <assert.h>
#include <err.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <inttypes.h>
#include <lib/fit/defer.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/zx/interrupt.h>
#include <string.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <array>
#include <optional>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_buffer.h>
#include <pretty/sizes.h>

#include "src/devices/bus/drivers/pci/allocation.h"
#include "src/devices/bus/drivers/pci/bus_device_interface.h"
#include "src/devices/bus/drivers/pci/capabilities/msi.h"
#include "src/devices/bus/drivers/pci/capabilities/msix.h"
#include "src/devices/bus/drivers/pci/capabilities/power_management.h"
#include "src/devices/bus/drivers/pci/common.h"
#include "src/devices/bus/drivers/pci/pci_bind.h"
#include "src/devices/bus/drivers/pci/ref_counted.h"
#include "src/devices/bus/drivers/pci/upstream_node.h"

namespace pci {

namespace {  // anon namespace.  Externals do not need to know about DeviceImpl

class DeviceImpl : public Device {
 public:
  static zx_status_t Create(zx_device_t* parent, std::unique_ptr<Config>&& cfg,
                            UpstreamNode* upstream, BusDeviceInterface* bdi, inspect::Node node,
                            bool has_acpi);

  // Implement ref counting, do not let derived classes override.
  PCI_IMPLEMENT_REFCOUNTED;

  // Disallow copying, assigning and moving.
  DISALLOW_COPY_ASSIGN_AND_MOVE(DeviceImpl);

 protected:
  DeviceImpl(zx_device_t* parent, std::unique_ptr<Config>&& cfg, UpstreamNode* upstream,
             BusDeviceInterface* bdi, inspect::Node node, bool has_acpi)
      : Device(parent, std::move(cfg), upstream, bdi, std::move(node), /*is_bridge=*/false,
               has_acpi) {}
};

zx_status_t DeviceImpl::Create(zx_device_t* parent, std::unique_ptr<Config>&& cfg,
                               UpstreamNode* upstream, BusDeviceInterface* bdi, inspect::Node node,
                               bool has_acpi) {
  fbl::AllocChecker ac;
  auto raw_dev =
      new (&ac) DeviceImpl(parent, std::move(cfg), upstream, bdi, std::move(node), has_acpi);
  if (!ac.check()) {
    zxlogf(ERROR, "[%s] Out of memory attemping to create PCIe device.", cfg->addr());
    return ZX_ERR_NO_MEMORY;
  }

  auto dev = fbl::AdoptRef(static_cast<Device*>(raw_dev));
  zx_status_t status = raw_dev->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "[%s] Failed to initialize PCIe device: %s", dev->config()->addr(),
           zx_status_get_string(status));
    return status;
  }

  bdi->LinkDevice(dev);
  return ZX_OK;
}

}  // namespace

Device::Device(zx_device_t* parent, std::unique_ptr<Config>&& config, UpstreamNode* upstream,
               BusDeviceInterface* bdi, inspect::Node node, bool is_bridge, bool has_acpi)
    : cfg_(std::move(config)),
      upstream_(upstream),
      bdi_(bdi),
      bar_count_(is_bridge ? PCI_BAR_REGS_PER_BRIDGE : PCI_BAR_REGS_PER_DEVICE),
      is_bridge_(is_bridge),
      has_acpi_(has_acpi),
      parent_(parent) {
  metrics_.node = std::move(node);
  metrics_.legacy.node = metrics_.node.CreateChild(kInspectLegacyInterrupt);
  metrics_.msi.node = metrics_.node.CreateChild(kInspectMsi);

  metrics_.irq_mode =
      metrics_.node.CreateString(kInspectIrqMode, kInspectIrqModes[PCI_INTERRUPT_MODE_DISABLED]);
  uint8_t pin = cfg_->Read(Config::kInterruptPin);
  switch (pin) {
    case 1:
    case 2:
    case 3:
    case 4: {
      // register values 1-4 map to pins A-D
      char s[2] = {static_cast<char>('A' + (pin - 1)), '\0'};
      metrics_.legacy.pin = metrics_.legacy.node.CreateString(kInspectLegacyInterruptPin, s);
      break;
    }
  }
  // Line should always exist if a pin exists, unless there was no mapping in the _PRT.
  uint8_t line = cfg_->Read(Config::kInterruptLine);
  if (line != 0 && line != 0xFF) {
    metrics_.legacy.line = metrics_.legacy.node.CreateUint(kInspectLegacyInterruptLine, line);
  }
  metrics_.legacy.ack_count = metrics_.legacy.node.CreateUint(kInspectLegacyAckCount, 0);
  metrics_.legacy.signal_count = metrics_.legacy.node.CreateUint(kInspectLegacySignalCount, 0);
  metrics_.legacy.disabled = metrics_.legacy.node.CreateBool(kInspectLegacyDisabled, false);
  metrics_.msi.base_vector = metrics_.msi.node.CreateUint(kInspectMsiBaseVector, 0);
  metrics_.msi.allocated = metrics_.msi.node.CreateUint(kInspectMsiAllocated, 0);
}

Device::~Device() {
  // We should already be unlinked from the bus's device tree.
  ZX_DEBUG_ASSERT(disabled_);
  ZX_DEBUG_ASSERT(!plugged_in_);

  // Make certain that all bus access (MMIO, PIO, Bus mastering) has been
  // disabled and disable IRQs.
  DisableInterrupts();
  SetBusMastering(false);
  ModifyCmd(/*clr_bits=*/PCI_CONFIG_COMMAND_IO_EN | PCI_CONFIG_COMMAND_MEM_EN, /*set_bits=*/0);
  // TODO(cja/fxbug.dev/32979): Remove this after porting is finished.
  zxlogf(TRACE, "%s [%s] dtor finished", is_bridge() ? "bridge" : "device", cfg_->addr());
}

zx_status_t Device::Create(zx_device_t* parent, std::unique_ptr<Config>&& config,
                           UpstreamNode* upstream, BusDeviceInterface* bdi, inspect::Node node,
                           bool has_acpi) {
  return DeviceImpl::Create(parent, std::move(config), upstream, bdi, std::move(node), has_acpi);
}

zx_status_t Device::Init() {
  fbl::AutoLock dev_lock(&dev_lock_);

  zx_status_t status = InitLocked();
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to initialize device %s: %d", cfg_->addr(), status);
    return status;
  }

  // Things went well and the device is in a good state. Flag the device as
  // plugged in and link ourselves up to the graph. This will keep the device
  // alive as long as the Bus owns it.
  upstream_->LinkDevice(this);
  plugged_in_ = true;

  return status;
}

zx_status_t Device::InitInterrupts() {
  zx_status_t status = zx::interrupt::create(*zx::unowned_resource(ZX_HANDLE_INVALID), 0,
                                             ZX_INTERRUPT_VIRTUAL, &irqs_.legacy);
  if (status != ZX_OK) {
    zxlogf(ERROR, "device %s could not create its legacy interrupt: %s", cfg_->addr(),
           zx_status_get_string(status));
    return status;
  }

  // Disable all interrupt modes until a driver enables the preferred method.
  // The legacy interrupt is disabled by hand because our Enable/Disable methods
  // for doing so need to interact with the Shared IRQ lists in Bus.
  ModifyCmdLocked(/*clr_bits=*/0, /*set_bits=*/PCIE_CFG_COMMAND_INT_DISABLE);
  irqs_.legacy_vector = 0;

  if (caps_.msi && (status = DisableMsi()) != ZX_OK) {
    zxlogf(ERROR, "failed to disable MSI: %s", zx_status_get_string(status));
    return status;
  }

  if (caps_.msix && (status = DisableMsix()) != ZX_OK) {
    zxlogf(ERROR, "failed to disable MSI-X: %s", zx_status_get_string(status));
    return status;
  }

  irqs_.mode = PCI_INTERRUPT_MODE_DISABLED;
  return ZX_OK;
}

zx_status_t Device::InitLocked() {
  // Cache basic device info
  vendor_id_ = cfg_->Read(Config::kVendorId);
  device_id_ = cfg_->Read(Config::kDeviceId);
  class_id_ = cfg_->Read(Config::kBaseClass);
  subclass_ = cfg_->Read(Config::kSubClass);
  prog_if_ = cfg_->Read(Config::kProgramInterface);
  rev_id_ = cfg_->Read(Config::kRevisionId);

  // Disable the device in event of a failure initializing. TA is disabled
  // because it cannot track the scope of AutoCalls and their associated
  // locking semantics. The lock is grabbed by |Init| and held at this point.
  auto disable = fit::defer([this]() __TA_NO_THREAD_SAFETY_ANALYSIS { DisableLocked(); });

  // Parse and sanity check the capabilities and extended capabilities lists
  // if they exist
  zx_status_t st = ProbeCapabilities();
  if (st != ZX_OK) {
    zxlogf(ERROR, "device %s encountered an error parsing capabilities: %d", cfg_->addr(), st);
    return st;
  }

  ProbeBars();

  // Now that we know what our capabilities are, initialize our internal IRQ
  // bookkeeping and disable all interrupts until a driver requests them.
  st = InitInterrupts();
  if (st != ZX_OK) {
    return st;
  }

  // Power the device on by transitioning to the highest power level if possible
  // and necessary.
  if (caps_.power) {
    if (auto state = caps_.power->GetPowerState(*cfg_);
        state != PowerManagementCapability::PowerState::D0) {
      zxlogf(DEBUG, "[%s] transitioning power state from D%d to D0", cfg_->addr(), state);
      caps_.power->SetPowerState(*cfg_, PowerManagementCapability::PowerState::D0);
    }
  }

  auto result = BanjoDevice::Create(parent_, this);
  if (result.is_error()) {
    return result.status_value();
  }

  result = FidlDevice::Create(parent_, this);
  if (result.is_error()) {
    return result.status_value();
  }

  disable.cancel();
  return ZX_OK;
}

zx::status<> Device::Configure() {
  // Some capabilities can only be configured after device BARs have been
  // configured, and device BARs cannot be configured when a Device object is
  // created since bridge windows still need to be allocated.  If BAR allocation
  // fails then from the perspective of driver binding we will consider the
  // device unbindable. If it it came out of the bootloader already enabled,
  // with existing BAR allocations then we'll just let it continue operating.
  // The most common situation we encounter like this is with a debug uart that
  // the kernel has already carved out an address space reservation for.
  bool had_configure_errors = false;
  if (auto result = AllocateBars(); result.is_error()) {
    zxlogf(ERROR, "[%s] failed to allocate bars: %s", cfg_->addr(), result.status_string());
    had_configure_errors = true;
  }

  // Capability parsing may fail if BAR allocation failed. For example, if an
  // MSI-X BAR for some reason is owned by the kernel (as unlikely as that
  // seems). Similarly to BARs, we won't bail out here, but we will note that no
  // driver should use the device.
  if (auto result = ConfigureCapabilities(); result.is_error()) {
    zxlogf(ERROR, "[%s] failed to configure capabilities: %s", cfg_->addr(),
           result.status_string());
    had_configure_errors = true;
  }

  // If a device is already running then we'll leave it on in case some function
  // coming out of the bootloader is in use by the kernel.
  if (had_configure_errors) {
    if (Enabled()) {
      zxlogf(DEBUG, "[%s] leaving device enabled despite configuration errors", cfg_->addr());
      return zx::ok();
    }

    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok();
}

zx_status_t Device::ModifyCmd(uint16_t clr_bits, uint16_t set_bits) {
  fbl::AutoLock dev_lock(&dev_lock_);
  // In order to keep internal bookkeeping coherent, and interactions between
  // MSI/MSI-X and Legacy IRQ mode safe, API users may not directly manipulate
  // the legacy IRQ enable/disable bit.  Just ignore them if they try to
  // manipulate the bit via the modify cmd API.
  // TODO(cja) This only applies to PCI(e)
  clr_bits = static_cast<uint16_t>(clr_bits & ~PCIE_CFG_COMMAND_INT_DISABLE);
  set_bits = static_cast<uint16_t>(set_bits & ~PCIE_CFG_COMMAND_INT_DISABLE);

  if (plugged_in_) {
    ModifyCmdLocked(clr_bits, set_bits);
    return ZX_OK;
  }

  return ZX_ERR_UNAVAILABLE;
}

// Performs a read-modify-write on the device's Command register, but only
// performs the write if the resulting modified value differs from the value
// read from the register. Returns the previous value so the caller can know
// the state that existed prior.
uint16_t Device::ModifyCmdLocked(uint16_t clr_bits, uint16_t set_bits) {
  uint16_t prev_value = cfg_->Read(Config::kCommand);
  uint16_t new_value = static_cast<uint16_t>((prev_value & ~clr_bits) | set_bits);
  if (new_value != prev_value) {
    cfg_->Write(Config::kCommand, new_value);
  }
  return prev_value;
}

void Device::Disable() {
  fbl::AutoLock dev_lock(&dev_lock_);
  DisableLocked();
}

void Device::DisableLocked() {
  // Flag the device as disabled.  Close the device's MMIO/PIO windows, shut
  // off device initiated accesses to the bus, disable legacy interrupts.
  // Basically, prevent the device from doing anything from here on out.
  ModifyCmdLocked(
      PCI_CONFIG_COMMAND_BUS_MASTER_EN | PCI_CONFIG_COMMAND_IO_EN | PCI_CONFIG_COMMAND_MEM_EN,
      PCI_CONFIG_COMMAND_INT_DISABLE);

  // Release all BAR allocations back into the pool they came from now that
  // we've disabled all IO access.
  for (auto& bar : bars_) {
    bar.reset();
  }

  disabled_ = true;
  zxlogf(TRACE, "[%s] %s disabled", cfg_->addr(), (is_bridge()) ? " (b) " : "");
}

zx_status_t Device::SetBusMastering(bool enabled) {
  // Only allow bus mastering to be turned off if the device is disabled.
  if (enabled && disabled_) {
    return ZX_ERR_BAD_STATE;
  }

  ModifyCmdLocked(enabled ? /*clr_bits=*/0 : /*set_bits=*/PCI_CONFIG_COMMAND_BUS_MASTER_EN,
                  enabled ? /*clr_bits=*/PCI_CONFIG_COMMAND_BUS_MASTER_EN : /*set_bits=*/0);
  return upstream_->SetBusMasteringUpstream(enabled);
}

// Configures the BAR represented by |bar| by writing to its register and configuring
// IO and Memory access accordingly.
zx_status_t Device::WriteBarInformation(const Bar& bar) {
  cfg_->Write(Config::kBar(bar.bar_id), static_cast<uint32_t>(bar.address));
  if (bar.is_64bit) {
    uint32_t addr_hi = static_cast<uint32_t>(bar.address >> 32);
    cfg_->Write(Config::kBar(bar.bar_id + 1), addr_hi);
  }
  return ZX_OK;
}

zx::status<> Device::ProbeBar(uint8_t bar_id) {
  ZX_DEBUG_ASSERT(bar_id < bar_count_);
  // Disable MMIO & PIO access while we perform the probe. We don't want the
  // addresses written during probing to conflict with anything else on the
  // bus. Note: No drivers should have access to this device's registers
  // during the probe process as the device should not have been published
  // yet. That said, there could be other (special case) parts of the system
  // accessing a devices registers at this point in time, like an early init
  // debug console or serial port.
  DeviceIoDisable io_disable(this);

  Bar bar{};
  uint32_t bar_val = cfg_->Read(Config::kBar(bar_id));
  bar.bar_id = bar_id;
  bar.is_mmio = (bar_val & PCI_BAR_IO_TYPE_MASK) == PCI_BAR_IO_TYPE_MMIO;
  bar.is_64bit = bar.is_mmio && ((bar_val & PCI_BAR_MMIO_TYPE_MASK) == PCI_BAR_MMIO_TYPE_64BIT);
  bar.is_prefetchable = bar.is_mmio && (bar_val & PCI_BAR_MMIO_PREFETCH_MASK);
  uint32_t addr_mask = (bar.is_mmio) ? PCI_BAR_MMIO_ADDR_MASK : PCI_BAR_PIO_ADDR_MASK;

  // Check the read-only configuration of the BAR. If it's invalid then don't add it to our BAR
  // list.
  if (bar.is_64bit && (bar.bar_id == bar_count_ - 1)) {
    zxlogf(ERROR, "[%s] has a 64bit bar in invalid position %u!", cfg_->addr(), bar.bar_id);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (bar.is_64bit && !bar.is_mmio) {
    zxlogf(ERROR, "[%s] bar %u is 64bit but not mmio!", cfg_->addr(), bar.bar_id);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (io_disable.io_was_enabled()) {
    // For enabled devices save the original address in the BAR. If the device
    // is enabled then we should assume the bios configured it and we should
    // attempt to retain the BAR allocation.
    bar.address = bar_val & addr_mask;
  }

  // Write ones to figure out the size of the BAR
  cfg_->Write(Config::kBar(bar_id), UINT32_MAX);
  bar_val = cfg_->Read(Config::kBar(bar_id));
  // BARs that are not wired up return all zeroes on read after probing.
  if (bar_val == 0) {
    return zx::ok();
  }

  uint64_t size_mask = ~(bar_val & addr_mask);
  if (bar.is_mmio && bar.is_64bit) {
    // Retain the high 32bits of the 64bit address address if the device was
    // enabled already.
    if (io_disable.io_was_enabled()) {
      bar.address |= static_cast<uint64_t>(cfg_->Read(Config::kBar(bar_id + 1))) << 32;
    }

    // Get the high 32 bits of size for the 64 bit BAR by repeating the
    // steps of writing 1s and then reading the value of the next BAR.
    cfg_->Write(Config::kBar(bar_id + 1), UINT32_MAX);
    size_mask |= static_cast<uint64_t>(~cfg_->Read(Config::kBar(bar_id + 1))) << 32;
  } else if (!bar.is_mmio && !(bar_val & (UINT16_MAX << 16))) {
    // Per spec, if the type is IO and the upper 16 bits were zero in the read
    // then they should be removed from the size mask before incrementing it.
    size_mask &= UINT16_MAX;
  }

  // No matter what configuration we've found, |size_mask| should contain a
  // mask representing all the valid bits that can be set in the address.
  bar.size = size_mask + 1;

  // Write the original address value we had before probing and re-enable its
  // access mode now that probing is complete.
  WriteBarInformation(bar);

  std::array<char, 8> pretty_size = {};
  zxlogf(DEBUG, "[%s] Region %u: probed %s (%s%sprefetchable) [size=%s]", cfg_->addr(), bar_id,
         (bar.is_mmio) ? "Memory" : "I/O ports", (bar.is_64bit) ? "64-bit, " : "",
         (bar.is_prefetchable) ? "" : "non-",
         format_size(pretty_size.data(), pretty_size.max_size(), bar.size));
  bars_[bar_id] = std::move(bar);
  return zx::ok();
}

void Device::ProbeBars() {
  for (uint32_t bar_id = 0; bar_id < bar_count_; bar_id++) {
    auto result = ProbeBar(bar_id);
    if (result.is_error()) {
      zxlogf(ERROR, "[%s] Skipping bar %u due to probing error: %s", cfg_->addr(), bar_id,
             result.status_string());
      continue;
    }

    // If the bar was probed as 64 bit then mark then we can just skip the next bar.
    if (bars_[bar_id] && bars_[bar_id]->is_64bit) {
      bar_id++;
    }
  }
}

// Allocates appropriate address space for BAR |bar| out of any suitable
// upstream allocators, using |base| as the base address if present.
zx::status<std::unique_ptr<PciAllocation>> Device::AllocateFromUpstream(
    const Bar& bar, std::optional<zx_paddr_t> base) {
  ZX_DEBUG_ASSERT(bar.size > 0);
  std::unique_ptr<PciAllocation> allocation;

  // On all platforms if a BAR is not marked in its register as MMIO then it
  // goes through the Root Host IO/PIO allocator, regardless of whether the
  // platform's IO is actually MMIO backed.
  if (!bar.is_mmio) {
    return upstream_->pio_regions().Allocate(base, bar.size);
  }

  // Prefetchable bars *must* come from a prefetchable region. However, Bridges
  // only allocate 64 bit space to the prefetchable window. This means if we
  // want to allocate a 64 bit BAR then it must also come from the prefetchable
  // window. At the Root Host level if no address base is provided it will
  // attempt to allocate from the 32 bit allocator if the platform does not
  // populate any space in the > 4GB region, but this does not matter at the
  // level of endpoints below a bridge since they will be assigning out of the
  // address windows provided to their upstream bridges.
  // TODO(fxb/32978): Do we need to worry about BARs that want to span the 4GB boundary?
  if (bar.is_prefetchable || bar.is_64bit) {
    if (auto result = upstream_->pf_mmio_regions().Allocate(base, bar.size); result.is_ok()) {
      return result;
    }
  }

  // If the BAR is 32 bit, or for some reason the 64 bit window wasn't populated
  // them fall back to the 32 bit allocator. 64 bit BARs are commonly allocated
  // out of the < 4GB range on Intel platforms.
  return upstream_->mmio_regions().Allocate(base, bar.size);
}

// Higher level method to allocate address space a previously probed BAR id
// |bar_id| and handle configuration space setup.
zx::status<> Device::AllocateBar(uint8_t bar_id) {
  ZX_DEBUG_ASSERT(upstream_);
  ZX_DEBUG_ASSERT(bar_id < bar_count_);
  ZX_DEBUG_ASSERT(bars_[bar_id].has_value());
  DeviceIoDisable io_disable(this);

  Bar& bar = *bars_[bar_id];
  // Try to retain existing bar address mappings.
  if (bar.address) {
    auto result = AllocateFromUpstream(bar, bar.address);
    if (result.is_ok()) {
      bar.allocation = std::move(result.value());
      // If we succeeded in allocating this range then the bar register itself
      // is already configured.
      return zx::ok();
    }

    // If the device was enabled and we can't remap it then we'll give up on it.
    if (io_disable.io_was_enabled()) {
      return result.take_error();
    }
  }

  // If we've made it here then we either had no existing address, or we had an
  // address but the device is not yet enableed. In either case it is safe to
  // allocate a new mapping for the bar.
  auto result = AllocateFromUpstream(bar, std::nullopt);
  if (result.is_error()) {
    return result.take_error();
  }
  bar.allocation = std::move(result.value());

  bar.address = bar.allocation->base();
  WriteBarInformation(bar);
  zxlogf(TRACE, "[%s] allocated [%#lx, %#lx) to BAR%u", cfg_->addr(), bar.allocation->base(),
         bar.allocation->base() + bar.allocation->size(), bar.bar_id);

  return zx::ok();
}

zx::status<> Device::AllocateBars() {
  fbl::AutoLock dev_lock(&dev_lock_);
  ZX_DEBUG_ASSERT(plugged_in_);
  ZX_DEBUG_ASSERT(bar_count_ <= bars_.max_size());

  // Allocate BARs for the device
  for (uint32_t bar_id = 0; bar_id < bar_count_; bar_id++) {
    if (bars_[bar_id]) {
      if (auto result = AllocateBar(bar_id); result.is_error()) {
        zxlogf(ERROR, "[%s] failed to allocate bar %u: %s", cfg_->addr(), bar_id,
               result.status_string());
        return result.take_error();
      }
    }
  }

  // Based on the Memory / IO space found in active BARs we need to switch on
  // the relevant access bits in the device's Command register.
  config::Command cmd{.value = ReadCmdLocked()};
  for (auto& bar : bars_) {
    if (bar) {
      if (bar->is_mmio) {
        cmd.set_memory_space(true);
      } else {
        cmd.set_io_space(true);
      }
    }
  }
  AssignCmdLocked(cmd.value);
  return zx::ok();
}

zx::status<PowerManagementCapability::PowerState> Device::GetPowerState() {
  fbl::AutoLock dev_lock(&dev_lock_);
  if (!caps_.power) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::ok(caps_.power->GetPowerState(*cfg_));
}

void Device::Unplug() {
  zxlogf(TRACE, "[%s] %s %s", cfg_->addr(), (is_bridge()) ? " (b)" : "", __func__);
  fbl::AutoLock dev_lock(&dev_lock_);
  // Disable should have been called before Unplug and would have disabled
  // everything in the command register
  ZX_DEBUG_ASSERT(disabled_);
  upstream_->UnlinkDevice(this);
  // After unplugging from the Bus there should be no further references to this
  // device and the dtor will be called.
  bdi_->UnlinkDevice(this);
  plugged_in_ = false;
  zxlogf(TRACE, "device [%s] unplugged", cfg_->addr());
}

}  // namespace pci
