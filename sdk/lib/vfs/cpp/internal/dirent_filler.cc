// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fdio/vfs.h>
#include <lib/vfs/cpp/internal/dirent_filler.h>
#include <limits.h>

namespace vfs {
namespace internal {

DirentFiller::DirentFiller(void* ptr, uint64_t len)
    : ptr_(static_cast<char*>(ptr)), pos_(0), len_(len) {}

zx_status_t DirentFiller::Next(const std::string& name, uint8_t type, uint64_t ino) {
  return Next(name.data(), name.length(), type, ino);
}

zx_status_t DirentFiller::Next(const char* name, size_t name_len, uint8_t type, uint64_t ino) {
// TODO(b/293936429): Remove use of deprecated `vdirent_t` when transitioning ReadDir to Enumerate
// as part of io2 migration.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
  vdirent_t* de = reinterpret_cast<vdirent_t*>(ptr_ + pos_);
  size_t sz = sizeof(vdirent_t) + name_len;
#pragma clang diagnostic pop

  if (sz > len_ - pos_ || name_len > NAME_MAX) {
    return ZX_ERR_INVALID_ARGS;
  }
  de->ino = ino;
  de->size = static_cast<uint8_t>(name_len);
  de->type = type;
  memcpy(de->name, name, name_len);
  pos_ += sz;
  return ZX_OK;
}

}  // namespace internal
}  // namespace vfs
