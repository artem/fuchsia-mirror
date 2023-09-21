// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/calls.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

BRCMF_DECLARE_OVERRIDE(getentropy, 1);

namespace wlan::brcmfmac {
namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

// Verify that an active scan succeeds even if we aren't able to generate a random MAC address
TEST_F(SimTest, ActiveScan) {
  constexpr uint64_t kScanId = 0x18c5f;

  // This won't cause getentropy() to be called, but at least it won't fail, either. That's good
  // enough to get us through initialization successfully.
  BRCMF_SET_VALUE(getentropy, 0);

  ASSERT_EQ(Init(), ZX_OK);

  SimInterface client_ifc;
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc), ZX_OK);

  BRCMF_SET_VALUE(getentropy, 1);

  env_->ScheduleNotification(std::bind(&SimInterface::StartScan, &client_ifc, kScanId, true,
                                       std::optional<const std::vector<uint8_t>>{}),
                             zx::sec(1));
  env_->Run(kSimulatedClockDuration);

  // Verify that scan completed successfully
  auto scan_result = client_ifc.ScanResultCode(kScanId);
  ASSERT_TRUE(scan_result);
  EXPECT_EQ(*scan_result, wlan_fullmac_wire::WlanScanResult::kSuccess);
}

}  // namespace wlan::brcmfmac
