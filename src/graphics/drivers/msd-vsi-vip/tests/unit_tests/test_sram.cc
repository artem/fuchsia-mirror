// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/magma_service/test_util/platform_device_helper.h>

#include <gtest/gtest.h>

TEST(Sram, AxiSramSize) {
  magma::PlatformDevice* platform_device = TestPlatformDevice::GetInstance();
  ASSERT_TRUE(platform_device);

  uint32_t mmio_count = platform_device->GetMmioCount();
  ASSERT_GE(mmio_count, 4u);
  if (mmio_count < 5) {
    // Some devices do not have an AXI SRAM.
    GTEST_SKIP();
  }
  const uint32_t kAxiSramMmioIndex = 4;
  auto buffer = platform_device->GetMmioBuffer(kAxiSramMmioIndex);
  ASSERT_TRUE(buffer);

  EXPECT_EQ(buffer->size(), 0x100000u);

  // Write below crashes if left uncached
  EXPECT_TRUE(buffer->SetCachePolicy(MAGMA_CACHE_POLICY_WRITE_COMBINING));

  void* ptr;
  ASSERT_TRUE(buffer->MapCpu(&ptr));
  memset(ptr, 0, buffer->size());
  EXPECT_TRUE(buffer->UnmapCpu());
}
