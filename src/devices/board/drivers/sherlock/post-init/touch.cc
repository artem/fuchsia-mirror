// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/focaltech/focaltech.h>

#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/focaltech/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>

#include "src/devices/board/drivers/sherlock/post-init/post-init.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

const std::vector kI2cRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_2),
    fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kI2cProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS,
                      bind_fuchsia_focaltech_platform::BIND_I2C_ADDRESS_TOUCH),
};

const std::vector kInterruptRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_1),
};

const std::vector kInterruptProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_INTERRUPT)};

const std::vector kResetRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            bind_fuchsia_amlogic_platform_t931::GPIOZ_PIN_ID_PIN_9),
};

const std::vector kResetProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_TOUCH_RESET),
};

zx::result<> PostInit::InitTouch() {
  static const FocaltechMetadata device_info = {
      .device_id = FOCALTECH_DEVICE_FT5726,
      .needs_firmware = true,
      .display_vendor = static_cast<uint8_t>(display_vendor_),
      .ddic_version = static_cast<uint8_t>(ddic_version_),
  };

  fpbus::Node dev;
  dev.name() = "focaltech_touch";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_FOCALTOUCH;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&device_info),
              reinterpret_cast<const uint8_t*>(&device_info) + sizeof(device_info)),
      }},
  };

  auto parents = std::vector{
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kI2cRules,
          .properties = kI2cProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kInterruptRules,
          .properties = kInterruptProperties,
      }},
      fuchsia_driver_framework::ParentSpec{{
          .bind_rules = kResetRules,
          .properties = kResetProperties,
      }},
  };

  auto composite_node_spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "focaltech_touch", .parents = parents}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('FOCL');
  fdf::WireUnownedResult result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev), fidl::ToWire(fidl_arena, composite_node_spec));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to add composite node spec: %s",
            zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace sherlock
