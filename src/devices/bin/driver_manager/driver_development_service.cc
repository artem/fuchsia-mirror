// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_development_service.h"

#include <fidl/fuchsia.driver.framework/cpp/wire_types.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/internal/transport.h>

#include <queue>
#include <unordered_set>

#include "src/devices/bin/driver_manager/driver_development/info_iterator.h"
#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace driver_manager {

namespace {

zx::result<fdd::wire::NodeInfo> CreateDeviceInfo(fidl::AnyArena& allocator,
                                                 const driver_manager::Node* node) {
  auto device_info = fdd::wire::NodeInfo::Builder(allocator);

  device_info.id(reinterpret_cast<uint64_t>(node));

  const auto& children = node->children();
  fidl::VectorView<uint64_t> child_ids(allocator, children.size());
  size_t i = 0;
  for (const auto& child : children) {
    child_ids[i++] = reinterpret_cast<uint64_t>(child.get());
  }
  if (!child_ids.empty()) {
    device_info.child_ids(child_ids);
  }

  const auto& parents = node->parents();
  fidl::VectorView<uint64_t> parent_ids(allocator, parents.size());
  i = 0;
  for (const auto& parent : parents) {
    auto parent_ptr = parent.lock();
    if (!parent_ptr) {
      LOGF(ERROR, "Parent node freed before it could be used");
      return zx::error(ZX_ERR_INTERNAL);
    }
    parent_ids[i++] = reinterpret_cast<uint64_t>(parent_ptr.get());
  }
  if (!parent_ids.empty()) {
    device_info.parent_ids(parent_ids);
  }

  auto v2_info_builder = fuchsia_driver_development::wire::V2NodeInfo::Builder(allocator);

  v2_info_builder.moniker(fidl::StringView(allocator, node->MakeComponentMoniker()));
  device_info.moniker(fidl::StringView(allocator, node->MakeComponentMoniker()));

  device_info.bound_driver_url(fidl::StringView(allocator, node->driver_url()));

  auto properties = node->properties();
  if (!properties.empty()) {
    fidl::VectorView<fdf::wire::NodeProperty> node_properties(allocator, properties.size());
    for (size_t i = 0; i < properties.size(); ++i) {
      node_properties[i] = fdf::wire::NodeProperty{
          .key = fidl::ToWire(allocator, fidl::ToNatural(properties[i].key)),
          .value = fidl::ToWire(allocator, fidl::ToNatural(properties[i].value)),
      };
    }
    v2_info_builder.node_property_list(node_properties);
    device_info.node_property_list(node_properties);
  }

  // TODO(https://fxbug.dev/42172220): Get topological path

  auto driver_host = node->driver_host();
  if (node->is_bound() && driver_host) {
    auto result = driver_host->GetProcessKoid();
    if (result.is_error()) {
      // ZX_ERR_SHOULD_WAIT means the driver host hasn't been fully connected to yet.
      // ZX_ERR_PEER_CLOSED means the host closed while we tried to query it.
      if (result.error_value() != ZX_ERR_SHOULD_WAIT &&
          result.error_value() != ZX_ERR_PEER_CLOSED) {
        LOGF(ERROR, "Failed to get the process KOID of a driver host: %s",
             zx_status_get_string(result.status_value()));
        return zx::error(result.status_value());
      }
    } else {
      device_info.driver_host_koid(result.value());
    }
  }

  // Copy over the offers.
  auto offers = node->offers();
  if (!offers.empty()) {
    fidl::VectorView<fuchsia_component_decl::wire::Offer> node_offers(allocator, offers.count());
    for (size_t i = 0; i < offers.count(); i++) {
      node_offers[i] = fidl::ToWire(allocator, fidl::ToNatural(offers[i]));
    }
  }
  v2_info_builder.offer_list(offers);
  device_info.offer_list(offers);

  auto versioned_info = fuchsia_driver_development::wire::VersionedNodeInfo::WithV2(
      allocator, v2_info_builder.Build());
  device_info.versioned_info(versioned_info);

  return zx::ok(device_info.Build());
}

}  // namespace

DriverDevelopmentService::DriverDevelopmentService(driver_manager::DriverRunner& driver_runner,
                                                   async_dispatcher_t* dispatcher)
    : driver_runner_(driver_runner), dispatcher_(dispatcher) {}

void DriverDevelopmentService::Publish(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<fdd::Manager>(
      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());
}

void DriverDevelopmentService::GetNodeInfo(GetNodeInfoRequestView request,
                                           GetNodeInfoCompleter::Sync& completer) {
  auto arena = std::make_unique<fidl::Arena<512>>();
  std::vector<fdd::wire::NodeInfo> device_infos;

  std::unordered_set<const driver_manager::Node*> unique_nodes;
  std::queue<const driver_manager::Node*> remaining_nodes;
  remaining_nodes.push(driver_runner_.root_node().get());
  while (!remaining_nodes.empty()) {
    auto node = remaining_nodes.front();
    remaining_nodes.pop();
    auto [_, inserted] = unique_nodes.insert(node);
    if (!inserted) {
      // Only insert unique nodes from the DAG.
      continue;
    }
    const auto& children = node->children();
    for (const auto& child : children) {
      remaining_nodes.push(child.get());
    }

    std::string moniker = node->MakeComponentMoniker();
    if (!request->node_filter.empty()) {
      bool found = false;
      for (const auto& node_filter : request->node_filter) {
        if (request->exact_match) {
          if (moniker == node_filter.get()) {
            found = true;
            break;
          }
        } else {
          if (moniker.find(node_filter.get()) != std::string::npos) {
            found = true;
            break;
          }
        }
      }
      if (!found) {
        continue;
      }
    }

    auto result = CreateDeviceInfo(*arena, node);
    if (result.is_error()) {
      return;
    }
    device_infos.push_back(std::move(result.value()));
  }
  auto iterator = std::make_unique<driver_development::DeviceInfoIterator>(std::move(arena),
                                                                           std::move(device_infos));
  fidl::BindServer(
      this->dispatcher_, std::move(request->iterator), std::move(iterator),
      [](auto* self, fidl::UnbindInfo info, fidl::ServerEnd<fdd::NodeInfoIterator> server_end) {
        if (info.is_user_initiated()) {
          return;
        }
        if (info.is_peer_closed()) {
          // For this development protocol, the client is free to disconnect
          // at any time.
          return;
        }
        LOGF(ERROR, "Error serving '%s': %s", fidl::DiscoverableProtocolName<fdd::Manager>,
             info.FormatDescription().c_str());
      });
}

void DriverDevelopmentService::GetCompositeInfo(GetCompositeInfoRequestView request,
                                                GetCompositeInfoCompleter::Sync& completer) {
  auto arena = std::make_unique<fidl::Arena<512>>();
  std::vector<fdd::wire::CompositeNodeInfo> list = driver_runner_.GetCompositeListInfo(*arena);
  auto iterator = std::make_unique<driver_development::CompositeInfoIterator>(std::move(arena),
                                                                              std::move(list));
  fidl::BindServer(this->dispatcher_, std::move(request->iterator), std::move(iterator),
                   [](auto* server, fidl::UnbindInfo info, auto channel) {
                     if (!info.is_peer_closed()) {
                       LOGF(WARNING, "Closed CompositeInfoIterator: %s", info.lossy_description());
                     }
                   });
}

void DriverDevelopmentService::GetDriverInfo(GetDriverInfoRequestView request,
                                             GetDriverInfoCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_index::DevelopmentManager>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_index::DevelopmentManager>,
         driver_index_client.status_string());
    request->iterator.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto info_result =
      driver_index->GetDriverInfo(std::move(request->driver_filter), std::move(request->iterator));
  if (!info_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::GetDriverInfo: %s\n",
         info_result.error().FormatDescription().data());
  }
}

void DriverDevelopmentService::GetCompositeNodeSpecs(
    GetCompositeNodeSpecsRequestView request, GetCompositeNodeSpecsCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_index::DevelopmentManager>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_index::DevelopmentManager>,
         driver_index_client.status_string());
    request->iterator.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto info_result =
      driver_index->GetCompositeNodeSpecs(request->name_filter, std::move(request->iterator));
  if (!info_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::GetCompositeNodeSpecs: %s\n",
         info_result.error().FormatDescription().data());
  }
}

void DriverDevelopmentService::DisableDriver(DisableDriverRequestView request,
                                             DisableDriverCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_index::DevelopmentManager>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_index::DevelopmentManager>,
         driver_index_client.status_string());
    completer.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto disable_result = driver_index->DisableDriver(request->driver_url, request->package_hash);
  if (!disable_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::DisableDriver: %s\n",
         disable_result.FormatDescription().c_str());
    completer.Close(disable_result.error().status());
    return;
  }

  completer.Reply(disable_result.value());
}

void DriverDevelopmentService::EnableDriver(EnableDriverRequestView request,
                                            EnableDriverCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_index::DevelopmentManager>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_index::DevelopmentManager>,
         driver_index_client.status_string());
    completer.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto enable_result = driver_index->EnableDriver(request->driver_url, request->package_hash);
  if (!enable_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::EnableDriver: %s\n",
         enable_result.FormatDescription().c_str());
    completer.Close(enable_result.error().status());
    return;
  }

  completer.Reply(enable_result.value());
}

void DriverDevelopmentService::RestartDriverHosts(RestartDriverHostsRequestView request,
                                                  RestartDriverHostsCompleter::Sync& completer) {
  auto result = driver_runner_.RestartNodesColocatedWithDriverUrl(request->driver_url.get(),
                                                                  request->rematch_flags);
  if (result.is_ok()) {
    completer.ReplySuccess(result.value());
  } else {
    completer.ReplyError(result.error_value());
  }
}

void DriverDevelopmentService::BindAllUnboundNodes(BindAllUnboundNodesCompleter::Sync& completer) {
  auto callback =
      [completer = completer.ToAsync()](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> result) mutable {
        completer.ReplySuccess(result);
      };
  driver_runner_.TryBindAllAvailable(std::move(callback));
}

void DriverDevelopmentService::AddTestNode(AddTestNodeRequestView request,
                                           AddTestNodeCompleter::Sync& completer) {
  fuchsia_driver_framework::NodeAddArgs args;
  args.name(fidl::ToNatural(request->args.name()));
  args.properties(fidl::ToNatural(request->args.properties()));

  driver_runner_.root_node()->AddChild(
      std::move(args), /* controller */ {}, /* node */ {},
      [this, completer = completer.ToAsync()](fit::result<fuchsia_driver_framework::wire::NodeError,
                                                          std::shared_ptr<driver_manager::Node>>
                                                  result) mutable {
        if (result.is_error()) {
          completer.Reply(result.take_error());
        } else {
          auto node = result.value();
          test_nodes_[node->name()] = node;
          completer.ReplySuccess();
        }
      });
}

void DriverDevelopmentService::RemoveTestNode(RemoveTestNodeRequestView request,
                                              RemoveTestNodeCompleter::Sync& completer) {
  auto name = std::string(request->name.get());
  if (test_nodes_.count(name) == 0) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }

  auto node = test_nodes_[name].lock();
  if (!node) {
    completer.ReplySuccess();
    test_nodes_.erase(name);
    return;
  }

  node->Remove(driver_manager::RemovalSet::kAll, nullptr);
  test_nodes_.erase(name);
  completer.ReplySuccess();
}

void DriverDevelopmentService::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_driver_development::Manager> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  std::string method_type;
  switch (metadata.unknown_method_type) {
    case fidl::UnknownMethodType::kOneWay:
      method_type = "one-way";
      break;
    case fidl::UnknownMethodType::kTwoWay:
      method_type = "two-way";
      break;
  };

  LOGF(WARNING, "DriverDevelopmentService received unknown %s method. Ordinal: %lu",
       method_type.c_str(), metadata.method_ordinal);
}

}  // namespace driver_manager
