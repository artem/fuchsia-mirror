// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HDMI_HOST_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HDMI_HOST_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/hdmi-transmitter.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

#define VID_PLL_DIV_1 0
#define VID_PLL_DIV_2 1
#define VID_PLL_DIV_3 2
#define VID_PLL_DIV_3p5 3
#define VID_PLL_DIV_3p75 4
#define VID_PLL_DIV_4 5
#define VID_PLL_DIV_5 6
#define VID_PLL_DIV_6 7
#define VID_PLL_DIV_6p25 8
#define VID_PLL_DIV_7 9
#define VID_PLL_DIV_7p5 10
#define VID_PLL_DIV_12 11
#define VID_PLL_DIV_14 12
#define VID_PLL_DIV_15 13
#define VID_PLL_DIV_2p5 14

enum viu_type {
  VIU_ENCL = 0,
  VIU_ENCI,
  VIU_ENCP,
  VIU_ENCT,
};

struct pll_param {
  uint32_t mode;
  uint32_t viu_channel;
  uint32_t viu_type;
  uint32_t hpll_clk_out;
  uint32_t od1;
  uint32_t od2;
  uint32_t od3;
  uint32_t vid_pll_div;
  uint32_t vid_clk_div;
  uint32_t hdmi_tx_pixel_div;
  uint32_t encp_div;
  uint32_t enci_div;
};

struct cea_timing {
  bool interlace_mode;
  int pfreq_khz;
  int8_t ln;
  bool pixel_repeat;
  bool venc_pixel_repeat;

  int hfreq;
  int hactive;
  int htotal;
  int hblank;
  int hfront;
  int hsync;
  int hback;
  bool hpol;

  int vfreq;
  int vactive;
  int vtotal;
  int vblank0;  // in case of interlace
  int vblank1;  // vblank0 + 1 for interlace
  int vfront;
  int vsync;
  int vback;
  bool vpol;
};

// HdmiHost has access to the amlogic/designware HDMI block and controls its
// operation. It also handles functions and keeps track of data that the
// amlogic/designware block does not need to know about, including clock
// calculations (which may move out of the host after fxb/69072 is resolved),
// VPU and HHI register handling, HDMI parameters, etc.
class HdmiHost {
 public:
  // `hdmi_transmitter` must not be null.
  //
  // `vpu_mmio` is the VPU MMIO register region. It must be a valid MMIO buffer.
  //
  // `hhi_mmio` is the HHI (HIU) MMIO register region. It must be a valid MMIO
  // buffer.
  //
  // `gpio_mux_mmio` is the GPIO Multiplexer (PERIPHS_REGS) MMIO register
  // region. It must be a valid MMIO buffer.
  //
  // The VPU, HIU and PERIPHS_REGS register regions are defined in Section 8.1
  // "Memory Map" of the AMLogic A311D datasheet.
  HdmiHost(std::unique_ptr<HdmiTransmitter> hdmi_transmitter, fdf::MmioBuffer vpu_mmio,
           fdf::MmioBuffer hhi_mmio, fdf::MmioBuffer gpio_mux_mmio);

  HdmiHost(const HdmiHost&) = delete;
  HdmiHost(HdmiHost&&) = delete;
  HdmiHost& operator=(const HdmiHost&) = delete;
  HdmiHost& operator=(HdmiHost&&) = delete;

  static zx::result<std::unique_ptr<HdmiHost>> Create(zx_device_t* parent);

  zx_status_t HostOn();
  void HostOff();

  // Configures the HDMI clock, encoder and physical layer to given `timing`.
  //
  // Returns ZX_OK and configures the video output module iff the display
  // timing and clock of `timing` is supported.
  zx_status_t ModeSet(const display::DisplayTiming& timing);

  zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count);

  // Returns true iff a display timing is supported by the display engine driver
  // and can be used in a display configuration.
  bool IsDisplayTimingSupported(const display::DisplayTiming& timing) const;

 private:
  void ConfigurePll(const pll_param& pll_params);
  void ConfigEncoder(const cea_timing& timings);
  void ConfigPhy();

  void ConfigureHpllClkOut(uint32_t hpll);
  void ConfigureOd3Div(uint32_t div_sel);
  void WaitForPllLocked();

  std::unique_ptr<HdmiTransmitter> hdmi_transmitter_;

  fdf::MmioBuffer vpu_mmio_;
  fdf::MmioBuffer hhi_mmio_;
  fdf::MmioBuffer gpio_mux_mmio_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_HDMI_HOST_H_
