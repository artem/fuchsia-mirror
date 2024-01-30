// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mac_addr_shim.h"

namespace network {

void MacAddrShim::Bind(fdf_dispatcher_t* dispatcher, ddk::MacAddrProtocolClient client_impl,
                       fdf::ServerEnd<netdriver::MacAddr> server_end) {
  std::unique_ptr impl = std::make_unique<MacAddrShim>(client_impl);
  MacAddrShim* impl_ptr = impl.get();

  fdf::ServerBindingRef binding_ref =
      fdf::BindServer(dispatcher, std::move(server_end), std::move(impl));
  impl_ptr->binding_.emplace(std::move(binding_ref));
}

MacAddrShim::MacAddrShim(ddk::MacAddrProtocolClient impl) : impl_(impl) {}

void MacAddrShim::SetMode(netdriver::wire::MacAddrSetModeRequest* request, fdf::Arena& arena,
                          SetModeCompleter::Sync& completer) {
  std::array<mac_address_t, MAX_MAC_FILTER> macs{};

  for (size_t i = 0; i < request->multicast_macs.count() && i < macs.size(); ++i) {
    memcpy(macs[i].octets, request->multicast_macs[i].octets.data(), MAC_SIZE);
  }

  impl_.SetMode(static_cast<uint32_t>(request->mode), macs.data(), request->multicast_macs.count());

  completer.buffer(arena).Reply();
}

void MacAddrShim::GetFeatures(fdf::Arena& arena, GetFeaturesCompleter::Sync& completer) {
  features_t features;
  impl_.GetFeatures(&features);

  fidl::WireTableBuilder builder = netdriver::wire::Features::Builder(arena);

  builder.multicast_filter_count(features.multicast_filter_count)
      .supported_modes(netdriver::wire::SupportedMacFilterMode(features.supported_modes));

  completer.buffer(arena).Reply(builder.Build());
}

void MacAddrShim::GetAddress(fdf::Arena& arena, GetAddressCompleter::Sync& completer) {
  mac_address_t addr;
  impl_.GetAddress(&addr);

  fuchsia_net::wire::MacAddress mac;
  std::copy(std::begin(addr.octets), std::end(addr.octets), mac.octets.begin());
  completer.buffer(arena).Reply(mac);
}

}  // namespace network
