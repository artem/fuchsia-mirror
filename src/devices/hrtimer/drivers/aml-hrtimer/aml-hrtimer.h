// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_H_
#define SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "src/devices/hrtimer/drivers/aml-hrtimer/aml-hrtimer-server.h"

namespace hrtimer {

static constexpr char kDeviceName[] = "aml-hrtimer";

struct PowerConfiguration {
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_client;
  fidl::ClientEnd<fuchsia_power_broker::Lessor> lessor_client;
};

class AmlHrtimer : public fdf::DriverBase {
 public:
  static constexpr size_t GetNumberOfIrqs() { return kNumberOfIrqs; }

  AmlHrtimer(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&AmlHrtimer::Serve>(this)) {}

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // For unit testing.
  std::optional<fidl::ClientEnd<fuchsia_power_broker::ElementControl>>& element_control() {
    return server_->element_control();
  }
  bool HasWaitCompleter(size_t timer_index) {
    return server_ && server_->HasWaitCompleter(timer_index);
  }

 private:
  static constexpr size_t kNumberOfIrqs = 8;  // These are provided by the platform, 8 total.

  zx::result<> CreateDevfsNode();
  void Serve(fidl::ServerEnd<fuchsia_hardware_hrtimer::Device> server) {
    bindings_.AddBinding(dispatcher(), std::move(server), server_.get(),
                         fidl::kIgnoreBindingClosure);
  }
  zx::result<PowerConfiguration> GetPowerConfiguration(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev);

  std::unique_ptr<AmlHrtimerServer> server_;
  fidl::ServerBindingGroup<fuchsia_hardware_hrtimer::Device> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_hardware_hrtimer::Device> devfs_connector_;
};

}  // namespace hrtimer

#endif  // SRC_DEVICES_HRTIMER_DRIVERS_AML_HRTIMER_AML_HRTIMER_H_
