// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_H_
#define LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>

#include <unordered_map>
#include <vector>

#include "lib/driver/devicetree/manager/visitor.h"

namespace fdf_devicetree {

class Manager final : public NodeManager {
 public:
  static zx::result<Manager> CreateFromNamespace(fdf::Namespace& ns);

  // Create a new device tree manager using the given FDT blob.
  explicit Manager(std::vector<uint8_t> fdt_blob)
      : fdt_blob_(std::move(fdt_blob)),
        tree_(devicetree::ByteView{fdt_blob_.data(), fdt_blob_.size()}) {}

  // This method does the following things -
  //   * Does the initial walk of the tree and discovers devices/nodes.
  //   * Calls |visitor.Visit()| for each node in the tree.
  //     If |visitor.Visit()| returns something that's not `zx::ok()`, then this
  //     will stop walking and return the error code.
  //
  // This method can be called with the |DefaultVisitors| or with a collection
  // of visitors of user's choice.
  // Example:
  //   DefaultVisitors<> visitors;
  //   auto result = manager.Walk(visitors)
  //
  // This needs to be called before |PublishDevices|.
  zx::result<> Walk(Visitor& visitor);

  // Publish the discovered devices.
  // The devices maybe added as a platform device using |pbus_client| if it contains any platform
  // resources, or it maybe added as a child of the board driver using |fdf_node|, or it maybe added
  // as a composite of multiple devices if it references other nodes.
  zx::result<> PublishDevices(
      fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus_client,
      fidl::ClientEnd<fuchsia_driver_framework::CompositeNodeManager> mgr,
      fidl::SyncClient<fuchsia_driver_framework::Node>& fdf_node);

  const std::vector<std::unique_ptr<Node>>& nodes() { return nodes_publish_order_; }

  // Returns node with phandle |id|.
  zx::result<ReferenceNode> GetReferenceNode(Phandle id) override;

  // Returns index of the node in the publish list.
  uint32_t GetPublishIndex(uint32_t node_id) override;

  // Moves the node with |node_id| to the |new_index| in the publish list.
  zx::result<> ChangePublishOrder(uint32_t node_id, uint32_t new_index) override;

 private:
  std::vector<uint8_t> fdt_blob_;
  devicetree::Devicetree tree_;

  // List of nodes, in the order that they were seen in the tree.
  std::vector<std::unique_ptr<Node>> nodes_publish_order_;
  // Nodes by phandle. Note that not every node in the tree has a phandle.
  std::unordered_map<Phandle, Node*> nodes_by_phandle_;
  // Nodes by path.
  std::unordered_map<std::string, Node*> nodes_by_path_;
  uint32_t node_id_ = 0;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_H_
