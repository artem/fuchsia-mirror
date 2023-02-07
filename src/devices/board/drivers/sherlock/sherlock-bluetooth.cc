// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fuchsia/hardware/gpioimpl/c/banjo.h>
#include <fuchsia/hardware/serial/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/hw/reg.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <unistd.h>

#include <soc/aml-t931/t931-gpio.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"
#include "src/devices/board/drivers/sherlock/sherlock-bluetooth-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> bt_uart_mmios{
    {{
        .base = T931_UART_A_BASE,
        .length = T931_UART_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> bt_uart_irqs{
    {{
        .irq = T931_UART_A_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

constexpr serial_port_info_t bt_uart_serial_info = {
    .serial_class = fidl::ToUnderlying(fuchsia_hardware_serial::Class::kBluetoothHci),
    .serial_vid = PDEV_VID_BROADCOM,
    .serial_pid = PDEV_PID_BCM43458,
};

static const std::vector<fpbus::Metadata> bt_uart_metadata{
    {{
        .type = DEVICE_METADATA_SERIAL_PORT_INFO,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&bt_uart_serial_info),
            reinterpret_cast<const uint8_t*>(&bt_uart_serial_info) + sizeof(bt_uart_serial_info)),
    }},
};

static const std::vector<fpbus::BootMetadata> bt_uart_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_MAC_ADDRESS,
        .zbi_extra = MACADDR_BLUETOOTH,
    }},
};

static const fpbus::Node bt_uart_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "bt-uart";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_AMLOGIC_UART;
  dev.mmio() = bt_uart_mmios;
  dev.irq() = bt_uart_irqs;
  dev.metadata() = bt_uart_metadata;
  dev.boot_metadata() = bt_uart_boot_metadata;
  return dev;
}();

zx_status_t Sherlock::BluetoothInit() {
  zx_status_t status;

  // set alternate functions to enable Bluetooth UART
  status = gpio_impl_.SetAltFunction(T931_UART_A_TX, T931_UART_A_TX_FN);
  if (status != ZX_OK) {
    return status;
  }

  status = gpio_impl_.SetAltFunction(T931_UART_A_RX, T931_UART_A_RX_FN);
  if (status != ZX_OK) {
    return status;
  }

  status = gpio_impl_.SetAltFunction(T931_UART_A_CTS, T931_UART_A_CTS_FN);
  if (status != ZX_OK) {
    return status;
  }

  status = gpio_impl_.SetAltFunction(T931_UART_A_RTS, T931_UART_A_RTS_FN);
  if (status != ZX_OK) {
    return status;
  }

  // Bind UART for Bluetooth HCI
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('BLUE');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, bt_uart_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, bt_uart_fragments,
                                               std::size(bt_uart_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Bluetooth(bt_uart_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Bluetooth(bt_uart_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock
