// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/driver.h"

#include <zircon/assert.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace display {

Driver::Driver(Controller* controller, zx_device_t* parent)
    : controller_(controller), parent_(parent) {
  ZX_DEBUG_ASSERT(controller);
}

Driver::~Driver() { zxlogf(TRACE, "Driver::~Driver"); }

zx_status_t Driver::Bind() {
  dc_ = ddk::DisplayControllerImplProtocolClient(parent_);
  if (!dc_.is_valid()) {
    ZX_DEBUG_ASSERT_MSG(false, "Display controller bind mismatch");
    return ZX_ERR_NOT_SUPPORTED;
  }

  // optional display controller clamp rgb protocol client
  dc_clamp_rgb_ = ddk::DisplayClampRgbImplProtocolClient(parent_);

  return ZX_OK;
}

void Driver::ReleaseImage(image_t* image) { dc_.ReleaseImage(image); }

zx_status_t Driver::ReleaseCapture(DriverCaptureImageId driver_capture_image_id) {
  return dc_.ReleaseCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
}

config_check_result_t Driver::CheckConfiguration(
    const display_config_t** display_config_list, size_t display_config_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  return dc_.CheckConfiguration(
      display_config_list, display_config_count, out_client_composition_opcodes_list,
      client_composition_opcodes_count, out_client_composition_opcodes_actual);
}

void Driver::ApplyConfiguration(const display_config_t** display_config_list,
                                size_t display_config_count, const config_stamp_t* config_stamp) {
  dc_.ApplyConfiguration(display_config_list, display_config_count, config_stamp);
}

void Driver::SetEld(DisplayId display_id, const uint8_t* raw_eld_list, size_t raw_eld_count) {
  dc_.SetEld(ToBanjoDisplayId(display_id), raw_eld_list, raw_eld_count);
}

void Driver::SetDisplayControllerInterface(display_controller_interface_protocol_ops_t* ops) {
  dc_.SetDisplayControllerInterface(controller_, ops);
}

zx_status_t Driver::SetDisplayCaptureInterface(display_capture_interface_protocol_ops_t* ops) {
  return dc_.SetDisplayCaptureInterface(controller_, ops);
}

zx_status_t Driver::ImportImage(image_t* image, DriverBufferCollectionId collection_id,
                                uint32_t index) {
  return dc_.ImportImage(image, ToBanjoDriverBufferCollectionId(collection_id), index);
}

zx_status_t Driver::ImportImageForCapture(DriverBufferCollectionId collection_id, uint32_t index,
                                          DriverCaptureImageId* capture_image_id) {
  uint64_t banjo_capture_image_id = ToBanjoDriverCaptureImageId(*capture_image_id);
  auto status = dc_.ImportImageForCapture(ToBanjoDriverBufferCollectionId(collection_id), index,
                                          &banjo_capture_image_id);
  *capture_image_id = ToDriverCaptureImageId(banjo_capture_image_id);
  return status;
}

zx_status_t Driver::ImportBufferCollection(DriverBufferCollectionId collection_id,
                                           zx::channel collection_token) {
  return dc_.ImportBufferCollection(ToBanjoDriverBufferCollectionId(collection_id),
                                    std::move(collection_token));
}

zx_status_t Driver::ReleaseBufferCollection(DriverBufferCollectionId collection_id) {
  return dc_.ReleaseBufferCollection(ToBanjoDriverBufferCollectionId(collection_id));
}

zx_status_t Driver::SetBufferCollectionConstraints(image_t* config,
                                                   DriverBufferCollectionId collection_id) {
  return dc_.SetBufferCollectionConstraints(config, ToBanjoDriverBufferCollectionId(collection_id));
}

zx_status_t Driver::StartCapture(DriverCaptureImageId driver_capture_image_id) {
  return dc_.StartCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
}

zx_status_t Driver::SetDisplayPower(DisplayId display_id, bool power_on) {
  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.SetDisplayPower(ToBanjoDisplayId(display_id), power_on);
}

zx_status_t Driver::SetMinimumRgb(uint8_t minimum_rgb) {
  if (dc_clamp_rgb_.is_valid()) {
    return dc_clamp_rgb_.SetMinimumRgb(minimum_rgb);
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Driver::GetSysmemConnection(zx::channel sysmem_handle) {
  return dc_.GetSysmemConnection(std::move(sysmem_handle));
}

}  // namespace display
