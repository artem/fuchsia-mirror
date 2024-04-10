// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/image-tiling-type.h"

namespace display {

namespace {

constexpr ImageMetadata kSmallDisplay({
    .width = 800,
    .height = 600,
    .tiling_type = kImageTilingTypeLinear,
});

constexpr ImageMetadata kSmallDisplay2({
    .width = 800,
    .height = 600,
    .tiling_type = kImageTilingTypeLinear,
});

constexpr ImageMetadata kSmallCaptured({
    .width = 800,
    .height = 600,
    .tiling_type = kImageTilingTypeCapture,
});

TEST(ImageMetadataTest, EqualityIsReflexive) {
  EXPECT_EQ(kSmallDisplay, kSmallDisplay);
  EXPECT_EQ(kSmallDisplay2, kSmallDisplay2);
  EXPECT_EQ(kSmallCaptured, kSmallCaptured);
}

TEST(ImageMetadataTest, EqualityIsSymmetric) {
  EXPECT_EQ(kSmallDisplay, kSmallDisplay2);
  EXPECT_EQ(kSmallDisplay2, kSmallDisplay);
}

TEST(ImageMetadataTest, EqualityForDifferentWidths) {
  static constexpr ImageMetadata kSmallSquareDisplay({
      .width = 600,
      .height = 600,
      .tiling_type = kImageTilingTypeLinear,
  });
  EXPECT_NE(kSmallDisplay, kSmallSquareDisplay);
  EXPECT_NE(kSmallSquareDisplay, kSmallDisplay);
}

TEST(ImageMetadataTest, EqualityForDifferentHeights) {
  static constexpr ImageMetadata kLargeSquareDisplay({
      .width = 800,
      .height = 800,
      .tiling_type = kImageTilingTypeLinear,
  });
  EXPECT_NE(kSmallDisplay, kLargeSquareDisplay);
  EXPECT_NE(kLargeSquareDisplay, kSmallDisplay);
}

TEST(ImageMetadataTest, EqualityForDifferentTilingTypes) {
  EXPECT_NE(kSmallDisplay, kSmallCaptured);
  EXPECT_NE(kSmallCaptured, kSmallDisplay);
}

TEST(ImageMetadataTest, FromDesignatedInitializer) {
  static constexpr ImageMetadata image_metadata({
      .width = 640,
      .height = 480,
      .tiling_type = kImageTilingTypeCapture,
  });
  EXPECT_EQ(640, image_metadata.width());
  EXPECT_EQ(480, image_metadata.height());
  EXPECT_EQ(kImageTilingTypeCapture, image_metadata.tiling_type());
}

TEST(ImageMetadataTest, FromFidlImageMetadata) {
  static constexpr fuchsia_hardware_display_types::wire::ImageMetadata fidl_image_metadata = {
      .width = 640,
      .height = 480,
      .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
  };

  static constexpr ImageMetadata image_metadata(fidl_image_metadata);
  EXPECT_EQ(640, image_metadata.width());
  EXPECT_EQ(480, image_metadata.height());
  EXPECT_EQ(kImageTilingTypeCapture, image_metadata.tiling_type());
}

TEST(ImageMetadataTest, FromBanjoImageMetadata) {
  static constexpr image_metadata_t banjo_image_metadata = {
      .width = 640,
      .height = 480,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };

  static constexpr ImageMetadata image_metadata(banjo_image_metadata);
  EXPECT_EQ(640, image_metadata.width());
  EXPECT_EQ(480, image_metadata.height());
  EXPECT_EQ(kImageTilingTypeCapture, image_metadata.tiling_type());
}

TEST(ImageMetadataTest, ToFidlImageMetadata) {
  static constexpr ImageMetadata image_metadata({
      .width = 640,
      .height = 480,
      .tiling_type = kImageTilingTypeCapture,
  });

  static constexpr fuchsia_hardware_display_types::wire::ImageMetadata fidl_image_metadata =
      image_metadata.ToFidl();
  EXPECT_EQ(640u, fidl_image_metadata.width);
  EXPECT_EQ(480u, fidl_image_metadata.height);
  EXPECT_EQ(fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
            fidl_image_metadata.tiling_type);
}

TEST(ImageMetadataTest, ToBanjoImageMetadata) {
  static constexpr ImageMetadata image_metadata({
      .width = 640,
      .height = 480,
      .tiling_type = kImageTilingTypeCapture,
  });

  static constexpr image_metadata_t banjo_image_metadata = image_metadata.ToBanjo();
  EXPECT_EQ(640u, banjo_image_metadata.width);
  EXPECT_EQ(480u, banjo_image_metadata.height);
  EXPECT_EQ(IMAGE_TILING_TYPE_CAPTURE, banjo_image_metadata.tiling_type);
}

TEST(ImageMetadataTest, IsValidFidlSmallDisplay) {
  EXPECT_TRUE(ImageMetadata::IsValid(fuchsia_hardware_display_types::wire::ImageMetadata{
      .width = 640,
      .height = 480,
      .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
  }));
}

TEST(ImageMetadataTest, IsValidFidlLargeWidth) {
  EXPECT_FALSE(ImageMetadata::IsValid(fuchsia_hardware_display_types::wire::ImageMetadata{
      .width = 1'000'000,
      .height = 480,
      .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
  }));
}

TEST(ImageMetadataTest, IsValidFidlLargeHeight) {
  EXPECT_FALSE(ImageMetadata::IsValid(fuchsia_hardware_display_types::wire::ImageMetadata{
      .width = 640,
      .height = 1'000'000,
      .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeCapture,
  }));
}

TEST(ImageMetadataTest, IsValidBanjoSmallDisplay) {
  EXPECT_TRUE(ImageMetadata::IsValid(image_metadata_t{
      .width = 640,
      .height = 480,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  }));
}

TEST(ImageMetadataTest, IsValidBanjoLargeWidth) {
  EXPECT_FALSE(ImageMetadata::IsValid(image_metadata_t{
      .width = 1'000'000,
      .height = 480,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  }));
}

TEST(ImageMetadataTest, IsValidBanjoLargeHeight) {
  EXPECT_FALSE(ImageMetadata::IsValid(image_metadata_t{
      .width = 640,
      .height = 1'000'000,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  }));
}

}  // namespace

}  // namespace display
