// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/virtualization/bin/vmm/device/virtio_net/src/cpp/guest_ethernet_interface.h"

#include <lib/syslog/cpp/macros.h>

zx_status_t guest_ethernet_context_create(GuestEthernetContext** context_out) {
  auto context = GuestEthernetContext::Create();
  if (context.is_error()) {
    return context.status_value();
  }
  *context_out = context.value().release();
  return ZX_OK;
}

void guest_ethernet_context_destroy(GuestEthernetContext* context) { delete context; }

zx_status_t guest_ethernet_create(GuestEthernetContext* context,
                                  GuestEthernet** guest_ethernet_out) {
  if (context == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto guest_ethernet = std::make_unique<GuestEthernet>(
      context->SyncDispatcher(), context->Dispatchers(), context->ShimDispatchers());
  *guest_ethernet_out = guest_ethernet.release();
  return ZX_OK;
}

void guest_ethernet_destroy(GuestEthernet* guest_ethernet) {
  FX_CHECK(guest_ethernet != nullptr);
  delete guest_ethernet;
}

zx_status_t guest_ethernet_initialize(GuestEthernet* guest_ethernet,
                                      const void* rust_guest_ethernet, const uint8_t* mac,
                                      size_t mac_len, bool enable_bridge) {
  FX_CHECK(guest_ethernet != nullptr);
  FX_CHECK(rust_guest_ethernet != nullptr);
  FX_CHECK(mac != nullptr);
  return guest_ethernet->Initialize(rust_guest_ethernet, mac, mac_len, enable_bridge);
}

zx_status_t guest_ethernet_send(GuestEthernet* guest_ethernet, const void* data, uint16_t length) {
  FX_CHECK(guest_ethernet != nullptr);
  FX_CHECK(data != nullptr);
  return guest_ethernet->Send(data, length);
}

void guest_ethernet_complete(GuestEthernet* guest_ethernet, uint32_t buffer_id,
                             zx_status_t status) {
  FX_CHECK(guest_ethernet != nullptr);
  guest_ethernet->Complete(buffer_id, status);
}
