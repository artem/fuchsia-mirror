// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "compat.h"

#include <dirent.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>
#include <lib/service/llcpp/service.h>

#include <fbl/unique_fd.h>

namespace compat {

namespace fdf = fuchsia_driver_framework;
namespace fcd = fuchsia_component_decl;

zx_status_t DeviceServer::AddMetadata(uint32_t type, const void* data, size_t size) {
  Metadata metadata(size);
  auto begin = static_cast<const uint8_t*>(data);
  std::copy(begin, begin + size, metadata.begin());
  auto [_, inserted] = metadata_.emplace(type, std::move(metadata));
  if (!inserted) {
    return ZX_ERR_ALREADY_EXISTS;
  }
  return ZX_OK;
}

zx_status_t DeviceServer::GetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) {
  auto it = metadata_.find(type);
  if (it == metadata_.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  auto& [_, metadata] = *it;

  auto size = std::min(buflen, metadata.size());
  auto begin = metadata.begin();
  std::copy(begin, begin + size, static_cast<uint8_t*>(buf));

  *actual = metadata.size();
  return ZX_OK;
}

zx_status_t DeviceServer::GetMetadataSize(uint32_t type, size_t* out_size) {
  auto it = metadata_.find(type);
  if (it == metadata_.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  auto& [_, metadata] = *it;
  *out_size = metadata.size();
  return ZX_OK;
}

void DeviceServer::GetTopologicalPath(GetTopologicalPathRequestView request,
                                      GetTopologicalPathCompleter::Sync& completer) {
  completer.Reply(fidl::StringView::FromExternal(topological_path_));
}

void DeviceServer::GetMetadata(GetMetadataRequestView request,
                               GetMetadataCompleter::Sync& completer) {
  std::vector<fuchsia_driver_compat::wire::Metadata> metadata;
  metadata.reserve(metadata_.size());
  for (auto& [type, data] : metadata_) {
    fuchsia_driver_compat::wire::Metadata new_metadata;
    new_metadata.type = type;
    zx::vmo vmo;

    zx_status_t status = zx::vmo::create(data.size(), 0, &new_metadata.data);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    status = new_metadata.data.write(data.data(), 0, data.size());
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    size_t size = data.size();
    status = new_metadata.data.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }

    metadata.push_back(std::move(new_metadata));
  }
  completer.ReplySuccess(fidl::VectorView<fuchsia_driver_compat::wire::Metadata>::FromExternal(
      metadata.data(), metadata.size()));
}

void DeviceServer::ConnectFidl(ConnectFidlRequestView request,
                               ConnectFidlCompleter::Sync& completer) {
  if (dir_.is_valid()) {
    auto path = std::string("svc/").append(request->name.data(), request->name.size());
    fdio_service_connect_at(dir_.channel().get(), path.data(), request->server.release());
  }
  completer.Reply();
}

zx::status<Interop> Interop::Create(async_dispatcher_t* dispatcher, const driver::Namespace* ns,
                                    component::OutgoingDirectory* outgoing) {
  Interop interop;
  interop.dispatcher_ = dispatcher;
  interop.ns_ = ns;
  interop.outgoing_ = outgoing;
  return zx::ok(std::move(interop));
}

zx::status<fidl::WireSharedClient<fuchsia_driver_compat::Device>> ConnectToParentDevice(
    async_dispatcher_t* dispatcher, const driver::Namespace* ns, std::string_view name) {
  auto path =
      std::string(fuchsia_driver_compat::Service::Name).append("/").append(name).append("/device");
  auto result = ns->Connect<fuchsia_driver_compat::Device>(path.c_str());
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(
      fidl::WireSharedClient<fuchsia_driver_compat::Device>(std::move(result.value()), dispatcher));
}

zx_status_t Interop::AddToOutgoing(Child* child) {
  // Add the service instance to outgoing.
  component::ServiceHandler handler;
  fuchsia_driver_compat::Service::Handler compat_service(&handler);
  auto device = [this,
                 child](fidl::ServerEnd<fuchsia_driver_compat::Device> server_end) mutable -> void {
    fidl::BindServer<fidl::WireServer<fuchsia_driver_compat::Device>>(
        dispatcher_, std::move(server_end), &child->compat_device());
  };
  zx::status<> status = compat_service.add_device(std::move(device));
  if (status.is_error()) {
    return status.error_value();
  }
  status = outgoing_->AddService<fuchsia_driver_compat::Service>(std::move(handler), child->name());
  if (status.is_error()) {
    return status.error_value();
  }

  // Add the ServiceOffer to the child's offers.
  ServiceOffer instance_offer;
  instance_offer.service_name = fuchsia_driver_compat::Service::Name,
  instance_offer.renamed_instances.push_back(ServiceOffer::RenamedInstance{
      .source_name = std::string(child->name()),
      .target_name = "default",
  });
  instance_offer.included_instances.push_back("default");
  instance_offer.remove_service_callback =
      std::make_shared<fit::deferred_callback>([this, name = std::string(child->name())]() {
        (void)outgoing_->RemoveService<fuchsia_driver_compat::Service>(name);
      });
  child->offers().AddService(std::move(instance_offer));

  // Add each service in the device as an service in our outgoing directory.
  // We rename each instance from "default" into the child name, and then rename it back to default
  // via the offer.
  for (const auto& service_name : child->compat_device().offers()) {
    auto handler = [child, service_name](zx::channel request) {
      const auto path = std::string("svc/").append(service_name).append("/default");
      (void)service::ConnectAt(child->compat_device().dir(),
                               fidl::ServerEnd<fuchsia_io::Directory>(std::move(request)),
                               path.c_str());
    };

    const auto path = std::string("svc/").append(service_name);
    auto result = outgoing_->AddProtocolAt(std::move(handler), path, child->name());
    if (result.is_error()) {
      return result.error_value();
    }

    // Lastly add the service offer.
    ServiceOffer instance_offer;
    instance_offer.service_name = service_name;
    instance_offer.renamed_instances.push_back(ServiceOffer::RenamedInstance{
        .source_name = std::string(child->name()),
        .target_name = "default",
    });
    instance_offer.included_instances.push_back("default");
    instance_offer.remove_service_callback =
        std::make_shared<fit::deferred_callback>([this, path, name = std::string(child->name())]() {
          (void)outgoing_->RemoveProtocolAt(path, name);
        });
    child->offers().AddService(std::move(instance_offer));
  }
  return ZX_OK;
}

std::vector<fuchsia_component_decl::wire::Offer> Child::CreateOffers(fidl::ArenaBase& arena) {
  return offers_.CreateOffers(arena);
}

std::vector<fuchsia_component_decl::wire::Offer> ChildOffers::CreateOffers(fidl::ArenaBase& arena) {
  std::vector<fuchsia_component_decl::wire::Offer> offers;
  for (auto& service : service_offers_) {
    auto offer = fcd::wire::OfferService::Builder(arena);
    offer.source_name(arena, service.service_name);
    offer.target_name(arena, service.service_name);

    fidl::VectorView<fcd::wire::NameMapping> mappings(arena, service.renamed_instances.size());
    for (size_t i = 0; i < service.renamed_instances.size(); i++) {
      mappings[i].source_name = fidl::StringView(arena, service.renamed_instances[i].source_name);
      mappings[i].target_name = fidl::StringView(arena, service.renamed_instances[i].target_name);
    }
    offer.renamed_instances(mappings);

    fidl::VectorView<fidl::StringView> includes(arena, service.included_instances.size());
    for (size_t i = 0; i < service.included_instances.size(); i++) {
      includes[i] = fidl::StringView(arena, service.included_instances[i]);
    }
    offer.source_instance_filter(includes);
    offers.push_back(fcd::wire::Offer::WithService(arena, offer.Build()));
  }
  // XXX: Do protocols.
  return offers;
}

}  // namespace compat
