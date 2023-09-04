// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/journal/superblock.h"

#include <lib/cksum.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include "src/storage/lib/vfs/cpp/journal/format.h"

namespace fs {

JournalSuperblock::JournalSuperblock() = default;

JournalSuperblock::JournalSuperblock(std::unique_ptr<storage::BlockBuffer> buffer)
    : buffer_(std::move(buffer)) {
  ZX_DEBUG_ASSERT_MSG(buffer_->capacity() > 0, "Buffer is too small for journal superblock");
}

zx_status_t JournalSuperblock::Validate() const {
#ifdef FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION
  return ZX_OK;
#else
  if (Info()->magic != kJournalMagic) {
    FX_LOGST(ERROR, "journal") << "Bad journal magic";
    return ZX_ERR_IO;
  }
  if (old_checksum() != new_checksum()) {
    FX_LOGST(ERROR, "journal") << "Bad journal info checksum";
    return ZX_ERR_IO;
  }
  return ZX_OK;
#endif
}

void JournalSuperblock::Update(uint64_t start, uint64_t sequence_number) {
  Info()->magic = kJournalMagic;
  Info()->start_block = start;
  Info()->timestamp = sequence_number;
  Info()->checksum = new_checksum();
}

uint32_t JournalSuperblock::new_checksum() const {
  JournalInfo info = *reinterpret_cast<const JournalInfo*>(buffer_->Data(0));
  info.checksum = 0;
  return crc32(0, reinterpret_cast<const uint8_t*>(&info), sizeof(JournalInfo));
}

}  // namespace fs
