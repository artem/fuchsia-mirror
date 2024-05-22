// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

bool IsSameDnode(NodePath &path, uint32_t node_offset) {
  if (node_offset == kInvalidNodeOffset) {
    return false;
  }
  return path.node_offset[path.depth] == node_offset;
}

size_t GetOfsInDnode(NodePath &path) { return path.offset_in_node[path.depth]; }

// The maximum depth is four.
// Offset[0] indicates inode offset.
zx::result<NodePath> GetNodePath(VnodeF2fs &vnode, pgoff_t block) {
  const pgoff_t direct_index = vnode.GetAddrsPerInode();
  const pgoff_t direct_blks = kAddrsPerBlock;
  const pgoff_t dptrs_per_blk = kNidsPerBlock;
  const pgoff_t indirect_blks =
      safemath::CheckMul(safemath::checked_cast<pgoff_t>(kAddrsPerBlock), kNidsPerBlock)
          .ValueOrDie();
  const pgoff_t dindirect_blks = indirect_blks * kNidsPerBlock;
  NodePath path;
  size_t &level = path.depth;
  auto &offset = path.offset_in_node;
  auto &noffset = path.node_offset;
  size_t n = 0;
  path.ino = vnode.Ino();

  noffset[0] = 0;
  if (block < direct_index) {
    offset[n++] = static_cast<int>(block);
    level = 0;
    return zx::ok(path);
  }
  block -= direct_index;
  if (block < direct_blks) {
    offset[n++] = kNodeDir1Block;
    noffset[n] = 1;
    offset[n++] = static_cast<int>(block);
    level = 1;
    return zx::ok(path);
  }
  block -= direct_blks;
  if (block < direct_blks) {
    offset[n++] = kNodeDir2Block;
    noffset[n] = 2;
    offset[n++] = static_cast<int>(block);
    level = 1;
    return zx::ok(path);
  }
  block -= direct_blks;
  if (block < indirect_blks) {
    offset[n++] = kNodeInd1Block;
    noffset[n] = 3;
    offset[n++] = static_cast<int>(block / direct_blks);
    noffset[n] = 4 + offset[n - 1];
    offset[n++] = safemath::checked_cast<int32_t>(
        safemath::CheckMod<pgoff_t>(block, direct_blks).ValueOrDie());
    level = 2;
    return zx::ok(path);
  }
  block -= indirect_blks;
  if (block < indirect_blks) {
    offset[n++] = kNodeInd2Block;
    noffset[n] = 4 + dptrs_per_blk;
    offset[n++] = safemath::checked_cast<int32_t>(block / direct_blks);
    noffset[n] = 5 + dptrs_per_blk + offset[n - 1];
    offset[n++] = safemath::checked_cast<int32_t>(
        safemath::CheckMod<pgoff_t>(block, direct_blks).ValueOrDie());
    level = 2;
    return zx::ok(path);
  }
  block -= indirect_blks;
  if (block < dindirect_blks) {
    offset[n++] = kNodeDIndBlock;
    noffset[n] = 5 + (dptrs_per_blk * 2);
    offset[n++] = static_cast<int>(block / indirect_blks);
    noffset[n] = 6 + (dptrs_per_blk * 2) + offset[n - 1] * (dptrs_per_blk + 1);
    offset[n++] = safemath::checked_cast<int32_t>((block / direct_blks) % dptrs_per_blk);
    noffset[n] = 7 + (dptrs_per_blk * 2) + offset[n - 2] * (dptrs_per_blk + 1) + offset[n - 1];
    offset[n++] = safemath::checked_cast<int32_t>(
        safemath::CheckMod<pgoff_t>(block, direct_blks).ValueOrDie());
    level = 3;
    return zx::ok(path);
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

NodeManager::NodeManager(F2fs *fs) : fs_(fs), superblock_info_(fs_->GetSuperblockInfo()) {}

NodeManager::NodeManager(SuperblockInfo *sbi) : superblock_info_(*sbi) {}

void NodeManager::SetNatCacheDirty(NatEntry &ne) {
  ZX_ASSERT(clean_nat_list_.erase(ne) != nullptr);
  dirty_nat_list_.push_back(&ne);
}

void NodeManager::ClearNatCacheDirty(NatEntry &ne) {
  ZX_ASSERT(dirty_nat_list_.erase(ne) != nullptr);
  clean_nat_list_.push_back(&ne);
}

void NodeManager::NodeInfoFromRawNat(NodeInfo &ni, RawNatEntry &raw_ne) {
  ni.ino = LeToCpu(raw_ne.ino);
  ni.blk_addr = LeToCpu(raw_ne.block_addr);
  ni.version = raw_ne.version;
}

zx::result<nid_t> NodeManager::GetNextFreeNid() {
  fs::SharedLock free_nid_lock(free_nid_tree_lock_);
  if (free_nid_tree_.empty()) {
    return zx::error(ZX_ERR_NO_RESOURCES);
  }
  return zx::ok(*free_nid_tree_.begin());
}

void NodeManager::GetNatBitmap(void *out) {
  CloneBits(out, nat_bitmap_, 0, GetBitSize(nat_bitmap_size_));
  CloneBits(nat_prev_bitmap_, nat_bitmap_, 0, GetBitSize(nat_bitmap_size_));
}

pgoff_t NodeManager::CurrentNatAddr(nid_t start) {
  pgoff_t block_off;
  pgoff_t block_addr;
  pgoff_t seg_off;

  block_off = NatBlockOffset(start);
  seg_off = block_off >> superblock_info_.GetLogBlocksPerSeg();

  block_addr =
      static_cast<pgoff_t>(nat_blkaddr_ + (seg_off << superblock_info_.GetLogBlocksPerSeg() << 1) +
                           (block_off & ((1 << superblock_info_.GetLogBlocksPerSeg()) - 1)));

  if (nat_bitmap_.GetOne(ToMsbFirst(block_off)))
    block_addr += superblock_info_.GetBlocksPerSeg();

  return block_addr;
}

bool NodeManager::IsUpdatedNatPage(nid_t start) {
  pgoff_t block_off;

  block_off = NatBlockOffset(start);

  return nat_bitmap_.GetOne(ToMsbFirst(block_off)) ^ nat_prev_bitmap_.GetOne(ToMsbFirst(block_off));
}

pgoff_t NodeManager::NextNatAddr(pgoff_t block_addr) {
  block_addr -= nat_blkaddr_;
  if ((block_addr >> superblock_info_.GetLogBlocksPerSeg()) % 2)
    block_addr -= superblock_info_.GetBlocksPerSeg();
  else
    block_addr += superblock_info_.GetBlocksPerSeg();

  return block_addr + nat_blkaddr_;
}

void NodeManager::SetToNextNat(nid_t start_nid) {
  pgoff_t block_off = NatBlockOffset(start_nid);

  if (nat_bitmap_.GetOne(ToMsbFirst(block_off)))
    nat_bitmap_.ClearOne(ToMsbFirst(block_off));
  else
    nat_bitmap_.SetOne(ToMsbFirst(block_off));
}

void NodeManager::GetCurrentNatPage(nid_t nid, LockedPage *out) {
  pgoff_t index = CurrentNatAddr(nid);
  fs_->GetMetaPage(index, out);
}

zx::result<LockedPage> NodeManager::GetNextNatPage(nid_t nid) {
  pgoff_t src_off = CurrentNatAddr(nid);
  pgoff_t dst_off = NextNatAddr(src_off);

  // get current nat block page with lock
  LockedPage src_page;
  if (zx_status_t ret = fs_->GetMetaPage(src_off, &src_page); ret != ZX_OK) {
    return zx::error(ret);
  }
  // Dirty src_page means that it is already the new target NAT page
#if 0  // porting needed
  // if (PageDirty(src_page))
#endif
  if (IsUpdatedNatPage(nid)) {
    return zx::ok(std::move(src_page));
  }

  LockedPage dst_page;
  fs_->GrabMetaPage(dst_off, &dst_page);

  dst_page->Write(src_page->GetAddress());
  dst_page.SetDirty();

  SetToNextNat(nid);

  return zx::ok(std::move(dst_page));
}

// Readahead NAT pages
void NodeManager::RaNatPages(nid_t nid) {
  for (int i = 0; i < kFreeNidPages; ++i, nid += kNatEntryPerBlock) {
    if (nid >= max_nid_) {
      nid = 0;
    }
    LockedPage page;
    pgoff_t index = CurrentNatAddr(nid);
    if (zx_status_t ret = fs_->GetMetaPage(index, &page); ret != ZX_OK) {
      continue;
    }
#if 0  // porting needed
    // page_cache_release(page);
#endif
  }
}

NatEntry *NodeManager::LookupNatCache(nid_t n) {
  if (auto nat_entry = nat_cache_.find(n); nat_entry != nat_cache_.end()) {
    return &(*nat_entry);
  }
  return nullptr;
}

uint32_t NodeManager::GangLookupNatCache(uint32_t nr, NatEntry **out) {
  uint32_t ret = 0;
  for (auto &entry : nat_cache_) {
    out[ret] = &entry;
    if (++ret == nr)
      break;
  }
  return ret;
}

void NodeManager::DelFromNatCache(NatEntry &entry) {
  ZX_ASSERT_MSG(clean_nat_list_.erase(entry) != nullptr, "Cannot find NAT in list(nid = %u)",
                entry.GetNid());
  auto deleted = nat_cache_.erase(entry);
  ZX_ASSERT_MSG(deleted != nullptr, "Cannot find NAT in cache(nid = %u)", entry.GetNid());
  --nat_entries_count_;
}

bool NodeManager::IsCheckpointedNode(nid_t nid) {
  fs::SharedLock nat_lock(nat_tree_lock_);
  NatEntry *ne = LookupNatCache(nid);
  return !(ne && !ne->IsCheckpointed());
}

NatEntry *NodeManager::GrabNatEntry(nid_t nid) {
  auto new_entry = std::make_unique<NatEntry>();

  if (!new_entry)
    return nullptr;

  auto entry = new_entry.get();
  entry->SetNid(nid);

  clean_nat_list_.push_back(entry);
  nat_cache_.insert(std::move(new_entry));
  ++nat_entries_count_;
  return entry;
}

void NodeManager::CacheNatEntry(nid_t nid, RawNatEntry &raw_entry) {
  while (true) {
    std::lock_guard lock(nat_tree_lock_);
    NatEntry *entry = LookupNatCache(nid);
    if (!entry) {
      if (entry = GrabNatEntry(nid); !entry) {
        continue;
      }
    }
    entry->SetBlockAddress(LeToCpu(raw_entry.block_addr));
    entry->SetIno(LeToCpu(raw_entry.ino));
    entry->SetVersion(raw_entry.version);
    entry->SetCheckpointed();
    break;
  }
}

void NodeManager::SetNodeAddr(NodeInfo &ni, block_t new_blkaddr) {
  while (true) {
    std::lock_guard nat_lock(nat_tree_lock_);
    NatEntry *entry = LookupNatCache(ni.nid);
    if (!entry) {
      entry = GrabNatEntry(ni.nid);
      if (!entry) {
        continue;
      }
      entry->SetNodeInfo(ni);
      entry->SetCheckpointed();
      ZX_ASSERT(ni.blk_addr != kNewAddr);
    } else if (new_blkaddr == kNewAddr) {
      // when nid is reallocated,
      // previous nat entry can be remained in nat cache.
      // So, reinitialize it with new information.
      entry->SetNodeInfo(ni);
      ZX_ASSERT(ni.blk_addr == kNullAddr);
    }

    if (new_blkaddr == kNewAddr) {
      entry->ClearCheckpointed();
    }

    // validity check
    ZX_ASSERT(!(entry->GetBlockAddress() != ni.blk_addr));
    ZX_ASSERT(!(entry->GetBlockAddress() == kNullAddr && new_blkaddr == kNullAddr));
    ZX_ASSERT(!(entry->GetBlockAddress() == kNewAddr && new_blkaddr == kNewAddr));
    ZX_ASSERT(!(entry->GetBlockAddress() != kNewAddr && entry->GetBlockAddress() != kNullAddr &&
                new_blkaddr == kNewAddr));

    // increament version no as node is removed
    if (entry->GetBlockAddress() != kNewAddr && new_blkaddr == kNullAddr) {
      entry->SetVersion(entry->GetVersion() + 1);
    }

    // change address
    entry->SetBlockAddress(new_blkaddr);
    SetNatCacheDirty(*entry);
    break;
  }
}

int NodeManager::TryToFreeNats(int nr_shrink) {
  if (nat_entries_count_ < 2 * kNmWoutThreshold)
    return 0;

  std::lock_guard nat_lock(nat_tree_lock_);
  while (nr_shrink && !clean_nat_list_.is_empty()) {
    NatEntry *cache_entry = &clean_nat_list_.front();
    DelFromNatCache(*cache_entry);
    --nr_shrink;
  }
  return nr_shrink;
}

// This function returns always success
void NodeManager::GetNodeInfo(nid_t nid, NodeInfo &out) {
  nid_t start_nid = StartNid(nid);
  RawNatEntry ne;
  int i;
  out.nid = nid;
  {
    // Check nat cache
    fs::SharedLock nat_lock(nat_tree_lock_);
    NatEntry *entry = LookupNatCache(nid);
    if (entry) {
      out.ino = entry->GetIno();
      out.blk_addr = entry->GetBlockAddress();
      out.version = entry->GetVersion();
      return;
    }
  }

  fs_->GetSegmentManager().GetSummaryBlock(CursegType::kCursegHotData, [&](SummaryBlock &sum) {
    // Check current segment summary
    i = LookupJournalInCursum(sum, JournalType::kNatJournal, nid, 0);
    if (i >= 0) {
      ne = NatInJournal(sum, i);
      NodeInfoFromRawNat(out, ne);
    }
    return ZX_OK;
  });
  if (i < 0) {
    LockedPage page;
    // Fill NodeInfo from nat page
    GetCurrentNatPage(start_nid, &page);
    NatBlock *nat_blk = page->GetAddress<NatBlock>();
    ne = nat_blk->entries[nid - start_nid];

    NodeInfoFromRawNat(out, ne);
  }
  CacheNatEntry(nid, ne);
}

zx::result<LockedPage> NodeManager::FindLockedDnodePage(NodePath &path) {
  const size_t level = path.depth;
  const size_t(&offset)[kMaxNodeBlockLevel] = path.offset_in_node;
  LockedPage node_page;
  if (zx_status_t err = GetNodePage(path.ino, &node_page); err != ZX_OK) {
    return zx::error(err);
  }

  for (size_t i = 0; i < level; ++i) {
    auto page_or = GetNextNodePage(node_page, offset[i]);
    if (page_or.is_error()) {
      return page_or.take_error();
    }
    node_page = std::move(*page_or);
  }
  return zx::ok(std::move(node_page));
}

zx::result<LockedPage> NodeManager::GetLockedDnodePage(NodePath &node_path, bool is_dir) {
  size_t level = node_path.depth;
  const size_t(&offset)[kMaxNodeBlockLevel] = node_path.offset_in_node;
  const size_t(&noffset)[kMaxNodeBlockLevel] = node_path.node_offset;

  LockedPage node_page;
  if (zx_status_t err = GetNodePage(node_path.ino, &node_page); err != ZX_OK) {
    return zx::error(err);
  }

  std::vector<nid_t> new_nids;
  auto truncate_nodes = fit::defer([&] {
    for (const nid_t nid : new_nids) {
      TruncateNode(nid);
    }
  });
  LockedPage parent = std::move(node_page);
  for (size_t i = 0; i < level; ++i) {
    nid_t nid = parent.GetPage<NodePage>().GetNid(offset[i]);
    if (!nid) {
      // alloc new node
      auto nid_or = AllocNid();
      if (nid_or.is_error()) {
        return zx::error(ZX_ERR_NO_SPACE);
      }
      nid = *nid_or;
      auto page_or = NewNodePage(node_path.ino, nid, is_dir, noffset[i + 1]);
      if (page_or.is_error()) {
        AddFreeNid(nid);
        return page_or.take_error();
      }
      new_nids.push_back(nid);
      node_page = std::move(*page_or);
      parent.GetPage<NodePage>().SetNid(offset[i], nid);
      parent.SetDirty();
      ++node_path.num_new_nodes;
    } else {
      auto page_or = GetNextNodePage(parent, offset[i]);
      if (page_or.is_error()) {
        return page_or.take_error();
      }
      node_page = std::move(*page_or);
    }
    parent = std::move(node_page);
  }
  truncate_nodes.cancel();
  return zx::ok(std::move(parent));
}

void NodeManager::TruncateNode(nid_t nid) {
  NodeInfo ni;
  GetNodeInfo(nid, ni);
  ZX_ASSERT(ni.blk_addr != kNullAddr);

  if (ni.blk_addr != kNullAddr && ni.blk_addr != kNewAddr) {
    fs_->GetSegmentManager().InvalidateBlocks(ni.blk_addr);
  }
  // Deallocate node address
  superblock_info_.DecValidNodeCount(1);
  SetNodeAddr(ni, kNullAddr);
  superblock_info_.SetDirty();
}

zx::result<LockedPage> NodeManager::NewNodePage(nid_t ino, nid_t nid, bool is_dir, size_t ofs) {
  NodeInfo old_ni, new_ni;
  LockedPage page;
  if (zx_status_t ret = fs_->GetNodeVnode().GrabCachePage(nid, &page); ret != ZX_OK) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  GetNodeInfo(nid, old_ni);

  page->SetUptodate();
  page.Zero();
  page.GetPage<NodePage>().FillNodeFooter(nid, ino, ofs);

  // Reinitialize old_ni with new node page
  ZX_ASSERT(old_ni.blk_addr == kNullAddr);
  new_ni = old_ni;
  new_ni.ino = ino;

  if (!superblock_info_.IncValidNodeCount(1)) {
    page->ClearUptodate();
    fs_->GetInspectTree().OnOutOfSpace();
    return zx::error(ZX_ERR_NO_SPACE);
  }
  if (ino == nid) {
    superblock_info_.IncValidInodeCount();
  }
  SetNodeAddr(new_ni, kNewAddr);
  page.SetDirty();
  page.GetPage<NodePage>().SetColdNode(is_dir);

  return zx::ok(std::move(page));
}

zx_status_t NodeManager::GetNodePage(nid_t nid, LockedPage *out) {
  NodeInfo ni;
  GetNodeInfo(nid, ni);
  if (ni.blk_addr == kNullAddr) {
    return ZX_ERR_NOT_FOUND;
  }
  LockedPage page;
  if (zx_status_t ret = fs_->GetNodeVnode().GrabCachePage(nid, &page); ret != ZX_OK) {
    return ret;
  }
  if (page->IsUptodate() || ni.blk_addr == kNewAddr) {
    page->SetUptodate();
    *out = std::move(page);
    return ZX_OK;
  }

  auto status = fs_->MakeReadOperation(page, ni.blk_addr, PageType::kNode);
  if (status.is_error()) {
    return status.error_value();
  }

  ZX_DEBUG_ASSERT(nid == page.GetPage<NodePage>().NidOfNode());
#if 0  // porting needed
  // mark_page_accessed(page);
#endif
  *out = std::move(page);
  return ZX_OK;
}

zx::result<LockedPage> NodeManager::GetNextNodePage(LockedPage &node_page, size_t start) {
  NodePage &parent = node_page.GetPage<NodePage>();
  size_t max_offset = kNidsPerBlock;
  if (parent.IsInode()) {
    max_offset = kNodeDir1Block + kNidsPerInode;
  }
  std::vector<nid_t> nids;
  for (size_t i = start; i < max_offset && nids.size() < kDefaultReadaheadSize; ++i) {
    nid_t nid = parent.GetNid(i);
    if (!nid) {
      if (i == start) {
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      continue;
    }
    nids.push_back(nid);
  }
  ZX_DEBUG_ASSERT(nids.size());

  std::vector<LockedPage> pages;
  std::vector<block_t> addrs;
  pages.reserve(nids.size());
  addrs.reserve(nids.size());

  nid_t target_nid = nids[0];
  for (nid_t nid : nids) {
    LockedPage page;
    ZX_ASSERT(nid);
    NodeInfo ni;
    GetNodeInfo(nid, ni);
    if (ni.blk_addr == kNullAddr) {
      if (nid == target_nid) {
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      continue;
    }

    if (zx_status_t ret = fs_->GetNodeVnode().GrabCachePage(nid, &page); ret != ZX_OK) {
      return zx::error(ret);
    }
    if (page->IsUptodate() || ni.blk_addr == kNewAddr) {
      page->SetUptodate();
      if (nid == target_nid) {
        return zx::ok(std::move(page));
      }
      continue;
    }

    addrs.push_back(ni.blk_addr);
    pages.push_back(std::move(page));
  }
  ZX_DEBUG_ASSERT(addrs.size());

  if (auto ret = fs_->MakeReadOperations(pages, addrs, PageType::kNode); ret.is_error()) {
    FX_LOGS(ERROR) << "failed to read node pages. " << ret.status_string();
    return ret.take_error();
  }
  return zx::ok(std::move(pages[0]));
}

pgoff_t NodeManager::FsyncNodePages(nid_t ino) {
  WritebackOperation op;
  op.bSync = true;
  op.if_page = [ino](fbl::RefPtr<Page> page) {
    auto node_page = fbl::RefPtr<NodePage>::Downcast(std::move(page));
    if (node_page->IsDirty() && node_page->InoOfNode() == ino && node_page->IsDnode() &&
        node_page->IsColdNode()) {
      return ZX_OK;
    }
    return ZX_ERR_NEXT;
  };
  op.page_cb = [ino, this](fbl::RefPtr<Page> page, bool is_last_dnode) {
    auto node_page = fbl::RefPtr<NodePage>::Downcast(std::move(page));
    node_page->SetFsyncMark(false);
    if (node_page->IsInode()) {
      node_page->SetDentryMark(!IsCheckpointedNode(ino));
    }
    if (is_last_dnode) {
      node_page->SetFsyncMark(true);
      node_page->SetSync();
    }
    return ZX_OK;
  };
  return fs_->GetNodeVnode().Writeback(op);
}

#if 0  // porting needed
int NodeManager::F2fsWriteNodePages(struct address_space *mapping, WritebackControl *wbc) {
  // struct SuperblockInfo *superblock_info = F2FS_SB(mapping->host->i_sb);
  // struct block_device *bdev = superblock_info->sb->s_bdev;
  // long nr_to_write = wbc->nr_to_write;

  // if (wbc->for_kupdate)
  // 	return 0;

  // if (superblock_info->GetPageCount(CountType::kDirtyNodes) == 0)
  // 	return 0;

  // if (try_to_free_nats(superblock_info, kNatEntryPerBlock)) {
  // 	write_checkpoint(superblock_info, false, false);
  // 	return 0;
  // }

  // /* if mounting is failed, skip writing node pages */
  // wbc->nr_to_write = bio_get_nr_vecs(bdev);
  // sync_node_pages(superblock_info, 0, wbc);
  // wbc->nr_to_write = nr_to_write -
  // 	(bio_get_nr_vecs(bdev) - wbc->nr_to_write);
  // return 0;
  return 0;
}
#endif

zx::result<> NodeManager::LookupFreeNidList(nid_t n) {
  if (auto iter = free_nid_tree_.find(n); iter != free_nid_tree_.end()) {
    return zx::ok();
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

int NodeManager::AddFreeNid(nid_t nid) {
  std::lock_guard free_nid_lock(free_nid_tree_lock_);
  // We have enough free nids.
  if (free_nid_tree_.size() > 2 * kMaxFreeNids) {
    return 0;
  }
  int ret = 0;
  if (const auto [iter, inserted] = free_nid_tree_.insert(nid); inserted) {
    ++ret;
  }
  return ret;
}

void NodeManager::RemoveFreeNid(nid_t nid) {
  std::lock_guard free_nid_lock(free_nid_tree_lock_);
  RemoveFreeNidUnsafe(nid);
}

void NodeManager::RemoveFreeNidUnsafe(nid_t nid) {
  if (auto state_or = LookupFreeNidList(nid); state_or.is_ok()) {
    free_nid_tree_.erase(nid);
  }
}

int NodeManager::ScanNatPage(Page &nat_page, nid_t start_nid) {
  NatBlock *nat_blk = nat_page.GetAddress<NatBlock>();
  block_t blk_addr;
  int free_nids = 0;

  // 0 nid should not be used
  if (start_nid == 0) {
    ++start_nid;
  }

  for (uint32_t i = start_nid % kNatEntryPerBlock; i < kNatEntryPerBlock; ++i, ++start_nid) {
    blk_addr = LeToCpu(nat_blk->entries[i].block_addr);
    ZX_ASSERT(blk_addr != kNewAddr);
    if (blk_addr == kNullAddr) {
      free_nids += AddFreeNid(start_nid);
    }
  }
  return free_nids;
}

void NodeManager::BuildFreeNids() {
  {
    std::lock_guard lock(build_lock_);
    nid_t nid = next_scan_nid_, init_scan_nid = next_scan_nid_;
    bool is_cycled = false;
    uint64_t free_nids = 0;

    RaNatPages(nid);
    while (true) {
      {
        LockedPage page;
        GetCurrentNatPage(nid, &page);
        free_nids += ScanNatPage(*page, nid);
      }

      nid += (kNatEntryPerBlock - (nid % kNatEntryPerBlock));
      if (nid >= max_nid_) {
        nid = 0;
        is_cycled = true;
      }
      // If we already have enough nids or check every nid, stop it.
      if (free_nids > kMaxFreeNids || (is_cycled && init_scan_nid <= nid)) {
        break;
      }
    }
    next_scan_nid_ = nid;
  }

  // find free nids from current sum_pages
  fs_->GetSegmentManager().GetSummaryBlock(CursegType::kCursegHotData, [&](SummaryBlock &sum) {
    for (int i = 0; i < NatsInCursum(sum); ++i) {
      block_t addr = LeToCpu(NatInJournal(sum, i).block_addr);
      nid_t nid = LeToCpu(NidInJournal(sum, i));
      if (addr == kNullAddr) {
        AddFreeNid(nid);
      } else {
        RemoveFreeNid(nid);
      }
    }
    return ZX_OK;
  });

  // remove the free nids from current allocated nids
  std::lock_guard lock(free_nid_tree_lock_);
  for (auto nid : free_nid_tree_) {
    fs::SharedLock nat_lock(nat_tree_lock_);
    NatEntry *entry = LookupNatCache(nid);
    if (entry && entry->GetBlockAddress() != kNullAddr) {
      RemoveFreeNidUnsafe(nid);
    }
  }
}

// If this function returns success, caller can obtain a new nid
// from second parameter of this function.
// The returned nid could be used ino as well as nid when inode is created.
zx::result<nid_t> NodeManager::AllocNid() {
  do {
    if (!GetFreeNidCount()) {
      // scan NAT in order to build free nid tree
      BuildFreeNids();
      if (!GetFreeNidCount()) {
        fs_->GetInspectTree().OnOutOfSpace();
        return zx::error(ZX_ERR_NO_SPACE);
      }
    }
    // We check free nid counts again since previous check is racy as
    // we didn't hold free_nid_tree_lock. So other thread
    // could consume all of free nids.
  } while (!GetFreeNidCount());

  std::lock_guard lock(free_nid_tree_lock_);
  ZX_ASSERT(!free_nid_tree_.empty());
  auto free_nid = free_nid_tree_.begin();
  nid_t nid = *free_nid;
  free_nid_tree_.erase(free_nid);
  return zx::ok(nid);
}

zx_status_t NodeManager::RecoverInodePage(NodePage &page) {
  nid_t ino = page.InoOfNode();
  NodeInfo old_node_info, new_node_info;
  LockedPage ipage;

  if (zx_status_t ret = fs_->GetNodeVnode().GrabCachePage(ino, &ipage); ret != ZX_OK) {
    return ret;
  }

  // Should not use this inode from free nid tree
  RemoveFreeNid(ino);

  GetNodeInfo(ino, old_node_info);

  ipage.Zero();
  ipage.GetPage<NodePage>().FillNodeFooter(ino, ino, 0);
  ipage->Write(page.GetAddress(), 0, offsetof(Inode, i_ext));
  ipage->SetUptodate();

  Inode &inode = ipage->GetAddress<Node>()->i;
  inode.i_size = 0;
  inode.i_blocks = 1;
  inode.i_links = 1;
  inode.i_xattr_nid = 0;

  new_node_info = old_node_info;
  new_node_info.ino = ino;

  ZX_ASSERT(superblock_info_.IncValidNodeCount(1));
  SetNodeAddr(new_node_info, kNewAddr);
  superblock_info_.IncValidInodeCount();
  ipage.SetDirty();
  return ZX_OK;
}

bool NodeManager::FlushNatsInJournal() {
  int i;
  zx_status_t status =
      fs_->GetSegmentManager().SetSummaryBlock(CursegType::kCursegHotData, [&](SummaryBlock &sum) {
        {
          fs::SharedLock nat_lock(nat_tree_lock_);
          size_t dirty_nat_cnt = dirty_nat_list_.size_slow();
          if ((NatsInCursum(sum) + dirty_nat_cnt) <= kNatJournalEntries) {
            return ZX_ERR_OUT_OF_RANGE;
          }
        }

        for (i = 0; i < NatsInCursum(sum); ++i) {
          NatEntry *cache_entry = nullptr;
          RawNatEntry raw_entry = NatInJournal(sum, i);
          nid_t nid = LeToCpu(NidInJournal(sum, i));

          while (!cache_entry) {
            std::lock_guard nat_lock(nat_tree_lock_);
            cache_entry = LookupNatCache(nid);
            if (cache_entry) {
              SetNatCacheDirty(*cache_entry);
            } else {
              cache_entry = GrabNatEntry(nid);
              if (!cache_entry) {
                continue;
              }
              cache_entry->SetBlockAddress(LeToCpu(raw_entry.block_addr));
              cache_entry->SetIno(LeToCpu(raw_entry.ino));
              cache_entry->SetVersion(raw_entry.version);
              SetNatCacheDirty(*cache_entry);
            }
          }
        }
        UpdateNatsInCursum(sum, -i);
        return ZX_OK;
      });
  if (status != ZX_OK) {
    return false;
  }
  return true;
}

// This function is called during the checkpointing process.
zx_status_t NodeManager::FlushNatEntries() {
  LockedPage page;
  NatBlock *nat_blk = nullptr;
  nid_t start_nid = 0, end_nid = 0;
  bool flushed;

  flushed = FlushNatsInJournal();

#if 0  // porting needed
  //	if (!flushed)
#endif

  // 1) flush dirty nat caches
  zx_status_t status =
      fs_->GetSegmentManager().SetSummaryBlock(CursegType::kCursegHotData, [&](SummaryBlock &sum) {
        std::lock_guard nat_lock(nat_tree_lock_);
        for (auto iter = dirty_nat_list_.begin(); iter != dirty_nat_list_.end();) {
          nid_t nid;
          RawNatEntry raw_ne;
          int offset = -1;
          [[maybe_unused]] block_t old_blkaddr, new_blkaddr;

          // During each iteration, |iter| can be removed from |dirty_nat_list_|.
          // Therefore, make a copy of |iter| and move to the next element before futher operations.
          NatEntry *cache_entry = iter.CopyPointer();
          ++iter;

          nid = cache_entry->GetNid();

          if (cache_entry->GetBlockAddress() == kNewAddr)
            continue;

          if (!flushed) {
            // if there is room for nat enries in curseg->sumpage
            offset = LookupJournalInCursum(sum, JournalType::kNatJournal, nid, 1);
          }

          if (offset >= 0) {  // flush to journal
            raw_ne = NatInJournal(sum, offset);
            old_blkaddr = LeToCpu(raw_ne.block_addr);
          } else {  // flush to NAT block
            if (!page || (start_nid > nid || nid > end_nid)) {
              if (page) {
                page.SetDirty();
                page.reset();
              }
              start_nid = StartNid(nid);
              end_nid = start_nid + kNatEntryPerBlock - 1;

              // get nat block with dirty flag, increased reference
              // count, mapped and lock
              auto page_or = GetNextNatPage(start_nid);
              if (page_or.is_error()) {
                return page_or.error_value();
              }
              page = std::move(*page_or);
              nat_blk = page->GetAddress<NatBlock>();
            }

            ZX_ASSERT(nat_blk);
            raw_ne = nat_blk->entries[nid - start_nid];
            old_blkaddr = LeToCpu(raw_ne.block_addr);
          }

          new_blkaddr = cache_entry->GetBlockAddress();

          raw_ne.ino = CpuToLe(cache_entry->GetIno());
          raw_ne.block_addr = CpuToLe(new_blkaddr);
          raw_ne.version = cache_entry->GetVersion();

          if (offset < 0) {
            nat_blk->entries[nid - start_nid] = raw_ne;
          } else {
            SetNatInJournal(sum, offset, raw_ne);
            SetNidInJournal(sum, offset, CpuToLe(nid));
          }

          if (cache_entry->GetBlockAddress() == kNullAddr) {
            DelFromNatCache(*cache_entry);
            // We can reuse this freed nid at this point
            AddFreeNid(nid);
          } else {
            ClearNatCacheDirty(*cache_entry);
            cache_entry->SetCheckpointed();
          }
        }
        return ZX_OK;
      });

  if (status != ZX_OK) {
    return status;
  }

  // Write out last modified NAT block
  if (page != nullptr) {
    page.SetDirty();
  }

  // 2) shrink nat caches if necessary
  TryToFreeNats(safemath::checked_cast<int>(nat_entries_count_) -
                safemath::checked_cast<int>(kNmWoutThreshold));
  return ZX_OK;
}

zx_status_t NodeManager::InitNodeManager() {
  const Superblock &sb = superblock_info_.GetSuperblock();

  // segment_count_nat includes pair segment so divide to 2
  uint32_t nat_segs = LeToCpu(sb.segment_count_nat) >> 1;
  uint32_t nat_blocks = nat_segs << LeToCpu(sb.log_blocks_per_seg);
  nat_blkaddr_ = LeToCpu(sb.nat_blkaddr);
  max_nid_ = kNatEntryPerBlock * nat_blocks;
  {
    std::lock_guard lock(build_lock_);
    next_scan_nid_ = LeToCpu(superblock_info_.GetCheckpoint().next_free_nid);
  }

  nat_bitmap_size_ = superblock_info_.GetNatBitmapSize();
  nat_bitmap_.Reset(GetBitSize(nat_bitmap_size_));
  nat_prev_bitmap_.Reset(GetBitSize(nat_bitmap_size_));

  uint8_t *version_bitmap = superblock_info_.GetNatBitmap();
  if (!version_bitmap)
    return ZX_ERR_INVALID_ARGS;

  // copy version bitmap
  CloneBits(nat_bitmap_, version_bitmap, 0, GetBitSize(nat_bitmap_size_));
  CloneBits(nat_prev_bitmap_, version_bitmap, 0, GetBitSize(nat_bitmap_size_));
  return ZX_OK;
}

zx_status_t NodeManager::BuildNodeManager() {
  if (zx_status_t err = InitNodeManager(); err != ZX_OK) {
    return err;
  }

  BuildFreeNids();
  return ZX_OK;
}

NodeManager::~NodeManager() {
  NatEntry *natvec[kNatvecSize];
  uint32_t found;

  {
    // destroy free nid tree
    std::lock_guard free_nid_lock(free_nid_tree_lock_);
    free_nid_tree_.clear();
  }

  {
    // destroy nat cache
    std::lock_guard nat_lock(nat_tree_lock_);
    while ((found = GangLookupNatCache(kNatvecSize, natvec))) {
      for (uint32_t idx = 0; idx < found; ++idx) {
        NatEntry *e = natvec[idx];
        DelFromNatCache(*e);
      }
    }
    ZX_ASSERT(!nat_entries_count_);
    ZX_ASSERT(clean_nat_list_.is_empty());
    ZX_ASSERT(dirty_nat_list_.is_empty());
    ZX_ASSERT(nat_cache_.is_empty());
  }
}

}  // namespace f2fs
