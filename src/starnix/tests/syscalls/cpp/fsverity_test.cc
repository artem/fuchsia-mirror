// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/vfs.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <cstdint>

#include <gtest/gtest.h>
#include <linux/capability.h>
#include <linux/fs.h>
#include <linux/fsverity.h>
#include <linux/stat.h>

#include "fidl/fuchsia.fs/cpp/common_types.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class FsverityTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::IsStarnix()) {
      // TODO(fxbug.dev/302596745): Find a way to support this.
      GTEST_SKIP()
          << "This test does not generally work on Linux as it requires a kernel with fsverity.";
    }

    const char *tmpdir = getenv("FSVERITY_TMPDIR");
    test_filename_ = std::string(tmpdir) + "/fsverity";

    int fd = open(test_filename_.c_str(), O_CREAT | O_RDWR);
    ASSERT_GT(fd, 0) << fd << " errno:" << errno;
    write(fd, "foo", 3);
    close(fd);
  }

  void TearDown() override { unlink(test_filename_.c_str()); }

 protected:
  const char *fname() const { return test_filename_.c_str(); }

 private:
  std::string test_filename_;
};

TEST(FsverityUnitTest, OriginalFileHandle) {
  if (!test_helper::IsStarnix()) {
    // TODO(fxbug.dev/302596745): Find a way to support this.
    GTEST_SKIP()
        << "This test does not generally work on Linux as it requires a kernel with fsverity.";
  }

  const char *tmpdir = getenv("FSVERITY_TMPDIR");
  std::string filename = std::string(tmpdir) + "/fsverity";
  int fd = open(filename.c_str(), O_CREAT | O_RDWR);
  ASSERT_GT(fd, 0) << fd << " errno:" << errno;
  write(fd, "foo", 3);

  // Can't enable fsverity using original writable file handle.
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, ETXTBSY);
  }

  // Can't enable if we still have the a writable file handle open.
  {
    int fd2 = open(filename.c_str(), O_RDONLY);
    ASSERT_GT(fd2, 0) << fd2;
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    ASSERT_EQ(ioctl(fd2, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, ETXTBSY);
    close(fd2);
  }
  close(fd);
}

TEST_F(FsverityTest, Version) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  fsverity_enable_arg arg = {
      .version = 2, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
  ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
  ASSERT_EQ(errno, EINVAL);
  close(fd);
}

TEST_F(FsverityTest, BlockSize) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 512};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EINVAL);
  }
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 1025};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EINVAL);
  }
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 16384};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EINVAL);
  }
  close(fd);
}

TEST_F(FsverityTest, HashAlgorithm) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  // Unsupported/invalid algorithm.
  {
    fsverity_enable_arg arg = {.version = 1, .hash_algorithm = 9, .block_size = 4096};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, ENOTSUP);
  }
  close(fd);
}
TEST_F(FsverityTest, Salt) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  // Very long salt is not supported.
  {
    char salt[64] = "1234";
    fsverity_enable_arg arg = {.version = 1,
                               .hash_algorithm = FS_VERITY_HASH_ALG_SHA256,
                               .block_size = 4096,
                               .salt_size = 48,
                               .salt_ptr = reinterpret_cast<uint64_t>(&salt[0])};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EINVAL);
  }
  close(fd);
}

TEST_F(FsverityTest, Signatures) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  // Signatures not yet supported.
  {
    fsverity_enable_arg arg = {.version = 1,
                               .hash_algorithm = FS_VERITY_HASH_ALG_SHA256,
                               .block_size = 4096,
                               .sig_size = 1};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, ENOTSUP);
  }
  close(fd);
}

TEST_F(FsverityTest, MeasureVerityWhenNotVerity) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  // We should get ENODATA if we ask for the digest.
  char buf[64];
  memset(&buf[0], 0, sizeof(buf));
  fsverity_digest arg = {.digest_algorithm = FS_VERITY_HASH_ALG_SHA256, .digest_size = 32};
  memcpy(&buf[0], &arg, sizeof(arg));
  ASSERT_EQ(ioctl(fd, FS_IOC_MEASURE_VERITY, &buf), -1);
  ASSERT_EQ(errno, ENODATA);
  close(fd);
}

TEST_F(FsverityTest, EnableVerity) {
  int fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);
  // TODO(fxbug.dev/300003181): Replace this when we switch to native support.
  // We are currently storing these ioctls in extended attributes which minfs
  // does not support. Minfs here acts as a test of the "ENOTSUP" case.
  bool is_minfs;
  {
    struct statfs64 fs;
    ASSERT_EQ(statfs64(fname(), &fs), 0);
    is_minfs = fs.f_type == static_cast<uint32_t>(fuchsia_fs::VfsType::kMinfs);
  }

  // Valid enable request, no salt, no sig.
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    if (is_minfs) {
      ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1) << errno;
      ASSERT_EQ(errno, EOPNOTSUPP);
      close(fd);
      return;
    }
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), 0) << errno;
  }
  // A second attempt should return EBUSY (if building) or EEXIST (if done).
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_TRUE(errno == EBUSY || errno == EEXIST);
  }

  // Now we should get back a digest for the data once fsverity has finished building.
  {
    char buf[64];
    fsverity_digest arg = {.digest_algorithm = FS_VERITY_HASH_ALG_SHA256, .digest_size = 32};
    memcpy(&buf[0], &arg, sizeof(arg));
    // Ugly polling wait for fsverity to build.
    for (int i = 0; i < 10000; i++) {
      if (ioctl(fd, FS_IOC_MEASURE_VERITY, &buf) == 0) {
        break;
      }
      ASSERT_EQ(errno, ENODATA);
      usleep(10000);
    }
    ASSERT_EQ(ioctl(fd, FS_IOC_MEASURE_VERITY, &buf), 0) << errno;

    // Obtained via:
    // ```
    // $ echo -ne "foo" > /tmp/foo.txt
    // $ fsverity digest /tmp/foo.txt
    // sha256:84c7384b3239274691380d7042dc3d8c13f9e606ef546544fe9e348afb0e8af5 /tmp/foo.txt
    // ```
    uint8_t expected_digest[32] = {0x84, 0xc7, 0x38, 0x4b, 0x32, 0x39, 0x27, 0x46, 0x91, 0x38, 0x0d,
                                   0x70, 0x42, 0xdc, 0x3d, 0x8c, 0x13, 0xf9, 0xe6, 0x06, 0xef, 0x54,
                                   0x65, 0x44, 0xfe, 0x9e, 0x34, 0x8a, 0xfb, 0x0e, 0x8a, 0xf5

    };
    ASSERT_TRUE(memcmp(&expected_digest[0], &buf[4], 32) == 0);
  }
  // Enabling now should return EEXIST
  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EEXIST);
  }

  close(fd);

  // The file is now using fsverity. Check that it persists across a fresh file handle.
  fd = open(fname(), O_RDONLY);
  ASSERT_GT(fd, 0);

  {
    fsverity_enable_arg arg = {
        .version = 1, .hash_algorithm = FS_VERITY_HASH_ALG_SHA256, .block_size = 4096};
    ASSERT_EQ(ioctl(fd, FS_IOC_ENABLE_VERITY, &arg), -1);
    ASSERT_EQ(errno, EEXIST);
  }
  // TODO(fxbug.dev/300003181): Test FS_IOC_READ_VERITY_METADATA -- Merkle Tree

  // Test FS_IOC_READ_VERITY_METADATA -- Descriptor
  {
    fsverity_descriptor descriptor = {};
    fsverity_read_metadata_arg arg = {
        .metadata_type = FS_VERITY_METADATA_TYPE_DESCRIPTOR,
        .offset = 0,
        .length = sizeof(descriptor),
        .buf_ptr = reinterpret_cast<__u64>(&descriptor),
    };
    ASSERT_EQ(ioctl(fd, FS_IOC_READ_VERITY_METADATA, &arg), 0);
    // Obtained via:
    // ```
    // $ echo -ne "foo" > /tmp/foo.txt
    // $ fsverity digest /tmp/foo.txt --out-descriptor=/tmp/descr
    // $ hexdump /tmp/descr -e "16/1 \"0x%02x,\" \"\n\"" -v
    uint8_t expected_descriptor[] = {
        0x01, 0x01, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xdf, 0xfd, 0xd9, 0x7c, 0xfb, 0xf2, 0x88, 0xa7, 0x29, 0xf6,
        0xaf, 0x66, 0xf1, 0x2a, 0xc8, 0x88, 0x4f, 0xd7, 0x8d, 0xf3, 0xf1, 0x87, 0x6d,
        0xcc, 0xc5, 0x8b, 0x5a, 0xb2, 0x36, 0x83, 0x9b, 0x49, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00};
    ASSERT_TRUE(memcmp(&expected_descriptor[0], &descriptor, sizeof(expected_descriptor)) == 0);
  }
  // Test FS_IOC_READ_VERITY_METADATA -- Signature (not supported)
  {
    char buf[1024];
    fsverity_read_metadata_arg arg = {
        .metadata_type = FS_VERITY_METADATA_TYPE_SIGNATURE,
        .offset = 0,
        .length = sizeof(buf),
        .buf_ptr = reinterpret_cast<__u64>(&buf),
    };
    ASSERT_EQ(ioctl(fd, FS_IOC_READ_VERITY_METADATA, &arg), -1);
    ASSERT_EQ(errno, ENOTSUP);
  }

  // Test FS_IOC_GETFLAGS for FS_VERITY_FL.
  {
    __u32 flags = 0;
    ASSERT_EQ(ioctl(fd, FS_IOC_GETFLAGS, &flags), 0);
    ASSERT_TRUE(flags | FS_VERITY_FL);
  }
  close(fd);

  // The file is now using fsverity. We can't open it in write mode.
  {
    int fd = open(fname(), O_RDWR);
    ASSERT_EQ(fd, -1);
    ASSERT_EQ(errno, EACCES);
  }
}
//
// TODO(fxbug.dev/302604990): Test statx.

}  // namespace
