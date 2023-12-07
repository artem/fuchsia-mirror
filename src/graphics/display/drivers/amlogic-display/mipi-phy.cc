// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/mipi-phy.h"

#include <lib/ddk/debug.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

namespace amlogic_display {

#define READ32_DSI_PHY_REG(a) dsi_phy_mmio_->Read32(a)
#define WRITE32_DSI_PHY_REG(a, v) dsi_phy_mmio_->Write32(v, a)

template <typename T>
constexpr inline uint8_t NsToLaneByte(T x, uint32_t lanebytetime) {
  return (static_cast<uint8_t>((x + lanebytetime - 1) / lanebytetime) & 0xFF);
}

constexpr uint32_t kUnit = (1 * 1000 * 1000 * 100);

zx::result<> MipiPhy::PhyCfgLoad(uint32_t bitrate) {
  // According to MIPI -PHY Spec, we need to define Unit Interval (UI).
  // This UI is defined as the time it takes to send a bit (i.e. bitrate)
  // The x100 is to ensure the ui is not rounded too much (i.e. 2.56 --> 256)
  // However, since we have introduced x100, we need to make sure we include x100
  // to all the PHY timings that are in ns units.
  const uint32_t ui = kUnit / (bitrate / 1000);

  // Calculate values will be rounded by the lanebyteclk
  const uint32_t lanebytetime = ui * 8;

  // lp_tesc:TX Escape Clock Division factor (from linebyteclk). Round up to units of ui
  dsi_phy_cfg_.lp_tesc = NsToLaneByte(DPHY_TIME_LP_TESC, lanebytetime);

  // lp_lpx: Transmit length of any LP state period
  dsi_phy_cfg_.lp_lpx = NsToLaneByte(DPHY_TIME_LP_LPX, lanebytetime);

  // lp_ta_sure
  dsi_phy_cfg_.lp_ta_sure = NsToLaneByte(DPHY_TIME_LP_TA_SURE, lanebytetime);

  // lp_ta_go
  dsi_phy_cfg_.lp_ta_go = NsToLaneByte(DPHY_TIME_LP_TA_GO, lanebytetime);

  // lp_ta_get
  dsi_phy_cfg_.lp_ta_get = NsToLaneByte(DPHY_TIME_LP_TA_GET, lanebytetime);

  // hs_exit
  dsi_phy_cfg_.hs_exit = NsToLaneByte(DPHY_TIME_HS_EXIT, lanebytetime);

  // clk-_prepare
  dsi_phy_cfg_.clk_prepare = NsToLaneByte(DPHY_TIME_CLK_PREPARE, lanebytetime);

  // clk_zero
  dsi_phy_cfg_.clk_zero = NsToLaneByte(DPHY_TIME_CLK_ZERO(ui), lanebytetime);

  // clk_pre
  dsi_phy_cfg_.clk_pre = NsToLaneByte(DPHY_TIME_CLK_PRE(ui), lanebytetime);

  // init
  dsi_phy_cfg_.init = NsToLaneByte(DPHY_TIME_INIT, lanebytetime);

  // wakeup
  dsi_phy_cfg_.wakeup = NsToLaneByte(DPHY_TIME_WAKEUP, lanebytetime);

  // clk_trail
  dsi_phy_cfg_.clk_trail = NsToLaneByte(DPHY_TIME_CLK_TRAIL, lanebytetime);

  // clk_post
  dsi_phy_cfg_.clk_post = NsToLaneByte(DPHY_TIME_CLK_POST(ui), lanebytetime);

  // hs_trail
  dsi_phy_cfg_.hs_trail = NsToLaneByte(DPHY_TIME_HS_TRAIL(ui), lanebytetime);

  // hs_prepare
  dsi_phy_cfg_.hs_prepare = NsToLaneByte(DPHY_TIME_HS_PREPARE(ui), lanebytetime);

  // hs_zero
  dsi_phy_cfg_.hs_zero = NsToLaneByte(DPHY_TIME_HS_ZERO(ui), lanebytetime);

  // Ensure both clk-trail and hs-trail do not exceed Teot (End of Transmission Time)
  const uint32_t time_req_max = NsToLaneByte(DPHY_TIME_EOT(ui), lanebytetime);
  if ((dsi_phy_cfg_.clk_trail > time_req_max) || (dsi_phy_cfg_.hs_trail > time_req_max)) {
    zxlogf(ERROR, "Invalid clk-trail and/or hs-trail exceed Teot!");
    zxlogf(ERROR, "clk-trail = 0x%02x, hs-trail =  0x%02x, Teot = 0x%02x", dsi_phy_cfg_.clk_trail,
           dsi_phy_cfg_.hs_trail, time_req_max);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  zxlogf(TRACE,
         "lp_tesc     = 0x%02x"
         "lp_lpx      = 0x%02x"
         "lp_ta_sure  = 0x%02x"
         "lp_ta_go    = 0x%02x"
         "lp_ta_get   = 0x%02x"
         "hs_exit     = 0x%02x"
         "hs_trail    = 0x%02x"
         "hs_zero     = 0x%02x"
         "hs_prepare  = 0x%02x"
         "clk_trail   = 0x%02x"
         "clk_post    = 0x%02x"
         "clk_zero    = 0x%02x"
         "clk_prepare = 0x%02x"
         "clk_pre     = 0x%02x"
         "init        = 0x%02x"
         "wakeup      = 0x%02x",
         dsi_phy_cfg_.lp_tesc, dsi_phy_cfg_.lp_lpx, dsi_phy_cfg_.lp_ta_sure, dsi_phy_cfg_.lp_ta_go,
         dsi_phy_cfg_.lp_ta_get, dsi_phy_cfg_.hs_exit, dsi_phy_cfg_.hs_trail, dsi_phy_cfg_.hs_zero,
         dsi_phy_cfg_.hs_prepare, dsi_phy_cfg_.clk_trail, dsi_phy_cfg_.clk_post,
         dsi_phy_cfg_.clk_zero, dsi_phy_cfg_.clk_prepare, dsi_phy_cfg_.clk_pre, dsi_phy_cfg_.init,
         dsi_phy_cfg_.wakeup);
  return zx::ok();
}

void MipiPhy::PhyInit() {
  // Enable phy clock.
  dsi_phy_mmio_->Write32(PHY_CTRL_TXDDRCLK_EN | PHY_CTRL_DDRCLKPATH_EN | PHY_CTRL_CLK_DIV_COUNTER |
                             PHY_CTRL_CLK_DIV_EN | PHY_CTRL_BYTECLK_EN,
                         MIPI_DSI_PHY_CTRL);

  // Toggle PHY CTRL RST
  dsi_phy_mmio_->Write32(SetFieldValue32(dsi_phy_mmio_->Read32(MIPI_DSI_PHY_CTRL),
                                         /*field_begin_bit=*/PHY_CTRL_RST_START,
                                         /*field_size_bits=*/PHY_CTRL_RST_BITS, /*field_value=*/1),
                         MIPI_DSI_PHY_CTRL);
  dsi_phy_mmio_->Write32(SetFieldValue32(dsi_phy_mmio_->Read32(MIPI_DSI_PHY_CTRL),
                                         /*field_begin_bit=*/PHY_CTRL_RST_START,
                                         /*field_size_bits=*/PHY_CTRL_RST_BITS, /*field_value=*/0),
                         MIPI_DSI_PHY_CTRL);

  dsi_phy_mmio_->Write32((dsi_phy_cfg_.clk_trail | (dsi_phy_cfg_.clk_post << 8) |
                          (dsi_phy_cfg_.clk_zero << 16) | (dsi_phy_cfg_.clk_prepare << 24)),
                         MIPI_DSI_CLK_TIM);

  dsi_phy_mmio_->Write32(dsi_phy_cfg_.clk_pre, MIPI_DSI_CLK_TIM1);

  dsi_phy_mmio_->Write32((dsi_phy_cfg_.hs_exit | (dsi_phy_cfg_.hs_trail << 8) |
                          (dsi_phy_cfg_.hs_zero << 16) | (dsi_phy_cfg_.hs_prepare << 24)),
                         MIPI_DSI_HS_TIM);

  dsi_phy_mmio_->Write32((dsi_phy_cfg_.lp_lpx | (dsi_phy_cfg_.lp_ta_sure << 8) |
                          (dsi_phy_cfg_.lp_ta_go << 16) | (dsi_phy_cfg_.lp_ta_get << 24)),
                         MIPI_DSI_LP_TIM);

  dsi_phy_mmio_->Write32(ANA_UP_TIME, MIPI_DSI_ANA_UP_TIM);
  dsi_phy_mmio_->Write32(dsi_phy_cfg_.init, MIPI_DSI_INIT_TIM);
  dsi_phy_mmio_->Write32(dsi_phy_cfg_.wakeup, MIPI_DSI_WAKEUP_TIM);
  dsi_phy_mmio_->Write32(LPOK_TIME, MIPI_DSI_LPOK_TIM);
  dsi_phy_mmio_->Write32(ULPS_CHECK_TIME, MIPI_DSI_ULPS_CHECK);
  dsi_phy_mmio_->Write32(LP_WCHDOG_TIME, MIPI_DSI_LP_WCHDOG);
  dsi_phy_mmio_->Write32(TURN_WCHDOG_TIME, MIPI_DSI_TURN_WCHDOG);

  dsi_phy_mmio_->Write32(0, MIPI_DSI_CHAN_CTRL);
}

void MipiPhy::Shutdown() {
  if (!phy_enabled_) {
    return;
  }

  // Power down DSI
  dsiimpl_.PowerDown();
  dsi_phy_mmio_->Write32(0x1f, MIPI_DSI_CHAN_CTRL);
  dsi_phy_mmio_->Write32(
      SetFieldValue32(dsi_phy_mmio_->Read32(MIPI_DSI_PHY_CTRL), /*field_begin_bit=*/7,
                      /*field_size_bits=*/1, /*field_value=*/0),
      MIPI_DSI_PHY_CTRL);
  phy_enabled_ = false;
}

zx::result<> MipiPhy::Startup() {
  if (phy_enabled_) {
    return zx::ok();
  }

  // Power up DSI
  dsiimpl_.PowerUp();

  // Setup Parameters of DPHY
  // Below we are sending test code 0x44 with parameter 0x74. This means
  // we are setting up the phy to operate in 1050-1099 Mbps mode
  // TODO(payamm): Find out why 0x74 was selected
  dsiimpl_.PhySendCode(0x00010044, 0x00000074);

  // Power up D-PHY
  dsiimpl_.PhyPowerUp();

  // Setup PHY Timing parameters
  PhyInit();

  // Wait for PHY to be read
  zx_status_t status;
  if ((status = dsiimpl_.PhyWaitForReady()) != ZX_OK) {
    // no need to print additional info.
    return zx::error(status);
  }

  // Trigger a sync active for esc_clk
  dsi_phy_mmio_->Write32(
      SetFieldValue32(dsi_phy_mmio_->Read32(MIPI_DSI_PHY_CTRL), /*field_begin_bit=*/1,
                      /*field_size_bits=*/1, /*field_value=*/1),
      MIPI_DSI_PHY_CTRL);

  phy_enabled_ = true;
  return zx::ok();
}

zx::result<std::unique_ptr<MipiPhy>> MipiPhy::Create(ddk::PDevFidl& pdev,
                                                     ddk::DsiImplProtocolClient dsi,
                                                     bool already_enabled) {
  fbl::AllocChecker ac;
  std::unique_ptr<MipiPhy> self(new (&ac) MipiPhy);
  if (!ac.check()) {
    zxlogf(ERROR, "MipiPhy: Could not allocate memory");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  self->dsiimpl_ = dsi;
  self->phy_enabled_ = already_enabled;

  // Map Mipi Dsi and Dsi Phy registers
  zx_status_t status = pdev.MapMmio(MMIO_DSI_PHY, &self->dsi_phy_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "MipiPhy: Could not map DSI PHY mmio");
    return zx::error(status);
  }

  return zx::ok(std::move(self));
}

void MipiPhy::Dump() {
  zxlogf(INFO, "%s: DUMPING PHY REGS", __func__);
  zxlogf(INFO, "MIPI_DSI_PHY_CTRL = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_PHY_CTRL));
  zxlogf(INFO, "MIPI_DSI_CHAN_CTRL = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_CHAN_CTRL));
  zxlogf(INFO, "MIPI_DSI_CHAN_STS = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_CHAN_STS));
  zxlogf(INFO, "MIPI_DSI_CLK_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_CLK_TIM));
  zxlogf(INFO, "MIPI_DSI_HS_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_HS_TIM));
  zxlogf(INFO, "MIPI_DSI_LP_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_LP_TIM));
  zxlogf(INFO, "MIPI_DSI_ANA_UP_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_ANA_UP_TIM));
  zxlogf(INFO, "MIPI_DSI_INIT_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_INIT_TIM));
  zxlogf(INFO, "MIPI_DSI_WAKEUP_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_WAKEUP_TIM));
  zxlogf(INFO, "MIPI_DSI_LPOK_TIM = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_LPOK_TIM));
  zxlogf(INFO, "MIPI_DSI_LP_WCHDOG = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_LP_WCHDOG));
  zxlogf(INFO, "MIPI_DSI_ANA_CTRL = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_ANA_CTRL));
  zxlogf(INFO, "MIPI_DSI_CLK_TIM1 = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_CLK_TIM1));
  zxlogf(INFO, "MIPI_DSI_TURN_WCHDOG = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_TURN_WCHDOG));
  zxlogf(INFO, "MIPI_DSI_ULPS_CHECK = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_ULPS_CHECK));
  zxlogf(INFO, "MIPI_DSI_TEST_CTRL0 = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_TEST_CTRL0));
  zxlogf(INFO, "MIPI_DSI_TEST_CTRL1 = 0x%x", dsi_phy_mmio_->Read32(MIPI_DSI_TEST_CTRL1));
  zxlogf(INFO, "");

  zxlogf(INFO, "#############################");
  zxlogf(INFO, "Dumping dsi_phy_cfg structure:");
  zxlogf(INFO, "#############################");
  zxlogf(INFO, "lp_tesc = 0x%x (%u)", dsi_phy_cfg_.lp_tesc, dsi_phy_cfg_.lp_tesc);
  zxlogf(INFO, "lp_lpx = 0x%x (%u)", dsi_phy_cfg_.lp_lpx, dsi_phy_cfg_.lp_lpx);
  zxlogf(INFO, "lp_ta_sure = 0x%x (%u)", dsi_phy_cfg_.lp_ta_sure, dsi_phy_cfg_.lp_ta_sure);
  zxlogf(INFO, "lp_ta_go = 0x%x (%u)", dsi_phy_cfg_.lp_ta_go, dsi_phy_cfg_.lp_ta_go);
  zxlogf(INFO, "lp_ta_get = 0x%x (%u)", dsi_phy_cfg_.lp_ta_get, dsi_phy_cfg_.lp_ta_get);
  zxlogf(INFO, "hs_exit = 0x%x (%u)", dsi_phy_cfg_.hs_exit, dsi_phy_cfg_.hs_exit);
  zxlogf(INFO, "hs_trail = 0x%x (%u)", dsi_phy_cfg_.hs_trail, dsi_phy_cfg_.hs_trail);
  zxlogf(INFO, "hs_zero = 0x%x (%u)", dsi_phy_cfg_.hs_zero, dsi_phy_cfg_.hs_zero);
  zxlogf(INFO, "hs_prepare = 0x%x (%u)", dsi_phy_cfg_.hs_prepare, dsi_phy_cfg_.hs_prepare);
  zxlogf(INFO, "clk_trail = 0x%x (%u)", dsi_phy_cfg_.clk_trail, dsi_phy_cfg_.clk_trail);
  zxlogf(INFO, "clk_post = 0x%x (%u)", dsi_phy_cfg_.clk_post, dsi_phy_cfg_.clk_post);
  zxlogf(INFO, "clk_zero = 0x%x (%u)", dsi_phy_cfg_.clk_zero, dsi_phy_cfg_.clk_zero);
  zxlogf(INFO, "clk_prepare = 0x%x (%u)", dsi_phy_cfg_.clk_prepare, dsi_phy_cfg_.clk_prepare);
  zxlogf(INFO, "clk_pre = 0x%x (%u)", dsi_phy_cfg_.clk_pre, dsi_phy_cfg_.clk_pre);
  zxlogf(INFO, "init = 0x%x (%u)", dsi_phy_cfg_.init, dsi_phy_cfg_.init);
  zxlogf(INFO, "wakeup = 0x%x (%u)", dsi_phy_cfg_.wakeup, dsi_phy_cfg_.wakeup);
}

}  // namespace amlogic_display
