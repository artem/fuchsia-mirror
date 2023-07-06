// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/a11y/lib/semantics/semantic_tree_service.h"

#include <fuchsia/accessibility/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/fd.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/event.h>

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/ui/a11y/bin/a11y_manager/tests/util/util.h"
#include "src/ui/a11y/lib/semantics/semantic_tree.h"
#include "src/ui/a11y/lib/semantics/tests/mocks/mock_semantic_listener.h"
#include "src/ui/a11y/lib/semantics/tests/semantic_tree_parser.h"
#include "src/ui/a11y/lib/util/util.h"

namespace accessibility_test {
namespace {

using fuchsia::accessibility::semantics::Node;
using fuchsia::accessibility::semantics::Role;
using ::testing::ElementsAre;

const int kMaxLogBufferSize = 1024;

class MockSemanticTree : public ::a11y::SemanticTree {
 public:
  bool Update(TreeUpdates updates) override {
    for (const auto& update : updates) {
      if (update.has_delete_node_id()) {
        deleted_node_ids_.push_back(update.delete_node_id());
        received_updates_.emplace_back(update.delete_node_id());
      } else if (update.has_node()) {
        Node copy1;
        Node copy2;
        update.node().Clone(&copy1);
        update.node().Clone(&copy2);
        updated_nodes_.push_back(std::move(copy1));
        received_updates_.emplace_back(std::move(copy2));
      }
    }
    if (reject_commit_) {
      return false;
    }
    return ::a11y::SemanticTree::Update(std::move(updates));
  }

  void OnSemanticsEvent(a11y::SemanticsEventInfo event_info) override { received_events_ = true; }

  void WillReturnFalseOnNextCommit() { reject_commit_ = true; }

  void ClearMockStatus() {
    received_updates_.clear();
    deleted_node_ids_.clear();
    updated_nodes_.clear();
    reject_commit_ = false;
    received_events_ = false;
  }

  TreeUpdates& received_updates() { return received_updates_; }

  std::vector<uint32_t>& deleted_node_ids() { return deleted_node_ids_; }

  std::vector<Node>& updated_nodes() { return updated_nodes_; }

  bool received_events() const { return received_events_; }

 private:
  // A copy of the updates sent to this tree.
  TreeUpdates received_updates_;

  std::vector<uint32_t> deleted_node_ids_;

  std::vector<Node> updated_nodes_;

  bool reject_commit_ = false;
  bool received_events_ = false;
};

const std::string kSemanticTreeSingleNodePath = "/pkg/data/semantic_tree_single_node.json";
const std::string kSemanticTreeOddNodesPath = "/pkg/data/semantic_tree_odd_nodes.json";

auto NodeIdEq(uint32_t node_id) { return testing::Property(&Node::node_id, node_id); }

class SemanticTreeServiceTest : public gtest::RealLoopFixture {
 public:
  SemanticTreeServiceTest() {}

 protected:
  void SetUp() override {
    RealLoopFixture::SetUp();
    // Create View Ref.
    zx::eventpair a;
    zx::eventpair::create(0u, &a, &b_);

    fuchsia::accessibility::semantics::SemanticListenerPtr semantic_listener;
    mock_semantic_listener_.Bind(semantic_listener);
    a11y::SemanticTreeService::CloseChannelCallback close_channel_callback(
        [this](zx_status_t status) {
          this->close_channel_called_ = true;
          this->close_channel_status_ = status;
        });
    auto tree = std::make_unique<MockSemanticTree>();
    tree_ptr_ = tree.get();
    semantic_tree_ = std::make_unique<a11y::SemanticTreeService>(
        std::move(tree), koid_, std::move(semantic_listener), std::move(close_channel_callback));
    semantic_tree_->EnableSemanticsUpdates(true);
  }

  std::vector<Node> BuildUpdatesFromFile(const std::string& file_path) {
    std::vector<Node> nodes;
    EXPECT_TRUE(semantic_tree_parser_.ParseSemanticTree(file_path, &nodes));
    return nodes;
  }

  void InitializeTreeNodesFromFile(const std::string& file_path) {
    ::a11y::SemanticTree::TreeUpdates updates;
    std::vector<Node> nodes;
    EXPECT_TRUE(semantic_tree_parser_.ParseSemanticTree(file_path, &nodes));
    for (auto& node : nodes) {
      updates.emplace_back(std::move(node));
    }
    EXPECT_TRUE(tree_ptr_->Update(std::move(updates)));
    tree_ptr_->ClearMockStatus();
  }

  sys::testing::ComponentContextProvider context_provider_;
  MockSemanticListener mock_semantic_listener_;
  std::unique_ptr<a11y::SemanticTreeService> semantic_tree_;
  MockSemanticTree* tree_ptr_ = nullptr;
  bool close_channel_called_ = false;
  zx_status_t close_channel_status_ = 0;
  zx_koid_t koid_ = 12345;
  SemanticTreeParser semantic_tree_parser_;

  // The event signaling pair member, used to invalidate the View Ref.
  zx::eventpair b_;
};

TEST_F(SemanticTreeServiceTest, IsSameViewReturnsTrueForTreeViewRef) {
  EXPECT_EQ(semantic_tree_->view_ref_koid(), koid_);
}

TEST_F(SemanticTreeServiceTest, UpdatesAreSentOnlyAfterCommit) {
  auto updates = BuildUpdatesFromFile(kSemanticTreeOddNodesPath);
  semantic_tree_->UpdateSemanticNodes(std::move(updates));
  EXPECT_TRUE(tree_ptr_->received_updates().empty());
  bool commit_called = false;
  auto callback = [&commit_called]() { commit_called = true; };
  semantic_tree_->CommitUpdates(std::move(callback));
  EXPECT_TRUE(commit_called);
  EXPECT_THAT(tree_ptr_->updated_nodes(),
              ElementsAre(NodeIdEq(0), NodeIdEq(1), NodeIdEq(2), NodeIdEq(3), NodeIdEq(4),
                          NodeIdEq(5), NodeIdEq(6)));
}

TEST_F(SemanticTreeServiceTest, InvalidTreeUpdatesClosesTheChannel) {
  auto updates = BuildUpdatesFromFile(kSemanticTreeOddNodesPath);
  tree_ptr_->WillReturnFalseOnNextCommit();
  semantic_tree_->UpdateSemanticNodes(std::move(updates));
  EXPECT_TRUE(tree_ptr_->received_updates().empty());
  bool commit_called = false;
  auto callback = [&commit_called]() { commit_called = true; };
  semantic_tree_->CommitUpdates(std::move(callback));
  EXPECT_TRUE(commit_called);
  // This commit failed, check if the callback to close the channel was invoked.
  EXPECT_TRUE(close_channel_called_);
  EXPECT_EQ(close_channel_status_, ZX_ERR_INVALID_ARGS);
}

TEST_F(SemanticTreeServiceTest, DeletesAreOnlySentAfterACommit) {
  auto updates = BuildUpdatesFromFile(kSemanticTreeOddNodesPath);
  semantic_tree_->UpdateSemanticNodes(std::move(updates));
  semantic_tree_->CommitUpdates([]() {});
  tree_ptr_->ClearMockStatus();

  semantic_tree_->DeleteSemanticNodes({5, 6});
  // Update the parent.
  std::vector<Node> new_updates;
  new_updates.emplace_back(CreateTestNode(2, "updated parent"));
  *new_updates.back().mutable_child_ids() = std::vector<uint32_t>();
  semantic_tree_->UpdateSemanticNodes(std::move(new_updates));
  semantic_tree_->CommitUpdates([]() {});
  EXPECT_THAT(tree_ptr_->deleted_node_ids(), ElementsAre(5, 6));
  EXPECT_THAT(tree_ptr_->updated_nodes(), ElementsAre(NodeIdEq(2)));
}

TEST_F(SemanticTreeServiceTest, EnableSemanticsUpdatesClearsTreeOnDisable) {
  InitializeTreeNodesFromFile(kSemanticTreeSingleNodePath);

  EXPECT_EQ(semantic_tree_->Get()->Size(), 1u);

  // Disable semantic updates and verify that tree is cleared.
  semantic_tree_->EnableSemanticsUpdates(false);

  EXPECT_EQ(semantic_tree_->Get()->Size(), 0u);
}

TEST_F(SemanticTreeServiceTest, SemanticEventsAreSentToTheTree) {
  fuchsia::accessibility::semantics::SemanticEvent event;
  bool callback_ran = false;
  semantic_tree_->SendSemanticEvent(std::move(event), [&callback_ran]() { callback_ran = true; });

  RunLoopUntilIdle();

  EXPECT_TRUE(callback_ran);
  EXPECT_TRUE(tree_ptr_->received_events());
}

TEST_F(SemanticTreeServiceTest, ActionRequested) {
  const uint32_t kNodeId = 1u;
  const fuchsia::accessibility::semantics::Action kAction =
      fuchsia::accessibility::semantics::Action::SECONDARY;

  mock_semantic_listener_.SetOnAccessibilityActionCallbackStatus(true);
  tree_ptr_->PerformAccessibilityAction(kNodeId, kAction, [this, kNodeId, kAction](bool result) {
    EXPECT_TRUE(result);
    EXPECT_TRUE(mock_semantic_listener_.OnAccessibilityActionRequestedCalled());
    EXPECT_EQ(mock_semantic_listener_.GetRequestedActionNodeId(), kNodeId);
    EXPECT_EQ(mock_semantic_listener_.GetRequestedAction(), kAction);
    QuitLoop();
  });

  RunLoop();
}

TEST_F(SemanticTreeServiceTest, HitTestRequested) {
  const uint32_t kNodeId = 2u;

  mock_semantic_listener_.SetOnAccessibilityActionCallbackStatus(true);
  mock_semantic_listener_.SetHitTestResult(kNodeId);
  tree_ptr_->PerformHitTesting({1.f, 2.f}, [this, kNodeId](auto hit) {
    ASSERT_TRUE(hit.has_node_id());
    EXPECT_EQ(hit.node_id(), kNodeId);
    QuitLoop();
  });

  RunLoop();
}

}  // namespace
}  // namespace accessibility_test
