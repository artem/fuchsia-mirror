// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_PEER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_PEER_H_

#include <unordered_set>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/device_address.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/macros.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/le_connection_parameters.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_gatt_server.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_l2cap.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_sdp_server.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_signaling_server.h"

namespace bt::testing {

class FakeController;

// FakePeer is used to emulate a remote Bluetooth device.
class FakePeer {
 public:
  // NOTE: Setting |connectable| to true will result in a "Connectable and
  // Scannable Advertisement" (i.e. ADV_IND) even if |scannable| is set to
  // false. This is OK since we use |scannable| to drive the receipt of Scan
  // Response PDUs: we use this to test the condition in which the advertisement
  // is scannable but the host never receives a scan response.
  explicit FakePeer(const DeviceAddress& address,
                    pw::async::Dispatcher& pw_dispatcher,
                    bool connectable = true,
                    bool scannable = true,
                    bool send_advertising_report = true);

  const ByteBuffer& advertising_data() const { return advertising_data_; }
  void set_advertising_data(const ByteBuffer& data) {
    advertising_data_ = DynamicByteBuffer(data);
  }

  void set_scan_response(const ByteBuffer& data);
  const ByteBuffer& scan_response() const { return scan_response_; }

  bool advertising_enabled() const { return advertising_enabled_; }
  void set_advertising_enabled(bool enabled) { advertising_enabled_ = enabled; }

  // Mark this device for directed advertising.
  bool directed_advertising_enabled() const { return directed_; }
  void set_directed_advertising_enabled(bool enable) { directed_ = enable; }

  // Toggles whether the address of this device represents a resolved RPA.
  bool address_resolved() const { return address_resolved_; }
  void set_address_resolved(bool value) { address_resolved_ = value; }

  // Extended advertising PDUs are used when advertising but allow for a greater
  // set of features and a larger payload versus legacy advertising PDUs.
  bool use_extended_advertising_pdus() const {
    return use_extended_advertising_pdus_;
  }
  void set_use_extended_advertising_pdus(bool value) {
    use_extended_advertising_pdus_ = value;
  }

  // TODO(armansito): Come up with a better scheme to determine supported
  // transport type instead of relying on address type, which doesn't translate
  // well to dual-mode.
  bool supports_bredr() const {
    // All BR/EDR devices have inquiry responses.
    return address().type() == DeviceAddress::Type::kBREDR;
  }

  // TODO(armansito): Come up with a better scheme to determine supported
  // transport type instead of relying on address type, which doesn't translate
  // well to dual-mode.
  bool supports_le() const {
    return address().type() != DeviceAddress::Type::kBREDR;
  }

  // Generates a Inquiry Response Event payload containing a inquiry result
  // response.
  DynamicByteBuffer CreateInquiryResponseEvent(
      pw::bluetooth::emboss::InquiryMode mode) const;

  const DeviceAddress& address() const { return address_; }
  int8_t rssi() const { return -1; }
  int8_t tx_power() const { return -2; }

  // The local name of the device. Used in HCI Remote Name Request event.
  std::string name() const { return name_; }
  void set_name(const std::string& name) { name_ = name; }

  // Returns true if this device is scannable. We use this to tell
  // FakeController whether or not it should send scan response PDUs.
  bool scannable() const { return scannable_; }

  bool connectable() const { return connectable_; }

  bool connected() const { return connected_; }
  void set_connected(bool connected) { connected_ = connected; }

  bool send_advertising_report() const { return send_advertising_report_; }
  void set_send_advertising_report(bool value) {
    send_advertising_report_ = value;
  }

  void set_class_of_device(DeviceClass class_of_device) {
    class_of_device_ = class_of_device;
  }

  const hci_spec::LEConnectionParameters& le_params() const {
    return le_params_;
  }
  void set_le_params(const hci_spec::LEConnectionParameters& value) {
    le_params_ = value;
  }

  bool supports_ll_conn_update_procedure() const {
    return supports_ll_conn_update_procedure_;
  }
  void set_supports_ll_conn_update_procedure(bool supports) {
    supports_ll_conn_update_procedure_ = supports;
  }

  hci_spec::LESupportedFeatures le_features() const { return le_features_; }
  void set_le_features(hci_spec::LESupportedFeatures le_features) {
    le_features_ = le_features;
  }

  // The response status that will be returned when this device receives a LE
  // Create Connection command.
  pw::bluetooth::emboss::StatusCode connect_response() const {
    return connect_response_;
  }
  void set_connect_response(pw::bluetooth::emboss::StatusCode response) {
    connect_response_ = response;
  }

  // The status that will be returned in the Command Status event in response to
  // a LE Create Connection command. If this is set to anything other than
  // pw::bluetooth::emboss::StatusCode::SUCCESS, then connect_response() will
  // have no effect.
  pw::bluetooth::emboss::StatusCode connect_status() const {
    return connect_status_;
  }
  void set_connect_status(pw::bluetooth::emboss::StatusCode status) {
    connect_status_ = status;
  }

  bool force_pending_connect() const { return force_pending_connect_; }
  void set_force_pending_connect(bool value) { force_pending_connect_ = value; }

  const std::optional<pw::bluetooth::emboss::LinkType>&
  last_connection_request_link_type() const {
    return last_connection_request_link_type_;
  }
  void set_last_connection_request_link_type(
      std::optional<pw::bluetooth::emboss::LinkType> type) {
    last_connection_request_link_type_ = type;
  }

  void AddLink(hci_spec::ConnectionHandle handle);
  void RemoveLink(hci_spec::ConnectionHandle handle);
  bool HasLink(hci_spec::ConnectionHandle handle) const;

  using HandleSet = std::unordered_set<hci_spec::ConnectionHandle>;
  const HandleSet& logical_links() const { return logical_links_; }

  // Marks this device as disconnected. Clears and returns all logical link
  // handles.
  HandleSet Disconnect();

  // Returns the FakeController that has been assigned to this device.
  FakeController* controller() const { return controller_; }

  // Returns the FakeSdpServer associated with this device.
  FakeSdpServer* sdp_server() { return &sdp_server_; }

  // Generates and returns a LE Advertising Report Event payload. If
  // |include_scan_rsp| is true, then the returned PDU will contain two reports
  // including the SCAN_IND report.
  DynamicByteBuffer BuildLegacyAdvertisingReportEvent(
      bool include_scan_rsp = false) const;

  // Generates a LE Advertising Report Event payload containing the scan
  // response.
  DynamicByteBuffer BuildLegacyScanResponseReportEvent() const;

  // Generates and returns an LE Extended Advertising Report Event payload.
  DynamicByteBuffer BuildExtendedAdvertisingReportEvent() const;

  // Generates an LE Extended Advertising Report Event payload containing the
  // scan response.
  DynamicByteBuffer BuildExtendedScanResponseEvent() const;

  // Populate an LEExtendedAdvertisingReportData as though it was received from
  // this fake peer
  void FillExtendedAdvertisingReport(
      pw::bluetooth::emboss::LEExtendedAdvertisingReportDataWriter report,
      const ByteBuffer& data,
      bool is_fragmented,
      bool is_scan_response) const;

 private:
  friend class FakeController;

  // Called by a FakeController when a FakePeer is registered with it.
  void set_controller(FakeController* ctrl) { controller_ = ctrl; }

  void WriteScanResponseReport(hci_spec::LEAdvertisingReportData* report) const;

  // Validate received L2CAP packets and then route them to the FakeL2cap
  // instance owned by the device. The FakeL2cap instance will process the
  // packet and route it to the appropriate packet handler.
  void OnRxL2CAP(hci_spec::ConnectionHandle conn, const ByteBuffer& pdu);

  // Sends packets over channel ID |cid| and handle |conn| using the
  // FakeController's SendL2CapBFrame function. Assumes that input buffer
  // |packet| has signaling packet header intact but does not have an L2CAP
  //  packet header.
  void SendPacket(hci_spec::ConnectionHandle conn,
                  l2cap::ChannelId cid,
                  const ByteBuffer& packet) const;

  DynamicByteBuffer BuildExtendedAdvertisingReports(
      const ByteBuffer& data, bool is_scan_response) const;

  // The FakeController that this FakePeer has been assigned to.
  FakeController* controller_;  // weak

  DeviceAddress address_;
  std::string name_;
  bool connected_;
  bool connectable_;
  bool scannable_;
  bool advertising_enabled_;
  bool directed_;
  bool address_resolved_;
  bool use_extended_advertising_pdus_ = false;
  bool send_advertising_report_;

  pw::bluetooth::emboss::StatusCode connect_status_;
  pw::bluetooth::emboss::StatusCode connect_response_;
  bool force_pending_connect_;  // Causes connection requests to remain pending.
  std::optional<pw::bluetooth::emboss::LinkType>
      last_connection_request_link_type_;

  hci_spec::LEConnectionParameters le_params_;

  // If false, FakeController will send LE Connection Update complete events
  // with status kRemoteFeatureNotSupported.
  bool supports_ll_conn_update_procedure_;

  hci_spec::LESupportedFeatures le_features_;

  DynamicByteBuffer advertising_data_;
  DynamicByteBuffer scan_response_;

  // Open connection handles.
  HandleSet logical_links_;

  // Class of device
  DeviceClass class_of_device_;

  FakeL2cap l2cap_;
  FakeGattServer gatt_server_;
  FakeSignalingServer signaling_server_;
  FakeSdpServer sdp_server_;
  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(FakePeer);
};

}  // namespace bt::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_PEER_H_
