// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/minfs/fsck.h"

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/defer.h>
#include <lib/sync/completion.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/lib/storage/block_client/cpp/fake_block_device.h"
#include "src/lib/storage/fs_management/cpp/mount.h"
#include "src/lib/storage/vfs/cpp/journal/format.h"
#include "src/storage/minfs/format.h"
#include "src/storage/minfs/minfs_private.h"

namespace minfs {
namespace {

using block_client::FakeBlockDevice;

constexpr uint64_t kBlockCount = 1 << 20;
constexpr uint32_t kBlockSize = 512;

class ConsistencyCheckerFixture : public testing::Test {
 public:
  ConsistencyCheckerFixture() : vfs_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  void SetUp() override { device_ = std::make_unique<FakeBlockDevice>(kBlockCount, kBlockSize); }

  async_dispatcher_t* dispatcher() const { return vfs_loop_.dispatcher(); }
  std::unique_ptr<FakeBlockDevice> take_device() { return std::move(device_); }

 private:
  async::Loop vfs_loop_;
  std::unique_ptr<FakeBlockDevice> device_;
};

using ConsistencyCheckerTest = ConsistencyCheckerFixture;

TEST_F(ConsistencyCheckerTest, NewlyFormattedFilesystemWithRepair) {
  auto device = take_device();
  auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
  ASSERT_TRUE(Fsck(std::move(bcache_or.value()), FsckOptions{.repair = true}).is_ok());
}

TEST_F(ConsistencyCheckerTest, NewlyFormattedFilesystemWithoutRepair) {
  auto device = take_device();
  auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
  ASSERT_TRUE(Fsck(std::move(bcache_or.value()), FsckOptions()).is_ok());
}

TEST_F(ConsistencyCheckerTest, NewlyFormattedFilesystemCheckAfterMount) {
  auto device = take_device();
  auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());

  MountOptions options = {};
  auto fs_or = Minfs::Create(dispatcher(), std::move(bcache_or.value()), options);
  ASSERT_TRUE(fs_or.is_ok());

  bcache_or = zx::ok(Minfs::Destroy(std::move(fs_or.value())));
  ASSERT_TRUE(Fsck(std::move(bcache_or.value()), FsckOptions{.repair = true}).is_ok());
}

class ConsistencyCheckerFixtureVerbose : public testing::Test {
 public:
  ConsistencyCheckerFixtureVerbose() : vfs_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  void SetUp() override {
    auto device = std::make_unique<FakeBlockDevice>(kBlockCount, kMinfsBlockSize);

    auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
    EXPECT_TRUE(bcache_or.is_ok());
    EXPECT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
    MountOptions options = {};

    auto fs_or = Minfs::Create(vfs_loop_.dispatcher(), std::move(bcache_or.value()), options);
    EXPECT_TRUE(fs_or.is_ok());
    fs_ = std::move(fs_or.value());
  }

  Minfs* get_fs() const { return fs_.get(); }

  void destroy_fs(std::unique_ptr<Bcache>* bcache) {
    sync_completion_t completion;
    fs_->Sync([&completion](zx_status_t status) { sync_completion_signal(&completion); });
    EXPECT_EQ(sync_completion_wait(&completion, zx::duration::infinite().get()), ZX_OK);
    *bcache = Minfs::Destroy(std::move(fs_));
  }

  fs::VnodeAttributes CreateAndWrite(const char* name, size_t truncate_size, size_t offset,
                                     size_t data_size) {
    auto root_or = fs_->VnodeGet(kMinfsRootIno);
    EXPECT_TRUE(root_or.is_ok());
    fbl::RefPtr<fs::Vnode> child;
    EXPECT_EQ(root_or->Create(name, 0, &child), ZX_OK);
    if (data_size != 0) {
      char data[data_size];
      memset(data, 0, data_size);
      size_t size_written;
      EXPECT_EQ(child->Write(data, data_size, offset, &size_written), ZX_OK);
      EXPECT_EQ(size_written, data_size);
    }
    if (truncate_size > 0) {
      EXPECT_EQ(child->Truncate(truncate_size), ZX_OK);
    }
    fs::VnodeAttributes stat;
    EXPECT_EQ(child->GetAttributes(&stat), ZX_OK);
    EXPECT_EQ(child->Close(), ZX_OK);
    return stat;
  }

  void TearDown() override { EXPECT_EQ(fs_.get(), nullptr); }
  Minfs& fs() { return *fs_; }
  std::unique_ptr<Minfs> TakeFs() { return std::move(fs_); }

  void MarkDirectoryEntryMissing(size_t offset, std::unique_ptr<Bcache>* bcache);

 private:
  async::Loop vfs_loop_;
  std::unique_ptr<Minfs> fs_;
};

TEST_F(ConsistencyCheckerFixtureVerbose, TwoInodesPointToABlock) {
  fs::VnodeAttributes file1_stat = {}, file2_stat = {};
  {
    // Create a file with one data block.
    file1_stat = CreateAndWrite("file1", 0, 0, kMinfsBlockSize);
  }

  {
    // Create an empty file.
    file2_stat = CreateAndWrite("file2", 0, 0, 0);
  }

  EXPECT_NE(file1_stat.inode, file2_stat.inode);

  // To keep test simple, we ensure here that inodes allocated for file1 and
  // file2 are within the same block in the inode table.
  EXPECT_EQ(file1_stat.inode / kMinfsInodesPerBlock, file2_stat.inode / kMinfsInodesPerBlock);

  std::unique_ptr<Bcache> bcache;
  destroy_fs(&bcache);

  Superblock sb;
  EXPECT_TRUE(bcache->Readblk(0, &sb).is_ok());

  Inode inodes[kMinfsInodesPerBlock];
  blk_t inode_block =
      safemath::checked_cast<uint32_t>(sb.ino_block + (file1_stat.inode / kMinfsInodesPerBlock));
  EXPECT_TRUE(bcache->Readblk(inode_block, &inodes).is_ok());

  size_t file1_ino = file1_stat.inode % kMinfsInodesPerBlock;
  size_t file2_ino = file2_stat.inode % kMinfsInodesPerBlock;

  // The test code has hard dependency on filesystem layout.
  // TODO(fxbug.dev/39741): Isolate this test from the on-disk format.
  EXPECT_GT(inodes[file1_ino].dnum[0], 0u);
  EXPECT_EQ(inodes[file2_ino].dnum[0], 0u);

  // Make second file to point to the block owned by first file.
  inodes[file2_ino].dnum[0] = inodes[file1_ino].dnum[0];
  inodes[file2_ino].block_count = inodes[file1_ino].block_count;
  inodes[file2_ino].size = inodes[file1_ino].size;
  EXPECT_TRUE(bcache->Writeblk(inode_block, inodes).is_ok());

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = true}).is_error());
}

TEST_F(ConsistencyCheckerFixtureVerbose, TwoOffsetsPointToABlock) {
  fs::VnodeAttributes file_stat = {};
  file_stat = CreateAndWrite("file", 2 * kMinfsBlockSize, 0, kMinfsBlockSize);

  std::unique_ptr<Bcache> bcache;
  destroy_fs(&bcache);

  Superblock sb;
  EXPECT_TRUE(bcache->Readblk(0, &sb).is_ok());

  Inode inodes[kMinfsInodesPerBlock];
  blk_t inode_block =
      safemath::checked_cast<uint32_t>(sb.ino_block + (file_stat.inode / kMinfsInodesPerBlock));
  EXPECT_TRUE(bcache->Readblk(inode_block, &inodes).is_ok());

  size_t file_ino = file_stat.inode % kMinfsInodesPerBlock;

  EXPECT_GT(inodes[file_ino].dnum[0], 0u);
  EXPECT_EQ(inodes[file_ino].dnum[1], 0u);

  // Make second block offset point to the first block.
  inodes[file_ino].dnum[1] = inodes[file_ino].dnum[0];
  EXPECT_TRUE(bcache->Writeblk(inode_block, inodes).is_ok());

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = true}).is_error());
}

TEST_F(ConsistencyCheckerFixtureVerbose, IndirectBlocksShared) {
  fs::VnodeAttributes file_stat = {};
  uint64_t double_indirect_offset =
      (kMinfsDirect + (kMinfsIndirect * kMinfsDirectPerIndirect) + 1) * kMinfsBlockSize;
  file_stat = CreateAndWrite("file", double_indirect_offset, 0, kMinfsBlockSize);

  std::unique_ptr<Bcache> bcache;
  destroy_fs(&bcache);

  Superblock sb;
  EXPECT_TRUE(bcache->Readblk(0, &sb).is_ok());

  Inode inodes[kMinfsInodesPerBlock];
  blk_t inode_block =
      safemath::checked_cast<uint32_t>(sb.ino_block + (file_stat.inode / kMinfsInodesPerBlock));
  EXPECT_TRUE(bcache->Readblk(inode_block, &inodes).is_ok());

  size_t file_ino = file_stat.inode % kMinfsInodesPerBlock;

  EXPECT_GT(inodes[file_ino].dnum[0], 0u);
  EXPECT_EQ(inodes[file_ino].dnum[1], 0u);
  EXPECT_EQ(inodes[file_ino].inum[0], 0u);
  EXPECT_EQ(inodes[file_ino].dinum[0], 0u);

  // Make various indirect blocks to point to the data block.
  inodes[file_ino].dnum[1] = inodes[file_ino].dnum[0];
  inodes[file_ino].inum[0] = inodes[file_ino].dnum[0];
  inodes[file_ino].dinum[0] = inodes[file_ino].dnum[0];
  EXPECT_TRUE(bcache->Writeblk(inode_block, inodes).is_ok());

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = true}).is_error());
}

void ConsistencyCheckerFixtureVerbose::MarkDirectoryEntryMissing(size_t offset,
                                                                 std::unique_ptr<Bcache>* bcache) {
  blk_t root_dir_block;
  {
    auto root_or = fs_->VnodeGet(kMinfsRootIno);
    EXPECT_TRUE(root_or.is_ok());
    root_dir_block = root_or->GetInode()->dnum[0] + fs().Info().dat_block;
  }

  destroy_fs(bcache);

  // Need this buffer to be a full block.
  DirentBuffer<kMinfsBlockSize> dirent_buffer;

  ASSERT_TRUE((*bcache)->Readblk(root_dir_block, dirent_buffer.raw).is_ok());
  dirent_buffer.dirent.ino = 0;
  ASSERT_TRUE((*bcache)->Writeblk(root_dir_block, dirent_buffer.raw).is_ok());
}

TEST_F(ConsistencyCheckerFixtureVerbose, MissingDotEntry) {
  std::unique_ptr<Bcache> bcache;
  MarkDirectoryEntryMissing(0, &bcache);

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = true}).is_error());
}

TEST_F(ConsistencyCheckerFixtureVerbose, MissingDotDotEntry) {
  std::unique_ptr<Bcache> bcache;
  MarkDirectoryEntryMissing(DirentSize(1), &bcache);

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = true}).is_error());
}

void CreateUnlinkedDirectoryWithEntry(std::unique_ptr<Minfs> fs,
                                      std::unique_ptr<Bcache>* bcache_out) {
  ino_t ino;
  blk_t inode_block;

  {
    auto root_or = fs->VnodeGet(kMinfsRootIno);
    ASSERT_TRUE(root_or.is_ok());
    fbl::RefPtr<fs::Vnode> child_;
    ASSERT_EQ(root_or->Create("foo", 0, &child_), ZX_OK);
    auto child = fbl::RefPtr<VnodeMinfs>::Downcast(std::move(child_));
    auto close_child = fit::defer([child]() { child->Close(); });
    ino = child->GetIno();
    ASSERT_GT(kMinfsInodesPerBlock, ino);

    // Need this buffer to be a full block.
    DirentBuffer<kMinfsBlockSize> dirent_buffer;

    uint8_t data[kMinfsBlockSize];
    dirent_buffer.dirent.ino = ino;
    dirent_buffer.dirent.reclen = DirentSize(1);
    dirent_buffer.dirent.namelen = 1;
    dirent_buffer.dirent.type = kMinfsTypeDir;
    dirent_buffer.dirent.name[0] = '.';

    size_t written;
    ASSERT_EQ(child->Write(data, dirent_buffer.dirent.reclen, 0, &written), ZX_OK);
    ASSERT_EQ(written, dirent_buffer.dirent.reclen);

    ASSERT_EQ(root_or->Unlink("foo", false), ZX_OK);

    sync_completion_t completion;
    fs->Sync([&completion](zx_status_t status) { sync_completion_signal(&completion); });
    EXPECT_EQ(sync_completion_wait(&completion, zx::duration::infinite().get()), ZX_OK);

    inode_block = fs->Info().ino_block;

    // Prevent the inode from being purged when we close the child.
    fs->SetReadonly(true);
    fs->StopWriteback();
  }

  std::unique_ptr<Bcache> bcache = Minfs::Destroy(std::move(fs));

  // Now hack the inode so it looks like a directory with an invalid entry count.
  Inode inodes[kMinfsInodesPerBlock];
  ASSERT_TRUE(bcache->Readblk(inode_block, &inodes).is_ok());
  Inode& inode = inodes[ino];
  inode.magic = kMinfsMagicDir;
  inode.dirent_count = 1;
  ASSERT_TRUE(bcache->Writeblk(inode_block, &inodes).is_ok());

  *bcache_out = std::move(bcache);
}

TEST_F(ConsistencyCheckerFixtureVerbose, UnlinkedDirectoryHasBadEntryCount) {
  std::unique_ptr<Bcache> bcache;
  ASSERT_NO_FATAL_FAILURE(CreateUnlinkedDirectoryWithEntry(TakeFs(), &bcache));
  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = false, .read_only = true}).is_error());
}

TEST_F(ConsistencyCheckerFixtureVerbose, CorruptSuperblock) {
  std::unique_ptr<Bcache> bcache;
  destroy_fs(&bcache);

  Superblock sb;
  EXPECT_TRUE(bcache->Readblk(0, &sb).is_ok());

  // Check if superblock magic is valid
  EXPECT_EQ(sb.magic0, kMinfsMagic0);
  EXPECT_EQ(sb.magic1, kMinfsMagic1);

  // Corrupt the superblock
  sb.checksum = 0;
  EXPECT_TRUE(bcache->Writeblk(0, &sb).is_ok());

  ASSERT_TRUE(Fsck(std::move(bcache), FsckOptions{.repair = false}).is_error());
}

}  // namespace
}  // namespace minfs
