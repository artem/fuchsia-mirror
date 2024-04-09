// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_DEVICE_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_DEVICE_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.phy/cpp/driver/fidl.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio-buffer.h>

#include <mutex>
namespace aml_usb_phy {

class AmlUsbPhy;

class AmlUsbPhyDevice : public fdf::DriverBase {
 private:
  static constexpr char kDeviceName[] = "aml_usb_phy";

  class ChildNode {
   public:
    explicit ChildNode(AmlUsbPhyDevice* parent, std::string_view name, uint32_t property_did)
        : parent_(parent), name_(name), property_did_(property_did) {}

    ChildNode& operator--();
    ChildNode& operator++();

   private:
    AmlUsbPhyDevice* parent_;
    const std::string_view name_;
    const uint32_t property_did_;

    std::mutex lock_;
    fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_ __TA_GUARDED(lock_);
    compat::SyncInitializedDeviceServer compat_server_ __TA_GUARDED(lock_);
    std::atomic_uint32_t count_ __TA_GUARDED(lock_) = 0;
  };

 public:
  AmlUsbPhyDevice(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(driver_dispatcher)) {}
  zx::result<> Start() override;
  void Stop() override;

  ChildNode xhci_{this, "xhci", PDEV_DID_USB_XHCI_COMPOSITE};
  ChildNode dwc2_{this, "dwc2", PDEV_DID_USB_DWC2};

  // For testing.
  std::unique_ptr<AmlUsbPhy>& device() { return device_; }

 private:
  zx::result<> CreateNode();

  // Virtual for testing.
  virtual zx::result<fdf::MmioBuffer> MapMmio(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t idx);

  std::unique_ptr<AmlUsbPhy> device_;

  fdf::ServerBindingGroup<fuchsia_hardware_usb_phy::UsbPhy> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_AML_USB_PHY_DEVICE_H_
