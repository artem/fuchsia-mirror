// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_H_

#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/result.h>
#include <unistd.h>
#include <zircon/compiler.h>

#include <optional>

#include <ddktl/device.h>
#include <hwreg/mmio.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/dsi.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

class Clock {
 public:
  // Factory method intended for production use.
  //
  // Creating a Clock instance doesn't change the hardware state, and is
  // therefore safe to use when adopting a bootloader initialized device.
  static zx::result<std::unique_ptr<Clock>> Create(ddk::PDevFidl& pdev, bool already_enabled);

  // Production code should prefer using the `Create()` factory method.
  //
  // `vpu_mmio` is the VPU MMIO register region. It must be a valid MMIO buffer.
  //
  // `hhi_mmio` is the HHI (HIU) MMIO register region. It must be a valid MMIO
  // buffer.
  //
  // The VPU and HIU register regions are defined in Section 8.1 "Memory Map"
  // of the AMLogic A311D datasheet.
  Clock(fdf::MmioBuffer vpu_mmio, fdf::MmioBuffer hhi_mmio, bool clock_enabled);

  zx::result<> Enable(const PanelConfig& panel_config);
  void Disable();

  void SetVideoOn(bool on);

  // This is only safe to call when the clock is Enable'd.
  uint32_t GetBitrate() const {
    ZX_DEBUG_ASSERT(clock_enabled_);
    return pll_cfg_.bitrate;
  }

  static LcdTiming CalculateLcdTiming(const display::DisplayTiming& display_timing);
  // This function calculates the required pll configurations needed to generate
  // the desired lcd clock
  static zx::result<PllConfig> GenerateHPLL(int32_t pixel_clock_frequency_khz,
                                            int64_t maximum_per_data_lane_bit_per_second);

 private:
  zx::result<> WaitForHdmiPllToLock();

  fdf::MmioBuffer vpu_mmio_;
  fdf::MmioBuffer hhi_mmio_;

  PllConfig pll_cfg_ = {};
  LcdTiming lcd_timing_ = {};

  bool clock_enabled_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_CLOCK_H_
