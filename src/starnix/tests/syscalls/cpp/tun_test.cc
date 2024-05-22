// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <net/if.h>
#include <string.h>
#include <sys/ioctl.h>

#include <gtest/gtest.h>
#include <linux/capability.h>
#include <linux/if_tun.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class TunTapTest : public testing::TestWithParam<bool> {};

const char kTunFile[] = "/dev/tun";
const char kTestTunDeviceName[] = "tun_test_tunif0";
const char kTestTapDeviceName[] = "tun_test_tapif0";

TEST_P(TunTapTest, CreateTunTapDevice) {
  if (!test_helper::HasCapability(CAP_NET_ADMIN)) {
    GTEST_SKIP() << "Need CAP_NET_ADMIN to run TunTest";
  }

  bool isTap = GetParam();

  int tun = open(kTunFile, O_RDWR);
  ASSERT_GT(tun, 0);

  const char* const device_name = isTap ? kTestTapDeviceName : kTestTunDeviceName;

  ifreq ifr{};
  ifr.ifr_flags = IFF_NO_PI | (isTap ? IFF_TAP : IFF_TUN);

  strncpy(ifr.ifr_name, device_name, IFNAMSIZ);

  auto result = ioctl(tun, TUNSETIFF, &ifr);
  ASSERT_EQ(result, 0) << errno;

  struct if_nameindex* nameindex = if_nameindex();
  ASSERT_NE(nameindex, nullptr);

  bool found_if = false;

  struct if_nameindex* curr;
  for (curr = nameindex; curr->if_index != 0 || curr->if_name != nullptr; curr++) {
    if (strncmp(curr->if_name, device_name, IFNAMSIZ) == 0) {
      ASSERT_EQ(found_if, false) << "found multiple interfaces with desired name";
      found_if = true;
    }
  }
  if_freenameindex(nameindex);
  ASSERT_EQ(found_if, true);

  ASSERT_EQ(close(tun), 0);
}

INSTANTIATE_TEST_SUITE_P(TunTap, TunTapTest, testing::Bool());

}  // namespace
