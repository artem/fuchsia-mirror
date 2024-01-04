// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/handle.h>

#include <optional>

#include <fbl/algorithm.h>
#include <hwreg/bitfields.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>
#include <wifi/wifi-config.h>

#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-sdio-bind.h"
#include "src/devices/board/drivers/sherlock/sherlock-wifi-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr uint32_t kGpioBase = fbl::round_down<uint32_t, uint32_t>(T931_GPIO_BASE, PAGE_SIZE);
constexpr uint32_t kGpioBaseOffset = T931_GPIO_BASE - kGpioBase;

class PadDsReg2A : public hwreg::RegisterBase<PadDsReg2A, uint32_t> {
 public:
  static constexpr uint32_t kDriveStrengthMax = 3;

  static auto Get() { return hwreg::RegisterAddr<PadDsReg2A>((0xd2 * 4) + kGpioBaseOffset); }

  DEF_FIELD(1, 0, gpiox_0_select);
  DEF_FIELD(3, 2, gpiox_1_select);
  DEF_FIELD(5, 4, gpiox_2_select);
  DEF_FIELD(7, 6, gpiox_3_select);
  DEF_FIELD(9, 8, gpiox_4_select);
  DEF_FIELD(11, 10, gpiox_5_select);
};

static const std::vector<fpbus::BootMetadata> wifi_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = MACADDR_WIFI,
    }},
};

static const std::vector<fpbus::Mmio> sd_emmc_mmios{
    {{
        .base = T931_SD_EMMC_A_BASE,
        .length = T931_SD_EMMC_A_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> sd_emmc_irqs{
    {{
        .irq = T931_SD_EMMC_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> sd_emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SDIO,
    }},
};

constexpr aml_sdmmc_config_t sd_emmc_config = {
    .min_freq = 500'000,      // 500KHz
    .max_freq = 208'000'000,  // 208MHz
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
            {"WW", 1},   {"AU", 923}, {"CA", 901}, {"US", 843}, {"GB", 889}, {"BE", 889},
            {"BG", 889}, {"CZ", 889}, {"DK", 889}, {"DE", 889}, {"EE", 889}, {"IE", 889},
            {"GR", 889}, {"ES", 889}, {"FR", 889}, {"HR", 889}, {"IT", 889}, {"CY", 889},
            {"LV", 889}, {"LT", 889}, {"LU", 889}, {"HU", 889}, {"MT", 889}, {"NL", 889},
            {"AT", 889}, {"PL", 889}, {"PT", 889}, {"RO", 889}, {"SI", 889}, {"SK", 889},
            {"FI", 889}, {"SE", 889}, {"EL", 889}, {"IS", 889}, {"LI", 889}, {"TR", 889},
            {"CH", 889}, {"NO", 889}, {"JP", 2},   {"", 0},
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

}  // namespace

zx_status_t Sherlock::SdioInit() {
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

  const std::vector<fpbus::Metadata> sd_emmc_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&sd_emmc_config),
              reinterpret_cast<const uint8_t*>(&sd_emmc_config) + sizeof(sd_emmc_config)),
      }},
      {{
          .type = DEVICE_METADATA_SDMMC,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node sdio_dev;
  sdio_dev.name() = "sherlock-sd-emmc";
  sdio_dev.vid() = PDEV_VID_AMLOGIC;
  sdio_dev.pid() = PDEV_PID_GENERIC;
  sdio_dev.did() = PDEV_DID_AMLOGIC_SDMMC_A;
  sdio_dev.mmio() = sd_emmc_mmios;
  sdio_dev.bti() = sd_emmc_btis;
  sdio_dev.irq() = sd_emmc_irqs;
  sdio_dev.metadata() = sd_emmc_metadata;

  // Configure eMMC-SD soc pads.
  gpio_init_steps_.push_back({T931_SDIO_D0, GpioSetAltFunction(T931_SDIO_D0_FN)});
  gpio_init_steps_.push_back({T931_SDIO_D1, GpioSetAltFunction(T931_SDIO_D1_FN)});
  gpio_init_steps_.push_back({T931_SDIO_D2, GpioSetAltFunction(T931_SDIO_D2_FN)});
  gpio_init_steps_.push_back({T931_SDIO_D3, GpioSetAltFunction(T931_SDIO_D3_FN)});
  gpio_init_steps_.push_back({T931_SDIO_CLK, GpioSetAltFunction(T931_SDIO_CLK_FN)});
  gpio_init_steps_.push_back({T931_SDIO_CMD, GpioSetAltFunction(T931_SDIO_CMD_FN)});

  zx::unowned_resource res(get_mmio_resource(parent()));
  zx::vmo vmo;
  zx_status_t status =
      zx::vmo::create_physical(*res, kGpioBase, kGpioBaseOffset + T931_GPIO_LENGTH, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "failed to create VMO: %s", zx_status_get_string(status));
    return status;
  }
  zx::result<fdf::MmioBuffer> buf = fdf::MmioBuffer::Create(
      0, kGpioBaseOffset + T931_GPIO_LENGTH, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);

  if (buf.is_error()) {
    zxlogf(ERROR, "fdf::MmioBuffer::Create() error: %s", buf.status_string());
    return buf.status_value();
  }

  PadDsReg2A::Get()
      .ReadFrom(&(*buf))
      .set_gpiox_0_select(PadDsReg2A::kDriveStrengthMax)
      .set_gpiox_1_select(PadDsReg2A::kDriveStrengthMax)
      .set_gpiox_2_select(PadDsReg2A::kDriveStrengthMax)
      .set_gpiox_3_select(PadDsReg2A::kDriveStrengthMax)
      .set_gpiox_4_select(PadDsReg2A::kDriveStrengthMax)
      .set_gpiox_5_select(PadDsReg2A::kDriveStrengthMax)
      .WriteTo(&(*buf));

  gpio_init_steps_.push_back({T931_WIFI_REG_ON, GpioSetAltFunction(T931_WIFI_REG_ON_FN)});
  gpio_init_steps_.push_back({T931_WIFI_HOST_WAKE, GpioSetAltFunction(T931_WIFI_HOST_WAKE_FN)});

  fdf::Arena sdio_arena('SDIO');
  auto result = pbus_.buffer(sdio_arena)
                    ->AddComposite(fidl::ToWire(fidl_arena, sdio_dev),
                                   platform_bus_composite::MakeFidlFragment(
                                       fidl_arena, sherlock_sd_emmc_fragments,
                                       std::size(sherlock_sd_emmc_fragments)),
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

}  // namespace sherlock
