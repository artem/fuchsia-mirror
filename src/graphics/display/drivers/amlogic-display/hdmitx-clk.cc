// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"
#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/hdmi-host.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

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

namespace amlogic_display {

// TODO(fxb/69072): Reconcile with amlogic-clock

namespace {

const uint32_t kFracMax = 131072;

}  // namespace

void HdmiHost::WaitForPllLocked() {
  bool err = false;
  do {
    unsigned int st = 0;
    int cnt = 10000;
    while (cnt--) {
      usleep(5);
      auto reg = HhiHdmiPllCntlReg::Get().ReadFrom(&(*hhi_mmio_));
      st = (reg.hdmi_dpll_lock() == 1) && (reg.hdmi_dpll_lock_a() == 1);
      if (st) {
        err = false;
        break;
      } else { /* reset hpll */
        HhiHdmiPllCntlReg::Get()
            .ReadFrom(&(*hhi_mmio_))
            .set_hdmi_dpll_reset(1)
            .WriteTo(&(*hhi_mmio_));
        HhiHdmiPllCntlReg::Get()
            .ReadFrom(&(*hhi_mmio_))
            .set_hdmi_dpll_reset(0)
            .WriteTo(&(*hhi_mmio_));
      }
    }
    zxlogf(ERROR, "pll[0x%x] reset %d times", HHI_HDMI_PLL_CNTL0, 10000 - cnt);
    if (cnt <= 0)
      err = true;
  } while (err);
}

namespace {

VideoInputUnitEncoderMuxControl::Encoder EncoderSelectionFromViuType(viu_type type) {
  switch (type) {
    case VIU_ENCL:
      return VideoInputUnitEncoderMuxControl::Encoder::kLcd;
    case VIU_ENCI:
      return VideoInputUnitEncoderMuxControl::Encoder::kInterlaced;
    case VIU_ENCP:
      return VideoInputUnitEncoderMuxControl::Encoder::kProgressive;
    case VIU_ENCT:
      return VideoInputUnitEncoderMuxControl::Encoder::kTvPanel;
  }
  zxlogf(ERROR, "Incorrect VIU type: %u", type);
  return VideoInputUnitEncoderMuxControl::Encoder::kLcd;
}

}  // namespace

void HdmiHost::ConfigurePll(const pll_param& pll_params) {
  // Set VIU Mux Ctrl
  if (pll_params.viu_channel == 1) {
    VideoInputUnitEncoderMuxControl::Get()
        .ReadFrom(&(*vpu_mmio_))
        .set_vsync_shared_by_viu_blocks(false)
        .set_viu1_encoder_selection(
            EncoderSelectionFromViuType(static_cast<viu_type>(pll_params.viu_type)))
        .WriteTo(&(*vpu_mmio_));
  } else {
    VideoInputUnitEncoderMuxControl::Get()
        .ReadFrom(&(*vpu_mmio_))
        .set_vsync_shared_by_viu_blocks(false)
        .set_viu2_encoder_selection(
            EncoderSelectionFromViuType(static_cast<viu_type>(pll_params.viu_type)))
        .WriteTo(&(*vpu_mmio_));
  }
  HdmiClockControl::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_hdmi_tx_system_clock_selection(
          HdmiClockControl::HdmiTxSystemClockSource::kExternalOscillator24Mhz)
      .SetHdmiTxSystemClockDivider(1)
      .set_hdmi_tx_system_clock_enabled(true)
      .WriteTo(&*hhi_mmio_);

  ConfigureHpllClkOut(pll_params.hpll_clk_out);

  HhiHdmiPllCntlReg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_dpll_od1(pll_params.od1 >> 1)
      .set_hdmi_dpll_od2(pll_params.od2 >> 1)
      .set_hdmi_dpll_od3(pll_params.od3 >> 1)
      .WriteTo(&(*hhi_mmio_));

  ConfigureOd3Div(static_cast<uint8_t>(pll_params.vid_pll_div));

  VideoClock1Control::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_mux_source(VideoClockMuxSource::kVideoPll)
      .WriteTo(&*hhi_mmio_);
  VideoClock1Divider::Get()
      .ReadFrom(&(*hhi_mmio_))
      .SetDivider0((pll_params.vid_clk_div == 0) ? 1 : (pll_params.vid_clk_div))
      .WriteTo(&(*hhi_mmio_));
  VideoClock1Control::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_div4_enabled(true)
      .set_div2_enabled(true)
      .set_div1_enabled(true)
      .WriteTo(&*hhi_mmio_);

  HdmiClockControl::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_tx_pixel_clock_selection(HdmiClockControl::HdmiTxPixelClockSource::kVideoClock1)
      .WriteTo(&(*hhi_mmio_));

  VideoClockOutputControl::Get()
      .ReadFrom(&*hhi_mmio_)
      .set_hdmi_tx_pixel_clock_enabled(true)
      .WriteTo(&*hhi_mmio_);

  if (pll_params.encp_div != (uint32_t)-1) {
    VideoClock1Divider::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_encp_clock_selection(EncoderClockSource::kVideoClock1)
        .WriteTo(&(*hhi_mmio_));
    VideoClockOutputControl::Get()
        .ReadFrom(&*hhi_mmio_)
        .set_encoder_progressive_enabled(true)
        .WriteTo(&*hhi_mmio_);
    VideoClock1Control::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_divider0_enabled(true)
        .WriteTo(&(*hhi_mmio_));
  }
  if (pll_params.enci_div != (uint32_t)-1) {
    VideoClock1Divider::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_enci_clock_selection(EncoderClockSource::kVideoClock1)
        .WriteTo(&(*hhi_mmio_));
    VideoClockOutputControl::Get()
        .ReadFrom(&*hhi_mmio_)
        .set_encoder_interlaced_enabled(true)
        .WriteTo(&*hhi_mmio_);
    VideoClock1Control::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_divider0_enabled(true)
        .WriteTo(&(*hhi_mmio_));
  }
}

void HdmiHost::ConfigureHpllClkOut(uint32_t hpll) {
  float desired_pll = (float)hpll / (float)24000;
  uint8_t whole;
  uint16_t frac;
  whole = (uint8_t)desired_pll;
  frac = static_cast<uint16_t>(((float)desired_pll - (float)whole) * kFracMax);

  zxlogf(ERROR, "Desired PLL = %f (frac = %d, whole = %d) (hpll = %d)", desired_pll, frac, whole,
         hpll);

  HhiHdmiPllCntlReg::Get().FromValue(0x0b3a0400).set_hdmi_dpll_M(whole).WriteTo(&(*hhi_mmio_));

  /* Enable and reset */
  HhiHdmiPllCntlReg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_hdmi_dpll_en(1)
      .set_hdmi_dpll_reset(1)
      .WriteTo(&(*hhi_mmio_));

  HhiHdmiPllCntl1Reg::Get().FromValue(frac).WriteTo(&(*hhi_mmio_));
  HhiHdmiPllCntl2Reg::Get().FromValue(0x0).WriteTo(&(*hhi_mmio_));

  /* G12A HDMI PLL Needs specific parameters for 5.4GHz */
  if (whole >= 0xf7) {
    HhiHdmiPllCntl3Reg::Get().FromValue(0x6a685c00).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllCntl4Reg::Get().FromValue(0x11551293).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllCntl5Reg::Get().FromValue(0x39272000).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllStsReg::Get().FromValue(0x55540000).WriteTo(&(*hhi_mmio_));
  } else {
    HhiHdmiPllCntl3Reg::Get().FromValue(0x0a691c00).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllCntl4Reg::Get().FromValue(0x33771290).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllCntl5Reg::Get().FromValue(0x39272000).WriteTo(&(*hhi_mmio_));
    HhiHdmiPllStsReg::Get().FromValue(0x50540000).WriteTo(&(*hhi_mmio_));
  }

  /* Reset PLL */
  HhiHdmiPllCntlReg::Get().ReadFrom(&(*hhi_mmio_)).set_hdmi_dpll_reset(1).WriteTo(&(*hhi_mmio_));

  /* UN-Reset PLL */
  HhiHdmiPllCntlReg::Get().ReadFrom(&(*hhi_mmio_)).set_hdmi_dpll_reset(0).WriteTo(&(*hhi_mmio_));

  /* Poll for lock bits */
  WaitForPllLocked();
}

void HdmiHost::ConfigureOd3Div(uint32_t div_sel) {
  int shift_val = 0;
  int shift_sel = 0;

  /* When div 6.25, need to reset vid_pll_div */
  if (div_sel == VID_PLL_DIV_6p25) {
    usleep(1);
    /* TODO(fxb/69679): Add in Resets
    auto result = display->reset_register_.WriteRegister32(PRESET0_REGISTER, 1 << 7, 1 << 7);
    if ((result.status() != ZX_OK) || result->is_error()) {
      zxlogf(ERROR, "Reset0 Set failed");
    }
    */
  }
  // Disable the output clock
  HhiVidPllClkDivReg::Get()
      .ReadFrom(&(*hhi_mmio_))
      .set_clk_final_en(0)
      .set_set_preset(0)
      .WriteTo(&(*hhi_mmio_));

  switch (div_sel) {
    case VID_PLL_DIV_1:
      shift_val = 0xFFFF;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_2:
      shift_val = 0x0aaa;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_3:
      shift_val = 0x0db6;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_3p5:
      shift_val = 0x36cc;
      shift_sel = 1;
      break;
    case VID_PLL_DIV_3p75:
      shift_val = 0x6666;
      shift_sel = 2;
      break;
    case VID_PLL_DIV_4:
      shift_val = 0x0ccc;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_5:
      shift_val = 0x739c;
      shift_sel = 2;
      break;
    case VID_PLL_DIV_6:
      shift_val = 0x0e38;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_6p25:
      shift_val = 0x0000;
      shift_sel = 3;
      break;
    case VID_PLL_DIV_7:
      shift_val = 0x3c78;
      shift_sel = 1;
      break;
    case VID_PLL_DIV_7p5:
      shift_val = 0x78f0;
      shift_sel = 2;
      break;
    case VID_PLL_DIV_12:
      shift_val = 0x0fc0;
      shift_sel = 0;
      break;
    case VID_PLL_DIV_14:
      shift_val = 0x3f80;
      shift_sel = 1;
      break;
    case VID_PLL_DIV_15:
      shift_val = 0x7f80;
      shift_sel = 2;
      break;
    case VID_PLL_DIV_2p5:
      shift_val = 0x5294;
      shift_sel = 2;
      break;
    default:
      zxlogf(ERROR, "Error: clocks_set_vid_clk_div:  Invalid parameter");
      break;
  }

  if (shift_val == 0xffff) {  // if divide by 1
    SET_BIT32(HHI, HHI_VID_PLL_CLK_DIV, 1, 18, 1);
    HhiVidPllClkDivReg::Get().ReadFrom(&(*hhi_mmio_)).set_clk_div1(1).WriteTo(&(*hhi_mmio_));
  } else {
    HhiVidPllClkDivReg::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_clk_div1(0)
        .set_clk_sel(0)
        .set_set_preset(0)
        .set_shift_preset(0)
        .WriteTo(&(*hhi_mmio_));

    HhiVidPllClkDivReg::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_clk_sel(shift_sel)
        .set_set_preset(1)
        .WriteTo(&(*hhi_mmio_));

    HhiVidPllClkDivReg::Get()
        .ReadFrom(&(*hhi_mmio_))
        .set_shift_preset(shift_val)
        .set_set_preset(0)
        .WriteTo(&(*hhi_mmio_));
  }
  // Enable the final output clock
  HhiVidPllClkDivReg::Get().ReadFrom(&(*hhi_mmio_)).set_clk_final_en(1).WriteTo(&(*hhi_mmio_));
}

}  // namespace amlogic_display
