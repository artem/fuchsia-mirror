// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include "src/power/testing/fake-suspend/control_server.h"
#include "src/power/testing/fake-suspend/device_server.h"

class FakeDevfsController : public fidl::testing::TestBase<fuchsia_device::Controller> {
 public:
  void Serve(fidl::ServerEnd<fuchsia_device::Controller> server_end) {
    bindings_.AddBinding(async_get_default_dispatcher(), std::move(server_end), this,
                         fidl::kIgnoreBindingClosure);
  }

 private:
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override {
    completer.Reply(zx::ok("fake-suspend/control"));
  }
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}
  fidl::ServerBindingGroup<fuchsia_device::Controller> bindings_;
};

int main() {
  async::Loop loop{&kAsyncLoopConfigAttachToCurrentThread};
  component::OutgoingDirectory outgoing(loop.dispatcher());

  auto suspend_states = std::make_shared<std::vector<fuchsia_hardware_suspend::SuspendState>>();
  auto control_server = std::make_shared<fake_suspend::ControlServer>(suspend_states);
  auto device_server = std::make_shared<fake_suspend::DeviceServer>(suspend_states);
  control_server->set_resumable(device_server);
  device_server->set_suspend_observer(control_server);

  auto devfs_controller = std::make_shared<FakeDevfsController>();

  auto result = outgoing.AddUnmanagedProtocolAt<fuchsia_hardware_suspend::Suspender>(
      "suspend",
      [device_server](fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> server_end) {
        device_server->Serve(async_get_default_dispatcher(), std::move(server_end));
      },
      "instance");
  if (result.is_error()) {
    return -1;
  }

  result = outgoing.AddUnmanagedProtocolAt<test_suspendcontrol::Device>(
      "controller/instance",
      [control_server](fidl::ServerEnd<test_suspendcontrol::Device> server_end) {
        control_server->Serve(async_get_default_dispatcher(), std::move(server_end));
      },
      "device_protocol");
  if (result.is_error()) {
    return -1;
  }

  result = outgoing.AddUnmanagedProtocolAt<fuchsia_device::Controller>(
      "controller/instance",
      [devfs_controller](fidl::ServerEnd<fuchsia_device::Controller> server_end) {
        devfs_controller->Serve(std::move(server_end));
      },
      "device_controller");
  if (result.is_error()) {
    return -1;
  }

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    return -1;
  }

  loop.Run();
  return 0;
}
