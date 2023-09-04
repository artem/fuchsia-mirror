// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_MINFS_DIRECTORY_H_
#define SRC_STORAGE_MINFS_DIRECTORY_H_

#include <lib/zx/result.h>

#include <string_view>

#include <fbl/algorithm.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/vnode.h"
#include "src/storage/minfs/format.h"
#include "src/storage/minfs/minfs.h"
#include "src/storage/minfs/superblock.h"
#include "src/storage/minfs/transaction_limits.h"
#include "src/storage/minfs/vnode.h"
#include "src/storage/minfs/writeback.h"

namespace minfs {

struct DirectoryOffset {
  size_t off = 0;       // Offset in directory of current record
  size_t off_prev = 0;  // Offset in directory of previous record
};

struct DirArgs {
  std::string_view name;
  ino_t ino = 0;
  uint32_t type = 0;
  uint32_t reclen = 0;
  Transaction* transaction = nullptr;
  DirectoryOffset offs;
};

// A specialization of the Minfs Vnode which implements a directory interface.
class Directory final : public VnodeMinfs, public fbl::Recyclable<Directory> {
 public:
  explicit Directory(Minfs* fs);
  ~Directory() final;

  // Required for memory management, see the class comment above Vnode for more.
  void fbl_recycle() { RecycleNode(); }

  // fs::Vnode interface.
  fs::VnodeProtocolSet GetProtocols() const final;
  zx_status_t Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) final;
  zx_status_t Read(void* data, size_t len, size_t off, size_t* out_actual) final;
  zx_status_t Write(const void* data, size_t len, size_t offset, size_t* out_actual) final;
  zx_status_t Append(const void* data, size_t len, size_t* out_end, size_t* out_actual) final;
  zx_status_t Readdir(fs::VdirCookie* cookie, void* dirents, size_t len, size_t* out_actual) final;
  zx_status_t Create(std::string_view name, uint32_t mode, fbl::RefPtr<fs::Vnode>* out) final;
  zx_status_t Unlink(std::string_view name, bool must_be_dir) final;
  zx_status_t Rename(fbl::RefPtr<fs::Vnode> newdir, std::string_view oldname,
                     std::string_view newname, bool src_must_be_dir, bool dst_must_be_dir) final;
  zx_status_t Link(std::string_view name, fbl::RefPtr<fs::Vnode> target) final;
  zx_status_t Truncate(size_t len) final;

 private:
  // Possible non-error return values for DirentCallback:
  enum class IteratorCommand {
    // Immediately stop iterating over the directory.
    kIteratorDone,
    // Access the next direntry in the directory. Offsets updated.
    kIteratorNext,
    // Identify that the direntry record was modified. Stop iterating.
    kIteratorSaveSync,
  };

  // minfs::Vnode interface.
  zx::result<> CanUnlink() const final;
  blk_t GetBlockCount() const final;
  uint64_t GetSize() const final;
  void SetSize(uint32_t new_size) final;
  void AcquireWritableBlock(Transaction* transaction, blk_t local_bno, blk_t old_bno,
                            blk_t* out_bno) final;
  void DeleteBlock(PendingWork* transaction, blk_t local_bno, blk_t old_bno, bool indirect) final;
  bool IsDirectory() const final { return true; }
  bool DirtyCacheEnabled() const final {
    // We don't yet enable dirty cache for directory.
    return false;
  }
  zx::result<> FlushCachedWrites() final { return zx::ok(); }
  void DropCachedWrites() final {}
  bool IsDirty() const final { return false; }

#ifdef __Fuchsia__
  void IssueWriteback(Transaction* transaction, blk_t vmo_offset, blk_t dev_offset,
                      blk_t count) final;
  bool HasPendingAllocation(blk_t vmo_offset) final;
  void CancelPendingWriteback() final;
#endif

  // Other, non-virtual methods:

  // Lookup which can traverse '..'
  zx::result<fbl::RefPtr<fs::Vnode>> LookupInternal(std::string_view name);

  // Verify that the 'newdir' inode is not a subdirectory of this Vnode.
  // Traces the path from newdir back to the root inode.
  zx::result<> CheckNotSubdirectory(fbl::RefPtr<Directory> newdir);

  using DirentCallback = zx::result<IteratorCommand> (*)(fbl::RefPtr<Directory>, Dirent*, DirArgs*);

  // Enumerates directories.
  // On success returns true if the exit was a result of the callback, and false if the listing was
  // exhausted with no action taken.
  zx::result<bool> ForEachDirent(DirArgs* args, DirentCallback func);

  // Directory callback functions.
  //
  // The following functions are passable to |ForEachDirent|, which reads the parent directory,
  // one dirent at a time, and passes each entry to the callback function, along with the DirArgs
  // information passed to the initial call of |ForEachDirent|.
  static zx::result<IteratorCommand> DirentCallbackFind(fbl::RefPtr<Directory>, Dirent*, DirArgs*);
  static zx::result<IteratorCommand> DirentCallbackUnlink(fbl::RefPtr<Directory>, Dirent*,
                                                          DirArgs*);
  static zx::result<IteratorCommand> DirentCallbackForceUnlink(fbl::RefPtr<Directory>, Dirent*,
                                                               DirArgs*);
  static zx::result<IteratorCommand> DirentCallbackAttemptRename(fbl::RefPtr<Directory>, Dirent*,
                                                                 DirArgs*);
  static zx::result<IteratorCommand> DirentCallbackUpdateInode(fbl::RefPtr<Directory>, Dirent*,
                                                               DirArgs*);
  static zx::result<IteratorCommand> DirentCallbackFindSpace(fbl::RefPtr<Directory>, Dirent*,
                                                             DirArgs*);

  static zx::result<IteratorCommand> NextDirent(Dirent* de, DirectoryOffset* offs);

  // Appends a new directory at the specified offset within |args|. This requires a prior call to
  // DirentCallbackFindSpace to find an offset where there is space for the direntry. It takes
  // the same |args| that were passed into DirentCallbackFindSpace.
  zx::result<> AppendDirent(DirArgs* args);

  zx::result<IteratorCommand> UnlinkChild(Transaction* transaction, fbl::RefPtr<VnodeMinfs> child,
                                          Dirent* de, DirectoryOffset* offs);
};

}  // namespace minfs

#endif  // SRC_STORAGE_MINFS_DIRECTORY_H_
