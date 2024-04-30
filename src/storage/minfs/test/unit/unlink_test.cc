// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <gtest/gtest.h>

#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "src/storage/minfs/bcache.h"
#include "src/storage/minfs/directory.h"
#include "src/storage/minfs/file.h"
#include "src/storage/minfs/format.h"
#include "src/storage/minfs/runner.h"

namespace minfs {
namespace {

using block_client::FakeBlockDevice;

TEST(UnlinkTest, PurgedFileHasCorrectMagic) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  constexpr uint64_t kBlockCount = 1 << 20;
  auto device = std::make_unique<FakeBlockDevice>(kBlockCount, kMinfsBlockSize);

  auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
  MountOptions options = {};

  auto fs_or = Runner::Create(loop.dispatcher(), std::move(bcache_or.value()), options);
  ASSERT_TRUE(fs_or.is_ok());

  ino_t ino;
  uint32_t inode_block;
  {
    auto root = fs_or->minfs().VnodeGet(kMinfsRootIno);
    ASSERT_TRUE(root.is_ok());
    zx::result fs_child = root->Create("foo", fs::CreationType::kFile);
    ASSERT_TRUE(fs_child.is_ok()) << fs_child.status_string();
    auto child = fbl::RefPtr<File>::Downcast(*std::move(fs_child));

    ino = child->GetIno();
    EXPECT_EQ(child->Close(), ZX_OK);
    EXPECT_EQ(root->Unlink("foo", /*must_be_dir=*/false), ZX_OK);
    inode_block = fs_or->minfs().Info().ino_block + ino / kMinfsInodesPerBlock;
  }
  bcache_or = zx::ok(Runner::Destroy(std::move(fs_or.value())));

  Inode inodes[kMinfsInodesPerBlock];
  EXPECT_TRUE(bcache_or->Readblk(inode_block, &inodes).is_ok());

  EXPECT_EQ(inodes[ino % kMinfsInodesPerBlock].magic, kMinfsMagicPurged);
}

TEST(UnlinkTest, UnlinkedDirectoryFailure) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  constexpr uint64_t kBlockCount = 1 << 20;
  auto device = std::make_unique<FakeBlockDevice>(kBlockCount, kMinfsBlockSize);

  auto bcache_or = Bcache::Create(std::move(device), kBlockCount);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
  MountOptions options = {};

  auto fs_or = Runner::Create(loop.dispatcher(), std::move(bcache_or.value()), options);
  ASSERT_TRUE(fs_or.is_ok());

  {
    auto root = fs_or->minfs().VnodeGet(kMinfsRootIno);
    ASSERT_TRUE(root.is_ok()) << root.status_string();
    zx::result fs_child = root->Create("foo", fs::CreationType::kDirectory);
    ASSERT_TRUE(fs_child.is_ok()) << fs_child.status_string();
    EXPECT_EQ(root->Unlink("foo", true), ZX_OK);
    auto child = fbl::RefPtr<Directory>::Downcast(*std::move(fs_child));
    EXPECT_EQ(0ul, child->GetInode()->size);
    EXPECT_EQ(child->Unlink("bar", false), ZX_ERR_NOT_FOUND);
    EXPECT_EQ(child->Rename(root.value(), "bar", "bar", false, false), ZX_ERR_NOT_FOUND);
    fbl::RefPtr<fs::Vnode> unused_child;
    EXPECT_EQ(child->Lookup("bar", &unused_child), ZX_ERR_NOT_FOUND);
    EXPECT_EQ(child->Close(), ZX_OK);
  }

  [[maybe_unused]] auto bcache = Runner::Destroy(std::move(fs_or.value()));
}

}  // namespace
}  // namespace minfs
