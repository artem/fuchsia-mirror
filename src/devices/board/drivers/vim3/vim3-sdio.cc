// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <wifi/wifi-config.h>

#include "src/devices/board/drivers/vim3/vim3-gpios.h"
#include "src/devices/board/drivers/vim3/vim3-sdio-bind.h"
#include "src/devices/board/drivers/vim3/vim3-wifi-bind.h"
#include "src/devices/board/drivers/vim3/vim3.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sdio_mmios{
    {{
        .base = A311D_EMMC_A_BASE,
        .length = A311D_EMMC_A_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sdio_irqs{
    {{
        .irq = A311D_SD_EMMC_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> sdio_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400'000,
    .max_freq = 100'000'000,
    .version_3 = true,
    .prefs = 0,
};

constexpr wifi_config_t wifi_config = {
    .oob_irq_mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    .clm_needed = false,
    .iovar_table =
        {
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_PM}, 0},
            {IOVAR_LIST_END_TYPE, {{0}}, 0},
        },
    .cc_table =
        {
            {"WW", 0}, {"AU", 0}, {"CA", 0}, {"US", 0}, {"GB", 0}, {"BE", 0}, {"BG", 0}, {"CZ", 0},
            {"DK", 0}, {"DE", 0}, {"EE", 0}, {"IE", 0}, {"GR", 0}, {"ES", 0}, {"FR", 0}, {"HR", 0},
            {"IT", 0}, {"CY", 0}, {"LV", 0}, {"LT", 0}, {"LU", 0}, {"HU", 0}, {"MT", 0}, {"NL", 0},
            {"AT", 0}, {"PL", 0}, {"PT", 0}, {"RO", 0}, {"SI", 0}, {"SK", 0}, {"FI", 0}, {"SE", 0},
            {"EL", 0}, {"IS", 0}, {"LI", 0}, {"TR", 0}, {"CH", 0}, {"NO", 0}, {"JP", 0}, {"", 0},
        },
};

static const std::vector<fpbus::Metadata> wifi_metadata{
    {{
        .type = DEVICE_METADATA_WIFI_CONFIG,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&wifi_config),
            reinterpret_cast<const uint8_t*>(&wifi_config) + sizeof(wifi_config)),
    }},
};

zx_status_t Vim3::SdioInit() {
  fidl::Arena<> fidl_arena;

  fit::result sdmmc_metadata =
      fidl::Persist(fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
                        // TODO(https://fxbug.dev/134787): Use the FIDL SDMMC protocol.
                        .use_fidl(false)
                        .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> sdio_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                       reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
      }},
      {{
          .type = DEVICE_METADATA_SDMMC,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node sdio_dev;
  sdio_dev.name() = "vim3-sdio";
  sdio_dev.vid() = PDEV_VID_AMLOGIC;
  sdio_dev.pid() = PDEV_PID_GENERIC;
  sdio_dev.did() = PDEV_DID_AMLOGIC_SDMMC_A;
  sdio_dev.mmio() = sdio_mmios;
  sdio_dev.irq() = sdio_irqs;
  sdio_dev.bti() = sdio_btis;
  sdio_dev.metadata() = sdio_metadata;

  using fuchsia_hardware_gpio::GpioFlags;

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioConfigIn(GpioFlags::kNoPull)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioConfigIn(GpioFlags::kNoPull)});

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioSetAltFunction(A311D_GPIOX_0_SDIO_D0_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioSetAltFunction(A311D_GPIOX_1_SDIO_D1_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioSetAltFunction(A311D_GPIOX_2_SDIO_D2_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioSetAltFunction(A311D_GPIOX_3_SDIO_D3_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioSetAltFunction(A311D_GPIOX_4_SDIO_CLK_FN)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioSetAltFunction(A311D_GPIOX_5_SDIO_CMD_FN)});

  gpio_init_steps_.push_back({A311D_SDIO_D0, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D1, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D2, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_D3, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_CLK, GpioSetDriveStrength(4'000)});
  gpio_init_steps_.push_back({A311D_SDIO_CMD, GpioSetDriveStrength(4'000)});

  fdf::Arena sdio_arena('SDIO');
  auto result =
      pbus_.buffer(sdio_arena)
          ->AddComposite(fidl::ToWire(fidl_arena, sdio_dev),
                         platform_bus_composite::MakeFidlFragment(fidl_arena, vim3_sdio_fragments,
                                                                  std::size(vim3_sdio_fragments)),
                         "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sdio_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sdio_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Add a composite device for wifi driver.
  fpbus::Node wifi_dev;
  wifi_dev.name() = "wifi";
  wifi_dev.vid() = PDEV_VID_BROADCOM;
  wifi_dev.pid() = PDEV_PID_BCM4359;
  wifi_dev.did() = PDEV_DID_BCM_WIFI;
  wifi_dev.metadata() = wifi_metadata;

  fdf::Arena wifi_arena('WIFI');
  fdf::WireUnownedResult wifi_result =
      pbus_.buffer(wifi_arena)
          ->AddComposite(fidl::ToWire(fidl_arena, wifi_dev),
                         platform_bus_composite::MakeFidlFragment(fidl_arena, wifi_fragments,
                                                                  std::size(wifi_fragments)),
                         "sdio-function-1");
  if (!wifi_result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request to platform bus: %s",
           wifi_result.status_string());
    return wifi_result.status();
  }
  if (wifi_result->is_error()) {
    zxlogf(ERROR, "Failed to add wifi composite to platform device: %s",
           zx_status_get_string(wifi_result->error_value()));
    return wifi_result->error_value();
  }

  return ZX_OK;
}

}  // namespace vim3
