// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/wlan/common/c/banjo.h>
#include <zircon/errors.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {
namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

// Some default AP and association request values
constexpr wlan_common::WlanChannel kDefaultChannel = {
    .primary = 9, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
constexpr wlan_ieee80211::CSsid kDefaultSsid = {.len = 15, .data = {.data_ = "Fuchsia Fake AP"}};
const common::MacAddr kDefaultBssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc});
const common::MacAddr kSecondBssid({0x12, 0x34, 0x56, 0x78, 0x9b, 0xbd});
const common::MacAddr kMadeupClient({0xde, 0xad, 0xbe, 0xef, 0x00, 0x01});

struct ClientIfc : public SimInterface {
  void DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                 DeauthIndCompleter::Sync& completer) override;

  // Once test is finished, associations and disassociations from teardowns are ignored
  bool test_complete_ = false;
};

class BeaconLostTest : public SimTest {
 public:
  // How long an individual test will run for. We need an end time because tests run until no more
  // events remain and so we need to stop aps from beaconing to drain the event queue.
  static constexpr zx::duration kTestDuration = zx::sec(100);

  void Init();
  void Finish();

  // Move client's location in environment
  void MoveClient(int32_t x, int32_t y);

 protected:
  // This is the interface we will use for our single client interface
  ClientIfc client_ifc_;

  // Keep track of the APs that are in operation so we can easily disable beaconing on all of them
  // at the end of each test.
  std::list<simulation::FakeAp*> aps_;
};

// Ignore any deauth indications that are received during teardown
void ClientIfc::DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                          DeauthIndCompleter::Sync& completer) {
  if (!test_complete_) {
    SimInterface::DeauthInd(request, arena, completer);
  }
}

// Create our device instance and hook up the callbacks
void BeaconLostTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_), ZX_OK);
  env_->ScheduleNotification(std::bind(&BeaconLostTest::Finish, this), kTestDuration);
}

void BeaconLostTest::Finish() {
  for (auto ap : aps_) {
    ap->DisableBeacon();
  }
  aps_.clear();
  client_ifc_.test_complete_ = true;
}

// Move the client (not the test)
void BeaconLostTest::MoveClient(int32_t x, int32_t y) {
  env_->MoveStation(device_->GetSim()->sim_fw->GetHardwareIfc(), x, y);
}

// Verify that deauthorization occurs if associated AP's beacons disappear
TEST_F(BeaconLostTest, NoBeaconDisassocTest) {
  // Create our device instance
  Init();

  // Start up fake AP
  simulation::FakeAp ap(env_.get(), kDefaultBssid, kDefaultSsid, kDefaultChannel);
  ap.EnableBeacon(zx::msec(100));

  // Associate with fake AP
  client_ifc_.AssociateWith(ap, zx::msec(10));

  // disable the beacon after association
  env_->ScheduleNotification(std::bind(&simulation::FakeAp::DisableBeacon, &ap), zx::sec(1));

  env_->Run(kSimulatedClockDuration);

  // Association with fake AP should be successful
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 1U);

  // A deauth should have occurred due a beacon timeout
  EXPECT_EQ(client_ifc_.stats_.deauth_indications.size(), 1U);
}

// Verify that deauthorization occurs after moving away from associated AP
// such that beacon is lost
// t = 0s
// +-------------------------------------------------+
// ap-client-----------------------------------------+
// +-------------------------------------------------+
// t = 1s
// +-------------------------------------------------+
// ap1-----------------------------------------client+
// +-------------------------------------------------+
TEST_F(BeaconLostTest, BeaconTooFarDisassocTest) {
  // Create our device instance
  Init();

  // Start up fake AP
  simulation::FakeAp ap(env_.get(), kDefaultBssid, kDefaultSsid, kDefaultChannel);
  ap.EnableBeacon(zx::msec(100));
  aps_.push_back(&ap);

  // Associate with fake AP
  client_ifc_.AssociateWith(ap, zx::msec(10));

  // Move away from the AP
  env_->ScheduleNotification(std::bind(&BeaconLostTest::MoveClient, this, 150, 0), zx::sec(1));

  env_->Run(kSimulatedClockDuration);

  // Association with fake AP should be successful
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 1U);

  // A deauth should have occurred due to moving away from the AP
  ASSERT_EQ(client_ifc_.stats_.deauth_indications.size(), 1U);
  auto deauth_ind = *client_ifc_.stats_.deauth_indications.begin();
  EXPECT_TRUE(deauth_ind.locally_initiated);
}

// Verify that losing a beacon from an unassociated ap does not cause any disassociation
// t = 0s
// +-------------------------------------------------+
// -------ap1------------client-------------------ap2+
// +-------------------------------------------------+
// t = 1s
// +-------------------------------------------------+
// client-ap1-------------------------------------ap2+
// +-------------------------------------------------+
TEST_F(BeaconLostTest, WrongBeaconLossTest) {
  // Create our device instance
  Init();

  // Start up fake AP
  simulation::FakeAp ap1(env_.get(), kDefaultBssid, kDefaultSsid, kDefaultChannel);
  ap1.EnableBeacon(zx::msec(100));
  constexpr wlan_ieee80211::CSsid kWrongSsid = {.len = 14, .data = {.data_ = "Fuchsia Fake AP"}};
  ASSERT_NE(kDefaultSsid.len, kWrongSsid.len);
  env_->MoveStation(&ap1, -50, 0);
  aps_.push_back(&ap1);
  simulation::FakeAp ap2(env_.get(), kSecondBssid, kWrongSsid, kDefaultChannel);
  ap2.EnableBeacon(zx::msec(100));
  env_->MoveStation(&ap2, 50, 0);
  aps_.push_back(&ap2);

  // Will associate with one AP
  client_ifc_.AssociateWith(ap1, zx::msec(10));

  // Move away closer to the AP we are associated to. Should not impact connection.
  env_->ScheduleNotification(std::bind(&BeaconLostTest::MoveClient, this, -75, 0), zx::sec(1));

  env_->Run(kSimulatedClockDuration);

  // Association with fake AP should be successful
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 1U);

  // No disassociation should occur
  EXPECT_EQ(client_ifc_.stats_.deauth_indications.size(), 0U);
}

// Verify that moving out of range of the beacon for a small duration does not cause a deauth
// t = 0s
// +-------------------------------------------------+
// ap1-client----------------------------------------+
// +-------------------------------------------------+
// t = 1s
// +-------------------------------------------------+
// ap1-------------------client----------------------+
// +-------------------------------------------------+
// t = 2s
// +-------------------------------------------------+
// ap1-client----------------------------------------+
// +-------------------------------------------------+
TEST_F(BeaconLostTest, TempBeaconLossTest) {
  // Create our device instance
  Init();

  // Start up fake AP
  simulation::FakeAp ap1(env_.get(), kDefaultBssid, kDefaultSsid, kDefaultChannel);
  ap1.EnableBeacon(zx::msec(100));
  constexpr wlan_ieee80211::CSsid kWrongSsid = {.len = 14, .data = {.data_ = "Fuchsia Fake AP"}};
  ASSERT_NE(kDefaultSsid.len, kWrongSsid.len);
  env_->MoveStation(&ap1, 0, 0);
  aps_.push_back(&ap1);

  // Will associate with one AP
  client_ifc_.AssociateWith(ap1, zx::msec(10));

  // Move away from the AP we are associated to.
  env_->ScheduleNotification(std::bind(&BeaconLostTest::MoveClient, this, 100, 0), zx::sec(1));

  // A second later, move back
  env_->ScheduleNotification(std::bind(&BeaconLostTest::MoveClient, this, 0, 0), zx::sec(2));

  env_->Run(kSimulatedClockDuration);

  // Association with fake AP should be successful
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 1U);

  // No disassociation should occur
  EXPECT_EQ(client_ifc_.stats_.deauth_indications.size(), 0U);
}

}  // namespace wlan::brcmfmac
