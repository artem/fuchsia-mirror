// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/lazy_dir.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include <algorithm>
#include <string_view>

#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace {
int CompareLazyDirPtrs(const void* a, const void* b) {
  auto a_id = static_cast<const LazyDir::LazyEntry*>(a)->id;
  auto b_id = static_cast<const LazyDir::LazyEntry*>(b)->id;
  if (a_id == b_id) {
    return 0;
  }
  return a_id < b_id ? -1 : 1;
}

bool DoDot(VdirCookie* cookie) {
  if (cookie->p == 0) {
    cookie->p = (void*)1;
    return true;
  }
  return false;
}
}  // namespace

LazyDir::LazyDir() = default;
LazyDir::~LazyDir() = default;

fuchsia_io::NodeProtocolKinds LazyDir::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kDirectory;
}

zx_status_t LazyDir::GetAttributes(VnodeAttributes* attr) {
  *attr = VnodeAttributes();
  attr->mode = V_TYPE_DIR | V_IRUSR;
  attr->inode = fio::wire::kInoUnknown;
  attr->link_count = 1;
  return ZX_OK;
}

zx_status_t LazyDir::Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out_vnode) {
  LazyEntryVector entries;
  GetContents(&entries);
  for (const auto& entry : entries) {
    if (name.compare(entry.name) == 0) {
      return GetFile(out_vnode, entry.id, entry.name);
    }
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t LazyDir::Readdir(VdirCookie* cookie, void* dirents, size_t len, size_t* out_actual) {
  LazyEntryVector entries;
  GetContents(&entries);
  qsort(entries.data(), entries.size(), sizeof(LazyEntry), CompareLazyDirPtrs);

  fs::DirentFiller df(dirents, len);
  zx_status_t r = 0;

  const uint64_t ino = fio::wire::kInoUnknown;
  if (DoDot(cookie)) {
    if ((r = df.Next(".", VTYPE_TO_DTYPE(V_TYPE_DIR), ino)) != ZX_OK) {
      *out_actual = df.BytesFilled();
      return r;
    }
  }

  for (auto it = std::lower_bound(entries.begin(), entries.end(), cookie->n,
                                  [](const LazyEntry& a, uint64_t b_id) { return a.id < b_id; });
       it < entries.end(); ++it) {
    if (cookie->n >= it->id) {
      continue;
    }
    if ((r = df.Next(it->name, VTYPE_TO_DTYPE(it->type), ino)) != ZX_OK) {
      *out_actual = df.BytesFilled();
      return r;
    }
    cookie->n = it->id;
  }
  *out_actual = df.BytesFilled();
  return ZX_OK;
}

}  // namespace fs
