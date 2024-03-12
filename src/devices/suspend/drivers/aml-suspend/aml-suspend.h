// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_AML_SUSPEND_H_
#define SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_AML_SUSPEND_H_

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "lib/fdf/cpp/dispatcher.h"

namespace suspend {

class AmlSuspend : public fdf::DriverBase,
                   public fidl::WireServer<fuchsia_hardware_suspend::Suspender> {
 public:
  AmlSuspend(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("aml-suspend", std::move(start_args), std::move(dispatcher)),
        devfs_connector_(fit::bind_member<&AmlSuspend::Serve>(this)) {}

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_suspend::Suspender> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unexpected suspend FIDL call: 0x%lx", metadata.method_ordinal);
  }

  void GetSuspendStates(GetSuspendStatesCompleter::Sync& completer) override;
  void Suspend(SuspendRequestView request, SuspendCompleter::Sync& completer) override;

 protected:
  virtual zx::result<zx::resource> GetCpuResource();

 private:
  void Serve(fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> request);
  zx::result<> CreateDevfsNode();

  fidl::ServerBindingGroup<fuchsia_hardware_suspend::Suspender> suspend_bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_hardware_suspend::Suspender> devfs_connector_;

  zx::resource cpu_resource_;
};

}  // namespace suspend

#endif  // SRC_DEVICES_SUSPEND_DRIVERS_AML_SUSPEND_AML_SUSPEND_H_
