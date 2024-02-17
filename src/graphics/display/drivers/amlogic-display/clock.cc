// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/clock.h"

#include <lib/ddk/debug.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/status.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/fixed-point-util.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

namespace {
constexpr uint32_t kKHZ = 1000;

}  // namespace

Clock::Clock(fdf::MmioBuffer vpu_mmio, fdf::MmioBuffer hhi_mmio, bool clock_enabled)
    : vpu_mmio_(std::move(vpu_mmio)),
      hhi_mmio_(std::move(hhi_mmio)),
      clock_enabled_(clock_enabled) {}

// static
LcdTiming Clock::CalculateLcdTiming(const display::DisplayTiming& d) {
  LcdTiming out;
  // Calculate and store DataEnable horizontal and vertical start/stop times
  const uint32_t de_hstart = d.horizontal_total_px() - d.horizontal_active_px - 1;
  const uint32_t de_vstart = d.vertical_total_lines() - d.vertical_active_lines;
  out.vid_pixel_on = de_hstart;
  out.vid_line_on = de_vstart;

  // Calculate and Store HSync horizontal and vertical start/stop times
  const uint32_t hstart = (de_hstart + d.horizontal_total_px() - d.horizontal_back_porch_px -
                           d.horizontal_sync_width_px) %
                          d.horizontal_total_px();
  const uint32_t hend =
      (de_hstart + d.horizontal_total_px() - d.horizontal_back_porch_px) % d.horizontal_total_px();
  out.hs_hs_addr = hstart;
  out.hs_he_addr = hend;

  // Calculate and Store VSync horizontal and vertical start/stop times
  out.vs_hs_addr = (hstart + d.horizontal_total_px()) % d.horizontal_total_px();
  out.vs_he_addr = out.vs_hs_addr;
  const uint32_t vstart = (de_vstart + d.vertical_total_lines() - d.vertical_back_porch_lines -
                           d.vertical_sync_width_lines) %
                          d.vertical_total_lines();
  const uint32_t vend = (de_vstart + d.vertical_total_lines() - d.vertical_back_porch_lines) %
                        d.vertical_total_lines();
  out.vs_vs_addr = vstart;
  out.vs_ve_addr = vend;
  return out;
}

zx::result<> Clock::WaitForHdmiPllToLock() {
  uint32_t pll_lock;

  constexpr int kMaxPllLockAttempt = 3;
  for (int lock_attempts = 0; lock_attempts < kMaxPllLockAttempt; lock_attempts++) {
    zxlogf(TRACE, "Waiting for PLL Lock: (%d/3).", lock_attempts + 1);

    // The configurations used in retries are from Amlogic-provided code which
    // is undocumented.
    if (lock_attempts == 1) {
      hhi_mmio_.Write32(
          SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL3), /*field_begin_bit=*/31,
                          /*field_size_bits=*/1, /*field_value=*/1),
          HHI_HDMI_PLL_CNTL3);
    } else if (lock_attempts == 2) {
      hhi_mmio_.Write32(0x55540000, HHI_HDMI_PLL_CNTL6);  // more magic
    }

    int retries = 1000;
    while ((pll_lock = GetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                       /*field_begin_bit=*/LCD_PLL_LOCK_HPLL_G12A,
                                       /*field_size_bits=*/1)) != 1 &&
           retries--) {
      zx_nanosleep(zx_deadline_after(ZX_USEC(50)));
    }
    if (pll_lock) {
      return zx::ok();
    }
  }

  zxlogf(ERROR, "Failed to lock HDMI PLL after %d attempts.", kMaxPllLockAttempt);
  return zx::error(ZX_ERR_UNAVAILABLE);
}

// static
zx::result<PllConfig> Clock::GenerateHPLL(int32_t pixel_clock_frequency_khz,
                                          int64_t maximum_per_data_lane_bit_per_second) {
  PllConfig pll_cfg;
  uint32_t pll_fout;
  // Requested Pixel clock
  pll_cfg.fout = pixel_clock_frequency_khz;
  if (pll_cfg.fout > MAX_PIXEL_CLK_KHZ) {
    zxlogf(ERROR, "Pixel clock out of range (%u KHz)", pll_cfg.fout);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  constexpr uint32_t kMinClockFactor = 1u;
  constexpr uint32_t kMaxClockFactor = 255u;

  // If clock factor is not specified in display panel configuration, the driver
  // will find the first valid clock factor in between kMinClockFactor and
  // kMaxClockFactor (both inclusive).
  uint32_t clock_factor_min = kMinClockFactor;
  uint32_t clock_factor_max = kMaxClockFactor;

  for (uint32_t clock_factor = clock_factor_min; clock_factor <= clock_factor_max; clock_factor++) {
    pll_cfg.clock_factor = clock_factor;

    // Desired PLL Frequency based on pixel clock needed
    pll_fout = pll_cfg.fout * pll_cfg.clock_factor;

    // Make sure all clocks are within range
    // If these values are not within range, we will not have a valid display
    const int64_t dsi_bit_rate_max_khz = maximum_per_data_lane_bit_per_second / kKHZ;
    const int64_t dsi_bit_rate_min_khz = dsi_bit_rate_max_khz - pll_cfg.fout;
    if ((pll_fout < dsi_bit_rate_min_khz) || (pll_fout > dsi_bit_rate_max_khz)) {
      zxlogf(TRACE, "Calculated clocks out of range for xd = %u, skipped", clock_factor);
      continue;
    }

    // Now that we have valid frequency ranges, let's calculated all the PLL-related
    // multipliers/dividers
    // [fin] * [m/n] = [pll_vco]
    // [pll_vco] / [od1] / [od2] / [od3] = pll_fout
    // [fvco] --->[OD1] --->[OD2] ---> [OD3] --> pll_fout
    uint32_t od3, od2, od1;
    od3 = (1 << (MAX_OD_SEL - 1));
    while (od3) {
      uint32_t fod3 = pll_fout * od3;
      od2 = od3;
      while (od2) {
        uint32_t fod2 = fod3 * od2;
        od1 = od2;
        while (od1) {
          uint32_t fod1 = fod2 * od1;
          if ((fod1 >= MIN_PLL_VCO_KHZ) && (fod1 <= MAX_PLL_VCO_KHZ)) {
            // within range!
            pll_cfg.pll_od1_sel = od1 >> 1;
            pll_cfg.pll_od2_sel = od2 >> 1;
            pll_cfg.pll_od3_sel = od3 >> 1;
            pll_cfg.pll_fout = pll_fout;
            zxlogf(TRACE, "od1=%d, od2=%d, od3=%d", (od1 >> 1), (od2 >> 1), (od3 >> 1));
            zxlogf(TRACE, "pll_fvco=%d", fod1);
            pll_cfg.pll_fvco = fod1;
            // for simplicity, assume n = 1
            // calculate m such that fin x m = fod1
            uint32_t m;
            uint32_t pll_frac;
            fod1 = fod1 / 1;
            m = fod1 / FIN_FREQ_KHZ;
            pll_frac = (fod1 % FIN_FREQ_KHZ) * PLL_FRAC_RANGE / FIN_FREQ_KHZ;
            pll_cfg.pll_m = m;
            pll_cfg.pll_n = 1;
            pll_cfg.pll_frac = pll_frac;
            zxlogf(TRACE, "m=%d, n=%d, frac=0x%x", m, 1, pll_frac);
            pll_cfg.bitrate = pll_fout * kKHZ;  // Hz
            return zx::ok(std::move(pll_cfg));
          }
          od1 >>= 1;
        }
        od2 >>= 1;
      }
      od3 >>= 1;
    }
  }

  zxlogf(ERROR, "Could not generate correct PLL values for: ");
  zxlogf(ERROR, "  pixel_clock_frequency_khz = %" PRId32, pixel_clock_frequency_khz);
  zxlogf(ERROR, "  max_per_data_lane_bit_rate_hz = %" PRId64, maximum_per_data_lane_bit_per_second);
  return zx::error(ZX_ERR_INTERNAL);
}

void Clock::Disable() {
  if (!clock_enabled_) {
    return;
  }
  vpu_mmio_.Write32(0, ENCL_VIDEO_EN);

  VideoClockOutputControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encoder_lvds_enabled(false)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_div1_enabled(false)
      .set_div2_enabled(false)
      .set_div4_enabled(false)
      .set_div6_enabled(false)
      .set_div12_enabled(false)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(false).WriteTo(&hhi_mmio_);

  // disable pll
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_EN_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/0),
                    HHI_HDMI_PLL_CNTL0);
  clock_enabled_ = false;
}

zx::result<> Clock::Enable(const PanelConfig& panel_config) {
  if (clock_enabled_) {
    return zx::ok();
  }

  // Populate internal LCD timing structure based on predefined tables
  lcd_timing_ = CalculateLcdTiming(panel_config.display_timing);
  zx::result<PllConfig> pll_result =
      GenerateHPLL(panel_config.display_timing.pixel_clock_frequency_khz,
                   panel_config.maximum_per_data_lane_bit_per_second());
  if (pll_result.is_error()) {
    zxlogf(ERROR, "Failed to generate HDMI PLL and Video clock tree configuration: %s",
           pll_result.status_string());
    return pll_result.take_error();
  }
  pll_cfg_ = std::move(pll_result).value();

  uint32_t regVal;
  PllConfig* pll_cfg = &pll_cfg_;
  bool useFrac = !!pll_cfg->pll_frac;

  regVal = ((1 << LCD_PLL_EN_HPLL_G12A) | (1 << LCD_PLL_OUT_GATE_CTRL_G12A) |  // clk out gate
            (pll_cfg->pll_n << LCD_PLL_N_HPLL_G12A) | (pll_cfg->pll_m << LCD_PLL_M_HPLL_G12A) |
            (pll_cfg->pll_od1_sel << LCD_PLL_OD1_HPLL_G12A) |
            (pll_cfg->pll_od2_sel << LCD_PLL_OD2_HPLL_G12A) |
            (pll_cfg->pll_od3_sel << LCD_PLL_OD3_HPLL_G12A) | (useFrac ? (1 << 27) : (0 << 27)));
  hhi_mmio_.Write32(regVal, HHI_HDMI_PLL_CNTL0);

  hhi_mmio_.Write32(pll_cfg->pll_frac, HHI_HDMI_PLL_CNTL1);
  hhi_mmio_.Write32(0x00, HHI_HDMI_PLL_CNTL2);
  // Magic numbers from U-Boot.
  hhi_mmio_.Write32(useFrac ? 0x6a285c00 : 0x48681c00, HHI_HDMI_PLL_CNTL3);
  hhi_mmio_.Write32(useFrac ? 0x65771290 : 0x33771290, HHI_HDMI_PLL_CNTL4);
  hhi_mmio_.Write32(0x39272000, HHI_HDMI_PLL_CNTL5);
  hhi_mmio_.Write32(useFrac ? 0x56540000 : 0x56540000, HHI_HDMI_PLL_CNTL6);

  // reset dpll
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_RST_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/1),
                    HHI_HDMI_PLL_CNTL0);
  zx_nanosleep(zx_deadline_after(ZX_USEC(100)));
  // release from reset
  hhi_mmio_.Write32(SetFieldValue32(hhi_mmio_.Read32(HHI_HDMI_PLL_CNTL0),
                                    /*field_begin_bit=*/LCD_PLL_RST_HPLL_G12A,
                                    /*field_size_bits=*/1, /*field_value=*/0),
                    HHI_HDMI_PLL_CNTL0);

  zx_nanosleep(zx_deadline_after(ZX_USEC(50)));
  zx::result<> wait_for_pll_lock_result = WaitForHdmiPllToLock();
  if (!wait_for_pll_lock_result.is_ok()) {
    zxlogf(ERROR, "Failed to lock HDMI PLL: %s", wait_for_pll_lock_result.status_string());
    return wait_for_pll_lock_result.take_error();
  }

  // Disable Video Clock mux 2 since we are changing its input selection.
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(false).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  // Sets the HDMI clock tree frequency division ratio to 1.

  // Disable the HDMI clock tree output
  HdmiClockTreeControl hdmi_clock_tree_control = HdmiClockTreeControl::Get().ReadFrom(&hhi_mmio_);
  hdmi_clock_tree_control.set_clock_output_enabled(false).WriteTo(&hhi_mmio_);

  hdmi_clock_tree_control.set_preset_pattern_update_enabled(false).WriteTo(&hhi_mmio_);
  hdmi_clock_tree_control.SetFrequencyDividerRatio(ToU28_4(1.0)).WriteTo(&hhi_mmio_);

  // Enable the final output clock
  hdmi_clock_tree_control.set_clock_output_enabled(true).WriteTo(&hhi_mmio_);

  // Enable DSI measure clocks.
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_dsi_measure_clock_selection(
          VideoInputMeasureClockControl::ClockSource::kExternalOscillator24Mhz)
      .WriteTo(&hhi_mmio_);
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .SetDsiMeasureClockDivider(1)
      .WriteTo(&hhi_mmio_);
  VideoInputMeasureClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_dsi_measure_clock_enabled(true)
      .WriteTo(&hhi_mmio_);

  // Use Video PLL (vid_pll) as MIPI_DSY PHY clock source.
  MipiDsiPhyClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_clock_source(MipiDsiPhyClockControl::ClockSource::kVideoPll)
      .WriteTo(&hhi_mmio_);
  // Enable MIPI-DSY PHY clock.
  MipiDsiPhyClockControl::Get().ReadFrom(&hhi_mmio_).set_enabled(true).WriteTo(&hhi_mmio_);
  // Set divider to 1.
  // TODO(https://fxbug.dev/42082049): This should occur before enabling the clock.
  MipiDsiPhyClockControl::Get().ReadFrom(&hhi_mmio_).SetDivider(1).WriteTo(&hhi_mmio_);

  // Set the Video clock 2 divider.
  VideoClock2Divider::Get()
      .ReadFrom(&hhi_mmio_)
      .SetDivider2(pll_cfg_.clock_factor)
      .WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  VideoClock2Control::Get()
      .ReadFrom(&hhi_mmio_)
      .set_mux_source(VideoClockMuxSource::kVideoPll)
      .WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_clock_enabled(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(2)));

  // Select video clock 2 for ENCL clock.
  VideoClock2Divider::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encl_clock_selection(EncoderClockSource::kVideoClock2)
      .WriteTo(&hhi_mmio_);
  // Enable video clock 2 divider.
  VideoClock2Divider::Get().ReadFrom(&hhi_mmio_).set_divider_enabled(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_div1_enabled(true).WriteTo(&hhi_mmio_);
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_soft_reset(true).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
  VideoClock2Control::Get().ReadFrom(&hhi_mmio_).set_soft_reset(false).WriteTo(&hhi_mmio_);
  zx_nanosleep(zx_deadline_after(ZX_USEC(5)));

  // Enable ENCL clock output.
  VideoClockOutputControl::Get()
      .ReadFrom(&hhi_mmio_)
      .set_encoder_lvds_enabled(true)
      .WriteTo(&hhi_mmio_);

  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));

  vpu_mmio_.Write32(0, ENCL_VIDEO_EN);

  // Connect both VIUs (Video Input Units) to the LCD Encoder.
  VideoInputUnitEncoderMuxControl::Get()
      .ReadFrom(&vpu_mmio_)
      .set_vsync_shared_by_viu_blocks(false)
      .set_viu1_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kLcd)
      .set_viu2_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kLcd)
      .WriteTo(&vpu_mmio_);

  // Undocumented registers below
  vpu_mmio_.Write32(0x8000, ENCL_VIDEO_MODE);      // bit[15] shadown en
  vpu_mmio_.Write32(0x0418, ENCL_VIDEO_MODE_ADV);  // Sampling rate: 1

  // bypass filter -- Undocumented registers
  const display::DisplayTiming& display_timing = panel_config.display_timing;
  vpu_mmio_.Write32(0x1000, ENCL_VIDEO_FILT_CTRL);
  vpu_mmio_.Write32(display_timing.horizontal_total_px() - 1, ENCL_VIDEO_MAX_PXCNT);
  vpu_mmio_.Write32(display_timing.vertical_total_lines() - 1, ENCL_VIDEO_MAX_LNCNT);
  vpu_mmio_.Write32(lcd_timing_.vid_pixel_on, ENCL_VIDEO_HAVON_BEGIN);
  vpu_mmio_.Write32(display_timing.horizontal_active_px - 1 + lcd_timing_.vid_pixel_on,
                    ENCL_VIDEO_HAVON_END);
  vpu_mmio_.Write32(lcd_timing_.vid_line_on, ENCL_VIDEO_VAVON_BLINE);
  vpu_mmio_.Write32(display_timing.vertical_active_lines - 1 + lcd_timing_.vid_line_on,
                    ENCL_VIDEO_VAVON_ELINE);
  vpu_mmio_.Write32(lcd_timing_.hs_hs_addr, ENCL_VIDEO_HSO_BEGIN);
  vpu_mmio_.Write32(lcd_timing_.hs_he_addr, ENCL_VIDEO_HSO_END);
  vpu_mmio_.Write32(lcd_timing_.vs_hs_addr, ENCL_VIDEO_VSO_BEGIN);
  vpu_mmio_.Write32(lcd_timing_.vs_he_addr, ENCL_VIDEO_VSO_END);
  vpu_mmio_.Write32(lcd_timing_.vs_vs_addr, ENCL_VIDEO_VSO_BLINE);
  vpu_mmio_.Write32(lcd_timing_.vs_ve_addr, ENCL_VIDEO_VSO_ELINE);
  vpu_mmio_.Write32(3, ENCL_VIDEO_RGBIN_CTRL);
  vpu_mmio_.Write32(1, ENCL_VIDEO_EN);

  vpu_mmio_.Write32(0, L_RGB_BASE_ADDR);
  vpu_mmio_.Write32(0x400, L_RGB_COEFF_ADDR);
  vpu_mmio_.Write32(0x400, L_DITH_CNTL_ADDR);

  // The driver behavior here deviates from the Amlogic-provided code.
  //
  // The Amlogic-provided code sets up the timing controller (TCON) within the
  // LCD Encoder to generate Display Enable (DE), Horizontal Sync (HSYNC) and
  // Vertical Sync (VSYNC) timing signals.
  //
  // These signals are useful for LVDS or TTL LCD interfaces, but not for the
  // MIPI-DSI interface. The MIPI-DSI Host Controller IP block always sets the
  // output timings using the values from its own control registers, and it
  // doesn't use the outputs from the timing controller.
  //
  // Therefore, this driver doesn't set the timing controller registers and
  // keeps the timing controller disabled. This was tested on Nelson (S905D3)
  // and Khadas VIM3 (A311D) boards.

  vpu_mmio_.Write32(vpu_mmio_.Read32(VPP_MISC) & ~(VPP_OUT_SATURATE), VPP_MISC);

  // Ready to be used
  clock_enabled_ = true;
  return zx::ok();
}

void Clock::SetVideoOn(bool on) { vpu_mmio_.Write32(on, ENCL_VIDEO_EN); }

// static
zx::result<std::unique_ptr<Clock>> Clock::Create(ddk::PDevFidl& pdev, bool already_enabled) {
  zx::result<fdf::MmioBuffer> vpu_mmio_result = MapMmio(MmioResourceIndex::kVpu, pdev);
  if (vpu_mmio_result.is_error()) {
    return vpu_mmio_result.take_error();
  }
  fdf::MmioBuffer vpu_mmio = std::move(vpu_mmio_result).value();

  zx::result<fdf::MmioBuffer> hhi_mmio_result = MapMmio(MmioResourceIndex::kHhi, pdev);
  if (hhi_mmio_result.is_error()) {
    return hhi_mmio_result.take_error();
  }
  fdf::MmioBuffer hhi_mmio = std::move(hhi_mmio_result).value();

  fbl::AllocChecker alloc_checker;
  auto clock =
      fbl::make_unique_checked<Clock>(&alloc_checker, std::move(vpu_mmio), std::move(hhi_mmio),
                                      /*clock_enabled=*/already_enabled);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for Clock");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(clock));
}

}  // namespace amlogic_display
