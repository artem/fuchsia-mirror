// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fuchsia/hardware/display/clamprgb/cpp/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"

namespace display {

static constexpr fdf_arena_tag_t kArenaTag = 'DISP';

class Controller;

// Manages the state associated with a display coordinator driver connection.
class Driver : public ddk::Device<Driver> {
 public:
  explicit Driver(Controller* controller, zx_device_t* parent);

  Driver(const Driver&) = delete;
  Driver& operator=(const Driver&) = delete;

  ~Driver();

  zx_status_t Bind();
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  zx_status_t Bind(std::unique_ptr<Driver>* device_ptr);

  void ReleaseImage(image_t* image);
  zx_status_t ReleaseCapture(DriverCaptureImageId driver_capture_image_id);

  config_check_result_t CheckConfiguration(
      const display_config_t** display_config_list, size_t display_config_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void ApplyConfiguration(const display_config_t** display_config_list, size_t display_config_count,
                          const config_stamp_t* config_stamp);

  void SetEld(DisplayId display_id, const uint8_t* raw_eld_list, size_t raw_eld_count);

  void SetDisplayControllerInterface(display_controller_interface_protocol_ops_t* ops);
  zx_status_t SetDisplayCaptureInterface(display_capture_interface_protocol_ops_t* ops);

  zx_status_t ImportImage(image_t* image, DriverBufferCollectionId collection_id, uint32_t index);
  zx_status_t ImportImageForCapture(DriverBufferCollectionId collection_id, uint32_t index,
                                    DriverCaptureImageId* capture_image_id);
  zx_status_t ImportBufferCollection(DriverBufferCollectionId collection_id,
                                     zx::channel collection_token);
  zx_status_t ReleaseBufferCollection(DriverBufferCollectionId collection_id);
  zx_status_t SetBufferCollectionConstraints(image_t* config,
                                             DriverBufferCollectionId collection_id);

  zx_status_t StartCapture(DriverCaptureImageId driver_capture_image_id);
  zx_status_t SetDisplayPower(DisplayId display_id, bool power_on);
  zx_status_t SetMinimumRgb(uint8_t minimum_rgb);

  zx_status_t GetSysmemConnection(zx::channel sysmem_handle);

 private:
  // TODO(https://fxbug.dev/325474586): Revisit whether a single arena is the
  // right approach.
  fdf::Arena arena_;
  Controller* const controller_;

  // DFV1 Parent
  zx_device_t* parent_;

  // FIDL Client
  fdf::WireSyncClient<fuchsia_hardware_display_engine::Engine> engine_;

  // Whether to use the FIDL client. If false, use the Banjo client.
  bool use_engine_ = false;

  // Banjo Client
  ddk::DisplayControllerImplProtocolClient dc_;
  ddk::DisplayClampRgbImplProtocolClient dc_clamp_rgb_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_
