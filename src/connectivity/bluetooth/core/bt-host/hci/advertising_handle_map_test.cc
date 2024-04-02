// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/advertising_handle_map.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"

namespace bt::hci {
namespace {

TEST(AdvertisingHandleMapTest, LegacyAndExtended) {
  AdvertisingHandleMap handle_map;
  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});

  std::optional<hci_spec::AdvertisingHandle> legacy_handle =
      handle_map.MapHandle(address, /*extended_pdu=*/false);
  std::optional<hci_spec::AdvertisingHandle> extended_handle =
      handle_map.MapHandle(address, /*extended_pdu=*/true);

  EXPECT_TRUE(legacy_handle);
  EXPECT_TRUE(extended_handle);
  EXPECT_NE(legacy_handle, extended_handle);
}

class AdvertisingHandleMapTest : public testing::TestWithParam<bool> {};
INSTANTIATE_TEST_SUITE_P(AdvertisingHandleMapTest,
                         AdvertisingHandleMapTest,
                         ::testing::Bool());

TEST_P(AdvertisingHandleMapTest, Bidirectional) {
  AdvertisingHandleMap handle_map;

  DeviceAddress address_a = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> handle_a =
      handle_map.MapHandle(address_a, GetParam());
  EXPECT_LE(handle_a.value(), hci_spec::kMaxAdvertisingHandle);
  EXPECT_TRUE(handle_a);

  DeviceAddress address_b = DeviceAddress(DeviceAddress::Type::kLEPublic, {1});
  std::optional<hci_spec::AdvertisingHandle> handle_b =
      handle_map.MapHandle(address_b, GetParam());
  EXPECT_TRUE(handle_b);
  EXPECT_LE(handle_b.value(), hci_spec::kMaxAdvertisingHandle);

  EXPECT_EQ(address_a, handle_map.GetAddress(handle_a.value()));
  EXPECT_EQ(address_b, handle_map.GetAddress(handle_b.value()));
}

TEST_P(AdvertisingHandleMapTest, GetHandleDoesntCreateMapping) {
  AdvertisingHandleMap handle_map;
  EXPECT_EQ(0u, handle_map.Size());
  EXPECT_TRUE(handle_map.Empty());

  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.GetHandle(address, GetParam());
  EXPECT_EQ(0u, handle_map.Size());
  EXPECT_TRUE(handle_map.Empty());
  EXPECT_FALSE(handle);

  handle = handle_map.MapHandle(address, GetParam());
  EXPECT_EQ(1u, handle_map.Size());
  EXPECT_FALSE(handle_map.Empty());
  EXPECT_TRUE(handle);
  EXPECT_EQ(0u, handle.value());

  handle = handle_map.GetHandle(address, GetParam());
  EXPECT_EQ(1u, handle_map.Size());
  EXPECT_FALSE(handle_map.Empty());
  EXPECT_TRUE(handle);
}

TEST_P(AdvertisingHandleMapTest, MapHandleAlreadyExists) {
  AdvertisingHandleMap handle_map;

  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> expected =
      handle_map.MapHandle(address, GetParam());
  EXPECT_LE(expected.value(), hci_spec::kMaxAdvertisingHandle);
  ASSERT_TRUE(expected);

  std::optional<hci_spec::AdvertisingHandle> actual =
      handle_map.MapHandle(address, GetParam());
  EXPECT_LE(actual.value(), hci_spec::kMaxAdvertisingHandle);
  ASSERT_TRUE(actual);
  EXPECT_EQ(expected, actual);
}

TEST_P(AdvertisingHandleMapTest, MapHandleMoreThanSupported) {
  AdvertisingHandleMap handle_map;

  for (uint8_t i = 0; i < handle_map.capacity(); i++) {
    DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {i});
    std::optional<hci_spec::AdvertisingHandle> handle =
        handle_map.MapHandle(address, GetParam());
    EXPECT_LE(handle.value(), hci_spec::kMaxAdvertisingHandle);
    EXPECT_TRUE(handle) << "Couldn't add device address " << i;
    EXPECT_EQ(i + 1u, handle_map.Size());
  }

  DeviceAddress address =
      DeviceAddress(DeviceAddress::Type::kLEPublic, {handle_map.capacity()});

  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.MapHandle(address, GetParam());
  EXPECT_FALSE(handle);
  EXPECT_EQ(handle_map.capacity(), handle_map.Size());
}

TEST_P(AdvertisingHandleMapTest, MapHandleSupportHandleReallocation) {
  AdvertisingHandleMap handle_map;

  for (uint8_t i = 0; i < handle_map.capacity(); i++) {
    DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {i});
    std::optional<hci_spec::AdvertisingHandle> handle =
        handle_map.MapHandle(address, GetParam());
    EXPECT_LE(handle.value(), hci_spec::kMaxAdvertisingHandle);
    EXPECT_TRUE(handle) << "Couldn't add device address " << i;
    EXPECT_EQ(i + 1u, handle_map.Size());
  }

  hci_spec::AdvertisingHandle old_handle = 0;
  std::optional<DeviceAddress> old_address = handle_map.GetAddress(old_handle);
  ASSERT_TRUE(old_address);

  handle_map.RemoveHandle(old_handle);

  DeviceAddress address =
      DeviceAddress(DeviceAddress::Type::kLEPublic, {handle_map.capacity()});
  std::optional<hci_spec::AdvertisingHandle> new_handle =
      handle_map.MapHandle(address, GetParam());
  EXPECT_LE(new_handle.value(), hci_spec::kMaxAdvertisingHandle);

  ASSERT_TRUE(new_handle);
  ASSERT_EQ(old_handle, new_handle.value());

  std::optional<DeviceAddress> new_address =
      handle_map.GetAddress(new_handle.value());
  ASSERT_TRUE(new_address);
  ASSERT_NE(old_address, new_address);
}

TEST_P(AdvertisingHandleMapTest, GetAddressNonExistent) {
  AdvertisingHandleMap handle_map;
  std::optional<DeviceAddress> address = handle_map.GetAddress(0);
  EXPECT_FALSE(address);
}

TEST_P(AdvertisingHandleMapTest, RemoveHandle) {
  AdvertisingHandleMap handle_map;
  EXPECT_TRUE(handle_map.Empty());

  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.MapHandle(address, GetParam());
  ASSERT_TRUE(handle);
  EXPECT_LE(handle.value(), hci_spec::kMaxAdvertisingHandle);
  EXPECT_EQ(1u, handle_map.Size());
  EXPECT_FALSE(handle_map.Empty());

  handle_map.RemoveHandle(handle.value());
  EXPECT_EQ(0u, handle_map.Size());
  EXPECT_TRUE(handle_map.Empty());
}

TEST_P(AdvertisingHandleMapTest, RemoveAddress) {
  AdvertisingHandleMap handle_map;
  EXPECT_TRUE(handle_map.Empty());

  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  handle_map.MapHandle(address, GetParam());
  EXPECT_EQ(1u, handle_map.Size());
  EXPECT_FALSE(handle_map.Empty());

  handle_map.RemoveAddress(address, GetParam());
  EXPECT_EQ(0u, handle_map.Size());
  EXPECT_TRUE(handle_map.Empty());
}

TEST_P(AdvertisingHandleMapTest, RemoveHandleNonExistent) {
  AdvertisingHandleMap handle_map;
  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.MapHandle(address, GetParam());
  ASSERT_TRUE(handle);

  size_t size = handle_map.Size();

  handle_map.RemoveHandle(handle.value() + 1);

  EXPECT_EQ(size, handle_map.Size());
  handle = handle_map.MapHandle(address, GetParam());
  EXPECT_TRUE(handle);
}

TEST_P(AdvertisingHandleMapTest, RemoveAddressNonExistent) {
  AdvertisingHandleMap handle_map;
  DeviceAddress address = DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.MapHandle(address, GetParam());
  ASSERT_TRUE(handle);

  size_t size = handle_map.Size();

  DeviceAddress nonexistent_address =
      DeviceAddress(DeviceAddress::Type::kLEPublic, {1});
  handle_map.RemoveAddress(nonexistent_address, GetParam());

  EXPECT_EQ(size, handle_map.Size());
  handle = handle_map.MapHandle(address, GetParam());
  EXPECT_TRUE(handle);
}

TEST_P(AdvertisingHandleMapTest, Clear) {
  AdvertisingHandleMap handle_map;
  std::optional<hci_spec::AdvertisingHandle> handle =
      handle_map.MapHandle(DeviceAddress(DeviceAddress::Type::kLEPublic, {0}),
                           /*extended_pdu=*/false);
  ASSERT_TRUE(handle);

  EXPECT_LE(handle.value(), hci_spec::kMaxAdvertisingHandle);
  EXPECT_EQ(1u, handle_map.Size());

  handle_map.Clear();
  EXPECT_EQ(0u, handle_map.Size());

  std::optional<DeviceAddress> address = handle_map.GetAddress(handle.value());
  EXPECT_FALSE(address);
}

}  // namespace
}  // namespace bt::hci
