// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_JOURNAL_INSPECTOR_JOURNAL_ENTRIES_H_
#define SRC_STORAGE_LIB_VFS_CPP_JOURNAL_INSPECTOR_JOURNAL_ENTRIES_H_

#include <array>
#include <cstddef>
#include <functional>

#include <disk_inspector/common_types.h>
#include <fbl/string_printf.h>

#include "src/storage/lib/vfs/cpp/journal/format.h"
#include "src/storage/lib/vfs/cpp/journal/inspector_journal.h"

namespace fs {

class JournalBlock : public disk_inspector::DiskObject {
 public:
  JournalBlock() = delete;
  JournalBlock(const JournalBlock&) = delete;
  JournalBlock(JournalBlock&&) = delete;
  JournalBlock& operator=(const JournalBlock&) = delete;
  JournalBlock& operator=(JournalBlock&&) = delete;

  // The api, like rest of the journal, accepts only kJournalBlockSize as block size.
  JournalBlock(uint32_t index, std::array<uint8_t, kJournalBlockSize> block);

  // DiskObject interface:
  const char* GetName() const override { return name_.c_str(); }

  uint32_t GetNumElements() const override;

  void GetValue(const void** out_buffer, size_t* out_buffer_size) const override;

  std::unique_ptr<DiskObject> GetElementAt(uint32_t index) const override;

 private:
  // This will be cast into more specific types, so ensure that it is aligned to support them.
  alignas(std::max_align_t) const std::array<uint8_t, kJournalBlockSize> block_;
  fbl::String name_;
  fs::JournalObjectType object_type_;
  const uint32_t index_ = 0;
  uint32_t num_elements_ = 0;
};

class JournalEntries : public disk_inspector::DiskObject {
 public:
  JournalEntries() = delete;
  JournalEntries(const JournalEntries&) = delete;
  JournalEntries(JournalEntries&&) = delete;
  JournalEntries& operator=(const JournalEntries&) = delete;
  JournalEntries& operator=(JournalEntries&&) = delete;

  JournalEntries(uint64_t start_block, uint64_t length, BlockReadCallback read_block)
      : start_block_(start_block), length_(length), read_block_(std::move(read_block)) {
    ZX_ASSERT(read_block_ != nullptr);
  }

  // DiskObject interface:
  const char* GetName() const override { return kJournalEntriesName; }

  uint32_t GetNumElements() const override { return static_cast<uint32_t>(length_); }

  void GetValue(const void** out_buffer, size_t* out_buffer_size) const override;

  std::unique_ptr<DiskObject> GetElementAt(uint32_t index) const override;

 private:
  uint64_t start_block_;
  uint64_t length_;
  BlockReadCallback read_block_;
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_JOURNAL_INSPECTOR_JOURNAL_ENTRIES_H_
