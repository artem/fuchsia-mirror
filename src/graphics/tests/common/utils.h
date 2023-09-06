// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_TESTS_COMMON_UTILS_H_
#define SRC_GRAPHICS_TESTS_COMMON_UTILS_H_

#include <stdio.h>

#include <string>

#include "vulkan/vulkan_core.h"

#include "vulkan/vulkan.hpp"

// Unconditionally return |err| with message.
#define RTN_MSG(err, ...)                          \
  {                                                \
    fprintf(stderr, "%s:%d ", __FILE__, __LINE__); \
    fprintf(stderr, __VA_ARGS__);                  \
    fflush(stderr);                                \
    return err;                                    \
  }                                                \
  static_assert(true, "no-op to require trailing semicolon")

// Return |err| based on |cond| with message.
#define RTN_IF_MSG(err, cond, ...)                 \
  if ((cond)) {                                    \
    fprintf(stderr, "%s:%d ", __FILE__, __LINE__); \
    fprintf(stderr, __VA_ARGS__);                  \
    return err;                                    \
  }                                                \
  static_assert(true, "no-op to require trailing semicolon")

// Log and return based on VkResult |r|.
#define RTN_IF_VK_ERR(err, r, ...)                                      \
  if (r != VK_SUCCESS) {                                                \
    fprintf(stderr, "%s:%d:\n\t(vk::Result::e%s) ", __FILE__, __LINE__, \
            vk::to_string(vk::Result(r)).c_str());                      \
    fprintf(stderr, __VA_ARGS__);                                       \
    fprintf(stderr, "\n");                                              \
    fflush(stderr);                                                     \
    return err;                                                         \
  }                                                                     \
  static_assert(true, "no-op to require trailing semicolon")

// Log and return based on vk::Result |r|.
#define RTN_IF_VKH_ERR(err, r, ...)                                                                \
  if (r != vk::Result::eSuccess) {                                                                 \
    fprintf(stderr, "%s:%d:\n\t(vk::Result::e%s) ", __FILE__, __LINE__, vk::to_string(r).c_str()); \
    fprintf(stderr, __VA_ARGS__);                                                                  \
    fprintf(stderr, "\n");                                                                         \
    fflush(stderr);                                                                                \
    return err;                                                                                    \
  }                                                                                                \
  static_assert(true, "no-op to require trailing semicolon")

enum class VulkanExtensionSupportState {
  kNotSupported,
  kSupportedInCore,
  kSupportedAsExtensionOnly,
};

// TODO(fxbug.dev/88970): Currently we can only get extension support for
// VK_KHR_timeline_semaphore. We should support checking other extensions.
VulkanExtensionSupportState GetVulkanTimelineSemaphoreSupport(uint32_t instance_api_version);

//
// DebugUtilsTestCallback will fail an EXPECT_TRUE() test if validation errors should
// not be ignored and the message severity is of type ::eError.  It directs errors to stderr
// and other severities to stdout.
//
// See test_vkcontext.cc for an example of how to send user data into the callback.
//
VKAPI_ATTR VkBool32 VKAPI_CALL
DebugUtilsTestCallback(VkDebugUtilsMessageSeverityFlagBitsEXT messageSeverity,
                       VkDebugUtilsMessageTypeFlagsEXT messageTypes,
                       const VkDebugUtilsMessengerCallbackDataEXT *pCallbackData, void *pUserData);

#endif  // SRC_GRAPHICS_TESTS_COMMON_UTILS_H_
