// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_MANAGER_NODE_H_
#define LIB_DRIVER_DEVICETREE_MANAGER_NODE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <zircon/errors.h>

#include <cstdint>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

namespace fdf_devicetree {

using Phandle = uint32_t;

class Visitor;
class ReferenceNode;
class ParentNode;
class ChildNode;

// Defines interface that an entity managing the Node should implement.
class NodeManager {
 public:
  // Returns node with phandle |id|.
  virtual zx::result<ReferenceNode> GetReferenceNode(Phandle id) = 0;
};

// Node represents the nodes in the device tree along with it's properties.
class Node {
 public:
  explicit Node(Node* parent, std::string_view name, devicetree::Properties properties, uint32_t id,
                NodeManager* manager);

  // Add |prop| as a bind property of the device, when it is eventually published.
  void AddBindProperty(fuchsia_driver_framework::NodeProperty prop);

  void AddMmio(fuchsia_hardware_platform_bus::Mmio mmio);

  void AddBti(fuchsia_hardware_platform_bus::Bti bti);

  void AddIrq(fuchsia_hardware_platform_bus::Irq irq);

  void AddMetadata(fuchsia_hardware_platform_bus::Metadata metadata);

  void AddBootMetadata(fuchsia_hardware_platform_bus::BootMetadata boot_metadata);

  void AddNodeSpec(fuchsia_driver_framework::ParentSpec spec);

  // Publish this node.
  // TODO(https://fxbug.dev/42059490): Switch to fdf::SyncClient when it's available.
  zx::result<> Publish(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus,
                       fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager>& mgr,
                       fidl::SyncClient<fuchsia_driver_framework::Node>& fdf_node);

  const std::string& name() const { return name_; }
  const std::string& fdf_name() const { return fdf_name_; }

  ParentNode parent() const;

  std::vector<ChildNode> children();

  const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties() const {
    return properties_;
  }

  zx::result<ReferenceNode> GetReferenceNode(Phandle parent);

  std::optional<Phandle> phandle() const { return phandle_; }

  uint32_t id() const { return id_; }

 private:
  Node* parent_;
  std::string name_;
  std::string fdf_name_;
  std::unordered_map<std::string_view, devicetree::PropertyValue> properties_;
  std::optional<Phandle> phandle_;
  std::vector<Node*> children_;

  // Platform bus node.
  fuchsia_hardware_platform_bus::Node pbus_node_;

  // Properties of the nodes after they have been transformed in the device group.
  std::vector<fuchsia_driver_framework::NodeProperty> node_properties_;

  // Parent specifications.
  std::vector<fuchsia_driver_framework::ParentSpec> parents_;

  // This is a unique ID we use to match our device group with the correct
  // platform bus node. It is generated at runtime and not stable across boots.
  uint32_t id_;

  // Boolean to indicate if a composite node spec needs to added.
  bool composite_ = false;

  // Boolean to indicate if a platform device needs to added.
  bool add_platform_device_ = false;

  // Storing handle to manager. This is ok as the manager always outlives the node instance.
  NodeManager* manager_;

  // Valid only when a non platform bus node is published.
  fidl::SyncClient<fuchsia_driver_framework::NodeController> node_controller_;
};

class ReferenceNode {
 public:
  explicit ReferenceNode(const Node* node) : node_(node) {}

  const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties() const {
    return node_->properties();
  }

  const std::string& name() const { return node_->name(); }
  const std::string& fdf_name() const { return node_->fdf_name(); }

  uint32_t id() const { return node_->id(); }

  std::optional<Phandle> phandle() const { return node_->phandle(); }

  ParentNode parent() const;

  explicit operator bool() const { return (node_ != nullptr); }

 private:
  const Node* node_;
};

class ParentNode {
 public:
  explicit ParentNode(Node* node) : node_(node) {}

  const std::string& name() const { return node_->name(); }
  const std::string& fdf_name() const { return node_->fdf_name(); }

  uint32_t id() const { return node_->id(); }

  explicit operator bool() const { return (node_ != nullptr); }

  const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties() const {
    return node_->properties();
  }

  ParentNode parent() const { return node_->parent(); }

  ReferenceNode MakeReferenceNode() const { return ReferenceNode(node_); }

 private:
  const Node* node_;
};

class ChildNode {
 public:
  explicit ChildNode(Node* node) : node_(node) {}

  const std::string& name() const { return node_->name(); }
  const std::string& fdf_name() const { return node_->fdf_name(); }

  uint32_t id() const { return node_->id(); }

  explicit operator bool() const { return (node_ != nullptr); }

  const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties() const {
    return node_->properties();
  }

  Node* GetNode() const { return node_; }

  void AddNodeSpec(fuchsia_driver_framework::ParentSpec spec) {
    node_->AddNodeSpec(std::move(spec));
  }

 private:
  Node* node_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_MANAGER_NODE_H_
