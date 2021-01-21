// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/x86/system_topology.h"

#include <debug.h>
#include <lib/acpi_lite.h>
#include <lib/acpi_lite/apic.h>
#include <lib/acpi_lite/numa.h>
#include <lib/system-topology.h>
#include <pow2.h>
#include <stdio.h>
#include <trace.h>

#include <algorithm>

#include <fbl/alloc_checker.h>
#include <kernel/topology.h>
#include <ktl/unique_ptr.h>
#include <platform/pc/acpi.h>

#define LOCAL_TRACE 0

namespace {

using cpu_id::Topology;

// TODO(edcoyne): move to fbl::Vector::resize().
template <typename T>
zx_status_t GrowVector(size_t new_size, fbl::Vector<T>* vector) {
  for (size_t i = vector->size(); i < new_size; i++) {
    fbl::AllocChecker checker;
    vector->push_back(T(), &checker);
    if (!checker.check()) {
      return ZX_ERR_NO_MEMORY;
    }
  }
  return ZX_OK;
}

class Core {
 public:
  explicit Core() {
    node_.entity_type = ZBI_TOPOLOGY_ENTITY_PROCESSOR;
    node_.parent_index = ZBI_TOPOLOGY_NO_PARENT;
    node_.entity.processor.logical_id_count = 0;
    node_.entity.processor.architecture = ZBI_TOPOLOGY_ARCH_X86;
    node_.entity.processor.architecture_info.x86.apic_id_count = 0;
  }

  void SetPrimary(bool primary) {
    node_.entity.processor.flags = (primary) ? ZBI_TOPOLOGY_PROCESSOR_PRIMARY : 0;
  }

  void AddThread(uint16_t logical_id, uint32_t apic_id) {
    auto& processor = node_.entity.processor;
    processor.logical_ids[processor.logical_id_count++] = logical_id;
    processor.architecture_info.x86.apic_ids[processor.architecture_info.x86.apic_id_count++] =
        apic_id;
  }

  void SetFlatParent(uint16_t parent_index) { node_.parent_index = parent_index; }

  const zbi_topology_node_t& node() const { return node_; }

 private:
  zbi_topology_node_t node_;
};

class SharedCache {
 public:
  explicit SharedCache(uint32_t id) {
    node_.entity_type = ZBI_TOPOLOGY_ENTITY_CACHE;
    node_.parent_index = ZBI_TOPOLOGY_NO_PARENT;
    node_.entity.cache.cache_id = id;
  }

  zx_status_t GetCore(int index, Core** core) {
    auto status = GrowVector(index + 1, &cores_);
    if (status != ZX_OK) {
      return status;
    }

    if (!cores_[index]) {
      fbl::AllocChecker checker;
      cores_[index].reset(new (&checker) Core());
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }
    }

    *core = cores_[index].get();

    return ZX_OK;
  }

  void SetFlatParent(uint16_t parent_index) { node_.parent_index = parent_index; }

  zbi_topology_node_t& node() { return node_; }

  fbl::Vector<ktl::unique_ptr<Core>>& cores() { return cores_; }

 private:
  zbi_topology_node_t node_;
  fbl::Vector<ktl::unique_ptr<Core>> cores_;
};

class Die {
 public:
  Die() {
    node_.entity_type = ZBI_TOPOLOGY_ENTITY_DIE;
    node_.parent_index = ZBI_TOPOLOGY_NO_PARENT;
  }

  zx_status_t GetCache(int index, SharedCache** cache) {
    auto status = GrowVector(index + 1, &caches_);
    if (status != ZX_OK) {
      return status;
    }

    if (!caches_[index]) {
      fbl::AllocChecker checker;
      caches_[index].reset(new (&checker) SharedCache(index));
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }
    }

    *cache = caches_[index].get();

    return ZX_OK;
  }

  zx_status_t GetCore(int index, Core** core) {
    auto status = GrowVector(index + 1, &cores_);
    if (status != ZX_OK) {
      return status;
    }

    if (!cores_[index]) {
      fbl::AllocChecker checker;
      cores_[index].reset(new (&checker) Core());
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }
    }

    *core = cores_[index].get();

    return ZX_OK;
  }

  void SetFlatParent(uint16_t parent_index) { node_.parent_index = parent_index; }

  zbi_topology_node_t& node() { return node_; }

  fbl::Vector<ktl::unique_ptr<SharedCache>>& caches() { return caches_; }

  fbl::Vector<ktl::unique_ptr<Core>>& cores() { return cores_; }

  void SetNuma(const acpi_lite::AcpiNumaDomain& numa) { numa_ = {numa}; }

  const ktl::optional<acpi_lite::AcpiNumaDomain>& numa() const { return numa_; }

 private:
  zbi_topology_node_t node_;
  fbl::Vector<ktl::unique_ptr<SharedCache>> caches_;
  fbl::Vector<ktl::unique_ptr<Core>> cores_;
  ktl::optional<acpi_lite::AcpiNumaDomain> numa_;
};

// Unlike the other topological levels, `Package`(/socket) does not define an
// explicit node in the synthesized topology; it serves here merely as a means
// of organizing dies.
class Package {
 public:
  Package() = default;

  zx_status_t GetDie(int index, Die** die) {
    auto status = GrowVector(index + 1, &dies_);
    if (status != ZX_OK) {
      return status;
    }

    if (!dies_[index]) {
      fbl::AllocChecker checker;
      dies_[index].reset(new (&checker) Die());
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }
    }

    *die = dies_[index].get();

    return ZX_OK;
  }

  fbl::Vector<ktl::unique_ptr<Die>>& dies() { return dies_; }

 private:
  fbl::Vector<ktl::unique_ptr<Die>> dies_;
};

class ApicDecoder {
 public:
  ApicDecoder(uint8_t smt, uint8_t core, uint8_t die, uint8_t cache)
      : smt_bits_(smt), core_bits_(core), die_bits_(die), cache_shift_(cache) {}

  static ktl::optional<ApicDecoder> From(const cpu_id::CpuId& cpuid) {
    uint8_t smt_bits = 0, core_bits = 0, die_bits = 0, cache_shift = 0;

    const auto topology = cpuid.ReadTopology();
    const auto cache = topology.highest_level_cache();
    cache_shift = cache.shift_width;
    dprintf(INFO, "Top cache level: %u shift: %u size: %#lx\n", cache.level, cache.shift_width,
            cache.size_bytes);

    const auto levels_opt = topology.levels();
    if (!levels_opt) {
      dprintf(INFO, "ERROR: Unable to determine topology from cpuid. Falling back to flat!\n");
    }

    // If cpuid failed to provide levels fallback to one that just treats
    // every core as separate.
    const auto& levels =
        levels_opt ? *levels_opt
                   : cpu_id::Topology::Levels{
                         .levels = {{.type = cpu_id::Topology::LevelType::CORE, .id_bits = 31}},
                         .level_count = 1};

    for (int i = 0; i < levels.level_count; i++) {
      const auto& level = levels.levels[i];
      switch (level.type) {
        case Topology::LevelType::SMT:
          smt_bits = level.id_bits;
          break;
        case Topology::LevelType::CORE:
          core_bits = level.id_bits;
          break;
        case Topology::LevelType::DIE:
          die_bits = level.id_bits;
          break;
        default:
          break;
      }
    }
    LTRACEF("smt_bits: %u core_bits: %u die_bits: %u cache_shift: %u \n", smt_bits, core_bits,
            die_bits, cache_shift);
    return {ApicDecoder(smt_bits, core_bits, die_bits, cache_shift)};
  }

  uint32_t smt_id(uint32_t apic_id) const { return apic_id & ToMask(smt_bits_); }

  uint32_t core_id(uint32_t apic_id) const { return (apic_id >> smt_bits_) & ToMask(core_bits_); }

  uint32_t die_id(uint32_t apic_id) const {
    return (apic_id >> (smt_bits_ + core_bits_)) & ToMask(die_bits_);
  }

  uint32_t package_id(uint32_t apic_id) const {
    return apic_id >> (smt_bits_ + core_bits_ + die_bits_);
  }

  uint32_t cache_id(uint32_t apic_id) const {
    return (cache_shift_ == 0) ? 0 : (apic_id >> cache_shift_);
  }

  bool has_cache_info() const { return cache_shift_ > 0; }

 private:
  uint32_t ToMask(uint8_t width) const { return valpow2<uint32_t>(width) - 1; }

  const uint8_t smt_bits_;
  const uint8_t core_bits_;
  const uint8_t die_bits_;
  const uint8_t cache_shift_;
};

class PackageList {
 public:
  PackageList(const cpu_id::CpuId& cpuid, const ApicDecoder& decoder) : decoder_(decoder) {
    // APIC of this processor, we will ensure it has logical_id 0;
    primary_apic_id_ = cpuid.ReadProcessorId().local_apic_id();
  }

  zx_status_t Add(const acpi_lite::AcpiMadtLocalApicEntry& entry) {
    const uint32_t apic_id = entry.apic_id;
    const bool is_primary = primary_apic_id_ == apic_id;

    const uint32_t pkg_id = decoder_.package_id(apic_id);
    const uint32_t die_id = decoder_.die_id(apic_id);
    const uint32_t core_id = decoder_.core_id(apic_id);
    const uint32_t smt_id = decoder_.smt_id(apic_id);
    const uint32_t cache_id = decoder_.cache_id(apic_id);

    if (pkg_id >= packages_.size()) {
      zx_status_t status = GrowVector(pkg_id + 1, &packages_);
      if (status != ZX_OK) {
        return status;
      }
    }
    auto& pkg = packages_[pkg_id];
    if (!pkg) {
      fbl::AllocChecker checker;
      pkg.reset(new (&checker) Package());
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }
    }

    Die* die = nullptr;
    zx_status_t status = pkg->GetDie(die_id, &die);
    if (status != ZX_OK) {
      return status;
    }

    SharedCache* cache = nullptr;
    if (decoder_.has_cache_info()) {
      zx_status_t status = die->GetCache(cache_id, &cache);
      if (status != ZX_OK) {
        return status;
      }
    }

    Core* core = nullptr;
    status = (cache != nullptr) ? cache->GetCore(core_id, &core) : die->GetCore(core_id, &core);
    if (status != ZX_OK) {
      return status;
    }

    const uint16_t logical_id = is_primary ? 0 : next_logical_id_++;
    core->SetPrimary(is_primary);
    core->AddThread(logical_id, apic_id);

    dprintf(INFO, "APIC: %#4x | Logical: %2u | Package: %2u | Die: %2u | Core: %2u | Thread: %2u |",
            apic_id, logical_id, pkg_id, die_id, core_id, smt_id);
    if (decoder_.has_cache_info()) {
      dprintf(INFO, " LLC: %2u |", cache_id);
    } else {
      dprintf(INFO, " LLC: %2s |", "?");
    }
    dprintf(INFO, "\n");
    return ZX_OK;
  }

  fbl::Vector<ktl::unique_ptr<Package>>& packages() { return packages_; }

 private:
  fbl::Vector<ktl::unique_ptr<Package>> packages_;
  uint32_t primary_apic_id_;
  const ApicDecoder& decoder_;
  uint16_t next_logical_id_ = 1;
};

zx_status_t GenerateTree(const cpu_id::CpuId& cpuid, const acpi_lite::AcpiParserInterface& parser,
                         const ApicDecoder& decoder,
                         fbl::Vector<ktl::unique_ptr<Package>>* packages) {
  // Get a list of all the APIC IDs in the system.
  PackageList pkg_list(cpuid, decoder);
  zx_status_t status = acpi_lite::EnumerateProcessorLocalApics(
      parser,
      [&pkg_list](const acpi_lite::AcpiMadtLocalApicEntry& value) { return pkg_list.Add(value); });
  if (status != ZX_OK) {
    return status;
  }

  *packages = ktl::move(pkg_list.packages());
  return ZX_OK;
}

zx_status_t AttachNumaInformation(const acpi_lite::AcpiParserInterface& parser,
                                  const ApicDecoder& decoder,
                                  fbl::Vector<ktl::unique_ptr<Package>>* packages) {
  return acpi_lite::EnumerateCpuNumaPairs(
      parser, [&decoder, packages](const acpi_lite::AcpiNumaDomain& domain, uint32_t apic_id) {
        const uint32_t pkg_id = decoder.package_id(apic_id);
        auto& pkg = (*packages)[pkg_id];
        if (!pkg) {
          dprintf(CRITICAL, "ERROR: could not find package #%u", pkg_id);
          return;
        }
        const uint32_t die_id = decoder.die_id(apic_id);
        Die* die = nullptr;
        if (zx_status_t status = pkg->GetDie(die_id, &die); status != ZX_OK) {
          dprintf(CRITICAL, "ERROR: could not find die #%u within package #%u", die_id, pkg_id);
          return;
        }
        if (die && !die->numa()) {
          die->SetNuma(domain);
        }
      });
}

zbi_topology_node_t ToFlatNode(const acpi_lite::AcpiNumaDomain& numa) {
  zbi_topology_node_t flat;
  flat.entity_type = ZBI_TOPOLOGY_ENTITY_NUMA_REGION;
  flat.parent_index = ZBI_TOPOLOGY_NO_PARENT;
  if (numa.memory_count > 0) {
    const auto& mem = numa.memory[0];
    flat.entity.numa_region.start_address = mem.base_address;
    flat.entity.numa_region.end_address = mem.base_address + mem.length;
  }
  return flat;
}

zx_status_t FlattenTree(const fbl::Vector<ktl::unique_ptr<Package>>& packages,
                        fbl::Vector<zbi_topology_node_t>* flat) {
  fbl::AllocChecker checker;
  for (auto& pkg : packages) {
    if (!pkg) {
      continue;
    }

    for (auto& die : pkg->dies()) {
      if (!die) {
        continue;
      }

      if (die->numa()) {
        const auto numa_flat_index = static_cast<uint16_t>(flat->size());
        flat->push_back(ToFlatNode(*die->numa()), &checker);
        if (!checker.check()) {
          return ZX_ERR_NO_MEMORY;
        }
        die->SetFlatParent(numa_flat_index);
      }

      const auto die_flat_index = static_cast<uint16_t>(flat->size());
      flat->push_back(die->node(), &checker);
      if (!checker.check()) {
        return ZX_ERR_NO_MEMORY;
      }

      auto insert_core = [&](const ktl::unique_ptr<Core>& core) {
        if (!core) {
          return ZX_OK;
        }

        flat->push_back(core->node(), &checker);
        if (!checker.check()) {
          return ZX_ERR_NO_MEMORY;
        }
        return ZX_OK;
      };

      for (auto& cache : die->caches()) {
        if (!cache) {
          continue;
        }

        cache->SetFlatParent(die_flat_index);
        const auto cache_flat_index = static_cast<uint16_t>(flat->size());
        flat->push_back(cache->node(), &checker);
        if (!checker.check()) {
          return ZX_ERR_NO_MEMORY;
        }

        // Add cores that are on a die with shared cache.
        for (auto& core : cache->cores()) {
          if (!core) {
            continue;
          }

          core->SetFlatParent(cache_flat_index);
          auto status = insert_core(core);
          if (status != ZX_OK) {
            return status;
          }
        }
      }

      // Add cores directly attached to die.
      for (auto& core : die->cores()) {
        if (!core) {
          continue;
        }

        core->SetFlatParent(die_flat_index);
        auto status = insert_core(core);
        if (status != ZX_OK) {
          return status;
        }
      }
    }
  }
  return ZX_OK;
}

// clang-format off
static constexpr zbi_topology_node_t kFallbackTopology = {
    .entity_type = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
    .parent_index = ZBI_TOPOLOGY_NO_PARENT,
    .entity = {
      .processor = {
        .logical_ids = {0},
        .logical_id_count = 1,
        .flags = ZBI_TOPOLOGY_PROCESSOR_PRIMARY,
        .architecture = ZBI_TOPOLOGY_ARCH_X86,
        .architecture_info = {
          .x86 = {
            .apic_ids = {0},
            .apic_id_count = 1,
          }
        }
      }
    }
};
// clang-format on

zx_status_t GenerateAndInitSystemTopology(const acpi_lite::AcpiParserInterface& parser) {
  fbl::Vector<zbi_topology_node_t> topology;

  const auto status = x86::GenerateFlatTopology(cpu_id::CpuId(), parser, &topology);
  if (status != ZX_OK) {
    dprintf(CRITICAL, "ERROR: failed to generate flat topology from cpuid and acpi data! : %d\n",
            status);
    return status;
  }

  return system_topology::Graph::InitializeSystemTopology(topology.data(), topology.size());
}

}  // namespace

namespace x86 {

zx_status_t GenerateFlatTopology(const cpu_id::CpuId& cpuid,
                                 const acpi_lite::AcpiParserInterface& parser,
                                 fbl::Vector<zbi_topology_node_t>* topology) {
  const auto decoder_opt = ApicDecoder::From(cpuid);
  if (!decoder_opt) {
    return ZX_ERR_INTERNAL;
  }

  const auto& decoder = *decoder_opt;
  fbl::Vector<ktl::unique_ptr<Package>> pkgs;
  auto status = GenerateTree(cpuid, parser, decoder, &pkgs);
  if (status != ZX_OK) {
    return status;
  }

  status = AttachNumaInformation(parser, decoder, &pkgs);
  if (status == ZX_ERR_NOT_FOUND) {
    // This is not a critical error. Systems, such as qemu, may not have the
    // tables present to enumerate NUMA information.
    dprintf(INFO, "System topology: Unable to attach NUMA information, missing ACPI tables.\n");
  } else if (status != ZX_OK) {
    return status;
  }

  return FlattenTree(pkgs, topology);
}

}  // namespace x86

void topology_init() {
  auto status = GenerateAndInitSystemTopology(GlobalAcpiLiteParser());
  if (status != ZX_OK) {
    dprintf(CRITICAL,
            "ERROR: Auto topology generation failed, falling back to only boot core! status: %d\n",
            status);
    status = system_topology::Graph::InitializeSystemTopology(&kFallbackTopology, 1);
    ZX_ASSERT(status == ZX_OK);
  }
}
