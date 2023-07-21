// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-sta-ifc.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"

namespace wlan::testing {
namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

constexpr simulation::WlanTxInfo kDefaultTxInfo = {
    .channel = {.primary = 9, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0}};
constexpr wlan_ieee80211::CSsid kApSsid = {.len = 15, .data = {.data_ = "Fuchsia Fake AP"}};
const common::MacAddr kApBssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc});
const common::MacAddr kClientMacAddr({0x11, 0x22, 0x33, 0x44, 0xee, 0xff});
constexpr auto kClientDisassocReason = wlan_ieee80211::ReasonCode::kUnspecifiedReason;
constexpr auto kApDisassocReason = wlan_ieee80211::ReasonCode::kInvalidAuthentication;

class AssocTest : public ::testing::Test, public simulation::StationIfc {
 public:
  AssocTest() : ap_(&env_, kApBssid, kApSsid, kDefaultTxInfo.channel) { env_.AddStation(this); }
  void DisassocFromAp(const common::MacAddr& sta, wlan_ieee80211::ReasonCode reason);
  void FinishAuth();
  simulation::Environment env_;
  simulation::FakeAp ap_;

  unsigned assoc_resp_count_ = 0;
  unsigned disassoc_req_count_ = 0;
  std::list<wlan_ieee80211::StatusCode> assoc_status_list_;
  std::list<wlan_ieee80211::ReasonCode> disassoc_reason_list_;

 private:
  // StationIfc methods
  void Rx(std::shared_ptr<const simulation::SimFrame> frame,
          std::shared_ptr<const simulation::WlanRxInfo> info) override;
};

void validateChannel(const wlan_common::WlanChannel& channel) {
  EXPECT_EQ(channel.primary, kDefaultTxInfo.channel.primary);
  EXPECT_EQ(channel.cbw, kDefaultTxInfo.channel.cbw);
  EXPECT_EQ(channel.secondary80, kDefaultTxInfo.channel.secondary80);
}

void AssocTest::DisassocFromAp(const common::MacAddr& sta, wlan_ieee80211::ReasonCode reason) {
  EXPECT_EQ(ap_.GetNumAssociatedClient(), 1U);
  ap_.DisassocSta(sta, reason);
}

void AssocTest::Rx(std::shared_ptr<const simulation::SimFrame> frame,
                   std::shared_ptr<const simulation::WlanRxInfo> info) {
  ASSERT_EQ(frame->FrameType(), simulation::SimFrame::FRAME_TYPE_MGMT);
  validateChannel(info->channel);
  auto mgmt_frame = std::static_pointer_cast<const simulation::SimManagementFrame>(frame);

  // Ignore the authentication responses.
  if (mgmt_frame->MgmtFrameType() == simulation::SimManagementFrame::FRAME_TYPE_AUTH) {
    return;
  } else if (mgmt_frame->MgmtFrameType() == simulation::SimManagementFrame::FRAME_TYPE_ASSOC_RESP) {
    auto assoc_resp_frame =
        std::static_pointer_cast<const simulation::SimAssocRespFrame>(mgmt_frame);

    EXPECT_EQ(assoc_resp_frame->src_addr_, kApBssid);
    EXPECT_EQ(assoc_resp_frame->dst_addr_, kClientMacAddr);

    assoc_resp_count_++;
    assoc_status_list_.push_back(assoc_resp_frame->status_);
  } else if (mgmt_frame->MgmtFrameType() ==
             simulation::SimManagementFrame::FRAME_TYPE_DISASSOC_REQ) {
    auto disassoc_req_frame =
        std::static_pointer_cast<const simulation::SimDisassocReqFrame>(mgmt_frame);
    EXPECT_EQ(disassoc_req_frame->src_addr_, kApBssid);
    EXPECT_EQ(disassoc_req_frame->dst_addr_, kClientMacAddr);

    disassoc_req_count_++;
    disassoc_reason_list_.push_back(disassoc_req_frame->reason_);
  } else {
    GTEST_FAIL();
  }
}

// Send a authentication request frame at the beginning to make the status for kClientMacAddr is
// AUTHENTICATED in AP.
void AssocTest::FinishAuth() {
  simulation::SimAuthFrame auth_req_frame(kClientMacAddr, kApBssid, 1, simulation::AUTH_TYPE_OPEN,
                                          wlan_ieee80211::StatusCode::kSuccess);
  env_.Tx(auth_req_frame, kDefaultTxInfo, this);
}

/* Verify that association requests that are not properly addressed are ignored.

   Timeline for this test:
   1s: send assoc request on different channel
   2s: send assoc request with different bssid
 */

TEST_F(AssocTest, RefuseIfNotAuthenticated) {
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::usec(50));

  env_.Run(kSimulatedClockDuration);

  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedReasonUnspecified);
  assoc_status_list_.pop_front();
  EXPECT_EQ(assoc_status_list_.size(), 0U);
}

TEST_F(AssocTest, RefusedWrongSsid) {
  static constexpr wlan_ieee80211::CSsid kWrongLenSsid = {.len = 14,
                                                          .data = {.data_ = "Fuchsia Fake A"}};
  static constexpr wlan_ieee80211::CSsid kWrongSsid = {.len = 15,
                                                       .data = {.data_ = "Fuchsia Fake AA"}};

  FinishAuth();

  simulation::SimAssocReqFrame wrong_ssid_len_frame(kClientMacAddr, kApBssid, kWrongLenSsid);
  simulation::SimAssocReqFrame wrong_ssid_frame(kClientMacAddr, kApBssid, kWrongSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, wrong_ssid_len_frame, kDefaultTxInfo, this),
      zx::sec(1));

  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, wrong_ssid_frame, kDefaultTxInfo, this),
      zx::sec(2));

  env_.Run(kSimulatedClockDuration);

  EXPECT_EQ(assoc_resp_count_, 2U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)2);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedReasonUnspecified);
  assoc_status_list_.pop_front();
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedReasonUnspecified);
  assoc_status_list_.pop_front();
}

TEST_F(AssocTest, IgnoredRequests) {
  constexpr simulation::WlanTxInfo kWrongChannelTxInfo = {
      .channel = {.primary = 10, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0}};

  static const common::MacAddr kWrongBssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbd});

  // Schedule assoc req on different channel
  simulation::SimAssocReqFrame wrong_chan_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, wrong_chan_frame, kWrongChannelTxInfo, this),
      zx::sec(1));

  // Schedule assoc req to different bssid
  simulation::SimAssocReqFrame wrong_bssid_frame(kClientMacAddr, kWrongBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, wrong_bssid_frame, kDefaultTxInfo, this),
      zx::sec(2));

  env_.Run(kSimulatedClockDuration);

  // Verify that no assoc responses were seen in the environment
  EXPECT_EQ(assoc_resp_count_, 0U);
}

/* Verify that several association requests sent in quick succession are all answered, and that
   only the first from a client is successful.

   Timeline for this test:
   50 usec: send assoc request
   100 usec: send assoc request
   150 usec: send assoc request
 */
TEST_F(AssocTest, BasicUse) {
  FinishAuth();
  // Schedule first request
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::usec(50));

  // Schedule second request
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::usec(100));

  // Schedule third request
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::usec(150));

  env_.Run(kSimulatedClockDuration);

  EXPECT_EQ(assoc_resp_count_, 3U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)3);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kSuccess);
  assoc_status_list_.pop_front();
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedTemporarily);
  assoc_status_list_.pop_front();
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedTemporarily);
  assoc_status_list_.pop_front();
}

/* Verify that association requests are ignored when the association handling state is set to
   ASSOC_IGNORED.

   Timeline for this test:
   1s: send assoc request
 */
TEST_F(AssocTest, IgnoreAssociations) {
  FinishAuth();
  // Schedule assoc req
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::sec(1));

  ap_.SetAssocHandling(simulation::FakeAp::ASSOC_IGNORED);

  env_.Run(kSimulatedClockDuration);

  // Verify that no assoc responses were seen in the environment
  EXPECT_EQ(assoc_resp_count_, 0U);
}

/* Verify that association requests are refused with kRefusedTemporarily when the association
   handling state is set to ASSOC_REFUSED_TEMPORARILY.

   Timeline for this test:
   1s: send assoc request
 */
TEST_F(AssocTest, TemporarilyRefuseAssociations) {
  // Schedule first request
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::sec(1));

  ap_.SetAssocHandling(simulation::FakeAp::ASSOC_REFUSED_TEMPORARILY);

  env_.Run(kSimulatedClockDuration);

  EXPECT_EQ(assoc_resp_count_, 1U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)1);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedTemporarily);
  assoc_status_list_.pop_front();
}

/* Verify that association requests are refused with REFUSED  when the association handling state is
   set to ASSOC_REFUSED.

   Timeline for this test:
   1s: send assoc request
 */
TEST_F(AssocTest, RefuseAssociations) {
  // Schedule first request
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::sec(1));

  ap_.SetAssocHandling(simulation::FakeAp::ASSOC_REFUSED);

  env_.Run(kSimulatedClockDuration);

  EXPECT_EQ(assoc_resp_count_, 1U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)1);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kRefusedReasonUnspecified);
  assoc_status_list_.pop_front();
}

/* Verify that Disassociation from previously associated STA is handled
   correctly.

   Timeline for this test:
   1s: send assoc request
   2s: send disassoc request
 */
TEST_F(AssocTest, DisassocFromSta) {
  FinishAuth();
  // Schedule assoc req
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::sec(1));

  // Schedule Disassoc request from STA
  simulation::SimDisassocReqFrame disassoc_req_frame(kClientMacAddr, kApBssid,
                                                     kClientDisassocReason);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, disassoc_req_frame, kDefaultTxInfo, this),
      zx::sec(2));

  env_.Run(kSimulatedClockDuration);

  // Verify that one assoc resp was seen and after disassoc the number of
  // clients should be 0.
  EXPECT_EQ(assoc_resp_count_, 1U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)1);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kSuccess);
  EXPECT_EQ(ap_.GetNumAssociatedClient(), 0U);
  assoc_status_list_.pop_front();
}

/* Verify that Disassociation from the FakeAP is handled correctly.

   Timeline for this test:
   1s: send assoc request
   2s: send disassoc request from AP
 */
TEST_F(AssocTest, DisassocFromAp) {
  FinishAuth();
  // Schedule assoc req
  simulation::SimAssocReqFrame assoc_req_frame(kClientMacAddr, kApBssid, kApSsid);
  env_.ScheduleNotification(
      std::bind(&simulation::Environment::Tx, &env_, assoc_req_frame, kDefaultTxInfo, this),
      zx::sec(1));

  // Schedule Disassoc request from AP
  env_.ScheduleNotification(
      std::bind(&AssocTest::DisassocFromAp, this, kClientMacAddr, kApDisassocReason), zx::sec(2));

  env_.Run(kSimulatedClockDuration);

  // Verify that one assoc resp was seen and after disassoc the number of
  // clients should be 0.
  EXPECT_EQ(assoc_resp_count_, 1U);
  ASSERT_EQ(assoc_status_list_.size(), (size_t)1);
  EXPECT_EQ(assoc_status_list_.front(), wlan_ieee80211::StatusCode::kSuccess);
  EXPECT_EQ(ap_.GetNumAssociatedClient(), 0U);
  EXPECT_EQ(disassoc_req_count_, 1U);
  EXPECT_EQ(disassoc_reason_list_.front(), kApDisassocReason);
  assoc_status_list_.pop_front();
  disassoc_reason_list_.pop_front();
}
}  // namespace wlan::testing
