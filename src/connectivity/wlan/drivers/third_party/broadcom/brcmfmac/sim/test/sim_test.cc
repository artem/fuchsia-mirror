// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

#include <fidl/fuchsia.wlan.common/cpp/wire_types.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/markers.h>
#include <fidl/fuchsia.wlan.phyimpl/cpp/markers.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>

#include <bind/fuchsia/wlan/fullmac/cpp/bind.h>
#include <fbl/string_buffer.h>

#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"

namespace wlan::brcmfmac {

// static
const std::vector<uint8_t> SimInterface::kDefaultScanChannels = {
    1,  2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  32,  36,  40,  44,  48,  52,  56, 60,
    64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165};

SimInterface::SimInterface() : test_arena_(fdf::Arena('IFAC')) {}

SimInterface::~SimInterface() {
  // If the client is valid, it means that this SimInterface has been connected to a real
  // WlanInterface object, otherwise skip the stop.
  if (client_.is_valid()) {
    auto result = client_.buffer(test_arena_)->Stop();
    ZX_ASSERT(result.ok());
  }
  if (ch_sme_ != ZX_HANDLE_INVALID) {
    zx_handle_close(ch_sme_);
  }
  if (ch_mlme_ != ZX_HANDLE_INVALID) {
    zx_handle_close(ch_mlme_);
  }

  if (server_binding_ != nullptr) {
    Reset();
  }
}

zx_status_t SimInterface::Init(simulation::Environment* env, wlan_common::WlanMacRole role) {
  zx_status_t result = zx_channel_create(0, &ch_sme_, &ch_mlme_);
  if (result == ZX_OK) {
    env_ = env;
    role_ = role;
  }
  return result;
}

void SimInterface::Reset() {
  libsync::Completion destroy_binding_completion;
  async::PostTask(fdf_dispatcher_get_async_dispatcher(server_dispatcher_), [&]() {
    server_binding_.reset();
    destroy_binding_completion.Signal();
  });

  destroy_binding_completion.Wait();
  if (client_.is_valid()) {
    client_.TakeClientEnd();
  }
}

zx_status_t SimInterface::Connect(fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> client_end,
                                  fdf_dispatcher_t* server_dispatcher) {
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl> client(std::move(client_end));

  // Establish the FIDL connection on the oppsite direction.
  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
  if (endpoints.is_error()) {
    BRCMF_ERR("Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  // Synchronously bind the server to the given dispatcher.
  server_dispatcher_ = server_dispatcher;
  libsync::Completion create_binding_completion;
  async::PostTask(
      fdf_dispatcher_get_async_dispatcher(server_dispatcher),
      [&, server_end = std::move(endpoints->server)]() mutable {
        server_binding_ =
            std::make_unique<fdf::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImplIfc>>(
                server_dispatcher_, std::move(server_end), this, fidl::kIgnoreBindingClosure);
        create_binding_completion.Signal();
      });

  create_binding_completion.Wait();

  auto result = client.buffer(test_arena_)->Start(std::move(endpoints->client));
  if (!result.ok()) {
    BRCMF_ERR("Failed to start wlanfullmac interface: %s", result.FormatDescription().c_str());
    return result.status();
  }

  if (result->is_error()) {
    BRCMF_ERR("Start failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Only assign the client if Start succeeded, otherwise client_ is assigned but not working.
  client_ = std::move(client);

  // Verify that the channel passed back from start() is the same one we gave to create_iface()
  if (result->value()->sme_channel.get() != ch_mlme_) {
    BRCMF_ERR("Channels don't match, sme_channel: %zu, ch_mlme_: %zu",
              result->value()->sme_channel.get(), ch_mlme_);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void SimInterface::OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                                OnScanResultCompleter::Sync& completer) {
  auto& copy = request->result;

  auto results = scan_results_.find(copy.txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  // Verify that the scan hasn't sent a completion notice
  ZX_ASSERT(!results->second.result_code);

  // Copy the IES data over since the original location may change data by the time we verify.
  std::vector<uint8_t> ies(copy.bss.ies.data(), copy.bss.ies.data() + copy.bss.ies.count());
  scan_results_ies_.push_back(ies);
  copy.bss.ies = fidl::VectorView<uint8_t>::FromExternal(*(scan_results_ies_.rbegin()));
  results->second.result_list.push_back(copy);
  completer.buffer(arena).Reply();
}

void SimInterface::OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                             OnScanEndCompleter::Sync& completer) {
  auto& end = request->end;
  auto results = scan_results_.find(end.txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  // Verify that the scan hasn't already received a completion notice
  ZX_ASSERT(!results->second.result_code);

  results->second.result_code = end.code;
  completer.buffer(arena).Reply();
}

void SimInterface::ConnectConf(ConnectConfRequestView request, fdf::Arena& arena,
                               ConnectConfCompleter::Sync& completer) {
  ZX_ASSERT(assoc_ctx_.state == AssocContext::kAssociating);
  auto& resp = request->resp;
  stats_.connect_results.push_back(resp);

  if (resp.result_code == wlan_ieee80211::StatusCode::kSuccess) {
    assoc_ctx_.state = AssocContext::kAssociated;
    stats_.connect_successes++;
  } else {
    assoc_ctx_.state = AssocContext::kNone;
  }
  completer.buffer(arena).Reply();
}

void SimInterface::RoamConf(RoamConfRequestView request, fdf::Arena& arena,
                            RoamConfCompleter::Sync& completer) {
  auto& resp = request->resp;
  ZX_ASSERT(assoc_ctx_.state == AssocContext::kAssociated);

  if (resp.result_code == wlan_ieee80211::StatusCode::kSuccess) {
    std::memcpy(assoc_ctx_.bssid.byte, resp.target_bssid.data(), ETH_ALEN);
    stats_.connect_successes++;
  } else {
    assoc_ctx_.state = AssocContext::kNone;
  }
  completer.buffer(arena).Reply();
}

void SimInterface::AuthInd(AuthIndRequestView request, fdf::Arena& arena,
                           AuthIndCompleter::Sync& completer) {
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);
  stats_.auth_indications.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                              DeauthConfCompleter::Sync& completer) {
  auto builder = wlan_fullmac_wire::WlanFullmacImplIfcBaseDeauthConfRequest::Builder(test_arena_);
  if (request->has_peer_sta_address()) {
    builder.peer_sta_address(request->peer_sta_address());
  }
  stats_.deauth_results.emplace_back(builder.Build());
  completer.buffer(arena).Reply();
}

void SimInterface::DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                             DeauthIndCompleter::Sync& completer) {
  stats_.deauth_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                            AssocIndCompleter::Sync& completer) {
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);
  stats_.assoc_indications.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                                DisassocConfCompleter::Sync& completer) {
  const auto disassoc_conf = wlan_fullmac_wire::WlanFullmacImplIfcBaseDisassocConfRequest{
      .resp = {.status = request->resp.status}};
  stats_.disassoc_results.emplace_back(disassoc_conf);
  completer.buffer(arena).Reply();
}

void SimInterface::DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                               DisassocIndCompleter::Sync& completer) {
  stats_.disassoc_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::StartConf(StartConfRequestView request, fdf::Arena& arena,
                             StartConfCompleter::Sync& completer) {
  stats_.start_confirmations.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::StopConf(StopConfRequestView request, fdf::Arena& arena,
                            StopConfCompleter::Sync& completer) {
  stats_.stop_confirmations.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::EapolConf(EapolConfRequestView request, fdf::Arena& arena,
                             EapolConfCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                                   OnChannelSwitchCompleter::Sync& completer) {
  stats_.csa_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::SignalReport(SignalReportRequestView request, fdf::Arena& arena,
                                SignalReportCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::EapolInd(EapolIndRequestView request, fdf::Arena& arena,
                            EapolIndCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                                  OnPmkAvailableCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                                   SaeHandshakeIndCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                              SaeFrameRxCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                                   OnWmmStatusRespCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::StopInterface() {
  auto result = client_.buffer(test_arena_)->Stop();
  if (!result.ok()) {
    BRCMF_ERR("Stop failed, FIDL error: %s", result.status_string());
  }
}

void SimInterface::Query(wlan_fullmac_wire::WlanFullmacQueryInfo* out_info) {
  auto result = client_.buffer(test_arena_)->Query();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_info = result->value()->info;
}

void SimInterface::QueryMacSublayerSupport(wlan_common::MacSublayerSupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QueryMacSublayerSupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::QuerySecuritySupport(wlan_common::SecuritySupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QuerySecuritySupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::QuerySpectrumManagementSupport(
    wlan_common::SpectrumManagementSupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QuerySpectrumManagementSupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::GetMacAddr(common::MacAddr* out_macaddr) {
  wlan_fullmac_wire::WlanFullmacQueryInfo info;
  Query(&info);
  memcpy(out_macaddr->byte, info.sta_addr.data(), ETH_ALEN);
}

void SimInterface::StartConnect(const common::MacAddr& bssid, const wlan_ieee80211::CSsid& ssid,
                                const wlan_common::WlanChannel& channel) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  stats_.connect_attempts++;

  // Save off context
  assoc_ctx_.state = AssocContext::kAssociating;
  assoc_ctx_.bssid = bssid;

  assoc_ctx_.ies.clear();
  assoc_ctx_.ies.push_back(0);         // SSID IE type ID
  assoc_ctx_.ies.push_back(ssid.len);  // SSID IE length
  assoc_ctx_.ies.insert(assoc_ctx_.ies.end(), ssid.data.data(), ssid.data.data() + ssid.len);
  assoc_ctx_.channel = channel;

  // Send connect request
  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseConnectRequest::Builder(test_arena_);
  fuchsia_wlan_internal::wire::BssDescription bss;
  memcpy(bss.bssid.data(), bssid.byte, ETH_ALEN);
  auto ies =
      std::vector<uint8_t>(assoc_ctx_.ies.data(), assoc_ctx_.ies.data() + assoc_ctx_.ies.size());
  bss.ies = fidl::VectorView(test_arena_, ies);
  bss.channel = channel;
  bss.bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
  builder.selected_bss(bss);
  builder.auth_type(wlan_fullmac_wire::WlanAuthType::kOpenSystem);
  builder.connect_failure_timeout(1000);  // ~1s (although value is ignored for now)

  auto result = client_.buffer(test_arena_)->Connect(builder.Build());
  ZX_ASSERT(result.ok());
}

void SimInterface::AssociateWith(const simulation::FakeAp& ap, std::optional<zx::duration> delay) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  common::MacAddr bssid = ap.GetBssid();
  wlan_ieee80211::CSsid ssid = ap.GetSsid();
  wlan_common::WlanChannel channel = ap.GetChannel();

  if (delay) {
    env_->ScheduleNotification(std::bind(&SimInterface::StartConnect, this, bssid, ssid, channel),
                               *delay);
  } else {
    StartConnect(ap.GetBssid(), ap.GetSsid(), ap.GetChannel());
  }
}

void SimInterface::DisassociateFrom(const common::MacAddr& bssid,
                                    wlan_ieee80211::ReasonCode reason) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseDisassocRequest::Builder(test_arena_);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), bssid.byte, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(reason);

  auto result = client_.buffer(test_arena_)->Disassoc(builder.Build());
  ZX_ASSERT(result.ok());
}

void SimInterface::DeauthenticateFrom(const common::MacAddr& bssid,
                                      wlan_ieee80211::ReasonCode reason) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseDeauthRequest::Builder(test_arena_);
  ::fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
  std::memcpy(peer_sta_address.data(), bssid.byte, ETH_ALEN);
  builder.peer_sta_address(peer_sta_address);
  builder.reason_code(reason);

  auto result = client_.buffer(test_arena_)->Deauth(builder.Build());
  ZX_ASSERT(result.ok());
}

void SimInterface::StartScan(uint64_t txn_id, bool active,
                             std::optional<const std::vector<uint8_t>> channels_arg) {
  wlan_fullmac_wire::WlanScanType scan_type =
      active ? wlan_fullmac_wire::WlanScanType::kActive : wlan_fullmac_wire::WlanScanType::kPassive;
  uint32_t dwell_time = active ? kDefaultActiveScanDwellTimeMs : kDefaultPassiveScanDwellTimeMs;
  const std::vector<uint8_t> channels =
      channels_arg.has_value() ? channels_arg.value() : kDefaultScanChannels;

  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseStartScanRequest::Builder(test_arena_);

  builder.txn_id(txn_id);
  builder.scan_type(scan_type);
  builder.channels(fidl::VectorView(test_arena_, channels));
  builder.min_channel_time(dwell_time);
  builder.max_channel_time(dwell_time);

  // Create an entry for tracking results
  ScanStatus scan_status;
  scan_results_.insert_or_assign(txn_id, scan_status);

  // Start the scan
  auto result = client_.buffer(test_arena_)->StartScan(builder.Build());
  ZX_ASSERT(result.ok());
}

std::optional<wlan_fullmac_wire::WlanScanResult> SimInterface::ScanResultCode(uint64_t txn_id) {
  auto results = scan_results_.find(txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  return results->second.result_code;
}

const std::list<wlan_fullmac_wire::WlanFullmacScanResult>* SimInterface::ScanResultList(
    uint64_t txn_id) {
  auto results = scan_results_.find(txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  return &results->second.result_list;
}

void SimInterface::StartSoftAp(const wlan_ieee80211::CSsid& ssid,
                               const wlan_common::WlanChannel& channel, uint32_t beacon_period,
                               uint32_t dtim_period) {
  // This should only be performed on an AP interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);

  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseStartBssRequest::Builder(test_arena_)
                     .bss_type(fuchsia_wlan_common_wire::BssType::kInfrastructure)
                     .beacon_period(beacon_period)
                     .dtim_period(dtim_period)
                     .channel(channel.primary)
                     .ssid(ssid);

  // Send request to driver
  auto result = client_.buffer(test_arena_)->StartBss(builder.Build());
  ZX_ASSERT(result.ok());

  // // Remember context
  soft_ap_ctx_.ssid = ssid;

  // Return value is handled asynchronously in OnStartConf
}

void SimInterface::StopSoftAp() {
  // This should only be performed on an AP interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);

  auto builder = wlan_fullmac_wire::WlanFullmacImplBaseStopBssRequest::Builder(test_arena_);
  // Use the ssid from the last call to StartSoftAp
  builder.ssid(soft_ap_ctx_.ssid);

  ZX_ASSERT(soft_ap_ctx_.ssid.data.size() == wlan_ieee80211::kMaxSsidByteLen);

  // Send request to driver
  auto result = client_.buffer(test_arena_)->StopBss(builder.Build());
  ZX_ASSERT(result.ok());
}

zx_status_t SimInterface::SetMulticastPromisc(bool enable) {
  auto result = client_.buffer(test_arena_)->SetMulticastPromisc(enable);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

SimTest::SimTest() : test_arena_(fdf::Arena('T')) {
  env_ = std::make_unique<simulation::Environment>();
  env_->AddStation(this);
}

SimTest::~SimTest() {
  // Clean the ifaces created in test but not deleted.
  for (auto iface : ifaces_) {
    auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_);
    builder.iface_id(iface.first);
    auto result = client_.buffer(test_arena_)->DestroyIface(builder.Build());
    if (!result.ok()) {
      BRCMF_ERR("Delete iface: %u failed", iface.first);
    }
    if (result->is_error()) {
      BRCMF_ERR("Delete iface: %u failed", iface.first);
    }
  }
  // Make sure to synchronously shut down the device here to avoid any in-flight FIDL calls arriving
  // during the rest of the destruction.
  zx::result prepare_stop_result = runtime().RunToCompletion(
      dut_.SyncCall(&fdf_testing::DriverUnderTest<brcmfmac::SimDevice>::PrepareStop));
  EXPECT_OK(prepare_stop_result.status_value());

  zx::result stop_result = dut_.SyncCall(&fdf_testing::DriverUnderTest<brcmfmac::Device>::Stop);
  EXPECT_OK(stop_result.status_value());
}

zx_status_t SimTest::PreInit() {
  zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_OK(start_args.status_value());

  driver_outgoing_ = std::move(start_args->outgoing_directory_client);

  zx::result init_result = test_environment_.SyncCall(
      &fdf_testing::TestEnvironment::Initialize, std::move(start_args->incoming_directory_server));
  EXPECT_OK(init_result.status_value());

  // Calling SimDevice::Start also allocates the dut. Trying to access the underlying
  // brcmfmac::SimDevice is invalid before this step.
  zx::result start_result = runtime().RunToCompletion(
      dut_.SyncCall(&fdf_testing::DriverUnderTest<brcmfmac::SimDevice>::Start,
                    std::move(start_args->start_args)));

  EXPECT_OK(start_result.status_value());

  WithSimDevice([this](brcmfmac::SimDevice* device) {
    device->InitWithEnv(env_.get(), driver_outgoing_.borrow());
  });

  driver_created_ = true;

  return ZX_OK;
}

zx_status_t SimTest::Init() {
  if (!driver_created_) {
    EXPECT_OK(PreInit());
  }

  libsync::Completion initialized;
  WithSimDevice([&](brcmfmac::SimDevice* device) {
    device->Initialize([&](zx_status_t status) {
      EXPECT_OK(status);
      initialized.Signal();
    });
  });
  initialized.Wait();

  // Connect to WlanPhyimpl served on outgoing directory.
  zx::result connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_wlan_phyimpl::Service::WlanPhyImpl>(
          CreateDriverSvcClient(), component::kDefaultInstance);

  client_ =
      fdf::WireSyncClient<fuchsia_wlan_phyimpl::WlanPhyImpl>(std::move(connect_result.value()));

  // Make a synchronous phyimpl request to ensure that we are actually connected to the phyimpl
  // protocol.
  auto result = client_.buffer(test_arena_)->GetSupportedMacRoles();
  EXPECT_TRUE(result.ok());

  return ZX_OK;
}

zx_status_t SimTest::StartInterface(wlan_common::WlanMacRole role, SimInterface* sim_ifc,
                                    std::optional<common::MacAddr> mac_addr) {
  zx_status_t status;
  if ((status = sim_ifc->Init(env_.get(), role)) != ZX_OK) {
    return status;
  }
  auto ch = zx::channel(sim_ifc->ch_mlme_);

  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_)
                     .role(role)
                     .mlme_channel(std::move(ch));

  if (mac_addr) {
    fidl::Array<unsigned char, 6> init_sta_addr;
    memcpy(&init_sta_addr, mac_addr.value().byte, ETH_ALEN);
    builder.init_sta_addr(init_sta_addr);
  }

  auto result = client_.buffer(test_arena_)->CreateIface(builder.Build());

  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    BRCMF_ERR("%s error happened while creating interface",
              zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  sim_ifc->iface_id_ = result->value()->iface_id();

  status = ZX_OK;

  if (!ifaces_.insert_or_assign(sim_ifc->iface_id_, sim_ifc).second) {
    BRCMF_ERR("Iface already exist in this test.\n");
    return ZX_ERR_ALREADY_EXISTS;
  }

  // Connect to WlanFullmacImpl
  std::string instance_name = role == wlan_common::WlanMacRole::kClient
                                  ? "brcmfmac-wlan-fullmac-client"
                                  : "brcmfmac-wlan-fullmac-ap";

  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>(
          CreateDriverSvcClient(), instance_name);
  EXPECT_EQ(ZX_OK, driver_connect_result.status_value());

  status = sim_ifc->Connect(std::move(driver_connect_result.value()), df_env_dispatcher_->get());
  if (status != ZX_OK) {
    BRCMF_ERR("Failed to establish FIDL connection with WlanInterface: %s",
              zx_status_get_string(status));
    return status;
  }

  // check that fullmac device count is expected.
  auto fullmac_service_prop = fdf::MakeProperty(bind_fuchsia_wlan_fullmac::SERVICE,
                                                bind_fuchsia_wlan_fullmac::SERVICE_DRIVERTRANSPORT);
  EXPECT_EQ(ifaces_.size(), DeviceCountWithProperty(fullmac_service_prop));

  return ZX_OK;
}

zx_status_t SimTest::InterfaceDestroyed(SimInterface* ifc) {
  auto iter = ifaces_.find(ifc->iface_id_);

  if (iter == ifaces_.end()) {
    BRCMF_ERR("Iface id: %d does not exist", ifc->iface_id_);
    return ZX_ERR_NOT_FOUND;
  }

  // Destroy the server_dispatcher_ so that when this SimInterface is started again, the
  // server_dispatcher_ can be overwritten.
  ifc->Reset();
  ifaces_.erase(iter);

  auto fullmac_service_prop = fdf::MakeProperty(bind_fuchsia_wlan_fullmac::SERVICE,
                                                bind_fuchsia_wlan_fullmac::SERVICE_DRIVERTRANSPORT);
  WaitForDeviceCountWithProperty(fullmac_service_prop, ifaces_.size());

  // Wait until reset is complete. This has to happen on this thread, not the driver dispatcher.
  // Otherwise the wait will block part of the recovery work that has to happen on the driver
  // dispatcher.
  brcmfmac::SimDevice* device_ptr = nullptr;
  WithSimDevice([&](brcmfmac::SimDevice* device) { device_ptr = device; });
  device_ptr->WaitForRecoveryComplete();

  return ZX_OK;
}

uint32_t SimTest::DeviceCount() {
  return node_server_.SyncCall([](fdf_testing::TestNode* root) { return root->children().size(); });
}

uint32_t SimTest::DeviceCountWithProperty(const fuchsia_driver_framework::NodeProperty& property) {
  return node_server_.SyncCall([&](fdf_testing::TestNode* root) {
    uint32_t count = 0;
    for (const auto& [_, child] : root->children()) {
      for (const fuchsia_driver_framework::NodeProperty& child_property : child.GetProperties()) {
        if (child_property == property) {
          count++;
          break;
        }
      }
    }

    return count;
  });
}

void SimTest::WaitForDeviceCount(uint32_t expected) {
  while (expected != DeviceCount()) {
    std::this_thread::sleep_for(std::chrono::seconds(1));
  }
}

void SimTest::WaitForDeviceCountWithProperty(const fuchsia_driver_framework::NodeProperty& property,
                                             uint32_t expected) {
  while (expected != DeviceCountWithProperty(property)) {
    std::this_thread::sleep_for(std::chrono::seconds(1));
  }
}

void SimTest::WithSimDevice(fit::function<void(brcmfmac::SimDevice*)> callback) {
  dut().SyncCall([callback = std::move(callback)](
                     fdf_testing::DriverUnderTest<brcmfmac::SimDevice>* dut) mutable {
    // *dut dereferences the pointer and yields a DriverUnderTest<SimDevice>
    // *(DriverUnderTest<SimDevice>) (i.e., **dut) yields a SimDevice*
    callback(**dut);
  });
}

zx_status_t SimTest::DeleteInterface(SimInterface* ifc) {
  auto iter = ifaces_.find(ifc->iface_id_);

  if (iter == ifaces_.end()) {
    BRCMF_ERR("Iface id: %d does not exist", ifc->iface_id_);
    return ZX_ERR_NOT_FOUND;
  }

  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_);
  builder.iface_id(iter->first);
  auto result = client_.buffer(test_arena_)->DestroyIface(builder.Build());
  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    BRCMF_ERR("Failed to destroy interface.\n");
    return result->error_value();
  }

  ifc->Reset();

  // Once the interface data structures have been deleted, our pointers are no longer valid.
  ifaces_.erase(iter);

  auto fullmac_service_prop = fdf::MakeProperty(bind_fuchsia_wlan_fullmac::SERVICE,
                                                bind_fuchsia_wlan_fullmac::SERVICE_DRIVERTRANSPORT);
  WaitForDeviceCountWithProperty(fullmac_service_prop, ifaces_.size());

  return ZX_OK;
}

fidl::ClientEnd<fuchsia_io::Directory> SimTest::CreateDriverSvcClient() {
  // Open the svc directory in the driver's outgoing, and store a client to it.
  auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

  zx_status_t status = fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                                    static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                    svc_endpoints.server.TakeChannel().release());
  EXPECT_EQ(ZX_OK, status);
  return std::move(svc_endpoints.client);
}

}  // namespace wlan::brcmfmac
