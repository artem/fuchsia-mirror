// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.thermal/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/syscalls/smc.h>

#include <soc/aml-common/aml-thermal.h>
#include <soc/aml-meson/sm1-clk.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>
#include <soc/aml-s905d3/s905d3-pwm.h>

#include "nelson.h"
#include "src/devices/board/drivers/nelson/aml_thermal_pll_bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> thermal_mmios{
    {{
        .base = S905D3_TEMP_SENSOR_BASE,
        .length = S905D3_TEMP_SENSOR_LENGTH,
    }},
    {{
        .base = S905D3_TEMP_SENSOR_TRIM,
        .length = S905D3_TEMP_SENSOR_TRIM_LENGTH,
    }},
    {{
        .base = S905D3_HIU_BASE,
        .length = S905D3_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> thermal_irqs{
    {{
        .irq = S905D3_TS_PLL_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Smc> thermal_smcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_BASE,
        .count = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_LENGTH,
        .exclusive = false,
    }},
};

constexpr fuchsia_hardware_thermal::wire::ThermalTemperatureInfo TripPoint(float temp_c,
                                                                           float hysteresis_c,
                                                                           uint16_t cpu_opp,
                                                                           uint16_t gpu_opp) {
  return {
      .up_temp_celsius = temp_c + hysteresis_c,
      .down_temp_celsius = temp_c - hysteresis_c,
      .fan_level = 0,
      .big_cluster_dvfs_opp = cpu_opp,
      .little_cluster_dvfs_opp = 0,
      .gpu_clk_freq_source = gpu_opp,
  };
}

/*
 * GPU_CLK_FREQUENCY_SOURCE -
 * 0 - 285.7 MHz
 * 1 - 400 MHz
 * 2 - 500 MHz
 * 3 - 666 MHz
 * 4 - 800 MHz
 * 5 - 846 MHz
 */
static const fuchsia_hardware_thermal::wire::ThermalDeviceInfo nelson_config = {
    .active_cooling = false,
    .passive_cooling = true,
    .gpu_throttling = true,
    .num_trip_points = 5,
    .big_little = false,
    .critical_temp_celsius = 110.0f,
    .trip_point_info =
        {
            // The first trip point entry is the default state of the machine
            // and the driver does not use the specified temperature/hysterisis
            // to set any interrupt trip points.
            TripPoint(0.0f, 5.0f, 11, 5),
            TripPoint(60.0f, 5.0f, 9, 4),
            TripPoint(75.0f, 5.0f, 8, 3),
            TripPoint(80.0f, 5.0f, 7, 2),
            TripPoint(110.0f, 1.0f, 0, 0),
            // 0 Kelvin is impossible, marks end of TripPoints
            TripPoint(-273.15f, 2.0f, 0, 0),
        },
    .opps = {},
};

static const aml_thermal_info_t aml_thermal_info = {
    .voltage_table =
        {
            {.microvolt = 1'050'000, .duty_cycle = 0},  {.microvolt = 1'040'000, .duty_cycle = 3},
            {.microvolt = 1'030'000, .duty_cycle = 6},  {.microvolt = 1'020'000, .duty_cycle = 8},
            {.microvolt = 1'010'000, .duty_cycle = 11}, {.microvolt = 1'000'000, .duty_cycle = 14},
            {.microvolt = 990'000, .duty_cycle = 17},   {.microvolt = 980'000, .duty_cycle = 20},
            {.microvolt = 970'000, .duty_cycle = 23},   {.microvolt = 960'000, .duty_cycle = 26},
            {.microvolt = 950'000, .duty_cycle = 29},   {.microvolt = 940'000, .duty_cycle = 31},
            {.microvolt = 930'000, .duty_cycle = 34},   {.microvolt = 920'000, .duty_cycle = 37},
            {.microvolt = 910'000, .duty_cycle = 40},   {.microvolt = 900'000, .duty_cycle = 43},
            {.microvolt = 890'000, .duty_cycle = 45},   {.microvolt = 880'000, .duty_cycle = 48},
            {.microvolt = 870'000, .duty_cycle = 51},   {.microvolt = 860'000, .duty_cycle = 54},
            {.microvolt = 850'000, .duty_cycle = 56},   {.microvolt = 840'000, .duty_cycle = 59},
            {.microvolt = 830'000, .duty_cycle = 62},   {.microvolt = 820'000, .duty_cycle = 65},
            {.microvolt = 810'000, .duty_cycle = 68},   {.microvolt = 800'000, .duty_cycle = 70},
            {.microvolt = 790'000, .duty_cycle = 73},   {.microvolt = 780'000, .duty_cycle = 76},
            {.microvolt = 770'000, .duty_cycle = 79},   {.microvolt = 760'000, .duty_cycle = 81},
            {.microvolt = 750'000, .duty_cycle = 84},   {.microvolt = 740'000, .duty_cycle = 87},
            {.microvolt = 730'000, .duty_cycle = 89},   {.microvolt = 720'000, .duty_cycle = 92},
            {.microvolt = 710'000, .duty_cycle = 95},   {.microvolt = 700'000, .duty_cycle = 98},
            {.microvolt = 690'000, .duty_cycle = 100},
        },
    .initial_cluster_frequencies =
        {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
            [static_cast<uint32_t>(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain)] =
                1'200'000'000,
#pragma GCC diagnostic pop
        },
    .voltage_pwm_period_ns = 1500,
    .opps =
        {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
            [static_cast<uint32_t>(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain)] =
#pragma GCC diagnostic pop
                {
                    {
                        .opp =
                            {
                                fuchsia_hardware_thermal::wire::OperatingPointEntry{
                                    .freq_hz = 100'000'000, .volt_uv = 760'000},
                                {.freq_hz = 250'000'000, .volt_uv = 760'000},
                                {.freq_hz = 500'000'000, .volt_uv = 760'000},
                                {.freq_hz = 667'000'000, .volt_uv = 780'000},
                                {.freq_hz = 1'000'000'000, .volt_uv = 800'000},
                                {.freq_hz = 1'200'000'000, .volt_uv = 810'000},
                                {.freq_hz = 1'404'000'000, .volt_uv = 820'000},
                                {.freq_hz = 1'512'000'000, .volt_uv = 830'000},
                                {.freq_hz = 1'608'000'000, .volt_uv = 860'000},
                                {.freq_hz = 1'704'000'000, .volt_uv = 900'000},
                                {.freq_hz = 1'800'000'000, .volt_uv = 940'000},
                                {.freq_hz = 1'908'000'000, .volt_uv = 970'000},
                            },
                        .latency = 0,
                        .count = 12,
                    },
                    {
                        .opp =
                            {
                                fuchsia_hardware_thermal::wire::OperatingPointEntry{
                                    .freq_hz = 100'000'000, .volt_uv = 760'000},
                                {.freq_hz = 250'000'000, .volt_uv = 760'000},
                                {.freq_hz = 500'000'000, .volt_uv = 760'000},
                                {.freq_hz = 667'000'000, .volt_uv = 780'000},
                                {.freq_hz = 1'000'000'000, .volt_uv = 800'000},
                                {.freq_hz = 1'200'000'000, .volt_uv = 810'000},
                                {.freq_hz = 1'404'000'000, .volt_uv = 820'000},
                                {.freq_hz = 1'500'000'000, .volt_uv = 830'000},
                                {.freq_hz = 1'608'000'000, .volt_uv = 860'000},
                                {.freq_hz = 1'704'000'000, .volt_uv = 900'000},
                                {.freq_hz = 1'800'000'000, .volt_uv = 910'000},
                                {.freq_hz = 1'908'000'000, .volt_uv = 910'000},
                            },
                        .latency = 0,
                        .count = 12,
                    },
                    {
                        .opp =
                            {
                                fuchsia_hardware_thermal::wire::OperatingPointEntry{
                                    .freq_hz = 100'000'000, .volt_uv = 760'000},
                                {.freq_hz = 250'000'000, .volt_uv = 760'000},
                                {.freq_hz = 500'000'000, .volt_uv = 760'000},
                                {.freq_hz = 667'000'000, .volt_uv = 780'000},
                                {.freq_hz = 1'000'000'000, .volt_uv = 800'000},
                                {.freq_hz = 1'200'000'000, .volt_uv = 810'000},
                                {.freq_hz = 1'404'000'000, .volt_uv = 820'000},
                                {.freq_hz = 1'500'000'000, .volt_uv = 830'000},
                                {.freq_hz = 1'608'000'000, .volt_uv = 860'000},
                                {.freq_hz = 1'704'000'000, .volt_uv = 860'000},
                                {.freq_hz = 1'800'000'000, .volt_uv = 860'000},
                                {.freq_hz = 1'908'000'000, .volt_uv = 860'000},
                            },
                        .latency = 0,
                        .count = 12,
                    },
                },
        },
    .cluster_id_map =
        {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
            [static_cast<uint32_t>(
                fuchsia_hardware_thermal::wire::PowerDomain::kBigClusterPowerDomain)] = 0,
#pragma GCC diagnostic pop
        },
};

static const std::vector<fpbus::Metadata> thermal_metadata{
    {{
        .type = DEVICE_METADATA_THERMAL_CONFIG,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&nelson_config),
            reinterpret_cast<const uint8_t*>(&nelson_config) + sizeof(nelson_config)),
    }},
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&aml_thermal_info),
            reinterpret_cast<const uint8_t*>(&aml_thermal_info) + sizeof(aml_thermal_info)),
    }},
};

static const fpbus::Node thermal_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-thermal-pll";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_S905D3;
  dev.did() = PDEV_DID_AMLOGIC_THERMAL_PLL;
  dev.mmio() = thermal_mmios;
  dev.irq() = thermal_irqs;
  dev.metadata() = thermal_metadata;
  dev.smc() = thermal_smcs;
  return dev;
}();

zx_status_t Nelson::ThermalInit() {
  // Configure the GPIO to be Output & set it to alternate
  // function 3 which puts in PWM_D mode.
  gpio_init_steps_.push_back({S905D3_PWM_D_PIN, GpioConfigOut(0)});

  gpio_init_steps_.push_back({S905D3_PWM_D_PIN, GpioSetAltFunction(S905D3_PWM_D_FN)});

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('THER');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, thermal_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, aml_thermal_pll_fragments,
                                               std::size(aml_thermal_pll_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Thermal(thermal_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Thermal(thermal_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace nelson
