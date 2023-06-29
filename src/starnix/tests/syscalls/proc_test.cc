// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/proc_test_base.h"
#include "src/starnix/tests/syscalls/test_helper.h"

class ProcUptimeTest : public ProcTestBase {
 protected:
  void SetUp() override {
    ProcTestBase::SetUp();
    Open();
  }

  void Close() {
    if (fd > 0) {
      close(fd);
      fd = -1;
    }
  }

  void Open() {
    Close();
    std::string path = proc_path() + "/uptime";
    fd = open(path.c_str(), O_RDONLY);
    ASSERT_GT(fd, 0);
  }

  std::pair<double, double> Parse(const char* buf) {
    double uptime, idle;
    int s = sscanf(buf, "%lf %lf\n", &uptime, &idle);
    EXPECT_EQ(s, 2);
    return std::make_pair(uptime, idle);
  }

  std::pair<double, double> Read() {
    char buf[100];
    long r = read(fd, buf, sizeof(buf));
    EXPECT_GT(r, 0);

    return Parse(buf);
  }

  ~ProcUptimeTest() override { Close(); }

  int fd = -1;
};

TEST_F(ProcUptimeTest, UptimeRead) { Read(); }

TEST_F(ProcUptimeTest, UptimeProgressReopen) {
  auto v1 = Read();
  Close();
  sleep(1);
  Open();
  auto v2 = Read();
  EXPECT_GT(v2.first, v1.first);
  EXPECT_GT(v2.second, v1.second);
}

// Verify that the reported value is updated after seeking /proc/uptime to the beginning.
TEST_F(ProcUptimeTest, UptimeProgressSeek) {
  auto v1 = Read();

  off_t pos = lseek(fd, 0, SEEK_SET);
  ASSERT_EQ(pos, 0);

  sleep(1);
  auto v2 = Read();

  EXPECT_GT(v2.first, v1.first);
  EXPECT_GT(v2.second, v1.second);
}

// Verify that a valid value is produced even when reading by single char.
TEST_F(ProcUptimeTest, UptimeByChar) {
  auto v1 = Read();
  Open();

  std::string buf;
  char c;
  ssize_t r = read(fd, &c, 1);
  ASSERT_EQ(r, 1);
  buf.push_back(c);

  // Keep the FD, and then read from a new FD.
  int old_fd = fd;
  fd = -1;
  Open();
  auto v2 = Read();

  // Wait for a bit and then read the old FD to the end.
  sleep(1);
  while ((r = read(old_fd, &c, 1)) == 1) {
    buf.push_back(c);
  }
  close(old_fd);

  auto v3 = Parse(buf.c_str());

  EXPECT_LE(v1.first, v3.first);
  EXPECT_LE(v1.second, v3.second);

  EXPECT_LE(v3.first, v2.first);
  EXPECT_LE(v3.second, v2.second);
}

class ProcSysNetTest : public ProcTestBase,
                       public ::testing::WithParamInterface<std::tuple<const char*, const char*>> {
 protected:
  void SetUp() override {
    ProcTestBase::SetUp();
    // Required to open the path below for writing.
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping.";
    }

    fd.reset();

    auto const& [dev, path_fmt] = GetParam();
    char buf[100] = {};
    sprintf(buf, path_fmt, dev);
    std::string path = proc_path() + "/sys/net" + buf;
    ASSERT_TRUE(fd = fbl::unique_fd(open(path.c_str(), O_RDWR)))
        << "path: " + path + "; " + strerror(errno);
  }

  fbl::unique_fd fd;
};

TEST_P(ProcSysNetTest, Write) {
  constexpr uint8_t kWriteBuf = 127;
  ASSERT_EQ(write(fd.get(), &kWriteBuf, sizeof(kWriteBuf)), static_cast<ssize_t>(sizeof(kWriteBuf)))
      << strerror(errno);
}

INSTANTIATE_TEST_SUITE_P(
    ProcSysNetTest, ProcSysNetTest,
    ::testing::Combine(
        ::testing::Values("all", "default"),
        ::testing::Values("/ipv4/neigh/%s/ucast_solicit", "/ipv4/neigh/%s/retrans_time_ms",
                          "/ipv4/neigh/%s/mcast_resolicit", "/ipv6/conf/%s/accept_ra",
                          "/ipv6/conf/%s/dad_transmits", "/ipv6/conf/%s/use_tempaddr",
                          "/ipv6/conf/%s/addr_gen_mode", "/ipv6/conf/%s/stable_secret",
                          "/ipv6/conf/%s/disable_ipv6", "/ipv6/neigh/%s/ucast_solicit",
                          "/ipv6/neigh/%s/retrans_time_ms", "/ipv6/neigh/%s/mcast_resolicit")));
