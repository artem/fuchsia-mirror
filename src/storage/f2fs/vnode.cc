// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/stat.h>
#include <zircon/syscalls-next.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

VnodeF2fs::VnodeF2fs(F2fs *fs, ino_t ino, umode_t mode)
    : PagedVnode(*fs->vfs()),
      superblock_info_(fs->GetSuperblockInfo()),
      ino_(ino),
      fs_(fs),
      mode_(mode) {
  if (IsMeta() || IsNode()) {
    InitFileCache();
  }
  SetFlag(InodeInfoFlag::kInit);
  Activate();
}

fuchsia_io::NodeProtocolKinds VnodeF2fs::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kFile;
}

void VnodeF2fs::SetMode(const umode_t &mode) { mode_ = mode; }

umode_t VnodeF2fs::GetMode() const { return mode_; }

bool VnodeF2fs::IsDir() const { return S_ISDIR(mode_); }

bool VnodeF2fs::IsReg() const { return S_ISREG(mode_); }

bool VnodeF2fs::IsLink() const { return S_ISLNK(mode_); }

bool VnodeF2fs::IsChr() const { return S_ISCHR(mode_); }

bool VnodeF2fs::IsBlk() const { return S_ISBLK(mode_); }

bool VnodeF2fs::IsSock() const { return S_ISSOCK(mode_); }

bool VnodeF2fs::IsFifo() const { return S_ISFIFO(mode_); }

bool VnodeF2fs::HasGid() const { return mode_ & S_ISGID; }

bool VnodeF2fs::IsNode() const { return ino_ == superblock_info_.GetNodeIno(); }

bool VnodeF2fs::IsMeta() const { return ino_ == superblock_info_.GetMetaIno(); }

zx_status_t VnodeF2fs::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo *out_vmo) {
  std::lock_guard lock(mutex_);
  auto size_or = CreatePagedVmo(GetSize());
  if (size_or.is_error()) {
    return size_or.error_value();
  }
  return ClonePagedVmo(flags, *size_or, out_vmo);
}

zx::result<size_t> VnodeF2fs::CreatePagedVmo(size_t size) {
  if (!paged_vmo().is_valid()) {
    if (auto status = EnsureCreatePagedVmo(size, ZX_VMO_RESIZABLE | ZX_VMO_TRAP_DIRTY);
        status.is_error()) {
      return status.take_error();
    }
    SetPagedVmoName();
  }
  return zx::ok(size);
}

void VnodeF2fs::SetPagedVmoName() {
  fbl::StringBuffer<ZX_MAX_NAME_LEN> name;
  name.Clear();
  name.AppendPrintf("%s-%.8s-%u", "f2fs", name_.data(), GetKey());
  paged_vmo().set_property(ZX_PROP_NAME, name.data(), name.size());
}

zx_status_t VnodeF2fs::ClonePagedVmo(fuchsia_io::wire::VmoFlags flags, size_t size,
                                     zx::vmo *out_vmo) {
  if (!paged_vmo()) {
    return ZX_ERR_NOT_FOUND;
  }

  zx_rights_t rights = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHT_GET_PROPERTY;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kRead) ? ZX_RIGHT_READ : 0;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kWrite) ? ZX_RIGHT_WRITE : 0;

  uint32_t options = 0;
  if (flags & fuchsia_io::wire::VmoFlags::kPrivateClone) {
    options = ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE;
    // Allowed only on private vmo.
    rights |= ZX_RIGHT_SET_PROPERTY;
  } else {
    // |size| should be 0 with ZX_VMO_CHILD_REFERENCE.
    size = 0;
    options = ZX_VMO_CHILD_REFERENCE;
  }

  if (!(flags & fuchsia_io::wire::VmoFlags::kWrite)) {
    options |= ZX_VMO_CHILD_NO_WRITE;
  }

  zx::vmo vmo;
  if (auto status = paged_vmo().create_child(options, 0, size, &vmo); status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate VMO: " << zx_status_get_string(status);
    return status;
  }
  DidClonePagedVmo();

  if (auto status = vmo.replace(rights, &vmo); status != ZX_OK) {
    return status;
  }
  *out_vmo = std::move(vmo);
  return ZX_OK;
}

void VnodeF2fs::VmoRead(uint64_t offset, uint64_t length) {
  zx::vmo vmo;
  auto size_or = CreateAndPopulateVmo(vmo, offset, length);
  fs::SharedLock rlock(mutex_);
  if (unlikely(!paged_vmo())) {
    // Races with calling FreePagedVmo() on another thread can result in stale read requests. Ignore
    // them if the VMO is gone.
    FX_LOGS(WARNING) << "A pager-backed VMO is already freed: " << ZX_ERR_NOT_FOUND;
    return;
  }
  if (unlikely(size_or.is_error())) {
    return ReportPagerErrorUnsafe(ZX_PAGER_VMO_READ, offset, length, size_or.error_value());
  }
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  if (auto ret = vfs.value().get().SupplyPages(paged_vmo(), offset, *size_or, std::move(vmo), 0);
      ret.is_error()) {
    ReportPagerErrorUnsafe(ZX_PAGER_VMO_READ, offset, length, ret.error_value());
  }
}

block_t VnodeF2fs::GetReadBlockSize(block_t start_block, block_t req_size, block_t end_block) {
  block_t size = req_size;
  size = std::max(kDefaultReadaheadSize, size);
  if (start_block + size > end_block) {
    size = end_block - start_block;
  }
  return size;
}

zx::result<size_t> VnodeF2fs::CreateAndPopulateVmo(zx::vmo &vmo, const size_t offset,
                                                   const size_t length) {
  std::vector<bool> block_bitmap;
  size_t vmo_size = fbl::round_up(length, kBlockSize);
  size_t file_size = GetSize();
  const block_t num_requested_blocks = safemath::checked_cast<block_t>(vmo_size / kBlockSize);
  block_t num_read_blocks = num_requested_blocks;
  block_t start_block = safemath::checked_cast<block_t>(offset / kBlockSize);

  // Just supply pages for appended or inline area without read I/Os.
  if (!TestFlag(InodeInfoFlag::kInlineData) && !TestFlag(InodeInfoFlag::kNoAlloc) &&
      offset < file_size) {
    block_t end_block =
        safemath::checked_cast<block_t>(fbl::round_up(file_size, kBlockSize) / kBlockSize);
    num_read_blocks = GetReadBlockSize(start_block, num_read_blocks, end_block);
    // Get the information about which of blocks are subject to writeback.
    block_bitmap = file_cache_->GetDirtyPagesInfo(start_block, num_read_blocks);
    ZX_ASSERT(num_read_blocks == block_bitmap.size());
    vmo_size = num_read_blocks * kBlockSize;
  }

  // Create vmo to feed paged vmo.
  if (auto status = zx::vmo::create(vmo_size, 0, &vmo); status != ZX_OK) {
    return zx::error(status);
  }

  // Commit pages to |vmo|. Ensure that newly commited pages are set to zero.
  if (auto ret = vmo.op_range(ZX_VMO_OP_COMMIT, 0, vmo_size, nullptr, 0); ret != ZX_OK) {
    return zx::error(ret);
  }

  // Read blocks only for valid block addrs unless regarding pages are subject to writeback.
  if (block_bitmap.size()) {
    auto addrs = GetDataBlockAddresses(start_block, num_read_blocks, true);
    if (addrs.is_error()) {
      return addrs.take_error();
    }
    uint32_t i = 0;
    block_t actual_read_blocks = 0;
    for (auto &addr : *addrs) {
      if (!block_bitmap[i++]) {
        // no read IOs for dirty and writeback pages.
        addr = kNullAddr;
      } else if (addr != kNewAddr && addr != kNullAddr) {
        // the number of blocks to be read.
        ++actual_read_blocks;
      }

      if (i == num_requested_blocks && actual_read_blocks == 0) {
        // There is no need for read IO on the requested blocks, so we skip the readahead checks.
        break;
      }
    }
    if (actual_read_blocks) {
      if (auto status = fs()->MakeReadOperations(vmo, *addrs, PageType::kData); status.is_error()) {
        return status.take_error();
      }
    }
  }
  return zx::ok(vmo_size);
}

void VnodeF2fs::VmoDirty(uint64_t offset, uint64_t length) {
  fs::SharedLock lock(mutex_);
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  auto ret = vfs.value().get().DirtyPages(paged_vmo(), offset, length);
  if (ret.is_error()) {
    // If someone has already dirtied or truncated these pages, do nothing.
    if (ret.error_value() == ZX_ERR_NOT_FOUND) {
      return;
    }
    ReportPagerErrorUnsafe(ZX_PAGER_OP_DIRTY, offset, length, ret.error_value());
  }
}

void VnodeF2fs::OnNoPagedVmoClones() {
  // Override PagedVnode::OnNoPagedVmoClones().
  // We intend to keep PagedVnode::paged_vmo alive while this vnode has any reference.
  ZX_DEBUG_ASSERT(!has_clones());
}

void VnodeF2fs::ReportPagerError(const uint32_t op, const uint64_t offset, const uint64_t length,
                                 const zx_status_t err) {
  fs::SharedLock lock(mutex_);
  return ReportPagerErrorUnsafe(op, offset, length, err);
}

void VnodeF2fs::ReportPagerErrorUnsafe(const uint32_t op, const uint64_t offset,
                                       const uint64_t length, const zx_status_t err) {
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  zx_status_t pager_err = err;
  // Notifies the kernel that a page request for the given `range` has failed. Sent in response
  // to a `ZX_PAGER_VMO_READ` or `ZX_PAGER_VMO_DIRTY` page request. See `ZX_PAGER_OP_FAIL` for
  // more information.
  if (err != ZX_ERR_IO && err != ZX_ERR_IO_DATA_INTEGRITY && err != ZX_ERR_BAD_STATE &&
      err != ZX_ERR_NO_SPACE && err != ZX_ERR_BUFFER_TOO_SMALL) {
    pager_err = ZX_ERR_BAD_STATE;
  }
  FX_LOGS(WARNING) << "Failed to handle a pager request(" << std::hex << op << "). "
                   << zx_status_get_string(err);
  if (auto result = vfs.value().get().ReportPagerError(paged_vmo(), offset, length, pager_err);
      result.is_error()) {
    FX_LOGS(ERROR) << "Failed to report a pager error. " << result.status_string();
  }
}

zx_status_t VnodeF2fs::Allocate(F2fs *fs, ino_t ino, umode_t mode, fbl::RefPtr<VnodeF2fs> *out) {
  if (fs->GetNodeManager().CheckNidRange(ino)) {
    if (S_ISDIR(mode)) {
      *out = fbl::MakeRefCounted<Dir>(fs, ino, mode);
    } else {
      *out = fbl::MakeRefCounted<File>(fs, ino, mode);
    }
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t VnodeF2fs::Create(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out) {
  LockedPage node_page;
  SuperblockInfo &superblock_info = fs->GetSuperblockInfo();
  if (ino < superblock_info.GetRootIno() || !fs->GetNodeManager().CheckNidRange(ino) ||
      fs->GetNodeManager().GetNodePage(ino, &node_page) != ZX_OK) {
    return ZX_ERR_NOT_FOUND;
  }

  Inode &inode = node_page->GetAddress<Node>()->i;
  std::string_view name(reinterpret_cast<char *>(inode.i_name),
                        std::min(kMaxNameLen, inode.i_namelen));
  if (inode.i_namelen != name.length() ||
      (ino != superblock_info.GetRootIno() && !fs::IsValidName(name))) {
    // TODO: Need to repair the file or set NeedFsck flag when fsck supports repair feature.
    // For now, we set kBad and clear link, so that it can be deleted without purging.
    return ZX_ERR_NOT_FOUND;
  }
  fbl::RefPtr<VnodeF2fs> vnode;
  if (zx_status_t status = Allocate(fs, ino, LeToCpu(inode.i_mode), &vnode); status != ZX_OK) {
    return status;
  }

  vnode->Init(node_page);
  *out = std::move(vnode);
  return ZX_OK;
}

void VnodeF2fs::RecycleNode() TA_NO_THREAD_SAFETY_ANALYSIS {
  ZX_ASSERT_MSG(open_count() == 0, "RecycleNode[%s:%u]: open_count must be zero (%lu)",
                GetNameView().data(), GetKey(), open_count());
  if (GetNlink()) {
    // It should not happen since f2fs removes the last reference of dirty vnodes at checkpoint time
    // during which any file operations are not allowed.
    if (GetDirtyPageCount()) {
      // It can happen only when CpFlag::kCpErrorFlag is set or with tests.
      FX_LOGS(WARNING) << "Vnode[" << GetNameView().data() << ":" << GetKey()
                       << "] is deleted with " << GetDirtyPageCount() << " of dirty pages"
                       << ". CpFlag::kCpErrorFlag is "
                       << (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag) ? "set."
                                                                              : "not set.");
    }
    file_cache_->Reset();
    fs()->GetVCache().Downgrade(this);
  } else {
    // During PagedVfs::Teardown, f2fs object is not available. In this case, we purge orphans at
    // next mount time.
    if (!fs()->IsTearDown()) {
      EvictVnode();
    }
    Deactivate();
    file_cache_->Reset();
    ReleasePagedVmoUnsafe();
    delete this;
  }
}

zx_status_t VnodeF2fs::GetAttributes(fs::VnodeAttributes *a) {
  *a = fs::VnodeAttributes();

  fs::SharedLock rlock(mutex_);
  a->mode = mode_;
  a->inode = ino_;
  a->content_size = GetSize();
  a->storage_size = GetBlockCount() * kBlockSize;
  a->link_count = nlink_;
  const auto &btime = GetTime<Timestamps::BirthTime>();
  const auto &mtime = GetTime<Timestamps::ModificationTime>();
  a->creation_time = zx_time_add_duration(ZX_SEC(btime.tv_sec), btime.tv_nsec);
  a->modification_time = zx_time_add_duration(ZX_SEC(mtime.tv_sec), mtime.tv_nsec);

  return ZX_OK;
}

zx_status_t VnodeF2fs::SetAttributes(fs::VnodeAttributesUpdate attr) {
  bool need_inode_sync = false;

  if (attr.has_creation_time()) {
    SetTime<Timestamps::BirthTime>(zx_timespec_from_duration(
        safemath::checked_cast<zx_duration_t>(attr.take_creation_time())));
    need_inode_sync = true;
  }
  if (attr.has_modification_time()) {
    SetTime<Timestamps::ModificationTime>(zx_timespec_from_duration(
        safemath::checked_cast<zx_duration_t>(attr.take_modification_time())));
    need_inode_sync = true;
  }

  if (attr.any()) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (need_inode_sync) {
    SetDirty();
  }

  return ZX_OK;
}

struct f2fs_iget_args {
  uint64_t ino;
  int on_free;
};

#if 0  // porting needed
// void VnodeF2fs::F2fsSetInodeFlags() {
  // uint64_t &flags = fi.i_flags;

  // inode_.i_flags &= ~(S_SYNC | S_APPEND | S_IMMUTABLE |
  //     S_NOATIME | S_DIRSYNC);

  // if (flags & FS_SYNC_FL)
  //   inode_.i_flags |= S_SYNC;
  // if (flags & FS_APPEND_FL)
  //   inode_.i_flags |= S_APPEND;
  // if (flags & FS_IMMUTABLE_FL)
  //   inode_.i_flags |= S_IMMUTABLE;
  // if (flags & FS_NOATIME_FL)
  //   inode_.i_flags |= S_NOATIME;
  // if (flags & FS_DIRSYNC_FL)
  //   inode_.i_flags |= S_DIRSYNC;
// }

// int VnodeF2fs::F2fsIgetTest(void *data) {
  // f2fs_iget_args *args = (f2fs_iget_args *)data;

  // if (ino_ != args->ino)
  //   return 0;
  // if (i_state & (I_FREEING | I_WILL_FREE)) {
  //   args->on_free = 1;
  //   return 0;
  // }
  // return 1;
// }

// VnodeF2fs *VnodeF2fs::F2fsIgetNowait(uint64_t ino) {
//   fbl::RefPtr<VnodeF2fs> vnode_refptr;
//   VnodeF2fs *vnode = nullptr;
//   f2fs_iget_args args = {.ino = ino, .on_free = 0};
//   vnode = ilookup5(sb, ino, F2fsIgetTest, &args);

//   if (vnode)
//     return vnode;
//   if (!args.on_free) {
//     Vget(fs(), ino, &vnode_refptr);
//     vnode = vnode_refptr.get();
//     return vnode;
//   }
//   return static_cast<VnodeF2fs *>(ErrPtr(ZX_ERR_NOT_FOUND));
// }
#endif

zx_status_t VnodeF2fs::Vget(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out) {
  fbl::RefPtr<VnodeF2fs> vnode;
  if (fs->LookupVnode(ino, &vnode) == ZX_OK) {
    vnode->WaitForInit();
    *out = std::move(vnode);
    return ZX_OK;
  }

  if (zx_status_t status = Create(fs, ino, &vnode); status != ZX_OK) {
    return status;
  }

  // VnodeCache should keep vnodes holding valid links except when purging orphans at mount time.
  if (ino != fs->GetSuperblockInfo().GetNodeIno() && ino != fs->GetSuperblockInfo().GetMetaIno() &&
      (!fs->IsOnRecovery() && !vnode->GetNlink())) {
    vnode->SetFlag(InodeInfoFlag::kBad);
    return ZX_ERR_NOT_FOUND;
  }

  if (zx_status_t status = fs->InsertVnode(vnode.get()); status != ZX_OK) {
    ZX_ASSERT(status == ZX_ERR_ALREADY_EXISTS);
    vnode->SetFlag(InodeInfoFlag::kBad);
    vnode.reset();
    ZX_ASSERT(fs->LookupVnode(ino, &vnode) == ZX_OK);
    vnode->WaitForInit();
    *out = std::move(vnode);
    return ZX_OK;
  }

  vnode->UnlockNewInode();
  *out = std::move(vnode);
  return ZX_OK;
}

void VnodeF2fs::UpdateInodePage(LockedPage &inode_page) {
  inode_page->WaitOnWriteback();
  fs::SharedLock lock(mutex_);
  Inode &inode = inode_page->GetAddress<Node>()->i;

  inode.i_mode = CpuToLe(GetMode());
  inode.i_advise = advise_;
  inode.i_uid = CpuToLe(uid_);
  inode.i_gid = CpuToLe(gid_);
  inode.i_links = CpuToLe(GetNlink());
  inode.i_size = CpuToLe(GetSize());
  // For on-disk i_blocks, we keep counting inode block for backward compatibility.
  inode.i_blocks = CpuToLe(safemath::CheckAdd<uint64_t>(GetBlocks(), 1).ValueOrDie());

  if (ExtentCacheAvailable()) {
    auto extent_info = extent_tree_->GetLargestExtent();
    inode.i_ext.blk_addr = CpuToLe(extent_info.blk_addr);
    inode.i_ext.fofs = CpuToLe(static_cast<uint32_t>(extent_info.fofs));
    inode.i_ext.len = CpuToLe(extent_info.len);
  } else {
    std::memset(&inode.i_ext, 0, sizeof(inode.i_ext));
  }

  // TODO(b/297201368): As there is no space for creation time, it temporarily considers ctime as
  // creation time.
  const timespec &atime = time_->Get<Timestamps::AccessTime>();
  const timespec &ctime = time_->Get<Timestamps::BirthTime>();
  const timespec &mtime = time_->Get<Timestamps::ModificationTime>();

  inode.i_atime = CpuToLe(static_cast<uint64_t>(atime.tv_sec));
  inode.i_ctime = CpuToLe(static_cast<uint64_t>(ctime.tv_sec));
  inode.i_mtime = CpuToLe(static_cast<uint64_t>(mtime.tv_sec));
  inode.i_atime_nsec = CpuToLe(static_cast<uint32_t>(atime.tv_nsec));
  inode.i_ctime_nsec = CpuToLe(static_cast<uint32_t>(ctime.tv_nsec));
  inode.i_mtime_nsec = CpuToLe(static_cast<uint32_t>(mtime.tv_nsec));
  inode.i_current_depth = CpuToLe(static_cast<uint32_t>(current_depth_));
  inode.i_xattr_nid = CpuToLe(xattr_nid_);
  inode.i_flags = CpuToLe(inode_flags_);
  inode.i_pino = CpuToLe(GetParentNid());
  inode.i_generation = CpuToLe(generation_);
  inode.i_dir_level = dir_level_;

  std::string_view name(name_);
  // double check |name|
  ZX_DEBUG_ASSERT(IsValidNameLength(name));
  auto size = safemath::checked_cast<uint32_t>(name.size());
  inode.i_namelen = CpuToLe(size);
  name.copy(reinterpret_cast<char *>(&inode.i_name[0]), size);

  if (TestFlag(InodeInfoFlag::kInlineData)) {
    inode.i_inline |= kInlineData;
  } else {
    inode.i_inline &= ~kInlineData;
  }
  if (TestFlag(InodeInfoFlag::kInlineDentry)) {
    inode.i_inline |= kInlineDentry;
  } else {
    inode.i_inline &= ~kInlineDentry;
  }
  if (extra_isize_) {
    inode.i_inline |= kExtraAttr;
    inode.i_extra_isize = extra_isize_;
    if (TestFlag(InodeInfoFlag::kInlineXattr)) {
      inode.i_inline_xattr_size = CpuToLe(inline_xattr_size_);
    }
  }
  if (TestFlag(InodeInfoFlag::kDataExist)) {
    inode.i_inline |= kDataExist;
  } else {
    inode.i_inline &= ~kDataExist;
  }
  if (TestFlag(InodeInfoFlag::kInlineXattr)) {
    inode.i_inline |= kInlineXattr;
  } else {
    inode.i_inline &= ~kInlineXattr;
  }

  inode_page.SetDirty();
}

zx_status_t VnodeF2fs::DoTruncate(size_t len) {
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    if (zx_status_t ret = TruncateBlocks(len); ret != ZX_OK) {
      return ret;
    }
  }
  // SetSize() adjusts the size of its vmo or vmo content, and then the kernel guarantees
  // that its vmo after |len| are zeroed. If necessary, it triggers VmoDirty() to let f2fs write
  // changes to disk.
  SetSize(len);
  if (GetSize() == 0) {
    ClearFlag(InodeInfoFlag::kDataExist);
  }

  SetTime<Timestamps::ModificationTime>();
  SetDirty();
  return ZX_OK;
}

zx_status_t VnodeF2fs::TruncateBlocks(uint64_t from) {
  uint32_t blocksize = superblock_info_.GetBlocksize();
  if (from > GetSize()) {
    return ZX_OK;
  }

  pgoff_t free_from =
      safemath::CheckRsh(fbl::round_up(from, blocksize), superblock_info_.GetLogBlocksize())
          .ValueOrDie();
  // Invalidate data pages starting from |free_from|. It safely removes any pages updating their
  // block addrs before purging the addrs in nodes.
  InvalidatePages(free_from);
  {
    auto path_or = GetNodePath(*this, free_from);
    if (path_or.is_error()) {
      return path_or.error_value();
    }
    auto node_page_or = fs()->GetNodeManager().FindLockedDnodePage(*path_or);
    if (node_page_or.is_ok()) {
      size_t ofs_in_node = GetOfsInDnode(*path_or);
      // If |from| starts from inode or the middle of dnode, purge the addrs in the start dnode.
      NodePage &node = (*node_page_or).GetPage<NodePage>();
      if (ofs_in_node || node.IsInode()) {
        size_t count = 0;
        if (node.IsInode()) {
          count = safemath::CheckSub(GetAddrsPerInode(), ofs_in_node).ValueOrDie();
        } else {
          count = safemath::CheckSub(kAddrsPerBlock, ofs_in_node).ValueOrDie();
        }
        TruncateDnodeAddrs(*node_page_or, ofs_in_node, count);
        free_from += count;
      }
    } else if (node_page_or.error_value() != ZX_ERR_NOT_FOUND) {
      return node_page_or.error_value();
    }
  }

  // Invalidate the rest nodes.
  if (zx_status_t err = TruncateInodeBlocks(free_from); err != ZX_OK) {
    return err;
  }
  return ZX_OK;
}

zx_status_t VnodeF2fs::TruncateHole(pgoff_t pg_start, pgoff_t pg_end, bool zero) {
  InvalidatePages(pg_start, pg_end, zero);
  for (pgoff_t index = pg_start; index < pg_end; ++index) {
    auto path_or = GetNodePath(*this, index);
    if (path_or.is_error()) {
      if (path_or.error_value() == ZX_ERR_NOT_FOUND) {
        continue;
      }
      return path_or.error_value();
    }
    auto page_or = fs()->GetNodeManager().GetLockedDnodePage(*path_or, IsDir());
    if (page_or.is_error()) {
      if (page_or.error_value() == ZX_ERR_NOT_FOUND) {
        continue;
      }
      return page_or.error_value();
    }
    IncBlocks(path_or->num_new_nodes);
    LockedPage dnode_page = std::move(*page_or);
    size_t ofs_in_dnode = GetOfsInDnode(*path_or);
    NodePage &node = dnode_page.GetPage<NodePage>();
    if (node.GetBlockAddr(ofs_in_dnode) != kNullAddr) {
      TruncateDnodeAddrs(dnode_page, ofs_in_dnode, 1);
    }
  }
  return ZX_OK;
}

void VnodeF2fs::TruncateToSize() {
  if (!(IsDir() || IsReg() || IsLink()))
    return;

  if (zx_status_t ret = TruncateBlocks(GetSize()); ret == ZX_OK) {
    SetTime<Timestamps::ModificationTime>();
  }
}

void VnodeF2fs::ReleasePagedVmo() {
  std::lock_guard lock(mutex_);
  ReleasePagedVmoUnsafe();
}

void VnodeF2fs::ReleasePagedVmoUnsafe() {
  if (paged_vmo()) {
    fbl::RefPtr<fs::Vnode> pager_reference = FreePagedVmo();
    ZX_DEBUG_ASSERT(!pager_reference);
  }
}

void VnodeF2fs::EvictVnode() {
  if (ino_ == superblock_info_.GetNodeIno() || ino_ == superblock_info_.GetMetaIno()) {
    return;
  }

  if (GetNlink() || IsBad()) {
    return;
  }

  SetFlag(InodeInfoFlag::kNoAlloc);
  SetSize(0);
  if (HasBlocks()) {
    TruncateToSize();
  }
  RemoveInodePage();
  fs()->EvictVnode(this);
}

void VnodeF2fs::InitFileCache(uint64_t nbytes) {
  std::lock_guard lock(mutex_);
  InitFileCacheUnsafe(nbytes);
}

void VnodeF2fs::InitFileCacheUnsafe(uint64_t nbytes) {
  zx::vmo vmo;
  VmoMode mode;
  size_t vmo_node_size = 0;

  ZX_DEBUG_ASSERT(!file_cache_);
  if (IsReg()) {
    if (auto size_or = CreatePagedVmo(nbytes); size_or.is_ok()) {
      zx_rights_t right = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHTS_PROPERTY | ZX_RIGHT_READ |
                          ZX_RIGHT_WRITE | ZX_RIGHT_RESIZE;
      ZX_ASSERT(paged_vmo().duplicate(right, &vmo) == ZX_OK);
      mode = VmoMode::kPaged;
      vmo_node_size = zx_system_get_page_size();
    }
  } else {
    mode = VmoMode::kDiscardable;
    vmo_node_size = kVmoNodeSize;
  }
  ZX_ASSERT(!(zx_system_get_page_size() % kBlockSize));
  ZX_ASSERT(!(vmo_node_size % zx_system_get_page_size()));
  vmo_manager_ = std::make_unique<VmoManager>(mode, nbytes, vmo_node_size, std::move(vmo));
  file_cache_ = std::make_unique<FileCache>(this, vmo_manager_.get());
}

void VnodeF2fs::InitTime() {
  std::lock_guard lock(mutex_);
  timespec cur;
  clock_gettime(CLOCK_REALTIME, &cur);
  time_ = Timestamps(UpdateMode::kRelative, cur, cur, cur, cur);
}

void VnodeF2fs::Init(LockedPage &node_page) {
  std::lock_guard lock(mutex_);
  Inode &inode = node_page->GetAddress<Node>()->i;
  std::string_view name(reinterpret_cast<char *>(inode.i_name),
                        std::min(kMaxNameLen, inode.i_namelen));

  name_ = name;
  uid_ = LeToCpu(inode.i_uid);
  gid_ = LeToCpu(inode.i_gid);
  SetNlink(LeToCpu(inode.i_links));
  // Don't count the in-memory inode.i_blocks for compatibility with the generic
  // filesystem including linux f2fs.
  SetBlocks(safemath::CheckSub<uint64_t>(LeToCpu(inode.i_blocks), 1).ValueOrDie());
  const timespec atime = {static_cast<time_t>(LeToCpu(inode.i_atime)),
                          static_cast<time_t>(LeToCpu(inode.i_atime_nsec))};
  // TODO(b/297201368): As there is no space for creation time, it temporarily considers ctime as
  // creation time.
  const timespec btime = {static_cast<time_t>(LeToCpu(inode.i_ctime)),
                          static_cast<time_t>(LeToCpu(inode.i_ctime_nsec))};
  const timespec mtime = {static_cast<time_t>(LeToCpu(inode.i_mtime)),
                          static_cast<time_t>(LeToCpu(inode.i_mtime_nsec))};
  time_ = Timestamps(UpdateMode::kRelative, atime, btime, mtime, mtime);
  generation_ = LeToCpu(inode.i_generation);
  SetParentNid(LeToCpu(inode.i_pino));
  current_depth_ = LeToCpu(inode.i_current_depth);
  xattr_nid_ = LeToCpu(inode.i_xattr_nid);
  inode_flags_ = LeToCpu(inode.i_flags);
  dir_level_ = inode.i_dir_level;
  data_version_ = superblock_info_.GetCheckpointVer() - 1;
  advise_ = inode.i_advise;

  if (inode.i_inline & kInlineDentry) {
    SetFlag(InodeInfoFlag::kInlineDentry);
    inline_xattr_size_ = kInlineXattrAddrs;
  }
  if (inode.i_inline & kInlineData) {
    SetFlag(InodeInfoFlag::kInlineData);
  }
  if (inode.i_inline & kInlineXattr) {
    SetFlag(InodeInfoFlag::kInlineXattr);
    inline_xattr_size_ = kInlineXattrAddrs;
  }
  if (inode.i_inline & kExtraAttr) {
    extra_isize_ = LeToCpu(inode.i_extra_isize);
    if (inode.i_inline & kInlineXattr) {
      inline_xattr_size_ = LeToCpu(inode.i_inline_xattr_size);
    }
  }
  if (inode.i_inline & kDataExist) {
    SetFlag(InodeInfoFlag::kDataExist);
  }
  InitExtentTree();
  if (extent_tree_ && inode.i_ext.blk_addr) {
    auto extent_info = ExtentInfo{.fofs = LeToCpu(inode.i_ext.fofs),
                                  .blk_addr = LeToCpu(inode.i_ext.blk_addr),
                                  .len = LeToCpu(inode.i_ext.len)};
    if (auto result = extent_tree_->InsertExtent(extent_info); result.is_error()) {
      SetFlag(InodeInfoFlag::kNoExtent);
    }
  }

  // If the roll-forward recovery creates it, it will initialize its cache from the latest inode
  // block later.
  if (!fs()->IsOnRecovery() || !GetNlink()) {
    InitFileCacheUnsafe(LeToCpu(inode.i_size));
  }
}

bool VnodeF2fs::SetDirty() {
  if (IsNode() || IsMeta() || !IsValid()) {
    return false;
  }
  return fs()->GetVCache().AddDirty(*this) == ZX_OK;
}

bool VnodeF2fs::ClearDirty() { return fs()->GetVCache().RemoveDirty(this) == ZX_OK; }

bool VnodeF2fs::IsDirty() { return fs()->GetVCache().IsDirty(*this); }

void VnodeF2fs::Sync(SyncCallback closure) {
  closure(SyncFile(0, safemath::checked_cast<loff_t>(GetSize()), 0));
}

bool VnodeF2fs::NeedToCheckpoint() {
  if (!IsReg()) {
    return true;
  }
  if (GetNlink() != 1) {
    return true;
  }
  if (TestFlag(InodeInfoFlag::kNeedCp)) {
    return true;
  }
  if (!superblock_info_.SpaceForRollForward()) {
    return true;
  }
  if (NeedToSyncDir()) {
    return true;
  }
  if (superblock_info_.TestOpt(MountOption::kDisableRollForward)) {
    return true;
  }
  if (fs()->FindVnodeSet(VnodeSet::kModifiedDir, GetParentNid())) {
    return true;
  }
  return false;
}

void VnodeF2fs::SetSize(const size_t nbytes) {
  ZX_ASSERT(vmo_manager_);
  vmo_manager().SetContentSize(nbytes);
}

uint64_t VnodeF2fs::GetSize() const {
  ZX_ASSERT(vmo_manager_);
  return vmo_manager().GetContentSize();
}

zx_status_t VnodeF2fs::SyncFile(loff_t start, loff_t end, int datasync) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  // TODO: When fdatasync is available, check if it should be written.
  if (!IsDirty()) {
    return ZX_OK;
  }

  WritebackOperation op = {.bSync = true};
  Writeback(op);

  bool need_cp = NeedToCheckpoint();
  if (need_cp) {
    fs()->SyncFs();
    ClearFlag(InodeInfoFlag::kNeedCp);
    if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
      return ZX_ERR_BAD_STATE;
    }
  } else {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    {
      LockedPage page;
      if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(ino_, &page); ret != ZX_OK) {
        return ret;
      }
      UpdateInodePage(page);
    }
    fs()->GetNodeManager().FsyncNodePages(Ino());
    // TODO: Add flags to log recovery information to NAT entries and decide whether to write
    // inode or not.
  }
  return ZX_OK;
}

bool VnodeF2fs::NeedToSyncDir() const {
  ZX_ASSERT(GetParentNid() < kNullIno);
  return !fs()->GetNodeManager().IsCheckpointedNode(GetParentNid());
}

void VnodeF2fs::Notify(std::string_view name, fuchsia_io::wire::WatchEvent event) {
  watcher_.Notify(name, event);
}

zx_status_t VnodeF2fs::WatchDir(fs::FuchsiaVfs *vfs, fuchsia_io::wire::WatchMask mask,
                                uint32_t options,
                                fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) {
  return watcher_.WatchDir(vfs, this, mask, options, std::move(watcher));
}

bool VnodeF2fs::ExtentCacheAvailable() {
  return superblock_info_.TestOpt(MountOption::kReadExtentCache) && IsReg() &&
         !TestFlag(InodeInfoFlag::kNoExtent);
}

void VnodeF2fs::InitExtentTree() {
  if (!ExtentCacheAvailable()) {
    return;
  }

  // Because the lifecycle of an extent_tree is tied to the lifecycle of a vnode, the extent tree
  // should not exist when the vnode is created.
  ZX_DEBUG_ASSERT(!extent_tree_);
  extent_tree_ = std::make_unique<ExtentTree>();
}

void VnodeF2fs::Activate() { SetFlag(InodeInfoFlag::kActive); }

void VnodeF2fs::Deactivate() {
  if (IsActive()) {
    ClearFlag(InodeInfoFlag::kActive);
    flag_cvar_.notify_all();
  }
}
void VnodeF2fs::WaitForDeactive(std::mutex &mutex) {
  if (IsActive()) {
    flag_cvar_.wait(mutex, [this]() { return !IsActive(); });
  }
}

bool VnodeF2fs::IsActive() const { return TestFlag(InodeInfoFlag::kActive); }

zx::result<PageBitmap> VnodeF2fs::GetBitmap(fbl::RefPtr<Page> page) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

void VnodeF2fs::SetOrphan() {
  // Clean the current dirty pages and set the orphan flag that prevents additional dirty pages.
  if (!file_cache_->SetOrphan()) {
    file_cache_->ClearDirtyPages();
    fs()->AddToVnodeSet(VnodeSet::kOrphan, GetKey());
    if (IsDir()) {
      Notify(".", fuchsia_io::wire::WatchEvent::kDeleted);
    }
    ClearDirty();
    // Update the inode pages of orphans to be logged on disk.
    LockedPage node_page;
    ZX_ASSERT(fs()->GetNodeManager().GetNodePage(GetKey(), &node_page) == ZX_OK);
    UpdateInodePage(node_page);
  }
}

void VnodeF2fs::TruncateNode(LockedPage &page) {
  nid_t nid = static_cast<nid_t>(page->GetKey());
  fs_->GetNodeManager().TruncateNode(nid);
  if (nid == Ino()) {
    fs_->RemoveFromVnodeSet(VnodeSet::kOrphan, nid);
    superblock_info_.DecValidInodeCount();
  } else {
    DecBlocks(1);
    SetDirty();
  }
  page->Invalidate();
  superblock_info_.SetDirty();
}

block_t VnodeF2fs::TruncateDnodeAddrs(LockedPage &dnode, size_t offset, size_t count) {
  block_t nr_free = 0;
  NodePage &node = dnode.GetPage<NodePage>();
  for (; count > 0; --count, ++offset) {
    block_t blkaddr = node.GetBlockAddr(offset);
    if (blkaddr == kNullAddr) {
      continue;
    }
    node.SetDataBlkaddr(offset, kNullAddr);
    UpdateExtentCache(node.StartBidxOfNode(GetAddrsPerInode()) + offset, kNullAddr);
    ++nr_free;
    if (blkaddr != kNewAddr) {
      fs()->GetSegmentManager().InvalidateBlocks(blkaddr);
    }
  }
  if (nr_free) {
    fs()->GetSuperblockInfo().DecValidBlockCount(nr_free);
    DecBlocks(nr_free);
    node.SetDirty();
    SetDirty();
  }
  return nr_free;
}

zx::result<size_t> VnodeF2fs::TruncateDnode(nid_t nid) {
  if (!nid) {
    return zx::ok(1);
  }

  LockedPage page;
  // get direct node
  if (zx_status_t err = fs_->GetNodeManager().GetNodePage(nid, &page); err != ZX_OK) {
    // It is already invalid.
    if (err == ZX_ERR_NOT_FOUND) {
      return zx::ok(1);
    }
    return zx::error(err);
  }

  TruncateDnodeAddrs(page, 0, kAddrsPerBlock);
  TruncateNode(page);
  return zx::ok(1);
}

zx::result<size_t> VnodeF2fs::TruncateNodes(nid_t start_nid, size_t nofs, size_t ofs,
                                            size_t depth) {
  ZX_DEBUG_ASSERT(depth == 2 || depth == 3);
  if (unlikely(depth < 2 || depth > 3)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  constexpr size_t kInvalidatedNids = kNidsPerBlock + 1;
  if (start_nid == 0) {
    return zx::ok(kInvalidatedNids);
  }

  LockedPage page;
  if (zx_status_t ret = fs_->GetNodeManager().GetNodePage(start_nid, &page); ret != ZX_OK) {
    if (ret != ZX_ERR_NOT_FOUND) {
      return zx::error(ret);
    }
    if (depth == 2) {
      return zx::ok(kInvalidatedNids);
    }
    return zx::ok(kInvalidatedNids * kNidsPerBlock + 1);
  }

  size_t child_nofs = 0, freed = 0;
  nid_t child_nid;
  IndirectNode &indirect_node = page->GetAddress<Node>()->in;
  if (depth < 3) {
    for (size_t i = ofs; i < kNidsPerBlock; ++i, ++freed) {
      child_nid = LeToCpu(indirect_node.nid[i]);
      if (child_nid == 0) {
        continue;
      }
      if (auto ret = TruncateDnode(child_nid); ret.is_error()) {
        return ret;
      }
      ZX_ASSERT(!page.GetPage<NodePage>().IsInode());
      page.GetPage<NodePage>().SetNid(i, 0);
      page.SetDirty();
    }
  } else {
    child_nofs = nofs + ofs * kInvalidatedNids + 1;
    for (size_t i = ofs; i < kNidsPerBlock; ++i) {
      child_nid = LeToCpu(indirect_node.nid[i]);
      auto freed_or = TruncateNodes(child_nid, child_nofs, 0, depth - 1);
      if (freed_or.is_error()) {
        return freed_or.take_error();
      }
      ZX_DEBUG_ASSERT(*freed_or == kInvalidatedNids);
      ZX_DEBUG_ASSERT(!page.GetPage<NodePage>().IsInode());
      page.GetPage<NodePage>().SetNid(i, 0);
      page.SetDirty();
      child_nofs += kInvalidatedNids;
      freed += kInvalidatedNids;
    }
  }

  if (!ofs) {
    TruncateNode(page);
    ++freed;
  }
  return zx::ok(freed);
}

zx_status_t VnodeF2fs::TruncatePartialNodes(const Inode &inode, const size_t (&offset)[4],
                                            size_t depth) {
  LockedPage pages[2];
  nid_t nid[3];
  size_t idx = depth - 2;

  if (nid[0] = LeToCpu(inode.i_nid[offset[0] - kNodeDir1Block]); !nid[0]) {
    return ZX_OK;
  }

  // get indirect nodes in the path
  for (size_t i = 0; i < idx + 1; ++i) {
    if (auto ret = fs_->GetNodeManager().GetNodePage(nid[i], &pages[i]); ret != ZX_OK) {
      return ret;
    }
    nid[i + 1] = pages[i].GetPage<NodePage>().GetNid(offset[i + 1]);
  }

  // free direct nodes linked to a partial indirect node
  for (auto i = offset[idx + 1]; i < kNidsPerBlock; ++i) {
    nid_t child_nid = pages[idx].GetPage<NodePage>().GetNid(i);
    if (!child_nid) {
      continue;
    }
    if (auto ret = TruncateDnode(child_nid); ret.is_error()) {
      return ret.error_value();
    }
    ZX_ASSERT(!pages[idx].GetPage<NodePage>().IsInode());
    pages[idx].GetPage<NodePage>().SetNid(i, 0);
    pages[idx].SetDirty();
  }

  if (offset[idx + 1] == 0) {
    TruncateNode(pages[idx]);
  }
  return ZX_OK;
}

// All the block addresses of data and nodes should be nullified.
zx_status_t VnodeF2fs::TruncateInodeBlocks(pgoff_t from) {
  auto node_path = GetNodePath(*this, from);
  if (node_path.is_error()) {
    return node_path.error_value();
  }

  const size_t level = node_path->depth;
  const size_t(&node_offsets)[kMaxNodeBlockLevel] = node_path->node_offset;
  size_t(&offsets_in_node)[kMaxNodeBlockLevel] = node_path->offset_in_node;
  size_t node_offset = 0;

  LockedPage locked_ipage;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(Ino(), &locked_ipage); ret != ZX_OK) {
    return ret;
  }
  locked_ipage->WaitOnWriteback();
  Inode &inode = locked_ipage->GetAddress<Node>()->i;
  switch (level) {
    case 0:
      node_offset = 1;
      break;
    case 1:
      node_offset = node_offsets[1];
      break;
    case 2:
      node_offset = node_offsets[1];
      if (!offsets_in_node[1]) {
        break;
      }
      if (zx_status_t ret = TruncatePartialNodes(inode, offsets_in_node, level);
          ret != ZX_OK && ret != ZX_ERR_NOT_FOUND) {
        return ret;
      }
      ++offsets_in_node[level - 2];
      offsets_in_node[level - 1] = 0;
      node_offset += 1 + kNidsPerBlock;
      break;
    case 3:
      node_offset = 5 + 2 * kNidsPerBlock;
      if (!offsets_in_node[2]) {
        break;
      }
      if (zx_status_t ret = TruncatePartialNodes(inode, offsets_in_node, level);
          ret != ZX_OK && ret != ZX_ERR_NOT_FOUND) {
        return ret;
      }
      ++offsets_in_node[level - 2];
      offsets_in_node[level - 1] = 0;
      break;
    default:
      ZX_ASSERT(0);
  }

  bool run = true;
  while (run) {
    zx::result<size_t> freed_or;
    nid_t nid = LeToCpu(inode.i_nid[offsets_in_node[0] - kNodeDir1Block]);
    switch (offsets_in_node[0]) {
      case kNodeDir1Block:
      case kNodeDir2Block:
        freed_or = TruncateDnode(nid);
        break;

      case kNodeInd1Block:
      case kNodeInd2Block:
        freed_or = TruncateNodes(nid, node_offset, offsets_in_node[1], 2);
        break;

      case kNodeDIndBlock:
        freed_or = TruncateNodes(nid, node_offset, offsets_in_node[1], 3);
        run = false;
        break;

      default:
        ZX_ASSERT(0);
    }
    if (freed_or.is_error()) {
      ZX_DEBUG_ASSERT(freed_or.error_value() != ZX_ERR_NOT_FOUND);
      return freed_or.error_value();
    }
    if (offsets_in_node[1] == 0) {
      inode.i_nid[offsets_in_node[0] - kNodeDir1Block] = 0;
      locked_ipage.SetDirty();
    }
    offsets_in_node[1] = 0;
    ++offsets_in_node[0];
    node_offset += *freed_or;
  }
  return ZX_OK;
}

zx_status_t VnodeF2fs::RemoveInodePage() {
  LockedPage ipage;
  nid_t ino = Ino();
  if (zx_status_t err = fs()->GetNodeManager().GetNodePage(ino, &ipage); err != ZX_OK) {
    return err;
  }

  if (xattr_nid_ > 0) {
    LockedPage page;
    if (zx_status_t err = fs()->GetNodeManager().GetNodePage(xattr_nid_, &page); err != ZX_OK) {
      return err;
    }
    xattr_nid_ = 0;
    TruncateNode(page);
  }
  ZX_DEBUG_ASSERT(!GetBlocks());
  TruncateNode(ipage);
  return ZX_OK;
}

zx::result<LockedPage> VnodeF2fs::NewInodePage() {
  if (TestFlag(InodeInfoFlag::kNoAlloc)) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }
  // allocate inode page for new inode
  auto page_or = fs()->GetNodeManager().NewNodePage(Ino(), Ino(), IsDir(), 0);
  if (page_or.is_error()) {
    return page_or.take_error();
  }
  SetDirty();
  return zx::ok(std::move(*page_or));
}

// TODO: Consider using a global lock as below
// if (!IsDir())
//   mutex_lock(&superblock_info->writepages);
// Writeback()
// if (!IsDir())
//   mutex_unlock(&superblock_info->writepages);
// fs()->RemoveDirtyDirInode(this);
pgoff_t VnodeF2fs::Writeback(WritebackOperation &operation) {
  pgoff_t nwritten = 0;
  std::vector<std::unique_ptr<VmoCleaner>> cleaners;
  std::vector<LockedPage> pages = file_cache_->GetLockedDirtyPages(operation);
  PageList pages_to_disk;
  for (auto &page : pages) {
    page->WaitOnWriteback();
    block_t addr = GetBlockAddr(page);
    ZX_DEBUG_ASSERT(addr != kNewAddr);
    if (addr == kNullAddr) {
      page.release();
      continue;
    }
    page->SetWriteback();
    if (operation.page_cb) {
      // |page_cb| conducts additional process for the last page of node and meta vnodes.
      bool is_last_page = page.get() == pages.back().get();
      operation.page_cb(page.CopyRefPtr(), is_last_page);
    }
    // Notify kernel of ZX_PAGER_OP_WRITEBACK_BEGIN for the page.
    // TODO(b/293977738): VmoCleaner is instantiated on a per-page basis, so
    // ZX_PAGER_OP_WRITEBACK_BEGIN is called for each dirty page. We need to change it to work on
    // a per-range basis for efficient system calls.
    if (vmo_manager_->IsPaged()) {
      cleaners.push_back(std::make_unique<VmoCleaner>(operation.bSync, fbl::RefPtr<VnodeF2fs>(this),
                                                      page->GetKey(), page->GetKey() + 1));
    }
    pages_to_disk.push_back(page.release());
    ++nwritten;

    if (!(nwritten % kDefaultBlocksPerSegment)) {
      fs()->ScheduleWriter(nullptr, std::move(pages_to_disk));
      cleaners.clear();
    }
  }
  if (!pages_to_disk.is_empty() || operation.bSync) {
    sync_completion_t completion;
    fs()->ScheduleWriter(operation.bSync ? &completion : nullptr, std::move(pages_to_disk));
    if (operation.bSync) {
      sync_completion_wait(&completion, ZX_TIME_INFINITE);
    }
  }
  return nwritten;
}

// Set multimedia files as cold files for hot/cold data separation
void VnodeF2fs::SetColdFile() {
  std::lock_guard lock(mutex_);
  const std::vector<std::string> &extension_list = superblock_info_.GetExtensionList();
  for (const auto &extension : extension_list) {
    if (cpp20::ends_with(std::string_view(name_), std::string_view(extension))) {
      SetAdvise(FAdvise::kCold);
      break;
    }
    // compare upper case
    std::string upper_sub(extension);
    std::transform(upper_sub.cbegin(), upper_sub.cend(), upper_sub.begin(), ::toupper);
    if (cpp20::ends_with(std::string_view(name_), std::string_view(upper_sub))) {
      SetAdvise(FAdvise::kCold);
      break;
    }
  }
}

bool VnodeF2fs::IsColdFile() {
  fs::SharedLock lock(mutex_);
  return IsAdviseSet(FAdvise::kCold);
}

}  // namespace f2fs
