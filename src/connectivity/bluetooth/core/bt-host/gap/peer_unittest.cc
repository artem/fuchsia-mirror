// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/gap/peer.h"

#include <lib/async/cpp/executor.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "lib/gtest/test_loop_fixture.h"
#include "src/connectivity/bluetooth/core/bt-host/common/manufacturer_names.h"
#include "src/connectivity/bluetooth/core/bt-host/hci-spec/util.h"

namespace bt::gap {
namespace {

using namespace inspect::testing;

constexpr uint16_t kManufacturer = 0x0001;
constexpr uint16_t kSubversion = 0x0002;

const auto kAdvData = StaticByteBuffer(0x05,  // Length
                                       0x09,  // AD type: Complete Local Name
                                       'T', 'e', 's', 't');

const bt::sm::LTK kLTK;

const DeviceAddress kAddrBrEdr(DeviceAddress::Type::kBREDR, {0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA});
// LE Public Device Address that has the same value as a BR/EDR BD_ADDR, e.g. on
// a dual-mode device.
const DeviceAddress kAddrLeAlias(DeviceAddress::Type::kLEPublic,
                                 {0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA});

const bt::sm::LTK kSecureBrEdrKey(sm::SecurityProperties(true /*encrypted*/, true /*authenticated*/,
                                                         true /*secure_connections*/,
                                                         sm::kMaxEncryptionKeySize),
                                  hci::LinkKey(UInt128{4}, 5, 6));

class PeerTest : public ::gtest::TestLoopFixture {
 public:
  PeerTest() = default;

  void SetUp() override {
    TestLoopFixture::SetUp();
    auto connectable = true;
    address_ = kAddrBrEdr;
    peer_ = std::make_unique<Peer>(fit::bind_member(this, &PeerTest::NotifyListenersCallback),
                                   fit::bind_member(this, &PeerTest::UpdateExpiryCallback),
                                   fit::bind_member(this, &PeerTest::DualModeCallback),
                                   RandomPeerId(), address_, connectable, &metrics_);
  }

  void TearDown() override {
    peer_.reset();
    TestLoopFixture::TearDown();
  }

 protected:
  Peer& peer() { return *peer_; }
  void set_notify_listeners_cb(Peer::NotifyListenersCallback cb) {
    notify_listeners_cb_ = std::move(cb);
  }
  void set_update_expiry_cb(Peer::PeerCallback cb) { update_expiry_cb_ = std::move(cb); }
  void set_dual_mode_cb(Peer::PeerCallback cb) { dual_mode_cb_ = std::move(cb); }

 private:
  void NotifyListenersCallback(const Peer& peer, Peer::NotifyListenersChange change) {
    if (notify_listeners_cb_) {
      notify_listeners_cb_(peer, change);
    }
  }

  void UpdateExpiryCallback(const Peer& peer) {
    if (update_expiry_cb_) {
      update_expiry_cb_(peer);
    }
  }

  void DualModeCallback(const Peer& peer) {
    if (dual_mode_cb_) {
      dual_mode_cb_(peer);
    }
  }

  std::unique_ptr<Peer> peer_;
  DeviceAddress address_;
  Peer::NotifyListenersCallback notify_listeners_cb_;
  Peer::PeerCallback update_expiry_cb_;
  Peer::PeerCallback dual_mode_cb_;
  PeerMetrics metrics_;
};

TEST_F(PeerTest, InspectHierarchy) {
  inspect::Inspector inspector;
  peer().AttachInspect(inspector.GetRoot());

  peer().set_version(hci::HCIVersion::k5_0, kManufacturer, kSubversion);
  ASSERT_TRUE(peer().bredr().has_value());

  // Initialize le_data
  peer().MutLe();
  ASSERT_TRUE(peer().le().has_value());

  peer().MutLe().SetFeatures(hci::LESupportedFeatures{0x0000000000000001});

  peer().MutBrEdr().AddService(UUID(uint16_t{0x110b}));

  auto hierarchy = inspect::ReadFromVmo(inspector.DuplicateVmo());

  // clang-format off
  auto bredr_data_matcher = AllOf(
    NodeMatches(AllOf(
      NameMatches(Peer::BrEdrData::kInspectNodeName),
      PropertyList(UnorderedElementsAre(
        StringIs(Peer::BrEdrData::kInspectConnectionStateName,
                 Peer::ConnectionStateToString(peer().bredr()->connection_state())),
        BoolIs(Peer::BrEdrData::kInspectLinkKeyName, peer().bredr()->bonded()),
        StringIs(Peer::BrEdrData::kInspectServicesName, "{ 0000110b-0000-1000-8000-00805f9b34fb }")
        )))));

  auto le_data_matcher = AllOf(
    NodeMatches(AllOf(
      NameMatches(Peer::LowEnergyData::kInspectNodeName),
      PropertyList(UnorderedElementsAre(
        StringIs(Peer::LowEnergyData::kInspectConnectionStateName,
                 Peer::ConnectionStateToString(peer().le()->connection_state())),
        BoolIs(Peer::LowEnergyData::kInspectBondDataName, peer().le()->bonded()),
        StringIs(Peer::LowEnergyData::kInspectFeaturesName, "0x0000000000000001")
        )))));

  auto peer_matcher = AllOf(
    NodeMatches(
      PropertyList(UnorderedElementsAre(
        StringIs(Peer::kInspectPeerIdName, peer().identifier().ToString()),
        StringIs(Peer::kInspectTechnologyName, TechnologyTypeToString(peer().technology())),
        StringIs(Peer::kInspectAddressName, peer().address().ToString()),
        BoolIs(Peer::kInspectConnectableName, peer().connectable()),
        BoolIs(Peer::kInspectTemporaryName, peer().temporary()),
        StringIs(Peer::kInspectFeaturesName, peer().features().ToString()),
        StringIs(Peer::kInspectVersionName, hci::HCIVersionToString(peer().version().value())),
        StringIs(Peer::kInspectManufacturerName, GetManufacturerName(kManufacturer))
        ))),
    ChildrenMatch(UnorderedElementsAre(bredr_data_matcher, le_data_matcher)));
  // clang-format on
  EXPECT_THAT(hierarchy.value(), AllOf(ChildrenMatch(UnorderedElementsAre(peer_matcher))));
}

TEST_F(PeerTest, BrEdrDataAddServiceNotifiesListeners) {
  // Initialize BrEdrData.
  peer().MutBrEdr();
  ASSERT_TRUE(peer().bredr()->services().empty());

  bool listener_notified = false;
  set_notify_listeners_cb([&](auto&, Peer::NotifyListenersChange change) {
    listener_notified = true;
    // Non-bonded peer should not update bond
    EXPECT_EQ(Peer::NotifyListenersChange::kBondNotUpdated, change);
  });

  constexpr UUID kServiceUuid;
  peer().MutBrEdr().AddService(kServiceUuid);
  EXPECT_TRUE(listener_notified);
  EXPECT_EQ(1u, peer().bredr()->services().count(kServiceUuid));

  // De-duplicate subsequent additions of the same service.
  listener_notified = false;
  peer().MutBrEdr().AddService(kServiceUuid);
  EXPECT_FALSE(listener_notified);
}

TEST_F(PeerTest, BrEdrDataAddServiceOnBondedPeerNotifiesListenersToUpdateBond) {
  // Initialize BrEdrData.
  peer().MutBrEdr().SetBondData({});
  ASSERT_TRUE(peer().bredr()->services().empty());

  bool listener_notified = false;
  set_notify_listeners_cb([&](auto&, Peer::NotifyListenersChange change) {
    listener_notified = true;
    // Bonded peer should update bond
    EXPECT_EQ(Peer::NotifyListenersChange::kBondUpdated, change);
  });

  peer().MutBrEdr().AddService(UUID());
  EXPECT_TRUE(listener_notified);
}

TEST_F(PeerTest, LowEnergyDataSetAdvDataWithInvalidUtf8NameDoesNotUpdatePeerName) {
  peer().MutLe();  // Initialize LowEnergyData.
  ASSERT_FALSE(peer().name().has_value());

  bool listener_notified = false;
  set_notify_listeners_cb([&](auto&, Peer::NotifyListenersChange) { listener_notified = true; });

  const StaticByteBuffer kAdvData(0x05,  // Length
                                  0x09,  // AD type: Complete Local Name
                                  'T', 'e', 's',
                                  0xFF  // 0xFF should not appear in a valid UTF-8 string
  );

  peer().MutLe().SetAdvertisingData(/*rssi=*/0, kAdvData, zx::time());
  EXPECT_TRUE(listener_notified);  // Fresh AD still results in an update
  EXPECT_FALSE(peer().name().has_value());
}

TEST_F(PeerTest, BrEdrDataSetEirDataWithInvalidUtf8NameDoesNotUpdatePeerName) {
  peer().MutBrEdr();  // Initialize BrEdrData.
  ASSERT_FALSE(peer().name().has_value());

  bool listener_notified = false;
  set_notify_listeners_cb([&](auto&, Peer::NotifyListenersChange) { listener_notified = true; });

  const StaticByteBuffer kEirData(0x05,  // Length
                                  0x09,  // AD type: Complete Local Name
                                  'T', 'e', 's',
                                  0xFF  // 0xFF should not appear in a valid UTF-8 string
  );
  hci::ExtendedInquiryResultEventParams eirep;
  eirep.num_responses = 1;
  eirep.bd_addr = peer().address().value();
  MutableBufferView(eirep.extended_inquiry_response, sizeof(eirep.extended_inquiry_response))
      .Write(kEirData);

  peer().MutBrEdr().SetInquiryData(eirep);
  EXPECT_TRUE(listener_notified);  // Fresh EIR data still results in an update
  EXPECT_FALSE(peer().name().has_value());
}

TEST_F(PeerTest, SetNameWithInvalidUtf8NameDoesNotUpdatePeerName) {
  ASSERT_FALSE(peer().name().has_value());

  bool listener_notified = false;
  set_notify_listeners_cb([&](auto&, Peer::NotifyListenersChange) { listener_notified = true; });

  const std::string kName = "Tes\xFF\x01";  // 0xFF should not appear in a valid UTF-8 string
  peer().SetName(kName);
  EXPECT_FALSE(listener_notified);
  EXPECT_FALSE(peer().name().has_value());
}

TEST_F(PeerTest, LowEnergyAdvertisingDataTimestamp) {
  EXPECT_FALSE(peer().MutLe().advertising_data_timestamp());
  peer().MutLe().SetAdvertisingData(/*rssi=*/0, kAdvData, zx::time(1));
  ASSERT_TRUE(peer().MutLe().advertising_data_timestamp());
  EXPECT_EQ(peer().MutLe().advertising_data_timestamp().value(), zx::time(1));

  peer().MutLe().SetAdvertisingData(/*rssi=*/0, kAdvData, zx::time(2));
  ASSERT_TRUE(peer().MutLe().advertising_data_timestamp());
  EXPECT_EQ(peer().MutLe().advertising_data_timestamp().value(), zx::time(2));
}

TEST_F(PeerTest, SettingLowEnergyAdvertisingDataUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().MutLe().SetAdvertisingData(/*rssi=*/0, kAdvData, zx::time(1));
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingLowEnergyConnectionStateUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().MutLe().SetConnectionState(Peer::ConnectionState::kInitializing);
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingLowEnergyBondDataUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  sm::PairingData data;
  data.peer_ltk = kLTK;
  data.local_ltk = kLTK;
  peer().MutLe().SetBondData(data);
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingBrEdrConnectionStateUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().MutBrEdr().SetConnectionState(Peer::ConnectionState::kInitializing);
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingInquiryDataUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  hci::InquiryResult ir;
  ir.bd_addr = kAddrLeAlias.value();
  peer().MutBrEdr().SetInquiryData(ir);
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingBrEdrBondDataUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().MutBrEdr().SetBondData(kSecureBrEdrKey);
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingAddingBrEdrServiceUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().MutBrEdr().AddService(UUID(uint16_t{0x110b}));
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

TEST_F(PeerTest, SettingNameUpdatesLastUpdated) {
  EXPECT_EQ(peer().last_updated(), zx::time(0));

  int notify_count = 0;
  set_notify_listeners_cb([&](const Peer&, Peer::NotifyListenersChange) {
    EXPECT_EQ(peer().last_updated(), zx::time(2));
    notify_count++;
  });

  RunLoopFor(zx::duration(2));
  peer().SetName("name");
  EXPECT_EQ(peer().last_updated(), zx::time(2));
  EXPECT_GE(notify_count, 1);
}

}  // namespace
}  // namespace bt::gap
