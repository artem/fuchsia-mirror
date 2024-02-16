// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/parent_set_collector.h"

#include "src/devices/lib/log/log.h"

namespace dfv2 {

zx::result<> ParentSetCollector::AddNode(uint32_t index, std::weak_ptr<Node> node) {
  ZX_ASSERT(index < parents_.size());
  if (!parents_[index].expired()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }
  parents_[index] = std::move(node);
  return zx::ok();
}

zx::result<std::shared_ptr<Node>> ParentSetCollector::TryToAssemble(
    NodeManager* node_manager, async_dispatcher_t* dispatcher) {
  if (completed_composite_node_ && !completed_composite_node_->expired()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  for (auto& node : parents_) {
    auto parent = node.lock();
    if (!parent) {
      return zx::error(ZX_ERR_SHOULD_WAIT);
    }
  }

  auto result =
      Node::CreateCompositeNode(composite_name_, parents_, parent_names_, {}, node_manager,
                                dispatcher, /* is_legacy */ false, primary_index_);
  if (result.is_error()) {
    return result.take_error();
  }

  LOGF(INFO, "Built composite node '%s' for completed composite node spec",
       composite_name_.c_str());
  completed_composite_node_.emplace(result.value());
  return zx::ok(result.value());
}

fidl::VectorView<fidl::StringView> ParentSetCollector::GetParentTopologicalPaths(
    fidl::AnyArena& arena) const {
  fidl::VectorView<fidl::StringView> parent_topological_paths(arena, parents_.size());
  for (uint32_t i = 0; i < parents_.size(); i++) {
    if (auto node = parents_[i].lock(); node) {
      parent_topological_paths[i] = fidl::StringView(arena, node->MakeTopologicalPath());
    } else {
      parent_topological_paths[i] = fidl::StringView();
    }
  }
  return parent_topological_paths;
}

}  // namespace dfv2
