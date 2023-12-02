// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/ethernet/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <limits.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/designware/platform/cpp/bind.h>
#include <bind/fuchsia/ethernet/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "vim3-gpios.h"
#include "vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Irq> eth_mac_irqs{
    {{
        .irq = A311D_ETH_GMAC_IRQ,
        .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
    }},
};

static const std::vector<fpbus::Mmio> eth_board_mmios{
    {{
        .base = A311D_PERIPHERALS_BASE,
        .length = A311D_PERIPHERALS_LENGTH,
    }},
    {{
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Mmio> eth_mac_mmios{
    {{
        .base = A311D_ETH_MAC_BASE,
        .length = A311D_ETH_MAC_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> eth_mac_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_ETHERNET,
    }},
};

static const std::vector<fpbus::BootMetadata> eth_mac_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = 0,
    }},
};

static const eth_dev_metadata_t eth_phy_device = {
    .vid = PDEV_VID_REALTEK,
    .pid = PDEV_PID_RTL8211F,
    .did = PDEV_DID_REALTEK_ETH_PHY,
};

static const std::vector<fpbus::Metadata> eth_mac_device_metadata{
    {{
        .type = DEVICE_METADATA_ETH_PHY_DEVICE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&eth_phy_device),
            reinterpret_cast<const uint8_t*>(&eth_phy_device) + sizeof(eth_phy_device)),
    }},
};

static const eth_dev_metadata_t eth_mac_device = {
    .vid = PDEV_VID_DESIGNWARE,
    .pid = 0,
    .did = PDEV_DID_DESIGNWARE_ETH_MAC,
};

static const std::vector<fpbus::Metadata> eth_board_metadata{
    {{
        .type = DEVICE_METADATA_ETH_MAC_DEVICE,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&eth_mac_device),
            reinterpret_cast<const uint8_t*>(&eth_mac_device) + sizeof(eth_mac_device)),
    }},
};

static const fpbus::Node eth_board_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "ethernet_mac";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A311D;
  dev.did() = PDEV_DID_AMLOGIC_ETH;
  dev.mmio() = eth_board_mmios;
  dev.metadata() = eth_board_metadata;
  return dev;
}();

static const fpbus::Node dwmac_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "dwmac";
  dev.vid() = PDEV_VID_DESIGNWARE;
  dev.did() = PDEV_DID_DESIGNWARE_ETH_MAC;
  dev.mmio() = eth_mac_mmios;
  dev.irq() = eth_mac_irqs;
  dev.bti() = eth_mac_btis;
  dev.metadata() = eth_mac_device_metadata;
  dev.boot_metadata() = eth_mac_metadata;
  return dev;
}();

// Composite binding rules for ethernet board driver.
static const zx_bind_inst_t gpio_int_match[] = {
    BI_ABORT_IF(NE, BIND_FIDL_PROTOCOL, ZX_FIDL_PROTOCOL_GPIO),
    BI_MATCH_IF(EQ, BIND_GPIO_PIN, VIM3_ETH_MAC_INTR),
};

static const device_fragment_part_t gpio_int_fragment[] = {
    {std::size(gpio_int_match), gpio_int_match},
};

static const zx_bind_inst_t gpio_init_match[] = {
    BI_MATCH_IF(EQ, BIND_INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

static const device_fragment_part_t gpio_init_fragment[] = {
    {std::size(gpio_init_match), gpio_init_match},
};

static const device_fragment_t eth_fragments[] = {
    {"gpio-int", std::size(gpio_int_fragment), gpio_int_fragment},
    {"gpio-init", std::size(gpio_init_fragment), gpio_init_fragment},
};

const std::vector<fuchsia_driver_framework::BindRule> kEthBoardRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_ethernet::BIND_FIDL_PROTOCOL_BOARD_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                            bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_VID_DESIGNWARE),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                            bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_DID_ETH_MAC),
};

const std::vector<fuchsia_driver_framework::NodeProperty> kEthBoardProperties = {
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                      bind_fuchsia_ethernet::BIND_FIDL_PROTOCOL_BOARD_SERVICE),
};

const std::vector<fuchsia_driver_framework::ParentSpec> kEthBoardParents = {
    fuchsia_driver_framework::ParentSpec{
        {.bind_rules = kEthBoardRules, .properties = kEthBoardProperties}}};

zx_status_t Vim3::EthInit() {
  // setup pinmux for RGMII connections
  gpio_init_steps_.push_back({A311D_GPIOZ(0), GpioSetAltFunction(A311D_GPIOZ_0_ETH_MDIO_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(1), GpioSetAltFunction(A311D_GPIOZ_1_ETH_MDC_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(2), GpioSetAltFunction(A311D_GPIOZ_2_ETH_RX_CLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(3), GpioSetAltFunction(A311D_GPIOZ_3_ETH_RX_DV_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(4), GpioSetAltFunction(A311D_GPIOZ_4_ETH_RXD0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(5), GpioSetAltFunction(A311D_GPIOZ_5_ETH_RXD1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(6), GpioSetAltFunction(A311D_GPIOZ_6_ETH_RXD2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(7), GpioSetAltFunction(A311D_GPIOZ_7_ETH_RXD3_FN)});

  gpio_init_steps_.push_back({A311D_GPIOZ(8), GpioSetAltFunction(A311D_GPIOZ_8_ETH_TX_CLK_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(9), GpioSetAltFunction(A311D_GPIOZ_9_ETH_TX_EN_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(10), GpioSetAltFunction(A311D_GPIOZ_10_ETH_TXD0_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(11), GpioSetAltFunction(A311D_GPIOZ_11_ETH_TXD1_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(12), GpioSetAltFunction(A311D_GPIOZ_12_ETH_TXD2_FN)});
  gpio_init_steps_.push_back({A311D_GPIOZ(13), GpioSetAltFunction(A311D_GPIOZ_13_ETH_TXD3_FN)});

  gpio_init_steps_.push_back({A311D_GPIOZ(0), GpioSetDriveStrength(2500)});
  gpio_init_steps_.push_back({A311D_GPIOZ(1), GpioSetDriveStrength(2500)});
  gpio_init_steps_.push_back({A311D_GPIOZ(2), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(3), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(4), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(5), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(6), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(7), GpioSetDriveStrength(3000)});

  gpio_init_steps_.push_back({A311D_GPIOZ(8), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(9), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(10), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(11), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(12), GpioSetDriveStrength(3000)});
  gpio_init_steps_.push_back({A311D_GPIOZ(13), GpioSetDriveStrength(3000)});

  // Add a composite device for ethernet board in a new devhost.
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('ETH_');
  auto result = pbus_.buffer(arena)->AddCompositeImplicitPbusFragment(
      fidl::ToWire(fidl_arena, eth_board_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, eth_fragments, std::size(eth_fragments)),
      {});
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Eth(eth_board_dev) request failed: %s",
           __func__, result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Eth(eth_board_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Add a composite device for dwmac driver in the ethernet board driver's driver host.
  auto spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "dwmac", .parents = kEthBoardParents}};
  fdf::WireUnownedResult dwmac_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(arena, dwmac_dev), fidl::ToWire(arena, spec));
  if (!dwmac_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Eth(dwmac_dev) request failed: %s", __func__,
           dwmac_result.FormatDescription().data());
    return dwmac_result.status();
  }
  if (dwmac_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Eth(dwmac_dev) failed: %s", __func__,
           zx_status_get_string(dwmac_result->error_value()));
    return dwmac_result->error_value();
  }

  return ZX_OK;
}
}  // namespace vim3
