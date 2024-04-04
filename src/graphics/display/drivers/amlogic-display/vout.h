// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include "src/graphics/display/drivers/amlogic-display/clock.h"
#include "src/graphics/display/drivers/amlogic-display/dsi-host.h"
#include "src/graphics/display/drivers/amlogic-display/hdmi-host.h"
#include "src/graphics/display/drivers/amlogic-display/panel-config.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

namespace amlogic_display {

enum class VoutType : uint8_t {
  kDsi,
  kHdmi,
};

class Vout : public ddk::I2cImplProtocol<Vout> {
 public:
  // Returns a non-null pointer to the Vout instance outputting DSI signal on
  // success.
  static zx::result<std::unique_ptr<Vout>> CreateDsiVout(zx_device_t* parent, uint32_t panel_type,
                                                         uint32_t width, uint32_t height,
                                                         inspect::Node node);

  // Returns a non-null pointer to the Vout instance outputting HDMI signal on
  // success.
  static zx::result<std::unique_ptr<Vout>> CreateHdmiVout(zx_device_t* parent, inspect::Node node);

  // Sets only the display size, feature bits and panel settings for testing.
  // Returns a non-null pointer to the Vout instance on success.
  static zx::result<std::unique_ptr<Vout>> CreateDsiVoutForTesting(uint32_t panel_type,
                                                                   uint32_t width, uint32_t height);

  // Creates a Vout instance that outputs MIPI-DSI signal.
  //
  // `panel_config` must be non-null and must outlive the `Vout` instance.
  Vout(std::unique_ptr<DsiHost> dsi_host, std::unique_ptr<Clock> dsi_clock, uint32_t width,
       uint32_t height, const PanelConfig* panel_config, inspect::Node node);

  // Creates a Vout instance that outputs HDMI signal.
  Vout(std::unique_ptr<HdmiHost> hdmi_host, inspect::Node node);

  Vout(Vout&&) = delete;
  Vout(const Vout&) = delete;
  Vout& operator=(Vout&&) = delete;
  Vout& operator=(const Vout&) = delete;

  void PopulateAddedDisplayArgs(
      added_display_args_t* args, display::DisplayId display_id,
      cpp20::span<const fuchsia_images2_pixel_format_enum_value_t> pixel_formats);

  VoutType type() { return type_; }
  bool supports_hpd() const { return supports_hpd_; }

  void DisplayConnected();
  void DisplayDisconnected();

  // Vout must be of `kHdmi` type.
  bool IsDisplayTimingSupported(const display::DisplayTiming& timing);

  // Vout must be of `kHdmi` type.
  zx::result<> ApplyConfiguration(const display::DisplayTiming& timing);

  // Required functions for I2cImpl
  zx_status_t I2cImplGetMaxTransferSize(size_t* out_size) {
    *out_size = UINT32_MAX;
    return ZX_OK;
  }
  zx_status_t I2cImplSetBitrate(uint32_t bitrate) { return ZX_OK; }  // no-op
  zx_status_t I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count);

  // Attempt to turn off all connected displays, and disable clocks. This will
  // also stop vsync interrupts. This is aligned with the interface for
  // fuchsia.hardware.display, where a disabled display does not produce OnVsync
  // events.
  //
  // This method is not guaranteed to power off all devices. Returns ZX_OK if
  // successful, ZX_ERR_UNSUPPORTED if the panel cannot be powered off. May
  // return other errors.
  zx::result<> PowerOff();

  // Turn on all connected displays and reprogram clocks. This will resume vsync
  // interrupts as well.
  //
  // This method is not guaranteed to power on all devices. Returns ZX_OK if
  // successful, ZX_ERR_UNSUPPORTED if the panel cannot be powered on. May
  // return other errors.
  zx::result<> PowerOn();

  void Dump();

 private:
  VoutType type_;

  // Features
  bool supports_hpd_ = false;

  inspect::Node node_;

  struct dsi_t {
    std::unique_ptr<DsiHost> dsi_host;
    std::unique_ptr<Clock> clock;

    // display dimensions and format
    uint32_t width = 0;
    uint32_t height = 0;

    // TODO(https://fxbug.dev/318800903): Use const reference when `dsi_` moves
    // to its own implementation class.
    PanelConfig panel_config;
  } dsi_;

  struct hdmi_t {
    std::unique_ptr<HdmiHost> hdmi_host;

    display::DisplayTiming current_display_timing_;
  } hdmi_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VOUT_H_
