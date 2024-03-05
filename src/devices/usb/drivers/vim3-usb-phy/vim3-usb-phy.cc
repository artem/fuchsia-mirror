// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/vim3-usb-phy/vim3-usb-phy.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>

#include <soc/aml-common/aml-registers.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy-regs.h"

namespace vim3_usb_phy {

zx_status_t Vim3UsbPhy::InitPhy2() {
  auto* usbctrl_mmio = &usbctrl_mmio_;

  // first reset USB
  auto portnum = usbphy2_.size();
  uint32_t reset_level = 0;
  while (portnum) {
    portnum--;
    reset_level = reset_level | (1 << (16 + portnum));
  }

  auto level_result =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, reset_level);
  if ((level_result.status() != ZX_OK) || level_result->is_error()) {
    FDF_LOG(ERROR, "Reset Level Write failed\n");
    return ZX_ERR_INTERNAL;
  }

  // amlogic_new_usbphy_reset_v2()
  auto register_result1 = reset_register_->WriteRegister32(
      RESET1_REGISTER_OFFSET, aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
      aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
  if ((register_result1.status() != ZX_OK) || register_result1->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  // amlogic_new_usb2_init()
  for (auto& phy : usbphy2_) {
    auto u2p_ro_v2 = U2P_R0_V2::Get(phy.idx()).ReadFrom(usbctrl_mmio).set_por(1);
    if (phy.is_otg_capable()) {
      u2p_ro_v2.set_idpullup0(1)
          .set_drvvbus0(1)
          .set_host_device(phy.dr_mode() == USB_MODE_PERIPHERAL ? 0 : 1)
          .WriteTo(usbctrl_mmio);
    } else {
      u2p_ro_v2.set_host_device(phy.dr_mode() == USB_MODE_HOST).WriteTo(usbctrl_mmio);
    }
    u2p_ro_v2.set_por(0).WriteTo(usbctrl_mmio);
  }

  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  // amlogic_new_usbphy_reset_phycfg_v2()
  auto register_result2 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, ~reset_level);
  if ((register_result2.status() != ZX_OK) || register_result2->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  auto register_result3 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, aml_registers::USB_RESET1_LEVEL_MASK,
                                       aml_registers::USB_RESET1_LEVEL_MASK);
  if ((register_result3.status() != ZX_OK) || register_result3->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(50)));

  for (auto& phy : usbphy2_) {
    auto mmio = &phy.mmio();
    USB_PHY_REG21::Get().ReadFrom(mmio).set_usb2_otg_aca_en(0).WriteTo(mmio);

    auto u2p_r1 = U2P_R1_V2::Get(phy.idx());
    int count = 0;
    while (!u2p_r1.ReadFrom(usbctrl_mmio).phy_rdy()) {
      // wait phy ready max 5ms, common is 100us
      if (count > 1000) {
        FDF_LOG(WARNING, "Vim3UsbPhy::InitPhy U2P_R1_PHY_RDY wait failed");
        break;
      }

      count++;
      zx::nanosleep(zx::deadline_after(zx::usec(5)));
    }
  }

  // One time PLL initialization
  for (auto& phy : usbphy2_) {
    phy.InitPll(pll_settings_);
  }

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitOtg() {
  auto* mmio = &usbctrl_mmio_;

  USB_R1_V2::Get().ReadFrom(mmio).set_u3h_fladj_30mhz_reg(0x20).WriteTo(mmio);

  USB_R5_V2::Get().ReadFrom(mmio).set_iddig_en0(1).set_iddig_en1(1).set_iddig_th(255).WriteTo(mmio);

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitPhy3() {
  for (auto& usbphy3 : usbphy3_) {
    auto status = usbphy3.Init(usbctrl_mmio_);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "usbphy3.Init() error %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

void Vim3UsbPhy::ChangeMode(UsbPhyBase& phy, UsbMode new_mode) {
  auto old_mode = phy.phy_mode();
  if (new_mode == old_mode) {
    FDF_LOG(ERROR, "Already in %d mode", static_cast<uint8_t>(new_mode));
    return;
  }
  phy.SetMode(new_mode, usbctrl_mmio_, pll_settings_);

  if (new_mode == UsbMode::HOST) {
    auto result = controller_->AddXhci();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add xHCI, %s", result.status_string());
    }
    if (old_mode != UsbMode::UNKNOWN) {
      result = controller_->RemoveDwc2();
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to remove DWC2, %s", result.status_string());
      }
    }
  } else {
    auto result = controller_->AddDwc2();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add DWC2, %s", result.status_string());
    }
    if (old_mode != UsbMode::UNKNOWN) {
      result = controller_->RemoveXhci();
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to remove xHCI, %s", result.status_string());
      }
    }
  }
}

void Vim3UsbPhy::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                           const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    return;
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "irq_.wait failed: %d", status);
    return;
  }

  {
    auto r5 = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_);
    // Acknowledge interrupt
    r5.set_usb_iddig_irq(0).WriteTo(&usbctrl_mmio_);

    // Read current host/device role.
    for (auto& phy : usbphy2_) {
      if (phy.dr_mode() != USB_MODE_OTG) {
        continue;
      }

      ChangeMode(phy, r5.iddig_curr() == 0 ? UsbMode::HOST : UsbMode::PERIPHERAL);
    }
  }

  irq_.ack();
}

zx_status_t Vim3UsbPhy::Init(bool has_otg) {
  auto status = InitPhy2();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitPhy2() error %s", zx_status_get_string(status));
    return status;
  }
  status = InitOtg();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitOtg() error %s", zx_status_get_string(status));
    return status;
  }
  status = InitPhy3();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitPhy3() error %s", zx_status_get_string(status));
    return status;
  }

  for (auto& phy : usbphy2_) {
    UsbMode mode;
    if (phy.dr_mode() != USB_MODE_OTG) {
      mode = phy.dr_mode() == USB_MODE_HOST ? UsbMode::HOST : UsbMode::PERIPHERAL;
    } else {
      has_otg = true;
      // Wait for PHY to stabilize before reading initial mode.
      zx::nanosleep(zx::deadline_after(zx::sec(1)));
      mode = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_).iddig_curr() == 0 ? UsbMode::HOST
                                                                         : UsbMode::PERIPHERAL;
    }

    ChangeMode(phy, mode);
  }

  for (auto& phy : usbphy3_) {
    if (phy.dr_mode() != USB_MODE_HOST) {
      FDF_LOG(ERROR, "Not support USB3 in non-host mode yet");
    }

    ChangeMode(phy, UsbMode::HOST);
  }

  if (has_otg) {
    irq_handler_.set_object(irq_.get());
    auto status = irq_handler_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    if (status != ZX_OK) {
      return ZX_ERR_INTERNAL;
    }

    return ZX_OK;
  }

  return ZX_OK;
}

// PHY tuning based on connection state
void Vim3UsbPhy::ConnectStatusChanged(ConnectStatusChangedRequest& request,
                                      ConnectStatusChangedCompleter::Sync& completer) {
  if (dwc2_connected_ == request.connected())
    return;

  for (auto& phy : usbphy2_) {
    if (phy.phy_mode() != UsbMode::PERIPHERAL) {
      continue;
    }
    auto* mmio = &phy.mmio();

    if (request.connected()) {
      PLL_REGISTER::Get(0x38).FromValue(pll_settings_[7]).WriteTo(mmio);
      PLL_REGISTER::Get(0x34).FromValue(pll_settings_[5]).WriteTo(mmio);
    } else {
      phy.InitPll(pll_settings_);
    }
  }

  dwc2_connected_ = request.connected();
}

}  // namespace vim3_usb_phy
