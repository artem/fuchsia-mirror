// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/vim3-usb-phy/vim3-usb-phy.h"

#include <assert.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/errors.h>

#include <cstdio>
#include <sstream>
#include <string>

#include <ddktl/fidl.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy-regs.h"
#include "src/devices/usb/drivers/vim3-usb-phy/vim3_usb_phy_bind.h"

namespace vim3_usb_phy {

// Based on set_usb_pll() in phy-aml-new-usb2-v2.c
void Vim3UsbPhy::InitPll(fdf::MmioBuffer* mmio) {
  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings_[0])
      .set_enable(1)
      .set_reset(1)
      .WriteTo(mmio);

  PLL_REGISTER::Get(0x44).FromValue(pll_settings_[1]).WriteTo(mmio);

  PLL_REGISTER::Get(0x48).FromValue(pll_settings_[2]).WriteTo(mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings_[0])
      .set_enable(1)
      .set_reset(0)
      .WriteTo(mmio);

  // Phy tuning for G12B
  PLL_REGISTER::Get(0x50).FromValue(pll_settings_[3]).WriteTo(mmio);

  PLL_REGISTER::Get(0x54).FromValue(0x2a).WriteTo(mmio);

  PLL_REGISTER::Get(0x34).FromValue(0x70000).WriteTo(mmio);

  // Disconnect threshold
  PLL_REGISTER::Get(0xc).FromValue(0x34).WriteTo(mmio);
}

zx_status_t Vim3UsbPhy::InitPhy() {
  auto* usbctrl_mmio = &*usbctrl_mmio_;

  // first reset USB
  int portnum = USB2PHY_PORTCOUNT;
  uint32_t reset_level = 0;
  while (portnum) {
    portnum--;
    reset_level = reset_level | (1 << (16 + portnum));
  }

  auto level_result =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, reset_level);
  if ((level_result.status() != ZX_OK) || level_result->is_error()) {
    zxlogf(ERROR, "Reset Level Write failed\n");
    return ZX_ERR_INTERNAL;
  }

  // amlogic_new_usbphy_reset_v2()
  auto register_result1 = reset_register_->WriteRegister32(
      RESET1_REGISTER_OFFSET, aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
      aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
  if ((register_result1.status() != ZX_OK) || register_result1->is_error()) {
    zxlogf(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  // amlogic_new_usb2_init()
  for (uint32_t i = 0; i < USB2PHY_PORTCOUNT; i++) {
    auto u2p_ro_v2 = U2P_R0_V2::Get(i).ReadFrom(usbctrl_mmio).set_por(1);
    u2p_ro_v2.set_host_device(1);
    if (i == 1) {
      u2p_ro_v2.set_idpullup0(1).set_drvvbus0(1);
    }

    u2p_ro_v2.WriteTo(usbctrl_mmio);
  }

  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  // amlogic_new_usbphy_reset_phycfg_v2()
  auto register_result2 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, ~reset_level);
  if ((register_result2.status() != ZX_OK) || register_result2->is_error()) {
    zxlogf(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  auto register_result3 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, aml_registers::USB_RESET1_LEVEL_MASK,
                                       aml_registers::USB_RESET1_LEVEL_MASK);
  if ((register_result3.status() != ZX_OK) || register_result3->is_error()) {
    zxlogf(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(50)));

  for (uint32_t i = 0; i < USB2PHY_PORTCOUNT; i++) {
    fdf::MmioBuffer* mmio;
    if (i == 0) {
      mmio = &*usbphy20_mmio_;
    }
    if (i == 1) {
      mmio = &*usbphy20_mmio_;
    }
    USB_PHY_REG21::Get().ReadFrom(mmio).set_usb2_otg_aca_en(0).WriteTo(mmio);

    auto u2p_r1 = U2P_R1_V2::Get(i);
    int count = 0;
    while (!u2p_r1.ReadFrom(usbctrl_mmio).phy_rdy()) {
      // wait phy ready max 5ms, common is 100us
      if (count > 1000) {
        zxlogf(WARNING, "Vim3UsbPhy::InitPhy U2P_R1_PHY_RDY wait failed");
        break;
      }

      count++;
      zx::nanosleep(zx::deadline_after(zx::usec(5)));
    }
  }

  // One time PLL initialization
  InitPll(&*usbphy20_mmio_);
  InitPll(&*usbphy21_mmio_);

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitOtg() {
  auto* mmio = &*usbctrl_mmio_;

  USB_R1_V2::Get().ReadFrom(mmio).set_u3h_fladj_30mhz_reg(0x20).WriteTo(mmio);

  USB_R5_V2::Get().ReadFrom(mmio).set_iddig_en0(1).set_iddig_en1(1).set_iddig_th(255).WriteTo(mmio);

  return ZX_OK;
}

void Vim3UsbPhy::SetMode(UsbMode mode, SetModeCompletion completion) {
  ZX_DEBUG_ASSERT(mode == UsbMode::HOST || mode == UsbMode::PERIPHERAL);
  // Only the irq thread calls |SetMode|, and it should have waited for the
  // previous call to |SetMode| to complete.
  ZX_DEBUG_ASSERT(!set_mode_completion_);
  auto cleanup = fit::defer([&]() {
    if (completion)
      completion();
  });

  if (mode == phy_mode_)
    return;

  auto* usbctrl_mmio = &*usbctrl_mmio_;

  auto r0 = USB_R0_V2::Get().ReadFrom(usbctrl_mmio);
  if (mode == UsbMode::HOST) {
    r0.set_u2d_act(0);
  } else {
    r0.set_u2d_act(1);
    r0.set_u2d_ss_scaledown_mode(0);
  }
  r0.WriteTo(usbctrl_mmio);

  USB_R4_V2::Get()
      .ReadFrom(usbctrl_mmio)
      .set_p21_sleepm0(mode == UsbMode::PERIPHERAL)
      .WriteTo(usbctrl_mmio);

  U2P_R0_V2::Get(0)
      .ReadFrom(usbctrl_mmio)
      .set_host_device(mode == UsbMode::HOST)
      .set_por(0)
      .WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  auto old_mode = phy_mode_;
  phy_mode_ = mode;

  if (old_mode != UsbMode::UNKNOWN) {
    auto* phy_mmio = &*usbphy21_mmio_;

    PLL_REGISTER::Get(0x38)
        .FromValue(mode == UsbMode::HOST ? pll_settings_[6] : 0)
        .WriteTo(phy_mmio);
    PLL_REGISTER::Get(0x34).FromValue(pll_settings_[5]).WriteTo(phy_mmio);
  }

  auto status = AddXhciDevice();
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddXhciDevice() error %s", zx_status_get_string(status));
  }
  status = AddDwc2Device();
  if (status != ZX_OK) {
    zxlogf(ERROR, "AddDwc2Device() error %s", zx_status_get_string(status));
  }
}

int Vim3UsbPhy::IrqThread() {
  auto* mmio = &*usbctrl_mmio_;

  // Wait for PHY to stabilize before reading initial mode.
  zx::nanosleep(zx::deadline_after(zx::sec(1)));

  lock_.Acquire();

  while (true) {
    auto r5 = USB_R5_V2::Get().ReadFrom(mmio);

    // Since |SetMode| is asynchronous, we need to block until it completes.
    sync_completion_t set_mode_sync;
    auto completion = [&]() { sync_completion_signal(&set_mode_sync); };
    // Read current host/device role.
    if (r5.iddig_curr() == 0) {
      zxlogf(INFO, "Entering USB Host Mode");
      SetMode(UsbMode::HOST, std::move(completion));
    } else {
      zxlogf(INFO, "Entering USB Peripheral Mode");
      SetMode(UsbMode::PERIPHERAL, std::move(completion));
    }

    lock_.Release();
    sync_completion_wait(&set_mode_sync, ZX_TIME_INFINITE);
    auto status = irq_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      return 0;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: irq_.wait failed: %d", __func__, status);
      return -1;
    }
    lock_.Acquire();

    // Acknowledge interrupt
    r5.ReadFrom(mmio).set_usb_iddig_irq(0).WriteTo(mmio);
  }

  lock_.Release();

  return 0;
}

zx_status_t Vim3UsbPhy::Create(void* ctx, zx_device_t* parent) {
  auto dev = std::make_unique<Vim3UsbPhy>(parent);

  auto status = dev->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* _ = dev.release();
  return ZX_OK;
}

zx_status_t Vim3UsbPhy::AddXhciDevice() {
  if (xhci_device_) {
    return ZX_ERR_BAD_STATE;
  }

  static zx_protocol_device_t ops = {
      .version = DEVICE_OPS_VERSION,
      // Defer get_protocol() to parent.
      .get_protocol =
          [](void* ctx, uint32_t id, void* proto) {
            return device_get_protocol(reinterpret_cast<Vim3UsbPhy*>(ctx)->zxdev(), id, proto);
          },
      .release = [](void*) {},
  };

  zx_device_prop_t props[] = {
      {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
      {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_GENERIC},
      {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_USB_XHCI_COMPOSITE},
  };

  // clang-format off
  device_add_args_t args = (ddk::DeviceAddArgs("xhci")
                            .set_context(this)
                            .set_props(props)
                            .set_proto_id(ZX_PROTOCOL_USB_PHY)
                            .set_ops(&ops)).get();
  // clang-format on

  auto status = device_add(zxdev(), &args, &xhci_device_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add() error %s", zx_status_get_string(status));
  }
  return status;
}

void Vim3UsbPhy::RemoveXhciDevice(SetModeCompletion completion) {
  auto cleanup = fit::defer([&]() {
    if (completion)
      completion();
  });
  if (xhci_device_) {
    // The callback will be run by the ChildPreRelease hook once the xhci device has been removed.
    set_mode_completion_ = std::move(completion);
    device_async_remove(xhci_device_);
    xhci_device_ = nullptr;
  }
}

zx_status_t Vim3UsbPhy::AddDwc2Device() {
  if (dwc2_device_) {
    return ZX_ERR_BAD_STATE;
  }

  static zx_protocol_device_t ops = {
      .version = DEVICE_OPS_VERSION,
      // Defer get_protocol() to parent.
      .get_protocol =
          [](void* ctx, uint32_t id, void* proto) {
            return device_get_protocol(reinterpret_cast<Vim3UsbPhy*>(ctx)->zxdev(), id, proto);
          },
      .release = [](void*) {},
  };

  zx_device_prop_t props[] = {
      {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
      {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_GENERIC},
      {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_USB_DWC2},
  };

  // clang-format off
  device_add_args_t args = (ddk::DeviceAddArgs("dwc2")
                            .set_context(this)
                            .set_props(props)
                            .set_proto_id(ZX_PROTOCOL_USB_PHY)
                            .set_ops(&ops)).get();
  // clang-format on

  auto status = device_add(zxdev(), &args, &dwc2_device_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add() error %s", zx_status_get_string(status));
  }
  return status;
}

void Vim3UsbPhy::RemoveDwc2Device(SetModeCompletion completion) {
  auto cleanup = fit::defer([&]() {
    if (completion)
      completion();
  });
  if (dwc2_device_) {
    // The callback will be run by the ChildPreRelease hook once the dwc2 device has been removed.
    set_mode_completion_ = std::move(completion);
    device_async_remove(dwc2_device_);
    dwc2_device_ = nullptr;
  }
}

zx_status_t Vim3UsbPhy::Init() {
  zx_status_t status = ZX_OK;
  pdev_ = ddk::PDevFidl::FromFragment(parent());
  if (!pdev_.is_valid()) {
    zxlogf(ERROR, "Vim3UsbPhy::Init: could not get platform device protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto reset_register_client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_registers::Service::Device>(parent(),
                                                                                  "register-reset");
  if (reset_register_client.is_error() || !reset_register_client.value().is_valid()) {
    zxlogf(ERROR, "%s: could not get reset_register fragment", __func__);
    return ZX_ERR_NO_RESOURCES;
  }

  reset_register_.Bind(std::move(reset_register_client.value()));

  size_t actual;
  status = DdkGetMetadata(DEVICE_METADATA_PRIVATE, pll_settings_, sizeof(pll_settings_), &actual);
  if (status != ZX_OK || actual != sizeof(pll_settings_)) {
    zxlogf(ERROR, "Vim3UsbPhy::Init could not get metadata for PLL settings");
    return ZX_ERR_INTERNAL;
  }

  dr_mode_ = USB_MODE_HOST;

  status = pdev_.MapMmio(0, &usbctrl_mmio_);
  if (status != ZX_OK) {
    return status;
  }
  status = pdev_.MapMmio(1, &usbphy20_mmio_);
  if (status != ZX_OK) {
    return status;
  }
  status = pdev_.MapMmio(2, &usbphy21_mmio_);
  if (status != ZX_OK) {
    return status;
  }

  status = pdev_.GetInterrupt(0, &irq_);
  if (status != ZX_OK) {
    return status;
  }

  status = InitPhy();
  if (status != ZX_OK) {
    return status;
  }
  status = InitOtg();
  if (status != ZX_OK) {
    return status;
  }

  return DdkAdd("vim3_usb_phy", DEVICE_ADD_NON_BINDABLE);
}

void Vim3UsbPhy::DdkInit(ddk::InitTxn txn) {
  if (dr_mode_ != USB_MODE_OTG) {
    sync_completion_t set_mode_sync;
    auto completion = [&]() { sync_completion_signal(&set_mode_sync); };
    fbl::AutoLock lock(&lock_);
    if (dr_mode_ == USB_MODE_PERIPHERAL) {
      zxlogf(INFO, "Entering USB Peripheral Mode");
      SetMode(UsbMode::PERIPHERAL, std::move(completion));
    } else {
      zxlogf(INFO, "Entering USB Host Mode");
      SetMode(UsbMode::HOST, std::move(completion));
    }
    sync_completion_wait(&set_mode_sync, ZX_TIME_INFINITE);

    return txn.Reply(ZX_OK);
  }

  irq_thread_started_ = true;
  int rc = thrd_create_with_name(
      &irq_thread_,
      [](void* arg) -> int { return reinterpret_cast<Vim3UsbPhy*>(arg)->IrqThread(); },
      reinterpret_cast<void*>(this), "amlogic-usb-thread");
  if (rc != thrd_success) {
    irq_thread_started_ = false;
    return txn.Reply(ZX_ERR_INTERNAL);  // This will schedule the device to be unbound.
  }
  return txn.Reply(ZX_OK);
}

// PHY tuning based on connection state
void Vim3UsbPhy::UsbPhyConnectStatusChanged(bool connected) {
  fbl::AutoLock lock(&lock_);

  if (dwc2_connected_ == connected)
    return;

  auto* mmio = &*usbphy21_mmio_;

  if (connected) {
    PLL_REGISTER::Get(0x38).FromValue(pll_settings_[7]).WriteTo(mmio);
    PLL_REGISTER::Get(0x34).FromValue(pll_settings_[5]).WriteTo(mmio);
  } else {
    InitPll(mmio);
  }

  dwc2_connected_ = connected;
}

void Vim3UsbPhy::DdkUnbind(ddk::UnbindTxn txn) {
  irq_.destroy();
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }
  txn.Reply();
}

void Vim3UsbPhy::DdkChildPreRelease(void* child_ctx) {
  fbl::AutoLock lock(&lock_);
  if (set_mode_completion_) {
    // If the mode is currently being set, the irq thread will be blocked
    // until we call this completion.
    set_mode_completion_();
  }
}

void Vim3UsbPhy::DdkRelease() { delete this; }

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Vim3UsbPhy::Create;
  return ops;
}();

}  // namespace vim3_usb_phy

ZIRCON_DRIVER(vim3_usb_phy, vim3_usb_phy::driver_ops, "zircon", "0.1");
