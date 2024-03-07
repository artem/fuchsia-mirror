// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/hdmi-host.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/errors.h>

#include <limits>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/clock-regs.h"
#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/gpio-mux-regs.h"
#include "src/graphics/display/drivers/amlogic-display/hhi-regs.h"
#include "src/graphics/display/drivers/amlogic-display/power-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware-hdmi/hdmi-transmitter-controller-impl.h"
#include "src/graphics/display/lib/designware-hdmi/hdmi-transmitter-controller.h"

namespace amlogic_display {

namespace {

// Range of valid frequencies of the DCO (digitally controlled oscillator) of a
// certain PLL (phase-locked loop).
struct ValidDcoFrequencyRange {
  int64_t minimum_frequency_khz;
  int64_t maximum_frequency_khz;
};

ValidDcoFrequencyRange GetHdmiPllValidDcoFrequencyRange(int64_t pixel_clock_khz) {
  // Amlogic datasheets (A311D, S905D2 and S905D3) specify that the frequency
  // of the DCO in the HDMI PLL must be between 3 GHz and 6 GHz.
  //
  // However, [1] Amlogic-provided code uses 2.97 GHz for some common display
  // resolutions; [2] Experiments on Khadas VIM3 (Amlogic A311D) also shows
  // that 2.9 GHz is a valid DCO frequency for all the display timings we have
  // tested and has fewer display glitches than using 5.8 GHz. So, we use
  // 2.9 GHz rather than 3 GHz as the minimum valid DCO frequency for default
  // cases.
  static constexpr int64_t kDefaultMinimumValidHdmiPllDcoFrequencyKhz = 2'900'000;
  static constexpr int64_t kDefaultMaximumValidHdmiPllDcoFrequencyKhz = 6'000'000;

  // For display timings with a very low pixel clock rate (for example, on
  // Surenoo SUR480480Y021A, it has a pixel clock of 16.96 MHz), in our
  // experiments, we had to lower the minimum allowed DCO frequency to 2.7 GHz
  // in order to keep the correct display aspect ratio.
  //
  // Since this is not a valid frequency documented in the datasheets, this
  // should only be used as an exception when the pixel clock rate is very
  // low. Thus, we only set the minimum allowed DCO frequency to 2.7 GHz, if
  // the pixel clock is lower than 20 MHz (which is lower than the pixel clock
  // of DMT timing of 640x480p@60Hz) so that it won't affect "normal" display
  // modes.
  static constexpr int64_t kLowPixelClockMinimumValidHdmiPllDcoFrequencyKhz = 2'700'000;
  static constexpr int64_t kLowPixelClockThresholdKhz = 20'000;
  if (pixel_clock_khz <= kLowPixelClockThresholdKhz) {
    return {
        .minimum_frequency_khz = kLowPixelClockMinimumValidHdmiPllDcoFrequencyKhz,
        .maximum_frequency_khz = kDefaultMaximumValidHdmiPllDcoFrequencyKhz,
    };
  }

  return {
      .minimum_frequency_khz = kDefaultMinimumValidHdmiPllDcoFrequencyKhz,
      .maximum_frequency_khz = kDefaultMaximumValidHdmiPllDcoFrequencyKhz,
  };
}

// `timing` must be a timing supported by `HdmiHost`.
pll_param CalculateClockParameters(const display::DisplayTiming& timing) {
  pll_param params;

  // TODO: We probably need a more sophisticated method for calculating
  // clocks. This will do for now.
  params.viu_channel = 1;
  params.viu_type = VIU_ENCP;

  params.hdmi_clock_tree_vid_pll_divider = 5;
  params.video_clock1_divider = 2;
  params.hdmi_transmitter_pixel_clock_divider = 1;
  params.encp_clock_divider = 1;

  params.output_divider1 = 1;
  params.output_divider2 = 1;
  params.output_divider3 = 1;

  params.hdmi_pll_vco_output_frequency_khz = int64_t{timing.pixel_clock_frequency_khz} * 10;

  const ValidDcoFrequencyRange valid_dco_frequency_range =
      GetHdmiPllValidDcoFrequencyRange(timing.pixel_clock_frequency_khz);
  while (params.hdmi_pll_vco_output_frequency_khz <
         valid_dco_frequency_range.minimum_frequency_khz) {
    if (params.output_divider1 < 4) {
      params.output_divider1 *= 2;
      params.hdmi_pll_vco_output_frequency_khz *= 2;
    } else if (params.output_divider2 < 4) {
      params.output_divider2 *= 2;
      params.hdmi_pll_vco_output_frequency_khz *= 2;
    } else if (params.output_divider3 < 4) {
      params.output_divider3 *= 2;
      params.hdmi_pll_vco_output_frequency_khz *= 2;
    } else {
      ZX_DEBUG_ASSERT_MSG(false,
                          "Failed to set HDMI PLL to a valid VCO frequency range for pixel clock "
                          "%d kHz. This should never happen since IsDisplayTimingSupported() "
                          "returned true.",
                          timing.pixel_clock_frequency_khz);
    }
  }
  ZX_DEBUG_ASSERT_MSG(
      params.hdmi_pll_vco_output_frequency_khz <= valid_dco_frequency_range.maximum_frequency_khz,
      "Calculated HDMI PLL VCO frequency (%" PRId64 " kHz) exceeds the VCO frequency limit %" PRId64
      " kHz. This should never happen since "
      "IsDisplayTimingSupported() returned true.",
      params.hdmi_pll_vco_output_frequency_khz, valid_dco_frequency_range.maximum_frequency_khz);
  return params;
}

zx::result<std::unique_ptr<HdmiTransmitter>> CreateHdmiTransmitter(ddk::PDevFidl& pdev) {
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "PDev protocol is invalid");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  zx::result<fdf::MmioBuffer> hdmi_tx_mmio_result =
      MapMmio(MmioResourceIndex::kHdmiTxController, pdev);
  if (hdmi_tx_mmio_result.is_error()) {
    return hdmi_tx_mmio_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<designware_hdmi::HdmiTransmitterController> designware_controller =
      fbl::make_unique_checked<designware_hdmi::HdmiTransmitterControllerImpl>(
          &alloc_checker, std::move(hdmi_tx_mmio_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Could not allocate memory for DesignWare HdmiTransmitterControllerImpl");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<fdf::MmioBuffer> hdmi_top_mmio_result = MapMmio(MmioResourceIndex::kHdmiTxTop, pdev);
  if (hdmi_top_mmio_result.is_error()) {
    return hdmi_top_mmio_result.take_error();
  }

  zx::result<zx::resource> smc_result =
      GetSecureMonitorCall(SecureMonitorCallResourceIndex::kSiliconProvider, pdev);
  if (smc_result.is_error()) {
    return smc_result.take_error();
  }

  std::unique_ptr<HdmiTransmitter> hdmi_transmitter = fbl::make_unique_checked<HdmiTransmitter>(
      &alloc_checker, std::move(designware_controller), std::move(hdmi_top_mmio_result).value(),
      std::move(smc_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Could not allocate memory for HdmiTransmitter");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(hdmi_transmitter));
}

}  // namespace

HdmiHost::HdmiHost(std::unique_ptr<HdmiTransmitter> hdmi_transmitter, fdf::MmioBuffer vpu_mmio,
                   fdf::MmioBuffer hhi_mmio, fdf::MmioBuffer gpio_mux_mmio)
    : hdmi_transmitter_(std::move(hdmi_transmitter)),
      vpu_mmio_(std::move(vpu_mmio)),
      hhi_mmio_(std::move(hhi_mmio)),
      gpio_mux_mmio_(std::move(gpio_mux_mmio)) {
  ZX_DEBUG_ASSERT(hdmi_transmitter_ != nullptr);
}

// static
zx::result<std::unique_ptr<HdmiHost>> HdmiHost::Create(zx_device_t* parent) {
  ddk::PDevFidl pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Could not get the platform device client.");
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result<fdf::MmioBuffer> vpu_mmio_result = MapMmio(MmioResourceIndex::kVpu, pdev);
  if (vpu_mmio_result.is_error()) {
    return vpu_mmio_result.take_error();
  }

  zx::result<fdf::MmioBuffer> hhi_mmio_result = MapMmio(MmioResourceIndex::kHhi, pdev);
  if (hhi_mmio_result.is_error()) {
    return hhi_mmio_result.take_error();
  }

  zx::result<fdf::MmioBuffer> gpio_mux_mmio_result = MapMmio(MmioResourceIndex::kGpioMux, pdev);
  if (gpio_mux_mmio_result.is_error()) {
    return gpio_mux_mmio_result.take_error();
  }

  zx::result<std::unique_ptr<HdmiTransmitter>> hdmi_transmitter = CreateHdmiTransmitter(pdev);
  if (hdmi_transmitter.is_error()) {
    zxlogf(ERROR, "Could not create HDMI transmitter: %s", hdmi_transmitter.status_string());
    return hdmi_transmitter.take_error();
  }
  ZX_ASSERT(hdmi_transmitter.value() != nullptr);

  fbl::AllocChecker alloc_checker;
  std::unique_ptr<HdmiHost> hdmi_host = fbl::make_unique_checked<HdmiHost>(
      &alloc_checker, std::move(hdmi_transmitter).value(), std::move(vpu_mmio_result).value(),
      std::move(hhi_mmio_result).value(), std::move(gpio_mux_mmio_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Could not allocate memory for the HdmiHost instance.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(hdmi_host));
}

zx_status_t HdmiHost::HostOn() {
  /* Step 1: Initialize various clocks related to the HDMI Interface*/
  gpio_mux_mmio_.Write32(
      SetFieldValue32(gpio_mux_mmio_.Read32(PAD_PULL_UP_EN_REG3), /*field_begin_bit=*/0,
                      /*field_size_bits=*/2, /*field_value=*/0),
      PAD_PULL_UP_EN_REG3);
  gpio_mux_mmio_.Write32(
      SetFieldValue32(gpio_mux_mmio_.Read32(PAD_PULL_UP_REG3), /*field_begin_bit=*/0,
                      /*field_size_bits=*/2, /*field_value=*/0),
      PAD_PULL_UP_REG3);
  gpio_mux_mmio_.Write32(
      SetFieldValue32(gpio_mux_mmio_.Read32(P_PREG_PAD_GPIO3_EN_N), /*field_begin_bit=*/0,
                      /*field_size_bits=*/2, /*field_value=*/3),
      P_PREG_PAD_GPIO3_EN_N);
  gpio_mux_mmio_.Write32(
      SetFieldValue32(gpio_mux_mmio_.Read32(PERIPHS_PIN_MUX_B), /*field_begin_bit=*/0,
                      /*field_size_bits=*/8, /*field_value=*/0x11),
      PERIPHS_PIN_MUX_B);

  // enable clocks
  HdmiClockControl::Get()
      .ReadFrom(&hhi_mmio_)
      .SetHdmiTxSystemClockDivider(1)
      .set_hdmi_tx_system_clock_enabled(true)
      .set_hdmi_tx_system_clock_selection(
          HdmiClockControl::HdmiTxSystemClockSource::kExternalOscillator24Mhz)
      .WriteTo(&hhi_mmio_);

  // enable clk81 (needed for HDMI module and a bunch of other modules)
  HhiGclkMpeg2Reg::Get().ReadFrom(&hhi_mmio_).set_clk81_en(1).WriteTo(&hhi_mmio_);

  // TODO(fxbug.com/132123): HDMI memory was supposed to be powered on during
  // the VPU power sequence. The AMLogic-supplied bringup code pauses for 5us
  // between each bit flip.
  auto memory_power0 = MemoryPower0::Get().ReadFrom(&hhi_mmio_);
  memory_power0.set_hdmi_memory0_powered_off(false);
  memory_power0.set_hdmi_memory1_powered_off(false);
  memory_power0.set_hdmi_memory2_powered_off(false);
  memory_power0.set_hdmi_memory3_powered_off(false);
  memory_power0.set_hdmi_memory4_powered_off(false);
  memory_power0.set_hdmi_memory5_powered_off(false);
  memory_power0.set_hdmi_memory6_powered_off(false);
  memory_power0.set_hdmi_memory7_powered_off(false);
  memory_power0.WriteTo(&hhi_mmio_);

  zx::result<> reset_result = hdmi_transmitter_->Reset();  // only supports 1 display for now
  if (reset_result.is_error()) {
    zxlogf(ERROR, "Failed to reset the HDMI transmitter: %s", reset_result.status_string());
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void HdmiHost::HostOff() {
  /* Close HDMITX PHY */
  hhi_mmio_.Write32(0, HHI_HDMI_PHY_CNTL0);
  hhi_mmio_.Write32(0, HHI_HDMI_PHY_CNTL3);
  /* Disable HPLL */
  hhi_mmio_.Write32(0, HHI_HDMI_PLL_CNTL0);
}

zx_status_t HdmiHost::ModeSet(const display::DisplayTiming& timing) {
  if (!IsDisplayTimingSupported(timing)) {
    zxlogf(ERROR,
           "Display timing (%" PRIu32 " x %" PRIu32 " @ pixel rate %" PRIu32
           " kHz) is not supported.",
           timing.horizontal_active_px, timing.vertical_active_lines,
           timing.pixel_clock_frequency_khz);
    return ZX_ERR_NOT_SUPPORTED;
  }

  pll_param clock_params = CalculateClockParameters(timing);
  ConfigurePll(clock_params);

  vpu_mmio_.Write32(0, VPU_ENCP_VIDEO_EN);
  vpu_mmio_.Write32(0, VPU_ENCI_VIDEO_EN);
  // Connect both VIUs (Video Input Units) to the Progressive Encoder (ENCP),
  // assuming the display is progressive.
  VideoInputUnitEncoderMuxControl::Get()
      .ReadFrom(&vpu_mmio_)
      .set_vsync_shared_by_viu_blocks(false)
      .set_viu1_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kProgressive)
      .set_viu2_encoder_selection(VideoInputUnitEncoderMuxControl::Encoder::kProgressive)
      .WriteTo(&vpu_mmio_);

  // Configure Encoder with detailed timing info (based on resolution)
  ConfigEncoder(timing);

  // Configure VDAC
  hhi_mmio_.Write32(0, HHI_VDAC_CNTL0_G12A);
  hhi_mmio_.Write32(8, HHI_VDAC_CNTL1_G12A);  // set Cdac_pwd [whatever that is]

  static constexpr designware_hdmi::ColorParam kColorParams{
      .input_color_format = designware_hdmi::ColorFormat::kCf444,

      // We choose the RGB 4:4:4 encoding unconditionally for the HDMI output
      // signals. This implies that we avoid YCbCr encodings, even if they are
      // unsupported.
      //
      // The HDMI specificiaton v1.4b, Section 6.2.3 "Pixel Encoding
      // Requirements" (page 106) requires that all HDMI sources and sinks
      // support RGB 4:4:4 encoding. Thus we think this approach will work with
      // all of our devices.
      //
      // Also, we encountered hardware (Yongxing HDMI to MIPI-DSI converters
      // board v1.2, using the Toshiba TC358870XBG converter chip, provided with
      // the Amelin AML028-30MB-A1 assembly) that claims support for the YCbCr
      // 4:4:4 pixel encoding in EDID, but does not display colors correctly
      // when we use that encoding. That hardware should be considered when
      // changing this strategy.
      .output_color_format = designware_hdmi::ColorFormat::kCfRgb,

      .color_depth = designware_hdmi::ColorDepth::kCd24B,
  };
  zx::result<> modeset_result = hdmi_transmitter_->ModeSet(timing, kColorParams);
  if (modeset_result.is_error()) {
    zxlogf(ERROR, "Failed to set display mode: %s", modeset_result.status_string());
    return modeset_result.status_value();
  }

  // Setup HDMI related registers in VPU
  // not really needed since we are not converting from 420/422. but set anyways
  VpuHdmiFmtCtrlReg::Get()
      .FromValue(0)
      .set_cntl_chroma_dnsmp(2)
      .set_cntl_hdmi_dith_en(0)
      .set_rounding_enable(1)
      .WriteTo(&vpu_mmio_);

  // setup some magic registers
  VpuHdmiDithCntlReg::Get()
      .ReadFrom(&vpu_mmio_)
      .set_cntl_hdmi_dith_en(1)
      .set_hsync_invert(0)
      .set_vsync_invert(0)
      .WriteTo(&vpu_mmio_);

  // reset vpu bridge
  uint32_t wr_rate = VpuHdmiSettingReg::Get().ReadFrom(&vpu_mmio_).wr_rate();
  vpu_mmio_.Write32(0, VPU_ENCP_VIDEO_EN);
  VpuHdmiSettingReg::Get().ReadFrom(&vpu_mmio_).set_src_sel(0).set_wr_rate(0).WriteTo(&vpu_mmio_);
  usleep(1);
  vpu_mmio_.Write32(1, VPU_ENCP_VIDEO_EN);
  usleep(1);
  VpuHdmiSettingReg::Get().ReadFrom(&vpu_mmio_).set_wr_rate(wr_rate).WriteTo(&vpu_mmio_);
  usleep(1);
  VpuHdmiSettingReg::Get().ReadFrom(&vpu_mmio_).set_src_sel(2).WriteTo(&vpu_mmio_);

  // setup hdmi phy
  ConfigPhy();

  zxlogf(INFO, "done!!");
  return ZX_OK;
}

zx_status_t HdmiHost::EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) {
  zx::result<> i2c_transact_result = hdmi_transmitter_->I2cTransact(op_list, op_count);
  if (i2c_transact_result.is_error()) {
    zxlogf(ERROR, "Failed to transfer EDID: %s", i2c_transact_result.status_string());
    return i2c_transact_result.status_value();
  }
  return ZX_OK;
}

namespace {

// Returns true iff the display PLL and clock trees can be programmed to
// generate a pixel clock of `pixel_clock_khz` kHz.
bool IsPixelClockSupported(int64_t pixel_clock_khz) {
  const ValidDcoFrequencyRange valid_dco_frequency_range =
      GetHdmiPllValidDcoFrequencyRange(pixel_clock_khz);

  // Fixed divisor values.
  //
  // HDMI clock tree divisor `vid_pll_div` == 5,
  // Video tree divisor /N0 `vid_clk_div` == 2,
  // Video tree ENCP clock selector `encp_div` == 1.
  //
  // TODO(https://fxbug.dev/42083149): Factor this out for pixel clock checking and
  // calculation logics.
  constexpr int kFixedPllDivisionFactor = 5 * 2 * 1;

  // TODO(https://fxbug.dev/42083149): Factor out ranges for each output frequency
  // divider so that they can be used for both clock checking and calculation.
  // OD1 = OD2 = OD3 = 1.
  constexpr int kMinimumPllDivisionFactor = 1 * 1 * 1;
  // OD1 = OD2 = OD3 = 4.
  constexpr int kMaximumPllDivisionFactor = 4 * 4 * 4;

  // The adjustable dividers OD1 / OD2 / OD3 cannot be calculated if the output
  // frequency using `kMinimumPllDivisionFactor` still exceeds the maximum
  // allowed value.
  const int64_t maximum_allowed_pixel_clock_khz =
      valid_dco_frequency_range.maximum_frequency_khz /
      (kFixedPllDivisionFactor * kMinimumPllDivisionFactor);
  if (pixel_clock_khz > maximum_allowed_pixel_clock_khz) {
    return false;
  }

  // The adjustable dividers OD1 / OD2 / OD3 cannot be calculated if the output
  // frequency using `kMaximumPllDivisionFactor` is still less than the minimum
  // allowed value.

  // ceil(kMinimumValidHdmiPllVcoFrequencyKhz / (kFixedPllDivisionFactor *
  // kMaximumPllDivisionFactor))
  const int64_t minimum_allowed_pixel_clock_khz =
      (valid_dco_frequency_range.minimum_frequency_khz +
       kFixedPllDivisionFactor * kMaximumPllDivisionFactor - 1) /
      (kFixedPllDivisionFactor * kMaximumPllDivisionFactor);
  if (pixel_clock_khz < minimum_allowed_pixel_clock_khz) {
    return false;
  }

  return true;
}

}  // namespace

bool HdmiHost::IsDisplayTimingSupported(const display::DisplayTiming& timing) const {
  // TODO(https://fxbug.dev/42075808): High-resolution display modes (4K or more) are not
  // supported.
  const int kMaximumAllowedWidthPixels = 2560;
  const int kMaximumAllowedHeightPixels = 1600;

  if (timing.horizontal_active_px > kMaximumAllowedWidthPixels ||
      timing.vertical_active_lines > kMaximumAllowedHeightPixels) {
    return false;
  }

  // TODO(https://fxbug.dev/42083230): Interlaced modes are not supported.
  if (timing.fields_per_frame == display::FieldsPerFrame::kInterlaced) {
    return false;
  }

  // TODO(https://fxbug.dev/42083230): Interlaced modes with alternating vblanks are not
  // supported.
  if (timing.vblank_alternates) {
    return false;
  }

  // TODO(https://fxbug.dev/42084414): Modes with pixel repetition are not supported.
  if (timing.pixel_repetition != 0) {
    return false;
  }

  if (!IsPixelClockSupported(timing.pixel_clock_frequency_khz)) {
    return false;
  }

  return true;
}

void HdmiHost::ConfigEncoder(const display::DisplayTiming& timing) {
  // TODO(https://fxbug.dev/42084909): For timings that have repetitive pixels
  // (for example, 1440x480p60 and 1440x480i60), the Amlogic-provided code has
  // contradictory and (in most cases) incomplete configurations. Thus, we'll
  // reject all such formats.
  ZX_DEBUG_ASSERT(timing.pixel_repetition == 0);

  // TODO(https://fxbug.dev/42084909): The current code assumes the timing is for
  // progressive fields.
  ZX_DEBUG_ASSERT(timing.fields_per_frame == display::FieldsPerFrame::kProgressive);

  vpu_mmio_.Write32(timing.horizontal_total_px() - 1, VPU_ENCP_VIDEO_MAX_PXCNT);
  vpu_mmio_.Write32(timing.vertical_total_lines() - 1, VPU_ENCP_VIDEO_MAX_LNCNT);

  vpu_mmio_.Write32(0, VPU_ENCI_VIDEO_EN);
  vpu_mmio_.Write32(1, VPU_ENCP_VIDEO_EN);

  vpu_mmio_.Write32(0x4040, VPU_ENCP_VIDEO_MODE);
  vpu_mmio_.Write32(0x18, VPU_ENCP_VIDEO_MODE_ADV);

  vpu_mmio_.Write32(SetFieldValue32(vpu_mmio_.Read32(VPU_ENCP_VIDEO_MODE), /*field_begin_bit=*/14,
                                    /*field_size_bits=*/1, /*field_value=*/1),
                    VPU_ENCP_VIDEO_MODE);  // DE Signal polarity

  const int video_horizontal_sync_begin_px = 0;
  const int video_horizontal_sync_end_px =
      video_horizontal_sync_begin_px + timing.horizontal_sync_width_px - 1;
  const int video_horizontal_active_begin_px =
      video_horizontal_sync_end_px + 1 + timing.horizontal_back_porch_px;
  const int video_horizontal_active_end_px =
      video_horizontal_active_begin_px + timing.horizontal_active_px - 1;
  const int video_horizontal_period_px = timing.horizontal_total_px();

  // Experiments on Khadas VIM3 (using Amlogic A311D) show that HAVON_*
  // registers must be set before HSO_* registers, otherwise the encoder
  // won't work.
  vpu_mmio_.Write32(video_horizontal_active_begin_px, VPU_ENCP_VIDEO_HAVON_BEGIN);
  vpu_mmio_.Write32(video_horizontal_active_end_px, VPU_ENCP_VIDEO_HAVON_END);
  vpu_mmio_.Write32(video_horizontal_sync_begin_px, VPU_ENCP_VIDEO_HSO_BEGIN);
  vpu_mmio_.Write32(video_horizontal_sync_end_px + 1, VPU_ENCP_VIDEO_HSO_END);

  const int video_vertical_sync_begin_line = 0;
  const int video_vertical_sync_end_line =
      video_vertical_sync_begin_line + timing.vertical_sync_width_lines - 1;
  const int video_vertical_active_begin_line =
      video_vertical_sync_end_line + 1 + timing.vertical_back_porch_lines;
  const int video_vertical_active_end_line =
      video_vertical_active_begin_line + timing.vertical_active_lines - 1;

  // Experiments on Khadas VIM3 (using Amlogic A311D) show that VAVON_*
  // registers must be set before VSO_* registers, otherwise the encoder
  // won't work.
  vpu_mmio_.Write32(video_vertical_active_begin_line, VPU_ENCP_VIDEO_VAVON_BLINE);
  vpu_mmio_.Write32(video_vertical_active_end_line, VPU_ENCP_VIDEO_VAVON_ELINE);
  vpu_mmio_.Write32(video_vertical_sync_begin_line, VPU_ENCP_VIDEO_VSO_BLINE);
  vpu_mmio_.Write32(video_vertical_sync_end_line + 1, VPU_ENCP_VIDEO_VSO_ELINE);
  vpu_mmio_.Write32(16, VPU_ENCP_VIDEO_VSO_BEGIN);
  vpu_mmio_.Write32(32, VPU_ENCP_VIDEO_VSO_END);

  // The latency between HDMI timing signals (DE, VSYNC, HSYNC) and the video
  // signal (from VFIFO).
  static constexpr int hdmi_signal_horizontal_offset = 2;

  const int hdmi_horizontal_sync_begin_px =
      (video_horizontal_sync_begin_px + hdmi_signal_horizontal_offset) % video_horizontal_period_px;
  const int hdmi_horizontal_sync_end_px =
      (video_horizontal_sync_end_px + hdmi_signal_horizontal_offset) % video_horizontal_period_px;
  const int hdmi_horizontal_active_begin_px =
      (video_horizontal_active_begin_px + hdmi_signal_horizontal_offset) %
      video_horizontal_period_px;
  const int hdmi_horizontal_active_end_px =
      (video_horizontal_active_end_px + hdmi_signal_horizontal_offset) % video_horizontal_period_px;

  vpu_mmio_.Write32(hdmi_horizontal_active_begin_px, VPU_ENCP_DE_H_BEGIN);
  vpu_mmio_.Write32(hdmi_horizontal_active_end_px + 1, VPU_ENCP_DE_H_END);
  vpu_mmio_.Write32(hdmi_horizontal_sync_begin_px, VPU_ENCP_DVI_HSO_BEGIN);
  vpu_mmio_.Write32(hdmi_horizontal_sync_end_px + 1, VPU_ENCP_DVI_HSO_END);

  const int hdmi_vertical_sync_begin_line = video_vertical_sync_begin_line;
  const int hdmi_vertical_sync_end_line = video_vertical_sync_end_line;
  const int hdmi_vertical_active_begin_line = video_vertical_active_begin_line;
  const int hdmi_vertical_active_end_line = video_vertical_active_end_line;

  vpu_mmio_.Write32(hdmi_vertical_active_begin_line, VPU_ENCP_DE_V_BEGIN_EVEN);
  vpu_mmio_.Write32(hdmi_vertical_active_end_line + 1, VPU_ENCP_DE_V_END_EVEN);
  vpu_mmio_.Write32(hdmi_vertical_sync_begin_line, VPU_ENCP_DVI_VSO_BLINE_EVN);
  vpu_mmio_.Write32(hdmi_vertical_sync_end_line + 1, VPU_ENCP_DVI_VSO_ELINE_EVN);
  vpu_mmio_.Write32(hdmi_horizontal_sync_begin_px, VPU_ENCP_DVI_VSO_BEGIN_EVN);
  vpu_mmio_.Write32(hdmi_horizontal_sync_begin_px, VPU_ENCP_DVI_VSO_END_EVN);

  // hsync, vsync active high. output CbYCr (GRB)
  // TODO: output desired format is hardcoded here to CbYCr (GRB)
  uint32_t vpu_hdmi_setting = 0b100 << 5;
  if (timing.hsync_polarity == display::SyncPolarity::kPositive) {
    vpu_hdmi_setting |= (1 << 2);
  }
  if (timing.vsync_polarity == display::SyncPolarity::kPositive) {
    vpu_hdmi_setting |= (1 << 3);
  }
  vpu_mmio_.Write32(vpu_hdmi_setting, VPU_HDMI_SETTING);

  // Select ENCP data to HDMI
  VpuHdmiSettingReg::Get().ReadFrom(&vpu_mmio_).set_src_sel(2).WriteTo(&vpu_mmio_);

  zxlogf(INFO, "done");
}

void HdmiHost::ConfigPhy() {
  HhiHdmiPhyCntl0Reg::Get().FromValue(0).WriteTo(&hhi_mmio_);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(0)
      .set_hdmi_fifo_enable(0)
      .set_hdmi_fifo_wr_enable(0)
      .set_msb_lsb_swap(0)
      .set_bit_invert(0)
      .set_ch0_swap(0)
      .set_ch1_swap(1)
      .set_ch2_swap(2)
      .set_ch3_swap(3)
      .set_new_prbs_en(0)
      .set_new_prbs_sel(0)
      .set_new_prbs_prbsmode(0)
      .set_new_prbs_mode(0)
      .WriteTo(&hhi_mmio_);

  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_phy_soft_reset(1)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&hhi_mmio_);
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&hhi_mmio_);
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_phy_soft_reset(1)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&hhi_mmio_);
  usleep(2);
  HhiHdmiPhyCntl1Reg::Get()
      .ReadFrom(&hhi_mmio_)
      .set_hdmi_tx_phy_soft_reset(0)
      .set_hdmi_tx_phy_clk_en(1)
      .set_hdmi_fifo_enable(1)
      .set_hdmi_fifo_wr_enable(1)
      .WriteTo(&hhi_mmio_);
  usleep(2);

  // The following configuration for HDMI PHY control register 0, 3 and 5 only
  // works for display modes where the display resolution is lower than
  // 3840 x 2160. The configuration currently works for all display modes
  // supported by this driver.
  //
  // TODO(https://fxbug.dev/42075808): Set the PHY control registers properly if the
  // display uses a 4k resolution (3840 x 2160 or higher).
  HhiHdmiPhyCntl0Reg::Get().FromValue(0).set_hdmi_ctl1(0x33eb).set_hdmi_ctl2(0x4242).WriteTo(
      &hhi_mmio_);
  HhiHdmiPhyCntl3Reg::Get().FromValue(0x2ab0ff3b).WriteTo(&hhi_mmio_);
  HhiHdmiPhyCntl5Reg::Get().FromValue(0x00000003).WriteTo(&hhi_mmio_);

  usleep(20);
  zxlogf(INFO, "done!");
}

}  // namespace amlogic_display
