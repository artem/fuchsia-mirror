// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpt.metadata/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/handle.h>
#include <zircon/hw/gpt.h>

#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-emmc-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

static const std::vector<fpbus::Mmio> emmc_mmios{
    {{
        .base = T931_SD_EMMC_C_BASE,
        .length = T931_SD_EMMC_C_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> emmc_irqs{
    {{
        .irq = T931_SD_EMMC_C_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_EMMC,
    }},
};

static aml_sdmmc_config_t config = {
    // As per AMlogic, on S912 chipset, HS400 mode can be operated at 125MHZ or low.
    .min_freq = 400'000,
    .max_freq = 166'666'667,
    .version_3 = true,
    .prefs = SDMMC_HOST_PREFS_DISABLE_HS400,
};

static const struct {
  const fidl::StringView name;
  const fuchsia_hardware_block_partition::wire::Guid guid;
} guid_map[] = {
    {"boot", GUID_ZIRCON_A_VALUE},
    {"system", GUID_ZIRCON_B_VALUE},
    {"recovery", GUID_ZIRCON_R_VALUE},
    {"cache", GUID_FVM_VALUE},
};

static_assert(sizeof(guid_map) / sizeof(guid_map[0]) <=
              fuchsia_hardware_gpt_metadata::wire::kMaxPartitions);

static const std::vector<fpbus::BootMetadata> emmc_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_PARTITION_MAP,
        .zbi_extra = 0,
    }},
};

}  // namespace

zx_status_t Sherlock::EmmcInit() {
  using fuchsia_hardware_gpio::GpioFlags;

  // set alternate functions to enable EMMC
  gpio_init_steps_.push_back({T931_EMMC_D0, GpioSetAltFunction(T931_EMMC_D0_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D1, GpioSetAltFunction(T931_EMMC_D1_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D2, GpioSetAltFunction(T931_EMMC_D2_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D3, GpioSetAltFunction(T931_EMMC_D3_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D4, GpioSetAltFunction(T931_EMMC_D4_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D5, GpioSetAltFunction(T931_EMMC_D5_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D6, GpioSetAltFunction(T931_EMMC_D6_FN)});
  gpio_init_steps_.push_back({T931_EMMC_D7, GpioSetAltFunction(T931_EMMC_D7_FN)});
  gpio_init_steps_.push_back({T931_EMMC_CLK, GpioSetAltFunction(T931_EMMC_CLK_FN)});
  gpio_init_steps_.push_back({T931_EMMC_RST, GpioSetAltFunction(T931_EMMC_RST_FN)});
  gpio_init_steps_.push_back({T931_EMMC_CMD, GpioSetAltFunction(T931_EMMC_CMD_FN)});
  gpio_init_steps_.push_back({T931_EMMC_DS, GpioSetAltFunction(T931_EMMC_DS_FN)});

  gpio_init_steps_.push_back({T931_EMMC_D0, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D1, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D2, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D3, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D4, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D5, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D6, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_D7, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_CLK, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_RST, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_CMD, GpioSetDriveStrength(4000)});
  gpio_init_steps_.push_back({T931_EMMC_DS, GpioSetDriveStrength(4000)});

  gpio_init_steps_.push_back({T931_EMMC_D0, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D1, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D2, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D3, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D4, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D5, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D6, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_D7, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_CLK, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_RST, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_CMD, GpioConfigIn(GpioFlags::kPullUp)});
  gpio_init_steps_.push_back({T931_EMMC_DS, GpioConfigIn(GpioFlags::kPullDown)});

  fidl::Arena<> fidl_arena;

  fidl::VectorView<fuchsia_hardware_gpt_metadata::wire::PartitionInfo> partition_info(
      fidl_arena, std::size(guid_map));

  for (size_t i = 0; i < std::size(guid_map); i++) {
    partition_info[i].name = guid_map[i].name;
    partition_info[i].options =
        fuchsia_hardware_gpt_metadata::wire::PartitionOptions::Builder(fidl_arena)
            .type_guid_override(guid_map[i].guid)
            .Build();
  }

  fit::result encoded =
      fidl::Persist(fuchsia_hardware_gpt_metadata::wire::GptInfo::Builder(fidl_arena)
                        .partition_info(partition_info)
                        .Build());
  if (!encoded.is_ok()) {
    zxlogf(ERROR, "Failed to encode GPT metadata: %s",
           encoded.error_value().FormatDescription().c_str());
    return encoded.error_value().status();
  }

  fit::result sdmmc_metadata = fidl::Persist(
      fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena)
          .enable_trim(true)
          // Maintain the current Sherlock behavior until we determine that cache is needed.
          .enable_cache(false)
          // Maintain the current Sherlock behavior until we determine that eMMC Packed Commands are
          // needed.
          .max_command_packing(0)
          // TODO(https://fxbug.dev/134787): Use the FIDL SDMMC protocol.
          .use_fidl(false)
          .Build());
  if (!sdmmc_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode SDMMC metadata: %s",
           sdmmc_metadata.error_value().FormatDescription().c_str());
    return sdmmc_metadata.error_value().status();
  }

  static const std::vector<fpbus::Metadata> sherlock_emmc_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                       reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
      }},
      {{
          .type = DEVICE_METADATA_GPT_INFO,
          .data = std::move(encoded.value()),
      }},
      {{
          .type = DEVICE_METADATA_SDMMC,
          .data = std::move(sdmmc_metadata.value()),
      }},
  };

  fpbus::Node emmc_dev;
  emmc_dev.name() = "sherlock-emmc";
  emmc_dev.vid() = PDEV_VID_AMLOGIC;
  emmc_dev.pid() = PDEV_PID_GENERIC;
  emmc_dev.did() = PDEV_DID_AMLOGIC_SDMMC_C;
  emmc_dev.mmio() = emmc_mmios;
  emmc_dev.irq() = emmc_irqs;
  emmc_dev.bti() = emmc_btis;
  emmc_dev.metadata() = sherlock_emmc_metadata;
  emmc_dev.boot_metadata() = emmc_boot_metadata;

  fdf::Arena arena('EMMC');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, emmc_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, sherlock_emmc_fragments,
                                               std::size(sherlock_emmc_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Emmc(emmc_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Emmc(emmc_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock
