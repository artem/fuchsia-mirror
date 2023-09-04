// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains functions that are shared between host
// and target implementations of Blobfs.

#ifndef SRC_STORAGE_BLOBFS_COMMON_H_
#define SRC_STORAGE_BLOBFS_COMMON_H_

#include <assert.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <zircon/types.h>

#include <ostream>

#include <bitmap/raw-bitmap.h>
#include <bitmap/storage.h>
#include <fbl/algorithm.h>
#include <fbl/macros.h>
#include <fbl/string_buffer.h>

#include "src/storage/blobfs/blob_layout.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/lib/vfs/cpp/transaction/transaction_handler.h"

namespace blobfs {

// Options for constructing new blobfs images.
struct FilesystemOptions {
  // Which layout to use to store blobs.
  BlobLayoutFormat blob_layout_format = BlobLayoutFormat::kCompactMerkleTreeAtEnd;

  // The oldest minor version to mark the filesystem as formatted by. This should be left unset (to
  // the default value of kBlobfsCurrentMinorVersion); it is exposed for overriding during tests.
  uint64_t oldest_minor_version = kBlobfsCurrentMinorVersion;

  // The number of inodes to allocate capacity for. This value will be rounded up to fill a complete
  // block (or slice, if within FVM). When blobfs is within FVM, this is used to determine the
  // initial node table size, which can grow when used.
  uint64_t num_inodes = kBlobfsDefaultInodeCount;
};

// The minimum size of a blob that we will consider for compression. Attempting to compress a blob
// smaller than this will not result in any size savings, so we can just skip it and save some work.
constexpr uint64_t kCompressionSizeThresholdBytes = kBlobfsBlockSize;

#ifdef __Fuchsia__
using RawBitmap = bitmap::RawBitmapGeneric<bitmap::VmoStorage>;
#else
using RawBitmap = bitmap::RawBitmapGeneric<bitmap::DefaultStorage>;
#endif

// Validates the metadata of a blobfs superblock, given a disk with |max| blocks. If |quiet| is
// true, no log messages will be emitted.
zx_status_t CheckSuperblock(const Superblock* info, uint64_t max, bool quiet = false);

// Returns number of blocks required for inode_count inodes
uint64_t BlocksRequiredForInode(uint64_t inode_count);

// Returns number of blocks required for bit_count bits
uint64_t BlocksRequiredForBits(uint64_t bit_count);

// Calling this method will initialize the superblock's common fields with non-fvm values but leave
// all of the size-related fields set to zero. It is the caller's responsibility to set the
// size-related and fvm-specific fields to appropriate values.
void InitializeSuperblockOptions(const FilesystemOptions& options, Superblock* info);

// Creates a superblock, formatted for |block_count| disk blocks on a non-FVM volume. Calls
// |InitializeSuperblockOptions|. Returns ZX_ERR_NO_SPACE if there is not enough blocks to make a
// minimal partition.
zx_status_t InitializeSuperblock(uint64_t block_count, const FilesystemOptions& options,
                                 Superblock* info);

// Get a pointer to the nth block of the bitmap.
inline void* GetRawBitmapData(const RawBitmap& bm, uint64_t n) {
  assert(n * kBlobfsBlockSize < bm.size());                // Accessing beyond end of bitmap
  assert(kBlobfsBlockSize <= (n + 1) * kBlobfsBlockSize);  // Avoid overflow
  return fs::GetBlock(kBlobfsBlockSize, bm.StorageUnsafe()->GetData(), n);
}

// Returns the blob layout format used in |info|. Panics if the blob layout format is invalid.
// |CheckSuperblock| should be used to validate |info| before trying to access the blob layout
// format.
BlobLayoutFormat GetBlobLayoutFormat(const Superblock& info);

// Helper functions to create VMO names for blobs in various states. Although rare, name collisions
// *are* possible, as the name is based on a prefix of the merkle root hash of |node|.

using VmoNameBuffer = fbl::StringBuffer<ZX_MAX_NAME_LEN>;

VmoNameBuffer FormatBlobDataVmoName(const digest::Digest& digest);
VmoNameBuffer FormatInactiveBlobDataVmoName(const digest::Digest& digest);
VmoNameBuffer FormatWritingBlobDataVmoName(const digest::Digest& digest);

// Pretty-print formatter for Blobfs Superblock fields.
std::ostream& operator<<(std::ostream& stream, const Superblock& info);

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_COMMON_H_
