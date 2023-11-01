// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <unordered_set>

#include <gtest/gtest.h>
#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/f2fs/f2fs_layout.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {
namespace {

using MmapTest = SingleFileTest;

TEST_F(MmapTest, GetVmo) {
  srand(testing::UnitTest::GetInstance()->random_seed());
  File *test_file = &vnode<File>();
  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  FileTester::AppendToFile(test_file, write_buf, PAGE_SIZE);

  zx::vmo vmo;
  uint8_t read_buf[PAGE_SIZE];
  ASSERT_EQ(
      test_file->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);
  vmo.read(read_buf, 0, PAGE_SIZE);
  vmo.reset();
  loop_.RunUntilIdle();

  ASSERT_EQ(memcmp(read_buf, write_buf, PAGE_SIZE), 0);
}

TEST_F(MmapTest, GetVmoSize) {
  srand(testing::UnitTest::GetInstance()->random_seed());
  File *test_file = &vnode<File>();
  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  FileTester::AppendToFile(test_file, write_buf, PAGE_SIZE);

  // Create paged_vmo
  zx::vmo vmo;
  ASSERT_EQ(
      test_file->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  // Increase file size
  FileTester::AppendToFile(test_file, write_buf, PAGE_SIZE);

  // Get new Private VMO. paged_vmo size is increased.
  zx::vmo private_vmo;
  ASSERT_EQ(test_file->GetVmo(
                fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead,
                &private_vmo),
            ZX_OK);

  size_t size;
  size_t expected_4k = PAGE_SIZE;
  size_t expected_8k = PAGE_SIZE * 2;
  vmo.get_size(&size);
  ASSERT_EQ(size, expected_4k);
  private_vmo.get_size(&size);
  ASSERT_EQ(size, expected_8k);
  private_vmo.reset();
  vmo.reset();

  loop_.RunUntilIdle();
}

TEST_F(MmapTest, GetVmoZeroSize) {
  zx::vmo vmo;
  uint8_t read_buf[PAGE_SIZE];
  File *test_vnode = &vnode<File>();
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);
  vmo.read(read_buf, 0, PAGE_SIZE);
  vmo.reset();
  loop_.RunUntilIdle();
}

TEST_F(MmapTest, GetVmoOnDirectory) {
  zx::vmo vmo;
  {
    fbl::RefPtr<fs::Vnode> test_vnode;
    ASSERT_EQ(root_dir_->Create("dir", S_IFDIR, &test_vnode), ZX_OK);
    fbl::RefPtr<Dir> test_dir = fbl::RefPtr<Dir>::Downcast(std::move(test_vnode));
    ASSERT_EQ(
        test_dir->GetVmo(
            fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
        ZX_ERR_NOT_SUPPORTED);
    vmo.reset();
    loop_.RunUntilIdle();
    test_dir->Close();
  }
}

TEST_F(MmapTest, GetVmoTruncatePartial) {
  constexpr size_t kPageCount = 5;
  constexpr size_t kBufferSize = kPageCount * PAGE_SIZE;
  uint8_t write_buf[kBufferSize];
  for (size_t i = 0; i < kPageCount; ++i) {
    memset(write_buf + (i * PAGE_SIZE), static_cast<int>(i), PAGE_SIZE);
  }
  File *test_vnode = &vnode<File>();
  FileTester::AppendToFile(test_vnode, write_buf, kBufferSize);
  ASSERT_EQ(test_vnode->GetSize(), kBufferSize);

  zx::vmo vmo;
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kSharedBuffer | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  uint8_t read_buf[kBufferSize];
  uint8_t zero_buf[kBufferSize];
  memset(zero_buf, 0, kBufferSize);

  // Truncate partial page
  size_t zero_size = PAGE_SIZE / 4;
  size_t truncate_size = kBufferSize - zero_size;
  test_vnode->Truncate(truncate_size);
  ASSERT_EQ(test_vnode->GetSize(), truncate_size);
  vmo.read(read_buf, 0, kBufferSize);
  ASSERT_EQ(memcmp(read_buf, write_buf, truncate_size), 0);
  ASSERT_EQ(memcmp(read_buf + truncate_size, zero_buf, zero_size), 0);

  zero_size = PAGE_SIZE / 2;
  truncate_size = kBufferSize - zero_size;
  test_vnode->Truncate(truncate_size);
  ASSERT_EQ(test_vnode->GetSize(), truncate_size);
  vmo.read(read_buf, 0, kBufferSize);
  ASSERT_EQ(memcmp(read_buf, write_buf, truncate_size), 0);
  ASSERT_EQ(memcmp(read_buf + truncate_size, zero_buf, zero_size), 0);

  vmo.reset();
  loop_.RunUntilIdle();
}

TEST_F(MmapTest, GetVmoTruncatePage) {
  constexpr size_t kPageCount = 5;
  constexpr size_t kBufferSize = kPageCount * PAGE_SIZE;
  uint8_t write_buf[kBufferSize];
  for (size_t i = 0; i < kPageCount; ++i) {
    memset(write_buf + (i * PAGE_SIZE), static_cast<int>(i), PAGE_SIZE);
  }
  File *test_vnode = &vnode<File>();
  FileTester::AppendToFile(test_vnode, write_buf, kBufferSize);
  ASSERT_EQ(test_vnode->GetSize(), kBufferSize);

  zx::vmo vmo;
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kSharedBuffer | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  uint8_t read_buf[kBufferSize];
  uint8_t zero_buf[kBufferSize];
  memset(zero_buf, 0, kBufferSize);

  // Truncate one page
  size_t zero_size = PAGE_SIZE;
  size_t truncate_size = kBufferSize - zero_size;
  ASSERT_EQ(test_vnode->Truncate(truncate_size), ZX_OK);
  ASSERT_EQ(test_vnode->GetSize(), truncate_size);
  vmo.read(read_buf, 0, kBufferSize);
  ASSERT_EQ(memcmp(read_buf, write_buf, truncate_size), 0);
  ASSERT_EQ(memcmp(read_buf + truncate_size, zero_buf, PAGE_SIZE), 0);

  // Truncate two pages
  zero_size = static_cast<size_t>(PAGE_SIZE) * 2;
  truncate_size = kBufferSize - zero_size;
  ASSERT_EQ(test_vnode->Truncate(truncate_size), ZX_OK);
  ASSERT_EQ(test_vnode->GetSize(), truncate_size);
  vmo.read(read_buf, 0, kBufferSize);
  ASSERT_EQ(memcmp(read_buf, write_buf, truncate_size), 0);
  ASSERT_EQ(memcmp(read_buf + truncate_size, zero_buf, zero_size), 0);

  vmo.reset();
  loop_.RunUntilIdle();
}

TEST_F(MmapTest, GetVmoException) {
  zx::vmo vmo;
  // Execute flag
  fuchsia_io::wire::VmoFlags flags = fuchsia_io::wire::VmoFlags::kExecute;
  File *test_file = &vnode<File>();
  ASSERT_EQ(test_file->GetVmo(flags, &vmo), ZX_ERR_NOT_SUPPORTED);

  vmo.reset();
  loop_.RunUntilIdle();
}

TEST_F(MmapTest, VmoRead) {
  srand(testing::UnitTest::GetInstance()->random_seed());
  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  File *test_vnode = &vnode<File>();
  // trigger page fault to invoke Vnode::VmoRead()
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);

  zx::vmo vmo;
  uint8_t read_buf[PAGE_SIZE];
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);
  vmo.read(read_buf, 0, PAGE_SIZE);
  vmo.reset();
  loop_.RunUntilIdle();

  ASSERT_EQ(memcmp(read_buf, write_buf, PAGE_SIZE), 0);
}

TEST_F(MmapTest, VmoReadSizeException) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  File *test_vnode = &vnode<File>();
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);

  zx::vmo vmo;
  uint8_t read_buf[PAGE_SIZE];
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);
  vmo.read(read_buf, 0, PAGE_SIZE);
  ASSERT_EQ(memcmp(read_buf, write_buf, PAGE_SIZE), 0);

  // Append to file after mmap
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);
  memset(read_buf, 0, PAGE_SIZE);
  vmo.read(read_buf, PAGE_SIZE, PAGE_SIZE);
  ASSERT_NE(memcmp(read_buf, write_buf, PAGE_SIZE), 0);

  vmo.reset();
  loop_.RunUntilIdle();
}

TEST_F(MmapTest, AvoidPagedVmoRaceCondition) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  File *test_vnode = &vnode<File>();
  // trigger page fault to invoke Vnode::VmoRead()
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);

  // Clone a VMO from pager-backed VMO
  zx::vmo vmo;
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  // Close the cloned VMO
  vmo.reset();
  loop_.RunUntilIdle();

  // Make sure pager-backed VMO is not freed without any clones
  ASSERT_EQ(test_vnode->HasPagedVmo(), true);

  // it should be able to handle page fault without any clones
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);

  // Make sure pager-backed VMO is not freed
  ASSERT_EQ(test_vnode->HasPagedVmo(), true);
}

TEST_F(MmapTest, ReleasePagedVmoInVnodeRecycle) {
  srand(testing::UnitTest::GetInstance()->random_seed());

  uint8_t write_buf[PAGE_SIZE];
  for (uint8_t &character : write_buf) {
    character = static_cast<uint8_t>(rand());
  }

  File *test_vnode = &vnode<File>();
  FileTester::AppendToFile(test_vnode, write_buf, PAGE_SIZE);

  // Sync to remove vnode from dirty list.
  WritebackOperation op;
  test_vnode->Writeback(op);
  fs_->SyncFs();

  zx::vmo vmo;
  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  // Pager-backed VMO is not freed becasue vnode is in vnode cache
  vmo.reset();
  loop_.RunUntilIdle();

  // Make sure pager-backed VMO is not freed
  ASSERT_EQ(test_vnode->HasPagedVmo(), true);

  // Release pager-backed VMO directly
  test_vnode->ReleasePagedVmo();

  // Make sure pager-backed VMO is freed
  ASSERT_EQ(test_vnode->HasPagedVmo(), false);

  ASSERT_EQ(
      test_vnode->GetVmo(
          fuchsia_io::wire::VmoFlags::kPrivateClone | fuchsia_io::wire::VmoFlags::kRead, &vmo),
      ZX_OK);

  // Pager-backed VMO has been reallocated
  ASSERT_EQ(test_vnode->HasPagedVmo(), true);

  vmo.reset();
  loop_.RunUntilIdle();

  // Reset the reference of the fixture, and invoke VnodeF2fs::fbl_recycle()
  CloseVnode();

  // Pager-backed VMO should maintain as long as it is alive in VnodeCache
  ASSERT_EQ(test_vnode->HasPagedVmo(), true);
}

}  // namespace
}  // namespace f2fs
