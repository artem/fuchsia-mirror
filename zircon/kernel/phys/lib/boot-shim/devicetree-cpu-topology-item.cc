// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/stdcompat/algorithm.h>
#include <lib/zbi-format/cpu.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <cstdint>
#include <optional>

#include "lib/devicetree/devicetree.h"

namespace boot_shim {

devicetree::ScanState DevictreeCpuTopologyItem::OnNode(const devicetree::NodePath& path,
                                                       const devicetree::PropertyDecoder& decoder) {
  if (path == "/") {
    return devicetree::ScanState::kActive;
  }

  if (!path.IsDescendentOf("/cpus")) {
    if (path == "/cpus") {
      return devicetree::ScanState::kActive;
    }
    return devicetree::ScanState::kDoneWithSubtree;
  }

  // For both |cpus_| and |entries_| we must count the number of entries so we can allocate a
  // container for them. If the pointer is not yet set, then it means we are still counting.

  if (!path.IsDescendentOf("/cpus/cpu-map")) {
    if (path == "/cpus/cpu-map") {
      has_cpu_map_ = true;
      return devicetree::ScanState::kActive;
    }

    auto node_name = path.back().name();
    // Actual 'cpu' nodes whose content describes each CPU. If no 'cpu-map' node is present,
    // a CPU entry is synthesized for each element in |cpus_|.
    if (path.IsChildOf("/cpus") && node_name == "cpu") {
      // If we have allocated a buffer already, then fill the contents.
      return cpu_entries_ ? AddCpuNodeSecondScan(path, decoder)
                          : IncreaseCpuNodeCountFirstScan(path, decoder);
    }

    // If we are not in the 'cpu-map' or 'cpu' node, then dont go any further.
    return devicetree::ScanState::kDoneWithSubtree;
  }

  // |entries_| correspond to nodes under 'cpu-map' which reference 'cpu' nodes through a phandle on
  // either a 'core' or 'thread' node.
  // If we have allocated a buffer already, then fill the contents.
  return map_entries_ ? AddEntryNodeSecondScan(path, decoder)
                      : IncreaseEntryNodeCountFirstScan(path, decoder);
}

devicetree::ScanState DevictreeCpuTopologyItem::OnSubtree(const devicetree::NodePath& path) {
  // Clusters can contain other clusters, when exiting a cluster, restore the containing cluster
  // to cluster containing the current cluster if any.
  if (current_cluster_) {
    if (path.IsDescendentOf("/cpus/cpu-map") && IsCpuMapNode(path.back(), "cluster")) {
      // Restore the previous cluster.
      if (map_entries_) {
        current_cluster_ = map_entries_[*current_cluster_].cluster_index;
      } else {
        current_cluster_ = std::nullopt;
      }
    }
  }

  // Allocated and filled up, means we are done going through the tree.
  if (cpu_entries_ && cpu_entry_index_ == cpu_entry_count_) {
    if (!has_cpu_map_) {
      topology_node_count_ = cpu_entry_count_;
      map_entry_count_ = cpu_entry_count_ * 2;
      map_entries_ = Allocate<CpuMapEntry>(map_entry_count_);
      if (!map_entries_) {
        return devicetree::ScanState::kDone;
      }

      for (size_t i = 0; i < cpu_entry_count_; ++i) {
        const auto& cpu = cpu_entries_[i];
        // Synthesize 1 core - 1 thread pair for every cpu entry if no
        // cpu map is available.
        size_t map_index = 2 * i;
        map_entries_[map_index] = CpuMapEntry{
            .type = TopologyEntryType::kCore,
            // No parent.
            .parent_index = map_index,
        };
        map_entries_[map_index + 1] = CpuMapEntry{
            .type = TopologyEntryType::kThread,
            // No parent.
            .parent_index = map_index,
            .cpu_phandle = cpu.phandle,
            .cpu_index = i,
        };
      }
      return devicetree::ScanState::kDone;
    }

    if (map_entries_ && map_entry_index_ == map_entry_count_) {
      return devicetree::ScanState::kDone;
    }
  }

  // This is the post order visitor, if we are exiting the node that represents the 'cpus'
  // container, then we have visited all nodes we are interested in.
  //
  // We are either on the allocation or filling phase. If we are in the allocation phase,
  // allocate a buffer.
  if (path == "/cpus") {
    if (!cpu_entries_) {
      cpu_entries_ = Allocate<CpuEntry>(cpu_entry_count_);
    }
  } else if (path == "/cpus/cpu-map") {
    if (!map_entries_) {
      map_entries_ = Allocate<CpuMapEntry>(map_entry_count_);
    }
  }
  return devicetree::ScanState::kActive;
}

devicetree::ScanState DevictreeCpuTopologyItem::IncreaseEntryNodeCountFirstScan(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_ASSERT(!map_entries_);
  std::string_view name = path.back();

  // Nodes in the CPU map, are named differently that other nodes,
  // instead of socket@N it just uses socketN. Probably because N
  // is not an address but just an arbitrary ID.
  if (IsCpuMapNode(name, "socket")) {
    map_entry_count_++;
    topology_node_count_++;
    return devicetree::ScanState::kActive;
  }

  if (IsCpuMapNode(name, "cluster")) {
    map_entry_count_++;
    cluster_count_++;
    topology_node_count_++;
    return devicetree::ScanState::kActive;
  }

  if (IsCpuMapNode(name, "core")) {
    map_entry_count_++;
    topology_node_count_++;
    if (decoder.FindProperty("cpu")) {
      // Threads are omitted, need to generate a thread entry
      // for every core that has cpu on it.
      map_entry_count_++;
    }
    return devicetree::ScanState::kActive;
  }

  if (IsCpuMapNode(name, "thread")) {
    map_entry_count_++;
    return devicetree::ScanState::kActive;
  }

  return devicetree::ScanState::kDoneWithSubtree;
}

devicetree::ScanState DevictreeCpuTopologyItem::AddEntryNodeSecondScan(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_ASSERT(map_entries_);
  auto name = path.back().name();
  // Nodes in the CPU map, are named differently that other nodes,
  // instead of socket@N it just uses socketN. Probably because N
  // is not an address but just an arbitrary ID.
  if (IsCpuMapNode(name, "socket")) {
    map_entries_[map_entry_index_] = CpuMapEntry{
        .type = TopologyEntryType::kSocket,
    };
    current_socket_ = map_entry_index_;
    map_entry_index_++;
    return devicetree::ScanState::kActive;
  }

  if (IsCpuMapNode(name, "cluster")) {
    map_entries_[map_entry_index_] = CpuMapEntry{
        .type = TopologyEntryType::kCluster,
        .parent_index = current_cluster_.value_or(current_socket_.value_or(map_entry_index_)),
        .cluster_index = current_cluster_,
    };
    current_cluster_ = map_entry_index_;
    map_entry_index_++;
    return devicetree::ScanState::kActive;
  }

  auto get_cpu = [](const devicetree::PropertyDecoder& decoder) -> std::optional<uint32_t> {
    auto phandle = decoder.FindProperty("cpu");
    if (phandle) {
      return phandle->AsUint32();
    }
    return std::nullopt;
  };

  if (IsCpuMapNode(name, "core")) {
    map_entries_[map_entry_index_] = CpuMapEntry{
        .type = TopologyEntryType::kCore,
        .parent_index = current_cluster_.value_or(map_entry_index_),
        .cluster_index = current_cluster_,
    };
    current_core_ = map_entry_index_;
    map_entry_index_++;

    // If 'core' entry has a 'cpu' phandle, then the 'thread' entry has been omitted,
    // this means 1:1 between threads and cores. Let's synthesize the thread entry.
    if (auto cpu_phandle = get_cpu(decoder)) {
      map_entries_[map_entry_index_] = CpuMapEntry{
          .type = TopologyEntryType::kThread,
          .parent_index = *current_core_,
          .cluster_index = current_cluster_,
          .cpu_phandle = get_cpu(decoder),
      };
      map_entry_index_++;
    }

    return devicetree::ScanState::kActive;
  }

  if (IsCpuMapNode(name, "thread")) {
    map_entries_[map_entry_index_] = CpuMapEntry{
        .type = TopologyEntryType::kThread,
        .parent_index = current_core_.value_or(map_entry_index_),
        .cluster_index = current_cluster_,
        .cpu_phandle = get_cpu(decoder),
    };
    map_entry_index_++;
    return devicetree::ScanState::kActive;
  }
  ZX_ASSERT(map_entry_index_ <= map_entry_count_);

  return devicetree::ScanState::kDoneWithSubtree;
}

devicetree::ScanState DevictreeCpuTopologyItem::IncreaseCpuNodeCountFirstScan(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_ASSERT(!cpu_entries_);
  cpu_entry_count_++;
  return devicetree::ScanState::kActive;
}

devicetree::ScanState DevictreeCpuTopologyItem::AddCpuNodeSecondScan(
    const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
  ZX_ASSERT(cpu_entries_ && (cpu_entry_index_ < cpu_entry_count_));

  std::optional<uint32_t> phandle_val;
  auto [phandle, reg] = decoder.FindProperties("phandle", "reg");

  if (phandle) {
    phandle_val = phandle->AsUint32();
  }

  if (!reg) {
    OnError("CPU node missing 'reg' property.");
    return devicetree::ScanState::kDone;
  }

  auto reg_val = reg->AsReg(decoder);
  if (!reg_val) {
    OnError("Failed to decode CPU node 'reg' property.");
    return devicetree::ScanState::kDone;
  }

  // Properties are not copy or move assignable, so we must initialize in place.
  new (&cpu_entries_[cpu_entry_index_]) CpuEntry{
      .phandle = phandle_val,
      .properties = decoder.properties(),
  };
  cpu_entry_index_++;
  return devicetree::ScanState::kActive;
}

fit::result<ItemBase::DataZbi::Error> DevictreeCpuTopologyItem::UpdateEntryCpuLinks() const {
  ZX_ASSERT(cpu_entries_ && map_entries_);

  // Not every devicetree defines a CPU map. When this happens, the entry nodes have been
  // generated from the cpu nodes and there is nothing else to do, since the cpu index is the
  // same as the entry index.
  if (!has_cpu_map_) {
    return fit::ok();
  }

  // Resolve reference to CPU nodes from the cpu map.
  cpp20::span cpus(cpu_entries_, cpu_entry_count_);
  cpp20::span entries(map_entries_, map_entry_count_);

  uint32_t current_cpu = 0;

  struct CpuByPhandle {
    uint32_t phandle = 0;
    uint32_t cpu_index = 0;
    bool present = false;
  };

  // sorted phandle to CPU index for lookup.
  auto* cpu_table = Allocate<CpuByPhandle>(cpu_entry_count_);
  if (!cpu_table) {
    return fit::error(DataZbi::Error{
        .zbi_error = "Failed to allocate scratch buffer for CPU look up.",
        .item_offset = 0,
    });
  }

  cpp20::span<CpuByPhandle> cpu_phandle_to_index(cpu_table, cpu_entry_count_);
  for (auto& [phandle, index, present] : cpu_phandle_to_index) {
    const auto& cpu = cpus[current_cpu];
    present = cpu.phandle.has_value();
    if (present) {
      phandle = *cpu.phandle;
      index = current_cpu;
    }
    current_cpu++;
  }

  cpp20::sort(cpu_phandle_to_index.begin(), cpu_phandle_to_index.end(),
              [](const auto& a, const auto& b) {
                return a.present && (!b.present || a.phandle <= b.phandle);
              });
  auto get_cpu_index =
      [cpu_phandle_to_index](std::optional<uint32_t> phandle) -> std::optional<uint32_t> {
    if (!phandle) {
      return std::nullopt;
    }
    auto index = std::lower_bound(cpu_phandle_to_index.begin(), cpu_phandle_to_index.end(),
                                  *phandle, [](const auto& element, const auto& phandle) {
                                    return !element.present || element.phandle < phandle;
                                  });
    if (index == cpu_phandle_to_index.end()) {
      return std::nullopt;
    }
    return index->cpu_index;
  };

  // Resolve CPU indices in the entries.
  for (auto& entry : entries) {
    // Only core or thread may have a reference to a cpu node.
    auto cpu_index = get_cpu_index(entry.cpu_phandle);
    if (!cpu_index) {
      continue;
    }
    entry.cpu_index = *cpu_index;
  }

  return fit::ok();
}

fit::result<ItemBase::DataZbi::Error> DevictreeCpuTopologyItem::CalculateClusterPerformanceClass(
    cpp20::span<zbi_topology_node_t> nodes) const {
  if (cluster_count_ <= 1) {
    return fit::ok();
  }

  cpp20::span cpus(cpu_entries_, cpu_entry_count_);
  cpp20::span entries(map_entries_, map_entry_count_);

  struct ClusterPerf {
    size_t cluster_index = 0;
    size_t cluster_parent = 0;
    uint32_t perf = 1;
  };

  auto* cluster_performance = Allocate<ClusterPerf>(cluster_count_);
  if (!cluster_performance) {
    return fit::error(DataZbi::Error{.zbi_error = "Failed to allocate scratch space."});
  }

  cpp20::span<ClusterPerf> perf(cluster_performance, cluster_count_);
  size_t current_cluster = 0;
  uint32_t max_cap = 1;
  for (size_t i = 0; i < entries.size(); ++i) {
    const auto& entry = entries[i];
    if (entry.type == TopologyEntryType::kCluster) {
      perf[current_cluster].cluster_index = static_cast<uint32_t>(i);
      perf[current_cluster].perf = 1;
      perf[current_cluster].cluster_parent = i;

      if (entry.cluster_index) {
        for (size_t j = current_cluster - 1; j >= 0 && j < current_cluster; --j) {
          if (perf[j].cluster_index == *entry.cluster_index) {
            perf[current_cluster].cluster_parent = j;
            break;
          }
        }
      }
      current_cluster++;

      continue;
    }
    // Not a Thread.
    if (!entry.cpu_index) {
      continue;
    }

    // Self-referential.
    if (entry.parent_index == i) {
      continue;
    }

    devicetree::PropertyDecoder decoder(cpus[*entry.cpu_index].properties);
    auto capacity = decoder.FindProperty("capacity-dmips-mhz");

    if (!capacity) {
      continue;
    }

    auto capacity_value = capacity->AsUint32();
    if (!capacity_value) {
      continue;
    }

    auto* cluster_perf = &perf[current_cluster - 1];
    size_t cluster_perf_index = current_cluster - 1;
    if (cluster_perf->perf < *capacity_value) {
      cluster_perf->perf = *capacity_value;
      // Bubble the performance toward parent clusters.
      while (cluster_perf->cluster_parent != perf[cluster_perf_index].cluster_index) {
        cluster_perf_index = cluster_perf->cluster_parent;
        cluster_perf = &perf[cluster_perf->cluster_parent];
        if (cluster_perf->perf >= *capacity_value) {
          break;
        }
        cluster_perf->perf = *capacity_value;
      }
      max_cap = std::max(cluster_perf->perf, max_cap);
    }
  }

  auto normalize_value = [](uint64_t real, uint32_t max) {
    uint64_t scaled = real * 255;
    uint8_t normalized = static_cast<uint8_t>(scaled / max);
    return std::max<uint8_t>(1, normalized);
  };

  // Normalize
  for (const auto& cluster_perf : perf) {
    nodes[*entries[cluster_perf.cluster_index].topology_node_index]
        .entity.cluster.performance_class = normalize_value(cluster_perf.perf, max_cap);
  }

  return fit::ok();
}

fit::result<DevictreeCpuTopologyItem::DataZbi::Error> DevictreeCpuTopologyItem::AppendItems(
    DataZbi& zbi) const {
  ZX_ASSERT(cpu_entries_ && cpu_entry_count_);
  ZX_DEBUG_ASSERT(arch_info_setter_);
  // Resolve reference to CPU nodes from the cpu map.
  cpp20::span cpus(cpu_entries_, cpu_entry_count_);

  // If there is no 'cpu-map' then we are going to synthesize |cpu_count_| elements
  // and move on.
  cpp20::span entries(map_entries_, map_entry_count_);

  if (auto result = UpdateEntryCpuLinks(); result.is_error()) {
    return result;
  }

  // Allocate the container in the zbi.
  auto result = zbi.Append({
      .type = ZBI_TYPE_CPU_TOPOLOGY,
      .length = static_cast<uint32_t>(node_element_count() * sizeof(zbi_topology_node_t)),
  });
  if (result.is_error()) {
    return result.take_error();
  }

  auto [header, payload] = **result;
  cpp20::span topology_nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                             node_element_count());

  size_t current_node = 0;
  uint16_t logical_cpu_id = 0;
  std::optional<size_t> cpu_zero_node_index;
  for (size_t entry_index = 0; entry_index < entries.size(); ++entry_index) {
    auto& entry = entries[entry_index];

    if (entry.type == TopologyEntryType::kThread) {
      if (entry.cpu_index) {
        size_t core_node_index = *entries[entry.parent_index].topology_node_index;
        auto& core_node = topology_nodes[core_node_index].entity.processor;
        if (logical_cpu_id == 0) {
          cpu_zero_node_index = core_node_index;
          core_node.flags |= ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
        }

        core_node.logical_ids[core_node.logical_id_count] = logical_cpu_id++;
        core_node.logical_id_count++;
        arch_info_setter_(core_node, cpus[*entry.cpu_index]);

        if (core_node.flags == ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY) {
          topology_nodes[*cpu_zero_node_index].entity.processor.logical_ids[0] = logical_cpu_id - 1;
          core_node.logical_ids[core_node.logical_id_count - 1] = 0;
        }
      } else {
        const_cast<DevictreeCpuTopologyItem*>(this)->OnError(
            "'thread' entry without an associated 'cpu' entry.");
      }
      continue;
    }

    // Self referencing nodes have no parent.
    auto& node = topology_nodes[current_node];
    node.parent_index =
        entry.parent_index == entry_index
            ? ZBI_TOPOLOGY_NO_PARENT
            : static_cast<uint16_t>(*entries[entry.parent_index].topology_node_index);

    switch (entry.type) {
      case TopologyEntryType::kSocket:
        node.entity.discriminant = ZBI_TOPOLOGY_ENTITY_SOCKET;
        node.entity.socket = {};
        entry.topology_node_index = current_node;
        break;

      case TopologyEntryType::kCluster:
        node.entity.discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER;
        node.entity.cluster.performance_class = 1;
        entry.topology_node_index = current_node;
        break;

      case TopologyEntryType::kCore:
        // Add an empty entry for thread entries to fill up.
        node.entity = {
            .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
            .processor =
                {
                    .flags = 0,
                    .logical_ids = {},
                    .logical_id_count = 0,
                },
        };
        entry.topology_node_index = current_node;
        break;

      // Thread entries are handled separately, because they update existing entries,
      // and not generate a new one.
      case TopologyEntryType::kThread:
        __UNREACHABLE;
        break;
    };
    current_node++;
  }
  return CalculateClusterPerformanceClass(topology_nodes);
}

}  // namespace boot_shim
