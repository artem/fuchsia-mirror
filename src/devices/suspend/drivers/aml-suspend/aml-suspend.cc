// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-suspend.h"

#include <lib/driver/component/cpp/driver_export.h>

#include "fidl/fuchsia.hardware.suspend/cpp/markers.h"
#include "fidl/fuchsia.hardware.suspend/cpp/wire_types.h"
#include "lib/driver/component/cpp/prepare_stop_completer.h"
#include "lib/fidl/cpp/wire/arena.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/vector_view.h"
#include "lib/zx/time.h"

namespace suspend {

static constexpr char kDeviceName[] = "aml-suspend-device";

zx::result<> AmlSuspend::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Error creating devfs node");
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("suspend");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client));
  parent_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

zx::result<> AmlSuspend::Start() {
  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_suspend::Suspender>(
      suspend_bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Suspender service %s", result.status_string());
    return result.take_error();
  }

  zx::result create_devfs_node_result = CreateDevfsNode();
  if (create_devfs_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", create_devfs_node_result.status_string());
    return create_devfs_node_result.take_error();
  }

  FDF_LOG(INFO, "Started Amlogic Suspend Driver");

  return zx::ok();
}

void AmlSuspend::Stop() {}

void AmlSuspend::PrepareStop(fdf::PrepareStopCompleter completer) { completer(zx::ok()); }

void AmlSuspend::GetSuspendStates(GetSuspendStatesCompleter::Sync& completer) {
  fidl::Arena arena;

  auto suspend_to_idle =
      fuchsia_hardware_suspend::wire::SuspendState::Builder(arena).resume_latency(0).Build();
  std::vector<fuchsia_hardware_suspend::wire::SuspendState> suspend_states = {
      std::move(suspend_to_idle)};

  auto resp = fuchsia_hardware_suspend::wire::SuspenderGetSuspendStatesResponse::Builder(arena)
                  .suspend_states(std::move(suspend_states))
                  .Build();

  completer.ReplySuccess(resp);
}

void AmlSuspend::Suspend(SuspendRequestView request, SuspendCompleter::Sync& completer) {
  fuchsia_hardware_suspend::wire::SuspenderSuspendResponse resp;
  resp.suspend_duration() = 0;
  resp.suspend_overhead() = 0;
  completer.ReplySuccess(resp);
}

void AmlSuspend::Serve(fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> request) {
  suspend_bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

}  // namespace suspend

FUCHSIA_DRIVER_EXPORT(suspend::AmlSuspend);
