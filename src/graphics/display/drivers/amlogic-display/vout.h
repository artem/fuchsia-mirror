// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <lib/device-protocol/display-panel.h>
#include <zircon/pixelformat.h>

#include "aml-dsi-host.h"
#include "amlogic-clock.h"

namespace amlogic_display {

enum VoutType { kDsi, kUnknown };

class Vout {
 public:
  Vout() = default;
  zx_status_t InitDsi(zx_device_t* parent, uint32_t panel_type, uint32_t width, uint32_t height);

  zx_status_t RestartDisplay(zx_device_t* parent);

  void PopulateAddedDisplayArgs(added_display_args_t* args, uint64_t display_id);
  bool IsFormatSupported(zx_pixel_format_t format);

  VoutType type() { return type_; }
  bool supports_afbc() const { return supports_afbc_; }
  bool supports_capture() const { return supports_capture_; }
  uint32_t display_width() {
    switch (type_) {
      case kDsi:
        return dsi_.disp_setting.h_active;
      default:
        return 0;
    }
  }
  uint32_t display_height() {
    switch (type_) {
      case kDsi:
        return dsi_.disp_setting.v_active;
      default:
        return 0;
    }
  }
  uint32_t fb_width() {
    switch (type_) {
      case kDsi:
        return dsi_.width;
      default:
        return 0;
    }
  }
  uint32_t fb_height() {
    switch (type_) {
      case kDsi:
        return dsi_.height;
      default:
        return 0;
    }
  }

  void Dump();

 private:
  VoutType type_ = VoutType::kUnknown;

  // Features
  bool supports_afbc_ = false;
  bool supports_capture_ = false;

  struct dsi_t {
    std::unique_ptr<amlogic_display::AmlDsiHost> dsi_host;
    std::unique_ptr<amlogic_display::AmlogicDisplayClock> clock;

    // display dimensions and format
    uint32_t width = 0;
    uint32_t height = 0;

    // Display structure used by various layers of display controller
    display_setting_t disp_setting;
  } dsi_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_
