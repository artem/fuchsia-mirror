// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCREENSHOT_SCREENSHOT_MANAGER_H_
#define SRC_UI_SCENIC_LIB_SCREENSHOT_SCREENSHOT_MANAGER_H_

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/flatland/engine/engine.h"
#include "src/ui/scenic/lib/flatland/renderer/renderer.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture.h"
#include "src/ui/scenic/lib/screenshot/flatland_screenshot.h"

using GetRenderables = std::function<flatland::Renderables()>;

namespace screenshot {

class ScreenshotManager {
 public:
  ScreenshotManager(std::shared_ptr<allocation::Allocator> allocator_,
                    std::shared_ptr<flatland::Renderer> renderer, GetRenderables get_renderables,
                    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>>
                        buffer_collection_importers,
                    fuchsia::math::SizeU display_size, int display_rotation);
  ~ScreenshotManager() = default;

  void CreateBinding(fidl::InterfaceRequest<fuchsia::ui::composition::Screenshot> request);

 private:
  // We need these for rendering the scene into the client supplied buffer.
  std::shared_ptr<allocation::Allocator> allocator_;
  std::shared_ptr<flatland::Renderer> renderer_;
  GetRenderables get_renderables_;
  std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> buffer_collection_importers_;

  fuchsia::math::SizeU display_size_;

  // Angle in degrees by which the display is rotated.
  int display_rotation_ = 0;

  fidl::BindingSet<fuchsia::ui::composition::Screenshot,
                   std::unique_ptr<fuchsia::ui::composition::Screenshot>>
      bindings_;
};

}  // namespace screenshot

#endif  // SRC_UI_SCENIC_LIB_SCREENSHOT_SCREENSHOT_MANAGER_H_
