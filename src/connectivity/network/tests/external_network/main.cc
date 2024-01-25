// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// These tests run with an external network interface providing default route
// addresses.

#include <arpa/inet.h>
#include <net/if.h>
#include <sys/ioctl.h>
#include <sys/utsname.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/connectivity/network/tests/os.h"

namespace {

// This is the device name we expect to get from the device name provider,
// specified in meta/device-name-provider.cml.
//
// Since this only used on Fuchsia, it is conditionally compiled.
#if defined(__Fuchsia__)
constexpr char kDerivedDeviceName[] = "fuchsia-test-node-name";
#endif

TEST(ExternalNetworkTest, ConnectToNonRoutableINET) {
  fbl::unique_fd fd;
  ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0)))
      << strerror(errno);

  struct sockaddr_in addr = {
      .sin_family = AF_INET,
      .sin_port = htons(1337),
  };

  // RFC5737#section-3
  //
  // The blocks 192.0.2.0/24 (TEST-NET-1), 198.51.100.0/24 (TEST-NET-2),and
  // 203.0.113.0/24 (TEST-NET-3) are provided for use in documentation.
  ASSERT_EQ(inet_pton(AF_INET, "192.0.2.55", &addr.sin_addr), 1) << strerror(errno);

  ASSERT_EQ(connect(fd.get(), reinterpret_cast<const struct sockaddr*>(&addr), sizeof(addr)), -1);

  // The host env (linux) may have a route to the remote (e.g. default route),
  // resulting in a TCP handshake being attempted and errno being set to
  // EINPROGRESS. In a fuchsia environment, errno will never be set to
  // EINPROGRESS because a TCP handshake will never be performed (the test is
  // run without network configurations that make the remote routable).
  //
  // TODO(https://fxbug.dev/42123494, https://gvisor.dev/issues/1988): Set errno to the
  // same value as linux when a remote is unroutable.
  if (!kIsFuchsia) {
    ASSERT_TRUE(errno == EINPROGRESS || errno == ENETUNREACH) << strerror(errno);
  } else {
    ASSERT_EQ(errno, EHOSTUNREACH) << strerror(errno);
  }

  ASSERT_EQ(close(fd.release()), 0) << strerror(errno);
}

TEST(ExternalNetworkTest, ConnectToNonRoutableINET6) {
  fbl::unique_fd fd;
  ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET6, SOCK_STREAM | SOCK_NONBLOCK, 0)))
      << strerror(errno);

  struct sockaddr_in6 addr = {
      .sin6_family = AF_INET6,
      .sin6_port = htons(1337),
  };

  // RFC3849#section-2
  //
  // The prefix allocated for documentation purposes is 2001:DB8::/32.
  ASSERT_EQ(inet_pton(AF_INET6, "2001:db8::55", &addr.sin6_addr), 1) << strerror(errno);

  ASSERT_EQ(connect(fd.get(), reinterpret_cast<const struct sockaddr*>(&addr), sizeof(addr)), -1);

  // The host env (linux) may have a route to the remote (e.g. default route),
  // resulting in a TCP handshake being attempted and errno being set to
  // EINPROGRESS. In a fuchsia environment, errno will never be set to
  // EINPROGRESS because a TCP handshake will never be performed (the test is
  // run without network configurations that make the remote routable).
  //
  // TODO(https://fxbug.dev/42123494): Set errno to the same value as linux when a
  // remote is unroutable.
  if (!kIsFuchsia) {
    ASSERT_TRUE(errno == EINPROGRESS || errno == ENETUNREACH) << strerror(errno);
  } else {
    ASSERT_EQ(errno, EHOSTUNREACH) << strerror(errno);
  }

  ASSERT_EQ(close(fd.release()), 0) << strerror(errno);
}

TEST(ExternalNetworkTest, GetHostName) {
  char hostname[HOST_NAME_MAX];
  ASSERT_GE(gethostname(hostname, sizeof(hostname)), 0) << strerror(errno);
#if defined(__Fuchsia__)
  ASSERT_STREQ(hostname, kDerivedDeviceName);
#endif
}

TEST(ExternalNetworkTest, Uname) {
  utsname uts;
  ASSERT_EQ(uname(&uts), 0) << strerror(errno);
#if defined(__Fuchsia__)
  ASSERT_STREQ(uts.nodename, kDerivedDeviceName);
#endif
}

constexpr char routable[] = "192.168.0.254";

TEST(ExternalNetworkTest, ConnectToRoutableNonexistentINET) {
  fbl::unique_fd fd;
  ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0))) << strerror(errno);

  struct sockaddr_in addr = {
      .sin_family = AF_INET,
      .sin_port = htons(1337),
  };

  // Connect to a routable address to a non-existing remote. This triggers ARP resolution which
  // is expected to fail.
  ASSERT_EQ(inet_pton(AF_INET, routable, &addr.sin_addr), 1) << strerror(errno);

  EXPECT_EQ(connect(fd.get(), reinterpret_cast<struct sockaddr*>(&addr), sizeof(addr)), -1);
  // TODO: write some netlink to find a routable but unassigned address when we're running on Linux.
  if (!kIsFuchsia) {
    EXPECT_EQ(errno, ETIMEDOUT) << strerror(errno);
  } else {
    EXPECT_EQ(errno, EHOSTUNREACH) << strerror(errno);
  }

  EXPECT_EQ(close(fd.release()), 0) << strerror(errno);
}

// Test to ensure UDP send doesn`t error even with ARP timeouts.
// TODO(https://fxb.dev/35006): Test needs to be extended or replicated to test
// against other transport send errors.
TEST(ExternalNetworkTest, UDPErrSend) {
  fbl::unique_fd fd;
  ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_DGRAM, 0))) << strerror(errno);

  struct sockaddr_in addr = {
      .sin_family = AF_INET,
      .sin_port = htons(1337),
  };
  char bytes[1];

  // Precondition sanity check: write completes without error.
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  EXPECT_EQ(sendto(fd.get(), bytes, sizeof(bytes), 0, reinterpret_cast<struct sockaddr*>(&addr),
                   sizeof(addr)),
            ssize_t(sizeof(bytes)))
      << strerror(errno);

  // Send to a routable address to a non-existing remote. This triggers ARP resolution which is
  // expected to fail, but that failure is expected to leave the socket alive. Before the change
  // that added this test, the socket would be incorrectly shut down.
  ASSERT_EQ(inet_pton(AF_INET, routable, &addr.sin_addr), 1) << strerror(errno);
  EXPECT_EQ(sendto(fd.get(), bytes, sizeof(bytes), 0, reinterpret_cast<struct sockaddr*>(&addr),
                   sizeof(addr)),
            ssize_t(sizeof(bytes)))
      << strerror(errno);

  // Postcondition sanity check: write completes without error.
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  EXPECT_EQ(sendto(fd.get(), bytes, sizeof(bytes), 0, reinterpret_cast<struct sockaddr*>(&addr),
                   sizeof(addr)),
            ssize_t(sizeof(bytes)))
      << strerror(errno);

  EXPECT_EQ(close(fd.release()), 0) << strerror(errno);
}

TEST(ExternalNetworkTest, IoctlGetInterfaceAddresses) {
  fbl::unique_fd fd;
  ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_DGRAM, 0))) << strerror(errno);

  // If `ifc_req` is NULL, SIOCGIFCONF should return the necessary buffer size
  // in bytes for receiving all available addresses in ifc_len. This allows the
  // caller to determine the necessary buffer size beforehand.
  // See: https://man7.org/linux/man-pages/man7/netdevice.7.html
  struct ifconf ifc = {};
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFCONF, &ifc), 0) << strerror(errno);

  struct ifaddr {
    const char* name;
    const char* addr;
  };
  std::vector<ifaddr> expected;
  if (!kIsFuchsia) {
    // On host, only verify the loopback interface, because we don't know which
    // interfaces will exist when this test is run on host (as opposed to when it
    // runs in an emulated network environment on Fuchsia).
    expected = {
        {
            .name = "lo",
            .addr = "127.0.0.1",
        },
    };
    ASSERT_GE(ifc.ifc_len, static_cast<int>(std::size(expected) * sizeof(struct ifreq)));
  } else {
    expected = {
        // The loopback interface should always be present on Fuchsia.
        {
            .name = "lo",
            .addr = "127.0.0.1",
        },
        // This interface with two static addresses is configured in the emulated
        // network environment in which this test is run. This configuration is in
        // this component manifest: meta/test.cml.
        {
            .name = "device",
            .addr = "192.168.0.1",
        },
        {
            .name = "device",
            .addr = "192.168.0.2",
        },
    };
    ASSERT_EQ(ifc.ifc_len, static_cast<int>(std::size(expected) * sizeof(struct ifreq)));
  }
  ASSERT_EQ(ifc.ifc_req, nullptr);

  // Get the interface configuration information.
  // Pass a buffer that is double the required size and verify that nothing is
  // written past `ifc.ifc_len`.
  struct ifreq ifreq_buffer[ifc.ifc_len / sizeof(struct ifreq) + 100];
  const char FILLER = 0xa;
  memset(ifreq_buffer, FILLER, sizeof(ifreq_buffer));
  ifc.ifc_req = ifreq_buffer;
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFCONF, &ifc), 0) << strerror(errno);

  struct ifreq* ifr = ifc.ifc_req;
  for (const auto& expected_ifaddr : expected) {
    ASSERT_STREQ(ifr->ifr_name, expected_ifaddr.name);

    ASSERT_EQ(ifr->ifr_addr.sa_family, AF_INET);
    auto addr = reinterpret_cast<const struct sockaddr_in*>(&ifr->ifr_addr);
    ASSERT_EQ(addr->sin_port, 0);
    char addrbuf[INET_ADDRSTRLEN];
    const char* addrstr = inet_ntop(AF_INET, &addr->sin_addr, addrbuf, sizeof(addrbuf));
    ASSERT_STREQ(addrstr, expected_ifaddr.addr);

    ifr++;
  }
  // Verify that the `ifc.ifc_req` buffer has not been overwritten past
  // `ifc.ifc_len`.
  char* buffer = reinterpret_cast<char*>(ifc.ifc_req);
  for (size_t i = ifc.ifc_len; i < sizeof(ifreq_buffer); i++) {
    EXPECT_EQ(buffer[i], FILLER) << i;
  }
}

}  // namespace
