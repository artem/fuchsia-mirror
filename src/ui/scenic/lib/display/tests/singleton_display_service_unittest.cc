// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/singleton_display_service.h"

#include <fidl/fuchsia.images2/cpp/fidl.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "src/lib/fsl/handles/object_info.h"

namespace scenic_impl {
namespace display {
namespace test {

TEST(SingletonDisplayService, GetMetrics) {
  static constexpr uint32_t kWidthInPx = 777;
  static constexpr uint32_t kHeightInPx = 555;
  static constexpr uint32_t kWidthInMm = 77;
  static constexpr uint32_t kHeightInMm = 55;
  static constexpr uint32_t kRefreshRate = 44000;
  auto display = std::make_shared<Display>(
      fuchsia::hardware::display::DisplayId{.value = 1}, kWidthInPx, kHeightInPx, kWidthInMm,
      kHeightInMm, std::vector{fuchsia_images2::PixelFormat::kBgra32}, kRefreshRate);
  auto singleton = std::make_unique<SingletonDisplayService>(display);

  uint32_t width_in_px = 0;
  uint32_t height_in_px = 0;
  uint32_t width_in_mm = 0;
  uint32_t height_in_mm = 0;
  float dpr_x = 0.f;
  float dpr_y = 0.f;
  uint32_t refresh_rate = 0;

  singleton->GetMetrics([&](::fuchsia::ui::display::singleton::Metrics info) {
    ASSERT_TRUE(info.has_extent_in_px());
    width_in_px = info.extent_in_px().width;
    height_in_px = info.extent_in_px().height;
    ASSERT_TRUE(info.has_extent_in_mm());
    width_in_mm = info.extent_in_mm().width;
    height_in_mm = info.extent_in_mm().height;
    ASSERT_TRUE(info.has_recommended_device_pixel_ratio());
    dpr_x = info.recommended_device_pixel_ratio().x;
    dpr_y = info.recommended_device_pixel_ratio().y;
    ASSERT_TRUE(info.has_maximum_refresh_rate_in_millihertz());
    refresh_rate = info.maximum_refresh_rate_in_millihertz();
  });

  EXPECT_EQ(width_in_px, kWidthInPx);
  EXPECT_EQ(height_in_px, kHeightInPx);
  EXPECT_EQ(width_in_mm, kWidthInMm);
  EXPECT_EQ(height_in_mm, kHeightInMm);
  EXPECT_EQ(dpr_x, 1.f);
  EXPECT_EQ(dpr_y, 1.f);
  EXPECT_EQ(refresh_rate, kRefreshRate);
}

TEST(SingletonDisplayService, DevicePixelRatioChange) {
  auto display = std::make_shared<Display>(fuchsia::hardware::display::DisplayId{.value = 1},
                                           /*width_in_px=*/777, /*height_in_px=*/555,
                                           /*width_in_mm=*/77, /*height_in_mm=*/55,
                                           std::vector{fuchsia_images2::PixelFormat::kBgra32},
                                           /*refresh_rate=*/4400);
  auto singleton = std::make_unique<SingletonDisplayService>(display);

  const float kDPRx = 1.25f;
  const float kDPRy = 1.25f;
  display->set_device_pixel_ratio({kDPRx, kDPRy});

  float dpr_x = 0.f;
  float dpr_y = 0.f;
  singleton->GetMetrics([&](::fuchsia::ui::display::singleton::Metrics info) {
    dpr_x = info.recommended_device_pixel_ratio().x;
    dpr_y = info.recommended_device_pixel_ratio().y;
  });

  EXPECT_EQ(dpr_x, kDPRx);
  EXPECT_EQ(dpr_y, kDPRy);
}

TEST(SingletonDisplayService, GetOwnershipEvent) {
  auto display = std::make_shared<Display>(fuchsia::hardware::display::DisplayId{.value = 1},
                                           /*width_in_px=*/777, /*height_in_px=*/555);
  auto singleton = std::make_unique<SingletonDisplayService>(display);

  std::optional<zx::event> event;
  singleton->GetEvent([&](zx::event e) { event = std::move(e); });
  EXPECT_EQ(fsl::GetKoid(event->get()), fsl::GetKoid(display->ownership_event().get()));
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
