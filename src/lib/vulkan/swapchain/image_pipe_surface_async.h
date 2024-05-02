// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_ASYNC_H_
#define SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_ASYNC_H_

#include <fuchsia/sysmem2/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>

#include <mutex>

#include "image_pipe_surface.h"
#include "src/lib/ui/flatland-frame-scheduling/src/simple_present.h"

namespace image_pipe_swapchain {

// An implementation of ImagePipeSurface based on an async fidl Flatland.
class ImagePipeSurfaceAsync : public ImagePipeSurface {
 public:
  explicit ImagePipeSurfaceAsync(zx_handle_t view_creation_token_handle)
      : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    loop_.StartThread();
    view_creation_token_.value = zx::channel(view_creation_token_handle);
    std::vector<VkSurfaceFormatKHR> formats(
        {{VK_FORMAT_R8G8B8A8_UNORM, VK_COLORSPACE_SRGB_NONLINEAR_KHR},
         {VK_FORMAT_R8G8B8A8_SRGB, VK_COLORSPACE_SRGB_NONLINEAR_KHR},
         {VK_FORMAT_B8G8R8A8_UNORM, VK_COLORSPACE_SRGB_NONLINEAR_KHR},
         {VK_FORMAT_B8G8R8A8_SRGB, VK_COLORSPACE_SRGB_NONLINEAR_KHR}});
    supported_image_properties_ = {formats};
  }

  ~ImagePipeSurfaceAsync() override {
    async::PostTask(loop_.dispatcher(), [this] {
      // flatland_ and flatland_allocator_ are thread hostile so it must be turn down on the thread
      // that will use it.
      flatland_connection_.reset();
      flatland_allocator_ = nullptr;
      loop_.Quit();
    });
    loop_.JoinThreads();
  }

  bool Init() override;

  bool IsLost() override;
  bool CreateImage(VkDevice device, VkLayerDispatchTable* pDisp, VkFormat format,
                   VkImageUsageFlags usage, VkSwapchainCreateFlagsKHR swapchain_flags,
                   VkExtent2D extent, uint32_t image_count, const VkAllocationCallbacks* pAllocator,
                   std::vector<ImageInfo>* image_info_out) override;

  void RemoveImage(uint32_t image_id) override;

  void PresentImage(uint32_t image_id, std::vector<std::unique_ptr<PlatformEvent>> acquire_fences,
                    std::vector<std::unique_ptr<PlatformEvent>> release_fences,
                    VkQueue queue) override;

  SupportedImageProperties& GetSupportedImageProperties() override;

 private:
  class FenceSignaler {
   public:
    explicit FenceSignaler(zx::event event) : event_(std::move(event)) {}
    ~FenceSignaler() {
      if (event_)
        event_.signal(0, ZX_EVENT_SIGNALED);
    }
    const zx::event& event() { return event_; }
    void reset() { event_.reset(); }

   private:
    zx::event event_;
  };

  // Called on the async loop.
  void PresentNextImageLocked() __attribute__((requires_capability(mutex_)));
  void OnErrorLocked() __attribute__((requires_capability(mutex_)));

  async::Loop loop_;
  std::mutex mutex_;

  // Can only be accessed from the async loop's thread.
  std::unique_ptr<simple_present::FlatlandConnection> flatland_connection_;
  fuchsia::ui::composition::AllocatorPtr flatland_allocator_;
  fuchsia::ui::views::ViewCreationToken view_creation_token_;

  fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator_;

  struct PendingPresent {
    uint32_t image_id;
    std::vector<zx::event> acquire_fences;
    // These fences will automatically be signaled when they go out of scope, which could happen if
    // the Flatland channel is closed. As these events are passed to the application as acquire
    // semaphores, this prevents semaphore waits on them from hanging until the GPU driver decides
    // to time them out.
    // The only case where we don't automatically signal them is if the OnFramePresented callback is
    // run, in which case scenic is responsible for signaling them.
    std::vector<std::unique_ptr<FenceSignaler>> release_fences;
  };
  std::vector<PendingPresent> queue_ __attribute__((guarded_by(mutex_)));
  bool present_pending_ __attribute__((guarded_by(mutex_))) = false;
  std::vector<std::unique_ptr<FenceSignaler>> previous_present_release_fences_
      __attribute__((guarded_by(mutex_)));
  SupportedImageProperties supported_image_properties_;
  bool channel_closed_ __attribute__((guarded_by(mutex_))) = false;
};

}  // namespace image_pipe_swapchain

#endif  // SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_ASYNC_H_
