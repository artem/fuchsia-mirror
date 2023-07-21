// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sim-frame.h"

#include <fuchsia/wlan/common/c/banjo.h>

namespace wlan::simulation {

/* InformationElement function implementations.*/
InformationElement::~InformationElement() = default;

// SsidInformationElement function implementations.
SsidInformationElement::SsidInformationElement(
    const wlan::simulation::SsidInformationElement& ssid_ie) {
  ssid_.len = ssid_ie.ssid_.len;
  std::memcpy(ssid_.data.data(), ssid_ie.ssid_.data.data(), ssid_.len);
}

InformationElement::SimIeType SsidInformationElement::IeType() const { return IE_TYPE_SSID; }

std::vector<uint8_t> SsidInformationElement::ToRawIe() const {
  std::vector<uint8_t> buf = {IE_TYPE_SSID, ssid_.len};
  for (int i = 0; i < ssid_.len; ++i) {
    buf.push_back(ssid_.data.data()[i]);
  }
  return buf;
}

SsidInformationElement::~SsidInformationElement() = default;

// CsaInformationElement function implementations.
CsaInformationElement::CsaInformationElement(
    const wlan::simulation::CsaInformationElement& csa_ie) {
  channel_switch_mode_ = csa_ie.channel_switch_mode_;
  new_channel_number_ = csa_ie.new_channel_number_;
  channel_switch_count_ = csa_ie.channel_switch_count_;
}

InformationElement::SimIeType CsaInformationElement::IeType() const { return IE_TYPE_CSA; }

std::vector<uint8_t> CsaInformationElement::ToRawIe() const {
  const uint8_t csa_len = 3;  // CSA variable is 3 bytes long: CSM + NCN + CSC.
  std::vector<uint8_t> buf = {IE_TYPE_CSA, csa_len,
                              static_cast<uint8_t>(channel_switch_mode_ ? 1 : 0),
                              new_channel_number_, channel_switch_count_};
  return buf;
}

CsaInformationElement::~CsaInformationElement() = default;

/* SimFrame function implementations.*/
SimFrame::~SimFrame() = default;

/* SimManagementFrame function implementations.*/
SimManagementFrame::SimManagementFrame(const SimManagementFrame& mgmt_frame) {
  src_addr_ = mgmt_frame.src_addr_;
  dst_addr_ = mgmt_frame.dst_addr_;
  sec_proto_type_ = mgmt_frame.sec_proto_type_;

  for (const auto& ie : mgmt_frame.IEs_) {
    switch (ie->IeType()) {
      case SsidInformationElement::IE_TYPE_SSID:
        IEs_.push_back(std::make_shared<SsidInformationElement>(
            *(std::static_pointer_cast<SsidInformationElement>(ie))));
        break;
      case CsaInformationElement::IE_TYPE_CSA:
        IEs_.push_back(std::make_shared<CsaInformationElement>(
            *(std::static_pointer_cast<CsaInformationElement>(ie))));
        break;
      default:;
    }
  }
  raw_ies_ = mgmt_frame.raw_ies_;
}

SimManagementFrame::~SimManagementFrame() = default;

SimFrame::SimFrameType SimManagementFrame::FrameType() const { return FRAME_TYPE_MGMT; }

std::shared_ptr<InformationElement> SimManagementFrame::FindIe(
    InformationElement::SimIeType ie_type) const {
  for (const auto& ie : IEs_) {
    if (ie->IeType() == ie_type) {
      return ie;
    }
  }

  return std::shared_ptr<InformationElement>(nullptr);
}

void SimManagementFrame::AddSsidIe(const wlan_ieee80211::CSsid& ssid) {
  auto ie = std::make_shared<SsidInformationElement>(ssid);
  // Ensure no IE with this IE type exists.
  AddIe(InformationElement::IE_TYPE_SSID, ie);
}

void SimManagementFrame::AddCsaIe(const wlan_common::WlanChannel& channel,
                                  uint8_t channel_switch_count) {
  // for nonmesh STAs, this field either is set to the number of TBTTs until the STA sending the
  // Channel Switch Announcement element switches to the new channel or is set to 0.
  auto ie = std::make_shared<CsaInformationElement>(false, channel.primary, channel_switch_count);
  // Ensure no IE with this IE type exist
  AddIe(InformationElement::IE_TYPE_CSA, ie);
}

void SimManagementFrame::AddRawIes(cpp20::span<const uint8_t> raw_ies) {
  raw_ies_.insert(raw_ies_.end(), raw_ies.begin(), raw_ies.end());
}

void SimManagementFrame::AddIe(InformationElement::SimIeType ie_type,
                               std::shared_ptr<InformationElement> ie) {
  if (FindIe(ie_type)) {
    RemoveIe(ie_type);
  }
  IEs_.push_back(ie);
}

void SimManagementFrame::RemoveIe(InformationElement::SimIeType ie_type) {
  for (auto it = IEs_.begin(); it != IEs_.end();) {
    if ((*it)->IeType() == ie_type) {
      it = IEs_.erase(it);
    } else {
      it++;
    }
  }
}

/* SimBeaconFrame function implementations.*/
SimBeaconFrame::SimBeaconFrame(const wlan_ieee80211::CSsid& ssid, const common::MacAddr& bssid)
    : bssid_(bssid) {
  // Beacon automatically gets the SSID information element.
  AddSsidIe(ssid);
}

SimBeaconFrame::SimBeaconFrame(const SimBeaconFrame& beacon) : SimManagementFrame(beacon) {
  bssid_ = beacon.bssid_;
  interval_ = beacon.interval_;
  capability_info_ = beacon.capability_info_;
  // IEs are copied by SimManagementFrame copy constructor.
}

SimBeaconFrame::~SimBeaconFrame() = default;
SimManagementFrame::SimMgmtFrameType SimBeaconFrame::MgmtFrameType() const {
  return FRAME_TYPE_BEACON;
}

SimFrame* SimBeaconFrame::CopyFrame() const { return new SimBeaconFrame(*this); }

/* SimProbeReqFrame function implementations.*/
SimProbeReqFrame::SimProbeReqFrame(const SimProbeReqFrame& probe_req)
    : SimManagementFrame(probe_req) {}

SimProbeReqFrame::~SimProbeReqFrame() = default;
SimManagementFrame::SimMgmtFrameType SimProbeReqFrame::MgmtFrameType() const {
  return FRAME_TYPE_PROBE_REQ;
}

SimFrame* SimProbeReqFrame::CopyFrame() const { return new SimProbeReqFrame(*this); }

/* SimProbeRespFrame function implementations.*/
SimProbeRespFrame::SimProbeRespFrame(const common::MacAddr& src, const common::MacAddr& dst,
                                     const wlan_ieee80211::CSsid& ssid)
    : SimManagementFrame(src, dst) {
  // Probe response automatically gets the SSID information element.
  AddSsidIe(ssid);
}

SimProbeRespFrame::SimProbeRespFrame(const SimProbeRespFrame& probe_resp)
    : SimManagementFrame(probe_resp) {
  capability_info_ = probe_resp.capability_info_;
  // IEs are copied by SimManagementFrame copy constructor.
}

SimProbeRespFrame::~SimProbeRespFrame() = default;
SimManagementFrame::SimMgmtFrameType SimProbeRespFrame::MgmtFrameType() const {
  return FRAME_TYPE_PROBE_RESP;
}

SimFrame* SimProbeRespFrame::CopyFrame() const { return new SimProbeRespFrame(*this); }

/* SimAssocReqFrame function implementations.*/
SimAssocReqFrame::SimAssocReqFrame(const SimAssocReqFrame& assoc_req)
    : SimManagementFrame(assoc_req) {
  bssid_ = assoc_req.bssid_;
  ssid_ = assoc_req.ssid_;
}

SimAssocReqFrame::~SimAssocReqFrame() = default;
SimManagementFrame::SimMgmtFrameType SimAssocReqFrame::MgmtFrameType() const {
  return FRAME_TYPE_ASSOC_REQ;
}

SimFrame* SimAssocReqFrame::CopyFrame() const { return new SimAssocReqFrame(*this); }

/* SimAssocRespFrame function implementations.*/
SimAssocRespFrame::SimAssocRespFrame(const SimAssocRespFrame& assoc_resp)
    : SimManagementFrame(assoc_resp) {
  status_ = assoc_resp.status_;
  capability_info_ = assoc_resp.capability_info_;
}

SimAssocRespFrame::~SimAssocRespFrame() = default;
SimManagementFrame::SimMgmtFrameType SimAssocRespFrame::MgmtFrameType() const {
  return FRAME_TYPE_ASSOC_RESP;
}

SimFrame* SimAssocRespFrame::CopyFrame() const { return new SimAssocRespFrame(*this); }

/* SimDisassocReqFrame function implementations.*/
SimDisassocReqFrame::SimDisassocReqFrame(const SimDisassocReqFrame& disassoc_req)
    : SimManagementFrame(disassoc_req) {
  reason_ = disassoc_req.reason_;
}

SimDisassocReqFrame::~SimDisassocReqFrame() = default;
SimManagementFrame::SimMgmtFrameType SimDisassocReqFrame::MgmtFrameType() const {
  return FRAME_TYPE_DISASSOC_REQ;
}

SimFrame* SimDisassocReqFrame::CopyFrame() const { return new SimDisassocReqFrame(*this); }

/* SimAuthFrame function implementations.*/
SimAuthFrame::SimAuthFrame(const SimAuthFrame& auth) : SimManagementFrame(auth) {
  seq_num_ = auth.seq_num_;
  auth_type_ = auth.auth_type_;
  status_ = auth.status_;
  payload_ = auth.payload_;
}

SimAuthFrame::~SimAuthFrame() = default;
SimManagementFrame::SimMgmtFrameType SimAuthFrame::MgmtFrameType() const { return FRAME_TYPE_AUTH; }

SimFrame* SimAuthFrame::CopyFrame() const { return new SimAuthFrame(*this); }

void SimAuthFrame::AddChallengeText(cpp20::span<const uint8_t> text) {
  // Clear the existing challenge text before setting new one.
  payload_.clear();
  payload_.insert(payload_.begin(), text.begin(), text.end());
}

/* SimDeauthFrame function implementations.*/
SimDeauthFrame::SimDeauthFrame(const SimDeauthFrame& deauth) : SimManagementFrame(deauth) {
  reason_ = deauth.reason_;
}

SimDeauthFrame::~SimDeauthFrame() = default;
SimManagementFrame::SimMgmtFrameType SimDeauthFrame::MgmtFrameType() const {
  return FRAME_TYPE_DEAUTH;
}

SimFrame* SimDeauthFrame::CopyFrame() const { return new SimDeauthFrame(*this); }

/* SimReassocReqFrame function implementations.*/
SimReassocReqFrame::SimReassocReqFrame(const SimReassocReqFrame& reassoc_req)
    : SimManagementFrame(reassoc_req) {
  bssid_ = reassoc_req.bssid_;
}

SimReassocReqFrame::~SimReassocReqFrame() = default;
SimManagementFrame::SimMgmtFrameType SimReassocReqFrame::MgmtFrameType() const {
  return FRAME_TYPE_REASSOC_REQ;
}

SimFrame* SimReassocReqFrame::CopyFrame() const { return new SimReassocReqFrame(*this); }

/* SimReassocRespFrame function implementations.*/
SimReassocRespFrame::SimReassocRespFrame(const SimReassocRespFrame& reassoc_resp)
    : SimManagementFrame(reassoc_resp) {
  status_ = reassoc_resp.status_;
  capability_info_ = reassoc_resp.capability_info_;
}

SimReassocRespFrame::~SimReassocRespFrame() = default;
SimManagementFrame::SimMgmtFrameType SimReassocRespFrame::MgmtFrameType() const {
  return FRAME_TYPE_REASSOC_RESP;
}

SimFrame* SimReassocRespFrame::CopyFrame() const { return new SimReassocRespFrame(*this); }

/* SimActionFrame function implementations.*/
SimActionFrame::SimActionFrame(const SimActionFrame& action) = default;

SimActionFrame::~SimActionFrame() = default;

SimManagementFrame::SimMgmtFrameType SimActionFrame::MgmtFrameType() const {
  return FRAME_TYPE_ACTION;
}

SimFrame* SimActionFrame::CopyFrame() const { return new SimActionFrame(*this); }

SimActionFrame::SimActionCategory SimActionFrame::ActionCategory() const { return category_; }

/* SimWnmActionFrame function implementations.*/
SimWnmActionFrame::SimWnmActionFrame(const SimWnmActionFrame& wnm_action) = default;

SimWnmActionFrame::~SimWnmActionFrame() = default;

SimFrame* SimWnmActionFrame::CopyFrame() const { return new SimWnmActionFrame(*this); }

/* SimBtmReqFrame function implementations.*/
SimBtmReqFrame::SimBtmReqFrame(const SimBtmReqFrame& btm_req) = default;

SimBtmReqFrame::~SimBtmReqFrame() = default;

SimFrame* SimBtmReqFrame::CopyFrame() const { return new SimBtmReqFrame(*this); }

std::vector<SimNeighborReportElement> SimBtmReqFrame::CandidateList() const {
  std::vector<SimNeighborReportElement> candidates = candidate_list_;
  return candidates;
}

/* SimDataFrame function implementations.*/
SimDataFrame::SimDataFrame(const SimDataFrame& data_frame) {
  toDS_ = data_frame.toDS_;
  fromDS_ = data_frame.fromDS_;
  addr1_ = data_frame.addr1_;
  addr2_ = data_frame.addr2_;
  addr3_ = data_frame.addr3_;
  addr4_ = data_frame.addr4_;
  qosControl_ = data_frame.qosControl_;
  payload_.assign(data_frame.payload_.begin(), data_frame.payload_.end());
}

SimDataFrame::~SimDataFrame() = default;
SimFrame::SimFrameType SimDataFrame::FrameType() const { return FRAME_TYPE_DATA; }

/* SimQosDataFrame function implementations.*/
SimQosDataFrame::SimQosDataFrame(const SimQosDataFrame& qos_data) : SimDataFrame(qos_data) {}

SimQosDataFrame::~SimQosDataFrame() = default;
SimDataFrame::SimDataFrameType SimQosDataFrame::DataFrameType() const {
  return FRAME_TYPE_QOS_DATA;
}

SimFrame* SimQosDataFrame::CopyFrame() const { return new SimQosDataFrame(*this); }

}  // namespace wlan::simulation
