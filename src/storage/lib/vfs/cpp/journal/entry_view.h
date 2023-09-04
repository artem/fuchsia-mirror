// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_JOURNAL_ENTRY_VIEW_H_
#define SRC_STORAGE_LIB_VFS_CPP_JOURNAL_ENTRY_VIEW_H_

#include <lib/zx/result.h>
#include <zircon/types.h>

#include <fbl/macros.h>
#include <storage/buffer/block_buffer_view.h>
#include <storage/operation/operation.h>

#include "src/storage/lib/vfs/cpp/journal/format.h"
#include "src/storage/lib/vfs/cpp/journal/header_view.h"
#include "src/storage/lib/vfs/cpp/journal/superblock.h"

namespace fs {

// A view into the filesystem journal entry, including the header and footer.
//
// This class does not have ownership over the underlying buffer, instead, it merely provides a
// basic mechanism to parse a view of the buffer which is owned elsewhere.
class JournalEntryView {
 public:
  // Creates a new entry view without modification.
  explicit JournalEntryView(storage::BlockBufferView view);

  // Creates a new entry view which encodes the operations into the view on construction.
  //
  // Asserts that |operations| is exactly the size of the journal entry.
  JournalEntryView(storage::BlockBufferView view,
                   const std::vector<storage::BufferedOperation>& operations,
                   uint64_t sequence_number);

  const JournalHeaderView& header() const { return header_; }

  const JournalCommitBlock* footer() const {
    return reinterpret_cast<const JournalCommitBlock*>(
        view_.Data(view_.length() - kJournalEntryCommitBlocks));
  }

  // Iterates through all blocks in the previously set entry, and resets all escaped blocks within
  // the constructor-provided buffer. On failure, does not modify underlying view.
  zx::result<> DecodePayloadBlocks();

  // Calculates the checksum of all blocks excluding the commit block.
  uint32_t CalculateChecksum() const;

 private:
  // Sets all fields of the journal entry.
  //
  // May modify the contents of the payload to "escape" blocks with a prefix that matches
  // |kJournalEntryMagic|.
  //
  // Asserts that |operations| is exactly the size of the journal entry.
  void Encode(const std::vector<storage::BufferedOperation>& operations, uint64_t sequence_number);

  JournalCommitBlock* footer() {
    return reinterpret_cast<JournalCommitBlock*>(
        view_.Data(view_.length() - kJournalEntryCommitBlocks));
  }

  storage::BlockBufferView view_;
  JournalHeaderView header_;
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_JOURNAL_ENTRY_VIEW_H_
