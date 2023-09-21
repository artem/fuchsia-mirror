// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/scanner.h"

#include <lib/sync/completion.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mlan_mocks.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mock_bus.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/utils.h"

namespace {

using wlan::nxpfmac::Scanner;

struct ScannerTest : public zxtest::Test {
  void SetUp() override {
    auto ioctl_adapter = wlan::nxpfmac::IoctlAdapter::Create(mocks_.GetAdapter(), &mock_bus_);
    ASSERT_OK(ioctl_adapter.status_value());
    ioctl_adapter_ = std::move(ioctl_adapter.value());
    context_.ioctl_adapter_ = ioctl_adapter_.get();
    context_.event_handler_ = &event_handler_;
  }

  wlan::nxpfmac::DeviceContext context_;
  wlan::nxpfmac::MlanMockAdapter mocks_;
  wlan::nxpfmac::EventHandler event_handler_;
  wlan::nxpfmac::MockBus mock_bus_;
  std::unique_ptr<wlan::nxpfmac::IoctlAdapter> ioctl_adapter_;
};

// Make sure this timeout is long enough to avoid flaky tests for all tests that are not expected to
// time out.
constexpr zx_duration_t kDefaultTimeout = ZX_MSEC(60000000000);

TEST_F(ScannerTest, Constructible) {
  // Test that a Scanner object can be constructed without crashing.

  wlan::nxpfmac::EventHandler event_handler;
  ASSERT_NO_FATAL_FAILURE(Scanner(&context_, 0));
}

TEST_F(ScannerTest, Scan) {
  // Test a basic scan and ensure that the correct scan results are passed on to the callback.

  constexpr uint32_t kBssIndex = 0;
  constexpr uint64_t kTxnId = 0x234776898adf83;

  constexpr uint8_t kChannels[] = {1,   2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  36,
                                   40,  44,  48,  52,  56,  60,  64,  100, 104, 108, 112, 116,
                                   120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165};

  // Our scan result
  BSSDescriptor_t scan_table[] = {{
      .mac_address = {0x01, 0x02, 0x03, 0x04, 0x5, 0x06},
      .rssi = -40,
      .channel = 11,
      .beacon_period = 100,
      .curr_bandwidth = 6,
  }};

  bool on_ioctl_set_called = false;
  bool on_ioctl_get_called = false;
  auto on_ioctl = [&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    auto request = reinterpret_cast<wlan::nxpfmac::IoctlRequest<mlan_ds_scan>*>(req);
    if (req->action == MLAN_ACT_SET) {
      if (request->UserReq().sub_command == MLAN_OID_SCAN_CANCEL) {
        // Cancel is a set command and should always succeed.
        return MLAN_STATUS_SUCCESS;
      }

      on_ioctl_set_called = true;

      EXPECT_EQ(MLAN_IOCTL_SCAN, req->req_id);
      EXPECT_EQ(MLAN_OID_SCAN_USER_CONFIG, request->UserReq().sub_command);

      auto user_scan =
          reinterpret_cast<wlan_user_scan_cfg*>(request->UserReq().param.user_scan.scan_cfg_buf);

      for (size_t i = 0; i < std::size(user_scan->chan_list); ++i) {
        if (i < std::size(kChannels)) {
          EXPECT_EQ(kChannels[i], user_scan->chan_list[i].chan_number);
          if (wlan::nxpfmac::is_dfs_channel(kChannels[i])) {
            EXPECT_EQ(MLAN_SCAN_TYPE_PASSIVE_TO_ACTIVE, user_scan->chan_list[i].scan_type);
          } else {
            EXPECT_EQ(MLAN_SCAN_TYPE_ACTIVE, user_scan->chan_list[i].scan_type);
          }
          EXPECT_EQ(wlan::nxpfmac::band_from_channel(kChannels[i]),
                    user_scan->chan_list[i].radio_type);
        } else {
          // Remaining channels should be zero.
          EXPECT_EQ(0, user_scan->chan_list[i].chan_number);
        }
      }

      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
      return MLAN_STATUS_PENDING;
    }
    if (req->req_id == MLAN_IOCTL_SCAN && req->action == MLAN_ACT_GET) {
      // Get scan results
      request->UserReq().param.scan_resp.num_in_scan_table = std::size(scan_table);
      request->UserReq().param.scan_resp.pscan_table = reinterpret_cast<uint8_t*>(&scan_table);

      on_ioctl_get_called = true;
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_BSS && req->action == MLAN_ACT_GET) {
      // Get the list of supported channels to scan. Provide a bunch of channels.
      auto request = reinterpret_cast<wlan::nxpfmac::IoctlRequest<mlan_ds_bss>*>(req);
      EXPECT_EQ(MLAN_OID_BSS_CHANNEL_LIST, request->UserReq().sub_command);
      for (size_t i = 0; i < std::size(kChannels); ++i) {
        request->UserReq().param.chanlist.cf[i].channel = kChannels[i];
      }
      request->UserReq().param.chanlist.num_of_chan = std::size(kChannels);
      return MLAN_STATUS_SUCCESS;
    }
    ADD_FAILURE("Unexpected ioctl");
    return MLAN_STATUS_FAILURE;
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  sync_completion_t scan_result_completion;
  sync_completion_t scan_end_completion;

  std::atomic<int> scan_results_seen = 0;
  auto on_scan_result = [&](const wlan_fullmac_wire::WlanFullmacScanResult& result) {
    ++scan_results_seen;
    EXPECT_BYTES_EQ(result.bss.bssid.data(), scan_table[0].mac_address,
                    sizeof(scan_table[0].mac_address));
    sync_completion_signal(&scan_result_completion);
  };

  auto on_scan_end = [&](uint64_t txn_id, wlan_fullmac_wire::WlanScanResult result) {
    EXPECT_EQ(kTxnId, txn_id);
    EXPECT_EQ(wlan_fullmac_wire::WlanScanResult::kSuccess, result);
    sync_completion_signal(&scan_end_completion);
  };

  Scanner scanner(&context_, kBssIndex);

  // Perform an active scan
  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
  builder.txn_id(kTxnId);
  builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);
  builder.min_channel_time(0);
  auto scan_request = builder.Build();
  ASSERT_OK(scanner.Scan(&scan_request, kDefaultTimeout, on_scan_result, on_scan_end));

  // The Set ioctl should've been called immediately, that's what starts the scan
  ASSERT_TRUE(on_ioctl_set_called);

  // Send an event indicating that there is a scan report. This triggers the retrieval of scan
  // results.
  mlan_event event{.event_id = MLAN_EVENT_ID_DRV_SCAN_REPORT};
  event_handler_.OnEvent(&event);

  // Verify that scan results were recieved and that scan end was called.
  ASSERT_OK(sync_completion_wait(&scan_result_completion, ZX_TIME_INFINITE));
  ASSERT_OK(sync_completion_wait(&scan_end_completion, ZX_TIME_INFINITE));

  // The Get ioctl should be called as part of getting the scan results.
  ASSERT_TRUE(on_ioctl_get_called);

  ASSERT_EQ(std::size(scan_table), scan_results_seen.load());

  // At this point there should be no scan to stop.
  ASSERT_EQ(ZX_ERR_NOT_FOUND, scanner.StopScan());
}

TEST_F(ScannerTest, StopScan) {
  // Test that StopScan works on a pending scan, test that StopScan returns an error if no scan in
  // progress and test that we can scan after stopping a scan.

  constexpr uint32_t kBssIndex = 0;
  constexpr uint64_t kTxnId = 0x234776898adf83;

  sync_completion_t called_with_cancel;
  std::vector<mlan_ioctl_req*> scan_requests;
  auto on_ioctl = [&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->req_id == MLAN_IOCTL_BSS && req->action == MLAN_ACT_GET) {
      // Retrieve the list of channels to use, should return success.
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_SCAN && req->action == MLAN_ACT_SET) {
      auto request = reinterpret_cast<wlan::nxpfmac::IoctlRequest<mlan_ds_scan>*>(req);
      if (request->UserReq().sub_command == MLAN_OID_SCAN_CANCEL) {
        // Emulate the behavior of the cancel scan command by canceling all pending scans.
        for (auto scan_request : scan_requests) {
          scan_request->status_code = MLAN_ERROR_CMD_CANCEL;
          // The cancelation also needs to call ioctl complete with a failure status.
          ioctl_adapter_->OnIoctlComplete(scan_request, wlan::nxpfmac::IoctlStatus::Failure);
        }
        scan_requests.clear();
        sync_completion_signal(&called_with_cancel);
        return MLAN_STATUS_SUCCESS;
      }
      // If it's not a cancel assume it's the scan. Don't call the callback here, let the scan
      // languish so we can stop it.
      scan_requests.push_back(req);
      return MLAN_STATUS_PENDING;
    }
    if (req->req_id == MLAN_IOCTL_SCAN && req->action == MLAN_ACT_CANCEL) {
      ZX_PANIC("Where is this cancel coming from?");
    }
    ADD_FAILURE("Unexpected ioctl 0x%08x", req->req_id);
    return MLAN_STATUS_FAILURE;
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  sync_completion_t scan_ended;
  auto on_scan_end = [&](uint64_t txn_id, wlan_fullmac_wire::WlanScanResult result) {
    EXPECT_EQ(kTxnId, txn_id);
    EXPECT_EQ(wlan_fullmac_wire::WlanScanResult::kCanceledByDriverOrFirmware, result);
    sync_completion_signal(&scan_ended);
  };

  {
    Scanner scanner(&context_, kBssIndex);

    // Perform an active scan
    fidl::Arena arena;
    auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
    builder.txn_id(kTxnId);
    builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);
    builder.min_channel_time(0);
    auto scan_request = builder.Build();

    ASSERT_OK(scanner.Scan(&scan_request, kDefaultTimeout, nullptr, on_scan_end));

    // Verify that we can stop a scan in progress
    ASSERT_OK(scanner.StopScan());

    // Wait for the ioctl to be called with the cancel.
    ASSERT_OK(sync_completion_wait(&called_with_cancel, ZX_TIME_INFINITE));

    // Wait for the scan end to be signaled
    ASSERT_OK(sync_completion_wait(&scan_ended, ZX_TIME_INFINITE));

    // Verify that if there is no scan in progress StopScan should return an error.
    ASSERT_EQ(ZX_ERR_NOT_FOUND, scanner.StopScan());

    sync_completion_reset(&scan_ended);
    // The second scan should be canceled when the scanner is destroyed
    auto on_second_scan_end = [&](uint64_t txn_id, wlan_fullmac_wire::WlanScanResult result) {
      EXPECT_EQ(kTxnId, txn_id);
      EXPECT_EQ(wlan_fullmac_wire::WlanScanResult::kCanceledByDriverOrFirmware, result);
      sync_completion_signal(&scan_ended);
    };

    // Verify that we can scan again after stopping.
    ASSERT_OK(scanner.Scan(&scan_request, kDefaultTimeout, nullptr, on_second_scan_end));
  }
  // Wait for the second scan end to be signaled.
  ASSERT_OK(sync_completion_wait(&scan_ended, ZX_TIME_INFINITE));
}

TEST_F(ScannerTest, StopScanWithNoScanInProgress) {
  // Test that StopScan returns an error when no scan is in progress.

  constexpr uint32_t kBssIndex = 0;
  Scanner scanner(&context_, kBssIndex);

  ASSERT_EQ(ZX_ERR_NOT_FOUND, scanner.StopScan());
}

TEST_F(ScannerTest, ScanSpecificSsids) {
  // Test scanning of a specific SSID

  constexpr uint32_t kBssIndex = 0;
  constexpr uint64_t kTxnId = 42;

  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
  builder.txn_id(kTxnId);
  builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);
  constexpr fuchsia_wlan_ieee80211::wire::CSsid kSsids[] = {{.len = 4, .data{"foo"}},
                                                            {.len = 8, .data{"another"}}};

  builder.ssids(fidl::VectorView<fuchsia_wlan_ieee80211::wire::CSsid>::FromExternal(
      const_cast<fuchsia_wlan_ieee80211::wire::CSsid*>(kSsids),
      sizeof(kSsids) / sizeof(kSsids[0])));
  builder.min_channel_time(0);

  auto scan_request = builder.Build();

  auto on_ioctl = [&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_GET) {
      // Don't do anything with the request to get scan results or channel list.
      return MLAN_STATUS_SUCCESS;
    }
    EXPECT_EQ(MLAN_ACT_SET, req->action);
    EXPECT_EQ(MLAN_IOCTL_SCAN, req->req_id);
    auto scan = reinterpret_cast<mlan_ds_scan*>(req->pbuf);
    EXPECT_EQ(MLAN_OID_SCAN_USER_CONFIG, scan->sub_command);
    auto scan_cfg = reinterpret_cast<wlan_user_scan_cfg*>(scan->param.user_scan.scan_cfg_buf);

    // Check that the requested SSIDs are part of the request.
    EXPECT_BYTES_EQ(kSsids[0].data.data(), scan_cfg->ssid_list[0].ssid, kSsids[0].len);
    EXPECT_BYTES_EQ(kSsids[1].data.data(), scan_cfg->ssid_list[1].ssid, kSsids[1].len);
    // The next SSID should be empty.
    EXPECT_EQ('\0', scan_cfg->ssid_list[2].ssid[0]);
    ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
    return MLAN_STATUS_PENDING;
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  sync_completion_t completion;
  auto on_scan_end = [&](uint64_t txn_id, wlan_fullmac_wire::WlanScanResult result) {
    EXPECT_EQ(wlan_fullmac_wire::WlanScanResult::kSuccess, result);
    EXPECT_EQ(kTxnId, txn_id);
    sync_completion_signal(&completion);
  };

  Scanner scanner(&context_, kBssIndex);

  ASSERT_OK(scanner.Scan(&scan_request, kDefaultTimeout, nullptr, on_scan_end));

  // End the scan by sending a scan report event.
  mlan_event event{.event_id = MLAN_EVENT_ID_DRV_SCAN_REPORT};
  event_handler_.OnEvent(&event);

  ASSERT_OK(sync_completion_wait(&completion, ZX_TIME_INFINITE));
}

TEST_F(ScannerTest, ScanTooManySsids) {
  // Test that scanning more than the supported number of SSIDs in one request will fail.

  constexpr uint32_t kBssIndex = 0;
  constexpr uint64_t kTxnId = 42;
  constexpr size_t kTooManySsids = 11;

  // Just use an empty list, the important thing is the number of SSIDs.
  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
  builder.txn_id(kTxnId);
  builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);
  constexpr fuchsia_wlan_ieee80211::wire::CSsid kSsids[kTooManySsids] = {};

  builder.ssids(fidl::VectorView<fuchsia_wlan_ieee80211::wire::CSsid>::FromExternal(
      const_cast<fuchsia_wlan_ieee80211::wire::CSsid*>(kSsids), kTooManySsids));
  builder.min_channel_time(0);

  auto scan_request = builder.Build();
  ASSERT_GT(scan_request.ssids().count(), 1u);

  Scanner scanner(&context_, kBssIndex);

  // This should immediately fail with an invalid args error.
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, scanner.Scan(&scan_request, kDefaultTimeout, nullptr, nullptr));
}

TEST_F(ScannerTest, ScanTimeout) {
  // Test that a scan times out properly.

  constexpr uint32_t kBssIndex = 0;
  constexpr uint64_t kTxnId = 42;
  constexpr zx_duration_t kShortTimeout = ZX_MSEC(5);

  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(arena);
  builder.txn_id(kTxnId);
  builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);
  builder.min_channel_time(0);
  auto scan_request = builder.Build();

  std::atomic<int> on_ioctl_calls = 0;
  std::atomic<bool> get_scan_results_called = false;
  auto on_ioctl = [&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    ++on_ioctl_calls;
    if (req->action == MLAN_ACT_CANCEL) {
      // The timeout will trigger a cancelation, we must emulate mlan's behavior of calling the
      // ioctl complete with a specific status code and result in this case.
      req->status_code = MLAN_ERROR_CMD_CANCEL;
      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Failure);
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_SCAN && req->action == MLAN_ACT_GET) {
      // The scanner will attempt to get any partial scan results available. This operation should
      // immediately return success.
      get_scan_results_called = true;
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_BSS && req->action == MLAN_ACT_GET) {
      // Get the list of channels to scan.
      return MLAN_STATUS_SUCCESS;
    }
    // Leave the scan as pending, allowing it to time out.
    return MLAN_STATUS_PENDING;
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  sync_completion_t completion;
  auto on_scan_end = [&](uint64_t txn_id, wlan_fullmac_wire::WlanScanResult result) {
    EXPECT_EQ(wlan_fullmac_wire::WlanScanResult::kCanceledByDriverOrFirmware, result);
    EXPECT_EQ(kTxnId, txn_id);
    sync_completion_signal(&completion);
  };

  Scanner scanner(&context_, kBssIndex);

  ASSERT_OK(scanner.Scan(&scan_request, kShortTimeout, nullptr, on_scan_end));

  ASSERT_OK(sync_completion_wait(&completion, ZX_TIME_INFINITE));
  // There should only have been four ioctl calls. One to get the channel list, one to start the
  // scan, one to cancel it, and one to fetch scan results.
  ASSERT_EQ(4u, on_ioctl_calls.load());
  ASSERT_TRUE(get_scan_results_called.load());
}

}  // namespace
