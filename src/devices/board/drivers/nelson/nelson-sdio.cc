// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zircon-internal/align.h>

#include <fbl/algorithm.h>
#include <hwreg/bitfields.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>
#include <wifi/wifi-config.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/board/drivers/nelson/nelson_aml_sdio_bind.h"
#include "src/devices/board/drivers/nelson/nelson_wifi_bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::BootMetadata> wifi_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = MACADDR_WIFI,
    }},
};

static const std::vector<fpbus::Mmio> sd_emmc_mmios{
    {{
        .base = S905D3_EMMC_A_SDIO_BASE,
        .length = S905D3_EMMC_A_SDIO_LENGTH,
    }},
    {{
        .base = S905D3_GPIO_BASE,
        .length = S905D3_GPIO_LENGTH,
    }},
    {{
        .base = S905D3_HIU_BASE,
        .length = S905D3_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sd_emmc_irqs{
    {{
        .irq = S905D3_EMMC_A_SDIO_IRQ,
        .mode = 0,
    }},
};

static const std::vector<fpbus::Bti> sd_emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400'000,
    .max_freq = 208'000'000,
    .version_3 = true,
    .prefs = 0,
};

constexpr wifi_config_t wifi_config = {
    .oob_irq_mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    .iovar_table =
        {
            {IOVAR_STR_TYPE, {"ampdu_ba_wsize"}, 32},
            {IOVAR_STR_TYPE, {"stbc_tx"}, 0},  // since tx_streams is 1
            {IOVAR_STR_TYPE, {"stbc_rx"}, 1},
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_PM}, 0},
            {IOVAR_CMD_TYPE, {.iovar_cmd = BRCMF_C_SET_FAKEFRAG}, 1},
            {IOVAR_LIST_END_TYPE, {{0}}, 0},
        },
    .cc_table =
        {
            {"WW", 2},   {"AU", 924}, {"CA", 902}, {"US", 844}, {"GB", 890}, {"BE", 890},
            {"BG", 890}, {"CZ", 890}, {"DK", 890}, {"DE", 890}, {"EE", 890}, {"IE", 890},
            {"GR", 890}, {"ES", 890}, {"FR", 890}, {"HR", 890}, {"IT", 890}, {"CY", 890},
            {"LV", 890}, {"LT", 890}, {"LU", 890}, {"HU", 890}, {"MT", 890}, {"NL", 890},
            {"AT", 890}, {"PL", 890}, {"PT", 890}, {"RO", 890}, {"SI", 890}, {"SK", 890},
            {"FI", 890}, {"SE", 890}, {"EL", 890}, {"IS", 890}, {"LI", 890}, {"TR", 890},
            {"CH", 890}, {"NO", 890}, {"JP", 3},   {"KR", 3},   {"TW", 3},   {"IN", 3},
            {"SG", 3},   {"MX", 3},   {"CL", 3},   {"PE", 3},   {"CO", 3},   {"NZ", 3},
            {"", 0},
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

// Composite binding rules for wifi driver.

// Composite binding rules for SDIO.

zx_status_t Nelson::SdioInit() {
  fidl::Arena<> fidl_arena;

  fit::result sdmmc_metadata =
      fidl::Persist(fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
                        // TODO(fxbug.dev/134787): Use the FIDL SDMMC protocol.
                        .use_fidl(false)
                        .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> sd_emmc_metadata{
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

  fpbus::Node sd_emmc_dev;
  sd_emmc_dev.name() = "aml-sdio";
  sd_emmc_dev.vid() = PDEV_VID_AMLOGIC;
  sd_emmc_dev.pid() = PDEV_PID_GENERIC;
  sd_emmc_dev.did() = PDEV_DID_AMLOGIC_SDMMC_A;
  sd_emmc_dev.mmio() = sd_emmc_mmios;
  sd_emmc_dev.irq() = sd_emmc_irqs;
  sd_emmc_dev.bti() = sd_emmc_btis;
  sd_emmc_dev.metadata() = sd_emmc_metadata;

  auto set_alt_function = [&arena = gpio_init_arena_](uint64_t alt_function) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithAltFunction(arena, alt_function);
  };

  auto set_drive_strength = [&arena = gpio_init_arena_](uint64_t drive_strength_ua) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithDriveStrengthUa(arena, drive_strength_ua);
  };

  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_D0, set_alt_function(S905D3_WIFI_SDIO_D0_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_D1, set_alt_function(S905D3_WIFI_SDIO_D1_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_D2, set_alt_function(S905D3_WIFI_SDIO_D2_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_D3, set_alt_function(S905D3_WIFI_SDIO_D3_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_CLK, set_alt_function(S905D3_WIFI_SDIO_CLK_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_CMD, set_alt_function(S905D3_WIFI_SDIO_CMD_FN)});
  gpio_init_steps_.push_back({S905D3_WIFI_SDIO_WAKE_HOST, set_alt_function(0)});

  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_D0, set_drive_strength(4000)});
  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_D1, set_drive_strength(4000)});
  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_D2, set_drive_strength(4000)});
  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_D3, set_drive_strength(4000)});
  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_CLK, set_drive_strength(4000)});
  gpio_init_steps_.push_back({GPIO_SOC_WIFI_SDIO_CMD, set_drive_strength(4000)});

  fdf::Arena sdio_arena('SDIO');
  auto result =
      pbus_.buffer(sdio_arena)
          ->AddComposite(fidl::ToWire(fidl_arena, sd_emmc_dev),
                         platform_bus_composite::MakeFidlFragment(fidl_arena, aml_sdio_fragments,
                                                                  std::size(aml_sdio_fragments)),
                         "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sd_emmc_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Sdio(sd_emmc_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Add a composite device for wifi driver.
  fpbus::Node wifi_dev;
  wifi_dev.name() = "wifi";
  wifi_dev.vid() = PDEV_VID_BROADCOM;
  wifi_dev.pid() = PDEV_PID_BCM43458;
  wifi_dev.did() = PDEV_DID_BCM_WIFI;
  wifi_dev.metadata() = wifi_metadata;
  wifi_dev.boot_metadata() = wifi_boot_metadata;

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

}  // namespace nelson
