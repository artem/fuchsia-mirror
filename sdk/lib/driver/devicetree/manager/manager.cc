// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "manager.h"

#include <fidl/fuchsia.boot/cpp/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/result.h>

#include <cstddef>
#include <list>

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace {
std::string GetPath(const devicetree::NodePath& node_path) {
  std::string path;
  for (std::string_view p : node_path) {
    // Skip adding '/' for the root node.
    if (path.length() != 1) {
      path.append("/");
    }
    path.append(p);
  }
  return path;
}

std::string GetParentPath(const devicetree::NodePath& node_path) {
  if (node_path.size() <= 1) {
    // root node.
    return "";
  }

  std::string path;
  auto it = node_path.begin();
  for (size_t i = 0; i < (node_path.size() - 1); i++, it++) {
    // Skip adding '/' for the root node.
    if (path.length() != 1) {
      path.append("/");
    }
    path.append(it->data());
  }
  return path;
}

}  // namespace

namespace fdf_devicetree {

zx::result<Manager> Manager::CreateFromNamespace(fdf::Namespace& ns) {
  zx::result client_end = ns.Connect<fuchsia_boot::Items>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.boot.Items: %s", client_end.status_string());
    return client_end.take_error();
  }

  fidl::WireSyncClient client(std::move(client_end.value()));
  fidl::WireResult result = client->Get2(ZBI_TYPE_DEVICETREE, {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send get2 request: %s", result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to get2: %s", zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }

  fidl::VectorView items = result->value()->retrieved_items;
  if (items.count() != 1) {
    FDF_LOG(ERROR, "Found wrong number of devicetrees: wanted 1, got %zu", items.count());
    // Temporary hack for u-boot changes on vim3. See b/297919767.
    // For now will continue execution and use the first devicetree.
    // return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_boot::wire::RetrievedItems& dt = result->value()->retrieved_items[0];
  std::vector<uint8_t> data;
  data.resize(dt.length);

  zx_status_t status = dt.payload.read(data.data(), 0, dt.length);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read %u bytes from the devicetree: %s", dt.length,
            zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(Manager(std::move(data)));
}

zx::result<> Manager::Walk(Visitor& visitor) {
  // Walk the tree and create all nodes before calling the visitor. This is required for
  // |GetReferenceNode| method to work properly.
  tree_.Walk([&, this](const devicetree::NodePath& path,
                       const devicetree::PropertyDecoder& decoder) {
    FDF_LOG(DEBUG, "Found node - %.*s", static_cast<int>(path.back().length()), path.back().data());

    Node* parent = nullptr;
    if (path != "/") {
      parent = nodes_by_path_[GetParentPath(path)];
    }

    // Create a node.
    const devicetree::Properties& properties = decoder.properties();
    auto node = std::make_unique<Node>(parent, path.back(), properties, node_id_++, this);
    Node* ptr = node.get();
    nodes_publish_order_.emplace_back(std::move(node));
    FDF_LOG(DEBUG, "Node[%d] - %s added for publishing", node_id_, path.back().data());

    if (ptr->phandle()) {
      nodes_by_phandle_.emplace(*(ptr->phandle()), ptr);
    }
    nodes_by_path_.emplace(GetPath(path), ptr);
    return true;
  });

  zx::result<> visit_status = zx::ok();
  tree_.Walk(
      [&, this](const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
        FDF_LOG(DEBUG, "Visit node - %.*s", static_cast<int>(path.back().length()),
                path.back().data());
        auto node = nodes_by_path_[GetPath(path)];
        visit_status = visitor.Visit(*node, decoder);
        if (visit_status.is_error()) {
          FDF_SLOG(ERROR, "Node visit failed.", KV("node_name", node->name()),
                   KV("status_str", visit_status.status_string()));
          return false;
        }
        return true;
      });
  if (visit_status.is_error()) {
    return visit_status;
  }

  // Call |FinalizeNode| method of the visitor on all nodes to complete the parsing. At this point
  // all references to the node is known and so the visitor can use that information to update any
  // Node properties if needed.
  for (auto& node : nodes_publish_order_) {
    FDF_LOG(DEBUG, "Finalize node - %.*s", static_cast<int>(node->name().length()),
            node->name().data());
    zx::result finalize_status = visitor.FinalizeNode(*node);
    if (finalize_status.is_error()) {
      FDF_SLOG(ERROR, "Node finalize failed.", KV("node_name", node->name()),
               KV("status_str", finalize_status.status_string()));
      return finalize_status;
    }
  }

  return zx::ok();
}

zx::result<> Manager::PublishDevices(
    fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus,
    fidl::ClientEnd<fuchsia_driver_framework::CompositeNodeManager> mgr) {
  auto pbus_client = fdf::WireSyncClient(std::move(pbus));
  auto mgr_client = fidl::SyncClient(std::move(mgr));

  for (auto& node : nodes_publish_order_) {
    zx::result<> status = node->Publish(pbus_client, mgr_client);
    if (status.is_error()) {
      return status.take_error();
    }
  }

  return zx::ok();
}

zx::result<ReferenceNode> Manager::GetReferenceNode(Phandle id) {
  auto node = nodes_by_phandle_.find(id);
  if (node == nodes_by_phandle_.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  return zx::ok(ReferenceNode(node->second));
}

}  // namespace fdf_devicetree
