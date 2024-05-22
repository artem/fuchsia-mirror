// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_INTERFACE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_INTERFACE_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
// TODO(https://fxbug.dev/42079190): Switch from Banjo to FIDL or api-types-cpp types.
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/zx/result.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"

namespace virtio_display {

// The methods in the [`fuchsia.hardware.display.engine/Engine`] FIDL interface.
//
// This abstract base class only represents the methods in the FIDL interface.
// The events are represented by `CoordinatorEventsInterface`.
//
// This abstract base class also represents the
// [`fuchsia.hardware.display.controller/DisplayControllerImpl`] Banjo
// interface.
class DisplayEngineInterface {
 public:
  DisplayEngineInterface() = default;

  DisplayEngineInterface(const DisplayEngineInterface&) = delete;
  DisplayEngineInterface(DisplayEngineInterface&&) = delete;
  DisplayEngineInterface& operator=(const DisplayEngineInterface&) = delete;
  DisplayEngineInterface& operator=(DisplayEngineInterface&&) = delete;

  virtual void OnCoordinatorConnected() = 0;

  virtual zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId driver_buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) = 0;
  virtual zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId driver_buffer_collection_id) = 0;

  virtual zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) = 0;
  virtual zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) = 0;
  virtual void ReleaseImage(display::DriverImageId driver_image_id) = 0;

  // TODO(costan): Switch from Banjo to FIDL or api-types-cpp types.
  virtual config_check_result_t CheckConfiguration(
      cpp20::span<const display_config_t> display_configs,
      cpp20::span<client_composition_opcode_t> out_client_composition_opcodes,
      size_t* out_client_composition_opcodes_actual) = 0;

  // TODO(costan): Switch from Banjo to FIDL or api-types-cpp types.
  virtual void ApplyConfiguration(cpp20::span<const display_config_t> display_configs,
                                  const config_stamp_t* banjo_config_stamp) = 0;

  virtual zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId driver_buffer_collection_id) = 0;

  virtual zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on) = 0;

  virtual bool IsCaptureSupported() = 0;
  virtual zx::result<> StartCapture(display::DriverCaptureImageId capture_image_id) = 0;
  virtual zx::result<> ReleaseCapture(display::DriverCaptureImageId capture_image_id) = 0;
  virtual bool IsCaptureCompleted() = 0;

  virtual zx::result<> SetMinimumRgb(uint8_t minimum_rgb) = 0;

 protected:
  // Destruction via base class pointer is not supported intentionally.
  // Instances are not expected to be owned by pointers to base classes.
  ~DisplayEngineInterface() = default;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_ENGINE_INTERFACE_H_
