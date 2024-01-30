// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fault_test.h"

#include <fcntl.h>
#include <sys/uio.h>

#include <gtest/gtest.h>

void* FaultTest::faulting_ptr_ = nullptr;

void FaultTest::SetUpTestSuite() {
  faulting_ptr_ = mmap(nullptr, kFaultingSize_, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(faulting_ptr_, MAP_FAILED);
}

void FaultTest::TearDownTestSuite() {
  faulting_ptr_ = mmap(nullptr, kFaultingSize_, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(faulting_ptr_, MAP_FAILED);
}

void FaultFileTest::SetUp() {
  auto f = GetParam();
  ASSERT_TRUE(fd_ = fbl::unique_fd(f())) << strerror(errno);
}

void FaultFileTest::TearDown() { fd_.reset(); }

void FaultFileTest::SetFdNonBlocking() {
  int flags = fcntl(fd().get(), F_GETFL, 0);
  ASSERT_GE(flags, 0) << strerror(errno);
  ASSERT_EQ(fcntl(fd().get(), F_SETFL, flags | O_NONBLOCK), 0) << strerror(errno);
}

TEST_P(FaultFileTest, Write) {
  ASSERT_EQ(write(fd().get(), faulting_ptr_, kFaultingSize_), -1);
  EXPECT_EQ(errno, EFAULT);
}

TEST_P(FaultFileTest, Read) {
  // First send a valid message that we can read.
  constexpr char kWriteBuf[] = "Hello world";
  ASSERT_EQ(write(fd().get(), &kWriteBuf, sizeof(kWriteBuf)),
            static_cast<ssize_t>(sizeof(kWriteBuf)));
  ASSERT_EQ(lseek(fd().get(), 0, SEEK_SET), 0) << strerror(errno);

  static_assert(kFaultingSize_ >= sizeof(kWriteBuf));
  ASSERT_EQ(read(fd().get(), faulting_ptr_, sizeof(kWriteBuf)), -1);
  EXPECT_EQ(errno, EFAULT);

  // Previous read failed so we should be able to read all the written bytes
  // here.
  char read_buf[sizeof(kWriteBuf)] = {};
  ASSERT_EQ(read(fd().get(), read_buf, sizeof(read_buf)), static_cast<ssize_t>(sizeof(kWriteBuf)));
  EXPECT_STREQ(read_buf, kWriteBuf);
}

TEST_P(FaultFileTest, ReadV) {
  // First send a valid message that we can read.
  constexpr char kWriteBuf[] = "Hello world";
  ASSERT_EQ(write(fd().get(), &kWriteBuf, sizeof(kWriteBuf)),
            static_cast<ssize_t>(sizeof(kWriteBuf)));
  ASSERT_EQ(lseek(fd().get(), 0, SEEK_SET), 0) << strerror(errno);

  char base0[1] = {};
  char base2[sizeof(kWriteBuf) - sizeof(base0)] = {};
  iovec iov[] = {
      {
          .iov_base = base0,
          .iov_len = sizeof(base0),
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = base2,
          .iov_len = sizeof(base2),
      },
  };

  // Read once with iov holding the invalid pointer. We should perform
  // a partial read.
  ASSERT_EQ(readv(fd().get(), iov, std::size(iov)), 1);
  ASSERT_EQ(base0[0], kWriteBuf[0]);

  // Read the rest.
  iov[0] = iovec{};
  iov[1] = iovec{};
  ASSERT_EQ(readv(fd().get(), iov, std::size(iov)), static_cast<ssize_t>(sizeof(base2)));
  EXPECT_STREQ(base2, &kWriteBuf[1]);
}

TEST_P(FaultFileTest, WriteV) {
  char write_buf[] = "Hello world";
  constexpr size_t kBase0Size = 1;
  iovec iov[] = {
      {
          .iov_base = write_buf,
          .iov_len = kBase0Size,
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = reinterpret_cast<char*>(write_buf) + kBase0Size,
          .iov_len = sizeof(write_buf) - kBase0Size,
      },
  };

  // Write with iov holding the invalid pointer.
  ASSERT_EQ(writev(fd().get(), iov, std::size(iov)), -1);
  EXPECT_EQ(errno, EFAULT);
  ASSERT_EQ(lseek(fd().get(), 0, SEEK_SET), 0) << strerror(errno);

  // The file should have no size since the above write failed.
  ASSERT_NO_FATAL_FAILURE(SetFdNonBlocking());
  char recv_buf[sizeof(write_buf)];
  EXPECT_EQ(read(fd().get(), recv_buf, sizeof(recv_buf)), 0);
}
