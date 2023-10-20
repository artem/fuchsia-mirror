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

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/wlan_interface.h"

#include <arpa/inet.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <wlan/common/element.h>
#include <wlan/common/ieee80211.h>
#include <wlan/common/ieee80211_codes.h>
#include <wlan/common/mac_frame.h>

#include "fidl/fuchsia.wlan.ieee80211/cpp/common_types.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/data_plane.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ies.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/key_ring.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/utils.h"

namespace wlan::nxpfmac {

namespace fuchsia_wlan_common_wire = fuchsia_wlan_common::wire;
namespace fuchsia_wlan_ieee80211_wire = fuchsia_wlan_ieee80211::wire;
namespace fuchsia_wlan_fullmac_wire = fuchsia_wlan_fullmac::wire;
// This usually completes in about 120 milliseconds, give it plenty of time to complete but not too
// long since connect scanning is synchronous.
constexpr zx_duration_t kConnectScanTimeout = ZX_MSEC(600);

WlanInterface::WlanInterface(zx_device_t* parent, uint32_t iface_index,
                             fuchsia_wlan_common::wire::WlanMacRole role, DeviceContext* context,
                             const uint8_t mac_address[ETH_ALEN], zx::channel&& mlme_channel)
    : WlanInterfaceDeviceType(parent),
      wlan::drivers::components::NetworkPort(context->data_plane_->NetDevIfcProto(), *this,
                                             static_cast<uint8_t>(iface_index)),
      role_(role),
      iface_index_(iface_index),
      mlme_channel_(std::move(mlme_channel)),
      key_ring_(context->ioctl_adapter_, iface_index),
      client_connection_(this, context, &key_ring_, iface_index),
      scanner_(context, iface_index),
      soft_ap_(this, context, iface_index),
      context_(context),
      outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {
  memcpy(mac_address_, mac_address, ETH_ALEN);
}

zx_status_t WlanInterface::Create(zx_device_t* parent, uint32_t iface_index,
                                  fuchsia_wlan_common::wire::WlanMacRole role,
                                  DeviceContext* context, const uint8_t mac_address[ETH_ALEN],
                                  zx::channel&& mlme_channel, WlanInterface** out_interface) {
  std::unique_ptr<WlanInterface> interface(
      new WlanInterface(parent, iface_index, role, context, mac_address, std::move(mlme_channel)));

  // Set the given mac address in FW.
  zx_status_t status = interface->SetMacAddressInFw();
  if (status != ZX_OK) {
    NXPF_ERR("Unable to set Mac address in FW: %s", zx_status_get_string(status));
    return status;
  }

  NetworkPort::Role net_port_role;
  switch (role) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient:
      net_port_role = NetworkPort::Role::Client;
      break;
    case fuchsia_wlan_common::wire::WlanMacRole::kAp:
      net_port_role = NetworkPort::Role::Ap;
      break;
    default:
      NXPF_ERR("Unsupported role %u", role);
      return ZX_ERR_INVALID_ARGS;
  }

  status = interface->NetworkPort::Init(net_port_role);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to initialize port: %s", zx_status_get_string(status));
    return status;
  }

  *out_interface = interface.release();  // This now has its lifecycle managed by the devhost.
  return ZX_OK;
}

void WlanInterface::Remove(fit::callback<void()>&& on_remove) {
  {
    std::lock_guard lock(mutex_);
    on_remove_ = std::move(on_remove);
  }
  DdkAsyncRemove();
}

void WlanInterface::DdkRelease() {
  fit::callback<void()> on_remove;
  {
    std::lock_guard lock(mutex_);
    if (on_remove_) {
      on_remove = std::move(on_remove_);
    }
  }
  delete this;
  if (on_remove) {
    on_remove();
  }
}

zx_status_t WlanInterface::ServeWlanFullmacImplProtocol(
    fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) mutable {
    fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(server_end), this);
  };
  fuchsia_wlan_fullmac::Service::InstanceHandler handler(
      {.wlan_fullmac_impl = std::move(protocol)});
  auto status = outgoing_dir_.AddService<fuchsia_wlan_fullmac::Service>(std::move(handler));
  if (status.is_error()) {
    NXPF_ERR("%s(): Failed to add service to outgoing directory: %s\n", __func__,
             status.status_string());
    return status.error_value();
  }
  auto result = outgoing_dir_.Serve(std::move(server_end));
  if (result.is_error()) {
    NXPF_ERR("%s(): Failed to serve outgoing directory: %s\n", __func__, result.status_string());
    return result.error_value();
  }

  return ZX_OK;
}

void WlanInterface::Start(StartRequestView request, fdf::Arena& arena,
                          StartCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);
  if (!mlme_channel_.is_valid()) {
    NXPF_ERR("IF already bound");
    completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_BOUND);
  }

  NXPF_INFO("Starting wlan_fullmac interface");
  {
    fullmac_ifc_ =
        fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(request->ifc));
  }
  is_up_ = true;

  completer.buffer(arena).ReplySuccess(std::move(mlme_channel_));
}

void WlanInterface::OnEapolResponse(wlan::drivers::components::Frame&& frame) {
  // This work cannot be run on this thread since it's triggered from the mlan main process. There
  // are locks in fullmac that could block this call if fullmac is calling into this driver and
  // causes something to wait for the mlan main process to run. Unfortunately std::function doesn't
  // allow capturing of move-only objects, it requires them to be copy-constructible. This means we
  // can't move capture `frame`, we have to make a copy of the eapol data instead. Fortunately this
  // is not performance critical.
  std::vector<uint8_t> eapol(frame.Data(), frame.Data() + frame.Size());
  zx_status_t status = async::PostTask(context_->device_->GetDispatcher(), [this, eapol] {
    constexpr size_t kEapolDataOffset = sizeof(ethhdr);
    fuchsia_wlan_fullmac_wire::WlanFullmacEapolIndication eapol_ind;
    uint8_t* data_bytes = const_cast<uint8_t*>(eapol.data());
    eapol_ind.data = ::fidl::VectorView<uint8_t>::FromExternal(data_bytes + kEapolDataOffset,
                                                               eapol.size() - kEapolDataOffset);

    auto eth = reinterpret_cast<const ethhdr*>(eapol.data());
    memcpy(eapol_ind.dst_addr.data(), eth->h_dest, sizeof(eapol_ind.dst_addr));
    memcpy(eapol_ind.src_addr.data(), eth->h_source, sizeof(eapol_ind.src_addr));

    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
      return;
    }
    auto result = fullmac_ifc_.buffer(*arena)->EapolInd(eapol_ind);
    if (!result.ok()) {
      NXPF_ERR("Failed to send eapol ind result.status: %s", result.status_string());
    }
  });
  if (status != ZX_OK) {
    NXPF_ERR("Failed to schedule EAPOL response: %s", zx_status_get_string(status));
  }
}

void WlanInterface::OnEapolTransmitted(zx_status_t status, const uint8_t* dst_addr) {
  const fuchsia_wlan_fullmac_wire::WlanEapolResult result =
      status == ZX_OK ? fuchsia_wlan_fullmac_wire::WlanEapolResult::kSuccess
                      : fuchsia_wlan_fullmac_wire::WlanEapolResult::kTransmissionFailure;
  fuchsia_wlan_fullmac_wire::WlanFullmacEapolConfirm response{.result_code = result};
  memcpy(response.dst_addr.data(), dst_addr, sizeof(response.dst_addr));

  // This work cannot be run on this thread since it's triggered from the mlan main process. There
  // are locks in fullmac that could block this call if fullmac is calling into this driver and
  // causes something to wait for the mlan main process to run.
  status = async::PostTask(context_->device_->GetDispatcher(), [this, res = &response] {
    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
      return;
    }
    auto sts = fullmac_ifc_.buffer(*arena)->EapolConf(*res);
    if (!sts.ok()) {
      NXPF_ERR("Failed to send eapol conf result.status: %s", sts.status_string());
    }
  });
  if (status != ZX_OK) {
    NXPF_ERR("Failed to schedule EAPOL transmit confirmation: %s", zx_status_get_string(status));
  }
}

void WlanInterface::Stop(fdf::Arena& arena, StopCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

#define WLAN_MAXRATE 108  /* in 500kbps units */
#define WLAN_RATE_1M 2    /* in 500kbps units */
#define WLAN_RATE_2M 4    /* in 500kbps units */
#define WLAN_RATE_5M5 11  /* in 500kbps units */
#define WLAN_RATE_11M 22  /* in 500kbps units */
#define WLAN_RATE_6M 12   /* in 500kbps units */
#define WLAN_RATE_9M 18   /* in 500kbps units */
#define WLAN_RATE_12M 24  /* in 500kbps units */
#define WLAN_RATE_18M 36  /* in 500kbps units */
#define WLAN_RATE_24M 48  /* in 500kbps units */
#define WLAN_RATE_36M 72  /* in 500kbps units */
#define WLAN_RATE_48M 96  /* in 500kbps units */
#define WLAN_RATE_54M 108 /* in 500kbps units */

void WlanInterface::Query(fdf::Arena& arena, QueryCompleter::Sync& completer) {
  constexpr uint8_t kRates2g[] = {WLAN_RATE_1M,  WLAN_RATE_2M,  WLAN_RATE_5M5, WLAN_RATE_11M,
                                  WLAN_RATE_6M,  WLAN_RATE_9M,  WLAN_RATE_12M, WLAN_RATE_18M,
                                  WLAN_RATE_24M, WLAN_RATE_36M, WLAN_RATE_48M, WLAN_RATE_54M};
  constexpr uint8_t kRates5g[] = {WLAN_RATE_6M,  WLAN_RATE_9M,  WLAN_RATE_12M, WLAN_RATE_18M,
                                  WLAN_RATE_24M, WLAN_RATE_36M, WLAN_RATE_48M, WLAN_RATE_54M};
  static_assert(std::size(kRates2g) <= fuchsia_wlan_ieee80211_MAX_SUPPORTED_BASIC_RATES);
  static_assert(std::size(kRates5g) <= fuchsia_wlan_ieee80211_MAX_SUPPORTED_BASIC_RATES);

  // Retrieve the list of supported channels. This does not call into firmware, it's just mlan
  // computing all the channels and it shouldn't fail as long as the request is properly formatted.
  IoctlRequest<mlan_ds_bss> get_channels(MLAN_IOCTL_BSS, MLAN_ACT_GET, iface_index_,
                                         {.sub_command = MLAN_OID_BSS_CHANNEL_LIST});
  auto& chanlist = get_channels.UserReq().param.chanlist;
  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&get_channels);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to retrieve supported channels: %d", io_status);
    // Clear this to make sure we don't populate any channels.
    chanlist.num_of_chan = 0;
    completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  }

  // Clear info first.
  fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo info;

  std::lock_guard lock(mutex_);

  // Retrieve FW Info (contains HT and VHT capabilities).
  IoctlRequest<mlan_ds_get_info> info_req;
  info_req = IoctlRequest<mlan_ds_get_info>(MLAN_IOCTL_GET_INFO, MLAN_ACT_GET, 0,
                                            {.sub_command = MLAN_OID_GET_FW_INFO});
  auto& fw_info = info_req.UserReq().param.fw_info;
  io_status = context_->ioctl_adapter_->IssueIoctlSync(&info_req);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("FW Info get req failed: %d", io_status);
    completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  }

  info.role = role_;
  info.band_cap_count = 2;
  memcpy(info.sta_addr.data(), mac_address_, sizeof(mac_address_));

  // "features" field is populated with default value.

  // 2.4 GHz
  fuchsia_wlan_fullmac::wire::WlanFullmacBandCapability* band_cap = &info.band_cap_list[0];
  band_cap->band = fuchsia_wlan_common::wire::WlanBand::kTwoGhz;
  band_cap->basic_rate_count = std::size(kRates2g);
  memcpy(band_cap->basic_rate_list.data(), kRates2g, sizeof(kRates2g));

  for (uint32_t i = 0, chan = 0;
       i < chanlist.num_of_chan && i < std::size(band_cap->operating_channel_list); ++i) {
    if (band_from_channel(chanlist.cf[i].channel) == BAND_2GHZ) {
      band_cap->operating_channel_list[chan++] = static_cast<uint8_t>(chanlist.cf[i].channel);
      ++band_cap->operating_channel_count;
    }
  }
  band_cap->ht_supported = true;
  band_cap->vht_supported = true;
  wlan::HtCapabilities* ht_caps =
      wlan::HtCapabilities::ViewFromRawBytes(band_cap->ht_caps.bytes.data());
  wlan::VhtCapabilities* vht_caps =
      wlan::VhtCapabilities::ViewFromRawBytes(band_cap->vht_caps.bytes.data());
  ht_caps->ht_cap_info.set_as_uint16(fw_info.hw_dot_11n_dev_cap & 0xFFFF);
  vht_caps->vht_cap_info.set_as_uint32(fw_info.hw_dot_11ac_dev_cap);

  // 5 GHz
  band_cap = &info.band_cap_list[1];
  band_cap->band = fuchsia_wlan_common::wire::WlanBand::kFiveGhz;
  band_cap->basic_rate_count = std::size(kRates5g);
  memcpy(band_cap->basic_rate_list.data(), kRates5g, sizeof(kRates5g));

  for (uint32_t i = 0, chan = 0;
       i < chanlist.num_of_chan && i < std::size(band_cap->operating_channel_list); ++i) {
    if (band_from_channel(chanlist.cf[i].channel) == BAND_5GHZ) {
      band_cap->operating_channel_list[chan++] = static_cast<uint8_t>(chanlist.cf[i].channel);
      ++band_cap->operating_channel_count;
    }
  }
  band_cap->ht_supported = true;
  band_cap->vht_supported = true;
  ht_caps = wlan::HtCapabilities::ViewFromRawBytes(band_cap->ht_caps.bytes.data());
  vht_caps = wlan::VhtCapabilities::ViewFromRawBytes(band_cap->vht_caps.bytes.data());
  ht_caps->ht_cap_info.set_as_uint16(fw_info.hw_dot_11n_dev_cap & 0xFFFF);
  vht_caps->vht_cap_info.set_as_uint32(fw_info.hw_dot_11ac_dev_cap);

  completer.buffer(arena).ReplySuccess(info);
}

void WlanInterface::QueryMacSublayerSupport(fdf::Arena& arena,
                                            QueryMacSublayerSupportCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  fuchsia_wlan_common::wire::MacSublayerSupport resp;
  resp.rate_selection_offload.supported = false;
  resp.data_plane.data_plane_type = fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice;
  resp.device.mac_implementation_type = fuchsia_wlan_common::wire::MacImplementationType::kFullmac;
  resp.device.is_synthetic = false;
  resp.device.tx_status_report_supported = false;

  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::QuerySecuritySupport(fdf::Arena& arena,
                                         QuerySecuritySupportCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  fuchsia_wlan_common::wire::SecuritySupport resp;
  resp.sae.sme_handler_supported = true;
  resp.sae.driver_handler_supported = false;
  resp.mfp.supported = true;

  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::QuerySpectrumManagementSupport(
    fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);
  fuchsia_wlan_common::wire::SpectrumManagementSupport resp;
  resp.dfs.supported = false;
  completer.buffer(arena).ReplySuccess(resp);
}

void WlanInterface::StartScan(StartScanRequestView request, fdf::Arena& arena,
                              StartScanCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  auto on_scan_result = [this](const fuchsia_wlan_fullmac_wire::WlanFullmacScanResult& result) {
    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
      return;
    }
    auto status = fullmac_ifc_.buffer(*arena)->OnScanResult(result);
    if (!status.ok()) {
      NXPF_ERR("Failed to send scan result.status: %s", status.status_string());
      return;
    }
  };

  auto on_scan_end = [this](uint64_t txn_id, fuchsia_wlan_fullmac_wire::WlanScanResult result) {
    fuchsia_wlan_fullmac_wire::WlanFullmacScanEnd end{.txn_id = txn_id, .code = result};
    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
      return;
    }
    auto status = fullmac_ifc_.buffer(*arena)->OnScanEnd(end);
    if (!status.ok()) {
      NXPF_ERR("Failed to send scan end result.status: %s", status.status_string());
      return;
    }
  };

  // TODO(fxbug.dev/108408): Consider calculating a more accurate scan timeout based on the max
  // scan time per channel in the scan request.
  constexpr zx_duration_t kScanTimeout = ZX_MSEC(12000);
  zx_status_t status =
      scanner_.Scan(request, kScanTimeout, std::move(on_scan_result), std::move(on_scan_end));
  if (status != ZX_OK) {
    NXPF_ERR("Scan failed: %s", zx_status_get_string(status));
    fuchsia_wlan_fullmac_wire::WlanFullmacScanEnd end{
        .txn_id = request->txn_id(),
        .code = fuchsia_wlan_fullmac_wire::WlanScanResult::kInternalError};
    auto fdf_arena = fdf::Arena::Create(0, 0);
    if (fdf_arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", fdf_arena.status_string());
      return;
    }
    auto result = fullmac_ifc_.buffer(*fdf_arena)->OnScanEnd(end);
    if (!result.ok()) {
      NXPF_ERR("Failed to send scan end result.status: %s", result.status_string());
      return;
    }
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Connect(ConnectRequestView request, fdf::Arena& arena,
                            ConnectCompleter::Sync& completer) {
  auto ssid =
      IeView(request->selected_bss().ies.data(), request->selected_bss().ies.count()).get(SSID);
  if (!ssid) {
    ConfirmConnectReq(ClientConnection::StatusCode::kInvalidParameters);
    completer.buffer(arena).Reply();
    return;
  }

  std::lock_guard lock(mutex_);

  // First stop any on-going scans.
  zx_status_t status = scanner_.StopScan();
  if (status == ZX_OK) {
    NXPF_INFO("Connecting, canceled pending scan");
  } else if (status != ZX_ERR_NOT_FOUND) {
    // An error occurred when canceling.
    NXPF_ERR("Failed to cancel on-going scan before connecting");
    ConfirmConnectReq(ClientConnection::StatusCode::kJoinFailure);
    completer.buffer(arena).Reply();
    return;
  }

  // Because MLAN expects the BSS to exist in its internal scan table we must perform a targetted
  // scan first. The WLAN stack does NOT guarantee a sequence of a connect scan followed by a
  // connection attempt.
  status = scanner_.ConnectScan(ssid->data(), ssid->size(), request->selected_bss().channel.primary,
                                kConnectScanTimeout);
  if (status != ZX_OK) {
    NXPF_ERR("Connect scan failed: %s", zx_status_get_string(status));
    ConfirmConnectReq(ClientConnection::StatusCode::kJoinFailure);
    completer.buffer(arena).Reply();
    return;
  }

  auto on_connect =
      [this](ClientConnection::StatusCode status_code, const uint8_t* ies, size_t ies_size)
          __TA_EXCLUDES(mutex_) { ConfirmConnectReq(status_code, ies, ies_size); };

  status = client_connection_.Connect(request, std::move(on_connect));
  if (status != ZX_OK) {
    ConfirmConnectReq(ClientConnection::StatusCode::kJoinFailure);
    completer.buffer(arena).Reply();
    return;
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::Reconnect(ReconnectRequestView request, fdf::Arena& arena,
                              ReconnectCompleter::Sync& completer) {
  NXPF_ERR("%s called", __func__);
  completer.buffer(arena).Reply();
}

void WlanInterface::AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                             AuthRespCompleter::Sync& completer) {
  NXPF_ERR("%s called", __func__);
  completer.buffer(arena).Reply();
}

void WlanInterface::Deauth(DeauthRequestView request, fdf::Arena& arena,
                           DeauthCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  if (!request->has_peer_sta_address() || !request->has_reason_code()) {
    NXPF_ERR("Deauth Req does not contain all the required fields peer addr: %d reason code: %d",
             request->has_peer_sta_address(), request->has_reason_code());
    return;
  }
  if (role_ == fuchsia_wlan_common::wire::WlanMacRole::kAp) {
    // Deauth the specified client associated to the SoftAP.
    zx_status_t status = soft_ap_.DeauthSta(request->peer_sta_address().data(),
                                            fidl::ToUnderlying(request->reason_code()));
    if (status != ZX_OK) {
      auto arena = fdf::Arena::Create(0, 0);
      if (arena.is_error()) {
        NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
        return;
      }

      auto resp = fuchsia_wlan_fullmac_wire::WlanFullmacImplIfcDeauthConfRequest::Builder(*arena)
                      .peer_sta_address(request->peer_sta_address())
                      .Build();

      auto result = fullmac_ifc_.buffer(*arena)->DeauthConf(resp);
      if (!result.ok()) {
        NXPF_ERR("Failed to send deauth conf result.status: %s", result.status_string());
        return;
      }
    }
    // If the request is successful, the notification should occur via the event handler.
    completer.buffer(arena).Reply();
    return;
  }

  auto on_disconnect = [this](IoctlStatus status) __TA_EXCLUDES(mutex_) {
    if (status != IoctlStatus::Success) {
      NXPF_ERR("Deauth failed: %d", status);
    }
    // This doesn't have any way of indicating what went wrong.
    ConfirmDeauth();
  };

  zx_status_t status = client_connection_.Disconnect(request->peer_sta_address().data(),
                                                     fidl::ToUnderlying(request->reason_code()),
                                                     std::move(on_disconnect));
  if (status != ZX_OK) {
    // The request didn't work, send the notification right away.
    ConfirmDeauth();
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                              AssocRespCompleter::Sync& completer) {
  NXPF_ERR("%s called", __func__);
  completer.buffer(arena).Reply();
}

void WlanInterface::Disassoc(DisassocRequestView request, fdf::Arena& arena,
                             DisassocCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  if (!request->has_reason_code() || !request->has_peer_sta_address()) {
    NXPF_ERR("Disassoc req does not contain all fields reason: %d sta address: %d",
             request->has_reason_code(), request->has_peer_sta_address());
    return;
  }
  auto on_disconnect = [this](IoctlStatus io_status) __TA_EXCLUDES(mutex_) {
    zx_status_t status = ZX_OK;
    switch (io_status) {
      case IoctlStatus::Success:
        status = ZX_OK;
        break;
      case IoctlStatus::Canceled:
        NXPF_ERR("Deauth canceled");
        status = ZX_ERR_CANCELED;
        break;
      case IoctlStatus::Timeout:
        NXPF_ERR("Deauth timed out");
        status = ZX_ERR_TIMED_OUT;
        break;
      default:
        NXPF_ERR("Deauth failed: %d", status);
        status = ZX_ERR_INTERNAL;
        break;
    }
    ConfirmDisassoc(status);
  };

  zx_status_t status = client_connection_.Disconnect(request->peer_sta_address().data(),
                                                     fidl::ToUnderlying(request->reason_code()),
                                                     std::move(on_disconnect));
  if (status != ZX_OK) {
    // The request didn't work, send the notification right away.
    ConfirmDisassoc(status);
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::ResetReq(ResetReqRequestView request, fdf::Arena& arena,
                             ResetReqCompleter::Sync& completer) {
  NXPF_ERR("%s called", __func__);
  completer.buffer(arena).Reply();
}

void WlanInterface::StartReq(StartReqRequestView request, fdf::Arena& arena,
                             StartReqCompleter::Sync& completer) {
  const fuchsia_wlan_fullmac_wire::WlanStartResult result =
      [&]() -> fuchsia_wlan_fullmac_wire::WlanStartResult {
    std::lock_guard lock(mutex_);
    if (role_ != fuchsia_wlan_common::wire::WlanMacRole::kAp) {
      NXPF_ERR("Start is supported only in AP mode, current mode: %d", role_);
      return fuchsia_wlan_fullmac_wire::WlanStartResult::kNotSupported;
    }
    if (request->req.bss_type != fuchsia_wlan_common_wire::BssType::kInfrastructure) {
      NXPF_ERR("Attempt to start AP in unsupported mode (%d)", request->req.bss_type);
      return fuchsia_wlan_fullmac_wire::WlanStartResult::kNotSupported;
    }
    return soft_ap_.Start(&request->req);
  }();

  fuchsia_wlan_fullmac_wire::WlanFullmacStartConfirm start_conf = {.result_code = result};
  auto ifc_arena = fdf::Arena::Create(0, 0);
  if (ifc_arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", ifc_arena.status_string());
    return;
  }
  auto status = fullmac_ifc_.buffer(*ifc_arena)->StartConf(start_conf);
  if (!status.ok()) {
    NXPF_ERR("Failed to send start conf result.status: %s", status.status_string());
    return;
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::StopReq(StopReqRequestView request, fdf::Arena& arena,
                            StopReqCompleter::Sync& completer) {
  const fuchsia_wlan_fullmac_wire::WlanStopResult result =
      [&]() -> fuchsia_wlan_fullmac_wire::WlanStopResult {
    std::lock_guard lock(mutex_);

    if (role_ != fuchsia_wlan_common::wire::WlanMacRole::kAp) {
      NXPF_ERR("Stop is supported only in AP mode, current mode: %d", role_);
      return fuchsia_wlan_fullmac_wire::WlanStopResult::kInternalError;
    }

    return soft_ap_.Stop(&request->req);
  }();

  fuchsia_wlan_fullmac_wire::WlanFullmacStopConfirm stop_conf = {.result_code = result};
  auto ifc_arena = fdf::Arena::Create(0, 0);
  if (ifc_arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", ifc_arena.status_string());
    return;
  }
  auto status = fullmac_ifc_.buffer(*ifc_arena)->StopConf(stop_conf);
  if (!status.ok()) {
    NXPF_ERR("Failed to send stop conf result.status: %s", status.status_string());
    return;
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                               SetKeysReqCompleter::Sync& completer) {
  fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysResp resp;
  for (uint64_t i = 0; i < request->req.num_keys; ++i) {
    const zx_status_t status = key_ring_.AddKey(request->req.keylist[i]);
    if (status != ZX_OK) {
      NXPF_WARN("Error adding key %" PRIu64 ": %s", i, zx_status_get_string(status));
    }
    resp.statuslist[i] = status;
  }
  resp.num_keys = request->req.num_keys;
  completer.buffer(arena).Reply(resp);
}

void WlanInterface::DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                               DelKeysReqCompleter::Sync& completer) {
  for (uint64_t i = 0; i < request->req.num_keys; ++i) {
    const zx_status_t status =
        key_ring_.RemoveKey(request->req.keylist[i].key_id, request->req.keylist[i].address.data());
    if (status != ZX_OK) {
      NXPF_WARN("Error deleting key %" PRIu64 ": %s", i, zx_status_get_string(status));
    }
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::EapolReq(EapolReqRequestView request, fdf::Arena& arena,
                             EapolReqCompleter::Sync& completer) {
  std::optional<wlan::drivers::components::Frame> frame = context_->data_plane_->AcquireFrame();
  if (!frame.has_value()) {
    NXPF_ERR("Failed to acquire frame container for EAPOL frame");
    fuchsia_wlan_fullmac_wire::WlanFullmacEapolConfirm response{
        .result_code = fuchsia_wlan_fullmac_wire::WlanEapolResult::kTransmissionFailure,
    };
    auto ifc_arena = fdf::Arena::Create(0, 0);
    if (ifc_arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", ifc_arena.status_string());
      return;
    }
    auto result = fullmac_ifc_.buffer(*ifc_arena)->EapolConf(response);
    if (!result.ok()) {
      NXPF_ERR("Failed to send eapol conf result.status: %s", result.status_string());
      return;
    }
    completer.buffer(arena).Reply();
    return;
  }

  const uint32_t packet_length =
      static_cast<uint32_t>(2ul * ETH_ALEN + sizeof(uint16_t) + request->req.data.count());

  frame->ShrinkHead(1024);
  frame->SetPortId(PortId());
  frame->SetSize(packet_length);

  memcpy(frame->Data(), request->req.dst_addr.data(), ETH_ALEN);
  memcpy(frame->Data() + ETH_ALEN, request->req.src_addr.data(), ETH_ALEN);
  *reinterpret_cast<uint16_t*>(frame->Data() + 2ul * ETH_ALEN) = htons(ETH_P_PAE);
  memcpy(frame->Data() + 2ul * ETH_ALEN + sizeof(uint16_t), request->req.data.data(),
         request->req.data.count());

  context_->data_plane_->NetDevQueueTx(cpp20::span<wlan::drivers::components::Frame>(&*frame, 1u));
  completer.buffer(arena).Reply();
}

void WlanInterface::GetIfaceCounterStats(fdf::Arena& arena,
                                         GetIfaceCounterStatsCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanInterface::GetIfaceHistogramStats(fdf::Arena& arena,
                                           GetIfaceHistogramStatsCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanInterface::StartCaptureFrames(StartCaptureFramesRequestView request, fdf::Arena& arena,
                                       StartCaptureFramesCompleter::Sync& completer) {
  // Reply with the structure initialized with default value(0).
  fuchsia_wlan_fullmac::wire::WlanFullmacStartCaptureFramesResp resp;
  completer.buffer(arena).Reply(resp);
}

void WlanInterface::StopCaptureFrames(fdf::Arena& arena,
                                      StopCaptureFramesCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void WlanInterface::SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                                        SetMulticastPromiscCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanInterface::SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                                     SaeHandshakeRespCompleter::Sync& completer) {
  std::lock_guard lock_(mutex_);
  const zx_status_t status = client_connection_.OnSaeResponse(
      request->resp.peer_sta_address.data(), static_cast<uint16_t>(request->resp.status_code));
  if (status != ZX_OK) {
    NXPF_ERR("Failed to handle SAE handshake response: %s", zx_status_get_string(status));
    if (request->resp.status_code != fuchsia_wlan_ieee80211_wire::StatusCode::kSuccess) {
      ConfirmConnectReq(static_cast<ClientConnection::StatusCode>(request->resp.status_code));
    } else {
      ConfirmConnectReq(ClientConnection::StatusCode::kJoinFailure);
    }
    completer.buffer(arena).Reply();
    return;
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                               SaeFrameTxCompleter::Sync& completer) {
  std::lock_guard lock(mutex_);

  cpp20::span<const uint8_t> sae_fields(request->frame.sae_fields.data(),
                                        request->frame.sae_fields.count());
  zx_status_t status = client_connection_.TransmitAuthFrame(
      mac_address_, request->frame.peer_sta_address.data(), request->frame.seq_num,
      static_cast<uint16_t>(request->frame.status_code), sae_fields);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to transmit SAE auth frame: %s", zx_status_get_string(status));
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/110091): Implement support for this.

  fuchsia_wlan_common_wire::WlanWmmParameters wmm{};
  auto ifc_arena = fdf::Arena::Create(0, 0);
  if (ifc_arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", ifc_arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*ifc_arena)->OnWmmStatusResp(ZX_OK, wmm);
  if (!result.ok()) {
    NXPF_ERR("Failed to send wmm status resp result.status: %s", result.status_string());
    return;
  }
  completer.buffer(arena).Reply();
}

void WlanInterface::OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                                       OnLinkStateChangedCompleter::Sync& completer) {
  SetPortOnline(request->online);
  completer.buffer(arena).Reply();
}

void WlanInterface::OnDisconnectEvent(uint16_t reason_code) {
  fuchsia_wlan_fullmac_wire::WlanFullmacDeauthIndication ind;

  memcpy(ind.peer_sta_address.data(), mac_address_, sizeof(mac_address_));
  ind.reason_code = static_cast<fuchsia_wlan_ieee80211_wire::ReasonCode>(reason_code);

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->DeauthInd(ind);
  if (!result.ok()) {
    NXPF_ERR("Failed to send scan end result.status: %s", result.status_string());
    return;
  }
}

void WlanInterface::SignalQualityIndication(int8_t rssi, int8_t snr) {
  fuchsia_wlan_fullmac_wire::WlanFullmacSignalReportIndication signal_ind = {.rssi_dbm = rssi,
                                                                             .snr_db = snr};
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->SignalReport(signal_ind);
  if (!result.ok()) {
    NXPF_ERR("Failed to send signal report result.status: %s", result.status_string());
    return;
  }
}

void WlanInterface::InitiateSaeHandshake(const uint8_t* peer_mac) {
  fuchsia_wlan_fullmac_wire::WlanFullmacSaeHandshakeInd ind{};
  memcpy(ind.peer_sta_address.data(), peer_mac, ETH_ALEN);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->SaeHandshakeInd(ind);
  if (!result.ok()) {
    NXPF_ERR("Failed to send sae handshake ind result.status: %s", result.status_string());
    return;
  }
}

void WlanInterface::OnAuthFrame(const uint8_t* peer_mac, const wlan::Authentication* frame,
                                cpp20::span<const uint8_t> trailing_data) {
  fuchsia_wlan_fullmac_wire::WlanFullmacSaeFrame sae_frame{
      .status_code = static_cast<fuchsia_wlan_ieee80211_wire::StatusCode>(frame->status_code),
      .seq_num = frame->auth_txn_seq_number};
  uint8_t* data_bytes = const_cast<uint8_t*>(trailing_data.data());
  memcpy(sae_frame.peer_sta_address.data(), peer_mac, ETH_ALEN);
  sae_frame.sae_fields =
      ::fidl::VectorView<uint8_t>::FromExternal(data_bytes, trailing_data.size());

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->SaeFrameRx(sae_frame);
  if (!result.ok()) {
    NXPF_ERR("Failed to send sae frame Rx result.status: %s", result.status_string());
    return;
  }
}

// Handle STA connect event for SoftAP.
void WlanInterface::OnStaConnectEvent(uint8_t* sta_mac_addr, uint8_t* ies, uint32_t ie_length) {
  // This is the only event from FW for STA connect. Indicate both auth and assoc to SME.
  fuchsia_wlan_fullmac_wire::WlanFullmacAuthInd auth_ind_params = {};
  memcpy(auth_ind_params.peer_sta_address.data(), sta_mac_addr, ETH_ALEN);
  // We always authenticate as an open system for WPA
  auth_ind_params.auth_type = fuchsia_wlan_fullmac_wire::WlanAuthType::kOpenSystem;
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->AuthInd(auth_ind_params);
  if (!result.ok()) {
    NXPF_ERR("Failed to send scan end result.status: %s", result.status_string());
    return;
  }

  // Indicate assoc.
  fuchsia_wlan_fullmac_wire::WlanFullmacAssocInd assoc_ind_params = {};
  memcpy(assoc_ind_params.peer_sta_address.data(), sta_mac_addr, ETH_ALEN);
  if (ie_length && ies && (ie_length <= sizeof(assoc_ind_params.vendor_ie))) {
    memcpy(assoc_ind_params.vendor_ie.data(), ies, ie_length);
    assoc_ind_params.vendor_ie_len = ie_length;
  }
  auto assoc_arena = fdf::Arena::Create(0, 0);
  if (assoc_arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", assoc_arena.status_string());
    return;
  }
  auto assoc_result = fullmac_ifc_.buffer(*assoc_arena)->AssocInd(assoc_ind_params);
  if (!assoc_result.ok()) {
    NXPF_ERR("Failed to send assoc ind result.status: %s", assoc_result.status_string());
    return;
  }
}

// Handle STA disconnect event for SoftAP.
void WlanInterface::OnStaDisconnectEvent(uint8_t* sta_mac_addr, uint16_t reason_code,
                                         bool locally_initiated) {
  // This is the only event from FW for STA disconnect. Indicate both deauth and disassoc to SME.
  if (!fullmac_ifc_.is_valid()) {
    NXPF_ERR("IFC is not valid...probably deleted?");
    return;
  }
  if (!locally_initiated) {
    NXPF_INFO("Disconnect Event, not locally initiated, send deauth and disassoc ind");
    fuchsia_wlan_fullmac_wire::WlanFullmacDeauthIndication deauth_ind_params{};
    // Disconnect event contains the STA's mac address at offset 2.
    memcpy(deauth_ind_params.peer_sta_address.data(), sta_mac_addr, ETH_ALEN);
    deauth_ind_params.reason_code =
        static_cast<fuchsia_wlan_ieee80211_wire::ReasonCode>(reason_code);
    deauth_ind_params.locally_initiated = locally_initiated;

    auto arena = fdf::Arena::Create(0, 0);
    if (arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
      return;
    }
    // Indicate deauth.
    auto result = fullmac_ifc_.buffer(*arena)->DeauthInd(deauth_ind_params);
    if (!result.ok()) {
      NXPF_ERR("Failed to send deauth ind result.status: %s", result.status_string());
      return;
    }
    fuchsia_wlan_fullmac_wire::WlanFullmacDisassocIndication disassoc_ind_params{};
    memcpy(disassoc_ind_params.peer_sta_address.data(), sta_mac_addr, ETH_ALEN);
    disassoc_ind_params.reason_code =
        static_cast<fuchsia_wlan_ieee80211_wire::ReasonCode>(reason_code);
    disassoc_ind_params.locally_initiated = locally_initiated;
    auto disassoc_arena = fdf::Arena::Create(0, 0);
    if (disassoc_arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", disassoc_arena.status_string());
      return;
    }
    auto disassoc_result = fullmac_ifc_.buffer(*disassoc_arena)->DisassocInd(disassoc_ind_params);
    if (!disassoc_result.ok()) {
      NXPF_ERR("Failed to send disassoc ind result.status: %s", disassoc_result.status_string());
      return;
    }
  } else {
    NXPF_INFO("Disconnect Event, locally initiated, send deauth conf");
    auto deauth_arena = fdf::Arena::Create(0, 0);
    if (deauth_arena.is_error()) {
      NXPF_ERR("Failed to create Arena status=%s", deauth_arena.status_string());
      return;
    }

    fidl::Array<uint8_t, ETH_ALEN> address;
    memcpy(address.data(), sta_mac_addr, ETH_ALEN);

    auto conf =
        fuchsia_wlan_fullmac_wire::WlanFullmacImplIfcDeauthConfRequest::Builder(*deauth_arena)
            .peer_sta_address(address)
            .Build();

    auto deauth_result = fullmac_ifc_.buffer(*deauth_arena)->DeauthConf(conf);
    if (!deauth_result.ok()) {
      NXPF_ERR("Failed to send deauth conf result.status: %s", deauth_result.status_string());
      return;
    }
  }
}
uint32_t WlanInterface::PortGetMtu() { return 1500u; }

void WlanInterface::MacGetAddress(mac_address_t* out_mac) {
  memcpy(out_mac->octets, mac_address_, MAC_SIZE);
}

void WlanInterface::MacGetFeatures(features_t* out_features) {
  *out_features = {
      .multicast_filter_count = MLAN_MAX_MULTICAST_LIST_SIZE,
      .supported_modes = SUPPORTED_MAC_FILTER_MODE_MULTICAST_FILTER |
                         SUPPORTED_MAC_FILTER_MODE_MULTICAST_PROMISCUOUS |
                         SUPPORTED_MAC_FILTER_MODE_PROMISCUOUS,
  };
}

void WlanInterface::MacSetMode(mode_t mode, cpp20::span<const mac_address_t> multicast_macs) {
  IoctlRequest<mlan_ds_bss> request(MLAN_IOCTL_BSS, MLAN_ACT_SET, PortId(),
                                    {.sub_command = MLAN_OID_BSS_MULTICAST_LIST});

  auto& multicast_list = request.UserReq().param.multicast_list;

  switch (mode) {
    case MAC_FILTER_MODE_MULTICAST_FILTER:
      multicast_list.mode = MLAN_MULTICAST_MODE;
      if (multicast_macs.size() > std::size(multicast_list.mac_list)) {
        NXPF_ERR("Number of multicast macs %zu exceeds maximum value of %zu", multicast_macs.size(),
                 std::size(multicast_list.mac_list));
        return;
      }
      for (size_t i = 0; i < multicast_macs.size(); ++i) {
        memcpy(multicast_list.mac_list[i], multicast_macs[i].octets, MAC_SIZE);
      }
      multicast_list.num_multicast_addr = static_cast<uint32_t>(multicast_macs.size());
      break;
    case MAC_FILTER_MODE_MULTICAST_PROMISCUOUS:
      multicast_list.mode = MLAN_ALL_MULTI_MODE;
      break;
    case MAC_FILTER_MODE_PROMISCUOUS:
      multicast_list.mode = MLAN_PROMISC_MODE;
      break;
    default:
      NXPF_ERR("Unsupported MAC mode %u", mode);
      return;
  }

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to set mac mode: %d", io_status);
    return;
  }
}

zx_status_t WlanInterface::SetMacAddressInFw() {
  IoctlRequest<mlan_ds_bss> request(MLAN_IOCTL_BSS, MLAN_ACT_SET, iface_index_,
                                    {.sub_command = MLAN_OID_BSS_MAC_ADDR});
  request.IoctlReq().bss_index = iface_index_;
  memcpy(request.UserReq().param.mac_addr, mac_address_, ETH_ALEN);

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to perform set MAC ioctl: %d", io_status);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void WlanInterface::ConfirmDeauth() {
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }

  fidl::Array<uint8_t, ETH_ALEN> address;
  memcpy(address.data(), mac_address_, sizeof(mac_address_));

  auto resp = fuchsia_wlan_fullmac_wire::WlanFullmacImplIfcDeauthConfRequest::Builder(*arena)
                  .peer_sta_address(address)
                  .Build();

  auto result = fullmac_ifc_.buffer(*arena)->DeauthConf(resp);
  if (!result.ok()) {
    NXPF_ERR("Failed to send deauth conf result.status: %s", result.status_string());
    return;
  }
}

void WlanInterface::ConfirmDisassoc(zx_status_t status) {
  fuchsia_wlan_fullmac_wire::WlanFullmacDisassocConfirm resp{.status = status};
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto result = fullmac_ifc_.buffer(*arena)->DisassocConf(resp);
  if (!result.ok()) {
    NXPF_ERR("Failed to send disassoc conf result.status: %s", result.status_string());
    return;
  }
}

void WlanInterface::ConfirmConnectReq(ClientConnection::StatusCode status, const uint8_t* ies,
                                      size_t ies_len) {
  fuchsia_wlan_fullmac_wire::WlanFullmacConnectConfirm result = {.result_code = status};
  uint8_t* ie_data = const_cast<uint8_t*>(ies);
  result.association_ies = ::fidl::VectorView<uint8_t>::FromExternal(ie_data, ies_len);
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    NXPF_ERR("Failed to create Arena status=%s", arena.status_string());
    return;
  }
  auto sts = fullmac_ifc_.buffer(*arena)->ConnectConf(result);
  if (!sts.ok()) {
    NXPF_ERR("Failed to send connect conf result.status: %s", sts.status_string());
    return;
  }
}

}  // namespace wlan::nxpfmac
