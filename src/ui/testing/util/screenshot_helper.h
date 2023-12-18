// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_TESTING_UTIL_SCREENSHOT_HELPER_H_
#define SRC_UI_TESTING_UTIL_SCREENSHOT_HELPER_H_

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>

#include <map>
#include <vector>

#include "src/ui/scenic/lib/utils/pixel.h"

namespace ui_testing {

using Pixel = utils::Pixel;

// Helper class to get information about a screenshot returned by
// |fuchsia.ui.composition.Screenshot| protocol.
class Screenshot {
 public:
  // Params:-
  // |screenshot_vmo| - The VMO returned by |fuchsia.ui.composition.Screenshot.Take| representing
  //                    the screenshot data in BGRA.
  // |width|, |height| - Width and height of the physical display in pixels as
  //                     returned by |fuchsia.ui.display.singleton.Info|.
  // |rotation| - The display rotation value in degrees. The width and the height of the screenshot
  //              are flipped if this value is 90 or 270 degrees as the screenshot shows how content
  //              is seen by the user.
  Screenshot(const zx::vmo& screenshot_vmo, uint64_t width, uint64_t height, int rotation);

  // Returns the |Pixel| located at (x,y) coordinates. |x| and |y| should range from [0,width_) and
  // [0,height_) respectively.
  //
  //  (0,0)________________width_____________(w-1,0)
  //      |                       |         |
  //      |                       | y       |h
  //      |          x            |         |e
  //      |-----------------------X         |i
  //      |                                 |g
  //      |                                 |h
  //      |                                 |t
  //      |_________________________________|
  //(0,h-1)           screenshot             (w-1,h-1)
  //
  // Clients should only use this function to get the pixel data.
  Pixel GetPixelAt(uint64_t x, uint64_t y) const;

  // Counts the frequencies of each color in a screenshot.
  std::map<Pixel, uint32_t> Histogram() const;

  // Returns percentage of pixels that match by comparing two screenshots. Returns 0 if the sizes of
  // the screenshots do not match.
  float ComputeSimilarity(const Screenshot& other) const;

  // Returns percentage of pixels that match by comparing the histograms of two screenshots,
  // allowing for pixel movement (e.g. shift, rotation) in the image. The comparison is
  // performed by measuring the percentage of the area of the histograms that overlaps,
  // i.e. the number of pixels that are both histograms.
  // Returns 0 if the sizes of the screenshots do not match.
  float ComputeHistogramSimilarity(const Screenshot& other) const;

  // Returns a 2D vector of size |height_ * width_|. Each value in the vector corresponds to a pixel
  // in the screenshot.
  std::vector<std::vector<Pixel>> screenshot() const { return screenshot_; }

  uint64_t width() const { return width_; }

  uint64_t height() const { return height_; }

  // Dumps the screenshot as a BGRA raw file to /custom_artifacts. Returns true if it is successful.
  // Note that the custom_artifacts storage capability needs to be added to the test. See
  // https://fuchsia.dev/fuchsia-src/development/testing/components/test_runner_framework?hl=en#custom-artifacts
  // for more details.
  bool DumpToCustomArtifacts(const std::string& filename = "screenshot.bgra") const;

 private:
  // Populates |screenshot_| by converting the linear array of bytes in |screenshot_vmo| of size |4
  // * width_ * height_| to a 2D vector of |Pixel|s of size |height_ * width_|.
  // Note: Size of each pixel is 4 bytes.
  void ExtractScreenshotFromVMO(uint8_t* screenshot_vmo);

  // Returns the |Pixel|s in the |row_index| row of the screenshot.
  std::vector<Pixel> GetPixelsInRow(uint8_t* screenshot_vmo, size_t row_index);

  uint64_t width_ = 0;
  uint64_t height_ = 0;
  std::vector<std::vector<Pixel>> screenshot_;
};

}  // namespace ui_testing

#endif  // SRC_UI_TESTING_UTIL_SCREENSHOT_HELPER_H_
