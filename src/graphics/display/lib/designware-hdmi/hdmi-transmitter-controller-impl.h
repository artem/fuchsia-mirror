// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_HDMI_TRANSMITTER_CONTROLLER_IMPL_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_HDMI_TRANSMITTER_CONTROLLER_IMPL_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/mmio/mmio-buffer.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware-hdmi/color-param.h"
#include "src/graphics/display/lib/designware-hdmi/hdmi-transmitter-controller.h"

namespace designware_hdmi {

// The implementation of the DesignWare Cores HDMI 2.0 TX Controller (with
// HDCP).
class HdmiTransmitterControllerImpl : public HdmiTransmitterController {
 public:
  // `controller_mmio` is the MMIO region for the HDMI Transmitter Controller
  // registers, as defined in Section 6 "Register Descriptions" (pages 185-508)
  // of Synopsys DesignWare Cores HDMI Transmitter Controller Databook, v2.12a,
  // dated April 2016.
  explicit HdmiTransmitterControllerImpl(fdf::MmioBuffer controller_mmio)
      : controller_mmio_(std::move(controller_mmio)) {}
  ~HdmiTransmitterControllerImpl() override = default;

  zx_status_t InitHw() override;
  zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) override;

  void ConfigHdmitx(const ColorParam& color_param, const display::DisplayTiming& mode,
                    const hdmi_param_tx& p) override;
  void SetupInterrupts() override;
  void Reset() override;
  void SetupScdc(bool is4k) override;
  void ResetFc() override;
  void SetFcScramblerCtrl(bool is4k) override;

  void PrintRegisters() override;

 private:
  void WriteReg(uint32_t addr, uint8_t data) { controller_mmio_.Write8(data, addr); }
  uint8_t ReadReg(uint32_t addr) { return controller_mmio_.Read8(addr); }

  void PrintReg(const char* name, uint32_t address);

  void ScdcWrite(uint8_t addr, uint8_t val);
  void ScdcRead(uint8_t addr, uint8_t* val);

  void ConfigCsc(const ColorParam& color_param);

  fdf::MmioBuffer controller_mmio_;
};

}  // namespace designware_hdmi

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_HDMI_TRANSMITTER_CONTROLLER_IMPL_H_
