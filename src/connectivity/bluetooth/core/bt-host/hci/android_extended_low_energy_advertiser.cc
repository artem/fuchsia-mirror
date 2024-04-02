// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/android_extended_low_energy_advertiser.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/vendor_protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/transport.h"

#include <pw_bluetooth/hci_common.emb.h>
#include <pw_bluetooth/hci_vendor.emb.h>

namespace bt::hci {
namespace pwemb = pw::bluetooth::emboss;

// Android range -70 to +20, select the middle for now
constexpr int8_t kTransmitPower = -25;

// AndroidExtendedLowEnergyAdvertiser doesn't support extended advertising PDUs
constexpr bool kUseExtendedPdu = false;

namespace hci_android = hci_spec::vendor::android;
namespace android_hci = pw::bluetooth::vendor::android_hci;

AndroidExtendedLowEnergyAdvertiser::AndroidExtendedLowEnergyAdvertiser(
    hci::Transport::WeakPtr hci_ptr, uint8_t max_advertisements)
    : LowEnergyAdvertiser(std::move(hci_ptr)),
      advertising_handle_map_(max_advertisements) {
  state_changed_event_handler_id_ =
      hci()->command_channel()->AddVendorEventHandler(
          hci_android::kLEMultiAdvtStateChangeSubeventCode,
          [this](const EmbossEventPacket& event_packet) {
            return OnAdvertisingStateChangedSubevent(event_packet);
          });
}

AndroidExtendedLowEnergyAdvertiser::~AndroidExtendedLowEnergyAdvertiser() {
  // This object is probably being destroyed because the stack is shutting down,
  // in which case the HCI layer may have already been destroyed.
  if (!hci().is_alive() || !hci()->command_channel()) {
    return;
  }

  hci()->command_channel()->RemoveEventHandler(state_changed_event_handler_id_);

  // TODO(https://fxbug.dev/42063496): This will only cancel one advertisement,
  // after which the SequentialCommandRunner will have been destroyed and no
  // further commands will be sent.
  StopAdvertising();
}

EmbossCommandPacket AndroidExtendedLowEnergyAdvertiser::BuildEnablePacket(
    const DeviceAddress& address,
    pwemb::GenericEnableParam enable,
    bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtEnableCommandWriter>(hci_android::kLEMultiAdvt);
  auto packet_view = packet.view_t();
  packet_view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtEnableSubopcode);
  packet_view.enable().Write(enable);
  packet_view.advertising_handle().Write(handle.value());
  return packet;
}

std::optional<EmbossCommandPacket>
AndroidExtendedLowEnergyAdvertiser::BuildSetAdvertisingParams(
    const DeviceAddress& address,
    pwemb::LEAdvertisingType type,
    pwemb::LEOwnAddressType own_address_type,
    const AdvertisingIntervalRange& interval,
    bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.MapHandle(address, extended_pdu);
  if (!handle) {
    bt_log(WARN,
           "hci-le",
           "could not allocate advertising handle for address: %s",
           bt_str(address));
    return std::nullopt;
  }

  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtSetAdvtParamCommandWriter>(
      hci_android::kLEMultiAdvt);
  auto view = packet.view_t();

  view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtSetAdvtParamSubopcode);
  view.adv_interval_min().Write(interval.min());
  view.adv_interval_max().Write(interval.max());
  view.adv_type().Write(type);
  view.own_addr_type().Write(own_address_type);
  view.adv_channel_map().channel_37().Write(true);
  view.adv_channel_map().channel_38().Write(true);
  view.adv_channel_map().channel_39().Write(true);
  view.adv_filter_policy().Write(pwemb::LEAdvertisingFilterPolicy::ALLOW_ALL);
  view.adv_handle().Write(handle.value());
  view.adv_tx_power().Write(hci_spec::kLEAdvertisingTxPowerMax);

  // We don't support directed advertising yet, so leave peer_address and
  // peer_address_type as 0x00
  // (|packet| parameters are initialized to zero above).

  return packet;
}

EmbossCommandPacket AndroidExtendedLowEnergyAdvertiser::BuildSetAdvertisingData(
    const DeviceAddress& address,
    const AdvertisingData& data,
    AdvFlags flags,
    bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  uint8_t adv_data_length =
      static_cast<uint8_t>(data.CalculateBlockSize(/*include_flags=*/true));
  size_t packet_size =
      android_hci::LEMultiAdvtSetAdvtDataCommandWriter::MinSizeInBytes()
          .Read() +
      adv_data_length;

  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtSetAdvtDataCommandWriter>(
      hci_android::kLEMultiAdvt, packet_size);
  auto view = packet.view_t();

  view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtSetAdvtDataSubopcode);
  view.adv_data_length().Write(adv_data_length);
  view.adv_handle().Write(handle.value());

  MutableBufferView data_view(view.adv_data().BackingStorage().data(),
                              adv_data_length);
  data.WriteBlock(&data_view, flags);

  return packet;
}

EmbossCommandPacket
AndroidExtendedLowEnergyAdvertiser::BuildUnsetAdvertisingData(
    const DeviceAddress& address, bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  size_t packet_size =
      android_hci::LEMultiAdvtSetAdvtDataCommandWriter::MinSizeInBytes().Read();
  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtSetAdvtDataCommandWriter>(
      hci_android::kLEMultiAdvt, packet_size);
  auto view = packet.view_t();

  view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtSetAdvtDataSubopcode);
  view.adv_data_length().Write(0);
  view.adv_handle().Write(handle.value());

  return packet;
}

EmbossCommandPacket AndroidExtendedLowEnergyAdvertiser::BuildSetScanResponse(
    const DeviceAddress& address,
    const AdvertisingData& data,
    bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  uint8_t scan_rsp_length = static_cast<uint8_t>(data.CalculateBlockSize());
  size_t packet_size =
      android_hci::LEMultiAdvtSetScanRespDataCommandWriter::MinSizeInBytes()
          .Read() +
      scan_rsp_length;
  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtSetScanRespDataCommandWriter>(
      hci_android::kLEMultiAdvt, packet_size);
  auto view = packet.view_t();

  view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtSetScanRespSubopcode);
  view.scan_resp_length().Write(scan_rsp_length);
  view.adv_handle().Write(handle.value());

  MutableBufferView data_view(view.adv_data().BackingStorage().data(),
                              scan_rsp_length);
  data.WriteBlock(&data_view, std::nullopt);

  return packet;
}

EmbossCommandPacket AndroidExtendedLowEnergyAdvertiser::BuildUnsetScanResponse(
    const DeviceAddress& address, bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  size_t packet_size =
      android_hci::LEMultiAdvtSetScanRespDataCommandWriter::MinSizeInBytes()
          .Read();
  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtSetScanRespDataCommandWriter>(
      hci_android::kLEMultiAdvt, packet_size);
  auto view = packet.view_t();

  view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtSetScanRespSubopcode);
  view.scan_resp_length().Write(0);
  view.adv_handle().Write(handle.value());

  return packet;
}

EmbossCommandPacket
AndroidExtendedLowEnergyAdvertiser::BuildRemoveAdvertisingSet(
    const DeviceAddress& address, bool extended_pdu) {
  std::optional<hci_spec::AdvertisingHandle> handle =
      advertising_handle_map_.GetHandle(address, extended_pdu);
  BT_ASSERT(handle);

  auto packet = hci::EmbossCommandPacket::New<
      android_hci::LEMultiAdvtEnableCommandWriter>(hci_android::kLEMultiAdvt);
  auto packet_view = packet.view_t();
  packet_view.vendor_command().sub_opcode().Write(
      hci_android::kLEMultiAdvtEnableSubopcode);
  packet_view.enable().Write(pwemb::GenericEnableParam::DISABLE);
  packet_view.advertising_handle().Write(handle.value());
  return packet;
}

void AndroidExtendedLowEnergyAdvertiser::StartAdvertising(
    const DeviceAddress& address,
    const AdvertisingData& data,
    const AdvertisingData& scan_rsp,
    const AdvertisingOptions& options,
    ConnectionCallback connect_callback,
    ResultFunction<> result_callback) {
  fit::result<HostError> result =
      CanStartAdvertising(address, data, scan_rsp, options);
  if (result.is_error()) {
    result_callback(ToResult(result.error_value()));
    return;
  }

  if (options.extended_pdu) {
    bt_log(WARN,
           "hci-le",
           "android vendor extensions cannot use extended advertising PDUs");
    result_callback(ToResult(HostError::kNotSupported));
    return;
  }

  AdvertisingData copied_data;
  data.Copy(&copied_data);

  AdvertisingData copied_scan_rsp;
  scan_rsp.Copy(&copied_scan_rsp);

  // if there is an operation currently in progress, enqueue this operation and
  // we will get to it the next time we have a chance
  if (!hci_cmd_runner().IsReady()) {
    bt_log(INFO,
           "hci-le",
           "hci cmd runner not ready, queuing advertisement commands for now");

    op_queue_.push([this,
                    address,
                    data = std::move(copied_data),
                    scan_rsp = std::move(copied_scan_rsp),
                    options,
                    conn_cb = std::move(connect_callback),
                    result_cb = std::move(result_callback)]() mutable {
      StartAdvertising(address,
                       data,
                       scan_rsp,
                       options,
                       std::move(conn_cb),
                       std::move(result_cb));
    });

    return;
  }

  if (IsAdvertising(address, options.extended_pdu)) {
    bt_log(DEBUG,
           "hci-le",
           "updating existing advertisement for %s",
           bt_str(address));
  }

  if (options.include_tx_power_level) {
    copied_data.SetTxPower(kTransmitPower);
    copied_scan_rsp.SetTxPower(kTransmitPower);
  }

  StartAdvertisingInternal(address,
                           copied_data,
                           copied_scan_rsp,
                           options,
                           std::move(connect_callback),
                           std::move(result_callback));
}

void AndroidExtendedLowEnergyAdvertiser::StopAdvertising() {
  LowEnergyAdvertiser::StopAdvertising();
  advertising_handle_map_.Clear();

  // std::queue doesn't have a clear method so we have to resort to this
  // tomfoolery :(
  decltype(op_queue_) empty;
  std::swap(op_queue_, empty);
}

void AndroidExtendedLowEnergyAdvertiser::StopAdvertising(
    const DeviceAddress& address, bool extended_pdu) {
  // if there is an operation currently in progress, enqueue this operation and
  // we will get to it the next time we have a chance
  if (!hci_cmd_runner().IsReady()) {
    bt_log(
        INFO,
        "hci-le",
        "hci cmd runner not ready, queueing stop advertising command for now");
    op_queue_.push([this, address, extended_pdu]() {
      StopAdvertising(address, extended_pdu);
    });
    return;
  }

  LowEnergyAdvertiser::StopAdvertisingInternal(address, kUseExtendedPdu);
  advertising_handle_map_.RemoveAddress(address, kUseExtendedPdu);
}

void AndroidExtendedLowEnergyAdvertiser::OnIncomingConnection(
    hci_spec::ConnectionHandle handle,
    pwemb::ConnectionRole role,
    const DeviceAddress& peer_address,
    const hci_spec::LEConnectionParameters& conn_params) {
  staged_connections_map_[handle] = {role, peer_address, conn_params};
}

// The LE multi-advertising state change subevent contains the mapping between
// connection handle and advertising handle. After the LE multi-advertising
// state change subevent, we have all the information necessary to create a
// connection object within the Host layer.
CommandChannel::EventCallbackResult
AndroidExtendedLowEnergyAdvertiser::OnAdvertisingStateChangedSubevent(
    const EmbossEventPacket& event) {
  BT_ASSERT(event.event_code() == hci_spec::kVendorDebugEventCode);
  BT_ASSERT(event.view<pwemb::VendorDebugEventView>().subevent_code().Read() ==
            hci_android::kLEMultiAdvtStateChangeSubeventCode);

  Result<> result = event.ToResult();
  if (bt_is_error(result,
                  ERROR,
                  "hci-le",
                  "advertising state change event, error received %s",
                  bt_str(result))) {
    return CommandChannel::EventCallbackResult::kContinue;
  }

  auto view = event.view<android_hci::LEMultiAdvtStateChangeSubeventView>();
  hci_spec::AdvertisingHandle adv_handle = view.advertising_handle().Read();
  std::optional<DeviceAddress> opt_local_address =
      advertising_handle_map_.GetAddress(adv_handle);

  // We use the identity address as the local address if we aren't advertising
  // or otherwise don't know about this advertising set. This is obviously
  // wrong. However, the link will be disconnected in that case before it can
  // propagate to higher layers.
  static DeviceAddress identity_address =
      DeviceAddress(DeviceAddress::Type::kLEPublic, {0});
  DeviceAddress local_address = identity_address;
  if (opt_local_address) {
    local_address = opt_local_address.value();
  }

  hci_spec::ConnectionHandle connection_handle =
      view.connection_handle().Read();
  auto staged_node = staged_connections_map_.extract(connection_handle);
  if (staged_node.empty()) {
    bt_log(ERROR,
           "hci-le",
           "advertising state change event, staged params not available "
           "(handle: %d)",
           view.advertising_handle().Read());
    return CommandChannel::EventCallbackResult::kContinue;
  }

  StagedConnectionParameters staged = staged_node.mapped();
  CompleteIncomingConnection(connection_handle,
                             staged.role,
                             local_address,
                             staged.peer_address,
                             staged.conn_params,
                             kUseExtendedPdu);

  return CommandChannel::EventCallbackResult::kContinue;
}

void AndroidExtendedLowEnergyAdvertiser::OnCurrentOperationComplete() {
  if (op_queue_.empty()) {
    return;  // no more queued operations so nothing to do
  }

  fit::closure closure = std::move(op_queue_.front());
  op_queue_.pop();
  closure();
}

}  // namespace bt::hci
