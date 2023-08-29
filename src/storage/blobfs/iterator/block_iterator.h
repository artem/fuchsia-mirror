// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_BLOBFS_ITERATOR_BLOCK_ITERATOR_H_
#define SRC_STORAGE_BLOBFS_ITERATOR_BLOCK_ITERATOR_H_

#include <lib/fit/function.h>
#include <stdbool.h>
#include <stdint.h>
#include <zircon/types.h>

#include <memory>

#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/iterator/allocated_extent_iterator.h"
#include "src/storage/blobfs/iterator/extent_iterator.h"

namespace blobfs {

// Wraps an ExtentIterator to allow traversal of a node in block-order rather than extent-order.
class BlockIterator {
 public:
  explicit BlockIterator(std::unique_ptr<ExtentIterator> iterator);
  BlockIterator(const BlockIterator&) = delete;
  BlockIterator& operator=(const BlockIterator&) = delete;
  BlockIterator(BlockIterator&&) = default;
  BlockIterator& operator=(BlockIterator&&) = default;

  // Returns true if there are no more blocks to be consumed.
  bool Done() const;

  // Returns the number of blocks we've iterated past in total.
  uint64_t BlockIndex() const;

  // Acquires up to |length| additional blocks.
  // Postcondition: |out_length| <= |length|.
  //
  // Returns the actual number of blocks available as |out_length|, starting at data block offset
  // |out_start|.
  zx_status_t Next(uint64_t length, uint64_t* out_length, uint64_t* out_start);

 private:
  std::unique_ptr<ExtentIterator> iterator_;
  // The latest extent pulled off of the iterator.
  const Extent* extent_ = nullptr;
  // The number of blocks left within the current extent.
  uint64_t blocks_left_ = 0;
};

// StreamBlocks is a utility function which reads exactly |block_count| blocks, dumping contiguous
// blocks encountered from |iterator| to the callback function |stream|. This function will exit
// with an error if |block_count| exceeds the amount of blocks that |iterator| points to.
using StreamFn = fit::function<zx_status_t(uint64_t local_off, uint64_t dev_off, uint64_t length)>;
zx_status_t StreamBlocks(BlockIterator* iterator, uint64_t block_count, StreamFn stream);

// IterateToBlock is a utility function which moves the iterator to block number |block_num|.
// Used by the blobfs pager to navigate to an arbitrary offset within a blob.
// NOTE: This can only move the iterator forward relative to the current position.
zx_status_t IterateToBlock(BlockIterator* iter, uint64_t block_num);

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_ITERATOR_BLOCK_ITERATOR_H_
