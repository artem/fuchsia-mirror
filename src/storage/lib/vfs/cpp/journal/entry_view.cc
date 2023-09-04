// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "entry_view.h"

#include <lib/cksum.h>
#include <zircon/types.h>

#include <vector>

#include <storage/operation/operation.h>

namespace fs {

JournalEntryView::JournalEntryView(storage::BlockBufferView view)
    : view_(std::move(view)),
      header_(cpp20::span<uint8_t>(reinterpret_cast<uint8_t*>(view_.Data(0)), view_.BlockSize())) {}

JournalEntryView::JournalEntryView(storage::BlockBufferView view,
                                   const std::vector<storage::BufferedOperation>& operations,
                                   uint64_t sequence_number)
    : view_(std::move(view)),
      header_(cpp20::span<uint8_t>(reinterpret_cast<uint8_t*>(view_.Data(0)), view_.BlockSize()),
              view_.length() - kEntryMetadataBlocks, sequence_number) {
  Encode(operations, sequence_number);
}

void JournalEntryView::Encode(const std::vector<storage::BufferedOperation>& operations,
                              uint64_t sequence_number) {
  ZX_DEBUG_ASSERT(header_.PayloadBlocks() <= kMaxBlockDescriptors);
  uint32_t block_index = 0;
  for (const auto& operation : operations) {
    for (size_t i = 0; i < operation.op.length; i++) {
      header_.SetTargetBlock(block_index, operation.op.dev_offset + i);
      auto block_ptr =
          reinterpret_cast<uint64_t*>(view_.Data(kJournalEntryHeaderBlocks + block_index));
      if (*block_ptr == kJournalEntryMagic) {
        // If the payload could be confused with a journal structure, replace it with zeros, and add
        // an "escaped" flag instead.
        *block_ptr = 0;
        header_.SetEscapedBlock(block_index, true);
      }
      block_index++;
    }
  }
  ZX_DEBUG_ASSERT_MSG(block_index == header_.PayloadBlocks(), "Mismatched block count");

  memset(footer(), 0, sizeof(JournalCommitBlock));
  footer()->prefix.magic = kJournalEntryMagic;
  footer()->prefix.sequence_number = sequence_number;
  footer()->prefix.flags = kJournalPrefixFlagCommit;
  footer()->checksum = CalculateChecksum();
}

zx::result<> JournalEntryView::DecodePayloadBlocks() {
  // Verify that all escaped blocks start with the correct prefix.
  for (uint32_t i = 0; i < header().PayloadBlocks(); i++) {
    if (header_.EscapedBlock(i)) {
      uint64_t* block_ptr = reinterpret_cast<uint64_t*>(view_.Data(kJournalEntryHeaderBlocks + i));
      if (*block_ptr != 0) {
        return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
      }
    }
  }
  // Overwrite escaped block with correct payload prefix.
  for (uint32_t i = 0; i < header().PayloadBlocks(); i++) {
    if (header_.EscapedBlock(i)) {
      uint64_t* block_ptr = reinterpret_cast<uint64_t*>(view_.Data(kJournalEntryHeaderBlocks + i));
      *block_ptr = kJournalEntryMagic;
    }
  }
  return zx::ok();
}

uint32_t JournalEntryView::CalculateChecksum() const {
  // Currently, the checksum includes all blocks excluding the commit block. If additional data is
  // to be added to the commit block, we should consider making the checksum include the commit
  // block (excluding the checksum location).
  uint32_t checksum = 0;
  for (size_t i = 0; i < view_.length() - 1; i++) {
    checksum = crc32(checksum, static_cast<const uint8_t*>(view_.Data(i)), kJournalBlockSize);
  }
  return checksum;
}

}  // namespace fs
