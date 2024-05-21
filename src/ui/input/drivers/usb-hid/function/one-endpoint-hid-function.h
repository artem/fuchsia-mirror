// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_USB_HID_FUNCTION_ONE_ENDPOINT_HID_FUNCTION_H_
#define SRC_UI_INPUT_DRIVERS_USB_HID_FUNCTION_ONE_ENDPOINT_HID_FUNCTION_H_

#include <fidl/fuchsia.hardware.hidbus/cpp/wire.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/device.h>

#include <memory>
#include <vector>

#include <ddktl/device.h>
#include <usb/hid.h>

namespace one_endpoint_hid_function {

// This driver is for testing the USB-HID driver. It binds as a peripheral USB
// device and sends fake HID report descriptors and HID reports. The tests for
// this driver and the USB-HID driver are with the other usb-virtual-bus tests.
class FakeUsbHidFunction;
using DeviceType = ddk::Device<FakeUsbHidFunction>;
class FakeUsbHidFunction : public DeviceType {
 public:
  FakeUsbHidFunction(zx_device_t* parent) : DeviceType(parent), function_(parent) {}
  zx_status_t Bind();
  // |ddk::Device|
  void DdkRelease();

  static size_t UsbFunctionInterfaceGetDescriptorsSize(void* ctx);

  static void UsbFunctionInterfaceGetDescriptors(void* ctx, uint8_t* out_descriptors_buffer,
                                                 size_t descriptors_size,
                                                 size_t* out_descriptors_actual);
  static zx_status_t UsbFunctionInterfaceControl(void* ctx, const usb_setup_t* setup,
                                                 const uint8_t* write_buffer, size_t write_size,
                                                 uint8_t* out_read_buffer, size_t read_size,
                                                 size_t* out_read_actual);
  static zx_status_t UsbFunctionInterfaceSetConfigured(void* ctx, bool configured,
                                                       usb_speed_t speed);
  static zx_status_t UsbFunctionInterfaceSetInterface(void* ctx, uint8_t interface,
                                                      uint8_t alt_setting);

 private:
  usb_function_interface_protocol_ops_t function_interface_ops_{
      .get_descriptors_size = UsbFunctionInterfaceGetDescriptorsSize,
      .get_descriptors = UsbFunctionInterfaceGetDescriptors,
      .control = UsbFunctionInterfaceControl,
      .set_configured = UsbFunctionInterfaceSetConfigured,
      .set_interface = UsbFunctionInterfaceSetInterface,
  };
  ddk::UsbFunctionProtocolClient function_;

  std::vector<uint8_t> report_desc_;
  std::vector<uint8_t> report_;

  struct fake_usb_hid_descriptor_t {
    usb_interface_descriptor_t interface;
    usb_endpoint_descriptor_t interrupt;
    usb_hid_descriptor_t hid_descriptor;
  } __PACKED;

  struct DescriptorDeleter {
    void operator()(fake_usb_hid_descriptor_t* desc) { free(desc); }
  };
  std::unique_ptr<fake_usb_hid_descriptor_t, DescriptorDeleter> descriptor_;
  size_t descriptor_size_;

  fuchsia_hardware_hidbus::wire::HidProtocol hid_protocol_ =
      fuchsia_hardware_hidbus::wire::HidProtocol::kReport;
};

}  // namespace one_endpoint_hid_function

#endif  // SRC_UI_INPUT_DRIVERS_USB_HID_FUNCTION_ONE_ENDPOINT_HID_FUNCTION_H_
