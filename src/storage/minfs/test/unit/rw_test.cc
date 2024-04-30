// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <gtest/gtest.h>

#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "src/storage/minfs/bcache.h"
#include "src/storage/minfs/format.h"
#include "src/storage/minfs/lazy_buffer.h"
#include "src/storage/minfs/minfs.h"
#include "src/storage/minfs/runner.h"

namespace minfs {
namespace {

using ::block_client::FakeBlockDevice;
using ReadWriteTest = testing::Test;

// This unit test verifies that minfs, without vfs, behaves as expected
// when zero-length writes are interleaved with non-zero length writes.
TEST_F(ReadWriteTest, WriteZeroLength) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  const int kNumBlocks = 1 << 20;
  auto device = std::make_unique<FakeBlockDevice>(kNumBlocks, kMinfsBlockSize);
  ASSERT_TRUE(device);
  auto bcache_or = Bcache::Create(std::move(device), kNumBlocks);
  ASSERT_TRUE(bcache_or.is_ok());
  ASSERT_TRUE(Mkfs(bcache_or.value().get()).is_ok());
  auto fs_or = Runner::Create(loop.dispatcher(), std::move(bcache_or.value()), MountOptions());
  ASSERT_TRUE(fs_or.is_ok());
  auto root = fs_or->minfs().VnodeGet(kMinfsRootIno);
  ASSERT_TRUE(root.is_ok());
  zx::result foo = root->Create("foo", fs::CreationType::kFile);
  ASSERT_TRUE(foo.is_ok()) << foo.status_string();

  constexpr size_t kBufferSize = 65374;
  std::unique_ptr<uint8_t[]> buffer(new uint8_t[kBufferSize]());

  memset(buffer.get(), 0, kBufferSize);
  uint64_t kLargeOffset = 50ull * 1024ull * 1024ull;
  uint64_t kOffset = 11ull * 1024ull * 1024ull;

  size_t written_len = 0;

  ASSERT_EQ(foo->Write(buffer.get(), kBufferSize, kLargeOffset, &written_len), ZX_OK);
  ASSERT_EQ(written_len, kBufferSize);

  ASSERT_EQ(foo->Write(nullptr, 0, kOffset, &written_len), ZX_OK);
  ASSERT_EQ(written_len, 0ul);

  ASSERT_EQ(foo->Write(buffer.get(), kBufferSize, kLargeOffset - 8192, &written_len), ZX_OK);
  ASSERT_EQ(written_len, kBufferSize);

  foo->Close();
}

}  // namespace
}  // namespace minfs
