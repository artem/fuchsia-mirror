// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_MOUSE_H_
#define SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_MOUSE_H_

#include "src/ui/input/drivers/virtio/input_device.h"

namespace virtio {

struct MouseReport {
  zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);

  enum ButtonIndex : uint8_t {
    kLeft = 1,
    kRight = 2,
    kMid = 3,

    kMaxButtonCount = 3,
  };
  std::array<bool, kMaxButtonCount> buttons;
  int16_t rel_x;
  int16_t rel_y;
  int16_t rel_wheel;

  void ToFidlInputReport(
      fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
      fidl::AnyArena& allocator);
};

class HidMouse : public HidDevice<MouseReport> {
 public:
  fuchsia_input_report::wire::DeviceDescriptor GetDescriptor(fidl::AnyArena& allocator) override;
  void ReceiveEvent(virtio_input_event_t* event) override;

 private:
  void ReceiveRelEvent(virtio_input_event_t* event);
  void ReceiveKeyEvent(virtio_input_event_t* event);
};

}  // namespace virtio

#endif  // SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_MOUSE_H_
