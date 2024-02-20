// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/util.h"

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <zircon/status.h>

#include "src/ui/scenic/lib/allocation/id.h"

namespace scenic_impl {

bool ImportBufferCollection(
    allocation::GlobalBufferCollectionId buffer_collection_id,
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
    const fuchsia::hardware::display::types::ImageConfig& image_config) {
  const fuchsia::hardware::display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);
  fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result result;
  zx_status_t status = display_coordinator->ImportBufferCollection(display_buffer_collection_id,
                                                                   std::move(token), &result);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to call FIDL ImportBufferCollection: "
                   << zx_status_get_string(status);
    return false;
  }
  if (result.is_err()) {
    FX_LOGS(ERROR) << "Failed to import BufferCollection: " << zx_status_get_string(result.err());
    return false;
  }

  fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result
      set_constraints_result;
  status = display_coordinator->SetBufferCollectionConstraints(
      display_buffer_collection_id, image_config, &set_constraints_result);
  auto release_buffer_collection_on_failure = fit::defer([&] {
    if (display_coordinator->ReleaseBufferCollection(display_buffer_collection_id) != ZX_OK) {
      FX_LOGS(ERROR) << "ReleaseBufferCollection failed.";
    }
  });

  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to call FIDL SetBufferCollectionConstraints: "
                   << zx_status_get_string(status);
    return false;
  }
  if (set_constraints_result.is_err()) {
    FX_LOGS(ERROR) << "Failed to set BufferCollection constraints: "
                   << zx_status_get_string(set_constraints_result.err());
    return false;
  }

  release_buffer_collection_on_failure.cancel();
  return true;
}

DisplayEventId ImportEvent(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const zx::event& event) {
  static uint64_t id_generator = fuchsia::hardware::display::types::INVALID_DISP_ID + 1;

  zx::event dup;
  if (event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate display controller event.";
    return {.value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  }

  // Generate a new display ID after we've determined the event can be duplicated as to not
  // waste an id.
  DisplayEventId event_id = {.value = id_generator++};

  auto before = zx::clock::get_monotonic();
  auto status = display_coordinator->ImportEvent(std::move(dup), event_id);
  if (status != ZX_OK) {
    auto after = zx::clock::get_monotonic();
    FX_LOGS(ERROR) << "Failed to import display controller event. Waited "
                   << (after - before).to_msecs() << "msecs. Error code: " << status;
    return {.value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  }
  return event_id;
}

bool IsCaptureSupported(const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator) {
  fuchsia::hardware::display::Coordinator_IsCaptureSupported_Result capture_supported_result;
  auto status = display_coordinator->IsCaptureSupported(&capture_supported_result);

  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "IsCaptureSupported status failure: " << status;
    return false;
  }

  if (!capture_supported_result.is_response()) {
    FX_LOGS(ERROR) << "IsCaptureSupported did not return a valid response.";
    return false;
  }

  return capture_supported_result.response().supported;
}

zx_status_t ImportImageForCapture(
    const fuchsia::hardware::display::CoordinatorSyncPtr& display_coordinator,
    const fuchsia::hardware::display::types::ImageConfig& image_config,
    allocation::GlobalBufferCollectionId buffer_collection_id, uint32_t vmo_idx,
    allocation::GlobalImageId image_id) {
  if (buffer_collection_id == 0) {
    FX_LOGS(ERROR) << "Buffer collection id is 0.";
    return 0;
  }

  if (image_config.type != fuchsia::hardware::display::types::TYPE_CAPTURE) {
    FX_LOGS(ERROR) << "Image config type must be TYPE_CAPTURE.";
    return 0;
  }

  const fuchsia::hardware::display::BufferCollectionId display_buffer_collection_id =
      allocation::ToDisplayBufferCollectionId(buffer_collection_id);
  const fuchsia::hardware::display::ImageId fidl_image_id = allocation::ToFidlImageId(image_id);
  fuchsia::hardware::display::Coordinator_ImportImage_Result import_result;
  const zx_status_t status =
      display_coordinator->ImportImage(image_config, /*buffer_id=*/
                                       {
                                           .buffer_collection_id = display_buffer_collection_id,
                                           .buffer_index = vmo_idx,
                                       },
                                       fidl_image_id, &import_result);

  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "FIDL transport error, status: " << status;
    return status;
  }
  if (import_result.is_err()) {
    FX_LOGS(ERROR) << "FIDL server error response: " << zx_status_get_string(import_result.err());
    return import_result.err();
  }
  return ZX_OK;
}

}  // namespace scenic_impl
