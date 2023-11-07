// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

using testing::ContainsRegex;
using testing::EndsWith;
using testing::IsSupersetOf;

class SysfsPowerTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Assume starnix always has /sys/power.
    if (!test_helper::IsStarnix() && access("/sys/power", F_OK) == -1) {
      GTEST_SKIP() << "/sys/power not available, skipping...";
    }
  }
};

TEST_F(SysfsPowerTest, PowerDirectoryContainsExpectedContents) {
  std::vector<std::string> suspend_stats_files;
  EXPECT_TRUE(files::ReadDirContents("/sys/power", &suspend_stats_files));
  EXPECT_THAT(suspend_stats_files, IsSupersetOf({"suspend_stats"}));
}

TEST_F(SysfsPowerTest, SuspendStatsDirectoryContainsExpectedContents) {
  std::vector<std::string> suspend_stats_files;
  EXPECT_TRUE(files::ReadDirContents("/sys/power/suspend_stats", &suspend_stats_files));
  EXPECT_THAT(suspend_stats_files,
              IsSupersetOf({"success", "fail", "last_failed_dev", "last_failed_errno"}));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsSuccess) {
  std::string success_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/success", &success_str));
  EXPECT_THAT(success_str, ContainsRegex("^[0-9]+\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsFail) {
  std::string fail_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/fail", &fail_str));
  EXPECT_THAT(fail_str, ContainsRegex("^[0-9]+\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedDev) {
  std::string last_failed_dev_str;
  EXPECT_TRUE(
      files::ReadFileToString("/sys/power/suspend_stats/last_failed_dev", &last_failed_dev_str));
  EXPECT_THAT(last_failed_dev_str, ContainsRegex("^.*\n"));
}

TEST_F(SysfsPowerTest, SuspendStatsFilesContainDefaultsLastFailedErrno) {
  std::string last_failed_errno_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/suspend_stats/last_failed_errno",
                                      &last_failed_errno_str));
  EXPECT_THAT(last_failed_errno_str, ContainsRegex("^(-[0-9]+|0)?\n"));
}

TEST_F(SysfsPowerTest, WakeupCountFileContainsExpectedContents) {
  std::string wakeup_count_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/wakeup_count", &wakeup_count_str));
  EXPECT_THAT(wakeup_count_str, ContainsRegex("^[0-9]+\n"));
}

TEST_F(SysfsPowerTest, WakeupCountFileWrite) {
  EXPECT_FALSE(files::WriteFile("/sys/power/wakeup_count", "test"));
  EXPECT_FALSE(files::WriteFile("/sys/power/wakeup_count", std::to_string(INT_MAX)));

  std::string wakeup_count_str;
  EXPECT_TRUE(files::ReadFileToString("/sys/power/wakeup_count", &wakeup_count_str));
  EXPECT_TRUE(files::WriteFile("/sys/power/wakeup_count", wakeup_count_str));
}
