// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <unordered_set>

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

using Runner = ComponentRunner;

class SegmentManagerTest : public F2fsFakeDevTestFixture {
 public:
  SegmentManagerTest() {}

 protected:
  void MakeDirtySegments(size_t invalidate_ratio, int num_files) {
    fs_->GetGcManager().DisableFgGc();
    for (int file_no = 0; file_no < num_files; ++file_no) {
      fbl::RefPtr<fs::Vnode> test_file;
      EXPECT_EQ(root_dir_->Create(std::to_string(file_num++), S_IFREG, &test_file), ZX_OK);
      auto vnode = fbl::RefPtr<File>::Downcast(std::move(test_file));
      std::array<char, kPageSize> buf;
      std::memset(buf.data(), file_no, buf.size());
      for (size_t i = 0; i < fs_->GetSuperblockInfo().GetBlocksPerSeg(); ++i) {
        FileTester::AppendToFile(vnode.get(), buf.data(), buf.size());
      }
      vnode->SyncFile(0, vnode->GetSize(), 0);
      size_t truncate_size = vnode->GetSize() * (100 - invalidate_ratio) / 100;
      EXPECT_EQ(vnode->Truncate(truncate_size), ZX_OK);
      vnode->Close();
    }
    fs_->SyncFs();
    fs_->GetGcManager().EnableFgGc();
  }

 private:
  int file_num = 0;
};

TEST_F(SegmentManagerTest, BlkChaining) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  std::vector<block_t> blk_chain(0);
  int nwritten = kDefaultBlocksPerSegment * 2;
  // write the root inode, and read the block where the previous version of the root inode is stored
  // to check if the block has a proper lba address to the next node block
  for (int i = 0; i < nwritten; ++i) {
    NodeInfo ni;
    {
      LockedPage read_page;
      fs_->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &read_page);
      blk_chain.push_back(read_page.GetPage<NodePage>().NextBlkaddrOfNode());
      read_page.SetDirty();
    }
    WritebackOperation op = {.bSync = true};
    fs_->GetNodeVnode().Writeback(op);

    fs_->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);
    ASSERT_EQ(ni.blk_addr, blk_chain[i]);
  }
}

TEST_F(SegmentManagerTest, DirtyToFree) TA_NO_THREAD_SAFETY_ANALYSIS {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();

  // check the precond. before making dirty segments
  std::vector<uint32_t> prefree_array(0);
  int nwritten = kDefaultBlocksPerSegment * 2;
  uint32_t nprefree = 0;
  ASSERT_FALSE(fs_->GetSegmentManager().PrefreeSegments());
  uint32_t nfree_segs = fs_->GetSegmentManager().FreeSegments();
  FreeSegmapInfo *free_i = &fs_->GetSegmentManager().GetFreeSegmentInfo();
  DirtySeglistInfo *dirty_i = &fs_->GetSegmentManager().GetDirtySegmentInfo();

  // write the root inode repeatedly as much as 2 segments
  for (int i = 0; i < nwritten; ++i) {
    NodeInfo ni;
    fs_->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);
    block_t old_addr = ni.blk_addr;

    {
      LockedPage read_page;
      fs_->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &read_page);
      read_page.SetDirty();
    }

    WritebackOperation op = {.bSync = true};
    fs_->GetNodeVnode().Writeback(op);

    if (fs_->GetSegmentManager().GetValidBlocks(fs_->GetSegmentManager().GetSegmentNumber(old_addr),
                                                0) == 0) {
      prefree_array.push_back(fs_->GetSegmentManager().GetSegmentNumber(old_addr));
      ASSERT_EQ(fs_->GetSegmentManager().PrefreeSegments(), ++nprefree);
    }
  }

  // check the bitmaps and the number of free/prefree segments
  ASSERT_EQ(fs_->GetSegmentManager().FreeSegments(), nfree_segs - nprefree);
  for (auto &pre : prefree_array) {
    ASSERT_TRUE(dirty_i->dirty_segmap[static_cast<int>(DirtyType::kPre)].GetOne(pre));
    ASSERT_TRUE(free_i->free_segmap.GetOne(pre));
  }
  // triggers checkpoint to make prefree segments transit to free ones
  fs_->SyncFs();

  // check the bitmaps and the number of free/prefree segments
  for (auto &pre : prefree_array) {
    ASSERT_FALSE(free_i->free_segmap.GetOne(pre));
    ASSERT_FALSE(dirty_i->dirty_segmap[static_cast<int>(DirtyType::kPre)].GetOne(pre));
  }
  ASSERT_EQ(fs_->GetSegmentManager().FreeSegments(), nfree_segs);
  ASSERT_FALSE(fs_->GetSegmentManager().PrefreeSegments());
}

TEST_F(SegmentManagerTest, BalanceFs) TA_NO_THREAD_SAFETY_ANALYSIS {
  uint32_t nfree_segs = fs_->GetSegmentManager().FreeSegments();

  fs_->ClearOnRecovery();
  fs_->GetSegmentManager().BalanceFs();

  ASSERT_EQ(fs_->GetSegmentManager().FreeSegments(), nfree_segs);
  ASSERT_FALSE(fs_->GetSegmentManager().PrefreeSegments());

  fs_->SetOnRecovery();
  fs_->GetSegmentManager().BalanceFs();

  ASSERT_EQ(fs_->GetSegmentManager().FreeSegments(), nfree_segs);
  ASSERT_FALSE(fs_->GetSegmentManager().PrefreeSegments());
}

TEST_F(SegmentManagerTest, InvalidateBlocksExceptionCase) {
  // read the root inode block
  LockedPage root_node_page;
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  fs_->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &root_node_page);
  ASSERT_NE(root_node_page, nullptr);

  // Check InvalidateBlocks() exception case
  block_t temp_written_valid_blocks = fs_->GetSegmentManager().GetSitInfo().written_valid_blocks;
  fs_->GetSegmentManager().InvalidateBlocks(kNewAddr);
  ASSERT_EQ(temp_written_valid_blocks, fs_->GetSegmentManager().GetSitInfo().written_valid_blocks);
}

TEST_F(SegmentManagerTest, GetNewSegmentHeap) {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();

  // Check GetNewSegment() on AllocDirection::kAllocLeft
  superblock_info.ClearOpt(MountOption::kNoHeap);
  uint32_t nwritten = kDefaultBlocksPerSegment * 3;

  for (uint32_t i = 0; i < nwritten; ++i) {
    NodeInfo ni, new_ni;
    fs_->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);

    {
      LockedPage read_page;
      fs_->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &read_page);
      read_page.SetDirty();
    }
    WritebackOperation op = {.bSync = true};
    fs_->GetNodeVnode().Writeback(op);

    fs_->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), new_ni);
    ASSERT_NE(new_ni.blk_addr, kNullAddr);
    ASSERT_NE(new_ni.blk_addr, kNewAddr);

    // first segment already has next segment with noheap option
    if ((i > kDefaultBlocksPerSegment - 1) && ((ni.blk_addr + 1) % kDefaultBlocksPerSegment == 0)) {
      ASSERT_LT(new_ni.blk_addr, ni.blk_addr);
    } else {
      ASSERT_GT(new_ni.blk_addr, ni.blk_addr);
    }
  }
}

TEST_F(SegmentManagerTest, GetVictimSelPolicy) TA_NO_THREAD_SAFETY_ANALYSIS {
  VictimSelPolicy policy = fs_->GetSegmentManager().GetVictimSelPolicy(
      GcType::kFgGc, CursegType::kCursegHotNode, AllocMode::kSSR);
  ASSERT_EQ(policy.gc_mode, GcMode::kGcGreedy);
  ASSERT_EQ(policy.ofs_unit, 1U);

  policy = fs_->GetSegmentManager().GetVictimSelPolicy(GcType::kFgGc, CursegType::kNoCheckType,
                                                       AllocMode::kLFS);
  ASSERT_EQ(policy.gc_mode, GcMode::kGcGreedy);
  ASSERT_EQ(policy.ofs_unit, fs_->GetSuperblockInfo().GetSegsPerSec());
  ASSERT_EQ(policy.offset,
            fs_->GetSegmentManager().GetLastVictim(static_cast<int>(GcMode::kGcGreedy)));

  policy = fs_->GetSegmentManager().GetVictimSelPolicy(GcType::kBgGc, CursegType::kNoCheckType,
                                                       AllocMode::kLFS);
  ASSERT_EQ(policy.gc_mode, GcMode::kGcCb);
  ASSERT_EQ(policy.ofs_unit, fs_->GetSuperblockInfo().GetSegsPerSec());
  ASSERT_EQ(policy.offset, fs_->GetSegmentManager().GetLastVictim(static_cast<int>(GcMode::kGcCb)));

  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();
  dirty_info->nr_dirty[static_cast<int>(DirtyType::kDirty)] = kMaxSearchLimit + 2;
  policy = fs_->GetSegmentManager().GetVictimSelPolicy(GcType::kBgGc, CursegType::kNoCheckType,
                                                       AllocMode::kLFS);
  ASSERT_EQ(policy.max_search, kMaxSearchLimit);
}

TEST_F(SegmentManagerTest, GetMaxCost) TA_NO_THREAD_SAFETY_ANALYSIS {
  VictimSelPolicy policy = fs_->GetSegmentManager().GetVictimSelPolicy(
      GcType::kFgGc, CursegType::kCursegHotNode, AllocMode::kSSR);
  policy.min_cost = fs_->GetSegmentManager().GetMaxCost(policy);
  ASSERT_EQ(policy.min_cost,
            static_cast<uint32_t>(1 << fs_->GetSuperblockInfo().GetLogBlocksPerSeg()));

  policy = fs_->GetSegmentManager().GetVictimSelPolicy(GcType::kFgGc, CursegType::kNoCheckType,
                                                       AllocMode::kLFS);
  policy.min_cost = fs_->GetSegmentManager().GetMaxCost(policy);
  ASSERT_EQ(policy.min_cost,
            static_cast<uint32_t>(2 * (1 << fs_->GetSuperblockInfo().GetLogBlocksPerSeg()) *
                                  policy.ofs_unit));

  policy = fs_->GetSegmentManager().GetVictimSelPolicy(GcType::kBgGc, CursegType::kNoCheckType,
                                                       AllocMode::kLFS);
  policy.min_cost = fs_->GetSegmentManager().GetMaxCost(policy);
  ASSERT_EQ(policy.min_cost, std::numeric_limits<uint32_t>::max());
}

TEST_F(SegmentManagerTest, GetVictimByDefault) TA_NO_THREAD_SAFETY_ANALYSIS {
  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();

  uint32_t target_segno;
  for (target_segno = 0; target_segno < fs_->GetSegmentManager().TotalSegs(); ++target_segno) {
    if (!fs_->GetSegmentManager().SecUsageCheck(fs_->GetSegmentManager().GetSecNo(target_segno)) &&
        fs_->GetSegmentManager().GetValidBlocks(target_segno, 0) == 0U) {
      break;
    }
  }
  ASSERT_NE(target_segno, fs_->GetSegmentManager().TotalSegs());
  fs_->GetSegmentManager().SetSegmentEntryType(target_segno, CursegType::kCursegHotNode);

  // 1. Test SSR victim
  fs_->GetSegmentManager().SetLastVictim(static_cast<int>(GcType::kBgGc), target_segno);
  if (!dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirtyHotNode)].GetOne(target_segno)) {
    dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirtyHotNode)].SetOne(target_segno);
    ++dirty_info->nr_dirty[static_cast<int>(DirtyType::kDirtyHotNode)];
  }

  auto victim_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kBgGc, CursegType::kCursegHotNode, AllocMode::kSSR);
  ASSERT_FALSE(victim_or.is_error());
  uint32_t get_victim = victim_or.value();
  ASSERT_EQ(get_victim, target_segno);

  // 2. Test FgGc victim
  fs_->GetSegmentManager().SetLastVictim(static_cast<int>(GcType::kFgGc), target_segno);
  if (!dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].GetOne(target_segno)) {
    dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)].SetOne(target_segno);
    ++dirty_info->nr_dirty[static_cast<int>(DirtyType::kDirty)];
  }

  victim_or = fs_->GetSegmentManager().GetVictimByDefault(GcType::kFgGc, CursegType::kNoCheckType,
                                                          AllocMode::kLFS);
  ASSERT_FALSE(victim_or.is_error());
  get_victim = victim_or.value();
  ASSERT_EQ(get_victim, target_segno);

  // 3. Skip if cur_victim_sec is set (SSR)
  ASSERT_EQ(fs_->GetSegmentManager().GetCurVictimSec(),
            fs_->GetSegmentManager().GetSecNo(target_segno));
  ASSERT_TRUE(
      dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirtyHotNode)].GetOne(target_segno));
  ASSERT_EQ(dirty_info->nr_dirty[static_cast<int>(DirtyType::kDirtyHotNode)], 1);
  victim_or = fs_->GetSegmentManager().GetVictimByDefault(GcType::kBgGc, CursegType::kCursegHotNode,
                                                          AllocMode::kSSR);
  ASSERT_TRUE(victim_or.is_error());

  // 4. Skip if victim_secmap is set (kBgGc)
  fs_->GetSegmentManager().SetCurVictimSec(kNullSecNo);
  ASSERT_TRUE(
      dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirtyHotNode)].GetOne(target_segno));
  ASSERT_EQ(dirty_info->nr_dirty[static_cast<int>(DirtyType::kDirty)], 1);
  dirty_info->victim_secmap.SetOne(fs_->GetSegmentManager().GetSecNo(target_segno));
  victim_or = fs_->GetSegmentManager().GetVictimByDefault(GcType::kBgGc, CursegType::kCursegHotNode,
                                                          AllocMode::kLFS);
  ASSERT_TRUE(victim_or.is_error());
}

TEST_F(SegmentManagerTest, SelectBGVictims) TA_NO_THREAD_SAFETY_ANALYSIS {
  DirtySeglistInfo *dirty_info = &fs_->GetSegmentManager().GetDirtySegmentInfo();
  auto &bitmap = dirty_info->dirty_segmap[static_cast<int>(DirtyType::kDirty)];

  size_t invalid_ratio = 2;
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  // Make a dirty segment with 98% of valid blocks
  MakeDirtySegments(invalid_ratio, 1);
  ASSERT_EQ(CountBits(bitmap, 0, bitmap.size()), 1U);

  // Make a dirty segment with 97% of valid blocks after 1 sec
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  MakeDirtySegments(invalid_ratio + 1, 1);
  ASSERT_EQ(CountBits(bitmap, 0, bitmap.size()), 2U);

  // Make a dirty segment with 99% of valid blocks after 1 sec
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  MakeDirtySegments(invalid_ratio - 1, 1);
  ASSERT_EQ(CountBits(bitmap, 0, bitmap.size()), 3U);

  size_t valid_blocks_98 = CheckedDivRoundUp<size_t>(
      fs_->GetSuperblockInfo().GetBlocksPerSeg() * (100 - invalid_ratio), 100);
  size_t valid_blocks_97 = CheckedDivRoundUp<size_t>(
      fs_->GetSuperblockInfo().GetBlocksPerSeg() * (100 - invalid_ratio - 1), 100);

  // GcType::kFgGc should select a victim according to valid blocks
  auto victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kFgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_TRUE(victim_seg_or.is_ok());
  ASSERT_EQ(fs_->GetSegmentManager().GetValidBlocks(*victim_seg_or, true), valid_blocks_97);
  fs_->GetSegmentManager().SetCurVictimSec(kNullSecNo);

  // GcType::kBgGc should select a victim according to the segment age and valid blocks
  victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kBgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_TRUE(victim_seg_or.is_ok());
  ASSERT_EQ(fs_->GetSegmentManager().GetValidBlocks(*victim_seg_or, true), valid_blocks_98);

  // When a victim that GcType::kBgGc chooses is not handled yet, GcType::kFgGc should select the
  // victim
  victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kFgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_TRUE(victim_seg_or.is_ok());
  ASSERT_EQ(fs_->GetSegmentManager().GetValidBlocks(*victim_seg_or, true), valid_blocks_98);
  fs_->GetSegmentManager().SetCurVictimSec(kNullSecNo);

  // Even after system time is modified, it can select a victim correctly.
  fs_->GetSegmentManager().GetSitInfo().min_mtime = time(nullptr);
  victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kBgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_TRUE(victim_seg_or.is_ok());
  ASSERT_EQ(fs_->GetSegmentManager().GetValidBlocks(*victim_seg_or, true), valid_blocks_98);

  fs_->GetSegmentManager().GetSitInfo().max_mtime = 0;
  dirty_info->victim_secmap.ClearOne(*victim_seg_or / fs_->GetSuperblockInfo().GetSegsPerSec());
  victim_seg_or = fs_->GetSegmentManager().GetVictimByDefault(
      GcType::kBgGc, CursegType::kNoCheckType, AllocMode::kLFS);
  ASSERT_TRUE(victim_seg_or.is_ok());
  ASSERT_EQ(fs_->GetSegmentManager().GetValidBlocks(*victim_seg_or, true), valid_blocks_98);
}

TEST_F(SegmentManagerTest, AllocateNewSegments) TA_NO_THREAD_SAFETY_ANALYSIS {
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();

  uint32_t temp_free_segment = fs_->GetSegmentManager().FreeSegments();
  fs_->GetSegmentManager().AllocateNewSegments();
  ASSERT_EQ(temp_free_segment - 3, fs_->GetSegmentManager().FreeSegments());

  superblock_info.ClearOpt(MountOption::kDisableRollForward);
  temp_free_segment = fs_->GetSegmentManager().FreeSegments();
  for (int i = static_cast<int>(CursegType::kCursegHotNode);
       i <= static_cast<int>(CursegType::kCursegColdNode); ++i) {
    fs_->GetSegmentManager().AllocateSegmentByDefault(static_cast<CursegType>(i), true);
  }
  uint8_t type =
      superblock_info.GetCheckpoint().alloc_type[static_cast<int>(CursegType::kCursegHotNode)];
  ASSERT_EQ(superblock_info.GetSegmentCount(type), 6UL);
  ASSERT_EQ(temp_free_segment - 3, fs_->GetSegmentManager().FreeSegments());
}

TEST_F(SegmentManagerTest, DirtySegments) TA_NO_THREAD_SAFETY_ANALYSIS {
  // read the root inode block
  LockedPage root_node_page;
  SuperblockInfo &superblock_info = fs_->GetSuperblockInfo();
  fs_->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &root_node_page);
  ASSERT_NE(root_node_page, nullptr);

  DirtySeglistInfo &dirty_info = fs_->GetSegmentManager().GetDirtySegmentInfo();
  uint32_t dirtyDataSegments = dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyHotData)] +
                               dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyWarmData)] +
                               dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyColdData)];

  uint32_t dirtyNodeSegments = dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyHotNode)] +
                               dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyWarmNode)] +
                               dirty_info.nr_dirty[static_cast<int>(DirtyType::kDirtyColdNode)];

  ASSERT_EQ(fs_->GetSegmentManager().DirtySegments(), dirtyDataSegments + dirtyNodeSegments);
}

TEST(SegmentManagerOptionTest, Section) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::unique_ptr<BcacheMapper> bc;
  MkfsOptions mkfs_options{};
  mkfs_options.segs_per_sec = 4;
  FileTester::MkfsOnFakeDevWithOptions(&bc, mkfs_options);

  std::unique_ptr<F2fs> fs;
  MountOptions mount_options{};
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), mount_options, &bc, &fs);

  uint32_t blocks_per_section = kDefaultBlocksPerSegment * mkfs_options.segs_per_sec;
  SuperblockInfo &superblock_info = fs->GetSuperblockInfo();

  for (uint32_t i = 0; i < blocks_per_section; ++i) {
    NodeInfo ni;
    CursegInfo *cur_segment = fs->GetSegmentManager().CURSEG_I(CursegType::kCursegHotNode);

    {
      LockedPage root_node_page;
      fs->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &root_node_page);
      ASSERT_NE(root_node_page, nullptr);

      // Consume a block in the current section
      root_node_page.SetDirty();
    }
    WritebackOperation op = {.bSync = true};
    fs->GetNodeVnode().Writeback(op);

    fs->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);

    unsigned int expected = 1;
    // When a new secton is allocated, the valid block count of the previous one should be zero
    if ((ni.blk_addr + 1) % blocks_per_section == 0) {
      expected = 0;
    }
    ASSERT_EQ(expected,
              fs->GetSegmentManager().GetValidBlocks(
                  cur_segment->segno, safemath::checked_cast<int>(mkfs_options.segs_per_sec)));
    ASSERT_FALSE(fs->GetSegmentManager().HasNotEnoughFreeSecs());
  }

  FileTester::Unmount(std::move(fs), &bc);
}

TEST(SegmentManagerOptionTest, GetNewSegmentHeap) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::unique_ptr<BcacheMapper> bc;
  MkfsOptions mkfs_options{};
  mkfs_options.heap_based_allocation = true;
  mkfs_options.segs_per_sec = 4;
  mkfs_options.secs_per_zone = 4;
  FileTester::MkfsOnFakeDevWithOptions(&bc, mkfs_options);

  std::unique_ptr<F2fs> fs;
  MountOptions mount_options{};
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), mount_options, &bc, &fs);

  // Clear kMountNoheap opt, Allocate a new segment for hot nodes
  SuperblockInfo &superblock_info = fs->GetSuperblockInfo();
  superblock_info.ClearOpt(MountOption::kNoHeap);
  fs->GetSegmentManager().NewCurseg(CursegType::kCursegHotNode, false);

  const uint32_t alloc_size = kDefaultBlocksPerSegment * mkfs_options.segs_per_sec;
  uint32_t nwritten = alloc_size * mkfs_options.secs_per_zone * 3;

  for (uint32_t i = 0; i < nwritten; ++i) {
    NodeInfo ni, new_ni;
    fs->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);

    {
      LockedPage root_node_page;
      fs->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &root_node_page);
      ASSERT_NE(root_node_page, nullptr);
      root_node_page.SetDirty();
    }

    WritebackOperation op = {.bSync = true};
    fs->GetNodeVnode().Writeback(op);

    fs->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), new_ni);
    ASSERT_NE(new_ni.blk_addr, kNullAddr);
    ASSERT_NE(new_ni.blk_addr, kNewAddr);

    // The heap style allocation tries to find a free node section from the end of main area
    if ((i > alloc_size * 2 - 1) && (new_ni.blk_addr % alloc_size == 0)) {
      ASSERT_LT(new_ni.blk_addr, ni.blk_addr);
    } else {
      ASSERT_GT(new_ni.blk_addr, ni.blk_addr);
    }
  }

  FileTester::Unmount(std::move(fs), &bc);
}

TEST(SegmentManagerOptionTest, GetNewSegmentNoHeap) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::unique_ptr<BcacheMapper> bc;
  MkfsOptions mkfs_options{};
  mkfs_options.heap_based_allocation = false;
  mkfs_options.segs_per_sec = 4;
  mkfs_options.secs_per_zone = 4;
  FileTester::MkfsOnFakeDevWithOptions(&bc, mkfs_options);

  std::unique_ptr<F2fs> fs;
  MountOptions mount_options;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), mount_options, &bc, &fs);

  // Set kMountNoheap opt, Allocate a new segment for hot nodes
  SuperblockInfo &superblock_info = fs->GetSuperblockInfo();
  superblock_info.SetOpt(MountOption::kNoHeap);
  fs->GetSegmentManager().NewCurseg(CursegType::kCursegHotNode, false);

  uint32_t nwritten =
      kDefaultBlocksPerSegment * mkfs_options.segs_per_sec * mkfs_options.secs_per_zone * 3;

  for (uint32_t i = 0; i < nwritten; ++i) {
    NodeInfo ni, new_ni;
    fs->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), ni);
    ASSERT_NE(ni.blk_addr, kNullAddr);
    ASSERT_NE(ni.blk_addr, kNewAddr);

    {
      LockedPage root_node_page;
      fs->GetNodeManager().GetNodePage(superblock_info.GetRootIno(), &root_node_page);
      ASSERT_NE(root_node_page, nullptr);
      root_node_page.SetDirty();
    }
    WritebackOperation op = {.bSync = true};
    fs->GetNodeVnode().Writeback(op);

    fs->GetNodeManager().GetNodeInfo(superblock_info.GetRootIno(), new_ni);
    ASSERT_NE(new_ni.blk_addr, kNullAddr);
    ASSERT_NE(new_ni.blk_addr, kNewAddr);
    // It tries to find a free nodesction from the start of main area
    ASSERT_GT(new_ni.blk_addr, ni.blk_addr);
  }

  FileTester::Unmount(std::move(fs), &bc);
}

TEST(SegmentManagerOptionTest, DestroySegmentManagerExceptionCase) {
  std::unique_ptr<BcacheMapper> bc;
  MkfsOptions mkfs_options{};
  FileTester::MkfsOnFakeDevWithOptions(&bc, mkfs_options);

  MountOptions mount_options;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  auto superblock = LoadSuperblock(*bc);
  ASSERT_TRUE(superblock.is_ok());
  // Create a vfs object for unit tests.
  auto vfs_or = Runner::CreateRunner(loop.dispatcher());
  ZX_ASSERT(vfs_or.is_ok());
  std::unique_ptr<F2fs> fs =
      std::make_unique<F2fs>(loop.dispatcher(), std::move(bc), mount_options, (*vfs_or).get());

  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_OK);

  fs->Sync();

  // fault injection
  fs->GetSegmentManager().SetDirtySegmentInfo(nullptr);
  fs->GetSegmentManager().SetFreeSegmentInfo(nullptr);
  fs->GetSegmentManager().SetSitInfo(nullptr);

  fs->GetVCache().Reset();
  // test exception case
  fs->Reset();
}

TEST(SegmentManagerOptionTest, ModeLfs) {
  std::unique_ptr<BcacheMapper> bc;
  MkfsOptions mkfs_options{};
  mkfs_options.segs_per_sec = 4;
  FileTester::MkfsOnFakeDevWithOptions(&bc, mkfs_options);

  std::unique_ptr<F2fs> fs;
  MountOptions mount_options;
  mount_options.SetValue(MountOption::kForceLfs, static_cast<size_t>(ModeType::kModeLfs));
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), mount_options, &bc, &fs);
  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  auto root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  ASSERT_EQ(fs->GetSuperblockInfo().TestOpt(MountOption::kForceLfs), true);
  ASSERT_EQ(fs->GetSegmentManager().NeedSSR(), false);

  // Make SSR, IPU condition
  FileTester::CreateChild(root_dir.get(), S_IFREG, "alpha");
  fbl::RefPtr<fs::Vnode> vn;
  FileTester::Lookup(root_dir.get(), "alpha", &vn);
  auto file = fbl::RefPtr<File>::Downcast(std::move(vn));
  char buf[4 * kPageSize] = {
      1,
  };
  while (!fs->GetSegmentManager().NeedInplaceUpdate(file->IsDir())) {
    size_t out_end, out_actual;
    if (auto ret = FileTester::Append(file.get(), buf, sizeof(buf), &out_end, &out_actual);
        ret == ZX_ERR_NO_SPACE) {
      break;
    } else {
      ASSERT_EQ(ret, ZX_OK);
    }
    WritebackOperation op = {.bSync = true};
    file->Writeback(op);
  }

  // Since kMountForceLfs is on, f2fs doesn't allocate segments in ssr manner.
  ASSERT_EQ(fs->GetSegmentManager().NeedSSR(), false);
  ASSERT_EQ(fs->GetSegmentManager().NeedInplaceUpdate(file->IsDir()), false);

  // Make SSR, IPU enable
  fs->GetSuperblockInfo().ClearOpt(MountOption::kForceLfs);
  ASSERT_EQ(fs->GetSegmentManager().NeedSSR(), true);

  EXPECT_EQ(file->Close(), ZX_OK);
  file = nullptr;

  // Test ClearPrefreeSegments()
  fs->GetSuperblockInfo().SetOpt(MountOption::kForceLfs);
  FileTester::DeleteChild(root_dir.get(), "alpha", false);
  fs->SyncFs();

  EXPECT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;
  FileTester::Unmount(std::move(fs), &bc);
  EXPECT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}

TEST(SegmentManagerExceptionTest, BuildSitEntriesDiskFail) TA_NO_THREAD_SAFETY_ANALYSIS {
  std::unique_ptr<BcacheMapper> bc;
  FileTester::MkfsOnFakeDevWithOptions(&bc, MkfsOptions{});

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  auto superblock = LoadSuperblock(*bc);
  ASSERT_TRUE(superblock.is_ok());

  // Create a vfs object for unit tests.
  auto vfs_or = Runner::CreateRunner(loop.dispatcher());
  ASSERT_TRUE(vfs_or.is_ok());
  std::unique_ptr<F2fs> fs =
      std::make_unique<F2fs>(loop.dispatcher(), std::move(bc), MountOptions{}, (*vfs_or).get());

  ASSERT_EQ(fs->LoadSuper(std::move(*superblock)), ZX_OK);

  pgoff_t target_addr = fs->GetSegmentManager().CurrentSitAddr(0) * kDefaultSectorsPerBlock;

  auto hook = [target_addr](const block_fifo_request_t &_req, const zx::vmo *_vmo) {
    if (_req.dev_offset == target_addr) {
      return ZX_ERR_PEER_CLOSED;
    }
    return ZX_OK;
  };

  fs->GetSegmentManager().DestroySegmentManager();

  // Invalidate sit page to read from disk
  LockedPage sit_page;
  ASSERT_EQ(fs->GetMetaPage(target_addr / kDefaultSectorsPerBlock, &sit_page), ZX_OK);
  sit_page->Invalidate();
  sit_page.reset();

  // Expect fail in BuildSitEntries()
  DeviceTester::SetHook(fs.get(), hook);
  ASSERT_EQ(fs->GetSegmentManager().BuildSegmentManager(), ZX_ERR_PEER_CLOSED);

  DeviceTester::SetHook(fs.get(), nullptr);

  fs->GetVCache().Reset();
  fs->Reset();
}

}  // namespace
}  // namespace f2fs
