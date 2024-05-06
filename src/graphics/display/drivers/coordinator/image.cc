// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/image.h"

#include <lib/ddk/debug.h>
#include <lib/trace/event.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <atomic>
#include <utility>

#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types-cpp/image-tiling-type.h"

namespace display {

Image::Image(Controller* controller, const ImageMetadata& metadata, DriverImageId driver_id,
             inspect::Node* parent_node, ClientId client_id)
    : driver_id_(driver_id), metadata_(metadata), controller_(controller), client_id_(client_id) {
  ZX_DEBUG_ASSERT(metadata.tiling_type() != kImageTilingTypeCapture);
  InitializeInspect(parent_node);
}
Image::~Image() {
  ZX_ASSERT(!std::atomic_load(&in_use_));
  ZX_ASSERT(!InDoublyLinkedList());
  controller_->ReleaseImage(driver_id_);
}

void Image::InitializeInspect(inspect::Node* parent_node) {
  if (!parent_node)
    return;
  node_ = parent_node->CreateChild(fbl::StringPrintf("image-%p", this).c_str());
  node_.CreateInt("width", metadata_.width(), &properties_);
  node_.CreateInt("height", metadata_.height(), &properties_);
  node_.CreateUint("tiling_type", metadata_.tiling_type().ValueForLogging(), &properties_);
  presenting_property_ = node_.CreateBool("presenting", false);
  retiring_property_ = node_.CreateBool("retiring", false);
}

mtx_t* Image::mtx() const { return controller_->mtx(); }

bool Image::InDoublyLinkedList() const { return doubly_linked_list_node_state_.InContainer(); }

fbl::RefPtr<Image> Image::RemoveFromDoublyLinkedList() {
  return doubly_linked_list_node_state_.RemoveFromContainer<DefaultDoublyLinkedListTraits>();
}

void Image::PrepareFences(fbl::RefPtr<FenceReference>&& wait,
                          fbl::RefPtr<FenceReference>&& retire) {
  wait_fence_ = std::move(wait);
  retire_fence_ = std::move(retire);

  if (wait_fence_) {
    zx_status_t status = wait_fence_->StartReadyWait();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to start waiting %d", status);
      // Mark the image as ready. Displaying garbage is better than hanging or crashing.
      wait_fence_ = nullptr;
    }
  }
}

bool Image::OnFenceReady(FenceReference* fence) {
  if (wait_fence_.get() == fence) {
    wait_fence_ = nullptr;
  }
  return wait_fence_ == nullptr;
}

void Image::StartPresent() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);
  TRACE_DURATION("gfx", "Image::StartPresent", "id", id.value());
  TRACE_FLOW_BEGIN("gfx", "present_image", id.value());

  presenting_ = true;
  presenting_property_.Set(true);
}

void Image::EarlyRetire() {
  // A client may re-use an image as soon as retire_fence_ fires. Set in_use_ first.
  std::atomic_store(&in_use_, false);
  if (wait_fence_) {
    wait_fence_->SetImmediateRelease(std::move(retire_fence_));
    wait_fence_ = nullptr;
  } else if (retire_fence_) {
    retire_fence_->Signal();
    retire_fence_ = nullptr;
  }
}

void Image::RetireWithFence(fbl::RefPtr<FenceReference>&& fence) {
  // Retire and acquire are not synchronized, so set in_use_ before signaling so
  // that the image can be reused as soon as the event is signaled. We don't have
  // to worry about the armed signal fence being overwritten on reuse since it is
  // on set in StartRetire, which is called under the same lock as OnRetire.
  std::atomic_store(&in_use_, false);
  if (fence) {
    fence->Signal();
  }
}

void Image::StartRetire() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);

  if (!presenting_) {
    RetireWithFence(std::move(retire_fence_));
  } else {
    retiring_ = true;
    retiring_property_.Set(true);
    armed_retire_fence_ = std::move(retire_fence_);
  }
}

void Image::OnRetire() {
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);

  presenting_ = false;
  presenting_property_.Set(false);

  if (retiring_) {
    RetireWithFence(std::move(armed_retire_fence_));
    retiring_ = false;
    retiring_property_.Set(false);
  }
}

void Image::DiscardAcquire() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);

  std::atomic_store(&in_use_, false);
}

bool Image::Acquire() { return !std::atomic_exchange(&in_use_, true); }

void Image::ResetFences() {
  if (wait_fence_) {
    wait_fence_->ResetReadyWait();
  }

  wait_fence_ = nullptr;
  armed_retire_fence_ = nullptr;
  retire_fence_ = nullptr;
}

}  // namespace display
