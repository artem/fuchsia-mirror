// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <zircon/errors.h>

#include <functional>
#include <list>
#include <memory>
#include <optional>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_utils.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {
namespace {

constexpr zx::duration kScanStartTime = zx::sec(1);
constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

struct ApInfo {
  explicit ApInfo(simulation::Environment* env, const common::MacAddr& bssid,
                  const wlan_ieee80211::CSsid& ssid, const wlan_common::WlanChannel& channel)
      : ap_(env, bssid, ssid, channel) {}

  simulation::FakeAp ap_;
  bool probe_resp_seen_ = false;
};

struct ClientIfc : public SimInterface {
 public:
  void OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                    OnScanResultCompleter::Sync& completer) override {
    on_scan_result_(&request->result);
    completer.buffer(arena).Reply();
  }
  void OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                 OnScanEndCompleter::Sync& completer) override {
    on_scan_end_(&request->end);
    completer.buffer(arena).Reply();
  }

  std::function<void(const wlan_fullmac_wire::WlanFullmacScanResult*)> on_scan_result_;
  std::function<void(const wlan_fullmac_wire::WlanFullmacScanEnd*)> on_scan_end_;
};

class ActiveScanTest : public SimTest {
 public:
  static constexpr zx::duration kBeaconInterval = zx::msec(100);
  static constexpr uint32_t kDwellTimeMs = 120;

  ActiveScanTest() {
    client_ifc_.on_scan_result_ =
        std::bind(&ActiveScanTest::OnScanResult, this, std::placeholders::_1);
    client_ifc_.on_scan_end_ = std::bind(&ActiveScanTest::OnScanEnd, this, std::placeholders::_1);
  }
  void Init();
  void StartFakeAp(const common::MacAddr& bssid, const wlan_ieee80211::CSsid& ssid,
                   const wlan_common::WlanChannel& channel,
                   zx::duration beacon_interval = kBeaconInterval);

  void StartScan(const wlan_fullmac_wire::WlanFullmacImplStartScanRequest* req);
  // TODO(fxbug.dev/https://fxbug.dev/83861): Align the way active_scan_test and passive_scan_test
  // verify scan results.
  void VerifyScanResults();
  void EndSimulation();

  bool all_aps_seen_ = false;

  void GetFirmwarePfnMac();

  uint32_t GetNumProbeReqsSeen() { return num_probe_reqs_seen; }

  void OnScanResult(const wlan_fullmac_wire::WlanFullmacScanResult* result);
  void OnScanEnd(const wlan_fullmac_wire::WlanFullmacScanEnd* end);

 protected:
  // This is the interface we will use for our single client interface
  ClientIfc client_ifc_;

  // The default active scan request
  const uint8_t default_channels_list_[5] = {1, 2, 3, 4, 5};

  // Txn ID for the current scan
  uint64_t scan_txn_id_ = 0;

  // Result of the previous scan
  wlan_fullmac_wire::WlanScanResult scan_result_code_ = wlan_fullmac_wire::WlanScanResult::kSuccess;

  std::list<wlan_fullmac_wire::WlanFullmacScanResult> scan_results_;
  // BSS's IEs are raw pointers. Store the IEs here so we don't have dangling pointers
  std::vector<std::vector<uint8_t>> seen_ies_;

 private:
  // StationIfc methods
  void Rx(std::shared_ptr<const simulation::SimFrame> frame,
          std::shared_ptr<const simulation::WlanRxInfo> info) override;

  // All simulated APs
  std::list<std::unique_ptr<ApInfo>> aps_;

  // Mac address of sim_fw
  common::MacAddr sim_fw_mac_;
  common::MacAddr last_pfn_mac_ = common::kZeroMac;
  std::optional<common::MacAddr> sim_fw_pfn_mac_;
  uint32_t num_probe_reqs_seen = 0;
};

void ActiveScanTest::OnScanResult(const wlan_fullmac_wire::WlanFullmacScanResult* result) {
  ASSERT_NE(result, nullptr);
  EXPECT_EQ(scan_txn_id_, result->txn_id);

  wlan_fullmac_wire::WlanFullmacScanResult copy = *result;
  // Copy the IES data over since the original location may change data by the time we verify.
  std::vector<uint8_t> ies(copy.bss.ies.data(), copy.bss.ies.data() + copy.bss.ies.count());
  seen_ies_.push_back(ies);
  copy.bss.ies = fidl::VectorView<uint8_t>::FromExternal(*seen_ies_.rbegin());
  scan_results_.emplace_back(copy);
}

void ActiveScanTest::OnScanEnd(const wlan_fullmac_wire::WlanFullmacScanEnd* end) {
  scan_result_code_ = end->code;
}

void ActiveScanTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_), ZX_OK);

  // Get the interface MAC address
  client_ifc_.GetMacAddr(&sim_fw_mac_);
}

void ActiveScanTest::StartFakeAp(const common::MacAddr& bssid, const wlan_ieee80211::CSsid& ssid,
                                 const wlan_common::WlanChannel& channel,
                                 zx::duration beacon_interval) {
  auto ap_info = std::make_unique<ApInfo>(env_.get(), bssid, ssid, channel);
  // Beacon is also enabled here to make sure this is not disturbing the correct result.
  ap_info->ap_.EnableBeacon(beacon_interval);
  aps_.push_back(std::move(ap_info));
}

// Tell the DUT to run a scan
void ActiveScanTest::StartScan(const wlan_fullmac_wire::WlanFullmacImplStartScanRequest* req) {
  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->StartScan(*req);
}

// Called when simulation time has run out. Takes down all fake APs and the simulated DUT.
void ActiveScanTest::EndSimulation() {
  for (auto& ap_info : aps_) {
    ap_info->ap_.DisableBeacon();
  }
}

void ActiveScanTest::GetFirmwarePfnMac() {
  brcmf_simdev* sim = device_->GetSim();
  if (!sim_fw_pfn_mac_) {
    struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, client_ifc_.iface_id_);
    zx_status_t status =
        brcmf_fil_iovar_data_get(ifp, "pfn_macaddr", sim_fw_pfn_mac_->byte, ETH_ALEN, nullptr);
    EXPECT_EQ(status, ZX_OK);
  }
}

void ActiveScanTest::VerifyScanResults() {
  // TODO(fxbug.dev/83883): Verify the timestamp of each scan result.
  for (auto result : scan_results_) {
    int matches_seen = 0;

    for (auto& ap_info : aps_) {
      common::MacAddr mac_addr = ap_info->ap_.GetBssid();
      ASSERT_EQ(result.bss.bssid.size(), sizeof(mac_addr.byte));
      if (!std::memcmp(result.bss.bssid.data(), mac_addr.byte, sizeof(mac_addr.byte))) {
        ap_info->probe_resp_seen_ = true;
        matches_seen++;

        // Verify SSID
        wlan_ieee80211::CSsid ssid_info = ap_info->ap_.GetSsid();
        auto ssid = brcmf_find_ssid_in_ies(result.bss.ies.data(), result.bss.ies.count());
        EXPECT_EQ(ssid.size(), ssid_info.len);
        ASSERT_LE(ssid_info.len, ssid_info.data.size());
        EXPECT_EQ(memcmp(ssid.data(), ssid_info.data.data(), ssid_info.len), 0);

        // Verify channel
        wlan_common::WlanChannel channel = ap_info->ap_.GetChannel();
        EXPECT_EQ(result.bss.channel.primary, channel.primary);
        EXPECT_EQ(result.bss.channel.cbw, channel.cbw);
        EXPECT_EQ(result.bss.channel.secondary80, channel.secondary80);

        // Verify has RSSI value
        ASSERT_LT(result.bss.rssi_dbm, 0);
        ASSERT_EQ(result.bss.snr_db,
                  sim_utils::SnrDbFromSignalStrength(result.bss.rssi_dbm, simulation::kNoiseLevel));
      }
    }

    // There should be exactly one AP per result.
    EXPECT_EQ(matches_seen, 1);
  }

  for (auto& ap_info : aps_) {
    if (ap_info->probe_resp_seen_ == false) {
      // Failure
      return;
    }
  }

  // pfn mac should be different from the one from last scan.
  EXPECT_NE(last_pfn_mac_, *sim_fw_pfn_mac_);
  last_pfn_mac_ = *sim_fw_pfn_mac_;

  // pfn mac will be set back to firmware mac after active scan.
  GetFirmwarePfnMac();
  EXPECT_EQ(sim_fw_mac_, *sim_fw_pfn_mac_);

  sim_fw_pfn_mac_.reset();

  // The probe response from all APs were seen
  all_aps_seen_ = true;
}

void ActiveScanTest::Rx(std::shared_ptr<const simulation::SimFrame> frame,
                        std::shared_ptr<const simulation::WlanRxInfo> info) {
  GetFirmwarePfnMac();

  ASSERT_EQ(frame->FrameType(), simulation::SimFrame::FRAME_TYPE_MGMT);

  auto mgmt_frame = std::static_pointer_cast<const simulation::SimManagementFrame>(frame);

  if (mgmt_frame->MgmtFrameType() == simulation::SimManagementFrame::FRAME_TYPE_PROBE_REQ) {
    // When a probe request is sent out, the src mac address should not be real mac address.
    auto probe_req = std::static_pointer_cast<const simulation::SimProbeReqFrame>(mgmt_frame);
    EXPECT_NE(probe_req->src_addr_, sim_fw_mac_);
    EXPECT_EQ(probe_req->src_addr_, *sim_fw_pfn_mac_);
    num_probe_reqs_seen++;
  }

  if (mgmt_frame->MgmtFrameType() == simulation::SimManagementFrame::FRAME_TYPE_PROBE_RESP) {
    auto probe_resp = std::static_pointer_cast<const simulation::SimProbeRespFrame>(mgmt_frame);
    EXPECT_NE(probe_resp->dst_addr_, sim_fw_mac_);
    EXPECT_EQ(probe_resp->dst_addr_, *sim_fw_pfn_mac_);
  }
}

// AP 1&2 on channel 2.
constexpr wlan_common::WlanChannel kDefaultChannel1 = {
    .primary = 2, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
constexpr wlan_ieee80211::CSsid kAp1Ssid = {.len = 16, .data = {.data_ = "Fuchsia Fake AP1"}};
const common::MacAddr kAp1Bssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc});
constexpr wlan_ieee80211::CSsid kAp2Ssid = {.len = 16, .data = {.data_ = "Fuchsia Fake AP2"}};
const common::MacAddr kAp2Bssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbd});

// AP 3 on channel 4.
constexpr wlan_common::WlanChannel kDefaultChannel2 = {
    .primary = 4, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
constexpr wlan_ieee80211::CSsid kAp3Ssid = {.len = 16, .data = {.data_ = "Fuchsia Fake AP3"}};
const common::MacAddr kAp3Bssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbe});

// This test case might fail in a very low possibility because it's random.
TEST_F(ActiveScanTest, RandomMacThreeAps) {
  // Create simulated device
  Init();

  // Start the first AP
  StartFakeAp(kAp1Bssid, kAp1Ssid, kDefaultChannel1);
  StartFakeAp(kAp2Bssid, kAp2Ssid, kDefaultChannel1);
  StartFakeAp(kAp3Bssid, kAp3Ssid, kDefaultChannel2);

  // Build scan request
  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 5));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto scan_req = builder.Build();

  env_->ScheduleNotification(std::bind(&ActiveScanTest::StartScan, this, &scan_req),
                             kScanStartTime);

  // Schedule scan end in environment
  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  VerifyScanResults();
  EXPECT_EQ(all_aps_seen_, true);
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kSuccess);
}

TEST_F(ActiveScanTest, ScanTwice) {
  Init();
  // Build scan request
  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 5));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto scan_req = builder.Build();

  env_->ScheduleNotification(std::bind(&ActiveScanTest::StartScan, this, &scan_req),
                             kScanStartTime);

  env_->Run(kSimulatedClockDuration);

  VerifyScanResults();

  env_->ScheduleNotification(std::bind(&ActiveScanTest::StartScan, this, &scan_req),
                             kScanStartTime);

  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  VerifyScanResults();
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kSuccess);
}

// Ensure that the FW sends out the max # probe requests set by the
// driver (as there are no APs in the environment).
TEST_F(ActiveScanTest, CheckNumProbeReqsSent) {
  Init();

  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 1));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto scan_req = builder.Build();

  env_->ScheduleNotification(std::bind(&ActiveScanTest::StartScan, this, &scan_req),
                             kScanStartTime);

  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  EXPECT_EQ(GetNumProbeReqsSeen(), (uint32_t)BRCMF_ACTIVE_SCAN_NUM_PROBES);
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kSuccess);
}

// This test is to verify brcmfmac driver will return an error when an empty list
// of channels is indicated in active scan test request.
TEST_F(ActiveScanTest, EmptyChannelList) {
  constexpr zx::duration kScanStartTime = zx::sec(1);

  Init();

  StartFakeAp(kAp1Bssid, kAp1Ssid, kDefaultChannel1);

  // Case contains empty channels_list.
  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  // Keep the table entry but make the VectorView empty.
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 0));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto empty_channel_list_scan_req = builder.Build();

  // Two active scans are scheduled,
  env_->ScheduleNotification(
      std::bind(&ActiveScanTest::StartScan, this, &empty_channel_list_scan_req), kScanStartTime);

  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  VerifyScanResults();
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kInvalidArgs);
}

// This test is to verify brcmfmac driver will return an error when an invalid ssid list
// is indicated in active scan test request.
TEST_F(ActiveScanTest, SsidTooLong) {
  constexpr zx::duration kScanStartTime = zx::sec(1);

  Init();

  StartFakeAp(kAp1Bssid, kAp1Ssid, kDefaultChannel1);

  wlan_ieee80211::CSsid invalid_scan_ssid = {
      .len = 33,
      .data = {.data_ = "1234567890"},
  };

  wlan_ieee80211::CSsid valid_scan_ssid = {
      .len = 16,
      .data = {.data_ = "Fuchsia Fake AP1"},
  };

  const wlan_ieee80211::CSsid ssids_list[] = {valid_scan_ssid, invalid_scan_ssid};

  // Case contains over-size ssid in ssids_list in request.
  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  // Keep the table entry but make the VectorView empty.
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 5));
  builder.ssids(fidl::VectorView<wlan_ieee80211::CSsid>::FromExternal(
      const_cast<wlan_ieee80211::CSsid*>(ssids_list), 2));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto break_ssids_list_scan_req = builder.Build();

  // Two active scans are scheduled,
  env_->ScheduleNotification(
      std::bind(&ActiveScanTest::StartScan, this, &break_ssids_list_scan_req), kScanStartTime);

  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  VerifyScanResults();
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kInvalidArgs);
}

// This test case verifies that the driver returns SHOULD_WAIT as the scan result code when firmware
// is busy.
TEST_F(ActiveScanTest, ScanWhenFirmwareBusy) {
  Init();

  // Start the first AP
  StartFakeAp(kAp1Bssid, kAp1Ssid, kDefaultChannel1);

  // Set up our injector
  brcmf_simdev* sim = device_->GetSim();
  sim->sim_fw->err_inj_.AddErrInjIovar("escan", ZX_OK, BCME_BUSY);

  auto builder =
      wlan_fullmac_wire::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);
  builder.txn_id(++scan_txn_id_);
  builder.scan_type(wlan_fullmac_wire::WlanScanType::kActive);
  // Keep the table entry but make the VectorView empty.
  builder.channels(
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(default_channels_list_), 5));
  builder.min_channel_time(kDwellTimeMs);
  builder.max_channel_time(kDwellTimeMs);
  auto scan_req = builder.Build();

  env_->ScheduleNotification(std::bind(&ActiveScanTest::StartScan, this, &scan_req),
                             kScanStartTime);

  env_->ScheduleNotification(std::bind(&ActiveScanTest::EndSimulation, this),
                             kSimulatedClockDuration);
  env_->Run(kSimulatedClockDuration);

  // Verify that there is no scan result and the scan result code is WLAN_SCAN_RESULT_SHOULD_WAIT.
  EXPECT_EQ(scan_results_.size(), 0U);
  EXPECT_EQ(scan_result_code_, wlan_fullmac_wire::WlanScanResult::kShouldWait);
}

}  // namespace wlan::brcmfmac
