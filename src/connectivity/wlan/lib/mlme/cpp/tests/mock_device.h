// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_MOCK_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_MOCK_DEVICE_H_

#include <fuchsia/hardware/wlan/mac/c/banjo.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <algorithm>
#include <memory>
#include <vector>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>
#include <wlan/mlme/device_interface.h>
#include <wlan/mlme/mlme.h>
#include <wlan/mlme/packet.h>

#include "mlme_msg.h"
#include "test_utils.h"

namespace wlan {

static constexpr uint8_t kClientAddress[] = {0x94, 0x3C, 0x49, 0x49, 0x9F, 0x2D};

namespace {

struct WlanPacket {
  std::unique_ptr<Packet> pkt;
  channel_bandwidth_t cbw;
  wlan_info_phy_type_t phy;
  wlan_tx_info_t tx_info;
};

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

// Reads a fidl_incoming_msg_t from a channel.
struct FidlMessage {
  static std::optional<FidlMessage> ReadFromChannel(const zx::channel* endpoint) {
    FidlMessage msg = {};
    zx_handle_info_t handle_infos[256];
    auto status =
        endpoint->read_etc(ZX_CHANNEL_READ_MAY_DISCARD, msg.bytes, handle_infos, sizeof(msg.bytes),
                           sizeof(handle_infos), &msg.actual_bytes, &msg.actual_handles);
    if (status != ZX_OK) {
      return {std::nullopt};
    }
    for (uint32_t i = 0; i < msg.actual_handles; i++) {
      msg.handles[i] = handle_infos[i].handle;
      msg.handle_metadata[i] = fidl_channel_handle_metadata_t{
          .obj_type = handle_infos[i].type,
          .rights = handle_infos[i].rights,
      };
    }
    return {std::move(msg)};
  }

  fidl_incoming_msg_t get() {
    return {.bytes = bytes,
            .handles = handles,
            .num_bytes = actual_bytes,
            .num_handles = actual_handles};
  }

  cpp20::span<uint8_t> data() { return {bytes, actual_bytes}; }

  FIDL_ALIGNDECL uint8_t bytes[256];
  zx_handle_t handles[256];
  fidl_channel_handle_metadata_t handle_metadata[256];
  uint32_t actual_bytes;
  uint32_t actual_handles;
};

// TODO(hahnr): Support for failing various device calls.
struct MockDevice : public DeviceInterface {
 public:
  using PacketList = std::vector<WlanPacket>;
  using KeyList = std::vector<wlan_key_config_t>;

  MockDevice(common::MacAddr addr = common::MacAddr(kClientAddress)) : sta_assoc_ctx_{} {
    auto [sme, mlme] = make_channel();
    sme_ = fidl::InterfaceHandle<fuchsia::wlan::mlme::MLME>(std::move(sme)).BindSync();
    mlme_ = std::make_optional(std::move(mlme));

    state = fbl::AdoptRef(new DeviceState);
    state->set_address(addr);

    memcpy(wlanmac_info.sta_addr, addr.byte, 6);
    wlanmac_info.mac_role = WLAN_INFO_MAC_ROLE_CLIENT;
    wlanmac_info.supported_phys =
        WLAN_INFO_PHY_TYPE_OFDM | WLAN_INFO_PHY_TYPE_HT | WLAN_INFO_PHY_TYPE_VHT;
    wlanmac_info.driver_features = 0;
    wlanmac_info.bands_count = 2;
    wlanmac_info.bands[0] = test_utils::FakeBandInfo(WLAN_INFO_BAND_2GHZ);
    wlanmac_info.bands[1] = test_utils::FakeBandInfo(WLAN_INFO_BAND_5GHZ);
    wlanmac_info.caps = 0;
    state->set_channel({
        .primary = 1,
        .cbw = CHANNEL_BANDWIDTH_CBW20,
    });
  }

  // DeviceInterface implementation.

  zx_status_t Start(const rust_wlanmac_ifc_protocol_copy_t* ifc,
                    zx::channel* out_sme_channel) final {
    protocol_ = std::make_optional(
        wlanmac_ifc_protocol_ops_t{.status = ifc->ops->status,
                                   .recv = ifc->ops->recv,
                                   .complete_tx = ifc->ops->complete_tx,
                                   .indication = ifc->ops->indication,
                                   .report_tx_status = ifc->ops->report_tx_status,
                                   .hw_scan_complete = ifc->ops->hw_scan_complete});
    protocol_ctx_ = ifc->ctx;
    if (mlme_->is_valid()) {
      *out_sme_channel = std::move(mlme_.value());
      return ZX_OK;
    } else {
      return ZX_ERR_BAD_STATE;
    }
  }

  zx_status_t DeliverEthernet(cpp20::span<const uint8_t> eth_frame) final {
    eth_queue.push_back({eth_frame.begin(), eth_frame.end()});
    return ZX_OK;
  }

  zx_status_t QueueTx(uint32_t options, std::unique_ptr<Packet> packet,
                      wlan_tx_info_t tx_info) final {
    WlanPacket wlan_packet;
    wlan_packet.pkt = std::move(packet);
    wlan_packet.tx_info = tx_info;
    wlan_queue.push_back(std::move(wlan_packet));
    return ZX_OK;
  }

  zx_status_t SetChannel(wlan_channel_t channel) final {
    state->set_channel(channel);
    return ZX_OK;
  }

  zx_status_t SetStatus(uint32_t status) final {
    state->set_online(status == 1);
    return ZX_OK;
  }

  zx_status_t ConfigureBss(bss_config_t* cfg) final {
    if (!cfg) {
      bss_cfg.reset();
    } else {
      // Copy config which might get freed by the MLME before the result was
      // verified.
      bss_cfg.reset(new bss_config_t);
      memcpy(bss_cfg.get(), cfg, sizeof(bss_config_t));
    }
    return ZX_OK;
  }

  zx_status_t ConfigureBeacon(std::unique_ptr<Packet> packet) final {
    beacon = std::move(packet);
    return ZX_OK;
  }

  zx_status_t EnableBeaconing(wlan_bcn_config_t* bcn_cfg) final {
    beaconing_enabled = (bcn_cfg != nullptr);
    return ZX_OK;
  }

  zx_status_t SetKey(wlan_key_config_t* cfg) final {
    keys.push_back(*cfg);
    return ZX_OK;
  }

  zx_status_t StartHwScan(const wlan_hw_scan_config_t* scan_config) override {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t ConfigureAssoc(wlan_assoc_ctx_t* assoc_ctx) final {
    sta_assoc_ctx_ = *assoc_ctx;
    return ZX_OK;
  }
  zx_status_t ClearAssoc(const common::MacAddr& peer_addr) final {
    std::memset(&sta_assoc_ctx_, 0, sizeof(sta_assoc_ctx_));
    return ZX_OK;
  }

  fbl::RefPtr<DeviceState> GetState() final { return state; }

  const wlanmac_info_t& GetWlanMacInfo() const final { return wlanmac_info; }

  // Convenience methods.

  wlan_channel_t GetChannel() { return state->channel(); }

  uint16_t GetChannelNumber() { return state->channel().primary; }

  void SendWlanPacket(std::unique_ptr<Packet> packet) {
    ZX_ASSERT(protocol_.has_value());
    wlan_rx_packet_t rx_packet = {
        .mac_frame_buffer = packet->data(),
        .mac_frame_size = packet->len(),
    };
    if (packet->has_ctrl_data<wlan_rx_info_t>()) {
      rx_packet.info = *packet->ctrl_data<wlan_rx_info_t>();
    }
    protocol_->recv(protocol_ctx_, &rx_packet);
  }

  template <typename T>
  std::optional<MlmeMsg<T>> GetNextMsgFromSmeChannel(uint64_t ordinal = MlmeMsg<T>::kNoOrdinal) {
    zx_signals_t observed;
    sme_.unowned_channel()->wait_one(ZX_CHANNEL_READABLE | ZX_SOCKET_PEER_CLOSED,
                                     zx::deadline_after(zx::msec(10)), &observed);
    if (!(observed & ZX_CHANNEL_READABLE)) {
      return {};
    };

    uint32_t read = 0;
    uint8_t buf[ZX_CHANNEL_MAX_MSG_BYTES];

    zx_status_t status =
        sme_.unowned_channel()->read(0, buf, nullptr, ZX_CHANNEL_MAX_MSG_BYTES, 0, &read, nullptr);
    ZX_ASSERT(status == ZX_OK);

    return MlmeMsg<T>::Decode(cpp20::span{buf, read}, ordinal);
  }

  template <typename T>
  MlmeMsg<T> AssertNextMsgFromSmeChannel(uint64_t ordinal = MlmeMsg<T>::kNoOrdinal) {
    zx_signals_t observed;
    sme_.unowned_channel()->wait_one(ZX_CHANNEL_READABLE | ZX_SOCKET_PEER_CLOSED,
                                     zx::deadline_after(zx::sec(1)), &observed);
    ZX_ASSERT(observed & ZX_CHANNEL_READABLE);

    uint32_t read = 0;
    uint8_t buf[ZX_CHANNEL_MAX_MSG_BYTES];

    zx_status_t status =
        sme_.unowned_channel()->read(0, buf, nullptr, ZX_CHANNEL_MAX_MSG_BYTES, 0, &read, nullptr);
    ZX_ASSERT(status == ZX_OK);

    auto msg = MlmeMsg<T>::Decode(cpp20::span{buf, read}, ordinal);
    ZX_ASSERT(msg.has_value());
    return std::move(msg).value();
  }

  std::vector<std::vector<uint8_t>> GetEthPackets() {
    std::vector<std::vector<uint8_t>> tmp;
    tmp.swap(eth_queue);
    return tmp;
  }

  PacketList GetWlanPackets() { return std::move(wlan_queue); }

  KeyList GetKeys() { return keys; }

  const wlan_assoc_ctx_t* GetStationAssocContext(void) { return &sta_assoc_ctx_; }

  bool AreQueuesEmpty() { return wlan_queue.empty() && svc_queue.empty() && eth_queue.empty(); }

  std::optional<FidlMessage> NextTxMlmeMsg() {
    return FidlMessage::ReadFromChannel(&*sme_.unowned_channel());
  }

  fbl::RefPtr<DeviceState> state;
  wlanmac_info_t wlanmac_info;
  PacketList wlan_queue;
  std::vector<std::vector<uint8_t>> svc_queue;
  std::vector<std::vector<uint8_t>> eth_queue;
  std::unique_ptr<bss_config_t> bss_cfg;
  KeyList keys;
  std::unique_ptr<Packet> beacon;
  bool beaconing_enabled;
  wlan_assoc_ctx_t sta_assoc_ctx_;
  fidl::SynchronousInterfacePtr<fuchsia::wlan::mlme::MLME> sme_;
  std::optional<zx::channel> mlme_;
  std::optional<wlanmac_ifc_protocol_ops_t> protocol_;
  void* protocol_ctx_;
};

}  // namespace
}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_MOCK_DEVICE_H_
