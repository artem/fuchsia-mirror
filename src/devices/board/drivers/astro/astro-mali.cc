// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpu.amlogic/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/meson/cpp/bind.h>
#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/hardware/gpu/mali/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <soc/aml-common/aml-registers.h>
#include <soc/aml-meson/g12a-clk.h>
#include <soc/aml-s905d2/s905d2-hw.h>

#include "astro.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> aml_gpu_mmios{
    {{
        .base = S905D2_MALI_BASE,
        .length = S905D2_MALI_LENGTH,
    }},
    {{
        .base = S905D2_HIU_BASE,
        .length = S905D2_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Mmio> mali_mmios{
    {{
        .base = S905D2_MALI_BASE,
        .length = S905D2_MALI_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> mali_irqs{
    {{
        .irq = S905D2_MALI_IRQ_PP,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
    {{
        .irq = S905D2_MALI_IRQ_GPMMU,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
    {{
        .irq = S905D2_MALI_IRQ_GP,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static const std::vector<fpbus::Bti> mali_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_MALI,
    }},
};

zx_status_t Astro::MaliInit() {
  {
    fpbus::Node aml_gpu_dev;
    aml_gpu_dev.name() = "aml_gpu";
    aml_gpu_dev.vid() = PDEV_VID_AMLOGIC;
    aml_gpu_dev.pid() = PDEV_PID_AMLOGIC_S905D2;
    aml_gpu_dev.did() = PDEV_DID_AMLOGIC_MALI_INIT;
    aml_gpu_dev.mmio() = aml_gpu_mmios;

    using fuchsia_hardware_gpu_amlogic::wire::Metadata;
    fidl::Arena allocator;
    Metadata metadata(allocator);
    metadata.set_supports_protected_mode(true);
    fit::result encoded_metadata = fidl::Persist(metadata);
    if (!encoded_metadata.is_ok()) {
      zxlogf(ERROR, "%s: Could not build metadata %s\n", __func__,
             encoded_metadata.error_value().FormatDescription().c_str());
      return encoded_metadata.error_value().status();
    }
    std::vector<uint8_t>& encoded_metadata_bytes = encoded_metadata.value();
    std::vector<fpbus::Metadata> mali_metadata_list{
        {{
            .type = fuchsia_hardware_gpu_amlogic::wire::kMaliMetadata,
            .data = std::move(encoded_metadata_bytes),
        }},
    };
    aml_gpu_dev.metadata() = mali_metadata_list;

    fidl::Arena<> fidl_arena;
    fdf::Arena arena('MALI');

    auto aml_gpu_register_reset_node = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bind_fuchsia_hardware_registers::SERVICE,
                                        bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(bind_fuchsia_register::NAME,
                                        bind_fuchsia_amlogic_platform::NAME_REGISTER_MALI_RESET),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_registers::SERVICE,
                                  bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_register::NAME,
                                  bind_fuchsia_amlogic_platform::NAME_REGISTER_MALI_RESET),
            },
    }};
    auto aml_gpu_clock_node = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                        bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(
                    bind_fuchsia::CLOCK_ID,
                    bind_fuchsia_amlogic_platform_meson::G12A_CLK_ID_CLK_GP0_PLL),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                                  bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                                  bind_fuchsia_clock::FUNCTION_GP0_PLL),
            },
    }};

    auto parents = std::vector<fuchsia_driver_framework::ParentSpec>{aml_gpu_register_reset_node,
                                                                     aml_gpu_clock_node};

    auto composite_node_spec = fuchsia_driver_framework::CompositeNodeSpec(
        {.name = "aml-gpu-composite", .parents = parents});

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, aml_gpu_dev), fidl::ToWire(fidl_arena, composite_node_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Mali(aml-gpu-composite) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Mali(aml-gpu-composite) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  {
    fpbus::Node mali_dev;
    mali_dev.name() = "mali";
    mali_dev.vid() = PDEV_VID_ARM;
    mali_dev.pid() = PDEV_PID_GENERIC;
    mali_dev.did() = PDEV_DID_ARM_MAGMA_MALI;
    mali_dev.mmio() = mali_mmios;
    mali_dev.irq() = mali_irqs;
    mali_dev.bti() = mali_btis;

    fidl::Arena<> fidl_arena;
    fdf::Arena arena('MALI');

    auto aml_gpu_bind_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

    auto aml_gpu_properties =
        std::vector{fdf::MakeProperty(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                      bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

    auto parents =
        std::vector{fuchsia_driver_framework::ParentSpec(aml_gpu_bind_rules, aml_gpu_properties)};

    auto composite_node_spec =
        fuchsia_driver_framework::CompositeNodeSpec({.name = "mali-composite", .parents = parents});

    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, mali_dev), fidl::ToWire(fidl_arena, composite_node_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddComposite Mali(mali_dev) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddComposite Mali(mali_dev) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace astro
