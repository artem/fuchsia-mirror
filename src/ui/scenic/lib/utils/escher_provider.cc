// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/escher_provider.h"

#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/pseudo_file.h>

#include <sstream>

#include <vulkan/vulkan.h>

#include "src/ui/lib/escher/fs/hack_filesystem.h"
#include "src/ui/lib/escher/hmd/pose_buffer_latching_shader.h"
#include "src/ui/lib/escher/paper/paper_renderer_static_config.h"
#include "src/ui/lib/escher/vk/vulkan_device_queues.h"
#include "src/ui/lib/escher/vk/vulkan_instance.h"

namespace utils {

escher::EscherUniquePtr CreateEscher(sys::ComponentContext* app_context) {
  // Initialize Vulkan.
  constexpr bool kRequiresSurface = false;
  escher::VulkanInstance::Params instance_params(
      {{},
       {
           VK_EXT_DEBUG_UTILS_EXTENSION_NAME,
           VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME,
           VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_EXTENSION_NAME,
           VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME,
       },
       kRequiresSurface});

  // Only enable Vulkan validation layers when in debug mode.
#if defined(SCENIC_ENABLE_VULKAN_VALIDATION)
  instance_params.layer_names.insert("VK_LAYER_KHRONOS_validation");
#endif
  instance_params.initial_debug_utils_message_callbacks.push_back(HandleDebugUtilsMessage);
  auto vulkan_instance = escher::VulkanInstance::New(std::move(instance_params));
  if (!vulkan_instance)
    return nullptr;

  // Tell Escher not to filter out queues that don't support presentation.
  // The display manager only supports a single connection, so none of the
  // available queues will support presentation.  This is OK, because we use
  // the display manager API to present frames directly, instead of using
  // Vulkan swapchains.
  escher::VulkanDeviceQueues::Params device_queues_params(
      {{
           VK_EXT_QUEUE_FAMILY_FOREIGN_EXTENSION_NAME,
           VK_FUCHSIA_BUFFER_COLLECTION_EXTENSION_NAME,
           VK_FUCHSIA_EXTERNAL_MEMORY_EXTENSION_NAME,
           VK_FUCHSIA_EXTERNAL_SEMAPHORE_EXTENSION_NAME,
           VK_KHR_BIND_MEMORY_2_EXTENSION_NAME,
           VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME,
           VK_KHR_EXTERNAL_SEMAPHORE_EXTENSION_NAME,
           VK_KHR_GET_MEMORY_REQUIREMENTS_2_EXTENSION_NAME,
           VK_KHR_MAINTENANCE1_EXTENSION_NAME,
       },
       {
           VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME,
           VK_KHR_SAMPLER_YCBCR_CONVERSION_EXTENSION_NAME,
       },
       vk::SurfaceKHR(),
       escher::VulkanDeviceQueues::Params::kDisableQueueFilteringForPresent |
           escher::VulkanDeviceQueues::Params::kAllowProtectedMemory});

  auto vulkan_device_queues =
      escher::VulkanDeviceQueues::New(vulkan_instance, device_queues_params);
  if (!vulkan_device_queues) {
    return nullptr;
  }

  // Provide a PseudoDir where the gfx system can register debugging services.
  auto debug_dir = std::make_shared<vfs::PseudoDir>();
  app_context->outgoing()->debug_dir()->AddSharedEntry("gfx", debug_dir);

  auto shader_fs = escher::HackFilesystem::New(debug_dir);
  {
#if ESCHER_USE_RUNTIME_GLSL
    auto paths = escher::kPaperRendererShaderPaths;
    paths.insert(paths.end(), escher::hmd::kPoseBufferLatchingPaths.begin(),
                 escher::hmd::kPoseBufferLatchingPaths.end());
    bool success = shader_fs->InitializeWithRealFiles(paths);
#else
    auto paths = escher::kPaperRendererShaderSpirvPaths;
    paths.insert(paths.end(), escher::hmd::kPoseBufferLatchingSpirvPaths.begin(),
                 escher::hmd::kPoseBufferLatchingSpirvPaths.end());
    bool success = shader_fs->InitializeWithRealFiles(paths);
#endif
    FX_DCHECK(success) << "Failed to init shader files.";
  }

  // Initialize Escher.
#if ESCHER_USE_RUNTIME_GLSL
  escher::GlslangInitializeProcess();
#endif
  return escher::EscherUniquePtr(
      new escher::Escher(vulkan_device_queues, std::move(shader_fs), /*gpu_allocator*/ nullptr),
      // Custom deleter.
      // The vulkan instance is a stack variable, but it is a
      // fxl::RefPtr, so we can store by value.
      [=](escher::Escher* escher) {
#if ESCHER_USE_RUNTIME_GLSL
        escher::GlslangFinalizeProcess();
#endif
        delete escher;
      });
}

VkBool32 HandleDebugUtilsMessage(VkDebugUtilsMessageSeverityFlagBitsEXT message_severity,
                                 VkDebugUtilsMessageTypeFlagsEXT message_types,
                                 const VkDebugUtilsMessengerCallbackDataEXT* callback_data,
                                 void* user_data) {
  vk::DebugUtilsMessageSeverityFlagsEXT severity(message_severity);
  vk::ObjectType object_type = callback_data->objectCount == 0
                                   ? vk::ObjectType::eUnknown
                                   : vk::ObjectType(callback_data->pObjects[0].objectType);
  uint64_t object_handle =
      callback_data->objectCount == 0 ? 0ull : callback_data->pObjects[0].objectHandle;
  int32_t message_id = callback_data->messageIdNumber;

  std::string message = (std::ostringstream()
                         << callback_data->pMessage << " (layer: " << callback_data->pMessageIdName
                         << "  id: " << message_id << "  object-type: "
                         << vk::to_string(object_type) << "  object: " << object_handle << ")")
                            .str();

  bool fatal = false;
  if (severity == vk::DebugUtilsMessageSeverityFlagBitsEXT::eInfo) {
    FX_LOGS(INFO) << "## Vulkan Information: " << message;
  } else if (severity == vk::DebugUtilsMessageSeverityFlagBitsEXT::eWarning) {
    if (message_types & VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT) {
      FX_LOGS(WARNING) << "## Vulkan Performance Warning: " << message;
    } else {
      FX_LOGS(WARNING) << "## Vulkan Warning: " << message;
    }
  } else if (severity == vk::DebugUtilsMessageSeverityFlagBitsEXT::eError) {
    // Treat all errors as fatal.
    fatal = true;
    FX_LOGS(ERROR) << "## Vulkan Error: " << message;
  } else if (severity == vk::DebugUtilsMessageSeverityFlagBitsEXT::eVerbose) {
    FX_LOGS(INFO) << "## Vulkan Debug: " << message;
  } else {
    // This should never happen, unless a new value has been added to
    // vk::DebugUtilsMessageSeverityFlagBitsEXT.  In that case, add a new if-clause above.
    fatal = true;
    FX_LOGS(ERROR) << "## Vulkan Unknown Message Severity (flags: " << vk::to_string(severity)
                   << "): ";
  }

  // Crash immediately on fatal errors.
  FX_CHECK(!fatal);

  return false;
}

}  // namespace utils
