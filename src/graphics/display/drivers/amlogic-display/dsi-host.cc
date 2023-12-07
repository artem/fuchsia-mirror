// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/dsi-host.h"

#include <lib/ddk/debug.h>
#include <lib/device-protocol/display-panel.h>
#include <zircon/status.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/initcodes-inl.h"

namespace amlogic_display {

#define READ32_MIPI_DSI_REG(a) mipi_dsi_mmio_->Read32(a)
#define WRITE32_MIPI_DSI_REG(a, v) mipi_dsi_mmio_->Write32(v, a)

#define READ32_HHI_REG(a) hhi_mmio_->Read32(a)
#define WRITE32_HHI_REG(a, v) hhi_mmio_->Write32(v, a)

namespace {

constexpr uint8_t kEmptySequence[] = {};
constexpr cpp20::span<const uint8_t> kEmptySequenceSpan = {kEmptySequence, 0};

// Convenience function for building PanelConfigs. Most op sequences are shared
// between panel types.
constexpr PanelConfig MakeConfigForAstroSherlockNelson(const char* name,
                                                       cpp20::span<const uint8_t> init_seq) {
  return {.name = name,
          .dsi_on = init_seq,
          .dsi_off = {lcd_shutdown_sequence, std::size(lcd_shutdown_sequence)},
          .power_on = {kLcdPowerOnSequenceForAstroSherlockNelson,
                       std::size(kLcdPowerOnSequenceForAstroSherlockNelson)},
          .power_off = {kLcdPowerOffSequenceForAstroSherlockNelson,
                        std::size(kLcdPowerOffSequenceForAstroSherlockNelson)}};
}

constexpr PanelConfig kVim3Ts050PanelConfig = {
    .name = "MTF050FHDI_03",
    .dsi_on = {lcd_init_sequence_MTF050FHDI_03, std::size(lcd_init_sequence_MTF050FHDI_03)},
    .dsi_off = {lcd_shutdown_sequence, std::size(lcd_shutdown_sequence)},
    .power_on = {kLcdPowerOnSequenceForVim3Ts050, std::size(kLcdPowerOnSequenceForVim3Ts050)},
    .power_off = {kLcdPowerOffSequenceForVim3Ts050, std::size(kLcdPowerOffSequenceForVim3Ts050)},
};

// LINT.IfChange
/// Panel type IDs are compact. This array should be updated when
/// <lib/device-protocol/display-panel.h> is.
constexpr PanelConfig kPanelConfig[] = {
    MakeConfigForAstroSherlockNelson(
        "TV070WSM_FT", {lcd_init_sequence_TV070WSM_FT, std::size(lcd_init_sequence_TV070WSM_FT)}),
    MakeConfigForAstroSherlockNelson(
        "P070ACB_FT", {lcd_init_sequence_P070ACB_FT, std::size(lcd_init_sequence_P070ACB_FT)}),
    MakeConfigForAstroSherlockNelson(
        "TV101WXM_FT", {lcd_init_sequence_TV101WXM_FT, std::size(lcd_init_sequence_TV101WXM_FT)}),
    MakeConfigForAstroSherlockNelson(
        "G101B158_FT", {lcd_init_sequence_G101B158_FT, std::size(lcd_init_sequence_G101B158_FT)}),
    // ILI9881C & ST7701S are not supported
    MakeConfigForAstroSherlockNelson("ILI9881C", kEmptySequenceSpan),
    MakeConfigForAstroSherlockNelson("ST7701S", kEmptySequenceSpan),
    MakeConfigForAstroSherlockNelson(
        "TV080WXM_FT", {lcd_init_sequence_TV080WXM_FT, std::size(lcd_init_sequence_TV080WXM_FT)}),
    MakeConfigForAstroSherlockNelson(
        "TV101WXM_FT_9365",
        {lcd_init_sequence_TV101WXM_FT_9365, std::size(lcd_init_sequence_TV101WXM_FT_9365)}),
    MakeConfigForAstroSherlockNelson(
        "TV070WSM_FT_9365",
        {lcd_init_sequence_TV070WSM_FT_9365, std::size(lcd_init_sequence_TV070WSM_FT_9365)}),
    MakeConfigForAstroSherlockNelson(
        "KD070D82_FT", {lcd_init_sequence_KD070D82_FT, std::size(lcd_init_sequence_KD070D82_FT)}),
    MakeConfigForAstroSherlockNelson(
        "KD070D82_FT_9365",
        {lcd_init_sequence_KD070D82_FT_9365, std::size(lcd_init_sequence_KD070D82_FT_9365)}),
    MakeConfigForAstroSherlockNelson(
        "TV070WSM_ST7703I",
        {lcd_init_sequence_TV070WSM_ST7703I, std::size(lcd_init_sequence_TV070WSM_ST7703I)}),
    kVim3Ts050PanelConfig,
};
// LINT.ThenChange(//src/graphics/display/lib/device-protocol-display/include/lib/device-protocol/display-panel.h)

const PanelConfig* GetPanelConfig(uint32_t panel_type) {
  ZX_DEBUG_ASSERT(panel_type <= PANEL_MTF050FHDI_03);
  ZX_DEBUG_ASSERT(panel_type != PANEL_ILI9881C);
  ZX_DEBUG_ASSERT(panel_type != PANEL_ST7701S);
  if (panel_type == PANEL_ILI9881C || panel_type == PANEL_ST7701S) {
    return nullptr;
  }
  return &(kPanelConfig[panel_type]);
}

}  // namespace

DsiHost::DsiHost(zx_device_t* parent, uint32_t panel_type,
                 fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> lcd_gpio)
    : pdev_(ddk::PDevFidl::FromFragment(parent)),
      dsiimpl_(parent, "dsi"),
      lcd_gpio_(std::move(lcd_gpio)),
      panel_type_(panel_type) {}

// static
zx::result<std::unique_ptr<DsiHost>> DsiHost::Create(zx_device_t* parent, uint32_t panel_type) {
  fbl::AllocChecker ac;
  const char* kLcdGpioFragmentName = "gpio-lcd-reset";
  zx::result lcd_gpio =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
          parent, kLcdGpioFragmentName);
  if (lcd_gpio.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment: %s", kLcdGpioFragmentName);
    return lcd_gpio.take_error();
  }
  std::unique_ptr<DsiHost> self = fbl::make_unique_checked<DsiHost>(
      &ac, DsiHost(parent, panel_type, std::move(lcd_gpio.value())));
  if (!ac.check()) {
    zxlogf(ERROR, "No memory to allocate a DSI host");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  self->panel_config_ = GetPanelConfig(panel_type);
  if (self->panel_config_ == nullptr) {
    zxlogf(ERROR, "Unrecognized panel type %d", panel_type);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (!self->pdev_.is_valid()) {
    zxlogf(ERROR, "DsiHost: Could not get ZX_PROTOCOL_PDEV protocol");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Map MIPI DSI and HHI registers
  zx_status_t status = self->pdev_.MapMmio(MMIO_MPI_DSI, &(self->mipi_dsi_mmio_));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map MIPI DSI mmio: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = self->pdev_.MapMmio(MMIO_HHI, &(self->hhi_mmio_));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map HHI mmio: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // panel_type_ is now canonical.
  lcd_gpio =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
          parent, kLcdGpioFragmentName);
  if (lcd_gpio.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment: %s", kLcdGpioFragmentName);
    return lcd_gpio.take_error();
  }
  zx::result<std::unique_ptr<Lcd>> lcd_or_status =
      Lcd::Create(self->panel_type_, self->panel_config_->dsi_on, self->panel_config_->dsi_off,
                  fit::bind_member(self.get(), &DsiHost::SetSignalPower), self->dsiimpl_,
                  std::move(lcd_gpio.value()), kBootloaderDisplayEnabled);
  if (lcd_or_status.is_error()) {
    zxlogf(ERROR, "Failed to create LCD object: %s", lcd_or_status.status_string());
    return zx::error(lcd_or_status.error_value());
  }
  self->lcd_ = std::move(lcd_or_status).value();

  auto phy_or_status = MipiPhy::Create(self->pdev_, self->dsiimpl_, kBootloaderDisplayEnabled);
  if (phy_or_status.is_error()) {
    zxlogf(ERROR, "Failed to create PHY object: %s", phy_or_status.status_string());
    return zx::error(phy_or_status.error_value());
  }
  self->phy_ = std::move(phy_or_status.value());

  self->enabled_ = kBootloaderDisplayEnabled;

  return zx::ok(std::move(self));
}

zx::result<> DsiHost::PerformPowerOpSequence(cpp20::span<const PowerOp> commands,
                                             fit::callback<zx::result<>()> power_on) {
  if (commands.size() == 0) {
    zxlogf(ERROR, "No power commands to execute");
    return zx::ok();
  }
  uint8_t wait_count = 0;

  for (const auto op : commands) {
    zxlogf(TRACE, "power_op %d index=%d value=%d sleep_ms=%d", op.op, op.index, op.value,
           op.sleep_ms);
    switch (op.op) {
      case kPowerOpExit:
        zxlogf(TRACE, "power_exit");
        return zx::ok();
      case kPowerOpGpio: {
        zxlogf(TRACE, "power_set_gpio pin #%d value=%d", op.index, op.value);
        if (op.index != 0) {
          zxlogf(ERROR, "Unrecognized GPIO pin #%d, ignoring", op.index);
          break;
        }
        fidl::WireResult result = lcd_gpio_->Write(op.value);
        if (!result.ok()) {
          zxlogf(ERROR, "Failed to send Write request to lcd gpio: %s", result.status_string());
          return zx::error(result.status());
        }
        if (result->is_error()) {
          zxlogf(ERROR, "Failed to write to lcd gpio: %s",
                 zx_status_get_string(result->error_value()));
          return result->take_error();
        }
        break;
      }
      case kPowerOpSignal: {
        zxlogf(TRACE, "power_signal dsi_init");
        zx::result<> power_on_result = power_on();
        if (!power_on_result.is_ok()) {
          zxlogf(ERROR, "Failed to power on MIPI DSI display: %s", power_on_result.status_string());
          return power_on_result.take_error();
        }
        break;
      }
      case kPowerOpAwaitGpio:
        zxlogf(TRACE, "power_await_gpio pin #%d value=%d timeout=%d msec", op.index, op.value,
               op.sleep_ms);
        if (op.index != 0) {
          zxlogf(ERROR, "Unrecognized GPIO pin #%d, ignoring", op.index);
          break;
        }
        {
          fidl::WireResult result =
              lcd_gpio_->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown);
          if (!result.ok()) {
            zxlogf(ERROR, "Failed to send ConfigIn request to lcd gpio: %s",
                   result.status_string());
            return zx::error(result.status());
          }

          auto& response = result.value();
          if (response.is_error()) {
            zxlogf(ERROR, "Failed to configure lcd gpio to input: %s",
                   zx_status_get_string(response.error_value()));
            return response.take_error();
          }
        }
        for (wait_count = 0; wait_count < op.sleep_ms; wait_count++) {
          fidl::WireResult read_result = lcd_gpio_->Read();
          if (!read_result.ok()) {
            zxlogf(ERROR, "Failed to send Read request to lcd gpio: %s",
                   read_result.status_string());
            return zx::error(read_result.status());
          }

          auto& read_response = read_result.value();
          if (read_response.is_error()) {
            zxlogf(ERROR, "Failed to read lcd gpio: %s",
                   zx_status_get_string(read_response.error_value()));
            return read_response.take_error();
          }
          if (read_result.value()->value == op.value) {
            break;
          }
          zx::nanosleep(zx::deadline_after(zx::msec(1)));
        }
        if (wait_count == op.sleep_ms) {
          zxlogf(ERROR, "Timed out waiting for GPIO value=%d", op.value);
        }
        break;
      default:
        zxlogf(ERROR, "Unrecognized power op %d", op.op);
        break;
    }
    if (op.op != kPowerOpAwaitGpio && op.sleep_ms != 0) {
      zxlogf(TRACE, "power_sleep %d msec", op.sleep_ms);
      zx::nanosleep(zx::deadline_after(zx::msec(op.sleep_ms)));
    }
  }
  return zx::ok();
}

zx::result<> DsiHost::ConfigureDsiHostController(const display_setting_t& disp_setting) {
  // Setup relevant TOP_CNTL register -- Undocumented --
  mipi_dsi_mmio_->Write32(
      SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), TOP_CNTL_DPI_CLR_MODE_START,
                      TOP_CNTL_DPI_CLR_MODE_BITS, SUPPORTED_DPI_FORMAT),
      MIPI_DSI_TOP_CNTL);
  mipi_dsi_mmio_->Write32(
      SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), TOP_CNTL_IN_CLR_MODE_START,
                      TOP_CNTL_IN_CLR_MODE_BITS, SUPPORTED_VENC_DATA_WIDTH),
      MIPI_DSI_TOP_CNTL);
  mipi_dsi_mmio_->Write32(
      SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), TOP_CNTL_CHROMA_SUBSAMPLE_START,
                      TOP_CNTL_CHROMA_SUBSAMPLE_BITS, 0),
      MIPI_DSI_TOP_CNTL);

  // setup dsi config
  dsi_config_t dsi_cfg;
  dsi_cfg.display_setting = disp_setting;
  dsi_cfg.video_mode_type = VIDEO_MODE_BURST;
  dsi_cfg.color_coding = COLOR_CODE_PACKED_24BIT_888;

  designware_config_t dw_cfg;
  dw_cfg.lp_escape_time = phy_->GetLowPowerEscaseTime();
  dw_cfg.lp_cmd_pkt_size = LPCMD_PKT_SIZE;
  dw_cfg.phy_timer_clkhs_to_lp = PHY_TMR_LPCLK_CLKHS_TO_LP;
  dw_cfg.phy_timer_clklp_to_hs = PHY_TMR_LPCLK_CLKLP_TO_HS;
  dw_cfg.phy_timer_hs_to_lp = PHY_TMR_HS_TO_LP;
  dw_cfg.phy_timer_lp_to_hs = PHY_TMR_LP_TO_HS;
  dw_cfg.auto_clklane = 1;
  dsi_cfg.vendor_config_buffer = reinterpret_cast<uint8_t*>(&dw_cfg);

  dsiimpl_.Config(&dsi_cfg);

  return zx::ok();
}

void DsiHost::PhyEnable() {
  hhi_mmio_->Write32(MIPI_CNTL0_CMN_REF_GEN_CTRL(0x29) | MIPI_CNTL0_VREF_SEL(VREF_SEL_VR) |
                         MIPI_CNTL0_LREF_SEL(LREF_SEL_L_ROUT) | MIPI_CNTL0_LBG_EN |
                         MIPI_CNTL0_VR_TRIM_CNTL(0x7) | MIPI_CNTL0_VR_GEN_FROM_LGB_EN,
                     HHI_MIPI_CNTL0);
  hhi_mmio_->Write32(MIPI_CNTL1_DSI_VBG_EN | MIPI_CNTL1_CTL, HHI_MIPI_CNTL1);
  hhi_mmio_->Write32(MIPI_CNTL2_DEFAULT_VAL, HHI_MIPI_CNTL2);  // 4 lane
}

void DsiHost::PhyDisable() {
  hhi_mmio_->Write32(0, HHI_MIPI_CNTL0);
  hhi_mmio_->Write32(0, HHI_MIPI_CNTL1);
  hhi_mmio_->Write32(0, HHI_MIPI_CNTL2);
}

void DsiHost::SetSignalPower(bool on) {
  // These bits latch after vsync.
  if (on) {
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), /*field_begin_bit=*/2,
                        /*field_size_bits=*/1, /*field_value=*/1),
        MIPI_DSI_TOP_CNTL);
    zx::nanosleep(zx::deadline_after(zx::msec(20)));
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), /*field_begin_bit=*/2,
                        /*field_size_bits=*/1, /*field_value=*/0),
        MIPI_DSI_TOP_CNTL);
    zx::nanosleep(zx::deadline_after(zx::msec(20)));
  } else {
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), /*field_begin_bit=*/2,
                        /*field_size_bits=*/1, /*field_value=*/0),
        MIPI_DSI_TOP_CNTL);
    zx::nanosleep(zx::deadline_after(zx::msec(20)));
  }
}

void DsiHost::Disable(const display_setting_t& disp_setting) {
  // turn host off only if it's been fully turned on
  if (!enabled_) {
    return;
  }

  // Place dsi in command mode first
  dsiimpl_.SetMode(DSI_MODE_COMMAND);
  fit::callback<zx::result<>()> power_off = [this]() -> zx::result<> {
    zx::result<> result = lcd_->Disable();
    if (!result.is_ok()) {
      return result.take_error();
    }
    PhyDisable();
    phy_->Shutdown();
    return zx::ok();
  };

  zx::result<> power_off_result =
      PerformPowerOpSequence(panel_config_->power_off, std::move(power_off));
  if (!power_off_result.is_ok()) {
    zxlogf(ERROR, "Failed to power off the DSI display: %s", power_off_result.status_string());
  }

  enabled_ = false;
}

zx::result<> DsiHost::Enable(const display_setting_t& disp_setting, uint32_t bitrate) {
  if (enabled_) {
    return zx::ok();
  }

  fit::callback<zx::result<>()> power_on = [&]() -> zx::result<> {
    // Enable MIPI PHY
    PhyEnable();

    // Load Phy configuration
    zx::result<> phy_config_load_result = phy_->PhyCfgLoad(bitrate);
    if (!phy_config_load_result.is_ok()) {
      zxlogf(ERROR, "Error during phy config calculations: %s",
             phy_config_load_result.status_string());
      return phy_config_load_result.take_error();
    }

    // Enable dwc mipi_dsi_host's clock
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL), /*field_begin_bit=*/4,
                        /*field_size_bits=*/2, /*field_value=*/0x3),
        MIPI_DSI_TOP_CNTL);
    // mipi_dsi_host's reset
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SW_RESET), /*field_begin_bit=*/0,
                        /*field_size_bits=*/4, /*field_value=*/0xf),
        MIPI_DSI_TOP_SW_RESET);
    // Release mipi_dsi_host's reset
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SW_RESET), /*field_begin_bit=*/0,
                        /*field_size_bits=*/4, /*field_value=*/0x0),
        MIPI_DSI_TOP_SW_RESET);
    // Enable dwc mipi_dsi_host's clock
    mipi_dsi_mmio_->Write32(
        SetFieldValue32(mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CLK_CNTL), /*field_begin_bit=*/0,
                        /*field_size_bits=*/2, /*field_value=*/0x3),
        MIPI_DSI_TOP_CLK_CNTL);

    mipi_dsi_mmio_->Write32(0, MIPI_DSI_TOP_MEM_PD);
    zx::nanosleep(zx::deadline_after(zx::msec(10)));

    // Initialize host in command mode first
    dsiimpl_.SetMode(DSI_MODE_COMMAND);
    zx::result<> dsi_host_config_result = ConfigureDsiHostController(disp_setting);
    if (!dsi_host_config_result.is_ok()) {
      zxlogf(ERROR, "Failed to configure the MIPI DSI Host Controller: %s",
             dsi_host_config_result.status_string());
      return dsi_host_config_result.take_error();
    }

    // Initialize mipi dsi D-phy
    zx::result<> phy_startup_result = phy_->Startup();
    if (!phy_startup_result.is_ok()) {
      zxlogf(ERROR, "Error during MIPI D-PHY Initialization: %s",
             phy_startup_result.status_string());
      return phy_startup_result.take_error();
    }

    // Load LCD Init values while in command mode
    zx::result<> lcd_enable_result = lcd_->Enable();
    if (!lcd_enable_result.is_ok()) {
      zxlogf(ERROR, "Failed to enable LCD: %s", lcd_enable_result.status_string());
    }

    // switch to video mode
    dsiimpl_.SetMode(DSI_MODE_VIDEO);
    return zx::ok();
  };

  zx::result<> power_on_result =
      PerformPowerOpSequence(panel_config_->power_on, std::move(power_on));
  if (!power_on_result.is_ok()) {
    zxlogf(ERROR, "Failed to power on the DSI Display: %s", power_on_result.status_string());
    return power_on_result.take_error();
  }
  // Host is On and Active at this point
  enabled_ = true;
  return zx::ok();
}

void DsiHost::Dump() {
  zxlogf(INFO, "MIPI_DSI_TOP_SW_RESET = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SW_RESET));
  zxlogf(INFO, "MIPI_DSI_TOP_CLK_CNTL = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CLK_CNTL));
  zxlogf(INFO, "MIPI_DSI_TOP_CNTL = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_CNTL));
  zxlogf(INFO, "MIPI_DSI_TOP_SUSPEND_CNTL = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SUSPEND_CNTL));
  zxlogf(INFO, "MIPI_DSI_TOP_SUSPEND_LINE = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SUSPEND_LINE));
  zxlogf(INFO, "MIPI_DSI_TOP_SUSPEND_PIX = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_SUSPEND_PIX));
  zxlogf(INFO, "MIPI_DSI_TOP_MEAS_CNTL = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEAS_CNTL));
  zxlogf(INFO, "MIPI_DSI_TOP_STAT = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_STAT));
  zxlogf(INFO, "MIPI_DSI_TOP_MEAS_STAT_TE0 = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEAS_STAT_TE0));
  zxlogf(INFO, "MIPI_DSI_TOP_MEAS_STAT_TE1 = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEAS_STAT_TE1));
  zxlogf(INFO, "MIPI_DSI_TOP_MEAS_STAT_VS0 = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEAS_STAT_VS0));
  zxlogf(INFO, "MIPI_DSI_TOP_MEAS_STAT_VS1 = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEAS_STAT_VS1));
  zxlogf(INFO, "MIPI_DSI_TOP_INTR_CNTL_STAT = 0x%x",
         mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_INTR_CNTL_STAT));
  zxlogf(INFO, "MIPI_DSI_TOP_MEM_PD = 0x%x", mipi_dsi_mmio_->Read32(MIPI_DSI_TOP_MEM_PD));
}

}  // namespace amlogic_display
