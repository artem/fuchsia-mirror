// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_UNIT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_UNIT_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/handle.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/pmt.h>
#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstdint>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/pixel-grid-size2d.h"
#include "src/graphics/display/drivers/amlogic-display/rdma.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace amlogic_display {

// The Video Input Unit (VIU) retrieves pixel data (images) from its input
// channels, and scales, post-processes and blends the pixel data into the
// display contents for the Video Output (VOUT) module to encode and transmit
// as electronic signals.
//
// The VideoInputUnit class is an abstraction of the VIU hardware block. It
// controls the OSD layers, scalers and blenders within the VIU.
class VideoInputUnit {
 public:
  static zx::result<std::unique_ptr<VideoInputUnit>> Create(ddk::PDevFidl* pdev,
                                                            inspect::Node* video_input_unit_node);

  VideoInputUnit(VideoInputUnit& other) = delete;

  // Disable the OSD layer and set the latest stamp to |config_stamp|.
  // If the driver disables (pauses) the layer because the client sets an empty
  // config, the |config_stamp| should be the client-provided stamp; otherwise
  // it should use the invalid stamp value indicating that the OSD has been
  // invalidated.
  void DisableLayer(display::ConfigStamp config_stamp = display::kInvalidConfigStamp);
  void EnableLayer();

  // Schedules the given |config| to be applied by the RDMA engine when the next VSYNC interrupt
  // occurs.
  void FlipOnVsync(uint8_t idx, const display_config_t* config, display::ConfigStamp config_stamp);

  // Returns the image handle that was most recently processed by the RDMA engine. If RDMA is
  // determined to be in progress and incomplete, then the previously applied image is returned. If
  // RDMA is determined to be complete at the time of a call, then the RDMA engine registers are
  // updated accordingly.
  //
  // This function is used by the vsync thread to determine the latest applied config.
  display::ConfigStamp GetLastConfigStampApplied();

  void Dump();
  void Release();

  // This function converts a float into Signed fixed point 3.10 format
  // [12][11:10][9:0] = [sign][integer][fraction]
  static uint32_t FloatToFixed3_10(float f);
  // This function converts a float into Signed fixed point 2.10 format
  // [11][10][9:0] = [sign][integer][fraction]
  static uint32_t FloatToFixed2_10(float f);

  void SetMinimumRgb(uint8_t minimum_rgb);

 private:
  VideoInputUnit(fdf::MmioBuffer vpu_mmio, std::unique_ptr<RdmaEngine> rdma);

  // Configures the video input unit hardware blocks so that the VIU displays a
  // single layer of unscaled image (of size `layer_image_size`) on the display
  // (of size `display_contents_size`).
  //
  // Both `layer_image_size` and `display_contents_size` must be valid.
  void ConfigForSingleNonscaledLayer(PixelGridSize2D layer_image_size,
                                     PixelGridSize2D display_contents_size);

  // Sets up the OSD layers before they are scaled and blended.
  //
  // The OSD layers read images of `layer_image_size` and display them on
  // a display device of `display_contents_size`.
  //
  // TODO(https://fxbug.dev/42062952): Fully support multiple layers.
  void ConfigOsdLayers(PixelGridSize2D layer_image_size, PixelGridSize2D display_contents_size);

  // Sets up the blending modules on the OSD layers and the Video Post
  // Processor (VPP) to display a single layer.
  //
  // It places the OSD1 layer (of size `layer_size`) on the top-left corner
  // of the display (of size `display_contents_size`).
  void ConfigSingleLayerBlending(PixelGridSize2D layer_size, PixelGridSize2D display_contents_size);

  // Disables framebuffer scaling.
  // TODO(https://fxbug.dev/317922128): Add OSD scaler support.
  void DisableScaling();

  // Sets up the AFBC (ARM Frame Buffer Compression) decoder IP block for the
  // OSD1 layer. The input image of the layer is of `layer_image_size`.
  //
  // TODO(https://fxbug.dev/42062952): Fully support multiple layers.
  void ConfigAfbcDecoder(PixelGridSize2D layer_image_size);

  bool ConfigNeededForSingleNonscaledLayer(PixelGridSize2D layer_image_size,
                                           PixelGridSize2D display_contents_size) const;

  void SetColorCorrection(uint32_t rdma_table_idx, const display_config_t* config);

  void DumpNonRdmaRegisters();

  fdf::MmioBuffer vpu_mmio_;

  PixelGridSize2D layer_image_size_ = kInvalidPixelGridSize2D;
  PixelGridSize2D display_contents_size_ = kInvalidPixelGridSize2D;

  std::unique_ptr<RdmaEngine> rdma_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_UNIT_H_
