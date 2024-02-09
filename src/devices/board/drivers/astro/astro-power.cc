// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/power/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <ddk/metadata/power.h>
#include <soc/aml-common/aml-power.h>
#include <soc/aml-s905d2/s905d2-power.h>
#include <soc/aml-s905d2/s905d2-pwm.h>

#include "astro-gpios.h"
#include "astro.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr aml_voltage_table_t kS905D2VoltageTable[] = {
    {1'022'000, 0}, {1'011'000, 3}, {1'001'000, 6}, {991'000, 10}, {981'000, 13}, {971'000, 16},
    {961'000, 20},  {951'000, 23},  {941'000, 26},  {931'000, 30}, {921'000, 33}, {911'000, 36},
    {901'000, 40},  {891'000, 43},  {881'000, 46},  {871'000, 50}, {861'000, 53}, {851'000, 56},
    {841'000, 60},  {831'000, 63},  {821'000, 67},  {811'000, 70}, {801'000, 73}, {791'000, 76},
    {781'000, 80},  {771'000, 83},  {761'000, 86},  {751'000, 90}, {741'000, 93}, {731'000, 96},
    {721'000, 100},
};

constexpr voltage_pwm_period_ns_t kS905d2PwmPeriodNs = 1250;

constexpr power_domain_t domains[] = {
    {static_cast<uint32_t>(S905d2PowerDomains::kArmCore)},
};

}  // namespace

zx_status_t AddPowerImpl(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node dev;
  dev.name() = "aml-power-impl-composite";
  dev.vid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE;
  dev.pid() = bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_ASTRO;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_POWER;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_AML_VOLTAGE_TABLE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kS905D2VoltageTable),
              reinterpret_cast<const uint8_t*>(&kS905D2VoltageTable) + sizeof(kS905D2VoltageTable)),
      }},
      {{
          .type = DEVICE_METADATA_AML_PWM_PERIOD_NS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kS905d2PwmPeriodNs),
              reinterpret_cast<const uint8_t*>(&kS905d2PwmPeriodNs) + sizeof(kS905d2PwmPeriodNs)),
      }},
  };

  const std::vector<fuchsia_driver_framework::BindRule> kPwmRules = {
      fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(S905D2_PWM_AO_D))};
  const std::vector<fuchsia_driver_framework::NodeProperty> kPwmProps = {
      fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_pwm::BIND_FIDL_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia_amlogic_platform::PWM_ID,
                        bind_fuchsia_amlogic_platform::PWM_ID_AO_D)};
  const std::vector<fdf::ParentSpec> kParents = {fdf::ParentSpec{{kPwmRules, kPwmProps}}};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('POWR');
  fdf::WireUnownedResult result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml-power-impl-composite", .parents = kParents}}));

  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t AddPdArmcore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node dev;
  dev.name() = "composite-pd-armcore";
  dev.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  dev.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_POWER_CORE;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_POWER_DOMAINS,
          .data =
              std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&domains),
                                   reinterpret_cast<const uint8_t*>(&domains) + sizeof(domains)),
      }},
  };

  const std::vector<fdf::BindRule> kPowerArmcoreRules = std::vector{
      fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_power::BIND_PROTOCOL_IMPL),
  };

  const std::vector<fdf::NodeProperty> kPowerArmcoreProperties = std::vector{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_power::BIND_PROTOCOL_IMPL),
  };

  const std::vector<fdf::ParentSpec> kParents = {
      {
          kPowerArmcoreRules,
          kPowerArmcoreProperties,
      },
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('PDAC');
  auto result = pbus.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, dev),
      fidl::ToWire(fidl_arena,
                   fdf::CompositeNodeSpec{{.name = "pd_armcore", .parents = kParents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddCompositeNodeSpec request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Astro::PowerInit() {
  zx_status_t status = AddPowerImpl(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add power-impl composite device: %s", zx_status_get_string(status));
    return status;
  }

  status = AddPdArmcore(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add pd-armcore composite device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace astro
