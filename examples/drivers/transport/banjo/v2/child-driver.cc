// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/banjo/v2/child-driver.h"

#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace banjo_transport {

zx::result<> ChildBanjoTransportDriver::Start() {
  // Connect to the `fuchsia.examples.gizmo.Misc` protocol provided by the parent.
  zx::result<ddk::MiscProtocolClient> client =
      compat::ConnectBanjo<ddk::MiscProtocolClient>(incoming());

  if (client.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect client", KV("status", client.status_string()));
    return client.take_error();
  }
  client_ = *client;

  zx_status_t status = QueryParent();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  zx::result child_result = AddChild("transport-child", {}, {});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  controller_.Bind(std::move(child_result.value()), dispatcher());
  return zx::ok();
}

zx_status_t ChildBanjoTransportDriver::QueryParent() {
  zx_status_t status = client_.GetHardwareId(&hardware_id_);
  if (status != ZX_OK) {
    return status;
  }
  FDF_LOG(INFO, "Transport client hardware: %X", hardware_id_);

  status = client_.GetFirmwareVersion(&major_version_, &minor_version_);
  if (status != ZX_OK) {
    return status;
  }
  FDF_LOG(INFO, "Transport client firmware: %d.%d", major_version_, minor_version_);
  return ZX_OK;
}

}  // namespace banjo_transport

FUCHSIA_DRIVER_EXPORT(banjo_transport::ChildBanjoTransportDriver);
