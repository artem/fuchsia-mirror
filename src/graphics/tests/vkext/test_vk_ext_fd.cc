// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <array>

#include <gtest/gtest.h>

#include "src/graphics/tests/common/vulkan_context.h"

// Test the vulkan semaphore external fd extension.
class TestVkExtFd : public testing::Test {
 public:
  void SetUp() override {
    auto app_info =
        vk::ApplicationInfo().setPApplicationName("test").setApplicationVersion(VK_API_VERSION_1_1);
    auto instance_info = vk::InstanceCreateInfo().setPApplicationInfo(&app_info);

    std::array<const char*, 1> device_extensions{VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME};

    auto builder = VulkanContext::Builder();
    builder.set_instance_info(instance_info).set_validation_layers_enabled(false);
    builder.set_device_info(builder.DeviceInfo().setPEnabledExtensionNames(device_extensions));

    context_ = builder.Unique();

    loader_.init(*context_->instance(), vkGetInstanceProcAddr);
  }

  std::unique_ptr<VulkanContext> context_;
  vk::DispatchLoaderDynamic loader_;
};

TEST_F(TestVkExtFd, ExportThenImport) {
  auto create_info = vk::SemaphoreCreateInfo();
  auto ret = context_->device()->createSemaphore(create_info);
  ASSERT_EQ(vk::Result::eSuccess, ret.result);
  vk::Semaphore& sem_export = ret.value;

  int fd;

  {
    auto semaphore_get_info =
        vk::SemaphoreGetFdInfoKHR()
            .setSemaphore(sem_export)
            .setHandleType(vk::ExternalSemaphoreHandleTypeFlagBits::eOpaqueFd);
    auto export_ret = context_->device()->getSemaphoreFdKHR(semaphore_get_info, loader_);
    ASSERT_EQ(vk::Result::eSuccess, export_ret.result);
    fd = export_ret.value;
  }

  // TODO(fxbug.dev/67565) - check non negative
  EXPECT_NE(0, fd);

  {
    auto ret = context_->device()->createSemaphore(create_info);
    ASSERT_EQ(vk::Result::eSuccess, ret.result);
    vk::Semaphore& sem_import = ret.value;

    auto semaphore_import_info =
        vk::ImportSemaphoreFdInfoKHR()
            .setSemaphore(sem_import)
            .setHandleType(vk::ExternalSemaphoreHandleTypeFlagBits::eOpaqueFd)
            .setFd(fd);
    auto import_ret = context_->device()->importSemaphoreFdKHR(semaphore_import_info, loader_);
    ASSERT_EQ(vk::Result::eSuccess, import_ret);
  }
}
