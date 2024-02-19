// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_LCD_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_LCD_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <lib/fit/function.h>
#include <unistd.h>
#include <zircon/compiler.h>

#include <fbl/alloc_checker.h>
#include <hwreg/mmio.h>

#include "src/graphics/display/drivers/amlogic-display/panel-config.h"

namespace amlogic_display {

// An Lcd controls the panel attached to a MIPI-DSI endpoint.
class Lcd {
 public:
  // Factory method intended for production use.
  //
  // `enabled` is true iff the driver adopts an already initialized panel.
  //
  // Creating an Lcd instance doesn't change the hardware state, and is
  // therefore safe to use when adopting a device previously initialized by
  // the bootloader or another driver.
  static zx::result<std::unique_ptr<Lcd>> Create(zx_device_t* parent, uint32_t panel_type,
                                                 const PanelConfig* panel_config, bool enabled);

  // Production code should prefer using the `Create()` factory method.
  //
  // It must satisfy the following constraints:
  // - `panel_config` must not be null and outlive the `Lcd` instance.
  // - `dsiimpl` must be valid.
  // - `lcd_reset_gpio` must be a valid GPIO pin.
  explicit Lcd(uint32_t panel_type, const PanelConfig* panel_config,
               ddk::DsiImplProtocolClient dsiimpl,
               fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> lcd_reset_gpio, bool enabled);

  Lcd(const Lcd&) = delete;
  Lcd& operator=(const Lcd&) = delete;

  // Turn the panel on
  zx::result<> Enable();

  // Turn the panel off
  zx::result<> Disable();

 private:
  // Decodes and performs the Amlogic-specific display initialization command
  // sequence stored in `encoded_commands` which is a packed buffer of all
  // encoded commands.
  //
  // The Amlogic-specific display initialization commands are defined in:
  // Amlogic MIPI DSI Panel Tuning User Guide, Version 0.1 (Google internal),
  // Section 3.2.6 "Init table config", page 19.
  zx::result<> PerformDisplayInitCommandSequence(cpp20::span<const uint8_t> encoded_commands);

  uint32_t panel_type_;
  const PanelConfig& panel_config_;

  ddk::DsiImplProtocolClient dsiimpl_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> lcd_reset_gpio_;

  bool enabled_ = false;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_LCD_H_
