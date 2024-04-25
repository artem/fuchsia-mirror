// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <ddk/metadata/clock.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/g12b-clk.h>

#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> clk_mmios{
    {{
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
    {{
        .base = A311D_DOS_BASE,
        .length = A311D_DOS_LENGTH,
    }},
    {{
        .base = A311D_MSR_CLK_BASE,
        .length = A311D_MSR_CLK_LENGTH,
    }},
};

// clang-format off
static const clock_id_t clock_ids[] = {
    {g12b_clk::G12B_CLK_SYS_PLL_DIV16},
    {g12b_clk::G12B_CLK_SYS_CPU_CLK_DIV16},
    {g12b_clk::G12B_CLK_SYS_PLLB_DIV16},
    {g12b_clk::G12B_CLK_SYS_CPUB_CLK_DIV16},
    {g12b_clk::G12B_CLK_DOS_GCLK_VDEC},
    {g12b_clk::G12B_CLK_DOS},
    {g12b_clk::G12B_CLK_AUDIO},
    {g12b_clk::G12B_CLK_EMMC_C},
    {g12b_clk::CLK_SYS_CPU_BIG_CLK},
    {g12b_clk::CLK_SYS_CPU_LITTLE_CLK},
    {g12b_clk::CLK_HIFI_PLL},
    {g12b_clk::G12B_CLK_TSENSOR},
};
// clang-format on

zx_status_t Vim3::ClkInit() {
  fuchsia_hardware_clockimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clockimpl::wire::InitStep>::FromExternal(
      clock_init_steps_.data(), clock_init_steps_.size());

  const fit::result encoded_metadata = fidl::Persist(metadata);
  if (!encoded_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode clock init metadata: %s",
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> clock_metadata{
      {{
          .type = DEVICE_METADATA_CLOCK_IDS,
          .data =
              std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(clock_ids),
                                   reinterpret_cast<const uint8_t*>(clock_ids) + sizeof(clock_ids)),
      }},
      {{
          .type = DEVICE_METADATA_CLOCK_INIT,
          .data = encoded_metadata.value(),
      }},
  };

  const fpbus::Node clk_dev = [&clock_metadata]() {
    fpbus::Node dev = {};
    dev.name() = "vim3-clk";
    dev.vid() = PDEV_VID_AMLOGIC;
    dev.pid() = PDEV_PID_AMLOGIC_A311D;
    dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_G12B_CLK;
    dev.mmio() = clk_mmios;
    dev.metadata() = clock_metadata;
    return dev;
  }();

  fidl::Arena fidl_arena;
  fdf::Arena arena('CLK_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, clk_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "Clk(clk_dev)Init: NodeAdd Clk(clk_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Clk(clk_dev)Init: NodeAdd Clk(clk_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}
}  // namespace vim3
