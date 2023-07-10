// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_SOFTWARE_COMPOSITOR_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_SOFTWARE_COMPOSITOR_H_

#include <lib/stdcompat/span.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types-cpp/alpha-mode.h"
#include "src/graphics/display/lib/api-types-cpp/frame.h"
#include "src/graphics/display/lib/api-types-cpp/transform.h"
#include "src/graphics/display/testing/software-compositor/pixel.h"

namespace software_compositor {

struct Offset2D {
  int x;
  int y;
};

inline bool operator==(const Offset2D& lhs, const Offset2D& rhs) {
  return lhs.x == rhs.x && lhs.y == rhs.y;
}

inline bool operator!=(const Offset2D& lhs, const Offset2D& rhs) { return !(lhs == rhs); }

struct ImageProperties {
  int width;
  int height;
  int stride_bytes;
  PixelFormat pixel_format;
};

struct OutputImage {
 public:
  inline PixelData At(const Offset2D& offset) const;
  inline void SetPixelData(const Offset2D& offset, const PixelData& color) const;

  cpp20::span<uint8_t> buffer;
  ImageProperties properties;
};

struct InputImage {
 public:
  inline PixelData At(const Offset2D& offset) const;

  cpp20::span<const uint8_t> buffer;
  ImageProperties properties;
};

// A compositor using software rendering to fill solid colors and draw images
// onto an output image (canvas).
class SoftwareCompositor {
 public:
  // Properties of composition of a layer onto the canvas.
  struct CompositionProperties {
    // The source frame. Only the `source_image` part of the input image will be
    // clipped and used.
    //
    // Equivalent to
    // - the `src_frame` field in FIDL [`fuchsia.hardware.display/Coordinator/
    //   SetLayerPrimaryPosition`] method, and
    // - the `src_frame` field in banjo [`fuchsia.hardware.display.controller/
    //   PrimaryLayer`] struct.
    ::display::Frame source_frame;

    // The destination (canvas) frame. The clipped input image will be scaled
    // and painted onto the `canvas_frame` part of the canvas. `canvas_frame`
    // will be cropped to the extent of the canvas.
    //
    // Equivalent to
    // - the `dst_frame` field in FIDL [`fuchsia.hardware.display/Coordinator/
    //   SetLayerPrimaryPosition`] method, and
    // - the `dst_frame` field in banjo [`fuchsia.hardware.display.controller/
    //   PrimaryLayer`] struct.
    ::display::Frame canvas_frame;

    // Indicates how image will be transformed (rotated / flipped).
    //
    // Equivalent to
    // - the `transform` field in FIDL [`fuchsia.hardware.display/
    //   Coordinator/SetLayerPrimaryPosition`] method, and
    // - the `transform_mode` field in banjo [`fuchsia.hardware.display.
    //   controller/PrimaryLayer`] struct.
    ::display::Transform transform;

    // Indicates whether alpha blending will be performed and the type of alpha
    // blending.
    //
    // Equivalent to
    // - the `mode` field in FIDL [`fuchsia.hardware.display/Coordinator/
    //   SetLayerPrimaryAlpha`] method, and
    // - the `alpha_mode` field in banjo [`fuchsia.hardware.display.controller/
    //   PrimaryLayer`] struct.
    ::display::AlphaMode alpha_mode;
  };

  // Represents an Image layer to be composited on the canvas.
  struct ImageLayerForComposition {
    InputImage image;
    CompositionProperties properties;
  };

  // The buffer backing `canvas` must outlive the newly created instance.
  explicit SoftwareCompositor(const OutputImage& canvas);

  // Clears the canvas by filling a solid `color` of format `pixel_format` on
  // the whole canvas.
  //
  // This produces the same result as Vulkan command `vkCmdClearColorImage`.
  void ClearCanvas(const PixelData& color, PixelFormat pixel_format) const;

  // Composites all the layers in `image_layers` onto the the canvas using
  // composition properties specified in each image layer.
  //
  // Canvas pixels not covered by any image layer in `image_layers` will not
  // be modified.
  //
  // Image layers are sorted by z-index in ascending order, i.e. image layer
  // in the front of the `image_layers` list will be composited first and on the
  // bottom, the layer in the end will be composited the last and on the top.
  //
  // For each ImageLayerForComposition, its `composition_properties` must
  // fulfill the following constraints:
  // - `alpha_mode` must be kDisable.
  // - `transform` must be kIdentity.
  // - the source frame `source_frame` must start at (0, 0) and have the same
  //   size as `input_image`.
  // - the destination frame `canvas_frame` must fall completely within the
  //   canvas and have the same size as `input_image`.
  // TODO(fxbug.dev/124683): Supports more composition properties.
  // TODO(fxbug.dev/130386): Instead of providing a separate ClearCanvas()
  // command, we should integrate background filling into
  // CompositeImageLayers().
  void CompositeImageLayers(cpp20::span<const ImageLayerForComposition> image_layers) const;

 private:
  // Composites the `input_image` onto the top of the canvas using given
  // `composition_properties`.
  //
  // For composition without alpha composition nor transformation, this produces
  // the same result as the Vulkan command `vkCmdBlitImage`.
  //
  // Currently only the following `composition_properties` is supported:
  // - `alpha_mode` must be kDisable.
  // - `transform` must be kIdentity.
  // - the source frame `source_frame` must start at (0, 0) and have the same
  //   size as `input_image`.
  // - the destination frame `canvas_frame` must fall completely within the
  //   canvas and have the same size as `input_image`.
  // TODO(fxbug.dev/124683): Supports more composition properties.
  void CompositeImage(const InputImage& input_image,
                      const CompositionProperties& composition_properties) const;

  const OutputImage canvas_;
};

}  // namespace software_compositor

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_SOFTWARE_COMPOSITOR_SOFTWARE_COMPOSITOR_H_
