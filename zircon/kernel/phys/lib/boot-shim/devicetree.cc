// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/boot-shim/devicetree.h"

#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/devicetree/path.h>
#include <lib/stdcompat/array.h>
#include <lib/stdcompat/string_view.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/driver-config.h>
#include <stdio.h>

#include <array>
#include <limits>
#include <string>
#include <string_view>
#include <type_traits>

namespace boot_shim {
namespace {

struct GicV3Regs {
  enum Regs : size_t {
    kGicd = 0,
    kGicr = 1,
    kGicc = 2,
    kGich = 3,
    kGicv = 4,
    kReserved = 5,
  };
};

struct GicV2Regs {
  enum Regs : size_t {
    kGicd = 0,
    kGicc = 1,

    // Virtualization Extension
    kGich = 2,
    kGicv = 3,
    kReserved = 4,
  };
};

}  // namespace

devicetree::ScanState ArmDevicetreePsciItem::HandlePsciNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  if (auto method = decoder.FindProperty("method")) {
    if (auto method_str = method->AsString()) {
      set_payload(zbi_dcfg_arm_psci_driver_t{
          .use_hvc = *method_str == "hvc",
      });
    }
  } else {
    OnError("\"method\" property missing.");
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState ArmDevicetreePsciItem::OnNode(const devicetree::NodePath& path,
                                                    const devicetree::PropertyDecoder& decoder) {
  auto [compatibles] = decoder.FindProperties("compatible");
  if (!compatibles) {
    return devicetree::ScanState::kActive;
  }

  auto compatible_list = compatibles->AsStringList();
  if (!compatible_list) {
    return devicetree::ScanState::kActive;
  }

  for (auto dev : *compatible_list) {
    for (auto psci : kCompatibleDevices) {
      if (dev == psci) {
        return HandlePsciNode(path, decoder);
      }
    }
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState ArmDevicetreeGicItem::HandleGicChildNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  VisitPayload([&](auto& dcfg) -> void {
    using dtype = std::decay_t<decltype(dcfg)>;
    if constexpr (std::is_same_v<dtype, zbi_dcfg_arm_gic_v2_driver_t>) {
      // If subnode is defined, then msi is enabled.
      const auto& node_name = path.back();
      if (cpp20::starts_with(node_name, "v2m")) {
        auto [reg_property] = decoder.FindProperties("reg");
        if (!reg_property) {
          return;
        }

        auto reg = reg_property->AsReg(decoder);
        if (!reg) {
          return;
        }
        dcfg.use_msi = true;

        uint64_t base_address = *(*reg)[0].address();
        if (ranges_) {
          base_address = ranges_->TranslateChildAddress(base_address).value_or(base_address);
        }
        dcfg.msi_frame_phys = base_address;
      }
    } else if constexpr (std::is_same_v<dtype, zbi_dcfg_arm_gic_v3_driver_t>) {
      // TODO(fxbug.dev/128235) : no support yet.
    } else {
      // No Driver set, but we have seen the GIC, so this is an error.
      ZX_PANIC("GIC Item should have been initialized before looking into its child.");
    }
  });
  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::HandleGicV3(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  const auto& [reg_property] = decoder.FindProperties("reg");
  if (!reg_property) {
    OnError("GIC v3: No 'reg' found.");
    return devicetree::ScanState::kDoneWithSubtree;
  }

  auto reg_ptr = reg_property->AsReg(decoder);
  if (!reg_ptr) {
    OnError("GIC v3: Failed to decode 'reg'.");
    return devicetree::ScanState::kDoneWithSubtree;
  }

  auto& reg = *reg_ptr;

  auto [redistributor_stride] = decoder.FindProperties("redistributor-stride");

  zbi_dcfg_arm_gic_v3_driver_t dcfg{};

  if (reg.size() > 1) {
    dcfg.mmio_phys = *reg[GicV3Regs::kGicd].address();
    dcfg.gicd_offset = 0;

    dcfg.gicr_offset = *reg[GicV3Regs::kGicr].address() - dcfg.mmio_phys;
    if (redistributor_stride) {
      if (auto stride = redistributor_stride->AsUint32()) {
        dcfg.gicr_stride = *stride;
      }
    } else {
      // See:
      //   "Arm Generic Interrupt Controller Architecture Specification GIC architecture version 3
      //   and version 4" 12.10 The GIC Redistributor register map
      // Specifically:
      //    - Each Redistributor defines two 64KB frames in the physical address map
      //
      // Note for GicV4: VLPI (Virtual Local Peripherial Interrupt), which would require an extra
      // two pages.
      dcfg.gicr_stride = 2 * 64 << 10;
    }
  }

  dcfg.ipi_base = 0;
  dcfg.optional = false;
  set_payload(dcfg);

  return devicetree::ScanState::kDone;
}

devicetree::ScanState ArmDevicetreeGicItem::HandleGicV2(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  const auto& [reg_property] = decoder.FindProperties("reg");

  if (!reg_property) {
    OnError("GIC v2: No 'reg' found.");
    return devicetree::ScanState::kDone;
  }

  auto reg_ptr = reg_property->AsReg(decoder);
  if (!reg_ptr) {
    OnError("GIC v2: Failed to decode 'reg'.");
    return devicetree::ScanState::kDone;
  }

  auto& reg = *reg_ptr;

  zbi_dcfg_arm_gic_v2_driver_t dcfg{};

  if (reg.size() > 1) {
    dcfg.mmio_phys = *reg[GicV2Regs::kGicd].address();
    dcfg.gicd_offset = 0;
    dcfg.gicc_offset = reg[GicV2Regs::kGicc].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
  }

  // If there are more than 2, then the virtualization extension is provided.
  if (reg.size() > 2) {
    dcfg.gich_offset = reg[GicV2Regs::kGich].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
    dcfg.gicv_offset = reg[GicV2Regs::kGicv].address().value_or(dcfg.mmio_phys) - dcfg.mmio_phys;
  }

  dcfg.ipi_base = 0;
  dcfg.optional = false;
  // Default values when msi is not enabled. This is determined by proper handlers.
  // The MSI Frame registers are contigous, so we will pick the lowest base address from all the
  // frames.
  dcfg.use_msi = false;
  dcfg.msi_frame_phys = std::numeric_limits<decltype(dcfg.msi_frame_phys)>::max();
  set_payload(dcfg);
  gic_ = &path.back();

  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::OnNode(const devicetree::NodePath& path,
                                                   const devicetree::PropertyDecoder& decoder) {
  if (IsGicChildNode()) {
    return HandleGicChildNode(path, decoder);
  }

  // Figure out which interrupt controller this is.
  const auto& [compatible, interrupt_controller, msi_controller, ranges] =
      decoder.FindProperties("compatible", "interrupt-controller", "msi-controller", "ranges");

  if (!interrupt_controller || !compatible) {
    return devicetree::ScanState::kActive;
  }

  if (ranges) {
    ranges_ = ranges->AsRanges(decoder);
  }

  auto compatible_list = compatible->AsStringList();
  if (!compatible_list) {
    return devicetree::ScanState::kActive;
  }

  // Check for gic version.
  for (auto comp : *compatible_list) {
    for (auto v3 : kGicV3CompatibleDevices) {
      if (v3 == comp) {
        return HandleGicV3(path, decoder);
      }
    }

    for (auto v2 : kGicV2CompatibleDevices) {
      if (v2 == comp) {
        return HandleGicV2(path, decoder);
      }
    }
  }

  // Keep looking.
  return devicetree::ScanState::kActive;
}

devicetree::ScanState ArmDevicetreeGicItem::OnSubtree(const devicetree::NodePath& path) {
  if (gic_ == nullptr) {
    return matched_ ? devicetree::ScanState::kDone : devicetree::ScanState::kActive;
  }

  if (gic_ != &path.back()) {
    return devicetree::ScanState::kActive;
  }

  gic_ = nullptr;
  matched_ = true;
  return devicetree::ScanState::kDone;
}

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::HandleBootstrapStdout(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  auto resolved_path = decoder.ResolvePath(stdout_path_);
  if (resolved_path.is_error()) {
    if (resolved_path.error_value() == devicetree::PropertyDecoder::PathResolveError::kNoAliases) {
      return devicetree::ScanState::kNeedsPathResolution;
    }
    return devicetree::ScanState::kDone;
  }

  // for hand off.
  resolved_stdout_ = *resolved_path;

  switch (devicetree::ComparePath(path, *resolved_path)) {
    case devicetree::CompareResult::kIsMatch:
      break;
    case devicetree::CompareResult::kIsAncestor:
      return devicetree::ScanState::kActive;
    default:
      return devicetree::ScanState::kDoneWithSubtree;
  }

  auto [compatible, interrupt, reg_property, interrupt_parent] =
      decoder.FindProperties("compatible", "interrupts", "reg", "interrupt-parent");

  // Without this we cant figure out what driver to use.
  if (!compatible) {
    return devicetree::ScanState::kDone;
  }

  // No MMIO region, we cant do anything.
  if (!reg_property) {
    return devicetree::ScanState::kDone;
  }

  auto reg = reg_property->AsReg(decoder);
  if (!reg) {
    return devicetree::ScanState::kDone;
  }

  auto addr = (*reg)[0].address();

  if (addr) {
    auto compatible_with = compatible->AsStringList();
    if (!compatible_with) {
      return devicetree::ScanState::kDone;
    }

    zbi_dcfg_simple_t dcfg{
        .mmio_phys = *addr,
        .irq = 0,
    };

    if (!match_(*compatible_with, dcfg)) {
      return devicetree::ScanState::kDone;
    }
  }

  return devicetree::ScanState::kDone;
}

devicetree::ScanState DevicetreeBootstrapChosenNodeItemBase::OnNode(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  if (found_chosen_) {
    if (!stdout_path_.empty()) {
      return HandleBootstrapStdout(path, decoder);
    }
    return devicetree::ScanState::kDone;
  }

  switch (devicetree::ComparePath(path, "/chosen")) {
    case devicetree::CompareResult::kIsAncestor:
      return devicetree::ScanState::kActive;
    case devicetree::CompareResult::kIsMismatch:
    case devicetree::CompareResult::kIsDescendant:
      return devicetree::ScanState::kDoneWithSubtree;
    case devicetree::CompareResult::kIsMatch:
      found_chosen_ = true;
      break;
  };

  // We are on /chosen, pull the cmdline, zbi and uart device path.
  auto [bootargs, stdout_path, legacy_stdout_path, ramdisk_start, ramdisk_end] =
      decoder.FindProperties("bootargs", "stdout-path", "linux,stdout-path", "linux,initrd-start",
                             "linux,initrd-end");
  if (bootargs) {
    if (auto cmdline = bootargs->AsString()) {
      cmdline_ = *cmdline;
    }
  }

  if (stdout_path) {
    stdout_path_ = stdout_path->AsString().value_or("");
  } else if (legacy_stdout_path) {
    stdout_path_ = legacy_stdout_path->AsString().value_or("");
  }

  if (ramdisk_start && ramdisk_end) {
    auto addr_start = ramdisk_start->AsUint32();
    auto addr_end = ramdisk_end->AsUint32();
    if (addr_start && addr_end) {
      zbi_ = cpp20::span<const std::byte>(reinterpret_cast<const std::byte*>(*addr_start),
                                          *addr_end - *addr_start);
    }
  }

  return devicetree::ScanState::kActive;
}

}  // namespace boot_shim
