// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/component/cpp/fidl.h>

#include "src/devices/bin/driver_manager/v2/driver_host.h"
#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"
#include "src/devices/bin/driver_manager/v2/tests/driver_manager_test_base.h"

using namespace dfv2;

class TestRealm final : public fidl::WireServer<fuchsia_component::Realm> {
 public:
  TestRealm(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  zx::result<fidl::ClientEnd<fuchsia_component::Realm>> Connect() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_component::Realm>();
    if (endpoints.is_error()) {
      return zx::error(endpoints.status_value());
    }
    fidl::BindServer(dispatcher_, std::move(endpoints->server), this);
    return zx::ok(std::move(endpoints->client));
  }

  void OpenExposedDir(OpenExposedDirRequestView request,
                      OpenExposedDirCompleter::Sync& completer) override {
    ZX_ASSERT(false);
  }

  void CreateChild(CreateChildRequestView request, CreateChildCompleter::Sync& completer) override {
    ZX_ASSERT(false);
  }

  void DestroyChild(DestroyChildRequestView request,
                    DestroyChildCompleter::Sync& completer) override {
    destroy_completers_[std::string(request->child.name.data(), request->child.name.size())] =
        completer.ToAsync();
  }

  void ListChildren(ListChildrenRequestView request,
                    ListChildrenCompleter::Sync& completer) override {
    ZX_ASSERT(false);
  }

  void ReplyDestroyChildRequest(std::string child_moniker) {
    ASSERT_TRUE(destroy_completers_[child_moniker].has_value());
    destroy_completers_[child_moniker]->ReplySuccess();
    destroy_completers_[child_moniker].reset();
  }

 private:
  async_dispatcher_t* dispatcher_;

  std::unordered_map<std::string, std::optional<DestroyChildCompleter::Async>> destroy_completers_;
};

class FakeDriverHost : public DriverHost {
 public:
  using StartCallback = fit::callback<void(zx::result<>)>;
  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_name,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fuchsia_component_runner::wire::ComponentStartInfo start_info,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override {
    drivers_[node_name] = std::move(driver);
    clients_[node_name] = std::move(client_end);

    if (should_queue_start_callback_) {
      start_callbacks_[node_name] = std::move(cb);
      return;
    }
    cb(zx::ok());
  }

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  void CloseDriver(std::string node_name) {
    drivers_[node_name].Close(ZX_OK);
    clients_[node_name].reset();
  }

  void InvokeStartCallback(std::string node_name, zx::result<> result) {
    start_callbacks_[node_name](result);
    start_callbacks_.erase(node_name);
  }

  void set_should_queue_start_callback(bool should_queue) {
    should_queue_start_callback_ = should_queue;
  }

 private:
  bool should_queue_start_callback_ = false;
  std::unordered_map<std::string, StartCallback> start_callbacks_;

  std::unordered_map<std::string, fidl::ServerEnd<fuchsia_driver_host::Driver>> drivers_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_driver_framework::Node>> clients_;
};

class FakeNodeManager : public TestNodeManagerBase {
 public:
  FakeNodeManager(fidl::WireClient<fuchsia_component::Realm> realm) : realm_(std::move(realm)) {}

  zx::result<DriverHost*> CreateDriverHost() override { return zx::ok(&driver_host_); }

  void DestroyDriverComponent(
      Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {
    auto name = node.MakeComponentMoniker();
    fuchsia_component_decl::wire::ChildRef child_ref{
        .name = fidl::StringView::FromExternal(name),
        .collection = "",
    };
    realm_->DestroyChild(child_ref).Then(std::move(callback));
    clients_.erase(node.name());
  }

  void CloseDriverForNode(std::string node_name) { driver_host_.CloseDriver(node_name); }

  void AddClient(const std::string& node_name,
                 fidl::ClientEnd<fuchsia_component_runner::ComponentController> client) {
    clients_[node_name] = std::move(client);
  }

  FakeDriverHost& driver_host() { return driver_host_; }

 private:
  fidl::WireClient<fuchsia_component::Realm> realm_;
  std::unordered_map<std::string, fidl::ClientEnd<fuchsia_component_runner::ComponentController>>
      clients_;
  FakeDriverHost driver_host_;
};

class NodeShutdownTest : public DriverManagerTestBase {
 public:
  void SetUp() override {
    DriverManagerTestBase::SetUp();
    realm_ = std::make_unique<TestRealm>(dispatcher());

    auto client = realm_->Connect();
    ASSERT_TRUE(client.is_ok());
    node_manager = std::make_unique<FakeNodeManager>(
        fidl::WireClient<fuchsia_component::Realm>(std::move(client.value()), dispatcher()));

    removal_tracker_ = std::make_unique<NodeRemovalTracker>(dispatcher());
    removal_tracker_->set_all_callback([this]() { remove_all_callback_invoked_ = true; });

    nodes_["root"] = root();
  }

  void StartDriver(std::string node_name) {
    ASSERT_NE(nodes_.find(node_name), nodes_.end());

    std::vector<fuchsia_data::DictionaryEntry> program_entries = {
        {{
            .key = "binary",
            .value = std::make_unique<fuchsia_data::DictionaryValue>(
                fuchsia_data::DictionaryValue::WithStr("driver/library.so")),
        }},
        {{
            .key = "colocate",
            .value = std::make_unique<fuchsia_data::DictionaryValue>(
                fuchsia_data::DictionaryValue::WithStr("false")),
        }},
    };

    auto outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, outgoing_endpoints.status_value());

    auto start_info = fuchsia_component_runner::ComponentStartInfo{{
        .resolved_url = node_name,
        .program = fuchsia_data::Dictionary{{.entries = std::move(program_entries)}},
        .outgoing_dir = std::move(outgoing_endpoints->server),
    }};

    auto controller_endpoints =
        fidl::CreateEndpoints<fuchsia_component_runner::ComponentController>();

    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);

    node_manager->AddClient(node->name(), std::move(controller_endpoints->client));

    fidl::Arena arena;
    node->StartDriver(fidl::ToWire(arena, std::move(start_info)),
                      std::move(controller_endpoints->server), [](zx::result<> result) {});
  }

  void AddNode(std::string node) { AddChildNode("root", node); }
  void AddNodeAndStartDriver(std::string node) { AddChildNodeAndStartDriver("root", node); }

  void AddChildNode(std::string parent_name, std::string child_name) {
    // This function should only be called for a new node.
    ASSERT_EQ(nodes_.find(child_name), nodes_.end());
    ASSERT_NE(nodes_.find(parent_name), nodes_.end());

    // For testing purposes, the parent should not contain the children with the same node names.
    auto parent = nodes_[parent_name].lock();
    ASSERT_TRUE(parent);
    for (auto child : parent->children()) {
      ASSERT_NE(child->name(), child_name);
    }
    nodes_[child_name] = DriverManagerTestBase::CreateNode(child_name, nodes_[parent_name]);
  }

  void AddCompositeNode(std::string composite_name, std::vector<std::string> parents) {
    ASSERT_EQ(nodes_.find(composite_name), nodes_.end());

    std::vector<std::weak_ptr<Node>> parent_nodes;
    parent_nodes.reserve(parents.size());
    for (auto& parent_name : parents) {
      ASSERT_NE(nodes_.find(parent_name), nodes_.end());
      parent_nodes.push_back(nodes_[parent_name]);
    }
    nodes_[composite_name] = CreateCompositeNode(composite_name, parent_nodes,
                                                 /* is_legacy */ false, /* primary_index */ 0);
  }

  std::shared_ptr<Node> GetNode(std::string node_name) { return nodes_[node_name].lock(); }

  void AddChildNodeAndStartDriver(std::string parent, std::string child) {
    AddChildNode(parent, child);
    StartDriver(child);
  }

  void InvokeDestroyChildResponse(std::string node_name) {
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    realm_->ReplyDestroyChildRequest(node->MakeComponentMoniker());
    RunLoopUntilIdle();
  }

  void CloseDriverForNode(std::string node_name) {
    node_manager->CloseDriverForNode(node_name);
    RunLoopUntilIdle();
  }

  void InvokeRemoveNode(std::string node_name) {
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    node->Remove(RemovalSet::kAll, removal_tracker_.get());
    removal_tracker_->FinishEnumeration();
    RunLoopUntilIdle();
  }

  void VerifyState(std::string node_name, NodeState expected_state) {
    RunLoopUntilIdle();
    auto node = nodes_[node_name].lock();
    ASSERT_TRUE(node);
    ASSERT_EQ(expected_state, node->GetShutdownHelper().node_state());
  }

  void VerifyStates(std::map<std::string, NodeState> expected_states) {
    for (const auto& [node_name, state] : expected_states) {
      VerifyState(node_name, state);
    }
  }

  void VerifyNodeRemovedFromParent(std::string node_name, std::string parent_name) {
    RunLoopUntilIdle();
    ASSERT_FALSE(nodes_[node_name].lock());
    if (auto parent = nodes_[parent_name].lock(); parent) {
      for (auto child : parent->children()) {
        ASSERT_NE(child->name(), node_name);
      }
    }
  }

  void VerifyRemovalTrackerCallbackInvoked() { ASSERT_TRUE(remove_all_callback_invoked_); }

 protected:
  NodeManager* GetNodeManager() override { return node_manager.get(); }

  TestRealm* realm() { return realm_.get(); }

  std::unique_ptr<FakeNodeManager> node_manager;

 private:
  std::unique_ptr<NodeRemovalTracker> removal_tracker_;

  bool remove_all_callback_invoked_ = false;

  std::unordered_map<std::string, std::weak_ptr<Node>> nodes_;

  std::unique_ptr<TestRealm> realm_;
};

TEST_F(NodeShutdownTest, BasicRemoveAllNodes) {
  // TODO, include the removal tracker and its callback.
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveCompositeNode) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a", "node_a_c");

  AddCompositeNode("composite_abc", {"node_a_a", "node_a_b", "node_a_c"});
  StartDriver("composite_abc");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_c", NodeState::kWaitingOnChildren},
                {"composite_abc", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("composite_abc");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_c", NodeState::kWaitingOnChildren},
                {"composite_abc", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("composite_abc");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_a");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_b");
  VerifyNodeRemovedFromParent("composite_abc", "node_a_c");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnDriver},
                {"node_a_c", NodeState::kWaitingOnDriver}});

  auto remove_nodes = {"node_a_a", "node_a_b", "node_a_c"};
  for (auto node : remove_nodes) {
    CloseDriverForNode(node);
    InvokeDestroyChildResponse(node);
    VerifyNodeRemovedFromParent(node, "node_a");
  }

  VerifyState("node_a", NodeState::kWaitingOnDriver);
  CloseDriverForNode("node_a");
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");

  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveLeafNode) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");

  InvokeRemoveNode("node_a_a");

  VerifyState("node_a", NodeState::kRunning);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a_a");
  VerifyState("node_a_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveNodeWithNoChildren) {
  AddNodeAndStartDriver("node_a");
  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveNodeWithNoDriverOrChildren) {
  AddNode("node_a");  // No driver.
  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, DriverShutdownWhileWaitingOnChildren) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  // Close node_a's while it's still waiting for node_a_a. Node_a should
  // still wait for its children.
  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriver);

  // Close node_a_a's driver.
  CloseDriverForNode("node_a_a");
  VerifyState("node_a", NodeState::kWaitingOnChildren);
  VerifyState("node_a_a", NodeState::kWaitingOnDriverComponent);

  // Destroy node_a_a's driver component. Since node_a's driver was already
  // closed, it should go straight to destroying the driver component.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);

  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, RemoveAfterBindFailure) {
  AddNodeAndStartDriver("node_a");
  GetNode("node_a")->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, BindFailureDuringRemove) {
  AddNodeAndStartDriver("node_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  GetNode("node_a")->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, DriverHostFailure) {
  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver("node_a");
  node_manager->driver_host().InvokeStartCallback("node_a", zx::error(ZX_ERR_INTERNAL));

  InvokeRemoveNode("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
}

TEST_F(NodeShutdownTest, RemoveDuringDriverHostStartWithFailure) {
  node_manager->driver_host().set_should_queue_start_callback(true);
  AddNodeAndStartDriver("node_a");

  InvokeRemoveNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);
  node_manager->driver_host().InvokeStartCallback("node_a", zx::error(ZX_ERR_INTERNAL));

  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, OverlappingRemoveCalls) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}

TEST_F(NodeShutdownTest, OverlappingRemoveCalls_DifferentNodes) {
  AddNodeAndStartDriver("node_a");
  AddChildNodeAndStartDriver("node_a", "node_a_a");
  AddChildNodeAndStartDriver("node_a", "node_a_b");
  AddChildNodeAndStartDriver("node_a_b", "node_a_b_a");

  InvokeRemoveNode("node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriver},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_a", NodeState::kWaitingOnDriverComponent},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  // Close node_a_a's driver component. The node completes shutdown and should be removed.
  InvokeDestroyChildResponse("node_a_a");
  VerifyNodeRemovedFromParent("node_a_a", "node_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  InvokeRemoveNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b_a");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnChildren},
                {"node_a_b_a", NodeState::kWaitingOnDriverComponent}});

  // Close node_a_b_a's driver component. The node should complete shutdown and be removed.
  InvokeDestroyChildResponse("node_a_b_a");
  VerifyNodeRemovedFromParent("node_a_b_a", "node_a_b");
  VerifyStates(
      {{"node_a", NodeState::kWaitingOnChildren}, {"node_a_b", NodeState::kWaitingOnDriver}});

  CloseDriverForNode("node_a_b");
  VerifyStates({{"node_a", NodeState::kWaitingOnChildren},
                {"node_a_b", NodeState::kWaitingOnDriverComponent}});

  InvokeDestroyChildResponse("node_a_b");
  VerifyNodeRemovedFromParent("node_a_b", "node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriver);

  CloseDriverForNode("node_a");
  VerifyState("node_a", NodeState::kWaitingOnDriverComponent);
  InvokeDestroyChildResponse("node_a");
  VerifyNodeRemovedFromParent("node_a", "root");
  VerifyRemovalTrackerCallbackInvoked();
}
