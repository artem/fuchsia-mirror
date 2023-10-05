// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_EXTERNAL_MEMORY_ALLOCATOR_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_EXTERNAL_MEMORY_ALLOCATOR_H_

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/zx/event.h>

#include <unordered_map>

#include "allocator.h"

namespace sysmem_driver {
class ExternalMemoryAllocator : public MemoryAllocator {
 public:
  ExternalMemoryAllocator(MemoryAllocator::Owner* owner,
                          fidl::WireSharedClient<fuchsia_sysmem2::Heap> heap,
                          fuchsia_sysmem2::HeapProperties properties);

  ~ExternalMemoryAllocator() override;

  zx_status_t Allocate(uint64_t size, const fuchsia_sysmem2::SingleBufferSettings& settings,
                       std::optional<std::string> name, uint64_t buffer_collection_id,
                       uint32_t buffer_index, zx::vmo* parent_vmo) override;
  void Delete(zx::vmo parent_vmo) override;
  bool is_empty() override { return allocations_.empty(); }

 private:
  MemoryAllocator::Owner* owner_;
  fidl::WireSharedClient<fuchsia_sysmem2::Heap> heap_;

  struct BufferKey {
    uint64_t buffer_collection_id;
    uint32_t buffer_index;
  };
  std::unordered_map<zx_handle_t, BufferKey> allocations_;

  inspect::Node node_;
  inspect::ValueList properties_;
};

}  // namespace sysmem_driver

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_EXTERNAL_MEMORY_ALLOCATOR_H_
