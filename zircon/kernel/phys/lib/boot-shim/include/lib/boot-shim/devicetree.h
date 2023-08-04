// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/item-base.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/fit/function.h>
#include <lib/fit/result.h>
#include <lib/memalloc/range.h>
#include <lib/stdcompat/array.h>
#include <lib/stdcompat/source_location.h>
#include <lib/stdcompat/string_view.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/cpu.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/storage-traits.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>

#include <array>
#include <optional>
#include <string>
#include <string_view>

#include <fbl/type_info.h>

namespace boot_shim {

// Base class for DevicetreeItems, providing default implementations for the Matcher API.
// Derived classes MUST implement OnNode.
template <typename T, size_t MaxScans>
class DevicetreeItemBase {
 public:
  static constexpr size_t kMaxScans = MaxScans;

  constexpr DevicetreeItemBase() = default;
  constexpr DevicetreeItemBase(const char* shim_name, FILE* log)
      : log_(log), shim_name_(shim_name) {}

  devicetree::ScanState OnNode(const devicetree::NodePath&, const devicetree::PropertyDecoder&) {
    static_assert(kMaxScans != MaxScans, "Must implement OnNode.");
    return devicetree::ScanState::kActive;
  }

  devicetree::ScanState OnScan() { return devicetree::ScanState::kActive; }

  void OnError(std::string_view error) {
    Log("Error on %s, %*s\n", fbl::TypeInfo<T>::Name(), static_cast<int>(error.length()),
        error.data());
  }

  devicetree::ScanState OnSubtree(const devicetree::NodePath&) {
    return devicetree::ScanState::kActive;
  }

  template <typename Shim>
  void Init(const Shim& shim) {
    static_assert(devicetree::kIsMatcher<T>);
    shim_name_ = shim.shim_name();
    log_ = shim.log();
  }

 protected:
  // Helper for logging in to |log_|.
  void Log(const char* fmt, ...) __PRINTFLIKE(2, 3) {
    fprintf(log_, "%s: ", shim_name_);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(log_, fmt, ap);
    va_end(ap);
  }

 private:
  FILE* log_;
  const char* shim_name_;
};

// Decodes PSCI information from a devicetree and synthesizes a
// DRIVER_CONFIG ZBI item for it.
//
// A PSCI device is encoded within a node called "psci" with a "compatible" property
// giving its compatible PSCI revisions (i.e., values of `kCompatibleDevices` below).
//
// For example,
//
// psci {
//      compatible  = "arm,psci-0.2";
//      method      = "smc";
// };
//
// For more details please see
// https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/psci.txt
class ArmDevicetreePsciItem
    : public DevicetreeItemBase<ArmDevicetreePsciItem, 1>,
      public SingleOptionalItem<zbi_dcfg_arm_psci_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_ARM_PSCI> {
 public:
  static constexpr auto kCompatibleDevices = cpp20::to_array<std::string_view>({
      // PSCI 0.1 : Not Supported.
      // "arm,psci",
      "arm,psci-0.2",
      "arm,psci-1.0",
  });

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);

 private:
  devicetree::ScanState HandlePsciNode(const devicetree::NodePath& path,
                                       const devicetree::PropertyDecoder& decoder);
};

// Parses either GIC v2 or GIC v3 device node into proper ZBI item.
//
// This item will scan the devicetree for either a node compatible with GIC v2 bindings or GIC v3
// bindings. Upon finding such node it will generate either a |zbi_dcfg_arm_gic_v2_driver_t| for
// GIC v2 or a |zbi_dcfg_arm_gic_v3_driver_t| for GIC v3.
//
// In case of GIC v2, it will determine whether the MSI extension is supported or not by looking
// at the children of the GIC v2 node.
//
// Each interrupt controller contains uses a custom format for their 'reg' property, which defines
// the different address ranges required for the driver.
//
// See for GIC v2:
// * https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/arm%2Cgic.txt
// See for GIC v3:
// * https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/arm%2Cgic-v3.txt
class ArmDevicetreeGicItem
    : public DevicetreeItemBase<ArmDevicetreeGicItem, 1>,
      public SingleVariantItemBase<ArmDevicetreeGicItem, zbi_dcfg_arm_gic_v2_driver_t,
                                   zbi_dcfg_arm_gic_v3_driver_t> {
 public:
  static constexpr auto kGicV2CompatibleDevices = cpp20::to_array<std::string_view>({
      "arm,gic-400",
      "arm,cortex-a15-gic",
      "arm,cortex-a9-gic",
      "arm,cortex-a7-gic",
      "arm,arm11mp-gic",
      "brcm,brahma-b15-gic",
      "arm,arm1176jzf-devchip-gic",
      "qcom,msm-8660-qgic",
      "qcom,msm-qgic2",
  });

  static constexpr auto kGicV3CompatibleDevices = cpp20::to_array<std::string_view>({"arm,gic-v3"});

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnSubtree(const devicetree::NodePath& path);

  devicetree::ScanState OnScan() {
    return matched_ ? devicetree::ScanState::kDone : devicetree::ScanState::kActive;
  }

  // Boot Shim Item API.
  static constexpr zbi_header_t ItemHeader(const zbi_dcfg_arm_gic_v2_driver_t& driver) {
    return {.type = ZBI_TYPE_KERNEL_DRIVER, .extra = ZBI_KERNEL_DRIVER_ARM_GIC_V2};
  }

  static constexpr zbi_header_t ItemHeader(const zbi_dcfg_arm_gic_v3_driver_t& driver) {
    return {.type = ZBI_TYPE_KERNEL_DRIVER, .extra = ZBI_KERNEL_DRIVER_ARM_GIC_V3};
  }

 private:
  devicetree::ScanState HandleGicV2(const devicetree::NodePath& path,
                                    const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleGicV3(const devicetree::NodePath& path,
                                    const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleGicChildNode(const devicetree::NodePath& path,
                                           const devicetree::PropertyDecoder& decoder);

  constexpr bool IsGicChildNode() const { return gic_ != nullptr; }

  const devicetree::Node* gic_ = nullptr;
  bool matched_ = false;
};

// This matcher parses the 'chosen' node, which is a child of the root node('/chosen'). This node
// contains information about the commandline, ramdisk and UART.
//
// * The cmdline is contained as part of the string block of the devicetree.
//
// * The ramdisk is represented as a range in memory where the firmware loaded it, usually a ZBI.
//
// * The UART on the other hand, is represented as path(which may be aliased). Is the job of this
//   item to bootstrap the UART, which means determining which drItemiver needs to be used.
//
// For more details on the chosen node please see:
//  https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#chosen-node
class DevicetreeChosenNodeMatcherBase
    : public DevicetreeItemBase<DevicetreeChosenNodeMatcherBase, 3> {
 public:
  DevicetreeChosenNodeMatcherBase(const char* shim_name, FILE* log)
      : DevicetreeItemBase(shim_name, log) {}

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);

  // Accessors

  // Input ZBI from devicetree.
  constexpr zbitl::ByteView zbi() const { return zbi_; }

  // Command line arguments from devicetree.
  constexpr std::optional<std::string_view> cmdline() const { return cmdline_; }

  // Resolved path for stdout device(e.g. uart) from the devicetree.
  constexpr std::optional<devicetree::ResolvedPath> stdout_path() const { return resolved_stdout_; }

 protected:
  auto& uart_matcher() { return uart_matcher_; }

  auto& uart_emplacer() { return uart_emplacer_; }

 private:
  devicetree::ScanState HandleBootstrapStdout(const devicetree::NodePath& path,
                                              const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleUartInterruptParent(const devicetree::PropertyDecoder& decoder);

  // Path to device node containing the stdout device (uart).
  bool found_chosen_ = false;
  std::string_view stdout_path_;
  std::optional<devicetree::ResolvedPath> resolved_stdout_;
  zbi_dcfg_simple_t uart_dcfg_ = {};
  std::optional<uint32_t> uart_interrupt_parent_;
  std::optional<devicetree::PropertyValue> uart_interrupts_;

  // Command line provided by the devicetree.
  std::string_view cmdline_;

  zbitl::ByteView zbi_;

  // Type erased match.
  fit::inline_function<bool(const devicetree::PropertyDecoder&)> uart_matcher_ = nullptr;
  fit::inline_function<void(const zbi_dcfg_simple_t&)> uart_emplacer_ = nullptr;
};

template <typename AllUartDrivers = uart::all::Driver>
class DevicetreeChosenNodeMatcher : public DevicetreeChosenNodeMatcherBase {
 public:
  DevicetreeChosenNodeMatcher(const char* shim_name, FILE* log = stdout)
      : DevicetreeChosenNodeMatcherBase(shim_name, log) {
    uart_matcher() = [this](const auto& decoder) -> bool {
      uart_emplacer() = uart_.MatchDevicetree(decoder);
      return uart_emplacer() != nullptr;
    };
  }

  constexpr const auto& uart() const { return uart_; }

 private:
  uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy, AllUartDrivers> uart_;
};

// This matcher parses 'memory' and 'reserved_memory' device nodes and 'memranges' from the
// devicetree and makes them available.
//
// The memory regions are encoded in three different sources, whose layout and number of ranges
// pero node may vary.
//  * Each 'memory' nodes defines a collection of ranges that represent ram. Memory nodes
//    are childs of the root node and contain an address as part of the name(E.g. "/memory@1234").
//  * 'reserved-memory' is a container node, whose children define collections of memory ranges
//  that should be reserved. The 'reserved-memory' node is located under the root node
//  '/reserved-memory'.
//  * 'memreseve' represents the memory reservation block, which encodes pairs describing base
//  address and length of reserved memory ranges.
//
// For more information and examples of each source see :
// '/memory' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#memory-node
// '/reserved-memory' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#reserved-memory-node
// 'memreserve' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter5-flattened-format.html#memory-reservation-block
//
class DevicetreeMemoryMatcher : public DevicetreeItemBase<DevicetreeMemoryMatcher, 1> {
 public:
  // Matcher API.
  constexpr DevicetreeMemoryMatcher(const char* shim_name, FILE* log,
                                    cpp20::span<memalloc::Range> storage)
      : DevicetreeItemBase(shim_name, log), ranges_(storage) {}

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnSubtree(const devicetree::NodePath& path);
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

  // Returns true if additional ranges from the devicetree are successfully appended.
  // The matcher API constitutes only ranges encoded as device nodes within the devicetree.
  // The additional ranges include those defined as 'memreserve'and the devicetree itself.
  bool AppendAdditionalRanges(const devicetree::Devicetree& fdt) {
    if (!AppendRange({
            .addr = reinterpret_cast<uintptr_t>(fdt.fdt().data()),
            .size = fdt.size_bytes(),
            // The original DT Blob is copied into a ZBI ITEM, and the original range is discarded.
            // It is only useful while the ZBI items are generated.
            .type = memalloc::Type::kDevicetreeBlob,
        })) {
      return false;
    }

    for (auto [start, size] : fdt.memory_reservations()) {
      if (!AppendRange(memalloc::Range{
              .addr = start,
              .size = size,
              .type = memalloc::Type::kReserved,
          })) {
        return false;
      }
    }

    return true;
  }

  // Memory Item API for the bootshim to initialize the memory layout.
  // An empty set of memory ranges indicates an error while parsing the devicetree
  // memory ranges.
  constexpr cpp20::span<const memalloc::Range> memory_ranges() const {
    if (ranges_count_ <= ranges_.size()) {
      return cpp20::span{ranges_.data(), ranges_count_};
    }
    return {};
  }

 private:
  // Append special ranges to the memory regions. This will be used later for
  // initializing the pool allocation memory.
  constexpr bool AppendRange(const memalloc::Range& range) {
    if (ranges_count_ >= ranges_.size()) {
      if (ranges_count_ == ranges_.size()) {
        OnError("Not enough preallocated ranges.");
      }
      ranges_count_ = ranges_.size() + 1;
      return false;
    }
    ranges_[ranges_count_++] = range;
    return true;
  }

  bool AppendRangesFromReg(const devicetree::PropertyDecoder& decoder,
                           const std::optional<devicetree::RangesProperty>& parent_range,
                           memalloc::Type memrange_type);

  devicetree::ScanState HandleMemoryNode(const devicetree::NodePath& path,

                                         const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleReservedMemoryNode(const devicetree::NodePath& path,
                                                 const devicetree::PropertyDecoder& decoder);

  cpp20::span<memalloc::Range> ranges_;
  size_t ranges_count_ = 0;

  const devicetree::Node* reserved_memory_root_ = nullptr;

  // Used to translate child node memory ranges.
  std::optional<devicetree::RangesProperty> root_ranges_;
  std::optional<devicetree::RangesProperty> reserved_memory_ranges_;
};

// This item parses the '/cpus' 'timebase-frequency property to generate a timer driver
// configuration ZBI item.
//
// The timebase frequency specifies the clock frequency of the RISC-V timer device.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/timer/riscv%2Ctimer.yaml
class RiscvDevicetreeTimerItem
    : public DevicetreeItemBase<RiscvDevicetreeTimerItem, 1>,
      public SingleOptionalItem<zbi_dcfg_riscv_generic_timer_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER> {
 public:
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
};

// Parses interrupt controller node that is compatible with PLIC (Platform Level Interrupt
// Controller bindings. For the time being, it only parses the mmio base for the plic register bank
// and the number of IRQs. Until the zbi item representing the riscv PLIC is extended to represent
// the contexts(hart_id, priority), the 'interrupt-extended' property is not yet decoded.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/sifive%2Cplic-1.0.0.txt
class RiscvDevicetreePlicItem
    : public DevicetreeItemBase<RiscvDevicetreePlicItem, 1>,
      public SingleOptionalItem<zbi_dcfg_riscv_plic_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_RISCV_PLIC> {
 public:
  static constexpr auto kCompatibleDevices = cpp20::to_array({"sifive,plic-1.0.0", "riscv,plic0"});

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);

 private:
  devicetree::ScanState HandlePlicNode(const devicetree::NodePath& path,
                                       const devicetree::PropertyDecoder& decoder);
};

// Parses '/cpus' node to generate |ZBI_TYPE_CPU_TOPOLOGY| item. This involves both parsing CPU
// nodes and the '/cpus/cpu-map' node when present. Lack of a 'cpu-map' means all nodes are
// considered siblings which is reflected with none of them having a parent.
//
// A cluster's performance class is the normalized capacity of a cluster based on the maximum
// capacity of all clusters.
//
// cluster-performance-class[i] = cluster-capacity[i] * 255 / max(cluster-capacity[0]....N)
//
// When a cluster-capacity is not able to be determined because no property in the node provides
// this value then all clusters are given a performance class of 1. Its important to realize that
// the actual value of the performance class is only a representative of the relative difference
// between difference clusters.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/cpu-capacity.txt
// https://www.kernel.org/doc/Documentation/devicetree/bindings/cpu/cpu-topology.txt
class DevictreeCpuTopologyItem : public DevicetreeItemBase<DevictreeCpuTopologyItem, 2>,
                                 public ItemBase {
 public:
  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnSubtree(const devicetree::NodePath& path);

  size_t size_bytes() const { return ItemSize(node_element_count() * sizeof(zbi_topology_node_t)); }

  fit::result<DataZbi::Error> AppendItems(DataZbi& zbi) const;

 protected:
  // Used for decoding CPU-related properties.
  struct CpuEntry {
    std::optional<uint32_t> phandle;
    devicetree::Properties properties;
  };

  // Callback used for setting up/updating processor information, that is dependent in
  // architecture specific information.
  using SetArchCpuInfo =
      fit::inline_function<void(zbi_topology_processor_t&, const CpuEntry& entry)>;

  template <typename Shim>
  void Init(const Shim& shim, SetArchCpuInfo arch_info_setter) {
    DevicetreeItemBase<DevictreeCpuTopologyItem, 2>::Init(shim);
    allocator_ = &shim.allocator();
    arch_info_setter_ = std::move(arch_info_setter);
  }

 private:
  // Devicetree 'cpu-map' entities.
  enum class TopologyEntryType {
    kSocket,
    kCluster,
    kCore,
    kThread,
  };

  // Generic entry in the devicetree, maintains parent relationship and a view into the properties.
  struct CpuMapEntry {
    // Type of the entry.
    TopologyEntryType type;
    // Index of the parent entry on the cpu map.
    size_t parent_index;
    // Index of the cluster entry where this node is contained within the cpu map.
    std::optional<uint32_t> cluster_index;
    // 'phandle' obtained from the 'core' or 'thread' entries. Nodes containing this 'phandle'
    // represent a processing unit, and are leaf nodes in the cpu map.
    std::optional<uint32_t> cpu_phandle;
    // Index of the |CpuEntry| in the |cpus_| representing the resolved link of the |cpu_phandle|
    // to a |cpu| node.
    std::optional<uint32_t> cpu_index;
    // Index of |zbi_topology_node_t| in the |ZBI_ITEM_TYPE_CPU_TOPOLOGY| that was generated from
    // this |CpuMapEntry|.
    std::optional<size_t> topology_node_index;
  };

  // May only be called after |Init| and a full match sequence has been performed.
  constexpr size_t node_element_count() const { return topology_node_count_; }

  devicetree::ScanState IncreaseEntryNodeCountFirstScan(const devicetree::NodePath& path,
                                                        const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState AddEntryNodeSecondScan(const devicetree::NodePath& path,
                                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState IncreaseCpuNodeCountFirstScan(const devicetree::NodePath& path,
                                                      const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState AddCpuNodeSecondScan(const devicetree::NodePath& path,
                                             const devicetree::PropertyDecoder& decoder);

  static constexpr bool IsCpuMapNode(std::string_view node_name, std::string_view prefix) {
    if (!cpp20::starts_with(node_name, prefix)) {
      return false;
    }
    // Must match prefix[0-9].
    return node_name.substr(prefix.length()).find_first_not_of("01234567890") ==
           std::string_view::npos;
  }

  // After both |entries_| and |cpus_| have been filled this routine will fill up
  // the reference from an entry to a 'cpu' node.
  fit::result<ItemBase::DataZbi::Error> UpdateEntryCpuLinks() const;

  // Recalculates performance class based on CPU capacity related properties.
  fit::result<ItemBase::DataZbi::Error> CalculateClusterPerformanceClass(
      cpp20::span<zbi_topology_node_t> nodes) const;

  template <typename T>
  T* Allocate(size_t count,
              cpp20::source_location location = cpp20::source_location::current()) const {
    auto* alloc = static_cast<T*>((*allocator_)(sizeof(T) * count, alignof(T)));
    if (!alloc) {
      // Log allocation failure. The effect is that the matcher will keep looking and will fail to
      // make progress. But the error will be logged.
      auto* self = const_cast<DevictreeCpuTopologyItem*>(this);
      self->OnError("Allocation Failed.");
      self->Log("at %s:%u\n", location.file_name(), static_cast<unsigned int>(location.line()));
    }
    return alloc;
  }

  // Flattened 'cpu-map'.
  CpuMapEntry* map_entries_ = nullptr;
  uint32_t map_entry_index_ = 0;
  uint32_t map_entry_count_ = 0;
  bool has_cpu_map_ = false;

  // Used to track parent-child relationships when building the flattened cpu-map.
  std::optional<uint32_t> current_socket_ = 0;
  std::optional<uint32_t> current_cluster_ = 0;
  std::optional<uint32_t> current_core_ = 0;

  CpuEntry* cpu_entries_ = nullptr;
  uint32_t cpu_entry_count_ = 0;
  uint32_t cpu_entry_index_ = 0;
  uint32_t cluster_count_ = 0;

  size_t topology_node_count_ = 0;

  // Allocation is environment specific, so we delegate that to a lambda.
  mutable const DevicetreeBootShimAllocator* allocator_ = nullptr;

  SetArchCpuInfo arch_info_setter_;
};

class RiscvDevictreeCpuTopologyItem : public DevictreeCpuTopologyItem {
 public:
  template <typename Shim>
  void Init(Shim& shim) {
    DevictreeCpuTopologyItem::Init(
        shim, [this](zbi_topology_processor_t& node, const CpuEntry& cpu_entry) -> void {
          node.architecture_info.discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64;
          devicetree::PropertyDecoder decoder(cpu_entry.properties);
          auto reg = decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsUint32>("reg");
          if (!reg) {
            return;
          }
          node.architecture_info.riscv64.hart_id = *reg;
          if (*reg == boot_hart_id_) {
            node.flags |= ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
          } else {
            node.flags &= ~ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
          }
        });
  }

  void set_boot_hart_id(uint64_t hart_id) { boot_hart_id_ = hart_id; }

 private:
  std::optional<uint64_t> boot_hart_id_;
};

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
