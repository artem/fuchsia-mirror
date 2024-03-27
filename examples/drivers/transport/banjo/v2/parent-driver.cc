// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/banjo/v2/parent-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/gizmo/example/cpp/bind.h>

namespace banjo_transport {

zx::result<> ParentBanjoTransportDriver::Start() {
  node_.Bind(std::move(node()));

  auto child_name = "banjo-transport-child";

  // Initialize our compat server with a banjo config from |banjo_server_|.
  {
    zx::result<> result = child_.Initialize(incoming(), outgoing(), node_name(), child_name,
                                            compat::ForwardMetadata::None(), get_banjo_config());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  // Add a child device node and offer the service capabilities.
  // Offer `fuchsia.examples.gizmo.Service` to the driver that binds to the node.
  auto args = fuchsia_driver_framework::NodeAddArgs({
      .name = child_name,
      .properties = {{banjo_server_.property(),
                      fdf::MakeProperty(bind_gizmo_example::TEST_NODE_ID, "banjo_child")}},
      .offers2 = child_.CreateOffers2(),
  });

  // Create endpoints of the `NodeController` for the node.
  auto [client_end, server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  auto result = node_->AddChild({std::move(args), std::move(server_end), {}});
  if (result.is_error()) {
    const auto& error = result.error_value();
    FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));
    return zx::error(error.is_domain_error() ? static_cast<uint32_t>(error.domain_error())
                                             : error.framework_error().status());
  }
  controller_.Bind(std::move(client_end), dispatcher());

  return zx::ok();
}

zx_status_t ParentBanjoTransportDriver::MiscGetHardwareId(uint32_t* out_response) {
  *out_response = 0x1234ABCD;
  return ZX_OK;
}

zx_status_t ParentBanjoTransportDriver::MiscGetFirmwareVersion(uint32_t* out_major,
                                                               uint32_t* out_minor) {
  *out_major = 0x0;
  *out_minor = 0x1;
  return ZX_OK;
}

}  // namespace banjo_transport

FUCHSIA_DRIVER_EXPORT(banjo_transport::ParentBanjoTransportDriver);
