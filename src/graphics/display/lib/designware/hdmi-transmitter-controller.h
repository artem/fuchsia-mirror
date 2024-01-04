// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_TRANSMITTER_CONTROLLER_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_TRANSMITTER_CONTROLLER_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/mmio/mmio-buffer.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware/color-param.h"

namespace designware_hdmi {

// TODO(https://fxbug.dev/136415): The struct name is against Google C++ style guide.
// Rename the struct.
struct hdmi_param_tx {
  uint16_t vic;
  uint8_t aspect_ratio;
  uint8_t colorimetry;
  bool is4K;
};

// The interface of the DesignWare Cores HDMI transmitter controller IP core
// (also known as DWC_hdmi_tx).
class HdmiTransmitterController {
 public:
  virtual ~HdmiTransmitterController() = default;

  // TODO(https://fxbug.dev/136258): Revise the design and naming of the class methods
  // below.

  virtual zx_status_t InitHw() = 0;
  virtual zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) = 0;

  virtual void ConfigHdmitx(const ColorParam& color_param, const display::DisplayTiming& mode,
                            const hdmi_param_tx& p) = 0;
  virtual void SetupInterrupts() = 0;
  virtual void Reset() = 0;
  virtual void SetupScdc(bool is4k) = 0;
  virtual void ResetFc() = 0;
  virtual void SetFcScramblerCtrl(bool is4k) = 0;

  virtual void PrintRegisters() = 0;
};

}  // namespace designware_hdmi

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_TRANSMITTER_CONTROLLER_H_
