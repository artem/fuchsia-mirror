// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <ddk/metadata/pwm.h>
#include <soc/aml-t931/t931-pwm.h>

#include "sherlock-gpios.h"
#include "sherlock.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> pwm_mmios{
    {{
        .base = T931_PWM_AB_BASE,
        .length = T931_PWM_LENGTH,
    }},
    {{
        .base = T931_PWM_CD_BASE,
        .length = T931_PWM_LENGTH,
    }},
    {{
        .base = T931_PWM_EF_BASE,
        .length = T931_PWM_LENGTH,
    }},
    {{
        .base = T931_AO_PWM_AB_BASE,
        .length = T931_AO_PWM_LENGTH,
    }},
    {{
        .base = T931_AO_PWM_CD_BASE,
        .length = T931_AO_PWM_LENGTH,
    }},
};

static const pwm_id_t pwm_ids[] = {
    {T931_PWM_A},
    {T931_PWM_B},
    {T931_PWM_C},
    {T931_PWM_D},
    {T931_PWM_E},
    {T931_PWM_F},
    {T931_PWM_AO_A},
    // T931_PWM_AO_B controls VDDEE_800 which is configured by the bootloader.
    // Marked as protect so we don't try to initialize it.
    {T931_PWM_AO_B, /*init = */ false},
    {T931_PWM_AO_C},
    {T931_PWM_AO_D},
};

static const std::vector<fpbus::Metadata> pwm_metadata{
    {{
        .type = DEVICE_METADATA_PWM_IDS,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&pwm_ids),
                                     reinterpret_cast<const uint8_t*>(&pwm_ids) + sizeof(pwm_ids)),
    }},
};

static const fpbus::Node pwm_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "pwm";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_PWM;
  dev.mmio() = pwm_mmios;
  dev.metadata() = pwm_metadata;
  return dev;
}();

const ddk::BindRule kPwmRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_pwm::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(T931_PWM_E)),
};

const device_bind_prop_t kPwmProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_pwm::BIND_FIDL_PROTOCOL_DEVICE),
};

const ddk::BindRule kGpioWifiRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            static_cast<uint32_t>(GPIO_SOC_WIFI_LPO_32k768)),
};

const device_bind_prop_t kGpioWifiProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_WIFI_LPO),
};

const ddk::BindRule kGpioBtRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SOC_BT_REG_ON)),
};

const device_bind_prop_t kGpioBtProperties[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_BT_REG_ON),
};

zx_status_t Sherlock::PwmInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('PWM_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, pwm_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Pwm(pwm_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Pwm(pwm_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  zx_status_t status =
      DdkAddCompositeNodeSpec("pwm_init", ddk::CompositeNodeSpec(kPwmRules, kPwmProperties)
                                              .AddParentSpec(kGpioWifiRules, kGpioWifiProperties)
                                              .AddParentSpec(kGpioBtRules, kGpioBtProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace sherlock
