// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_ALLOCATION_MOCK_BUFFER_COLLECTION_IMPORTER_H_
#define SRC_UI_SCENIC_LIB_ALLOCATION_MOCK_BUFFER_COLLECTION_IMPORTER_H_

#include <gmock/gmock.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"

namespace allocation {

// Mock class of BufferCollectionImporter for API testing.
class MockBufferCollectionImporter : public BufferCollectionImporter {
 public:
  MOCK_METHOD(bool, ImportBufferCollection,
              (GlobalBufferCollectionId, fuchsia::sysmem2::Allocator_Sync*,
               fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>,
               allocation::BufferCollectionUsage, std::optional<fuchsia::math::SizeU>));

  MOCK_METHOD(void, ReleaseBufferCollection,
              (GlobalBufferCollectionId, allocation::BufferCollectionUsage));

  MOCK_METHOD(bool, ImportBufferImage, (const ImageMetadata&, allocation::BufferCollectionUsage));

  MOCK_METHOD(void, ReleaseBufferImage, (GlobalImageId));
};

}  // namespace allocation

#endif  // SRC_UI_SCENIC_LIB_ALLOCATION_MOCK_BUFFER_COLLECTION_IMPORTER_H_
