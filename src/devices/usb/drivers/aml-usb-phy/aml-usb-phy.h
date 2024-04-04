// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/async/cpp/irq.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy-device.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy2.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy3.h"

namespace aml_usb_phy {

class AmlUsbPhy : public fdf::Server<fuchsia_hardware_usb_phy::UsbPhy> {
 public:
  AmlUsbPhy(AmlUsbPhyDevice* controller, PhyType type,
            fidl::ClientEnd<fuchsia_hardware_registers::Device> reset_register,
            fdf::MmioBuffer usbctrl_mmio, zx::interrupt irq, std::vector<UsbPhy2> usbphy2,
            std::vector<UsbPhy3> usbphy3, bool needs_hack)
      : type_(type),
        controller_(controller),
        reset_register_(std::move(reset_register)),
        usbctrl_mmio_(std::move(usbctrl_mmio)),
        usbphy2_(std::move(usbphy2)),
        usbphy3_(std::move(usbphy3)),
        irq_(std::move(irq)),
        needs_hack_(needs_hack) {}
  ~AmlUsbPhy() override {
    irq_handler_.Cancel();
    irq_.destroy();
  }

  zx_status_t Init();

  // fuchsia_hardware_usb_phy::UsbPhy required methods
  void ConnectStatusChanged(ConnectStatusChangedRequest& request,
                            ConnectStatusChangedCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_phy::UsbPhy> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unknown method %lu", metadata.method_ordinal);
  }

  // For testing.
  bool dwc2_connected() const { return dwc2_connected_; }
  UsbPhyBase* usbphy(UsbProtocol proto, uint32_t idx) {
    return proto == UsbProtocol::Usb2_0 ? static_cast<UsbPhyBase*>(&usbphy2_.at(idx))
                                        : static_cast<UsbPhyBase*>(&usbphy3_.at(idx));
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(AmlUsbPhy);

  zx_status_t InitPhy2();
  zx_status_t InitPhy3();
  zx_status_t InitOtg();

  void ChangeMode(UsbPhyBase& phy, UsbMode new_mode);

  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  // Used for debugging.
  void dump_regs();

  const PhyType type_;
  AmlUsbPhyDevice* controller_;

  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_register_;
  fdf::MmioBuffer usbctrl_mmio_;
  std::vector<UsbPhy2> usbphy2_;
  std::vector<UsbPhy3> usbphy3_;

  zx::interrupt irq_;
  async::IrqMethod<AmlUsbPhy, &AmlUsbPhy::HandleIrq> irq_handler_{this};

  // S905D2 and S905D3 set a few PLL values differently. needs_hack_ is true if PID is S905D2 or
  // S905D3.
  const bool needs_hack_;
  bool dwc2_connected_ = false;
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_H_
