// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/process/lifecycle/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <zircon/processargs.h>

#include "fidl/fuchsia.bluetooth.host/cpp/fidl.h"
#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "host_component.h"
#include "lib/component/incoming/cpp/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/bt_host_config.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"
#include "util.h"

using InitCallback = fit::callback<void(bool success)>;
using ErrorCallback = fit::callback<void()>;

const std::string OUTGOING_SERVICE_NAME = "fuchsia.bluetooth.host.Host";

class LifecycleHandler : public fuchsia::process::lifecycle::Lifecycle,
                         public fidl::AsyncEventHandler<fuchsia_bluetooth_host::Receiver> {
 public:
  using WeakPtr = WeakSelf<bthost::BtHostComponent>::WeakPtr;

  explicit LifecycleHandler(async::Loop* loop, WeakPtr host) : loop_(loop), host_(std::move(host)) {
    // Get the PA_LIFECYCLE handle, and instantiate the channel with it
    zx::channel channel = zx::channel(zx_take_startup_handle(PA_LIFECYCLE));
    // Bind to the channel and start listening for events
    bindings_.AddBinding(
        this, fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle>(std::move(channel)),
        loop_->dispatcher());
  }

  void Stop() override {
    host_->ShutDown();
    loop_->Shutdown();
    bindings_.CloseAll();
  }

  // AsyncEventHandler overrides
  void on_fidl_error(fidl::UnbindInfo error) override {
    bt_log(WARN, "bt-host", "Receiver interface disconnected");
    Stop();
  }

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_bluetooth_host::Receiver> metadata) override {
    bt_log(WARN, "bt-host", "Received an unknown event with ordinal %lu", metadata.event_ordinal);
  }

 private:
  async::Loop* loop_;
  WeakPtr host_;
  fidl::BindingSet<fuchsia::process::lifecycle::Lifecycle> bindings_;
};

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  bt_log(INFO, "bt-host", "Starting bt-host");

  bt_host_config::Config config = bt_host_config::Config::TakeFromStartupHandle();
  if (config.device_path().empty()) {
    bt_log(ERROR, "bt-host", "device_path is empty! Can't open. Quitting.");
    return 1;
  }
  bt_log(INFO, "bt-host", "device_path: %s", config.device_path().c_str());

  std::unique_ptr<bthost::BtHostComponent> host =
      bthost::BtHostComponent::Create(loop.dispatcher(), config.device_path());

  LifecycleHandler lifecycle_handler(&loop, host->GetWeakPtr());

  auto init_cb = [&host, &lifecycle_handler, &loop](bool success) {
    BT_DEBUG_ASSERT(host);
    if (!success) {
      bt_log(ERROR, "bt-host", "Failed to initialize bt-host; shutting down...");
      // TODO(https://fxbug.dev/42086155): Verify that calling Lifecycler handler's stop function does
      // not cause use after free in Adapter with integration tests
      lifecycle_handler.Stop();
      return;
    }
    bt_log(DEBUG, "bt-host", "bt-host initialized; starting FIDL servers...");

    // Bind current host to Host protocol interface
    auto endpoints = fidl::CreateEndpoints<fuchsia_bluetooth_host::Host>();
    if (endpoints.is_error()) {
      bt_log(ERROR, "bt-host", "Couldn't create endpoints: %d", endpoints.error_value());
      lifecycle_handler.Stop();
      return;
    }
    host->BindToHostInterface(std::move(endpoints->server));

    // Add Host device and protocol to bt-gap via Receiver
    zx::result receiver_client = component::Connect<fuchsia_bluetooth_host::Receiver>();
    if (!receiver_client.is_ok()) {
      bt_log(ERROR, "bt-host", "Error connecting to the Receiver protocol: %s",
             receiver_client.status_string());
      lifecycle_handler.Stop();
      return;
    }
    fidl::Client client(std::move(*receiver_client), loop.dispatcher(), &lifecycle_handler);
    fit::result<fidl::Error> result = client->AddHost(
        fuchsia_bluetooth_host::ReceiverAddHostRequest(std::move(endpoints->client)));
    if (!result.is_ok()) {
      bt_log(ERROR, "bt-host", "Failed to add host: %s",
             result.error_value().FormatDescription().c_str());
      lifecycle_handler.Stop();
    }
  };

  auto error_cb = [&lifecycle_handler]() {
    bt_log(WARN, "bt-host", "Error initializing bt-host; shutting down...");
    // TODO(https://fxbug.dev/42086155): Verify that calling Lifecycler handler's stop function does
    // not cause use after free in Adapter with integration tests
    lifecycle_handler.Stop();
  };

  fuchsia::hardware::bluetooth::HciHandle hci_handle =
      bthost::CreateHciHandle(config.device_path());
  if (!hci_handle) {
    bt_log(ERROR, "bt-host", "Failed to create HciHandle; cannot initialize bt-host");
    return 1;
  }

  bool initialize_res = host->Initialize(std::move(hci_handle), init_cb, error_cb);
  if (!initialize_res) {
    bt_log(ERROR, "bt-host", "Error initializing bt-host; shutting down...");
    return 1;
  }

  loop.Run();
  return 0;
}
