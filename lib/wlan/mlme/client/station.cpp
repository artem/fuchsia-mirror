// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <wlan/mlme/client/station.h>

#include <wlan/common/channel.h>
#include <wlan/common/energy.h>
#include <wlan/common/logging.h>
#include <wlan/common/stats.h>
#include <wlan/mlme/client/bss.h>
#include <wlan/mlme/debug.h>
#include <wlan/mlme/device_interface.h>
#include <wlan/mlme/mac_frame.h>
#include <wlan/mlme/packet.h>
#include <wlan/mlme/sequence.h>
#include <wlan/mlme/service.h>
#include <wlan/mlme/timer.h>

#include <fuchsia/wlan/mlme/c/fidl.h>

#include <inttypes.h>
#include <algorithm>
#include <cstring>
#include <utility>

namespace wlan {

namespace wlan_stats = ::fuchsia::wlan::stats;
using common::dBm;

// TODO(hahnr): Revisit frame construction to reduce boilerplate code.

static constexpr size_t kAssocBcnCountTimeout = 20;
static constexpr size_t kSignalReportBcnCountTimeout = 10;
static constexpr zx::duration kOnChannelTimeAfterSend = zx::msec(500);

Station::Station(DeviceInterface* device, fbl::unique_ptr<Timer> timer,
                 ChannelScheduler* chan_sched)
    : device_(device), timer_(std::move(timer)), chan_sched_(chan_sched) {
    (void)assoc_timeout_;
    bssid_.Reset();
}

void Station::Reset() {
    debugfn();

    timer_->CancelTimer();
    state_ = WlanState::kUnjoined;
    bss_.reset();
    join_timeout_ = zx::time();
    auth_timeout_ = zx::time();
    last_seen_ = zx::time();
    bssid_.Reset();
}
zx_status_t Station::HandleAnyMlmeMsg(const BaseMlmeMsg& mlme_msg) {
    WLAN_STATS_INC(svc_msg.in);

    // Always process MLME-JOIN.requests.
    if (auto join_req = mlme_msg.As<wlan_mlme::JoinRequest>()) {
        return HandleMlmeJoinReq(*join_req);
    }

    // Drop other MLME requests if there is no BSSID set yet.
    if (bssid() == nullptr) { return ZX_OK; }

    if (auto auth_req = mlme_msg.As<wlan_mlme::AuthenticateRequest>()) {
        return HandleMlmeAuthReq(*auth_req);
    } else if (auto deauth_req = mlme_msg.As<wlan_mlme::DeauthenticateRequest>()) {
        return HandleMlmeDeauthReq(*deauth_req);
    } else if (auto assoc_req = mlme_msg.As<wlan_mlme::AssociateRequest>()) {
        return HandleMlmeAssocReq(*assoc_req);
    } else if (auto eapol_req = mlme_msg.As<wlan_mlme::EapolRequest>()) {
        return HandleMlmeEapolReq(*eapol_req);
    } else if (auto setkeys_req = mlme_msg.As<wlan_mlme::SetKeysRequest>()) {
        return HandleMlmeSetKeysReq(*setkeys_req);
    }
    return ZX_OK;
}

zx_status_t Station::HandleAnyFrame(fbl::unique_ptr<Packet> pkt) {
    switch (pkt->peer()) {
    case Packet::Peer::kEthernet: {
        if (auto eth_frame = EthFrameView::CheckType(pkt.get()).CheckLength()) {
            HandleEthFrame(eth_frame.IntoOwned(fbl::move(pkt)));
        }
        break;
    }
    case Packet::Peer::kWlan:
        return HandleAnyWlanFrame(fbl::move(pkt));
    default:
        errorf("unknown Packet peer: %u\n", pkt->peer());
        break;
    }

    return ZX_OK;
}

zx_status_t Station::HandleAnyWlanFrame(fbl::unique_ptr<Packet> pkt) {
    if (auto possible_mgmt_frame = MgmtFrameView<>::CheckType(pkt.get())) {
        auto mgmt_frame = possible_mgmt_frame.CheckLength();
        if (!mgmt_frame) { return ZX_ERR_BUFFER_TOO_SMALL; }

        HandleAnyMgmtFrame(mgmt_frame.IntoOwned(fbl::move(pkt)));
    } else if (auto possible_data_frame = DataFrameView<>::CheckType(pkt.get())) {
        auto data_frame = possible_data_frame.CheckLength();
        if (!data_frame) { return ZX_ERR_BUFFER_TOO_SMALL; }

        HandleAnyDataFrame(data_frame.IntoOwned(fbl::move(pkt)));
    }

    return ZX_OK;
}

zx_status_t Station::HandleAnyMgmtFrame(MgmtFrame<>&& frame) {
    auto mgmt_frame = frame.View();

    WLAN_STATS_INC(mgmt_frame.in);
    if (ShouldDropMgmtFrame(mgmt_frame)) {
        WLAN_STATS_INC(mgmt_frame.drop);
        return ZX_ERR_NOT_SUPPORTED;
    }
    WLAN_STATS_INC(mgmt_frame.out);

    if (auto possible_bcn_frame = mgmt_frame.CheckBodyType<Beacon>()) {
        if (auto bcn_frame = possible_bcn_frame.CheckLength()) {
            HandleBeacon(bcn_frame.IntoOwned(frame.Take()));
        }
    } else if (auto possible_auth_frame = mgmt_frame.CheckBodyType<Authentication>()) {
        if (auto auth_frame = possible_auth_frame.CheckLength()) {
            HandleAuthentication(auth_frame.IntoOwned(frame.Take()));
        }
    } else if (auto possible_deauth_frame = mgmt_frame.CheckBodyType<Deauthentication>()) {
        if (auto deauth_frame = possible_deauth_frame.CheckLength()) {
            HandleDeauthentication(deauth_frame.IntoOwned(frame.Take()));
        }
    } else if (auto possible_assoc_resp_frame = mgmt_frame.CheckBodyType<AssociationResponse>()) {
        if (auto assoc_resp_frame = possible_assoc_resp_frame.CheckLength()) {
            HandleAssociationResponse(assoc_resp_frame.IntoOwned(frame.Take()));
        }
    } else if (auto possible_disassoc_frame = mgmt_frame.CheckBodyType<Disassociation>()) {
        if (auto disassoc_frame = possible_disassoc_frame.CheckLength()) {
            HandleDisassociation(disassoc_frame.IntoOwned(frame.Take()));
        }
    } else if (auto possible_action_frame = mgmt_frame.CheckBodyType<ActionFrame>()) {
        if (auto action_frame = possible_action_frame.CheckLength()) {
            HandleActionFrame(action_frame.IntoOwned(frame.Take()));
        }
    }

    return ZX_OK;
}

zx_status_t Station::HandleAnyDataFrame(DataFrame<>&& frame) {
    auto data_frame = frame.View();
    if (kFinspectEnabled) { DumpDataFrame(data_frame); }

    WLAN_STATS_INC(data_frame.in);
    if (ShouldDropDataFrame(data_frame)) { return ZX_ERR_NOT_SUPPORTED; }

    if (auto amsdu_frame = data_frame.CheckBodyType<AmsduSubframeHeader>().CheckLength()) {
        HandleAmsduFrame(amsdu_frame.IntoOwned(frame.Take()));
    } else if (auto llc_frame = data_frame.CheckBodyType<LlcHeader>().CheckLength()) {
        HandleDataFrame(llc_frame.IntoOwned(frame.Take()));
    } else if (auto null_frame = data_frame.CheckBodyType<NullDataHdr>().CheckLength()) {
        HandleNullDataFrame(null_frame.IntoOwned(frame.Take()));
    }

    return ZX_OK;
}

zx_status_t Station::HandleMlmeJoinReq(const MlmeMsg<wlan_mlme::JoinRequest>& req) {
    debugfn();

    if (state_ != WlanState::kUnjoined) {
        warnf("already joined; resetting station\n");
        Reset();
    }

    // Clone request to take ownership of the BSS.
    bss_ = wlan_mlme::BSSDescription::New();
    req.body()->selected_bss.Clone(bss_.get());
    bssid_.Set(bss_->bssid.data());

    auto chan = GetBssChan();

    // TODO(NET-449): Move this logic to policy engine
    // Validation and sanitization
    if (!common::IsValidChan(chan)) {
        wlan_channel_t chan_sanitized = chan;
        chan_sanitized.cbw = common::GetValidCbw(chan);
        errorf("Wlanstack attempts to configure an invalid channel: %s. Falling back to %s\n",
               common::ChanStr(chan).c_str(), common::ChanStr(chan_sanitized).c_str());
        chan = chan_sanitized;
    }
    if (IsCbw40RxReady()) {
        // Override with CBW40 support
        wlan_channel_t chan_override = chan;
        chan_override.cbw = CBW40;
        chan_override.cbw = common::GetValidCbw(chan_override);

        infof("CBW40 Rx is ready. Overriding the channel configuration from %s to %s\n",
              common::ChanStr(chan).c_str(), common::ChanStr(chan_override).c_str());
        chan = chan_override;
    }

    debugjoin("setting channel to %s\n", common::ChanStr(chan).c_str());
    zx_status_t status = chan_sched_->SetChannel(chan);

    if (status != ZX_OK) {
        errorf("could not set wlan channel to %s (status %d)\n", common::ChanStr(chan).c_str(),
               status);
        Reset();
        service::SendJoinConfirm(device_, wlan_mlme::JoinResultCodes::JOIN_FAILURE_TIMEOUT);
        return status;
    }

    // Stay on channel to make sure we don't miss the beacon
    chan_sched_->EnsureOnChannel(zx::deadline_after(kOnChannelTimeAfterSend));

    join_chan_ = chan;
    join_timeout_ = deadline_after_bcn_period(req.body()->join_failure_timeout);

    status = timer_->SetTimer(join_timeout_);
    if (status != ZX_OK) {
        errorf("could not set join timer: %d\n", status);
        Reset();
        service::SendJoinConfirm(device_, wlan_mlme::JoinResultCodes::JOIN_FAILURE_TIMEOUT);
    }

    // TODO(hahnr): Update when other BSS types are supported.
    wlan_bss_config_t cfg{
        .bss_type = WLAN_BSS_TYPE_INFRASTRUCTURE,
        .remote = true,
    };
    bssid_.CopyTo(cfg.bssid);
    device_->ConfigureBss(&cfg);
    return status;
}  // namespace wlan

zx_status_t Station::HandleMlmeAuthReq(const MlmeMsg<wlan_mlme::AuthenticateRequest>& req) {
    debugfn();

    if (bss_ == nullptr) { return ZX_ERR_BAD_STATE; }

    // TODO(tkilbourn): better result codes
    common::MacAddr peer_sta_addr(req.body()->peer_sta_address.data());
    if (bssid_ != peer_sta_addr) {
        errorf("cannot authenticate before joining\n");
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::REFUSED);
    }
    if (state_ == WlanState::kUnjoined) {
        errorf("must join before authenticating\n");
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::REFUSED);
    }
    if (state_ != WlanState::kUnauthenticated) {
        warnf("already authenticated; sending request anyway\n");
    }
    if (req.body()->auth_type != wlan_mlme::AuthenticationTypes::OPEN_SYSTEM) {
        // TODO(tkilbourn): support other authentication types
        // TODO(tkilbourn): set the auth_alg_ when we support other authentication types
        errorf("only OpenSystem authentication is supported\n");
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::REFUSED);
    }

    debugjoin("authenticating to %s\n", MACSTR(bssid_));

    MgmtFrame<Authentication> frame;
    auto status = BuildMgmtFrame(&frame);
    if (status != ZX_OK) {
        errorf("authing: failed to build a frame\n");
        return status;
    }

    auto hdr = frame.hdr();
    const common::MacAddr& mymac = device_->GetState()->address();
    hdr->addr1 = bssid_;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid_;
    SetSeqNo(hdr, &seq_);
    frame.FillTxInfo();

    // TODO(tkilbourn): this assumes Open System authentication
    auto auth = frame.body();
    auth->auth_algorithm_number = auth_alg_;
    auth->auth_txn_seq_number = 1;
    auth->status_code = 0;  // Reserved, so set to 0

    finspect("Outbound Mgmt Frame(Auth): %s\n", debug::Describe(*hdr).c_str());
    status = SendNonData(frame.Take());
    if (status != ZX_OK) {
        errorf("could not send auth packet: %d\n", status);
        service::SendAuthConfirm(device_, bssid_, wlan_mlme::AuthenticateResultCodes::REFUSED);
        return status;
    }

    auth_timeout_ = deadline_after_bcn_period(req.body()->auth_failure_timeout);
    status = timer_->SetTimer(auth_timeout_);
    if (status != ZX_OK) {
        errorf("could not set auth timer: %d\n", status);
        // This is the wrong result code, but we need to define our own codes at some later time.
        service::SendAuthConfirm(device_, bssid_,
                                 wlan_mlme::AuthenticateResultCodes::AUTH_FAILURE_TIMEOUT);
        // TODO(tkilbourn): reset the station?
    }
    return status;
}

zx_status_t Station::HandleMlmeDeauthReq(const MlmeMsg<wlan_mlme::DeauthenticateRequest>& req) {
    debugfn();

    if (state_ != WlanState::kAssociated && state_ != WlanState::kAuthenticated) {
        errorf("not associated or authenticated; ignoring deauthenticate request\n");
        return ZX_OK;
    }

    if (bss_ == nullptr) { return ZX_ERR_BAD_STATE; }

    // Check whether the request wants to deauthenticate from this STA's BSS.
    common::MacAddr peer_sta_addr(req.body()->peer_sta_address.data());
    if (bssid_ != peer_sta_addr) { return ZX_OK; }

    MgmtFrame<Deauthentication> frame;
    auto status = BuildMgmtFrame(&frame);
    if (status != ZX_OK) { return status; }

    auto hdr = frame.hdr();
    const common::MacAddr& mymac = device_->GetState()->address();
    hdr->addr1 = bssid_;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid_;
    SetSeqNo(hdr, &seq_);
    frame.FillTxInfo();

    auto deauth = frame.body();
    deauth->reason_code = static_cast<uint16_t>(req.body()->reason_code);

    finspect("Outbound Mgmt Frame(Deauth): %s\n", debug::Describe(*hdr).c_str());
    status = SendNonData(frame.Take());
    if (status != ZX_OK) {
        errorf("could not send deauth packet: %d\n", status);
        // Deauthenticate nevertheless. IEEE isn't clear on what we are supposed to do.
    }

    infof("deauthenticating from %s, reason=%hu\n", bss_->ssid->data(), req.body()->reason_code);

    // TODO(hahnr): Refactor once we have the new state machine.
    state_ = WlanState::kUnauthenticated;
    device_->SetStatus(0);
    controlled_port_ = eapol::PortState::kBlocked;
    service::SendDeauthConfirm(device_, bssid_);

    return ZX_OK;
}

zx_status_t Station::HandleMlmeAssocReq(const MlmeMsg<wlan_mlme::AssociateRequest>& req) {
    debugfn();

    if (bss_ == nullptr) { return ZX_ERR_BAD_STATE; }

    // TODO(tkilbourn): better result codes
    common::MacAddr peer_sta_addr(req.body()->peer_sta_address.data());
    if (bssid_ != peer_sta_addr) {
        errorf("bad peer STA address for association\n");
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::REFUSED);
    }
    if (state_ == WlanState::kUnjoined || state_ == WlanState::kUnauthenticated) {
        errorf("must authenticate before associating\n");
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::REFUSED);
    }
    if (state_ == WlanState::kAssociated) {
        warnf("already authenticated; sending request anyway\n");
    }

    debugjoin("associating to %s\n", MACSTR(bssid_));

    size_t body_payload_len = 128;
    MgmtFrame<AssociationRequest> frame;
    auto status = BuildMgmtFrame(&frame, body_payload_len);
    if (status != ZX_OK) { return status; }

    // TODO(tkilbourn): a lot of this is hardcoded for now. Use device capabilities to set up the
    // request.
    auto hdr = frame.hdr();
    const common::MacAddr& mymac = device_->GetState()->address();
    hdr->addr1 = bssid_;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid_;
    SetSeqNo(hdr, &seq_);
    frame.FillTxInfo();

    auto assoc = frame.body();
    assoc->cap.set_ess(1);
    assoc->cap.set_short_preamble(0);  // For robustness. TODO(porce): Enforce Ralink
    assoc->listen_interval = 0;

    ElementWriter w(assoc->elements,
                    frame.len() - sizeof(MgmtFrameHeader) - sizeof(AssociationRequest));
    if (!w.write<SsidElement>(bss_->ssid->data())) {
        errorf("could not write ssid \"%s\" to association request\n", bss_->ssid->data());
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return ZX_ERR_IO;
    }
    // TODO(tkilbourn): determine these rates based on hardware and the AP
    std::vector<uint8_t> rates = {0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24};

    if (!w.write<SupportedRatesElement>(std::move(rates))) {
        errorf("could not write supported rates\n");
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return ZX_ERR_IO;
    }

    std::vector<uint8_t> ext_rates = {0x30, 0x48, 0x60, 0x6c};
    if (!w.write<ExtendedSupportedRatesElement>(std::move(ext_rates))) {
        errorf("could not write extended supported rates\n");
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return ZX_ERR_IO;
    }

    // Write RSNE from MLME-Association.request if available.
    if (req.body()->rsn) {
        if (!w.write<RsnElement>(req.body()->rsn->data(), req.body()->rsn->size())) {
            return ZX_ERR_IO;
        }
    }

    if (IsHTReady()) {
        HtCapabilities htc{};
        zx_status_t status = BuildHtCapabilities(&htc);
        if (status != ZX_OK) {
            errorf("could not build HtCapabilities. status %d\n", status);
            service::SendAssocConfirm(device_,
                                      wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
            return ZX_ERR_IO;
        }

        if (!w.write<HtCapabilities>(htc.ht_cap_info, htc.ampdu_params, htc.mcs_set, htc.ht_ext_cap,
                                     htc.txbf_cap, htc.asel_cap)) {
            errorf("could not write HtCapabilities\n");
            service::SendAssocConfirm(device_,
                                      wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
            return ZX_ERR_IO;
        }
    }

    // Validate the request in debug mode
    ZX_DEBUG_ASSERT(assoc->Validate(w.size()));

    size_t body_len = sizeof(AssociationRequest) + w.size();
    status = frame.set_body_len(body_len);
    if (status != ZX_OK) {
        errorf("could not set body length to %zu: %d\n", body_len, status);
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return status;
    }

    finspect("Outbound Mgmt Frame (AssocReq): %s\n", debug::Describe(*hdr).c_str());
    status = SendNonData(frame.Take());
    if (status != ZX_OK) {
        errorf("could not send assoc packet: %d\n", status);
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return status;
    }

    // TODO(tkilbourn): get the assoc timeout from somewhere
    assoc_timeout_ = deadline_after_bcn_period(kAssocBcnCountTimeout);
    status = timer_->SetTimer(assoc_timeout_);
    if (status != ZX_OK) {
        errorf("could not set auth timer: %d\n", status);
        // This is the wrong result code, but we need to define our own codes at some later time.
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        // TODO(tkilbourn): reset the station?
    }
    return status;
}

bool Station::ShouldDropMgmtFrame(const MgmtFrameView<>& frame) {
    // Drop management frames if either, there is no BSSID set yet,
    // or the frame is not from the BSS.
    return bssid() == nullptr || *bssid() != frame.hdr()->addr3;
}

// TODO(hahnr): Support ProbeResponses.
zx_status_t Station::HandleBeacon(MgmtFrame<Beacon>&& frame) {
    debugfn();
    ZX_DEBUG_ASSERT(bss_ != nullptr);

    avg_rssi_dbm_.add(dBm(frame.View().rx_info()->rssi_dbm));

    // TODO(tkilbourn): update any other info (like rolling average of rssi)
    last_seen_ = timer_->Now();
    if (join_timeout_ > zx::time()) {
        join_timeout_ = zx::time();
        timer_->CancelTimer();
        state_ = WlanState::kUnauthenticated;
        debugjoin("joined %s\n", bss_->ssid->data());
        return service::SendJoinConfirm(device_, wlan_mlme::JoinResultCodes::SUCCESS);
    }

    auto bcn = frame.body();
    size_t elt_len = frame.body_len() - sizeof(Beacon);
    ElementReader reader(bcn->elements, elt_len);
    while (reader.is_valid()) {
        const ElementHeader* hdr = reader.peek();
        if (hdr == nullptr) break;

        switch (hdr->id) {
        case element_id::kTim: {
            auto tim = reader.read<TimElement>();
            if (tim == nullptr) goto done_iter;

            // Do not process the Beacon's TIM element unless the client is associated.
            if (state_ != WlanState::kAssociated) { continue; }

            if (tim->traffic_buffered(aid_)) { SendPsPoll(); }
            break;
        }
        default:
            reader.skip(sizeof(ElementHeader) + hdr->len);
            break;
        }
    }

done_iter:
    return ZX_OK;
}

zx_status_t Station::HandleAuthentication(MgmtFrame<Authentication>&& frame) {
    debugfn();

    if (state_ != WlanState::kUnauthenticated) {
        // TODO(tkilbourn): should we process this Authentication packet anyway? The spec is
        // unclear.
        debugjoin("unexpected authentication frame\n");
        return ZX_OK;
    }

    auto auth = frame.body();
    if (auth->auth_algorithm_number != auth_alg_) {
        errorf("mismatched authentication algorithm (expected %u, got %u)\n", auth_alg_,
               auth->auth_algorithm_number);
        return ZX_ERR_BAD_STATE;
    }

    // TODO(tkilbourn): this only makes sense for Open System.
    if (auth->auth_txn_seq_number != 2) {
        errorf("unexpected auth txn sequence number (expected 2, got %u)\n",
               auth->auth_txn_seq_number);
        return ZX_ERR_BAD_STATE;
    }

    if (auth->status_code != status_code::kSuccess) {
        errorf("authentication failed (status code=%u)\n", auth->status_code);
        // TODO(tkilbourn): is this the right result code?
        service::SendAuthConfirm(device_, bssid_,
                                 wlan_mlme::AuthenticateResultCodes::AUTHENTICATION_REJECTED);
        auth_timeout_ = zx::time();
        return ZX_ERR_BAD_STATE;
    }

    common::MacAddr bssid(bss_->bssid.data());
    debugjoin("authenticated to %s\n", MACSTR(bssid));
    state_ = WlanState::kAuthenticated;
    auth_timeout_ = zx::time();
    timer_->CancelTimer();
    service::SendAuthConfirm(device_, bssid_, wlan_mlme::AuthenticateResultCodes::SUCCESS);
    return ZX_OK;
}

zx_status_t Station::HandleDeauthentication(MgmtFrame<Deauthentication>&& frame) {
    debugfn();

    if (state_ != WlanState::kAssociated && state_ != WlanState::kAuthenticated) {
        debugjoin("got spurious deauthenticate; ignoring\n");
        return ZX_OK;
    }

    auto deauth = frame.body();
    infof("deauthenticating from %s, reason=%hu\n", bss_->ssid->data(), deauth->reason_code);

    state_ = WlanState::kUnauthenticated;
    device_->SetStatus(0);
    controlled_port_ = eapol::PortState::kBlocked;

    return service::SendDeauthIndication(device_, bssid_,
                                         static_cast<wlan_mlme::ReasonCode>(deauth->reason_code));
}

zx_status_t Station::HandleAssociationResponse(MgmtFrame<AssociationResponse>&& frame) {
    debugfn();

    if (state_ != WlanState::kAuthenticated) {
        // TODO(tkilbourn): should we process this Association response packet anyway? The spec is
        // unclear.
        debugjoin("unexpected association response frame\n");
        return ZX_OK;
    }

    auto assoc = frame.body();
    if (assoc->status_code != status_code::kSuccess) {
        errorf("association failed (status code=%u)\n", assoc->status_code);
        // TODO(tkilbourn): map to the correct result code
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return ZX_ERR_BAD_STATE;
    }

    auto status = SetAssocContext(frame.View());
    if (status != ZX_OK) {
        errorf("failed to set association context (status %d)\n", status);
        service::SendAssocConfirm(device_,
                                  wlan_mlme::AssociateResultCodes::REFUSED_REASON_UNSPECIFIED);
        return ZX_ERR_BAD_STATE;
    }

    // TODO(porce): Move into |assoc_ctx_|
    common::MacAddr bssid(bss_->bssid.data());
    state_ = WlanState::kAssociated;
    assoc_timeout_ = zx::time();
    aid_ = assoc->aid & kAidMask;
    timer_->CancelTimer();
    service::SendAssocConfirm(device_, wlan_mlme::AssociateResultCodes::SUCCESS, aid_);

    signal_report_timeout_ = deadline_after_bcn_period(kSignalReportBcnCountTimeout);
    timer_->SetTimer(signal_report_timeout_);
    avg_rssi_dbm_.reset();
    avg_rssi_dbm_.add(dBm(frame.View().rx_info()->rssi_dbm));
    service::SendSignalReportIndication(device_, common::dBm(frame.View().rx_info()->rssi_dbm));

    // Open port if user connected to an open network.
    if (bss_->rsn.is_null()) {
        debugjoin("802.1X controlled port is now open\n");
        controlled_port_ = eapol::PortState::kOpen;
        device_->SetStatus(ETH_STATUS_ONLINE);
    }

    const common::MacAddr& mymac = device_->GetState()->address();
    infof("NIC %s associated with \"%s\"(%s) in channel %s, %s, %s\n", mymac.ToString().c_str(),
          bss_->ssid->data(), bssid.ToString().c_str(), common::ChanStr(GetJoinChan()).c_str(),
          common::BandStr(GetJoinChan()).c_str(), IsHTReady() ? "802.11n HT" : "802.11g/a");

    // TODO(porce): Time when to establish BlockAck session
    // Handle MLME-level retry, if MAC-level retry ultimately fails
    // Wrap this as EstablishBlockAckSession(peer_mac_addr)
    // Signal to lower MAC for proper session handling
    SendAddBaRequestFrame();
    return ZX_OK;
}

zx_status_t Station::HandleDisassociation(MgmtFrame<Disassociation>&& frame) {
    debugfn();

    if (state_ != WlanState::kAssociated) {
        debugjoin("got spurious disassociate; ignoring\n");
        return ZX_OK;
    }

    auto disassoc = frame.body();
    common::MacAddr bssid(bss_->bssid.data());
    infof("disassociating from %s(%s), reason=%u\n", MACSTR(bssid), bss_->ssid->data(),
          disassoc->reason_code);

    state_ = WlanState::kAuthenticated;
    device_->SetStatus(0);
    controlled_port_ = eapol::PortState::kBlocked;

    signal_report_timeout_ = zx::time();
    timer_->CancelTimer();

    return service::SendDisassociateIndication(device_, bssid, disassoc->reason_code);
}

zx_status_t Station::HandleActionFrame(MgmtFrame<ActionFrame>&& frame) {
    debugfn();

    auto action_frame = frame.View().NextFrame();
    if (auto action_ba_frame = action_frame.CheckBodyType<ActionFrameBlockAck>().CheckLength()) {
        auto ba_frame = action_ba_frame.NextFrame();
        if (auto add_ba_resp_frame = ba_frame.CheckBodyType<AddBaResponseFrame>().CheckLength()) {
            finspect("Inbound ADDBA Resp frame: len %zu\n", add_ba_resp_frame.body_len());
            finspect("  addba resp: %s\n", debug::Describe(*add_ba_resp_frame.body()).c_str());

            // TODO(porce): Handle AddBaResponses and keep the result of negotiation.
        } else if (auto add_ba_req_frame =
                       ba_frame.CheckBodyType<AddBaRequestFrame>().CheckLength()) {
            finspect("Inbound ADDBA Req frame: len %zu\n", add_ba_req_frame.body_len());
            finspect("  addba req: %s\n", debug::Describe(*add_ba_req_frame.body()).c_str());

            return HandleAddBaRequest(*add_ba_req_frame.body());
        }
    }

    return ZX_OK;
}

zx_status_t Station::HandleAddBaRequest(const AddBaRequestFrame& addbareq) {
    debugfn();

    // Construct AddBaResponse frame
    MgmtFrame<ActionFrame> frame;
    size_t payload_len = sizeof(ActionFrameBlockAck) + sizeof(AddBaRequestFrame);
    auto status = BuildMgmtFrame(&frame, payload_len);
    if (status != ZX_OK) { return status; }

    auto hdr = frame.hdr();
    const common::MacAddr& mymac = device_->GetState()->address();
    hdr->addr1 = bssid_;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid_;
    SetSeqNo(hdr, &seq_);
    frame.FillTxInfo();

    auto action_frame = frame.body();
    action_frame->category = ActionFrameBlockAck::ActionCategory();

    auto ba_frame = frame.NextFrame<ActionFrameBlockAck>();
    ba_frame.hdr()->action = AddBaResponseFrame::BlockAckAction();

    auto addbaresp_frame = ba_frame.NextFrame<AddBaResponseFrame>();
    auto addbaresp = addbaresp_frame.hdr();
    addbaresp->dialog_token = addbareq.dialog_token;

    // TODO(porce): Implement DelBa as a response to AddBar for decline

    // Note: Returning AddBaResponse with status_code::kRefused seems ineffective.
    // ArubaAP is persistent not honoring that.
    addbaresp->status_code = status_code::kSuccess;

    // TODO(porce): Query the radio chipset capability to build the response.
    // TODO(NET-567): Use the outcome of the association negotiation
    addbaresp->params.set_amsdu(addbareq.params.amsdu() == 1 && IsAmsduRxReady());
    addbaresp->params.set_policy(BlockAckParameters::kImmediate);
    addbaresp->params.set_tid(addbareq.params.tid());

    // TODO(porce): Once chipset capability is ready, refactor below buffer_size
    // calculation.
    auto buffer_size_ap = addbareq.params.buffer_size();
    constexpr size_t buffer_size_ralink = 64;
    auto buffer_size = (buffer_size_ap <= buffer_size_ralink) ? buffer_size_ap : buffer_size_ralink;
    addbaresp->params.set_buffer_size(buffer_size);
    addbaresp->timeout = addbareq.timeout;

    finspect("Outbound ADDBA Resp frame: len %zu\n", addbaresp_frame.len());
    finspect("Outbound Mgmt Frame(ADDBA Resp): %s\n", debug::Describe(*addbaresp).c_str());

    status = SendNonData(addbaresp_frame.Take());
    if (status != ZX_OK) {
        errorf("could not send AddBaResponse: %d\n", status);
        return status;
    }

    return ZX_OK;
}

bool Station::ShouldDropDataFrame(const DataFrameView<>& frame) {
    if (state_ != WlanState::kAssociated) { return true; }

    return bssid() == nullptr || *bssid() != frame.hdr()->addr2;
}

zx_status_t Station::HandleNullDataFrame(DataFrame<NullDataHdr>&& frame) {
    debugfn();
    ZX_DEBUG_ASSERT(bssid() != nullptr);
    ZX_DEBUG_ASSERT(state_ == WlanState::kAssociated);

    // Take signal strength into account.
    avg_rssi_dbm_.add(dBm(frame.View().rx_info()->rssi_dbm));

    // Some AP's such as Netgear Routers send periodic NULL data frames to test whether a client
    // timed out. The client must respond with a NULL data frame itself to not get
    // deauthenticated.
    SendKeepAliveResponse();
    return ZX_OK;
}

zx_status_t Station::HandleDataFrame(DataFrame<LlcHeader>&& frame) {
    debugfn();
    ZX_DEBUG_ASSERT(bssid() != nullptr);
    ZX_DEBUG_ASSERT(state_ == WlanState::kAssociated);

    // For data frames within BlockAck session.
    if (frame.hdr()->fc.subtype() == DataSubtype::kQosdata) {
        ZX_DEBUG_ASSERT(frame.hdr()->HasQosCtrl());
        ZX_DEBUG_ASSERT(frame.hdr()->qos_ctrl() != nullptr);
    }

    // Take signal strength into account.
    avg_rssi_dbm_.add(dBm(frame.View().rx_info()->rssi_dbm));

    auto hdr = frame.hdr();
    auto llc = frame.body();

    // Forward EAPOL frames to SME.
    size_t payload_len = frame.body_len() - sizeof(LlcHeader);
    if (be16toh(llc->protocol_id) == kEapolProtocolId) {
        if (payload_len < sizeof(EapolFrame)) {
            warnf("short EAPOL frame; len = %zu", payload_len);
            return ZX_OK;
        }
        auto eapol = reinterpret_cast<const EapolFrame*>(llc->payload);
        size_t actual_body_len = payload_len;
        uint16_t expected_body_len = be16toh(eapol->packet_body_length);
        if (actual_body_len >= expected_body_len) {
            return service::SendEapolIndication(device_, *eapol, hdr->addr3, hdr->addr1);
        }
        return ZX_OK;
    }

    // Drop packets if RSNA was not yet established.
    if (controlled_port_ == eapol::PortState::kBlocked) { return ZX_OK; }

    // PS-POLL if there are more buffered unicast frames.
    if (hdr->fc.more_data() && hdr->addr1.IsUcast()) { SendPsPoll(); }

    ZX_DEBUG_ASSERT(frame.body_len() >= sizeof(LlcHeader));
    if (frame.body_len() < sizeof(LlcHeader)) {
        errorf("Inbound LLC frame too short (%zu bytes). Drop.", frame.body_len());
        return ZX_ERR_STOP;
    }
    return HandleLlcFrame(*llc, frame.body_len(), hdr->addr1, hdr->addr3);
}

zx_status_t Station::HandleLlcFrame(const LlcHeader& llc_frame, size_t llc_frame_len,
                                    const common::MacAddr& dest, const common::MacAddr& src) {
    auto llc_payload_len = llc_frame_len - sizeof(LlcHeader);

    finspect("Inbound LLC frame: len %zu\n", llc_frame_len);
    finspect("  llc hdr: %s\n", debug::Describe(llc_frame).c_str());
    auto payload = reinterpret_cast<const uint8_t*>(&llc_frame) + sizeof(LlcHeader);
    finspect("  llc payload: %s\n", debug::HexDump(payload, llc_payload_len).c_str());

    // Prepare a packet
    const size_t eth_len = llc_payload_len + sizeof(EthernetII);
    auto buffer = GetBuffer(eth_len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }
    auto eth_packet = fbl::unique_ptr<Packet>(new Packet(std::move(buffer), eth_len));
    eth_packet->set_peer(Packet::Peer::kEthernet);

    // Construct an Ethernet frame
    auto eth = eth_packet->mut_field<EthernetII>(0);
    eth->dest = dest;
    eth->src = src;
    eth->ether_type = llc_frame.protocol_id;
    std::memcpy(eth->payload, llc_frame.payload, llc_payload_len);

    // Send up
    zx_status_t status = device_->SendEthernet(std::move(eth_packet));
    if (status != ZX_OK) { errorf("could not send ethernet data: %d\n", status); }
    return status;
}

zx_status_t Station::HandleAmsduFrame(DataFrame<AmsduSubframeHeader>&& frame) {
    // TODO(porce): Define A-MSDU or MSDU signature, and avoid forceful conversion.
    debugfn();

    // Non-DMG stations use basic subframe format only.
    auto amsdu_len = frame.body_len();
    if (amsdu_len == 0) { return ZX_OK; }
    finspect("Inbound AMSDU: len %zu\n", amsdu_len);

    // TODO(porce): The received AMSDU should not be greater than max_amsdu_len, specified in
    // HtCapabilities IE of Association. Warn or discard if violated.

    size_t offset = 0;  // Tracks up to which point of byte stream is parsed.
    auto amsdu = reinterpret_cast<const uint8_t*>(frame.body());
    while (offset < amsdu_len) {
        size_t offset_prev = offset;
        auto subframe = reinterpret_cast<const AmsduSubframe*>(amsdu + offset);

        if (!(offset + sizeof(AmsduSubframeHeader) <= amsdu_len)) {
            errorf(
                "malformed A-MSDU subframe: amsdu_len %zu offset %zu offset_prev %zu "
                "AmsduSubframeHeader size: %zu\n",
                amsdu_len, offset, offset_prev, sizeof(AmsduSubframeHeader));
            return ZX_ERR_STOP;
        }

        finspect("amsdu subframe: %s\n", debug::Describe(*subframe).c_str());
        finspect(
            "amsdu subframe dump: %s\n",
            debug::HexDump(reinterpret_cast<const uint8_t*>(subframe), sizeof(AmsduSubframeHeader))
                .c_str());

        offset += sizeof(AmsduSubframeHeader);
        auto msdu_len = subframe->hdr.msdu_len();

        // Note: msdu_len == 0 is valid

        if (msdu_len > 0) {
            if (!(offset + msdu_len <= amsdu_len && msdu_len >= sizeof(LlcHeader))) {
                errorf("malformed A-MSDU subframe: amsdu_len %zu offset %zu msdu_len %u\n",
                       amsdu_len, offset, msdu_len);
                return ZX_ERR_STOP;
            }

            offset += msdu_len;

            // TODO(porce): Process MSDU - Construct an Ethernet frame and send up
            auto llc_frame = subframe->get_msdu();
            HandleLlcFrame(*llc_frame, msdu_len, subframe->hdr.da, subframe->hdr.sa);
        }

        // Skip by zero-padding
        bool is_last_subframe = (offset == amsdu_len);
        offset += subframe->PaddingLen(is_last_subframe);

        ZX_DEBUG_ASSERT(offset > offset_prev);  // Otherwise infinite loop
    }

    return ZX_OK;
}

zx_status_t Station::HandleEthFrame(EthFrame&& frame) {
    debugfn();

    // For now, drop outgoing data frames if we are off channel
    // TODO(NET-1294)
    if (!chan_sched_->OnChannel()) {
        return ZX_OK;
    }

    // Drop Ethernet frames when not associated.
    auto bss_setup = (bssid() != nullptr);
    auto associated = (state_ == WlanState::kAssociated);
    if (!associated) { debugf("dropping eth packet while not associated\n"); }
    if (!bss_setup || !associated) { return ZX_OK; }

    auto eth = frame.hdr();
    const size_t buf_len = kDataFrameHdrLenMax + sizeof(LlcHeader) + frame.body_len();
    auto buffer = GetBuffer(buf_len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }

    wlan_tx_info_t txinfo = {
        // TODO(porce): Determine PHY and CBW based on the association negotiation.
        .tx_flags = 0x0,
        .valid_fields =
            WLAN_TX_INFO_VALID_PHY | WLAN_TX_INFO_VALID_CHAN_WIDTH | WLAN_TX_INFO_VALID_MCS,
        .phy = WLAN_PHY_HT,
        .cbw = CBW20,
        //.date_rate = 0x0,
        .mcs = 0x7,
    };

    auto wlan_packet = fbl::unique_ptr<Packet>(new Packet(std::move(buffer), buf_len));
    // no need to clear the whole packet; we memset the headers instead and copy over all bytes in
    // the payload
    wlan_packet->set_peer(Packet::Peer::kWlan);
    auto hdr = wlan_packet->mut_field<DataFrameHeader>(0);

    bool has_qos_ctrl = IsQosReady();
    bool has_ht_ctrl = false;
    // Set header
    std::memset(hdr, 0, kDataFrameHdrLenMax);
    hdr->fc.set_type(FrameType::kData);
    hdr->fc.set_subtype(has_qos_ctrl ? DataSubtype::kQosdata : DataSubtype::kDataSubtype);
    hdr->fc.set_to_ds(1);
    hdr->fc.set_from_ds(0);
    hdr->fc.set_htc_order(has_ht_ctrl ? 1 : 0);

    // Ensure all outgoing data frames are protected when RSNA is established.
    if (!bss_->rsn.is_null() && controlled_port_ == eapol::PortState::kOpen) {
        hdr->fc.set_protected_frame(1);
        txinfo.tx_flags |= WLAN_TX_INFO_FLAGS_PROTECTED;
    }

    hdr->addr1 = common::MacAddr(bss_->bssid.data());
    hdr->addr2 = eth->src;
    hdr->addr3 = eth->dest;

    // TODO(porce): Construct addr4 field

    if (IsCbw40TxReady()) {
        // Ralink appears to setup BlockAck session AND AMPDU handling
        // TODO(porce): Use a separate sequence number space in that case
        if (hdr->addr3.IsUcast()) {
            // 40MHz direction does not matter here.
            // Radio uses the operational channel setting. This indicates the bandwidth without
            // direction.
            txinfo.cbw = CBW40;
        }
    }

    if (hdr->HasQosCtrl()) {  // QoS Control field
        auto qos_ctrl = hdr->qos_ctrl();
        qos_ctrl->set_tid(GetTid(frame));
        qos_ctrl->set_eosp(0);
        qos_ctrl->set_ack_policy(ack_policy::kNormalAck);

        // AMSDU: set_amsdu_present(1) requires dot11HighthroughputOptionImplemented should be true.
        qos_ctrl->set_amsdu_present(0);
        qos_ctrl->set_byte(0);
    }

    // TODO(porce): Construct htc_order field

    SetSeqNo(hdr, &seq_);

    auto llc = wlan_packet->mut_field<LlcHeader>(hdr->len());
    llc->dsap = kLlcSnapExtension;
    llc->ssap = kLlcSnapExtension;
    llc->control = kLlcUnnumberedInformation;
    std::memcpy(llc->oui, kLlcOui, sizeof(llc->oui));
    llc->protocol_id = eth->ether_type;

    std::memcpy(llc->payload, eth->payload, frame.body_len());

    auto frame_len = hdr->len() + sizeof(LlcHeader) + frame.body_len();
    auto status = wlan_packet->set_len(frame_len);
    if (status != ZX_OK) {
        errorf("could not set data frame length to %zu: %d\n", frame_len, status);
        return status;
    }

    finspect("Outbound data frame: len %zu, hdr_len:%zu body_len:%zu frame_len:%zu\n",
             wlan_packet->len(), hdr->len(), frame.body_len(), frame_len);
    finspect("  wlan hdr: %s\n", debug::Describe(*hdr).c_str());
    finspect("  llc  hdr: %s\n", debug::Describe(*llc).c_str());
    finspect("  frame   : %s\n", debug::HexDump(wlan_packet->data(), frame_len).c_str());

    wlan_packet->CopyCtrlFrom(txinfo);

    status = device_->SendWlan(std::move(wlan_packet));
    if (status != ZX_OK) { errorf("could not send wlan data: %d\n", status); }
    return status;
}

zx_status_t Station::HandleTimeout() {
    debugfn();
    zx::time now = timer_->Now();
    if (join_timeout_ > zx::time() && now > join_timeout_) {
        debugjoin("join timed out; resetting\n");

        Reset();
        return service::SendJoinConfirm(device_, wlan_mlme::JoinResultCodes::JOIN_FAILURE_TIMEOUT);
    }

    if (auth_timeout_ > zx::time() && now >= auth_timeout_) {
        debugjoin("auth timed out; moving back to joining\n");
        auth_timeout_ = zx::time();
        return service::SendAuthConfirm(device_, bssid_,
                                        wlan_mlme::AuthenticateResultCodes::AUTH_FAILURE_TIMEOUT);
    }

    if (assoc_timeout_ > zx::time() && now >= assoc_timeout_) {
        debugjoin("assoc timed out; moving back to authenticated\n");
        assoc_timeout_ = zx::time();
        // TODO(tkilbourn): need a better error code for this
        return service::SendAssocConfirm(device_,
                                         wlan_mlme::AssociateResultCodes::REFUSED_TEMPORARILY);
    }

    if (signal_report_timeout_ > zx::time() && now > signal_report_timeout_ &&
        state_ == WlanState::kAssociated) {
        signal_report_timeout_ = deadline_after_bcn_period(kSignalReportBcnCountTimeout);
        timer_->SetTimer(signal_report_timeout_);
        service::SendSignalReportIndication(device_, common::to_dBm(avg_rssi_dbm_.avg()));
    }

    return ZX_OK;
}

zx_status_t Station::SendKeepAliveResponse() {
    if (state_ != WlanState::kAssociated) {
        warnf("cannot send keep alive response before being associated\n");
        return ZX_OK;
    }

    const common::MacAddr& mymac = device_->GetState()->address();
    size_t len = sizeof(DataFrameHeader);
    fbl::unique_ptr<Buffer> buffer = GetBuffer(len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }

    auto packet = fbl::unique_ptr<Packet>(new Packet(std::move(buffer), len));
    packet->clear();
    packet->set_peer(Packet::Peer::kWlan);
    auto hdr = packet->mut_field<DataFrameHeader>(0);
    hdr->fc.set_type(FrameType::kData);
    hdr->fc.set_subtype(DataSubtype::kNull);
    hdr->fc.set_to_ds(1);

    common::MacAddr bssid(bss_->bssid.data());
    hdr->addr1 = bssid;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid;
    SetSeqNo(hdr, &seq_);

    zx_status_t status = SendNonData(std::move(packet));
    if (status != ZX_OK) {
        errorf("could not send keep alive packet: %d\n", status);
        return status;
    }
    return ZX_OK;
}

zx_status_t Station::SendAddBaRequestFrame() {
    debugfn();

    if (state_ != WlanState::kAssociated) {
        errorf("won't send ADDBA Request in other than Associated state. Current state: %d\n",
               state_);
        return ZX_ERR_BAD_STATE;
    }

    MgmtFrame<ActionFrame> frame;
    size_t payload_len = sizeof(ActionFrameBlockAck) + sizeof(AddBaRequestFrame);
    auto status = BuildMgmtFrame(&frame, payload_len);
    if (status != ZX_OK) { return status; }

    auto hdr = frame.hdr();
    const common::MacAddr& mymac = device_->GetState()->address();
    hdr->addr1 = bssid_;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid_;
    SetSeqNo(hdr, &seq_);
    frame.FillTxInfo();

    auto action_hdr = frame.body();
    action_hdr->category = ActionFrameBlockAck::ActionCategory();

    auto ba_frame = frame.NextFrame<ActionFrameBlockAck>();
    ba_frame.hdr()->action = AddBaRequestFrame::BlockAckAction();

    auto addbareq_frame = ba_frame.NextFrame<AddBaRequestFrame>();
    auto addbareq = addbareq_frame.hdr();

    // It appears there is no particular rule to choose the value for
    // dialog_token. See IEEE Std 802.11-2016, 9.6.5.2.
    addbareq->dialog_token = 0x01;
    addbareq->params.set_amsdu(IsAmsduRxReady());
    addbareq->params.set_policy(BlockAckParameters::BlockAckPolicy::kImmediate);
    addbareq->params.set_tid(GetTid());  // TODO(porce): Communicate this with lower MAC.
    // TODO(porce): Fix the discrepancy of this value from the Ralink's TXWI ba_win_size setting
    addbareq->params.set_buffer_size(64);
    addbareq->timeout = 0;               // Disables the timeout
    addbareq->seq_ctrl.set_fragment(0);  // TODO(porce): Send this down to the lower MAC
    addbareq->seq_ctrl.set_starting_seq(1);

    finspect("Outbound ADDBA Req frame: len %zu\n", addbareq_frame.len());
    finspect("  addba req: %s\n", debug::Describe(*addbareq).c_str());

    status = SendNonData(addbareq_frame.Take());
    if (status != ZX_OK) {
        errorf("could not send AddBaRequest: %d\n", status);
        return status;
    }

    return ZX_OK;
}

zx_status_t Station::HandleMlmeEapolReq(const MlmeMsg<wlan_mlme::EapolRequest>& req) {
    debugfn();

    if (!bss_) { return ZX_ERR_BAD_STATE; }
    if (state_ != WlanState::kAssociated) {
        debugf("dropping MLME-EAPOL.request while not being associated. STA in state %d\n", state_);
        return ZX_OK;
    }

    size_t len = sizeof(DataFrameHeader) + sizeof(LlcHeader) + req.body()->data->size();
    fbl::unique_ptr<Buffer> buffer = GetBuffer(len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }
    auto packet = fbl::unique_ptr<Packet>(new Packet(std::move(buffer), len));
    packet->clear();
    packet->set_peer(Packet::Peer::kWlan);
    auto hdr = packet->mut_field<DataFrameHeader>(0);
    hdr->fc.set_type(FrameType::kData);
    hdr->fc.set_to_ds(1);

    hdr->addr1.Set(req.body()->dst_addr.data());
    hdr->addr2.Set(req.body()->src_addr.data());
    hdr->addr3.Set(req.body()->dst_addr.data());

    SetSeqNo(hdr, &seq_);

    auto llc = packet->mut_field<LlcHeader>(sizeof(DataFrameHeader));
    llc->dsap = kLlcSnapExtension;
    llc->ssap = kLlcSnapExtension;
    llc->control = kLlcUnnumberedInformation;
    std::memcpy(llc->oui, kLlcOui, sizeof(llc->oui));
    llc->protocol_id = htobe16(kEapolProtocolId);
    std::memcpy(llc->payload, req.body()->data->data(), req.body()->data->size());

    zx_status_t status = SendNonData(std::move(packet));
    if (status != ZX_OK) {
        errorf("could not send eapol request packet: %d\n", status);
        service::SendEapolConfirm(device_, wlan_mlme::EapolResultCodes::TRANSMISSION_FAILURE);
        return status;
    }

    service::SendEapolConfirm(device_, wlan_mlme::EapolResultCodes::SUCCESS);

    return status;
}

zx_status_t Station::HandleMlmeSetKeysReq(const MlmeMsg<wlan_mlme::SetKeysRequest>& req) {
    debugfn();

    for (auto& keyDesc : *req.body()->keylist) {
        if (keyDesc.key.is_null()) { return ZX_ERR_NOT_SUPPORTED; }

        uint8_t key_type;
        switch (keyDesc.key_type) {
        case wlan_mlme::KeyType::PAIRWISE:
            key_type = WLAN_KEY_TYPE_PAIRWISE;
            break;
        case wlan_mlme::KeyType::PEER_KEY:
            key_type = WLAN_KEY_TYPE_PEER;
            break;
        case wlan_mlme::KeyType::IGTK:
            key_type = WLAN_KEY_TYPE_IGTK;
            break;
        default:
            key_type = WLAN_KEY_TYPE_GROUP;
            break;
        }

        wlan_key_config_t key_config = {};
        memcpy(key_config.key, keyDesc.key->data(), keyDesc.key->size());
        key_config.key_type = key_type;
        key_config.key_len = static_cast<uint8_t>(keyDesc.key->size());
        key_config.key_idx = keyDesc.key_id;
        key_config.protection = WLAN_PROTECTION_RX_TX;
        key_config.cipher_type = keyDesc.cipher_suite_type;
        memcpy(key_config.cipher_oui, keyDesc.cipher_suite_oui.data(),
               sizeof(key_config.cipher_oui));
        memcpy(key_config.peer_addr, keyDesc.address.data(), sizeof(key_config.peer_addr));

        auto status = device_->SetKey(&key_config);
        if (status != ZX_OK) {
            errorf("Could not configure keys in hardware: %d\n", status);
            return status;
        }
    }

    // Once keys have been successfully configured, open controlled port and report link up
    // status.
    // TODO(hahnr): This is a very simplified assumption and we might need a little more logic to
    // correctly track the port's state.
    controlled_port_ = eapol::PortState::kOpen;
    device_->SetStatus(ETH_STATUS_ONLINE);
    return ZX_OK;
}

void Station::PreSwitchOffChannel() {
    debugfn();
    if (state_ == WlanState::kAssociated) {
        SetPowerManagementMode(true);
    }
}

void Station::BackToMainChannel() {
    debugfn();
    if (state_ == WlanState::kAssociated) {
        SetPowerManagementMode(false);
    }
}

void Station::DumpDataFrame(const DataFrameView<>& frame) {
    // TODO(porce): Should change the API signature to MSDU
    const common::MacAddr& mymac = device_->GetState()->address();

    auto hdr = frame.hdr();

    auto is_ucast_to_self = mymac == hdr->addr1;
    auto is_mcast = hdr->addr1.IsBcast();
    auto is_bcast = hdr->addr1.IsMcast();
    auto is_interesting = is_ucast_to_self || is_mcast || is_bcast;

    auto associated = (state_ == WlanState::kAssociated);
    auto from_bss = (bssid() != nullptr && *bssid() == hdr->addr2);
    if (associated) { is_interesting = is_interesting && from_bss; }

    if (!is_interesting) { return; }

    auto msdu = frame.body()->data;
    finspect("Inbound data frame: len %zu\n", frame.len());
    finspect("  wlan hdr: %s\n", debug::Describe(*hdr).c_str());
    finspect("  msdu    : %s\n", debug::HexDump(msdu, frame.body_len()).c_str());
}

zx_status_t Station::SendNonData(fbl::unique_ptr<Packet> packet) {
    chan_sched_->EnsureOnChannel(zx::deadline_after(kOnChannelTimeAfterSend));
    return device_->SendWlan(fbl::move(packet));
}

zx_status_t Station::SetPowerManagementMode(bool ps_mode) {
    if (state_ != WlanState::kAssociated) {
        warnf("cannot adjust power management before being associated\n");
        return ZX_OK;
    }

    const common::MacAddr& mymac = device_->GetState()->address();
    size_t len = sizeof(DataFrameHeader);
    fbl::unique_ptr<Buffer> buffer = GetBuffer(len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }
    auto packet = fbl::unique_ptr<Packet>(new Packet(std::move(buffer), len));
    packet->clear();
    packet->set_peer(Packet::Peer::kWlan);
    auto hdr = packet->mut_field<DataFrameHeader>(0);
    hdr->fc.set_type(FrameType::kData);
    hdr->fc.set_subtype(DataSubtype::kNull);
    hdr->fc.set_pwr_mgmt(ps_mode);
    hdr->fc.set_to_ds(1);

    common::MacAddr bssid(bss_->bssid.data());
    hdr->addr1 = bssid;
    hdr->addr2 = mymac;
    hdr->addr3 = bssid;

    SetSeqNo(hdr, &seq_);

    zx_status_t status = device_->SendWlan(std::move(packet));
    if (status != ZX_OK) {
        errorf("could not send power management packet: %d\n", status);
        return status;
    }
    return ZX_OK;
}

zx_status_t Station::SendPsPoll() {
    // TODO(hahnr): We should probably wait for an RSNA if the network is an
    // RSN. Else we cannot work with the incoming data frame.
    if (state_ != WlanState::kAssociated) {
        warnf("cannot send ps-poll before being associated\n");
        return ZX_OK;
    }

    const common::MacAddr& mymac = device_->GetState()->address();
    size_t len = sizeof(CtrlFrameHdr) + sizeof(PsPollFrame);
    fbl::unique_ptr<Buffer> buffer = GetBuffer(len);
    if (buffer == nullptr) { return ZX_ERR_NO_RESOURCES; }
    auto packet = fbl::make_unique<Packet>(std::move(buffer), len);
    packet->clear();
    packet->set_peer(Packet::Peer::kWlan);

    CtrlFrame<PsPollFrame> frame(std::move(packet));
    ZX_DEBUG_ASSERT(frame.HasValidLen());
    frame.hdr()->fc.set_type(FrameType::kControl);
    frame.hdr()->fc.set_subtype(ControlSubtype::kPsPoll);
    frame.body()->aid = aid_;
    frame.body()->bssid = common::MacAddr(bss_->bssid.data());
    frame.body()->ta = mymac;

    zx_status_t status = SendNonData(frame.Take());
    if (status != ZX_OK) {
        errorf("could not send power management packet: %d\n", status);
        return status;
    }
    return ZX_OK;
}

zx::time Station::deadline_after_bcn_period(size_t bcn_count) {
    ZX_DEBUG_ASSERT(bss_ != nullptr);
    return timer_->Now() + WLAN_TU(bss_->beacon_period * bcn_count);
}

bool Station::IsHTReady() const {
    // TODO(porce): Test capabilites and configurations of the client and its BSS.
    return true;
}

bool Station::IsCbw40RxReady() const {
    // TODO(porce): Test capabilites and configurations of the client and its BSS.
    return true;
}

bool Station::IsCbw40TxReady() const {
    // TODO(porce): Test capabilites and configurations of the client and its BSS.
    // TODO(porce): Ralink dependency on BlockAck, AMPDU handling
    return false;
}

bool Station::IsQosReady() const {
    // TODO(NET-567,NET-599): Determine for each outbound data frame,
    // given the result of the dynamic capability negotiation, data frame
    // classification, and QoS policy.

    // Aruba / Ubiquiti are confirmed to be compatible with QoS field for the BlockAck session,
    // independently of 40MHz operation.
    return true;
}

bool Station::IsAmsduRxReady() const {
    // [Interop]
    // IEEE Std 802.11-2016 9.4.1.14's wording is ambiguous, and it can cause interop issue.
    // In particular, a peer may tear off BlockAck session if interpretation of the field
    // "A-MSDU Supported" in Block Ack Parameter set of ADDBA Request and Response is different.
    // Declare such that Fuchsia "can do" AMSDU. This hints the peer that
    // peer may assume that this Fuchsia device can process inbound A-MSDU data frame.
    // Since the presence of A-MSDU frame is indicated in the "amsdu_present" field of
    // QoS field in MPDU header, and the use of A-MSDU frame is optional in flight-time,
    // setting "A-MSDU Supported" both in ADDBA Request and Response is deemed to be most
    // interoperable way.
    return true;
}

zx_status_t Station::BuildMcsSet(SupportedMcsSet* mcs_set) const {
    //  Retrieve client wlanphy capability
    const wlan_info_t& ifc_info = device_->GetWlanInfo().ifc_info;
    auto is_5ghz = common::Is5Ghz(join_chan_);

    auto bi = FindBand(ifc_info, is_5ghz);
    if (bi == nullptr) {
        errorf("failed to find band info from wlan info for band %s\n",
               is_5ghz ? "5 GHz" : "2 GHz");
        return ZX_ERR_INTERNAL;
    }

    *mcs_set = *reinterpret_cast<const SupportedMcsSet*>(&bi->ht_caps.supported_mcs_set);
    return ZX_OK;
}

zx_status_t Station::BuildHtCapabilities(HtCapabilities* htc) const {
    // TODO(porce): Determine which value to use for each field
    // (a) client radio capabilities, as reported by device driver
    // (b) intersection of (a) and radio configurations
    // (c) intersection of (b) and BSS capabilities
    // (d) intersection of (c) and radio configuration

    ZX_DEBUG_ASSERT(htc != nullptr);
    if (htc == nullptr) { return ZX_ERR_INVALID_ARGS; }

    // Static cooking for Proof-of-Concept
    HtCapabilityInfo& hci = htc->ht_cap_info;

    hci.set_ldpc_coding_cap(0);  // Ralink RT5370 is incapable of LDPC.

    if (IsCbw40RxReady()) {
        hci.set_chan_width_set(HtCapabilityInfo::TWENTY_FORTY);
    } else {
        hci.set_chan_width_set(HtCapabilityInfo::TWENTY_ONLY);
    }

    hci.set_sm_power_save(HtCapabilityInfo::DISABLED);
    hci.set_greenfield(0);
    hci.set_short_gi_20(1);
    hci.set_short_gi_40(1);
    hci.set_tx_stbc(0);  // No plan to support STBC Tx
    hci.set_rx_stbc(1);  // one stream.
    hci.set_delayed_block_ack(0);

    // TODO(NET-599): Reflect the chipset capability and USB read size in negotiation
    hci.set_max_amsdu_len(HtCapabilityInfo::OCTETS_3839);
    hci.set_dsss_in_40(0);
    hci.set_intolerant_40(0);
    hci.set_lsig_txop_protect(0);

    AmpduParams& ampdu = htc->ampdu_params;
    ampdu.set_exponent(3);                                // 65535 bytes
    ampdu.set_min_start_spacing(AmpduParams::FOUR_USEC);  // Aruba
    // ampdu.set_min_start_spacing(AmpduParams::EIGHT_USEC);  // TP-Link
    // ampdu.set_min_start_spacing(AmpduParams::SIXTEEN_USEC);

    zx_status_t status = BuildMcsSet(&htc->mcs_set);
    if (status != ZX_OK) { return status; }

    HtExtCapabilities& hec = htc->ht_ext_cap;
    hec.set_pco(0);
    hec.set_pco_transition(HtExtCapabilities::PCO_RESERVED);
    hec.set_mcs_feedback(HtExtCapabilities::MCS_NOFEEDBACK);
    hec.set_htc_ht_support(0);
    hec.set_rd_responder(0);

    TxBfCapability& txbf = htc->txbf_cap;
    txbf.set_implicit_rx(0);
    txbf.set_rx_stag_sounding(0);
    txbf.set_tx_stag_sounding(0);
    txbf.set_rx_ndp(0);
    txbf.set_tx_ndp(0);
    txbf.set_implicit(0);
    txbf.set_calibration(TxBfCapability::CALIBRATION_NONE);
    txbf.set_csi(0);
    txbf.set_noncomp_steering(0);
    txbf.set_comp_steering(0);
    txbf.set_csi_feedback(TxBfCapability::FEEDBACK_NONE);
    txbf.set_noncomp_feedback(TxBfCapability::FEEDBACK_NONE);
    txbf.set_comp_feedback(TxBfCapability::FEEDBACK_NONE);
    txbf.set_min_grouping(TxBfCapability::MIN_GROUP_ONE);
    txbf.set_csi_antennas_human(1);           // 1 antenna
    txbf.set_noncomp_steering_ants_human(1);  // 1 antenna
    txbf.set_comp_steering_ants_human(1);     // 1 antenna
    txbf.set_csi_rows_human(1);               // 1 antenna
    txbf.set_chan_estimation_human(1);        // # space-time stream

    AselCapability& asel = htc->asel_cap;
    asel.set_asel(0);
    asel.set_csi_feedback_tx_asel(0);
    asel.set_explicit_csi_feedback(0);
    asel.set_antenna_idx_feedback(0);
    asel.set_rx_asel(0);
    asel.set_tx_sounding_ppdu(0);

    return ZX_OK;
}

uint8_t Station::GetTid() {
    // IEEE Std 802.11-2016, 3.1(Traffic Identifier), 5.1.1.1 (Data Service - General), 9.4.2.30
    // (Access Policy), 9.2.4.5.2 (TID subfield) Related topics: QoS facility, TSPEC, WM, QMF, TXOP.
    // A TID is from [0, 15], and is assigned to an MSDU in the layers above the MAC.
    // [0, 7] identify Traffic Categories (TCs)
    // [8, 15] identify parameterized Traffic Streams (TSs).

    // TODO(NET-599): Implement QoS policy engine.
    return 0;
}

uint8_t Station::GetTid(const EthFrame& frame) {
    return GetTid();
}

zx_status_t Station::SetAssocContext(const MgmtFrameView<AssociationResponse>& frame) {
    assoc_ctx_ = AssocContext{};
    assoc_ctx_.ts_start = zx::time();
    assoc_ctx_.bssid = common::MacAddr(bss_->bssid.data());
    assoc_ctx_.aid = frame.body()->aid & kAidMask;

    AssocContext ap{};
    ap.cap = frame.body()->cap;

    auto ie_chains = frame.body()->elements;
    size_t ie_chains_len = frame.body_len() - frame.body()->len();
    auto status = ParseAssocRespIe(ie_chains, ie_chains_len, &ap);
    if (status != ZX_OK) {
        debugf("failed to parse AssocResp. status %d\n", status);
        return status;
    }
    debugjoin("rxed AssocResp:[%s]\n", debug::Describe(ap).c_str());

    auto ifc_info = device_->GetWlanInfo().ifc_info;
    auto client = ToAssocContext(ifc_info, join_chan_);
    debugjoin("from WlanInfo: [%s]\n", debug::Describe(client).c_str());

    assoc_ctx_.cap = IntersectCapInfo(ap.cap, client.cap);
    SetAssocCtxSuppRates(ap, client, &assoc_ctx_.supported_rates, &assoc_ctx_.ext_supported_rates);

    assoc_ctx_.has_ht_cap = ap.has_ht_cap && client.has_ht_cap;
    if (assoc_ctx_.has_ht_cap) {
        // TODO(porce): Supported MCS Set field from the outcome of the intersection
        // requires the conditional treatment depending on the value of the following fields:
        // - "Tx MCS Set Defined"
        // - "Tx Rx MCS Set Not Equal"
        // - "Tx Maximum Number Spatial Streams Supported"
        // - "Tx Unequal Modulation Supported"
        assoc_ctx_.ht_cap = IntersectHtCap(ap.ht_cap, client.ht_cap);
        assoc_ctx_.has_ht_op = ap.has_ht_op;
        if (assoc_ctx_.has_ht_op) { assoc_ctx_.ht_op = ap.ht_op; }
    }
    assoc_ctx_.has_vht_cap = ap.has_vht_cap && client.has_vht_cap;
    if (assoc_ctx_.has_vht_cap) {
        assoc_ctx_.vht_cap = IntersectVhtCap(ap.vht_cap, client.vht_cap);
        assoc_ctx_.has_vht_cap = ap.has_vht_op;
        if (assoc_ctx_.has_vht_op) { assoc_ctx_.vht_op = ap.vht_op; }
    }
    debugjoin("final AssocCtx:[%s]\n", debug::Describe(assoc_ctx_).c_str());

    return ZX_OK;
}

wlan_stats::ClientMlmeStats Station::stats() const {
    return stats_.ToFidl();
}

const wlan_band_info_t* FindBand(const wlan_info_t& ifc_info, bool is_5ghz) {
    ZX_DEBUG_ASSERT(ifc_info.num_bands <= WLAN_MAX_BANDS);

    for (uint8_t idx = 0; idx < ifc_info.num_bands; idx++) {
        auto bi = &ifc_info.bands[idx];
        auto base_freq = bi->supported_channels.base_freq;

        if (is_5ghz && base_freq == common::kBaseFreq5Ghz) {
            return bi;
        } else if (!is_5ghz && base_freq == common::kBaseFreq2Ghz) {
            return bi;
        }
    }

    return nullptr;
}

// TODO(NET-1287): Refactor together with Bss::ParseIE()
zx_status_t ParseAssocRespIe(const uint8_t* ie_chains, size_t ie_chains_len,
                             AssocContext* assoc_ctx) {
    ZX_DEBUG_ASSERT(assoc_ctx != nullptr);

    ElementReader reader(ie_chains, ie_chains_len);
    while (reader.is_valid()) {
        const ElementHeader* hdr = reader.peek();
        if (hdr == nullptr) { break; }

        switch (hdr->id) {
        case element_id::kSuppRates: {
            auto ie = reader.read<SupportedRatesElement>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            for (uint8_t i = 0; i < ie->hdr.len; i++) {
                assoc_ctx->supported_rates.push_back(ie->rates[i]);
            }
            break;
        }
        case element_id::kExtSuppRates: {
            auto ie = reader.read<ExtendedSupportedRatesElement>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            for (uint8_t i = 0; i < ie->hdr.len; i++) {
                assoc_ctx->ext_supported_rates.push_back(ie->rates[i]);
            }
            break;
        }
        case element_id::kHtCapabilities: {
            auto ie = reader.read<HtCapabilities>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            assoc_ctx->has_ht_cap = true;
            assoc_ctx->ht_cap = *ie;
            break;
        }
        case element_id::kHtOperation: {
            auto ie = reader.read<HtOperation>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            assoc_ctx->has_ht_op = true;
            assoc_ctx->ht_op = *ie;
            break;
        }
        case element_id::kVhtCapabilities: {
            auto ie = reader.read<VhtCapabilities>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            assoc_ctx->has_vht_cap = true;
            assoc_ctx->vht_cap = *ie;
            break;
        }
        case element_id::kVhtOperation: {
            auto ie = reader.read<VhtOperation>();
            if (ie == nullptr) { return ZX_ERR_INTERNAL; }
            assoc_ctx->has_vht_op = true;
            assoc_ctx->vht_op = *ie;
            break;
        }
        default:
            reader.skip(sizeof(ElementHeader) + hdr->len);
            break;
        }
    }

    return ZX_OK;
}

AssocContext ToAssocContext(const wlan_info_t& ifc_info, const wlan_channel_t join_chan) {
    AssocContext assoc_ctx{};

    assoc_ctx.cap = *reinterpret_cast<const CapabilityInfo*>(&ifc_info.caps);

    auto band_info = FindBand(ifc_info, common::Is5Ghz(join_chan));

    for (uint8_t rate : band_info->basic_rates) {
        if (rate == 0) { break; }  // basic_rates has fixed-length and is "null-terminated".
        // Not using ext_supported_rates because this assoc_ctx is temporary
        assoc_ctx.supported_rates.push_back(rate);
    }

    if (ifc_info.supported_phys & WLAN_PHY_HT) {
        assoc_ctx.has_ht_cap = true;
        static_assert(sizeof(HtCapabilities) == sizeof(wlan_ht_caps_t) + sizeof(ElementHeader),
                      "HtCap size mimatch between IE and DDK");
        auto elem = reinterpret_cast<uint8_t*>(&assoc_ctx.ht_cap);
        memcpy(elem + sizeof(ElementHeader), &band_info->ht_caps, sizeof(wlan_ht_caps_t));
    }

    if (band_info->vht_supported) {
        assoc_ctx.has_vht_cap = true;
        static_assert(sizeof(VhtCapabilities) == sizeof(wlan_vht_caps_t) + sizeof(ElementHeader),
                      "VhtCap size mimatch between IE and DDK");
        auto elem = reinterpret_cast<uint8_t*>(&assoc_ctx.vht_cap);
        memcpy(elem + sizeof(ElementHeader), &band_info->vht_caps, sizeof(wlan_vht_caps_t));
    }

    return assoc_ctx;
}

void SetAssocCtxSuppRates(const AssocContext& ap, const AssocContext& client,
                          std::vector<uint8_t>* supp_rates, std::vector<uint8_t>* ext_rates) {
    auto cbegin1 = ap.supported_rates.cbegin();
    auto cend1 = ap.supported_rates.cend();
    auto cbegin2 = ap.ext_supported_rates.cbegin();
    auto cend2 = ap.ext_supported_rates.cend();
    std::vector<uint8_t> ap_rates;
    std::set_union(cbegin1, cend1, cbegin2, cend2, std::back_inserter(ap_rates));

    *supp_rates = IntersectRatesAp(ap_rates, client.supported_rates);

    // SupportedRates Element can hold at most 8 rates. The rest go to ExtSupportedRates
    if (supp_rates->size() > SupportedRatesElement::kMaxLen) {
        std::move(supp_rates->cbegin() + SupportedRatesElement::kMaxLen, supp_rates->cend(),
                  std::back_inserter(*ext_rates));
        supp_rates->resize(SupportedRatesElement::kMaxLen);
    }
}

}  // namespace wlan
