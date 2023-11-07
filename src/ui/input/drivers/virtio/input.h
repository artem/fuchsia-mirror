// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_H_
#define SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_H_

#include <fuchsia/hardware/hidbus/c/banjo.h>
#include <fuchsia/hardware/hidbus/cpp/banjo.h>
#include <lib/ddk/io-buffer.h>
#include <lib/hid/boot.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <stdlib.h>

#include <memory>

#include <ddktl/device.h>
#include <virtio/input.h>

#include "src/ui/input/drivers/virtio/input_kbd.h"
#include "src/ui/input/drivers/virtio/input_mouse.h"
#include "src/ui/input/drivers/virtio/input_touch.h"

namespace virtio {

class InputDevice : public Device,
                    public ddk::Device<InputDevice>,
                    public ddk::HidbusProtocol<InputDevice, ddk::base_protocol> {
 public:
  InputDevice(zx_device_t* device, zx::bti bti, std::unique_ptr<Backend> backend);
  virtual ~InputDevice();

  zx_status_t Init() override;

  void IrqRingUpdate() override;
  void IrqConfigChange() override;
  const char* tag() const override { return "virtio-input"; }

  // DDK driver hooks
  void DdkRelease();
  zx_status_t HidbusStart(const hidbus_ifc_protocol_t* ifc);
  void HidbusStop();
  zx_status_t HidbusQuery(uint32_t options, hid_info_t* info);
  zx_status_t HidbusGetDescriptor(hid_description_type_t desc_type, uint8_t* out_data_buffer,
                                  size_t data_size, size_t* out_data_actual);
  // Unsupported calls:
  zx_status_t HidbusGetReport(hid_report_type_t rpt_type, uint8_t rpt_id, uint8_t* out_data_buffer,
                              size_t data_size, size_t* out_data_actual);
  zx_status_t HidbusSetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                              const uint8_t* data_buffer, size_t data_size);
  zx_status_t HidbusGetIdle(uint8_t rpt_id, uint8_t* out_duration);
  zx_status_t HidbusSetIdle(uint8_t rpt_id, uint8_t duration);
  zx_status_t HidbusGetProtocol(hid_protocol_t* out_protocol);
  zx_status_t HidbusSetProtocol(hid_protocol_t protocol);

 private:
  void ReceiveEvent(virtio_input_event_t* event);

  void SelectConfig(uint8_t select, uint8_t subsel);

  virtio_input_config_t config_;

  static const size_t kEventCount = 64;
  io_buffer_t buffers_[kEventCount];

  fbl::Mutex lock_;

  uint8_t dev_class_;
  hidbus_ifc_protocol_t hidbus_ifc_;

  std::unique_ptr<HidDevice> hid_device_;
  Ring vring_ = {this};
};

}  // namespace virtio

#endif  // SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_H_
