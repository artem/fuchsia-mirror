// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_
#define SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace astro {

class PostInit : public fdf::DriverBase {
 public:
  PostInit(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("post-init", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

 private:
  fidl::SyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> controller_;

  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;
  fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager> composite_manager_;
};

}  // namespace astro

#endif  // SRC_DEVICES_BOARD_DRIVERS_ASTRO_POST_INIT_POST_INIT_H_
