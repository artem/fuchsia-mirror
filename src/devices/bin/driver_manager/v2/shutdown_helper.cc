// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/shutdown_helper.h"

#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"
#include "src/devices/lib/log/log.h"

namespace dfv2 {

namespace {

// Flag for enabling test delays in shutdown.
// TODO(fxb/134783): Set this value from the structured config.
const bool kEnableShutdownDelay = false;

// The range of test delay time in milliseconds.
constexpr uint32_t kMinTestDelayMs = 0;
constexpr uint32_t kMaxTestDelayMs = 5;

}  // namespace

ShutdownHelper::ShutdownHelper(NodeShutdownBridge* bridge, async_dispatcher_t* dispatcher)
    : bridge_(bridge), distribution_(kMinTestDelayMs, kMaxTestDelayMs), tasks_(dispatcher) {
  if (kEnableShutdownDelay) {
    // TODO(fxb/134783): Pass in a seed so that all ShutdownHelpers share the same one.
    rng_gen_ = std::mt19937(random_device_());
  }
}

void ShutdownHelper::Remove(std::shared_ptr<Node> node, RemovalSet removal_set,
                            NodeRemovalTracker* removal_tracker) {
  std::stack<std::shared_ptr<Node>> nodes_to_check_for_removal;
  std::stack<std::shared_ptr<Node>> nodes = std::stack<std::shared_ptr<Node>>({node});
  while (!nodes.empty()) {
    std::shared_ptr<Node> node = nodes.top();
    nodes.pop();

    ShutdownHelper& shutdown_helper = node->GetShutdownHelper();
    if (!removal_tracker && shutdown_helper.removal_tracker_) {
      // TODO(fxbug.dev/115171): Change this to an error when we track shutdown steps better.
      LOGF(WARNING, "Untracked Node::Remove() called on %s, indicating an error during shutdown",
           node->MakeTopologicalPath().c_str());
    }

    if (removal_tracker) {
      shutdown_helper.SetRemovalTracker(removal_tracker);
    }

    LOGF(DEBUG, "Remove called on Node: %s", node->name().c_str());
    // Two cases where we will transition state and take action:
    // Removing kAll, and state is Running or Prestop
    // Removing kPkg, and state is Running
    if ((shutdown_helper.node_state() != NodeState::kPrestop &&
         shutdown_helper.node_state() != NodeState::kRunning) ||
        (shutdown_helper.node_state() == NodeState::kPrestop &&
         removal_set == RemovalSet::kPackage)) {
      if (node->parents().size() <= 1) {
        LOGF(WARNING, "Node::Remove() %s called late, already in state %s",
             node->MakeComponentMoniker().c_str(), shutdown_helper.NodeStateAsString());
      }
      continue;
    }

    // Now, the cases where we do something:
    // Set the new state
    if (removal_set == RemovalSet::kPackage &&
        (node->collection() == Collection::kBoot || node->collection() == Collection::kNone)) {
      shutdown_helper.node_state_ = NodeState::kPrestop;
    } else {
      shutdown_helper.node_state_ = NodeState::kWaitingOnChildren;
      // Either removing kAll, or is package driver and removing kPackage.
      // All children should be removed regardless as they block removal of this node.
      removal_set = RemovalSet::kAll;
    }

    // Propagate removal message to children
    shutdown_helper.NotifyRemovalTracker();

    // Ask each of our children to remove themselves.
    for (auto& child : node->children()) {
      LOGF(DEBUG, "Node: %s calling remove on child: %s", node->name().c_str(),
           child->name().c_str());
      nodes.push(child);
    }
    nodes_to_check_for_removal.push(std::move(node));
  }

  while (!nodes_to_check_for_removal.empty()) {
    nodes_to_check_for_removal.top()->GetShutdownHelper().CheckNodeState();
    nodes_to_check_for_removal.pop();
  }
}

void ShutdownHelper::ResetShutdown() {
  ZX_ASSERT_MSG(node_state_ == NodeState::kStopped,
                "ShutdownHelper::ResetShutdown called in invalid node state: %s",
                NodeStateAsString());
  node_state_ = NodeState::kRunning;
  shutdown_intent_ = ShutdownIntent::kRemoval;
}

void ShutdownHelper::CheckNodeState() {
  if (is_transition_pending_) {
    return;
  }

  switch (node_state_) {
    case NodeState::kRunning:
    case NodeState::kPrestop:
    case NodeState::kStopped:
      return;
    case NodeState::kWaitingOnChildren: {
      CheckWaitingOnChildren();
      return;
    }
    case NodeState::kWaitingOnDriver: {
      CheckWaitingOnDriver();
      return;
    }
    case NodeState::kWaitingOnDriverComponent: {
      CheckWaitingOnDriverComponent();
      return;
    }
  }
}

void ShutdownHelper::CheckWaitingOnChildren() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnChildren,
                "ShutdownHelper::CheckWaitingOnChildren called in invalid node state: %s",
                NodeStateAsString());
  // Remain on this state if the node still has children.
  if (bridge_->HasChildren()) {
    return;
  }
  PerformTransition([this]() mutable {
    bridge_->StopDriver();
    UpdateAndNotifyState(NodeState::kWaitingOnDriver);
  });
}

void ShutdownHelper::CheckWaitingOnDriver() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnDriver,
                "ShutdownHelper::CheckWaitingOnDriver called in invalid node state: %s",
                NodeStateAsString());

  // Remain on this state if the node still has a driver set to it.
  if (bridge_->HasDriver()) {
    return;
  }
  PerformTransition([this]() mutable {
    bridge_->StopDriverComponent();
    UpdateAndNotifyState(NodeState::kWaitingOnDriverComponent);
  });
}

void ShutdownHelper::CheckWaitingOnDriverComponent() {
  ZX_ASSERT(!is_transition_pending_);
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnDriverComponent,
                "ShutdownHelper::CheckWaitingOnDriverComponent called in invalid node state: %s",
                NodeStateAsString());

  // Remain on this state if the node still has a driver component.
  if (bridge_->HasDriverComponent()) {
    return;
  }

  PerformTransition([this]() mutable {
    bridge_->FinishShutdown([this]() { UpdateAndNotifyState(NodeState::kStopped); });
  });
}

void ShutdownHelper::PerformTransition(fit::function<void()> action) {
  ZX_ASSERT(!is_transition_pending_);

  std::optional<uint32_t> delay_ms;
  if (kEnableShutdownDelay) {
    // Randomize a 20% chance for adding a shutdown delay.
    if (distribution_(rng_gen_) % 5 == 1) {
      delay_ms = distribution_(rng_gen_);
    }
  }

  if (!delay_ms) {
    action();
    return;
  }

  is_transition_pending_ = true;
  tasks_.Post([action = std::move(action), delay_amount = delay_ms.value()] {
    std::this_thread::sleep_for(std::chrono::microseconds(delay_amount));
    action();
  });
}

void ShutdownHelper::UpdateAndNotifyState(NodeState state) {
  node_state_ = state;
  is_transition_pending_ = false;
  NotifyRemovalTracker();
  CheckNodeState();
}

void ShutdownHelper::NotifyRemovalTracker() {
  if (!removal_tracker_ || removal_id_ == std::nullopt) {
    return;
  }
  removal_tracker_->Notify(removal_id_.value(), node_state_);
}

bool ShutdownHelper::IsShuttingDown() const { return node_state_ != NodeState::kRunning; }

const char* ShutdownHelper::NodeStateAsString() const {
  switch (node_state_) {
    case NodeState::kRunning:
      return "kRunning";
    case NodeState::kPrestop:
      return "kPrestop";
    case NodeState::kWaitingOnChildren:
      return "kWaitingOnChildren";
    case NodeState::kWaitingOnDriver:
      return "kWaitingOnDriver";
    case NodeState::kWaitingOnDriverComponent:
      return "kWaitingOnDriverComponent";
    case NodeState::kStopped:
      return "kStopped";
  }
}

void ShutdownHelper::SetRemovalTracker(NodeRemovalTracker* removal_tracker) {
  ZX_ASSERT(removal_tracker);
  if (removal_tracker_) {
    // We should never have two competing trackers
    ZX_ASSERT(removal_tracker_ == removal_tracker);
    return;
  }
  removal_tracker_ = removal_tracker;
  std::pair<std::string, Collection> node_info = bridge_->GetRemovalTrackerInfo();
  removal_id_ = removal_tracker->RegisterNode(NodeRemovalTracker::Node{
      .name = node_info.first,
      .collection = node_info.second,
      .state = node_state_,
  });
}

}  // namespace dfv2
