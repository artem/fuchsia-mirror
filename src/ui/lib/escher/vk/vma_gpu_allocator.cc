// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/escher/vk/vma_gpu_allocator.h"

#include "src/ui/lib/escher/impl/vulkan_utils.h"
#include "src/ui/lib/escher/util/image_utils.h"

#include <vulkan/vulkan.hpp>

namespace {

class VmaGpuMem : public escher::GpuMem {
 public:
  VmaGpuMem(VmaAllocator allocator, VmaAllocation allocation, VmaAllocationInfo info)
      : GpuMem(info.deviceMemory, info.size, info.offset, static_cast<uint8_t*>(info.pMappedData)),
        allocator_(allocator),
        allocation_(allocation) {}

  ~VmaGpuMem() { vmaFreeMemory(allocator_, allocation_); }

 private:
  VmaAllocator allocator_;
  VmaAllocation allocation_;
};

class VmaBuffer : public escher::Buffer {
 public:
  VmaBuffer(escher::ResourceManager* manager, VmaAllocator allocator, VmaAllocation allocation,
            VmaAllocationInfo info, vk::DeviceSize vk_buffer_size, vk::Buffer buffer)
      : Buffer(manager, buffer, vk_buffer_size, static_cast<uint8_t*>(info.pMappedData)),
        allocator_(allocator),
        allocation_(allocation) {}

  ~VmaBuffer() { vmaDestroyBuffer(allocator_, vk(), allocation_); }

 private:
  VmaAllocator allocator_;
  VmaAllocation allocation_;
};

// Vma objects (i.e., buffers, images) with mapped memory are cleaned up by
// destroying the original object, not by destroying a separate memory
// allocation object. However, we can request mapped pointers from vma objects.
// Therefore, we implement an 'out_mem' GpuMem object by keeping a strong
// reference to the original vma object, and wrapping only the mapped pointer,
// offset, and size parameters.
class VmaMappedGpuMem : public escher::GpuMem {
 public:
  VmaMappedGpuMem(VmaAllocationInfo info, const fxl::RefPtr<escher::Resource>& keep_alive)
      : GpuMem(info.deviceMemory, info.size, info.offset, static_cast<uint8_t*>(info.pMappedData)),
        keep_alive_(keep_alive) {}

 private:
  const fxl::RefPtr<escher::Resource> keep_alive_;
};

class VmaImage : public escher::Image {
 public:
  VmaImage(escher::ResourceManager* manager, escher::ImageInfo image_info, vk::Image image,
           VmaAllocator allocator, VmaAllocation allocation, VmaAllocationInfo allocation_info,
           vk::ImageLayout initial_layout)
      : Image(manager, image_info, image, allocation_info.size,
              static_cast<uint8_t*>(allocation_info.pMappedData), initial_layout),
        allocator_(allocator),
        allocation_(allocation) {}

  ~VmaImage() { vmaDestroyImage(allocator_, vk(), allocation_); }

  // |Image|
  vk::DeviceSize GetDeviceMemoryCommitment() override {
    if (is_transient()) {
      VmaAllocationInfo info;
      vmaGetAllocationInfo(allocator_, allocation_, &info);
      return vk_device().getMemoryCommitment(vk::DeviceMemory(info.deviceMemory));
    } else {
      return size();
    }
  }

 private:
  VmaAllocator allocator_;
  VmaAllocation allocation_;
};

}  // namespace

namespace escher {

VmaGpuAllocator::VmaGpuAllocator(const VulkanContext& context)
    : physical_device_(context.physical_device) {
  FX_DCHECK(context.instance);
  FX_DCHECK(context.device);
  FX_DCHECK(context.physical_device);
  VmaAllocatorCreateInfo allocatorInfo = {};
  allocatorInfo.instance = context.instance;
  allocatorInfo.physicalDevice = context.physical_device;
  allocatorInfo.device = context.device;
  allocatorInfo.flags = VMA_ALLOCATOR_CREATE_KHR_DEDICATED_ALLOCATION_BIT;
  // This number was tuned for Skia on Android, but might be a good starting point for us. The
  // allocator starts making blocks at 1/8 this size, and doubles until capping out at this value.
  allocatorInfo.preferredLargeHeapBlockSize = 4 * 1024 * 1024;
  vmaCreateAllocator(&allocatorInfo, &allocator_);
}

VmaGpuAllocator::~VmaGpuAllocator() { vmaDestroyAllocator(allocator_); }

GpuMemPtr VmaGpuAllocator::AllocateMemory(vk::MemoryRequirements reqs,
                                          vk::MemoryPropertyFlags flags) {
  // Needed so we have a pointer to the C-style type.
  VkMemoryRequirements c_reqs = reqs;

  // VMA specific allocation parameters.
  VmaAllocationCreateInfo create_info = {VMA_ALLOCATION_CREATE_MAPPED_BIT,
                                         VMA_MEMORY_USAGE_UNKNOWN,
                                         static_cast<VkMemoryPropertyFlags>(flags),
                                         0u,
                                         0u,
                                         VK_NULL_HANDLE,
                                         nullptr};

  // Output structs.
  VmaAllocation allocation;
  VmaAllocationInfo allocation_info;
  auto status = vmaAllocateMemory(allocator_, &c_reqs, &create_info, &allocation, &allocation_info);

  FX_DCHECK(status == VK_SUCCESS) << "vmaAllocateMemory failed with status code " << status;
  if (status != VK_SUCCESS)
    return nullptr;

  return fxl::AdoptRef(new VmaGpuMem(allocator_, allocation, allocation_info));
}

BufferPtr VmaGpuAllocator::AllocateBuffer(ResourceManager* manager, vk::DeviceSize size,
                                          vk::BufferUsageFlags usage_flags,
                                          vk::MemoryPropertyFlags memory_property_flags,
                                          GpuMemPtr* out_ptr) {
  vk::BufferCreateInfo info;
  info.size = size;
  info.usage = usage_flags;
  info.sharingMode = vk::SharingMode::eExclusive;

  // Needed so we can have a pointer to the C-style type.
  VkBufferCreateInfo c_buffer_info = info;

  VmaAllocationCreateInfo create_info = {VMA_ALLOCATION_CREATE_MAPPED_BIT,
                                         VMA_MEMORY_USAGE_UNKNOWN,
                                         static_cast<VkMemoryPropertyFlags>(memory_property_flags),
                                         0u,
                                         0u,
                                         VK_NULL_HANDLE,
                                         nullptr};

  if (out_ptr) {
    create_info.flags |= VMA_ALLOCATION_CREATE_DEDICATED_MEMORY_BIT;
  }

  // Output structs.
  VkBuffer buffer;
  VmaAllocation allocation;
  VmaAllocationInfo allocation_info;
  auto status = vmaCreateBuffer(allocator_, &c_buffer_info, &create_info, &buffer, &allocation,
                                &allocation_info);

  FX_DCHECK(status == VK_SUCCESS) << "vmaAllocateMemory failed with status code " << status;
  if (status != VK_SUCCESS)
    return nullptr;

  FX_DCHECK(info.size >= size) << "Size of allocated memory should not be less than requested size";

  auto retval =
      fxl::AdoptRef(new VmaBuffer(manager, allocator_, allocation, allocation_info, size, buffer));

  if (out_ptr) {
    FX_DCHECK(allocation_info.offset == 0);
    *out_ptr = fxl::AdoptRef(new VmaMappedGpuMem(allocation_info, retval));
  }

  return retval;
}

ImagePtr VmaGpuAllocator::AllocateImage(ResourceManager* manager, const ImageInfo& info,
                                        GpuMemPtr* out_ptr) {
  constexpr vk::ImageLayout kInitialLayout = vk::ImageLayout::eUndefined;

  // Needed so we have a pointer to the C-style type.
  VkImageCreateInfo c_image_info = image_utils::CreateVkImageCreateInfo(info, kInitialLayout);

  // Check if the image create info above is valid.
  if (!impl::CheckImageCreateInfoValidity(physical_device_, c_image_info)) {
    FX_LOGS(ERROR) << "VmaGpuAllocator::AllocateImage(): ImageCreateInfo invalid. Create failed.";
    return ImagePtr();
  }

  VmaAllocationCreateInfo create_info = {/*creation flags filled in below*/ 0U,
                                         VMA_MEMORY_USAGE_UNKNOWN,
                                         static_cast<VkMemoryPropertyFlags>(info.memory_flags),
                                         0u,
                                         GetMemoryTypeBitsMask(info),
                                         VK_NULL_HANDLE,
                                         nullptr};

  // Create dedicated memory to reduce memory footprint of protected memory allocations, see
  // https://fxbug.dev/36620 for motivation.
  if (out_ptr || c_image_info.flags & VK_IMAGE_CREATE_PROTECTED_BIT) {
    create_info.flags |= VMA_ALLOCATION_CREATE_DEDICATED_MEMORY_BIT;
  }

  if (c_image_info.usage & VK_IMAGE_USAGE_TRANSIENT_ATTACHMENT_BIT) {
    create_info.usage = VMA_MEMORY_USAGE_GPU_LAZILY_ALLOCATED;
  } else {
    // In all other cases (i.e. not using lazy/transient images), we assume that the
    // image should be mappable to host memory.
    create_info.flags |= VMA_ALLOCATION_CREATE_MAPPED_BIT;
  }

  // Output structs.
  VkImage image;
  VmaAllocation allocation;
  VmaAllocationInfo allocation_info;
  if (!CreateImage(c_image_info, create_info, &image, &allocation, &allocation_info)) {
    return nullptr;
  }

  auto retval = fxl::AdoptRef(
      new VmaImage(manager, info, image, allocator_, allocation, allocation_info, kInitialLayout));

  if (out_ptr) {
    FX_DCHECK(allocation_info.offset == 0);
    *out_ptr = fxl::AdoptRef(new VmaMappedGpuMem(allocation_info, retval));
  }

  return retval;
}

size_t VmaGpuAllocator::GetTotalBytesAllocated() const {
  VmaStats stats;
  vmaCalculateStats(allocator_, &stats);
  return stats.total.usedBytes;
}

size_t VmaGpuAllocator::GetUnusedBytesAllocated() const {
  VmaStats stats;
  vmaCalculateStats(allocator_, &stats);
  return stats.total.unusedBytes;
}

bool VmaGpuAllocator::CreateImage(const VkImageCreateInfo& image_create_info,
                                  const VmaAllocationCreateInfo& allocation_create_info,
                                  VkImage* image, VmaAllocation* vma_allocation,
                                  VmaAllocationInfo* vma_allocation_info) {
  auto status = vmaCreateImage(allocator_, &image_create_info, &allocation_create_info, image,
                               vma_allocation, vma_allocation_info);
#if !defined(NDEBUG)
  if (status != VK_SUCCESS) {
    // Copy into vulkan.hpp struct for more convenient printing.
    vk::ImageCreateInfo ici(image_create_info);
    FX_DCHECK(status == VK_SUCCESS)
        << "vmaAllocateMemory failed with status code " << status  //
        << "\n\tflags:" << vk::to_string(ici.flags) << " mem-flags:"
        << vk::to_string(vk::MemoryPropertyFlags(allocation_create_info.requiredFlags))  //
        << " image-type:" << vk::to_string(ici.imageType)                                //
        << " format:" << vk::to_string(ici.format)                                       //
        << " usage:" << vk::to_string(ici.usage);
  }
#endif  // !defined(NDEBUG)
  return status == VK_SUCCESS;
}

uint32_t VmaGpuAllocator::GetMemoryTypeBitsMask(const escher::ImageInfo& info) {
  uint32_t memory_type_bits = 0;
#ifdef VK_USE_PLATFORM_MACOS_MVK
  // On macOS using Metal layers for rendering, Metal requires textures using
  // multi-sampling to use only DeviceLocal memory types.
  if (info.sample_count > 1) {
    // We cache the calculated memory type bits mask to avoid duplicated
    // calculation.
    if (memory_type_bits_mask_) {
      return *memory_type_bits_mask_;
    }
    auto memory_properties = physical_device_.getMemoryProperties();
    for (uint32_t i = 0; i < memory_properties.memoryTypeCount; i++) {
      if ((memory_properties.memoryTypes[i].propertyFlags &
           vk::MemoryPropertyFlagBits::eDeviceLocal) &&
          !(memory_properties.memoryTypes[i].propertyFlags &
            vk::MemoryPropertyFlagBits::eHostVisible)) {
        memory_type_bits |= (1 << i);
      }
    }
    memory_type_bits_mask_.emplace(memory_type_bits);
  }
#endif  // VK_USE_PLATFORM_MACOS_MVK
  return memory_type_bits;
}

}  // namespace escher
