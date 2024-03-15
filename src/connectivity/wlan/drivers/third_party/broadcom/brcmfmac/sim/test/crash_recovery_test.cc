// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/test/device_inspect_test_utils.h"

namespace wlan::brcmfmac {

constexpr wlan_common::WlanChannel kDefaultChannel = {
    .primary = 9, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
const common::MacAddr kDefaultBssid({0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc});
constexpr wlan_ieee80211::CSsid kDefaultSsid = {.len = 15, .data = {.data_ = "Fuchsia Fake AP"}};

class CrashRecoveryTest : public SimTest {
 public:
  CrashRecoveryTest() : ap_(env_.get(), kDefaultBssid, kDefaultSsid, kDefaultChannel) {}

  static constexpr zx::duration kTestDuration = zx::sec(50);
  void Init();
  void ScheduleCrash(zx::duration delay);
  void RecreateClientIface();
  void VerifyScanResult(const uint64_t scan_id, size_t min_result_num,
                        wlan_fullmac_wire::WlanScanResult expect_code);

  // Get the value of inspect counter of firmware recovery. It is used to verify the number of
  // counted firmware recovery in driver's metrics.
  void GetFwRcvrInspectCount(uint64_t* out_count);

  simulation::FakeAp ap_;
  SimInterface client_ifc_;
  common::MacAddr client_mac_addr_;
};

void CrashRecoveryTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_), ZX_OK);
  ap_.EnableBeacon(zx::msec(100));
  client_ifc_.GetMacAddr(&client_mac_addr_);
  uint64_t count;
  GetFwRcvrInspectCount(&count);
  ASSERT_EQ(0U, count);
}

void CrashRecoveryTest::RecreateClientIface() {
  // Since the interface was destroyed as part of the recovery process, we
  // need to notify the sim about it before attempting to recreate.
  SimTest::InterfaceDestroyed(&client_ifc_);
  SimTest::StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_);
}

void CrashRecoveryTest::ScheduleCrash(zx::duration delay) {
  auto crash_firmware_callback = [this]() {
    WithSimDevice([&](brcmfmac::SimDevice* device) {
      brcmf_simdev* sim = device->GetSim();
      struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, client_ifc_.iface_id_);
      EXPECT_OK(brcmf_fil_iovar_int_set(ifp, "crash", 0, nullptr));
    });
  };
  env_->ScheduleNotification(crash_firmware_callback, delay);

  // Reset the MAC address to firmware after recovery.
  auto reset_mac_addr_callback = [this]() {
    WithSimDevice([&](brcmfmac::SimDevice* device) {
      brcmf_simdev* sim = device->GetSim();
      struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, client_ifc_.iface_id_);
      EXPECT_OK(
          brcmf_fil_iovar_data_set(ifp, "cur_etheraddr", client_mac_addr_.byte, ETH_ALEN, nullptr));
    });
  };
  env_->ScheduleNotification(reset_mac_addr_callback, delay + zx::msec(1));
}

void CrashRecoveryTest::VerifyScanResult(const uint64_t scan_id, size_t min_result_num,
                                         wlan_fullmac_wire::WlanScanResult expect_code) {
  EXPECT_GE(client_ifc_.ScanResultList(scan_id)->size(), min_result_num);

  wlan_fullmac_wire::WlanFullmacScanResult back_scan_result =
      client_ifc_.ScanResultList(scan_id)->back();
  auto ssid =
      brcmf_find_ssid_in_ies(back_scan_result.bss.ies.data(), back_scan_result.bss.ies.count());
  common::MacAddr bssid(back_scan_result.bss.bssid.data());

  EXPECT_EQ(bssid, kDefaultBssid);
  EXPECT_EQ(ssid.size(), kDefaultSsid.len);
  EXPECT_EQ(std::memcmp(ssid.data(), kDefaultSsid.data.data(), kDefaultSsid.len), 0);

  ASSERT_NE(client_ifc_.ScanResultCode(scan_id), std::nullopt);
  EXPECT_EQ(client_ifc_.ScanResultCode(scan_id).value(), expect_code);
}

void CrashRecoveryTest::GetFwRcvrInspectCount(uint64_t* out_count) {
  ASSERT_NOT_NULL(out_count);

  fpromise::result<inspect::Hierarchy> hierarchy;
  WithSimDevice([&](brcmfmac::SimDevice* device) {
    hierarchy = FetchHierarchy(device->GetInspect()->inspector());
  });

  auto* root = hierarchy.value().GetByPath({"brcmfmac-phy"});
  ASSERT_NOT_NULL(root);
  // Only verify the value of hourly counter here, the relationship between hourly counter and daily
  // counter is verified in device_inspect_test.
  auto* uint_property = root->node().get_property<inspect::UintPropertyValue>("fw_recovered");
  ASSERT_NOT_NULL(uint_property);
  *out_count = uint_property->value();
}

TEST_F(CrashRecoveryTest, DeviceDestroyOnCrash) {
  Init();
  uint32_t dev_count = DeviceCount();

  ScheduleCrash(zx::msec(10));
  env_->Run(kTestDuration);

  // Since we currently have one client interface, that should have gotten destroyed.
  WaitForDeviceCount(dev_count - 1);

  // Ensure RecreateClientIface brings it back.
  RecreateClientIface();
  WaitForDeviceCount(dev_count);
}

// Verify that an association can be done correctly after a crash and a recovery happen after a scan
// is started.
TEST_F(CrashRecoveryTest, ConnectAfterCrashDuringScan) {
  constexpr uint64_t kScanId = 0x18c5f;

  Init();
  env_->ScheduleNotification(std::bind(&SimInterface::StartScan, &client_ifc_, kScanId, false,
                                       std::optional<const std::vector<uint8_t>>{}),
                             zx::msec(10));
  // Crash before the first scan result is sent up.
  ScheduleCrash(zx::msec(15));
  env_->ScheduleNotification(std::bind(&CrashRecoveryTest::RecreateClientIface, this),
                             zx::msec(18));
  client_ifc_.AssociateWith(ap_, zx::msec(20));

  env_->Run(kTestDuration);

  // Verify no scan result is received from SME
  EXPECT_EQ(client_ifc_.ScanResultList(kScanId)->size(), 0U);

  // Verify that the association succeeded.
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 1U);

  // Verify inspect is updated.
  uint64_t count;
  GetFwRcvrInspectCount(&count);
  EXPECT_EQ(1U, count);
}

// Verify that an association can be done correctly after firmware crashes while driver is already
// in associated state, we don't care about the association state machine in SME in this test.
TEST_F(CrashRecoveryTest, ConnectAfterCrashAfterConnect) {
  Init();

  client_ifc_.AssociateWith(ap_, zx::msec(10));
  ScheduleCrash(zx::msec(20));
  env_->ScheduleNotification(std::bind(&CrashRecoveryTest::RecreateClientIface, this),
                             zx::msec(30));
  client_ifc_.AssociateWith(ap_, zx::msec(40));

  env_->Run(kTestDuration);

  // Verify that both association succeeded.
  EXPECT_EQ(client_ifc_.stats_.connect_attempts, 2U);
  EXPECT_EQ(client_ifc_.stats_.connect_successes, 2U);

  // Verify inspect is updated.
  uint64_t count;
  GetFwRcvrInspectCount(&count);
  EXPECT_EQ(1U, count);
}

// Verify that a scan can be done correctly after a crash recovery happens when client is connected
// to an AP.
TEST_F(CrashRecoveryTest, ScanAfterCrashAfterConnect) {
  constexpr uint64_t kScanId = 0x18c5f;
  // Firmware will receive 2 beacons while scanning the 9th channel with 120ms dwell time.
  const size_t kExpectMinScanResultNumber = 1;

  Init();

  client_ifc_.AssociateWith(ap_, zx::msec(10));
  ScheduleCrash(zx::msec(20));
  env_->ScheduleNotification(std::bind(&CrashRecoveryTest::RecreateClientIface, this),
                             zx::msec(30));
  env_->ScheduleNotification(std::bind(&SimInterface::StartScan, &client_ifc_, kScanId, false,
                                       std::optional<const std::vector<uint8_t>>{}),
                             zx::msec(40));

  env_->Run(kTestDuration);

  VerifyScanResult(kScanId, kExpectMinScanResultNumber,
                   wlan_fullmac_wire::WlanScanResult::kSuccess);

  // Verify inspect is updated.
  uint64_t count;
  GetFwRcvrInspectCount(&count);
  EXPECT_EQ(1U, count);
}

}  // namespace wlan::brcmfmac
