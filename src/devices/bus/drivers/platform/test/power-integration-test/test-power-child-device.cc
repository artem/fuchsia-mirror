// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/test/power-integration-test/test-power-child-device.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>

namespace fake_child_device {
zx::result<> FakeChild::Start() {
  // Connect to the Device from our platform device parent and get our power
  // configuration.
  auto device_connection =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("platform-device");
  if (!device_connection.is_ok()) {
    return device_connection.take_error();
  }

  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> device_client(
      std::move(device_connection.value()));
  auto power_config = device_client->GetPowerConfiguration();

  // Ask the library to get our dependency tokens.
  auto res = fdf_power::GetDependencyTokens(*incoming(), power_config->value()->config[0]);
  if (res.is_error()) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  fdf_power::TokenMap tokens = std::move(res.value());

  // Add our power element.
  {
    // Get a connection to the Topology capability.
    auto pb_open = incoming()->Connect<fuchsia_power_broker::Topology>();
    if (pb_open.is_error()) {
      return pb_open.take_error();
    }
    fidl::ClientEnd<fuchsia_power_broker::Topology> broker = std::move(pb_open.value());

    fdf_power::ElementDesc description =
        fdf_power::ElementDescBuilder(power_config->value()->config[0], std::move(tokens)).Build();

    fit::result<fdf_power::Error, fuchsia_power_broker::TopologyAddElementResponse> add_result =
        fdf_power::AddElement(broker, description);
    if (!add_result.is_ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    // That worked, so store the channels we'll need to work with the element.
    required_level_ = std::move(description.required_level_client_.value());
    current_level_ = std::move(description.current_level_client_.value());
    lessor_ = std::move(description.lessor_client_.value());
  }
  return zx::ok();
}
}  // namespace fake_child_device

FUCHSIA_DRIVER_EXPORT(fake_child_device::FakeChild);
