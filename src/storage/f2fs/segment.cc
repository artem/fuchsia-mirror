// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

int UpdateNatsInCursum(SummaryBlock *raw_summary, int i) {
  int n_nats = NatsInCursum(raw_summary);
  raw_summary->n_nats = CpuToLe(safemath::checked_cast<uint16_t>(n_nats + i));
  return n_nats;
}

static int UpdateSitsInCursum(SummaryBlock *raw_summary, int i) {
  int n_sits = SitsInCursum(raw_summary);
  raw_summary->n_sits = CpuToLe(safemath::checked_cast<uint16_t>(n_sits + i));
  return n_sits;
}

SegmentEntry &SegmentManager::GetSegmentEntry(uint32_t segno) { return sit_info_->sentries[segno]; }

SectionEntry &SegmentManager::GetSectionEntry(uint32_t segno) {
  return sit_info_->sec_entries[GetSecNo(segno)];
}

uint32_t SegmentManager::GetValidBlocks(uint32_t segno, uint32_t section) {
  // In order to get # of valid blocks in a section instantly from many
  // segments, f2fs manages two counting structures separately.
  if (section > 1) {
    return GetSectionEntry(segno).valid_blocks;
  }
  return GetSegmentEntry(segno).valid_blocks;
}

void SegmentManager::SegInfoFromRawSit(SegmentEntry &segment_entry, SitEntry &raw_sit) {
  segment_entry.valid_blocks = GetSitVblocks(raw_sit);
  segment_entry.ckpt_valid_blocks = GetSitVblocks(raw_sit);
  CloneBits<RawBitmapHeap>(segment_entry.cur_valid_map, raw_sit.valid_map, 0,
                           GetBitSize(kSitVBlockMapSize));
  CloneBits<RawBitmapHeap>(segment_entry.ckpt_valid_map, raw_sit.valid_map, 0,
                           GetBitSize(kSitVBlockMapSize));
  segment_entry.type = GetSitType(raw_sit);
  segment_entry.mtime = LeToCpu(uint64_t{raw_sit.mtime});
}

void SegmentManager::SegInfoToRawSit(SegmentEntry &segment_entry, SitEntry &raw_sit) {
  uint16_t raw_vblocks =
      static_cast<uint16_t>(segment_entry.type << kSitVblocksShift) | segment_entry.valid_blocks;
  raw_sit.vblocks = CpuToLe(raw_vblocks);
  CloneBits<RawBitmapHeap>(raw_sit.valid_map, segment_entry.cur_valid_map, 0,
                           GetBitSize(kSitVBlockMapSize));
  CloneBits<RawBitmapHeap>(segment_entry.ckpt_valid_map, raw_sit.valid_map, 0,
                           GetBitSize(kSitVBlockMapSize));
  segment_entry.ckpt_valid_blocks = segment_entry.valid_blocks;
  raw_sit.mtime = CpuToLe(static_cast<uint64_t>(segment_entry.mtime));
}

uint32_t SegmentManager::FindNextInuse(uint32_t max, uint32_t start) {
  fs::SharedLock segmap_lock(free_info_->segmap_lock);
  size_t segno = start;
  if (free_info_->free_segmap.Scan(segno, max, false, &segno)) {
    return max;
  }
  return safemath::checked_cast<uint32_t>(segno);
}

void SegmentManager::SetFree(uint32_t segno) {
  uint32_t secno = segno / superblock_info_->GetSegsPerSec();
  uint32_t start_segno = secno * superblock_info_->GetSegsPerSec();

  std::lock_guard segmap_lock(free_info_->segmap_lock);
  free_info_->free_segmap.ClearOne(segno);
  ++free_info_->free_segments;

  size_t dirty_seg;
  if (!free_info_->free_segmap.Scan(start_segno, TotalSegs(), false, &dirty_seg) &&
      dirty_seg < start_segno + superblock_info_->GetSegsPerSec()) {
    return;
  }
  free_info_->free_secmap.ClearOne(secno);
  ++free_info_->free_sections;
}

void SegmentManager::SetInuse(uint32_t segno) {
  uint32_t secno = segno / superblock_info_->GetSegsPerSec();
  free_info_->free_segmap.SetOne(segno);
  --free_info_->free_segments;
  if (!free_info_->free_secmap.GetOne(secno)) {
    free_info_->free_secmap.SetOne(secno);
    --free_info_->free_sections;
  }
}

void SegmentManager::SetTestAndFree(uint32_t segno) {
  uint32_t secno = segno / superblock_info_->GetSegsPerSec();
  uint32_t start_segno = secno * superblock_info_->GetSegsPerSec();
  std::lock_guard segmap_lock(free_info_->segmap_lock);
  if (free_info_->free_segmap.GetOne(segno)) {
    free_info_->free_segmap.ClearOne(segno);
    ++free_info_->free_segments;

    size_t next;
    if (!free_info_->free_segmap.Scan(start_segno, TotalSegs(), false, &next)) {
      if (next < start_segno + superblock_info_->GetSegsPerSec())
        return;
    }
    if (free_info_->free_secmap.GetOne(secno)) {
      free_info_->free_secmap.ClearOne(secno);
      ++free_info_->free_sections;
    }
  }
}

void SegmentManager::SetTestAndInuse(uint32_t segno) {
  uint32_t secno = segno / superblock_info_->GetSegsPerSec();
  std::lock_guard segmap_lock(free_info_->segmap_lock);
  if (!free_info_->free_segmap.GetOne(segno)) {
    free_info_->free_segmap.SetOne(segno);
    --free_info_->free_segments;
    if (!free_info_->free_secmap.GetOne(secno)) {
      free_info_->free_secmap.SetOne(secno);
      --free_info_->free_sections;
    }
  }
}

void SegmentManager::GetSitBitmap(void *dst_addr) {
  CloneBits(dst_addr, sit_info_->sit_bitmap, 0, GetBitSize(sit_info_->bitmap_size));
}

block_t SegmentManager::FreeSegments() {
  fs::SharedLock segmap_lock(free_info_->segmap_lock);
  block_t free_segs = free_info_->free_segments;
  return free_segs;
}

block_t SegmentManager::FreeSections() {
  fs::SharedLock segmap_lock(free_info_->segmap_lock);
  block_t free_secs = free_info_->free_sections;
  return free_secs;
}

block_t SegmentManager::PrefreeSegments() {
  return dirty_info_->nr_dirty[static_cast<int>(DirtyType::kPre)];
}

block_t SegmentManager::DirtySegments() {
  return dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyHotData)] +
         dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyWarmData)] +
         dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyColdData)] +
         dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyHotNode)] +
         dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyWarmNode)] +
         dirty_info_->nr_dirty[static_cast<int>(DirtyType::kDirtyColdNode)];
}

block_t SegmentManager::OverprovisionSections() {
  return GetOPSegmentsCount() / superblock_info_->GetSegsPerSec();
}

block_t SegmentManager::ReservedSections() {
  return GetReservedSegmentsCount() / superblock_info_->GetSegsPerSec();
}
bool SegmentManager::NeedSSR() {
  return (!superblock_info_->TestOpt(MountOption::kForceLfs) &&
          FreeSections() < static_cast<uint32_t>(OverprovisionSections()));
}

int SegmentManager::GetSsrSegment(CursegType type) {
  CursegInfo *curseg = CURSEG_I(type);
  auto segno_or = GetVictimByDefault(GcType::kBgGc, type, AllocMode::kSSR);
  if (segno_or.is_error()) {
    return 0;
  }
  curseg->next_segno = segno_or.value();
  return 1;
}

uint32_t SegmentManager::Utilization() {
  return static_cast<uint32_t>(static_cast<int64_t>(fs_->ValidUserBlocks()) * 100 /
                               static_cast<int64_t>(superblock_info_->GetUserBlockCount()));
}

// Sometimes f2fs may be better to drop out-of-place update policy.
// So, if fs utilization is over kMinIpuUtil, then f2fs tries to write
// data in the original place likewise other traditional file systems.
// TODO: Currently, we do not use IPU. Consider using IPU for fsynced data.
constexpr uint32_t kMinIpuUtil = 100;
bool SegmentManager::NeedInplaceUpdate(VnodeF2fs *vnode) {
  if (vnode->IsDir())
    return false;
  if (superblock_info_->TestOpt(MountOption::kForceLfs)) {
    return false;
  }
  return NeedSSR() && Utilization() > kMinIpuUtil;
}

uint32_t SegmentManager::CursegSegno(int type) {
  CursegInfo *curseg = CURSEG_I(static_cast<CursegType>(type));
  return curseg->segno;
}

uint8_t SegmentManager::CursegAllocType(int type) {
  CursegInfo *curseg = CURSEG_I(static_cast<CursegType>(type));
  return curseg->alloc_type;
}

uint16_t SegmentManager::CursegBlkoff(int type) {
  CursegInfo *curseg = CURSEG_I(static_cast<CursegType>(type));
  return curseg->next_blkoff;
}

void SegmentManager::CheckSegRange(uint32_t segno) const { ZX_ASSERT(segno < segment_count_); }

#if 0  // porting needed
// This function is used for only debugging.
// NOTE: In future, we have to remove this function.
void SegmentManager::VerifyBlockAddr(block_t blk_addr) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  SmInfo *sm_info = GetSmInfo(&superblock_info);
  block_t total_blks = sm_info->segment_count << superblock_info.GetLogBlocksPerSeg();
  [[maybe_unused]] block_t start_addr = sm_info->seg0_blkaddr;
  [[maybe_unused]] block_t end_addr = start_addr + total_blks - 1;
  ZX_ASSERT(!(blk_addr < start_addr));
  ZX_ASSERT(!(blk_addr > end_addr));
}
#endif

// Summary block is always treated as invalid block
void SegmentManager::CheckBlockCount(uint32_t segno, SitEntry &raw_sit) {
  uint32_t end_segno = segment_count_ - 1;
  int valid_blocks = 0;

  // check segment usage
  ZX_ASSERT(!(GetSitVblocks(raw_sit) > superblock_info_->GetBlocksPerSeg()));

  // check boundary of a given segment number
  ZX_ASSERT(!(segno > end_segno));

  // check bitmap with valid block count
  PageBitmap bits(raw_sit.valid_map, kSitVBlockMapSizeInBit);
  for (uint32_t i = 0; i < superblock_info_->GetBlocksPerSeg(); ++i) {
    if (bits.Test(ToMsbFirst(i)))
      ++valid_blocks;
  }
  ZX_ASSERT(GetSitVblocks(raw_sit) == valid_blocks);
}

pgoff_t SegmentManager::CurrentSitAddr(uint32_t start) {
  uint32_t offset = SitBlockOffset(start);
  block_t blk_addr = sit_info_->sit_base_addr + offset;

  CheckSegRange(start);

  // calculate sit block address
  if (sit_info_->sit_bitmap.GetOne(ToMsbFirst(offset)))
    blk_addr += sit_info_->sit_blocks;

  return blk_addr;
}

pgoff_t SegmentManager::NextSitAddr(pgoff_t block_addr) {
  block_addr -= sit_info_->sit_base_addr;
  if (block_addr < sit_info_->sit_blocks)
    block_addr += sit_info_->sit_blocks;
  else
    block_addr -= sit_info_->sit_blocks;

  return block_addr + sit_info_->sit_base_addr;
}

void SegmentManager::SetToNextSit(uint32_t start) {
  size_t offset = ToMsbFirst(SitBlockOffset(start));
  if (sit_info_->sit_bitmap.GetOne(offset)) {
    sit_info_->sit_bitmap.ClearOne(offset);
  } else {
    sit_info_->sit_bitmap.SetOne(offset);
  }
}

uint64_t SegmentManager::GetMtime() {
  auto cur_time = time(nullptr);
  return sit_info_->elapsed_time + cur_time - sit_info_->mounted_time;
}

void SegmentManager::SetSummary(Summary *sum, nid_t nid, uint32_t ofs_in_node, uint8_t version) {
  sum->nid = CpuToLe(nid);
  sum->ofs_in_node = CpuToLe(static_cast<uint16_t>(ofs_in_node));
  sum->version = version;
}

block_t SegmentManager::StartSumBlock() {
  return superblock_info_->StartCpAddr() +
         LeToCpu(superblock_info_->GetCheckpoint().cp_pack_start_sum);
}

block_t SegmentManager::SumBlkAddr(int base, int type) {
  return superblock_info_->StartCpAddr() +
         LeToCpu(superblock_info_->GetCheckpoint().cp_pack_total_block_count) - (base + 1) + type;
}

bool SegmentManager::SecUsageCheck(unsigned int secno) {
  return IsCurSec(secno) || (fs_->GetGcManager().GetCurVictimSec() == secno);
}

SegmentManager::SegmentManager(F2fs *fs) : fs_(fs) { superblock_info_ = &fs->GetSuperblockInfo(); }

bool SegmentManager::HasNotEnoughFreeSecs(uint32_t freed) {
  if (superblock_info_->IsOnRecovery())
    return false;

  return (FreeSections() + freed) <= (fs_->GetFreeSectionsForDirtyPages() + ReservedSections());
}

// This function balances dirty node and dentry pages.
// In addition, it controls garbage collection.
void SegmentManager::BalanceFs() {
  if (superblock_info_->IsOnRecovery()) {
    return;
  }

  // If there is not enough memory, wait for writeback.
  fs_->WaitForAvailableMemory();
  if (HasNotEnoughFreeSecs()) {
    if (auto ret = fs_->GetGcManager().F2fsGc(); ret.is_error()) {
      // F2fsGc() returns ZX_ERR_UNAVAILABLE when there is no available victim section, otherwise
      // BUG
      ZX_DEBUG_ASSERT(ret.error_value() == ZX_ERR_UNAVAILABLE);
    }
  }
}

void SegmentManager::LocateDirtySegment(uint32_t segno, DirtyType dirty_type) {
  // need not be added
  if (IsCurSeg(segno)) {
    return;
  }

  if (!dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].GetOne(segno)) {
    dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].SetOne(segno);
    ++dirty_info_->nr_dirty[static_cast<int>(dirty_type)];
  }

  if (dirty_type == DirtyType::kDirty) {
    SegmentEntry &sentry = GetSegmentEntry(segno);
    dirty_type = static_cast<DirtyType>(sentry.type);
    if (!dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].GetOne(segno)) {
      dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].SetOne(segno);
      ++dirty_info_->nr_dirty[static_cast<int>(dirty_type)];
    }
  }
}

void SegmentManager::RemoveDirtySegment(uint32_t segno, DirtyType dirty_type) {
  if (dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].GetOne(segno)) {
    dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].ClearOne(segno);
    --dirty_info_->nr_dirty[static_cast<int>(dirty_type)];
  }

  if (dirty_type == DirtyType::kDirty) {
    SegmentEntry &sentry = GetSegmentEntry(segno);
    dirty_type = static_cast<DirtyType>(sentry.type);
    if (dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].GetOne(segno)) {
      dirty_info_->dirty_segmap[static_cast<int>(dirty_type)].ClearOne(segno);
      --dirty_info_->nr_dirty[static_cast<int>(dirty_type)];
    }
    if (GetValidBlocks(segno, superblock_info_->GetSegsPerSec()) == 0) {
      dirty_info_->victim_secmap.ClearOne(GetSecNo(segno));
    }
  }
}

// Should not occur error such as ZX_ERR_NO_MEMORY.
// Adding dirty entry into seglist is not critical operation.
// If a given segment is one of current working segments, it won't be added.
void SegmentManager::LocateDirtySegment(uint32_t segno) {
  uint32_t valid_blocks;

  if (segno == kNullSegNo || IsCurSeg(segno))
    return;

  std::lock_guard seglist_lock(dirty_info_->seglist_lock);

  valid_blocks = GetValidBlocks(segno, 0);

  if (valid_blocks == 0) {
    LocateDirtySegment(segno, DirtyType::kPre);
    RemoveDirtySegment(segno, DirtyType::kDirty);
  } else if (valid_blocks < superblock_info_->GetBlocksPerSeg()) {
    LocateDirtySegment(segno, DirtyType::kDirty);
  } else {
    // Recovery routine with SSR needs this
    RemoveDirtySegment(segno, DirtyType::kDirty);
  }
}

// Should call clear_prefree_segments after checkpoint is done.
void SegmentManager::SetPrefreeAsFreeSegments() {
  std::lock_guard seglist_lock(dirty_info_->seglist_lock);
  const size_t total_segs = TotalSegs();
  for (size_t segno = 0; !dirty_info_->dirty_segmap[static_cast<int>(DirtyType::kPre)].Scan(
           segno, total_segs, false, &segno);
       ++segno) {
    SetTestAndFree(safemath::checked_cast<uint32_t>(segno));
  }
}

void SegmentManager::ClearPrefreeSegments() {
  const uint32_t total_segs = TotalSegs();
  bool need_align =
      superblock_info_->TestOpt(MountOption::kForceLfs) && superblock_info_->GetSegsPerSec() > 1;

  std::lock_guard seglist_lock(dirty_info_->seglist_lock);
  for (size_t segno = 0; !dirty_info_->dirty_segmap[static_cast<int>(DirtyType::kPre)].Scan(
           segno, total_segs, false, &segno);) {
    uint32_t start = safemath::checked_cast<uint32_t>(segno);
    uint32_t end;
    if (need_align) {
      start = GetSecNo(start) * superblock_info_->GetSegsPerSec();
      end = start + superblock_info_->GetSegsPerSec();
    } else {
      if (dirty_info_->dirty_segmap[static_cast<int>(DirtyType::kPre)].Scan(start + 1, total_segs,
                                                                            true, &segno)) {
        segno = total_segs;
      }
      end = safemath::checked_cast<uint32_t>(segno);
    }

    for (size_t i = start; i < end; ++i) {
      if (dirty_info_->dirty_segmap[static_cast<int>(DirtyType::kPre)].GetOne(i)) {
        dirty_info_->dirty_segmap[static_cast<int>(DirtyType::kPre)].ClearOne(i);
        --dirty_info_->nr_dirty[static_cast<int>(DirtyType::kPre)];
      }
    }
    segno = end;

    if (!superblock_info_->TestOpt(MountOption::kDiscard)) {
      continue;
    }

    if (!need_align) {
      block_t num_of_blocks =
          (safemath::CheckSub<block_t>(end, start) * superblock_info_->GetBlocksPerSeg())
              .ValueOrDie();
      fs_->MakeTrimOperation(StartBlock(start), num_of_blocks);
    } else {
      // In kMountForceLfs mode, a section is reusable only when all
      // segments of the section are free. Therefore, trim operation is
      // performed in section unit only in this case.
      while (start < end) {
        uint32_t secno = GetSecNo(start);
        uint32_t start_segno =
            safemath::CheckMul(secno, superblock_info_->GetSegsPerSec()).ValueOrDie();
        if (!IsCurSec(secno) && GetValidBlocks(start, superblock_info_->GetSegsPerSec()) == 0) {
          block_t num_of_blocks = safemath::CheckMul<block_t>(superblock_info_->GetSegsPerSec(),
                                                              superblock_info_->GetBlocksPerSeg())
                                      .ValueOrDie();
          fs_->MakeTrimOperation(StartBlock(start_segno), num_of_blocks);
        }
        start = safemath::CheckAdd(start_segno, superblock_info_->GetSegsPerSec()).ValueOrDie();
      }
    }
  }
}

void SegmentManager::MarkSitEntryDirty(uint32_t segno) {
  if (!sit_info_->dirty_sentries_bitmap.GetOne(segno)) {
    sit_info_->dirty_sentries_bitmap.SetOne(segno);
    ++sit_info_->dirty_sentries;
  }
}

void SegmentManager::SetSitEntryType(CursegType type, uint32_t segno, int modified) {
  SegmentEntry &segment_entry = GetSegmentEntry(segno);
  segment_entry.type = static_cast<uint8_t>(type);
  if (modified)
    MarkSitEntryDirty(segno);
}

void SegmentManager::UpdateSitEntry(block_t blkaddr, int del) {
  uint32_t offset;
  uint64_t new_vblocks;
  uint32_t segno = GetSegmentNumber(blkaddr);
  SegmentEntry &segment_entry = GetSegmentEntry(segno);

  new_vblocks = segment_entry.valid_blocks + del;
  offset = GetSegOffFromSeg0(blkaddr) & (superblock_info_->GetBlocksPerSeg() - 1);

  ZX_ASSERT(!((new_vblocks >> (sizeof(uint16_t) << 3) ||
               (new_vblocks > superblock_info_->GetBlocksPerSeg()))));

  segment_entry.valid_blocks = static_cast<uint16_t>(new_vblocks);
  segment_entry.mtime = GetMtime();
  sit_info_->max_mtime = segment_entry.mtime;

  // Update valid block bitmap
  if (del > 0) {
    ZX_ASSERT(!segment_entry.cur_valid_map.GetOne(ToMsbFirst(offset)));
    segment_entry.cur_valid_map.SetOne(ToMsbFirst(offset));
  } else {
    ZX_ASSERT(segment_entry.cur_valid_map.GetOne(ToMsbFirst(offset)));
    segment_entry.cur_valid_map.ClearOne(ToMsbFirst(offset));
  }
  if (!segment_entry.ckpt_valid_map.GetOne(ToMsbFirst(offset)))
    segment_entry.ckpt_valid_blocks += del;

  MarkSitEntryDirty(segno);

  // update total number of valid blocks to be written in ckpt area
  sit_info_->written_valid_blocks += del;

  if (superblock_info_->GetSegsPerSec() > 1)
    GetSectionEntry(segno).valid_blocks += del;
}

void SegmentManager::RefreshSitEntry(block_t old_blkaddr, block_t new_blkaddr) {
  UpdateSitEntry(new_blkaddr, 1);
  if (GetSegmentNumber(old_blkaddr) != kNullSegNo)
    UpdateSitEntry(old_blkaddr, -1);
}

void SegmentManager::InvalidateBlocks(block_t addr) {
  uint32_t segno = GetSegmentNumber(addr);

  ZX_ASSERT(addr != kNullAddr);
  if (addr == kNewAddr)
    return;

  std::lock_guard sentry_lock(sit_info_->sentry_lock);

  // add it into sit main buffer
  UpdateSitEntry(addr, -1);

  // add it into dirty seglist
  LocateDirtySegment(segno);
}

// This function should be resided under the curseg_mutex lock
void SegmentManager::AddSumEntry(CursegType type, Summary *sum, uint16_t offset) {
  CursegInfo *curseg = CURSEG_I(type);
  curseg->sum_blk->entries[offset] = *sum;
}

// Calculate the number of current summary pages for writing
int SegmentManager::NpagesForSummaryFlush() {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  uint32_t total_size_bytes = 0;
  uint32_t valid_sum_count = 0;

  for (int i = static_cast<int>(CursegType::kCursegHotData);
       i <= static_cast<int>(CursegType::kCursegColdData); ++i) {
    if (superblock_info.GetCheckpoint().alloc_type[i] == static_cast<uint8_t>(AllocMode::kSSR)) {
      valid_sum_count += superblock_info.GetBlocksPerSeg();
    } else {
      valid_sum_count += CursegBlkoff(i);
    }
  }

  total_size_bytes =
      (safemath::CheckMul<uint32_t>(valid_sum_count, kSummarySize + 1) +
       safemath::checked_cast<uint32_t>(sizeof(NatJournal) + 2U + sizeof(SitJournal) + 2U))
          .ValueOrDie();
  uint32_t sum_space = kPageSize - kSumFooterSize;
  if (total_size_bytes < sum_space) {
    return 1;
  } else if (total_size_bytes < 2 * sum_space) {
    return 2;
  }
  return 3;
}

// Caller should put this summary page
void SegmentManager::GetSumPage(uint32_t segno, LockedPage *out) {
  fs_->GetMetaPage(GetSumBlock(segno), out);
}

void SegmentManager::WriteSumPage(SummaryBlock *sum_blk, block_t blk_addr) {
  LockedPage page;
  fs_->GrabMetaPage(blk_addr, &page);
  page->Write(sum_blk);
  page.SetDirty();
}

// Find a new segment from the free segments bitmap to right order
// This function should be returned with success, otherwise BUG
// TODO: after LFS allocation available, raise out of space event of inspect tree when new segment
// cannot be allocated.
void SegmentManager::GetNewSegment(uint32_t *newseg, bool new_sec, AllocDirection dir) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  uint32_t total_secs = superblock_info.GetTotalSections();
  uint32_t segno, secno, zoneno;
  uint32_t total_zones = superblock_info.GetTotalSections() / superblock_info.GetSecsPerZone();
  uint32_t hint = *newseg / superblock_info.GetSegsPerSec();
  uint32_t old_zoneno = GetZoneNoFromSegNo(*newseg);
  uint32_t left_start = hint;
  bool find_another = true;
  bool go_left = false;
  bool got_it = false;

  std::lock_guard segmap_lock(free_info_->segmap_lock);

  auto to_left = [&]() -> bool {
    size_t free_sec;
    if (free_info_->free_secmap.Scan(hint, total_secs, true, &free_sec)) {
      if (dir == AllocDirection::kAllocRight) {
        ZX_ASSERT(!free_info_->free_secmap.Scan(0, total_secs, true, &free_sec));
      } else {
        go_left = true;
        left_start = hint - 1;
        free_sec = total_secs;
      }
    }
    secno = safemath::checked_cast<uint32_t>(free_sec);
    return go_left;
  };

  if (!new_sec && ((*newseg + 1) % superblock_info.GetSegsPerSec())) {
    size_t free_seg;
    if (free_info_->free_segmap.Scan(*newseg + 1, TotalSegs(), true, &free_seg)) {
      free_seg = TotalSegs();
    } else {
      got_it = true;
    }
    segno = safemath::checked_cast<uint32_t>(free_seg);
  }

  while (!got_it) {
    if (to_left()) {
      while (free_info_->free_secmap.GetOne(left_start)) {
        if (left_start > 0) {
          --left_start;
          continue;
        }
        size_t free_sec;
        ZX_ASSERT(!free_info_->free_secmap.Scan(0, total_secs, true, &free_sec));
        left_start = safemath::checked_cast<uint32_t>(free_sec);
        break;
      }
      secno = left_start;
    }

    hint = secno;
    segno = secno * superblock_info.GetSegsPerSec();
    zoneno = secno / superblock_info.GetSecsPerZone();

    // give up on finding another zone
    if (!find_another) {
      break;
    }
    if (superblock_info.GetSecsPerZone() == 1) {
      break;
    }
    if (zoneno == old_zoneno) {
      break;
    }
    if (dir == AllocDirection::kAllocLeft) {
      if (!go_left && zoneno + 1 >= total_zones) {
        break;
      }
      if (go_left && zoneno == 0) {
        break;
      }
    }
    bool is_current_zone = false;
    for (size_t i = 0; i < kNrCursegType; ++i) {
      if (CURSEG_I(static_cast<CursegType>(i))->zone == zoneno) {
        is_current_zone = true;
        break;
      }
    }
    if (!is_current_zone) {
      break;
    }
    // zone is in use, try another
    if (go_left) {
      hint = zoneno * superblock_info.GetSecsPerZone() - 1;
    } else if (zoneno + 1 >= total_zones) {
      hint = 0;
    } else {
      hint = (zoneno + 1) * superblock_info.GetSecsPerZone();
    }
    find_another = false;
  }
  // set it as dirty segment in free segmap
  ZX_ASSERT(!free_info_->free_segmap.GetOne(segno));
  SetInuse(segno);
  *newseg = segno;
}

void SegmentManager::ResetCurseg(CursegType type, int modified) {
  CursegInfo *curseg = CURSEG_I(type);
  SummaryFooter *sum_footer;

  curseg->segno = curseg->next_segno;
  curseg->zone = GetZoneNoFromSegNo(curseg->segno);
  curseg->next_blkoff = 0;
  curseg->next_segno = kNullSegNo;

  sum_footer = &(curseg->sum_blk->footer);
  memset(sum_footer, 0, sizeof(SummaryFooter));
  if (IsDataSeg(type))
    SetSumType(sum_footer, kSumTypeData);
  if (IsNodeSeg(type))
    SetSumType(sum_footer, kSumTypeNode);
  SetSitEntryType(type, curseg->segno, modified);
}

// Allocate a current working segment.
// This function always allocates a free segment in LFS manner.
void SegmentManager::NewCurseg(CursegType type, bool new_sec) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  CursegInfo *curseg = CURSEG_I(type);
  uint32_t segno = curseg->segno;
  AllocDirection dir = AllocDirection::kAllocLeft;

  WriteSumPage(&curseg->sum_blk, GetSumBlock(curseg->segno));
  if (type == CursegType::kCursegWarmData || type == CursegType::kCursegColdData)
    dir = AllocDirection::kAllocRight;

  if (superblock_info.TestOpt(MountOption::kNoHeap))
    dir = AllocDirection::kAllocRight;

  GetNewSegment(&segno, new_sec, dir);
  curseg->next_segno = segno;
  ResetCurseg(type, 1);
  curseg->alloc_type = static_cast<uint8_t>(AllocMode::kLFS);
}

void SegmentManager::NextFreeBlkoff(CursegInfo *seg, block_t start) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  SegmentEntry &segment_entry = GetSegmentEntry(seg->segno);
  block_t ofs;
  for (ofs = start; ofs < superblock_info.GetBlocksPerSeg(); ++ofs) {
    if (!segment_entry.ckpt_valid_map.GetOne(ToMsbFirst(ofs)) &&
        !segment_entry.cur_valid_map.GetOne(ToMsbFirst(ofs)))
      break;
  }
  seg->next_blkoff = static_cast<uint16_t>(ofs);
}

// If a segment is written by LFS manner, next block offset is just obtained
// by increasing the current block offset. However, if a segment is written by
// SSR manner, next block offset obtained by calling __next_free_blkoff
void SegmentManager::RefreshNextBlkoff(CursegInfo *seg) {
  if (seg->alloc_type == static_cast<uint8_t>(AllocMode::kSSR)) {
    NextFreeBlkoff(seg, seg->next_blkoff + 1);
  } else {
    ++seg->next_blkoff;
  }
}

// This function always allocates a used segment (from dirty seglist) by SSR
// manner, so it should recover the existing segment information of valid blocks
void SegmentManager::ChangeCurseg(CursegType type, bool reuse) {
  CursegInfo *curseg = CURSEG_I(type);
  uint32_t new_segno = curseg->next_segno;

  WriteSumPage(&curseg->sum_blk, GetSumBlock(curseg->segno));
  SetTestAndInuse(new_segno);

  {
    std::lock_guard seglist_lock(dirty_info_->seglist_lock);
    RemoveDirtySegment(new_segno, DirtyType::kPre);
    RemoveDirtySegment(new_segno, DirtyType::kDirty);
  }

  ResetCurseg(type, 1);
  curseg->alloc_type = static_cast<uint8_t>(AllocMode::kSSR);
  NextFreeBlkoff(curseg, 0);

  if (reuse) {
    LockedPage sum_page;
    GetSumPage(new_segno, &sum_page);
    sum_page->Read(curseg->sum_blk.get(), 0, kSumEntrySize);
  }
}

// Allocate a segment for new block allocations in CURSEG_I(type).
// This function is supposed to be successful. Otherwise, BUG.
void SegmentManager::AllocateSegmentByDefault(CursegType type, bool force) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  CursegInfo *curseg = CURSEG_I(type);

  if (force) {
    NewCurseg(type, true);
  } else {
    if (type == CursegType::kCursegWarmNode) {
      NewCurseg(type, false);
    } else if (NeedSSR() && GetSsrSegment(type)) {
      ChangeCurseg(type, true);
    } else {
      NewCurseg(type, false);
    }
  }
  superblock_info.IncSegmentCount(curseg->alloc_type);
}

void SegmentManager::AllocateNewSegments() {
  CursegInfo *curseg;
  uint32_t old_curseg;

  for (int i = static_cast<int>(CursegType::kCursegHotData);
       i <= static_cast<int>(CursegType::kCursegColdData); ++i) {
    curseg = CURSEG_I(static_cast<CursegType>(i));
    old_curseg = curseg->segno;
    AllocateSegmentByDefault(static_cast<CursegType>(i), true);
    LocateDirtySegment(old_curseg);
  }
}

bool SegmentManager::HasCursegSpace(CursegType type) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  CursegInfo *curseg = CURSEG_I(type);
  return curseg->next_blkoff < superblock_info.GetBlocksPerSeg();
}

CursegType SegmentManager::GetSegmentType2(Page &page, PageType p_type) {
  if (p_type == PageType::kData) {
    return CursegType::kCursegHotData;
  } else {
    return CursegType::kCursegHotNode;
  }
}

CursegType SegmentManager::GetSegmentType4(Page &page, PageType p_type) {
  if (p_type == PageType::kData) {
    VnodeF2fs &vnode = page.GetVnode();

    if (vnode.IsDir()) {
      return CursegType::kCursegHotData;
    }
    return CursegType::kCursegColdData;
  }

  ZX_ASSERT(p_type == PageType::kNode);
  NodePage *node_page = static_cast<NodePage *>(&page);
  if (node_page->IsDnode() && !node_page->IsColdNode()) {
    return CursegType::kCursegHotNode;
  }
  return CursegType::kCursegColdNode;
}

CursegType SegmentManager::GetSegmentType6(Page &page, PageType p_type) {
  if (p_type == PageType::kData) {
    VnodeF2fs &vnode = page.GetVnode();

    if (vnode.IsDir()) {
      return CursegType::kCursegHotData;
    } else if (page.IsColdData() || NodeManager::IsColdFile(vnode)) {
      return CursegType::kCursegColdData;
    }
    return CursegType::kCursegWarmData;
  }
  ZX_ASSERT(p_type == PageType::kNode);
  NodePage *node_page = static_cast<NodePage *>(&page);
  if (node_page->IsDnode()) {
    return node_page->IsColdNode() ? CursegType::kCursegWarmNode : CursegType::kCursegHotNode;
  }
  return CursegType::kCursegColdNode;
}

CursegType SegmentManager::GetSegmentType(Page &page, PageType p_type) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  switch (superblock_info.GetActiveLogs()) {
    case 2:
      return GetSegmentType2(page, p_type);
    case 4:
      return GetSegmentType4(page, p_type);
    case 6:
      return GetSegmentType6(page, p_type);
    default:
      ZX_ASSERT(0);
  }
}

zx::result<block_t> SegmentManager::GetBlockAddrOnSegment(LockedPage &page, block_t old_blkaddr,
                                                          Summary *sum, PageType p_type) {
  CursegInfo *curseg;
  CursegType type;

  type = GetSegmentType(*page, p_type);
  curseg = CURSEG_I(type);

  block_t new_blkaddr;
  {
    std::lock_guard curseg_lock(curseg->curseg_mutex);
    new_blkaddr = NextFreeBlkAddr(type);

    // AddSumEntry should be resided under the curseg_mutex
    // because this function updates a summary entry in the
    // current summary block.
    AddSumEntry(type, sum, curseg->next_blkoff);

    {
      std::lock_guard sentry_lock(sit_info_->sentry_lock);
      RefreshNextBlkoff(curseg);
      superblock_info_->IncBlockCount(curseg->alloc_type);

      // SIT information should be updated before segment allocation,
      // since SSR needs latest valid block information.
      RefreshSitEntry(old_blkaddr, new_blkaddr);

      if (!HasCursegSpace(type)) {
        AllocateSegmentByDefault(type, false);
      }

      LocateDirtySegment(GetSegmentNumber(old_blkaddr));
      LocateDirtySegment(GetSegmentNumber(new_blkaddr));
    }

    if (p_type == PageType::kNode) {
      page.GetPage<NodePage>().FillNodeFooterBlkaddr(NextFreeBlkAddr(type));
    }
  }

  ZX_ASSERT(page->SetBlockAddr(new_blkaddr).is_ok());
  return zx::ok(new_blkaddr);
}

zx::result<PageList> SegmentManager::GetBlockAddrsForDirtyDataPages(std::vector<LockedPage> pages,
                                                                    bool is_reclaim) {
  PageList pages_to_disk;
  for (auto &page : pages) {
    auto &vnode = page->GetVnode();
    ZX_DEBUG_ASSERT(page->IsUptodate());
    ZX_ASSERT_MSG(
        vnode.GetPageType() == PageType::kData,
        "failed to allocate blocks for vnode %u that should not have any dirty data Pages.",
        vnode.Ino());
    if (!is_reclaim || fs_->CanReclaim()) {
      auto addr_or = vnode.GetBlockAddrForDirtyDataPage(page, is_reclaim);
      if (addr_or.is_error()) {
        if (page->IsUptodate() && addr_or.status_value() != ZX_ERR_NOT_FOUND) {
          // In case of failure, redirty it.
          page.SetDirty();
          FX_LOGS(WARNING) << "failed to allocate a block: " << addr_or.status_value();
        }
        page->ClearWriteback();
      } else {
        ZX_ASSERT(*addr_or != kNullAddr && *addr_or != kNewAddr);
        pages_to_disk.push_back(page.release());
      }
    } else {
      // Writeback for memory reclaim is not allowed for now.
      ZX_ASSERT(page->GetFileCache().GetDirtyPageList().AddDirty(page).is_ok());
    }
    // Unlock |page| for waiters.
    page.reset();
  }
  return zx::ok(std::move(pages_to_disk));
}

zx::result<block_t> SegmentManager::GetBlockAddrForDirtyMetaPage(LockedPage &page,
                                                                 bool is_reclaim) {
  block_t addr = kNullAddr;
  page->WaitOnWriteback();
  if (page->ClearDirtyForIo()) {
    addr = safemath::checked_cast<block_t>(page->GetIndex());
    page->SetWriteback();
    ZX_ASSERT(page->SetBlockAddr(addr).is_ok());
  }
  return zx::ok(addr);
}

zx::result<block_t> SegmentManager::GetBlockAddrForNodePage(LockedPage &page, uint32_t nid,
                                                            block_t old_blkaddr) {
  Summary sum;
  SetSummary(&sum, nid, 0, 0);
  return GetBlockAddrOnSegment(page, old_blkaddr, &sum, PageType::kNode);
}

zx::result<block_t> SegmentManager::GetBlockAddrForDataPage(LockedPage &page, nid_t nid,
                                                            uint32_t ofs_in_node,
                                                            block_t old_blkaddr) {
  Summary sum;
  NodeInfo ni;

  ZX_ASSERT(old_blkaddr != kNullAddr);
  fs_->GetNodeManager().GetNodeInfo(nid, ni);
  SetSummary(&sum, nid, ofs_in_node, ni.version);

  return GetBlockAddrOnSegment(page, old_blkaddr, &sum, PageType::kData);
}

void SegmentManager::RecoverDataPage(Summary &sum, block_t old_blkaddr, block_t new_blkaddr) {
  CursegInfo *curseg;
  uint32_t old_cursegno;
  CursegType type;
  uint32_t segno = GetSegmentNumber(new_blkaddr);
  SegmentEntry &segment_entry = GetSegmentEntry(segno);

  type = static_cast<CursegType>(segment_entry.type);

  if (segment_entry.valid_blocks == 0 && !IsCurSeg(segno)) {
    if (old_blkaddr == kNullAddr) {
      type = CursegType::kCursegColdData;
    } else {
      type = CursegType::kCursegWarmData;
    }
  }
  curseg = CURSEG_I(type);

  std::lock_guard curseg_lock(curseg->curseg_mutex);
  std::lock_guard sentry_lock(sit_info_->sentry_lock);

  old_cursegno = curseg->segno;

  // change the current segment
  if (segno != curseg->segno) {
    curseg->next_segno = segno;
    ChangeCurseg(type, true);
  }

  curseg->next_blkoff = safemath::checked_cast<uint16_t>(GetSegOffFromSeg0(new_blkaddr) &
                                                         (superblock_info_->GetBlocksPerSeg() - 1));
  AddSumEntry(type, &sum, curseg->next_blkoff);

  RefreshSitEntry(old_blkaddr, new_blkaddr);

  LocateDirtySegment(old_cursegno);
  LocateDirtySegment(GetSegmentNumber(old_blkaddr));
  LocateDirtySegment(GetSegmentNumber(new_blkaddr));
}

zx_status_t SegmentManager::ReadCompactedSummaries() {
  Checkpoint &ckpt = superblock_info_->GetCheckpoint();
  LockedPage page;
  block_t start;
  int offset;

  start = StartSumBlock();

  if (zx_status_t ret = fs_->GetMetaPage(start++, &page); ret != ZX_OK) {
    return ret;
  }

  // Step 1: restore nat cache
  CursegInfo *seg_i = CURSEG_I(CursegType::kCursegHotData);
  page->Read(&seg_i->sum_blk->n_nats, 0, kSumJournalSize);

  // Step 2: restore sit cache
  seg_i = CURSEG_I(CursegType::kCursegColdData);
  page->Read(&seg_i->sum_blk->n_sits, kSumJournalSize, kSumJournalSize);
  offset = 2 * kSumJournalSize;

  // Step 3: restore summary entries
  for (int i = static_cast<int>(CursegType::kCursegHotData);
       i <= static_cast<int>(CursegType::kCursegColdData); ++i) {
    uint16_t blk_off;
    uint32_t segno;

    seg_i = CURSEG_I(static_cast<CursegType>(i));
    segno = LeToCpu(ckpt.cur_data_segno[i]);
    blk_off = LeToCpu(ckpt.cur_data_blkoff[i]);
    seg_i->next_segno = segno;
    ResetCurseg(static_cast<CursegType>(i), 0);
    seg_i->alloc_type = ckpt.alloc_type[i];
    seg_i->next_blkoff = blk_off;

    if (seg_i->alloc_type == static_cast<uint8_t>(AllocMode::kSSR))
      blk_off = static_cast<uint16_t>(superblock_info_->GetBlocksPerSeg());

    for (int j = 0; j < blk_off; ++j) {
      page->Read(&seg_i->sum_blk->entries[j], offset, kSummarySize);
      offset += kSummarySize;
      if (offset + kSummarySize <= kPageSize - kSumFooterSize) {
        continue;
      }

      if (zx_status_t ret = fs_->GetMetaPage(start++, &page); ret != ZX_OK) {
        return ret;
      }
      offset = 0;
    }
  }
  return ZX_OK;
}

zx_status_t SegmentManager::ReadNormalSummaries(int type) {
  Checkpoint &ckpt = superblock_info_->GetCheckpoint();
  SummaryBlock *sum;
  CursegInfo *curseg;
  uint16_t blk_off;
  uint32_t segno = 0;
  block_t blk_addr = 0;

  // get segment number and block addr
  if (IsDataSeg(static_cast<CursegType>(type))) {
    segno = LeToCpu(ckpt.cur_data_segno[type]);
    blk_off = LeToCpu(ckpt.cur_data_blkoff[type - static_cast<int>(CursegType::kCursegHotData)]);
    if (superblock_info_->TestCpFlags(CpFlag::kCpUmountFlag)) {
      blk_addr = SumBlkAddr(kNrCursegType, type);
    } else
      blk_addr = SumBlkAddr(kNrCursegDataType, type);
  } else {
    segno = LeToCpu(ckpt.cur_node_segno[type - static_cast<int>(CursegType::kCursegHotNode)]);
    blk_off = LeToCpu(ckpt.cur_node_blkoff[type - static_cast<int>(CursegType::kCursegHotNode)]);
    if (superblock_info_->TestCpFlags(CpFlag::kCpUmountFlag)) {
      blk_addr = SumBlkAddr(kNrCursegNodeType, type - static_cast<int>(CursegType::kCursegHotNode));
    } else
      blk_addr = GetSumBlock(segno);
  }

  LockedPage new_page;
  if (zx_status_t ret = fs_->GetMetaPage(blk_addr, &new_page); ret != ZX_OK) {
    return ret;
  }
  sum = new_page->GetAddress<SummaryBlock>();

  if (IsNodeSeg(static_cast<CursegType>(type))) {
    if (superblock_info_->TestCpFlags(CpFlag::kCpUmountFlag)) {
      Summary *ns = &sum->entries[0];
      for (uint32_t i = 0; i < superblock_info_->GetBlocksPerSeg(); ++i, ++ns) {
        ns->version = 0;
        ns->ofs_in_node = 0;
      }
    } else {
      if (fs_->GetNodeManager().RestoreNodeSummary(segno, *sum)) {
        return ZX_ERR_INVALID_ARGS;
      }
    }
  }

  // set uncompleted segment to curseg
  curseg = CURSEG_I(static_cast<CursegType>(type));
  {
    std::lock_guard curseg_lock(curseg->curseg_mutex);
    new_page->Read(curseg->sum_blk.get());
    curseg->next_segno = segno;
    ResetCurseg(static_cast<CursegType>(type), 0);
    curseg->alloc_type = ckpt.alloc_type[type];
    curseg->next_blkoff = blk_off;
  }
  return ZX_OK;
}

zx_status_t SegmentManager::RestoreCursegSummaries() {
  int type = static_cast<int>(CursegType::kCursegHotData);

  if (superblock_info_->TestCpFlags(CpFlag::kCpCompactSumFlag)) {
    // restore for compacted data summary
    if (ReadCompactedSummaries())
      return ZX_ERR_INVALID_ARGS;
    type = static_cast<int>(CursegType::kCursegHotNode);
  }

  for (; type <= static_cast<int>(CursegType::kCursegColdNode); ++type) {
    if (ReadNormalSummaries(type))
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

void SegmentManager::WriteCompactedSummaries(block_t blkaddr) {
  LockedPage page;
  fs_->GrabMetaPage(blkaddr++, &page);

  size_t written_size = 0;
  // Step 1: write nat cache
  CursegInfo *seg_i = CURSEG_I(CursegType::kCursegHotData);
  page->Write(&seg_i->sum_blk->n_nats, 0, kSumJournalSize);
  written_size += kSumJournalSize;

  // Step 2: write sit cache
  seg_i = CURSEG_I(CursegType::kCursegColdData);
  page->Write(&seg_i->sum_blk->n_sits, written_size, kSumJournalSize);
  written_size += kSumJournalSize;

  page.SetDirty();

  // Step 3: write summary entries
  for (int i = static_cast<int>(CursegType::kCursegHotData);
       i <= static_cast<int>(CursegType::kCursegColdData); ++i) {
    uint16_t blkoff;
    seg_i = CURSEG_I(static_cast<CursegType>(i));
    if (superblock_info_->GetCheckpoint().alloc_type[i] == static_cast<uint8_t>(AllocMode::kSSR)) {
      blkoff = static_cast<uint16_t>(superblock_info_->GetBlocksPerSeg());
    } else {
      blkoff = CursegBlkoff(i);
    }

    for (int j = 0; j < blkoff; ++j) {
      if (!page) {
        fs_->GrabMetaPage(blkaddr++, &page);
        written_size = 0;
        page.SetDirty();
      }
      page->Write(&seg_i->sum_blk->entries[j], written_size, kSummarySize);
      written_size += kSummarySize;

      if (written_size + kSummarySize <= kPageSize - kSumFooterSize) {
        continue;
      }

      page.reset();
    }
  }
}

void SegmentManager::WriteNormalSummaries(block_t blkaddr, CursegType type) {
  int end;

  if (IsDataSeg(type)) {
    end = static_cast<int>(type) + kNrCursegDataType;
  } else {
    end = static_cast<int>(type) + kNrCursegNodeType;
  }

  for (int i = static_cast<int>(type); i < end; ++i) {
    CursegInfo *sum = CURSEG_I(static_cast<CursegType>(i));
    std::lock_guard curseg_lock(sum->curseg_mutex);
    WriteSumPage(&sum->sum_blk, blkaddr + (i - static_cast<int>(type)));
  }
}

void SegmentManager::WriteDataSummaries(block_t start_blk) {
  if (superblock_info_->TestCpFlags(CpFlag::kCpCompactSumFlag)) {
    WriteCompactedSummaries(start_blk);
  } else {
    WriteNormalSummaries(start_blk, CursegType::kCursegHotData);
  }
}

void SegmentManager::WriteNodeSummaries(block_t start_blk) {
  if (superblock_info_->TestCpFlags(CpFlag::kCpUmountFlag))
    WriteNormalSummaries(start_blk, CursegType::kCursegHotNode);
}

int LookupJournalInCursum(SummaryBlock *sum, JournalType type, uint32_t val, int alloc) {
  if (type == JournalType::kNatJournal) {
    for (int i = 0; i < NatsInCursum(sum); ++i) {
      if (LeToCpu(NidInJournal(sum, i)) == val)
        return i;
    }
    if (alloc && NatsInCursum(sum) < static_cast<int>(kNatJournalEntries))
      return UpdateNatsInCursum(sum, 1);
  } else if (type == JournalType::kSitJournal) {
    for (int i = 0; i < SitsInCursum(sum); ++i) {
      if (LeToCpu(SegnoInJournal(sum, i)) == val)
        return i;
    }
    if (alloc && SitsInCursum(sum) < static_cast<int>(kSitJournalEntries))
      return UpdateSitsInCursum(sum, 1);
  }
  return -1;
}

zx::result<LockedPage> SegmentManager::GetCurrentSitPage(uint32_t segno) {
  uint32_t offset = SitBlockOffset(segno);
  block_t blk_addr = sit_info_->sit_base_addr + offset;

  CheckSegRange(segno);

  // calculate sit block address
  if (sit_info_->sit_bitmap.GetOne(ToMsbFirst(offset)))
    blk_addr += sit_info_->sit_blocks;

  LockedPage locked_page;
  if (zx_status_t ret = fs_->GetMetaPage(blk_addr, &locked_page); ret != ZX_OK) {
    return zx::error(ret);
  }
  return zx::ok(std::move(locked_page));
}

zx::result<LockedPage> SegmentManager::GetNextSitPage(uint32_t start) {
  pgoff_t src_off, dst_off;

  src_off = CurrentSitAddr(start);
  dst_off = NextSitAddr(src_off);

  // get current sit block page without lock
  LockedPage src_page;
  if (zx_status_t ret = fs_->GetMetaPage(src_off, &src_page); ret != ZX_OK) {
    return zx::error(ret);
  }
  LockedPage dst_page;
  fs_->GrabMetaPage(dst_off, &dst_page);
  ZX_ASSERT(!src_page->IsDirty());

  dst_page->Write(src_page->GetAddress());

  dst_page.SetDirty();

  SetToNextSit(start);

  return zx::ok(std::move(dst_page));
}

bool SegmentManager::FlushSitsInJournal() {
  CursegInfo *curseg = CURSEG_I(CursegType::kCursegColdData);
  SummaryBlock *sum = &curseg->sum_blk;

  // If the journal area in the current summary is full of sit entries,
  // all the sit entries will be flushed. Otherwise the sit entries
  // are not able to replace with newly hot sit entries.
  if ((SitsInCursum(sum) + sit_info_->dirty_sentries) > static_cast<int>(kSitJournalEntries)) {
    for (int i = SitsInCursum(sum) - 1; i >= 0; --i) {
      uint32_t segno;
      segno = LeToCpu(SegnoInJournal(sum, i));
      MarkSitEntryDirty(segno);
    }
    UpdateSitsInCursum(sum, -SitsInCursum(sum));
    return true;
  }
  return false;
}

// CP calls this function, which flushes SIT entries including SitJournal,
// and moves prefree segs to free segs.
zx_status_t SegmentManager::FlushSitEntries() {
  RawBitmap &bitmap = sit_info_->dirty_sentries_bitmap;
  CursegInfo *curseg = CURSEG_I(CursegType::kCursegColdData);
  SummaryBlock *sum = &curseg->sum_blk;
  block_t nsegs = TotalSegs();
  LockedPage page;
  uint32_t start = 0, end = 0;
  {
    std::lock_guard curseg_lock(curseg->curseg_mutex);
    std::lock_guard sentry_lock(sit_info_->sentry_lock);

    // "flushed" indicates whether sit entries in journal are flushed
    // to the SIT area or not.
    bool flushed = FlushSitsInJournal();

    for (size_t sentry = 0; !bitmap.Scan(sentry, nsegs, false, &sentry); ++sentry) {
      uint32_t segno = safemath::checked_cast<uint32_t>(sentry);
      SegmentEntry &segment_entry = GetSegmentEntry(segno);
      uint32_t sit_offset;
      int offset = -1;

      sit_offset = SitEntryOffset(segno);

      if (!flushed) {
        offset = LookupJournalInCursum(sum, JournalType::kSitJournal, segno, 1);
      }

      if (offset >= 0) {
        SetSegnoInJournal(sum, offset, CpuToLe(segno));
        SegInfoToRawSit(segment_entry, SitInJournal(sum, offset));
      } else {
        if (!page || (start > segno) || (segno > end)) {
          if (page) {
            page.SetDirty();
            page.reset();
          }

          start = StartSegNo(segno);
          end = start + kSitEntryPerBlock - 1;

          // read sit block that will be updated
          auto page_or = GetNextSitPage(start);
          if (page_or.is_error()) {
            return page_or.error_value();
          }
          page = std::move(*page_or);
        }

        // udpate entry in SIT block
        SegInfoToRawSit(segment_entry, page->GetAddress<SitBlock>()->entries[sit_offset]);
      }
      bitmap.ClearOne(segno);
      --sit_info_->dirty_sentries;
    }
  }

  // Write out last modified SIT block
  if (page != nullptr) {
    page.SetDirty();
  }

  SetPrefreeAsFreeSegments();
  return ZX_OK;
}

zx_status_t SegmentManager::BuildSitInfo() {
  const Superblock &raw_super = superblock_info_->GetRawSuperblock();
  Checkpoint &ckpt = superblock_info_->GetCheckpoint();
  uint32_t sit_segs;

  // allocate memory for SIT information
  sit_info_ = std::make_unique<SitInfo>();

  SitInfo *sit_i = sit_info_.get();
  if (sit_i->sentries = std::make_unique<SegmentEntry[]>(TotalSegs()); !sit_i->sentries) {
    return ZX_ERR_NO_MEMORY;
  }

  sit_i->dirty_sentries_bitmap.Reset(TotalSegs());

  for (uint32_t start = 0; start < TotalSegs(); ++start) {
    sit_i->sentries[start].cur_valid_map.Reset(GetBitSize(kSitVBlockMapSize));
    sit_i->sentries[start].ckpt_valid_map.Reset(GetBitSize(kSitVBlockMapSize));
  }

  if (superblock_info_->GetSegsPerSec() > 1) {
    if (sit_i->sec_entries = std::make_unique<SectionEntry[]>(superblock_info_->GetTotalSections());
        !sit_i->sec_entries) {
      return ZX_ERR_NO_MEMORY;
    }
  }

  uint32_t bitmap_size = superblock_info_->GetSitBitmapSize();
  // get information related with SIT
  sit_segs = LeToCpu(raw_super.segment_count_sit) >> 1;

  // setup SIT bitmap from ckeckpoint pack
  sit_i->sit_bitmap.Reset(GetBitSize(bitmap_size));
  CloneBits(sit_i->sit_bitmap, superblock_info_->GetSitBitmap(), 0, GetBitSize(bitmap_size));

#if 0  // porting needed
  /* init SIT information */
  // sit_i->s_ops = &default_salloc_ops;
#endif
  auto cur_time = time(nullptr);

  sit_i->sit_base_addr = LeToCpu(raw_super.sit_blkaddr);
  sit_i->sit_blocks = sit_segs << superblock_info_->GetLogBlocksPerSeg();
  sit_i->written_valid_blocks = LeToCpu(static_cast<block_t>(ckpt.valid_block_count));
  sit_i->bitmap_size = bitmap_size;
  sit_i->dirty_sentries = 0;
  sit_i->sents_per_block = kSitEntryPerBlock;
  sit_i->elapsed_time = LeToCpu(superblock_info_->GetCheckpoint().elapsed_time);
  sit_i->mounted_time = cur_time;
  return ZX_OK;
}

zx_status_t SegmentManager::BuildFreeSegmap() {
  // allocate memory for free segmap information
  free_info_ = std::make_unique<FreeSegmapInfo>();

  if (zx_status_t status = free_info_->free_segmap.Reset(TotalSegs()); status != ZX_OK) {
    return status;
  }
  if (zx_status_t status = free_info_->free_secmap.Reset(superblock_info_->GetTotalSections());
      status != ZX_OK) {
    return status;
  }

  // set all segments as dirty temporarily
  free_info_->free_segmap.Set(0, TotalSegs());
  free_info_->free_secmap.Set(0, superblock_info_->GetTotalSections());

  // init free segmap information
  free_info_->start_segno = GetSegNoFromSeg0(main_blkaddr_);
  free_info_->free_segments = 0;
  free_info_->free_sections = 0;

  return ZX_OK;
}

zx_status_t SegmentManager::BuildCurseg() {
  for (auto &curseg : curseg_array_) {
    memset(curseg.sum_blk.get(), 0, kBlockSize);
    curseg.segno = kNullSegNo;
    curseg.next_blkoff = 0;
  }
  return RestoreCursegSummaries();
}

zx_status_t SegmentManager::BuildSitEntries() {
  CursegInfo *curseg = CURSEG_I(CursegType::kCursegColdData);
  SummaryBlock *sum = &curseg->sum_blk;

  for (uint32_t start = 0; start < TotalSegs(); ++start) {
    SegmentEntry &segment_entry = sit_info_->sentries[start];
    SitBlock *sit_blk;
    SitEntry sit;
    bool got_it = false;
    {
      std::lock_guard curseg_lock(curseg->curseg_mutex);
      for (int i = 0; i < SitsInCursum(sum); ++i) {
        if (LeToCpu(SegnoInJournal(sum, i)) == start) {
          sit = SitInJournal(sum, i);
          got_it = true;
          break;
        }
      }
    }
    if (!got_it) {
      LockedPage page;
      auto page_or = GetCurrentSitPage(start);
      if (page_or.is_error()) {
        return page_or.error_value();
      }
      page = std::move(*page_or);
      sit_blk = page->GetAddress<SitBlock>();
      sit = sit_blk->entries[SitEntryOffset(start)];
    }
    CheckBlockCount(start, sit);
    SegInfoFromRawSit(segment_entry, sit);
    if (superblock_info_->GetSegsPerSec() > 1) {
      SectionEntry &e = GetSectionEntry(start);
      e.valid_blocks += segment_entry.valid_blocks;
    }
  }
  return ZX_OK;
}

void SegmentManager::InitFreeSegmap() {
  for (uint32_t start = 0; start < TotalSegs(); ++start) {
    SegmentEntry &sentry = GetSegmentEntry(start);
    if (!sentry.valid_blocks)
      SetFree(start);
  }

  // set use the current segments
  for (int type = static_cast<int>(CursegType::kCursegHotData);
       type <= static_cast<int>(CursegType::kCursegColdNode); ++type) {
    CursegInfo *curseg_t = CURSEG_I(static_cast<CursegType>(type));
    SetTestAndInuse(curseg_t->segno);
  }
}

void SegmentManager::InitDirtySegmap() {
  /* find dirty segment based on free segmap */
  for (uint32_t segno = 0; (segno = FindNextInuse(TotalSegs(), segno)) < TotalSegs(); ++segno) {
    size_t valid_blocks = GetValidBlocks(segno, 0);
    if (valid_blocks >= superblock_info_->GetBlocksPerSeg() || !valid_blocks) {
      continue;
    }
    std::lock_guard seglist_lock(dirty_info_->seglist_lock);
    LocateDirtySegment(segno, DirtyType::kDirty);
  }
}

zx_status_t SegmentManager::BuildDirtySegmap() {
  dirty_info_ = std::make_unique<DirtySeglistInfo>();
  for (uint32_t i = 0; i < static_cast<int>(DirtyType::kNrDirtytype); ++i) {
    dirty_info_->dirty_segmap[i].Reset(TotalSegs());
    dirty_info_->nr_dirty[i] = 0;
  }

  InitDirtySegmap();
  return dirty_info_->victim_secmap.Reset(superblock_info_->GetTotalSections());
}

// Update min, max modified time for cost-benefit GC algorithm
void SegmentManager::InitMinMaxMtime() {
  std::lock_guard sentry_lock(sit_info_->sentry_lock);

  sit_info_->min_mtime = LLONG_MAX;

  for (uint32_t segno = 0; segno < TotalSegs(); segno += superblock_info_->GetSegsPerSec()) {
    uint64_t mtime = 0;

    for (uint32_t i = 0; i < superblock_info_->GetSegsPerSec(); ++i) {
      mtime += GetSegmentEntry(segno + i).mtime;
    }

    mtime /= static_cast<uint64_t>(superblock_info_->GetSegsPerSec());

    if (sit_info_->min_mtime > mtime) {
      sit_info_->min_mtime = mtime;
    }
  }
  sit_info_->max_mtime = GetMtime();
}

zx_status_t SegmentManager::BuildSegmentManager() {
  const Superblock &raw_super = superblock_info_->GetRawSuperblock();
  Checkpoint &ckpt = superblock_info_->GetCheckpoint();

  seg0_blkaddr_ = LeToCpu(raw_super.segment0_blkaddr);
  main_blkaddr_ = LeToCpu(raw_super.main_blkaddr);
  segment_count_ = LeToCpu(raw_super.segment_count);
  reserved_segments_ = LeToCpu(ckpt.rsvd_segment_count);
  ovp_segments_ = LeToCpu(ckpt.overprov_segment_count);
  main_segments_ = LeToCpu(raw_super.segment_count_main);
  ssa_blkaddr_ = LeToCpu(raw_super.ssa_blkaddr);

  if (zx_status_t ret = BuildSitInfo(); ret != ZX_OK) {
    return ret;
  }

  if (zx_status_t ret = BuildFreeSegmap(); ret != ZX_OK) {
    return ret;
  }

  if (zx_status_t ret = BuildCurseg(); ret != ZX_OK) {
    return ret;
  }

  // reinit free segmap based on SIT
  if (zx_status_t ret = BuildSitEntries(); ret != ZX_OK) {
    return ret;
  }

  InitFreeSegmap();
  if (zx_status_t ret = BuildDirtySegmap(); ret != ZX_OK) {
    return ret;
  }

  InitMinMaxMtime();
  return ZX_OK;
}

void SegmentManager::DestroyDirtySegmap() {
  if (!dirty_info_)
    return;

  for (int i = 0; i < static_cast<int>(DirtyType::kNrDirtytype); ++i) {
    std::lock_guard seglist_lock(dirty_info_->seglist_lock);
    dirty_info_->nr_dirty[i] = 0;
  }

  dirty_info_.reset();
}

void SegmentManager::DestroySegmentManager() { DestroyDirtySegmap(); }

}  // namespace f2fs
