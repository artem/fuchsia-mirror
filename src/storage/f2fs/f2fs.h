// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_F2FS_H_
#define SRC_STORAGE_F2FS_F2FS_H_

// clang-format off
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/trace/event.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <fidl/fuchsia.fs.startup/cpp/wire.h>

#include <fcntl.h>

#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <lib/syslog/cpp/macros.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>

#include <fbl/algorithm.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/macros.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_buffer.h>

#include <atomic>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <semaphore>

#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vnode.h"
#include "src/storage/lib/vfs/cpp/transaction/buffered_operations_builder.h"

#include "src/storage/lib/vfs/cpp/paged_vfs.h"
#include "src/storage/lib/vfs/cpp/paged_vnode.h"
#include "src/storage/lib/vfs/cpp/watcher.h"
#include "src/storage/lib/vfs/cpp/shared_mutex.h"
#include "src/storage/lib/vfs/cpp/service.h"

#include "lib/inspect/cpp/inspect.h"
#include "lib/inspect/service/cpp/service.h"

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/inspect/inspect_tree.h"

#include "src/storage/f2fs/common.h"
#include "src/storage/f2fs/f2fs_layout.h"
#include "src/storage/f2fs/bcache.h"
#include "src/storage/f2fs/mount.h"
#include "src/storage/f2fs/f2fs_internal.h"
#include "src/storage/f2fs/namestring.h"
#include "src/storage/f2fs/storage_buffer.h"
#include "src/storage/f2fs/writeback.h"
#include "src/storage/f2fs/reader.h"
#include "src/storage/f2fs/vnode.h"
#include "src/storage/f2fs/vnode_cache.h"
#include "src/storage/f2fs/dir.h"
#include "src/storage/f2fs/file.h"
#include "src/storage/f2fs/node.h"
#include "src/storage/f2fs/segment.h"
#include "src/storage/f2fs/gc.h"
#include "src/storage/f2fs/mkfs.h"
#include "src/storage/f2fs/fsck.h"
#include "src/storage/f2fs/service/admin.h"
#include "src/storage/f2fs/service/startup.h"
#include "src/storage/f2fs/service/lifecycle.h"
#include "src/storage/f2fs/component_runner.h"
#include "src/storage/f2fs/dir_entry_cache.h"
#include "src/storage/f2fs/inspect.h"
#include "src/storage/f2fs/memory_watcher.h"
// clang-format on

namespace f2fs {

zx::result<std::unique_ptr<Superblock>> LoadSuperblock(BcacheMapper &bc);

class F2fs final {
 public:
  // Not copyable or moveable
  F2fs(const F2fs &) = delete;
  F2fs &operator=(const F2fs &) = delete;
  F2fs(F2fs &&) = delete;
  F2fs &operator=(F2fs &&) = delete;

  explicit F2fs(FuchsiaDispatcher dispatcher, std::unique_ptr<f2fs::BcacheMapper> bc,
                const MountOptions &mount_options, PlatformVfs *vfs);

  static zx::result<std::unique_ptr<F2fs>> Create(FuchsiaDispatcher dispatcher,
                                                  std::unique_ptr<f2fs::BcacheMapper> bc,
                                                  const MountOptions &options, PlatformVfs *vfs);

  zx::result<fs::FilesystemInfo> GetFilesystemInfo();
  DirEntryCache &GetDirEntryCache() { return dir_entry_cache_; }
  InspectTree &GetInspectTree() { return *inspect_tree_; }
  void Sync(SyncCallback closure);

  VnodeCache &GetVCache() { return vnode_cache_; }
  zx_status_t InsertVnode(VnodeF2fs *vn) { return vnode_cache_.Add(vn); }
  void EvictVnode(VnodeF2fs *vn) { vnode_cache_.Evict(vn); }
  zx_status_t LookupVnode(ino_t ino, fbl::RefPtr<VnodeF2fs> *out) {
    return vnode_cache_.Lookup(ino, out);
  }

  zx::result<std::unique_ptr<f2fs::BcacheMapper>> TakeBc() {
    if (!bc_) {
      return zx::error(ZX_ERR_UNAVAILABLE);
    }
    return zx::ok(std::move(bc_));
  }

  BcacheMapper &GetBc() const {
    ZX_DEBUG_ASSERT(bc_ != nullptr);
    return *bc_;
  }
  SuperblockInfo &GetSuperblockInfo() const {
    ZX_DEBUG_ASSERT(superblock_info_ != nullptr);
    return *superblock_info_;
  }
  SegmentManager &GetSegmentManager() const {
    ZX_DEBUG_ASSERT(segment_manager_ != nullptr);
    return *segment_manager_;
  }
  NodeManager &GetNodeManager() const {
    ZX_DEBUG_ASSERT(node_manager_ != nullptr);
    return *node_manager_;
  }
  GcManager &GetGcManager() const {
    ZX_DEBUG_ASSERT(gc_manager_ != nullptr);
    return *gc_manager_;
  }
  PlatformVfs *vfs() const { return vfs_; }

  // For testing Reset() and TakeBc()
  bool IsValid() const;
  void ResetPsuedoVnodes() {
    root_vnode_.reset();
    meta_vnode_.reset();
    node_vnode_.reset();
  }
  void ResetSuperblockInfo() { superblock_info_.reset(); }
  void ResetSegmentManager() {
    segment_manager_->DestroySegmentManager();
    segment_manager_.reset();
  }
  void ResetNodeManager() {
    node_manager_->DestroyNodeManager();
    node_manager_.reset();
  }
  void ResetGcManager() { gc_manager_.reset(); }

  // f2fs.cc
  void PutSuper();
  void SyncFs(bool bShutdown = false);
  zx_status_t LoadSuper(std::unique_ptr<Superblock> sb);
  void Reset();
  zx_status_t GrabMetaPage(pgoff_t index, LockedPage *out);
  zx_status_t GetMetaPage(pgoff_t index, LockedPage *out);
  bool CanReclaim() const;
  bool IsTearDown() const;
  void SetTearDown();

  // checkpoint.cc
  zx_status_t CheckOrphanSpace();
  void AddOrphanInode(VnodeF2fs *vnode);
  void PurgeOrphanInode(nid_t ino);
  int PurgeOrphanInodes();
  void WriteOrphanInodes(block_t start_blk);
  zx_status_t GetValidCheckpoint();
  zx_status_t ValidateCheckpoint(block_t cp_addr, uint64_t *version, LockedPage *out);
  void BlockOperations();
  void UnblockOperations();
  zx_status_t DoCheckpoint(bool is_umount);
  zx_status_t WriteCheckpoint(bool blocked, bool is_umount);
  uint32_t GetFreeSectionsForDirtyPages();
  bool IsCheckpointAvailable();

  // recovery.cc
  // For the list of fsync inodes, used only during recovery
  class FsyncInodeEntry : public fbl::DoublyLinkedListable<std::unique_ptr<FsyncInodeEntry>> {
   public:
    explicit FsyncInodeEntry(fbl::RefPtr<VnodeF2fs> vnode_refptr)
        : vnode_(std::move(vnode_refptr)) {}

    FsyncInodeEntry() = delete;
    FsyncInodeEntry(const FsyncInodeEntry &) = delete;
    FsyncInodeEntry &operator=(const FsyncInodeEntry &) = delete;
    FsyncInodeEntry(FsyncInodeEntry &&) = delete;
    FsyncInodeEntry &operator=(FsyncInodeEntry &&) = delete;

    block_t GetLastDnodeBlkaddr() const { return last_dnode_blkaddr_; }
    void SetLastDnodeBlkaddr(block_t blkaddr) { last_dnode_blkaddr_ = blkaddr; }
    VnodeF2fs &GetVnode() const { return *vnode_; }

   private:
    fbl::RefPtr<VnodeF2fs> vnode_ = nullptr;  // vfs inode pointer
    block_t last_dnode_blkaddr_ = 0;          // block address locating the last dnode
  };
  using FsyncInodeList = fbl::DoublyLinkedList<std::unique_ptr<FsyncInodeEntry>>;

  FsyncInodeEntry *GetFsyncInode(FsyncInodeList &inode_list, nid_t ino);
  zx_status_t RecoverDentry(NodePage &ipage, VnodeF2fs &vnode);
  zx_status_t RecoverInode(VnodeF2fs &vnode, NodePage &node_page);
  zx_status_t FindFsyncDnodes(FsyncInodeList &inode_list);
  void DestroyFsyncDnodes(FsyncInodeList &inode_list);
  void CheckIndexInPrevNodes(block_t blkaddr);
  void DoRecoverData(VnodeF2fs &vnode, NodePage &page);
  void RecoverData(FsyncInodeList &inode_list, CursegType type);
  void RecoverFsyncData();

  VnodeF2fs &GetNodeVnode() { return *node_vnode_; }
  VnodeF2fs &GetMetaVnode() { return *meta_vnode_; }
  zx::result<fbl::RefPtr<VnodeF2fs>> GetRootVnode() {
    if (root_vnode_) {
      return zx::ok(root_vnode_);
    }
    return zx::error(ZX_ERR_BAD_STATE);
  }

  // For testing
  void SetVfsForTests(std::unique_ptr<PlatformVfs> vfs) { vfs_for_tests_ = std::move(vfs); }
  zx::result<std::unique_ptr<PlatformVfs>> TakeVfsForTests() {
    if (vfs_for_tests_) {
      return zx::ok(std::move(vfs_for_tests_));
    }
    return zx::error(ZX_ERR_UNAVAILABLE);
  }

  zx::result<> MakeReadOperations(std::vector<LockedPage> &pages, std::vector<block_t> &addrs,
                                  PageType type, bool is_sync = true);
  zx::result<> MakeReadOperation(LockedPage &page, block_t blk_addr, PageType type,
                                 bool is_sync = true);
  zx::result<> MakeReadOperations(zx::vmo &vmo, std::vector<block_t> &addrs, PageType type,
                                  bool is_sync = true);
  zx_status_t MakeTrimOperation(block_t blk_addr, block_t nblocks) const;

  void ScheduleWriter(sync_completion_t *completion = nullptr, PageList pages = {},
                      bool flush = true) {
    writer_->ScheduleWriteBlocks(completion, std::move(pages), flush);
  }
  void ScheduleWriter(fpromise::promise<> task) { writer_->ScheduleTask(std::move(task)); }

  void ScheduleWriteback(size_t num_pages = kDefaultBlocksPerSegment);
  zx::result<> WaitForWriteback() {
    if (!writeback_flag_.try_acquire_for(std::chrono::seconds(kWriteTimeOut))) {
      return zx::error(ZX_ERR_TIMED_OUT);
    }
    writeback_flag_.release();
    return zx::ok();
  }
  std::atomic_flag &GetStopReclaimFlag() { return stop_reclaim_flag_; }

  bool StopWriteback() {
    // release-acquire ordering with MemoryPressureWatcher::OnLevelChanged().
    auto level = current_memory_pressure_level_.load(std::memory_order_acquire);
    return level == MemoryPressure::kLow ||
           !superblock_info_->GetPageCount(CountType::kDirtyData) ||
           (level == MemoryPressure::kUnknown &&
            superblock_info_->GetPageCount(CountType::kDirtyData) < kMaxDirtyDataPages / 4);
  }

  bool HasNotEnoughMemory() {
    // release-acquire ordering with MemoryPressureWatcher::OnLevelChanged().
    auto level = current_memory_pressure_level_.load(std::memory_order_acquire);
    return (level > MemoryPressure::kLow &&
            superblock_info_->GetPageCount(CountType::kDirtyData)) ||
           (level == MemoryPressure::kUnknown &&
            superblock_info_->GetPageCount(CountType::kDirtyData) >= kMaxDirtyDataPages);
  }

  void WaitForAvailableMemory() {
    while (HasNotEnoughMemory()) {
      ScheduleWriteback();
      ZX_ASSERT(WaitForWriteback().is_ok());
    }
  }

  bool IsOnRecovery() const { return on_recovery_; }
  void SetOnRecovery() { on_recovery_ = true; }
  void ClearOnRecovery() { on_recovery_ = false; }

  void AddToVnodeSet(VnodeSet type, nid_t ino) __TA_EXCLUDES(vnode_set_mutex_);
  void RemoveFromVnodeSet(VnodeSet type, nid_t ino) __TA_EXCLUDES(vnode_set_mutex_);
  bool FindVnodeSet(VnodeSet type, nid_t ino) __TA_EXCLUDES(vnode_set_mutex_);
  size_t GetVnodeSetSize(VnodeSet type) __TA_EXCLUDES(vnode_set_mutex_);
  void ForAllVnodeSet(VnodeSet type, fit::function<void(nid_t)> callback)
      __TA_EXCLUDES(vnode_set_mutex_);
  void ClearVnodeSet() __TA_EXCLUDES(vnode_set_mutex_);

  fs::SharedMutex &GetFsLock(LockType type) { return fs_lock_[static_cast<int>(type)]; }
  void mutex_lock_op(LockType t) __TA_ACQUIRE(&fs_lock_[static_cast<int>(t)]) {
    fs_lock_[static_cast<int>(t)].lock();
  }
  void mutex_unlock_op(LockType t) __TA_RELEASE(&fs_lock_[static_cast<int>(t)]) {
    fs_lock_[static_cast<int>(t)].unlock();
  }

 private:
  void StartMemoryPressureWatcher();

  // Flush all dirty meta Pages.
  pgoff_t FlushDirtyMetaPages(bool is_commit);
  // Flush all dirty data Pages that meet |operation|.if_vnode and if_page.
  pgoff_t FlushDirtyDataPages(WritebackOperation &operation, bool wait_writer = false);

  std::mutex checkpoint_mutex_;
  std::atomic_flag teardown_flag_ = ATOMIC_FLAG_INIT;
  std::atomic_flag stop_reclaim_flag_ = ATOMIC_FLAG_INIT;
  std::binary_semaphore writeback_flag_{1};

  FuchsiaDispatcher dispatcher_;
  PlatformVfs *const vfs_ = nullptr;
  std::unique_ptr<f2fs::BcacheMapper> bc_;
  // for unittest
  std::unique_ptr<PlatformVfs> vfs_for_tests_;

  MountOptions mount_options_;

  std::unique_ptr<SuperblockInfo> superblock_info_;
  std::unique_ptr<SegmentManager> segment_manager_;
  std::unique_ptr<NodeManager> node_manager_;
  std::unique_ptr<GcManager> gc_manager_;

  std::unique_ptr<Reader> reader_;
  std::unique_ptr<Writer> writer_;

  std::unique_ptr<VnodeF2fs> meta_vnode_;
  std::unique_ptr<VnodeF2fs> node_vnode_;
  fbl::RefPtr<VnodeF2fs> root_vnode_;

  VnodeCache vnode_cache_;
  DirEntryCache dir_entry_cache_;

  bool on_recovery_ = false;  // recovery is doing or not
  // for inode number management
  fs::SharedMutex vnode_set_mutex_;
  std::map<ino_t, uint32_t> vnode_set_ __TA_GUARDED(vnode_set_mutex_);
  size_t vnode_set_size_[static_cast<size_t>(VnodeSet::kMax)] __TA_GUARDED(vnode_set_mutex_) = {
      0,
  };

  fs::SharedMutex fs_lock_[static_cast<int>(LockType::kNrLockType)];  // for blocking FS operations

  zx::event fs_id_;
  std::unique_ptr<InspectTree> inspect_tree_;
  std::atomic<MemoryPressure> current_memory_pressure_level_ = MemoryPressure::kUnknown;
  std::unique_ptr<MemoryPressureWatcher> memory_pressure_watcher_;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_F2FS_H_
