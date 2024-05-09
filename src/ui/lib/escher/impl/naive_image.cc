// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/escher/impl/naive_image.h"

#include "src/ui/lib/escher/impl/vulkan_utils.h"
#include "src/ui/lib/escher/resources/resource_manager.h"
#include "src/ui/lib/escher/util/trace_macros.h"
#include "src/ui/lib/escher/vk/gpu_mem.h"

#include <vulkan/vulkan.hpp>

namespace escher {
namespace impl {

ImagePtr NaiveImage::AdoptVkImage(ResourceManager* image_owner, ImageInfo info, vk::Image vk_image,
                                  GpuMemPtr mem, vk::ImageLayout initial_layout) {
  TRACE_DURATION("gfx", "escher::NaiveImage::AdoptImage (from VkImage)");
  FX_CHECK(vk_image);
  FX_CHECK(mem);

  // Check image memory requirements before binding the image to memory.
  auto mem_requirements = image_owner->vk_device().getImageMemoryRequirements(vk_image);

  auto size_required = mem_requirements.size;
  auto alignment_required = mem_requirements.alignment;

  if (!mem->base()) {
    FX_LOGS(ERROR) << "AdoptVkImage failed: Memory has null handle.";
    return nullptr;
  }

  if (mem->size() < size_required) {
    FX_LOGS(ERROR) << "AdoptVkImage failed: Image requires " << size_required
                   << " bytes of memory, while the provided mem size is " << mem->size()
                   << " bytes.";
    return nullptr;
  }

  if (mem->offset() % alignment_required != 0) {
    FX_LOGS(ERROR) << "Memory requirements check failed: Buffer requires alignment of "
                   << alignment_required << " bytes, while the provided mem offset is "
                   << mem->offset();
    return nullptr;
  }

  auto bind_result = image_owner->vk_device().bindImageMemory(vk_image, mem->base(), mem->offset());
  if (bind_result != vk::Result::eSuccess) {
    FX_DLOGS(ERROR) << "vkBindImageMemory failed: " << vk::to_string(bind_result);
    return nullptr;
  }

  return fxl::AdoptRef(new NaiveImage(image_owner, info, vk_image, mem, initial_layout));
}

NaiveImage::NaiveImage(ResourceManager* image_owner, ImageInfo info, vk::Image image, GpuMemPtr mem,
                       vk::ImageLayout initial_layout)
    : Image(image_owner, info, image, mem->size(), mem->mapped_ptr(), initial_layout),
      mem_(std::move(mem)) {
  FX_CHECK(info.is_transient() ==
           static_cast<bool>(info.memory_flags & vk::MemoryPropertyFlagBits::eLazilyAllocated));
}

NaiveImage::~NaiveImage() { vulkan_context().device.destroyImage(vk()); }

vk::DeviceSize NaiveImage::GetDeviceMemoryCommitment() {
  if (is_transient()) {
    return vk_device().getMemoryCommitment(mem_->base());
  } else {
    return size();
  }
}

}  // namespace impl
}  // namespace escher
