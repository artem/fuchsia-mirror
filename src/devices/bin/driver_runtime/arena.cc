// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_runtime/arena.h"

#include <fbl/ref_ptr.h>

// static
zx_status_t fdf_arena::Create(uint32_t options, uint32_t tag, fdf_arena** out_arena) {
  auto arena = fbl::AdoptRef(new fdf_arena(tag));
  if (!arena) {
    return ZX_ERR_NO_MEMORY;
  }
  *out_arena = fbl::ExportToRawPtr(&arena);
  return ZX_OK;
}

void* fdf_arena::Allocate(size_t bytes) {
  fbl::AutoLock lock(&lock_);

  bytes = FIDL_ALIGN(bytes);

  if (available_size_ < bytes) {
    // The data doesn't fit within the current block => allocate a new block.
    size_t block_size = (bytes > ExtraBlock::kExtraSize) ? bytes : ExtraBlock::kExtraSize;
    size_t extra_bytes = block_size + FIDL_ALIGN(sizeof(ExtraBlockNode));

    // Account for the new block
    auto extra_block = new (new uint8_t[extra_bytes]) ExtraBlock();
    extra_blocks_.push_front(extra_block);
    uint8_t* data = extra_block->data();
    allocated_ranges_.insert({reinterpret_cast<uintptr_t>(data), block_size});

    if (bytes < ExtraBlock::kExtraSize) {
      // If the allocation is smaller than the extra block size, use the new block
      // for further allocations.
      // Note: the data available at the end of the current block is lost forever (until the
      // deallocation of the arena).
      next_data_available_ = current_block_ = data;
      available_size_ = block_size;
    } else {
      // Otherwise, just return the block and continue using the one we already
      // have allocated.
      return data;
    }
  }
  // At this point we have enough space within the current block (either because there was enough
  // space within the existing block or because we allocate a new block).
  uint8_t* data = next_data_available_;
  next_data_available_ += bytes;
  available_size_ -= bytes;
  return data;
}

// No-op for initial implementation.
void fdf_arena::Free(void* data) {}

namespace {

// Returns whether the range [addr, addr + num_bytes) contains
// [want_addr, want_addr + want_num_bytes).
bool contains_range(uintptr_t addr, size_t num_bytes, uintptr_t want_addr, size_t want_num_bytes) {
  if (addr > want_addr) {
    return false;
  }
  uintptr_t range_end = addr + num_bytes;
  uintptr_t want_end = want_addr + want_num_bytes;
  return want_end <= range_end;
}

}  // namespace

bool fdf_arena::Contains(const void* data, size_t num_bytes) {
  fbl::AutoLock lock(&lock_);

  uintptr_t want_addr = reinterpret_cast<uintptr_t>(data);

  // Check if the requested address lies in the initial buffer, or if we have to
  // find it in the extra_blocks map.
  uintptr_t allocated_addr = reinterpret_cast<uintptr_t>(initial_buffer_);
  size_t allocated_size = kInitialBufferSize;
  if (want_addr < allocated_addr || want_addr >= allocated_addr + allocated_size) {
    // |std::upper_bound| will return the first element greater than the requested key.
    auto it = allocated_ranges_.upper_bound(want_addr);
    if (it == allocated_ranges_.begin()) {
      return false;
    }
    it--;  // This now points to the key less than or equal to the requested key.
    allocated_addr = it->first;
    allocated_size = it->second;
  }

  // If we are checking against the newest buffer, part of it might actually not have been allocated
  // to the user yet.
  if (allocated_addr == reinterpret_cast<uintptr_t>(current_block_)) {
    ZX_ASSERT(allocated_size >= available_size_);
    allocated_size -= available_size_;
  }
  return contains_range(allocated_addr, allocated_size, want_addr, num_bytes);
}

void fdf_arena::Destroy() { [[maybe_unused]] auto ref = fbl::ImportFromRawPtr(this); }

fdf_arena::~fdf_arena() __TA_NO_THREAD_SAFETY_ANALYSIS {
  // Deletes all the extra blocks.
  while (!extra_blocks_.is_empty()) {
    auto* to_be_deleted = extra_blocks_.pop_front();
    delete[] reinterpret_cast<uint8_t*>(to_be_deleted);
  }
}
