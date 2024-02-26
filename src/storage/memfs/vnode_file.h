// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_MEMFS_VNODE_FILE_H_
#define SRC_STORAGE_MEMFS_VNODE_FILE_H_

#include <lib/zx/vmo.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode.h"

namespace memfs {

class VnodeFile final : public Vnode {
 public:
  explicit VnodeFile(Memfs& memfs);
  ~VnodeFile() final;

  fuchsia_io::NodeProtocolKinds GetProtocols() const final;

  zx::result<zx::stream> CreateStream(uint32_t stream_options) final;
  void DidModifyStream() final;

  zx_status_t Truncate(size_t len) final;
  zx_status_t GetAttributes(fs::VnodeAttributes* a) final;
  zx_status_t SetAttributes(fs::VnodeAttributesUpdate a) final;
  zx_status_t GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo) final;
  zx_status_t CloseNode() final;
  void Sync(SyncCallback closure) final;
  bool SupportsClientSideStreams() const final { return true; }

 private:
  zx_status_t CreateBackingStoreIfNeeded() __TA_REQUIRES(mutex_);

  // Returns the content size of |paged_vmo()|. Requires |paged_vmo()| to be valid.
  uint64_t GetContentSize() const __TA_REQUIRES_SHARED(mutex_);

  // Checks to see if the contents of this file were modified since the last time this method was
  // called. If the file was modified then the mtime is updated.
  void UpdateModifiedIfVmoChanged() __TA_REQUIRES_SHARED(mutex_);

  [[maybe_unused]] Memfs& memfs_;
};

}  // namespace memfs

#endif  // SRC_STORAGE_MEMFS_VNODE_FILE_H_
