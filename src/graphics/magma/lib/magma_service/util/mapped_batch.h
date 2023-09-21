// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_UTIL_MAPPED_BATCH_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_UTIL_MAPPED_BATCH_H_

#include "gpu_mapping.h"

namespace magma {

// Abstract container for an executable unit (batch).
template <typename Context, typename Buffer>
class MappedBatch {
 public:
  virtual ~MappedBatch() = default;

  virtual std::weak_ptr<Context> GetContext() const = 0;
  virtual uint64_t GetGpuAddress() const = 0;
  virtual uint64_t GetLength() const = 0;
  virtual void SetSequenceNumber(uint32_t sequence_number) = 0;
  virtual uint32_t GetSequenceNumber() const = 0;
  virtual uint64_t GetBatchBufferId() const { return 0; }
  virtual const GpuMappingView<Buffer>* GetBatchMapping() const = 0;
  virtual bool IsCommandBuffer() const { return false; }
};

}  // namespace magma

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_UTIL_MAPPED_BATCH_H_
