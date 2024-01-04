// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <iostream>

#include <fbl/algorithm.h>
#include <fbl/unique_fd.h>

#include "fs_test_fixture.h"

namespace fs_test {
namespace {

using ParamType = std::tuple<TestFilesystemOptions, /*remount=*/bool>;

constexpr int kMb = 1 << 20;
constexpr int kPrintSize = 100 * kMb;

class MaxFileTest : public BaseFilesystemTest, public testing::WithParamInterface<ParamType> {
 public:
  MaxFileTest() : BaseFilesystemTest(std::get<0>(GetParam())) {}

  bool ShouldRemount() const { return std::get<1>(GetParam()); }
};

// Test writing as much as we can to a file until we run out of space.
TEST_P(MaxFileTest, ReadAfterWriteMaxFileSucceeds) {
  // TODO(https://fxbug.dev/31604): We avoid making files that consume more than half
  // of physical memory. When we can page out files, this restriction
  // should be removed.
  const size_t physmem = zx_system_get_physmem();
  const size_t max_cap = physmem / 2;

  fbl::unique_fd fd(open(GetPath("bigfile").c_str(), O_CREAT | O_RDWR, 0644));
  ASSERT_TRUE(fd);
  char data_a[8192];
  char data_b[8192];
  char data_c[8192];
  memset(data_a, 0xaa, sizeof(data_a));
  memset(data_b, 0xbb, sizeof(data_b));
  memset(data_c, 0xcc, sizeof(data_c));
  size_t sz = 0;
  ssize_t r;

  auto rotate = [&](const char* data) {
    if (data == data_a) {
      return data_b;
    } else if (data == data_b) {
      return data_c;
    } else {
      return data_a;
    }
  };

  const char* data = data_a;
  for (;;) {
    if (sz >= max_cap) {
      std::cout << "Approaching physical memory capacity: " << sz << " bytes" << std::endl;
      r = 0;
      break;
    }

    const auto offset = static_cast<ptrdiff_t>(sz % sizeof(data_a));
    const auto len = static_cast<ptrdiff_t>(sizeof(data_a)) - offset;
    if ((r = write(fd.get(), data + offset, len)) < 0) {
      std::cout << "bigfile received error: " << strerror(errno) << std::endl;
      if ((errno == EFBIG) || (errno == ENOSPC)) {
        // Either the file should be too big (EFBIG) or the file should
        // consume the whole volume (ENOSPC).
        std::cout << "(This was an expected error)" << std::endl;
        r = 0;
      }
      break;
    }
    if ((sz + r) % kPrintSize < (sz % kPrintSize)) {
      std::cout << "wrote " << (sz + r) / kMb << " MB" << std::endl;
    }
    sz += r;
    if (r == len) {
      // Rotate which data buffer we use
      data = rotate(data);
    } else {
      ASSERT_LT(r, len);
    }
  }
  ASSERT_EQ(r, 0) << "Saw an unexpected error from write";
  std::cout << "wrote " << sz << " bytes" << std::endl;

  struct stat buf;
  ASSERT_EQ(fstat(fd.get(), &buf), 0);
  ASSERT_EQ(buf.st_size, static_cast<ssize_t>(sz));

  // Try closing, re-opening, and verifying the file
  ASSERT_EQ(close(fd.release()), 0);
  if (ShouldRemount()) {
    EXPECT_EQ(fs().Unmount().status_value(), ZX_OK);
    EXPECT_EQ(fs().Mount().status_value(), ZX_OK);
  }
  fd.reset(open(GetPath("bigfile").c_str(), O_RDWR, 0644));
  ASSERT_TRUE(fd);
  ASSERT_EQ(fstat(fd.get(), &buf), 0);
  ASSERT_EQ(buf.st_size, static_cast<ssize_t>(sz));
  char readbuf[8192];
  size_t bytes_read = 0;
  data = data_a;
  while (bytes_read < sz) {
    r = read(fd.get(), readbuf, sizeof(readbuf));
    ASSERT_EQ(r, static_cast<ssize_t>(std::min(sz - bytes_read, sizeof(readbuf))));
    ASSERT_EQ(memcmp(readbuf, data, r), 0);
    data = rotate(data);
    bytes_read += r;
  }

  ASSERT_EQ(bytes_read, sz);

  ASSERT_EQ(unlink(GetPath("bigfile").c_str()), 0);
  ASSERT_EQ(close(fd.release()), 0);
}

using MaxFileSizeTest = MaxFileTest;

TEST_P(MaxFileSizeTest, WritingToMaxSupportedOffset) {
  fbl::unique_fd fd(open(GetPath("foo").c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(fd);
  ASSERT_EQ(pwrite(fd.get(), "h", 1, fs().GetTraits().max_file_size - 1), 1);
}

TEST_P(MaxFileTest, WritingBeyondMaxSupportedOffset) {
  if (!fs().GetTraits().supports_sparse_files) {
    return;
  }
  if (fs().GetTraits().name == "memfs") {
    // TODO(https://fxbug.dev/116484): Remove this when the memfs file size limit is back under off_t::max.
    return;
  }
  fbl::unique_fd fd(open(GetPath("foo").c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(fd);
  ASSERT_EQ(pwrite(fd.get(), "h", 1, fs().GetTraits().max_file_size), -1);
}

TEST_P(MaxFileTest, TruncatingToMaxSupportedOffset) {
  if (!fs().GetTraits().supports_sparse_files) {
    return;
  }
  fbl::unique_fd fd(open(GetPath("foo").c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(fd);
  ASSERT_EQ(ftruncate(fd.get(), fs().GetTraits().max_file_size), 0);
}

TEST_P(MaxFileTest, TruncatingBeyondMaxSupportedOffset) {
  if (!fs().GetTraits().supports_sparse_files ||
      fs().GetTraits().max_file_size == std::numeric_limits<off_t>::max()) {
    return;
  }
  fbl::unique_fd fd(open(GetPath("foo").c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(fd);
  ASSERT_EQ(ftruncate(fd.get(), fs().GetTraits().max_file_size + 1), -1);
}

// Test writing to two files, in alternation, until we run out of space. For trivial (sequential)
// block allocation policies, this will create two large files with non-contiguous block
// allocations.
TEST_P(MaxFileTest, ReadAfterNonContiguousWritesSuceeds) {
  // TODO(https://fxbug.dev/31604): We avoid making files that consume more than half
  // of physical memory. When we can page out files, this restriction
  // should be removed.
  const size_t physmem = zx_system_get_physmem();
  const size_t max_cap = physmem / 4;

  fbl::unique_fd fda(open(GetPath("bigfile-A").c_str(), O_CREAT | O_RDWR, 0644));
  fbl::unique_fd fdb(open(GetPath("bigfile-B").c_str(), O_CREAT | O_RDWR, 0644));
  ASSERT_TRUE(fda);
  ASSERT_TRUE(fdb);
  char data_a[8192];
  char data_b[8192];
  memset(data_a, 0xaa, sizeof(data_a));
  memset(data_b, 0xbb, sizeof(data_b));
  size_t sz_a = 0;
  size_t sz_b = 0;
  ssize_t r;

  size_t* sz = &sz_a;
  int fd = fda.get();
  const char* data = data_a;
  for (;;) {
    if (*sz >= max_cap) {
      std::cout << "Approaching physical memory capacity: " << *sz << " bytes" << std::endl;
      r = 0;
      break;
    }

    const auto offset = static_cast<ptrdiff_t>(*sz % sizeof(data_a));
    const auto len = static_cast<ptrdiff_t>(sizeof(data_a)) - offset;
    if ((r = write(fd, data + offset, len)) <= 0) {
      std::cout << "bigfile received error: " << strerror(errno);
      // Either the file should be too big (EFBIG) or the file should
      // consume the whole volume (ENOSPC).
      ASSERT_TRUE(errno == EFBIG || errno == ENOSPC);
      std::cout << "(This was an expected error)";
      break;
    }
    if ((*sz + r) % kPrintSize < (*sz % kPrintSize)) {
      std::cout << "wrote " << (*sz + r) / kMb << " MB";
    }
    *sz += r;
    if (r == len) {
      fd = (fd == fda.get()) ? fdb.get() : fda.get();
      data = (data == data_a) ? data_b : data_a;
      sz = (sz == &sz_a) ? &sz_b : &sz_a;
    } else {
      ASSERT_LT(r, len);
    }
  }
  std::cout << "wrote " << sz_a << " bytes (to A)";
  std::cout << "wrote " << sz_b << " bytes (to B)";

  struct stat buf;
  ASSERT_EQ(fstat(fda.get(), &buf), 0);
  ASSERT_EQ(buf.st_size, static_cast<ssize_t>(sz_a));
  ASSERT_EQ(fstat(fdb.get(), &buf), 0);
  ASSERT_EQ(buf.st_size, static_cast<ssize_t>(sz_b));

  // Try closing, re-opening, and verifying the file
  ASSERT_EQ(close(fda.release()), 0);
  ASSERT_EQ(close(fdb.release()), 0);
  if (ShouldRemount()) {
    EXPECT_EQ(fs().Unmount().status_value(), ZX_OK);
    EXPECT_EQ(fs().Mount().status_value(), ZX_OK);
  }
  fda.reset(open(GetPath("bigfile-A").c_str(), O_RDWR, 0644));
  fdb.reset(open(GetPath("bigfile-B").c_str(), O_RDWR, 0644));
  ASSERT_TRUE(fda);
  ASSERT_TRUE(fdb);

  char readbuf[8192];
  size_t bytes_read_a = 0;
  size_t bytes_read_b = 0;

  fd = fda.get();
  data = data_a;
  sz = &sz_a;
  size_t* bytes_read = &bytes_read_a;
  while (*bytes_read < *sz) {
    r = read(fd, readbuf, sizeof(readbuf));
    ASSERT_EQ(r, static_cast<ssize_t>(std::min(*sz - *bytes_read, sizeof(readbuf))));
    ASSERT_EQ(memcmp(readbuf, data, r), 0);
    *bytes_read += r;

    fd = (fd == fda.get()) ? fdb.get() : fda.get();
    data = (data == data_a) ? data_b : data_a;
    sz = (sz == &sz_a) ? &sz_b : &sz_a;
    bytes_read = (bytes_read == &bytes_read_a) ? &bytes_read_b : &bytes_read_a;
  }

  ASSERT_EQ(bytes_read_a, sz_a);
  ASSERT_EQ(bytes_read_b, sz_b);

  ASSERT_EQ(unlink(GetPath("bigfile-A").c_str()), 0);
  ASSERT_EQ(unlink(GetPath("bigfile-B").c_str()), 0);
  ASSERT_EQ(close(fda.release()), 0);
  ASSERT_EQ(close(fdb.release()), 0);
}

TEST_P(MaxFileTest, PartialWriteWhenFull) {
  fbl::unique_fd fd(open(GetPath("bigfile").c_str(), O_CREAT | O_RDWR, 0644));
  ASSERT_TRUE(fd);
  const TestFilesystemOptions& options = fs().options();
  auto size = options.device_block_size * options.device_block_count * 2;
  auto buf = std::make_unique<uint8_t[]>(size);

  // We should be able to write something.
  ssize_t result = write(fd.get(), buf.get(), size);
  EXPECT_NE(result, -1) << errno;
  EXPECT_NE(result, 0);
  printf("wrote: %zd\n", result);
}

std::string GetParamDescription(const testing::TestParamInfo<ParamType>& param) {
  std::stringstream s;
  s << std::get<0>(param.param) << (std::get<1>(param.param) ? "WithRemount" : "WithoutRemount");
  return s.str();
}

std::vector<ParamType> GetTestCombinations() {
  std::vector<ParamType> test_combinations;
  for (TestFilesystemOptions options : AllTestFilesystems()) {
    // Fatfs is slow and there's no real benefit from having a larger ram-disk.
    if (!options.filesystem->GetTraits().is_slow) {
      // Use a larger ram-disk than the default so that the maximum transaction limit is exceeded
      // for during delayed data allocation on non-FVM-backed Minfs partitions.
      options.device_block_size = 512;
      options.device_block_count = 1'048'576;
      options.fvm_slice_size = 8'388'608;
    }
    test_combinations.emplace_back(options, false);
    if (!options.filesystem->GetTraits().in_memory) {
      test_combinations.emplace_back(options, true);
    }
  }
  return test_combinations;
}

std::vector<ParamType> GetCombinationWithSparseFileSupport() {
  std::vector<ParamType> test_combinations;
  for (TestFilesystemOptions options : AllTestFilesystems()) {
    // For those filesystems that don't support sparse files,  it is not worth the time spent
    // writing a huge file.
    if (!options.filesystem->GetTraits().supports_sparse_files) {
      continue;
    }
    // Use a larger ram-disk than the default so that the maximum transaction limit is exceeded
    // for during delayed data allocation on non-FVM-backed Minfs partitions.
    options.device_block_size = 512;
    options.device_block_count = 1'048'576;
    options.fvm_slice_size = 8'388'608;
    test_combinations.emplace_back(options, false);
    if (!options.filesystem->GetTraits().in_memory) {
      test_combinations.emplace_back(options, true);
    }
  }
  return test_combinations;
}

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, MaxFileTest, testing::ValuesIn(GetTestCombinations()),
                         GetParamDescription);

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, MaxFileSizeTest,
                         testing::ValuesIn(GetCombinationWithSparseFileSupport()),
                         GetParamDescription);

GTEST_ALLOW_UNINSTANTIATED_PARAMETERIZED_TEST(MaxFileSizeTest);

}  // namespace
}  // namespace fs_test
