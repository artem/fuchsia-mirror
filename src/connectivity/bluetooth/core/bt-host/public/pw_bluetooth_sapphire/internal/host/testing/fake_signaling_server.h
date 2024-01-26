// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_SIGNALING_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_SIGNALING_SERVER_H_

#include "fake_l2cap.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/packet_view.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/l2cap_defs.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_dynamic_channel.h"

namespace bt::testing {

// This class unpacks signaling packets (generally received over a FakeL2cap
// link). Each FakePeer should own its own FakeSignalingServer.
class FakeSignalingServer final {
 public:
  FakeSignalingServer() = default;

  // Registers this FakeSignalingServer's HandleSdu function with |l2cap_| on
  // kSignalingChannelId such that all packets processed by |l2cap_| with the
  // ChannelId kSignalingChanneld will be processed by this server.
  // FakeL2cap will also handle actually sending packets generated by this
  // FakeSignalingServer instance.
  void RegisterWithL2cap(FakeL2cap* l2cap_);

  // Handles the service data unit |sdu| received over link with handle |conn|
  // by confirming that the received packet is valid and then calling
  // ProcessSignalingPacket.
  void HandleSdu(hci_spec::ConnectionHandle conn, const ByteBuffer& sdu);

  // Parses the InformationRequest signaling packet |info_req| and then
  // calls the appropriate function to construct and send a response packet
  // over handle |conn| and ID |id|.
  void ProcessInformationRequest(hci_spec::ConnectionHandle conn,
                                 l2cap::CommandId id,
                                 const ByteBuffer& info_req);

  // Handle incoming ConnectionRequest |connection_req| by validating the
  // request, creating a FakeDynamicChannel object, and registering that
  // channel with the associated FakeL2cap module. Also will use the server's
  // SendFrameCallback to send a ConnectionResponse and ConfigurationRequest
  // over handle |conn| and ID |id|.
  void ProcessConnectionRequest(hci_spec::ConnectionHandle conn,
                                l2cap::CommandId id,
                                const ByteBuffer& connection_req);

  // Handle incoming ConfigurationRequest |configuration_req|. Note that
  // because the all channels here are basic mode, this is simply part of the
  // connection process and does not actually configure specific parameters
  // of the associated FakeDynamicChannel aside from enabling data transfer
  // when both sides send and receive connection requests.
  // The emulator assumes that the ProcessConnectionRequest function handles
  // sending the initial ConfigurationRequest from FakePeer, so if the emulator
  // received a ConfigurationRequest from bt-host, it will assume the
  // channel is ready to open.
  // FakePeer will still respond with a ConfigurationResponse over handle
  // |conn| using the ID |id| and send it using the FakeL2cap instance's
  // SendFrameCallback.
  void ProcessConfigurationRequest(hci_spec::ConnectionHandle conn,
                                   l2cap::CommandId id,
                                   const ByteBuffer& configuration_req);

  // Handle configuration responses sent by bt-host. The emulator assumes that
  // the ProcessConnectionRequest function handles sending the initial
  // ConfigurationRequest from FakePeer, so it disregards this
  // ConnfigurationResponse and instead only processes bt-host's inbound
  // ConfigurationRequest.
  void ProcessConfigurationResponse(hci_spec::ConnectionHandle conn,
                                    l2cap::CommandId id,
                                    const ByteBuffer& configuration_res);

  // Upon receiving the disconnection request |disconnection_req|, close and
  // delete the associated channel from the map associated with fake_l2cap_,
  // and then send back a disconnection response with connection handle
  //  |conn| and ID |id| and send it using the FakeL2cap instance's
  // SendFrameCallback..
  void ProcessDisconnectionRequest(hci_spec::ConnectionHandle conn,
                                   l2cap::CommandId id,
                                   const ByteBuffer& disconnection_req);

  // Helper function for sending an individual payload buffer |payload_buffer|
  // by assembling a header with CommandCode |code| and CommandId |id| and then
  // send it over handle |conn| and with the FakeL2cap instance's
  // SendFrameCallback.
  void SendCFrame(hci_spec::ConnectionHandle conn,
                  l2cap::CommandCode code,
                  l2cap::CommandId id,
                  DynamicByteBuffer& payload_buffer);

  // Reject a command packet for some |reason|. Assemble a command reject
  // packet using the handle |conn| and the CommandId |id| and send with the
  // FakeL2cap instance's SendFrameCallback.
  void SendCommandReject(hci_spec::ConnectionHandle conn,
                         l2cap::CommandId id,
                         l2cap::RejectReason reason);

  // Respond to a received fixed channels InformationRequest packet with a
  // InformationResponse. Assemble the InformationResponse using the handle
  // |conn| and id |id|. Send with the FakeL2cap instance's SendFrameCallback.
  void SendInformationResponseFixedChannels(hci_spec::ConnectionHandle conn,
                                            l2cap::CommandId id);

  // Respond to a received extended features InformationRequest packet with a
  // InformationResponse. Assemble the InformationResponse using the handle
  // |conn| and id |id|. Send with the FakeL2cap instance's SendFrameCallback.
  void SendInformationResponseExtendedFeatures(hci_spec::ConnectionHandle conn,
                                               l2cap::CommandId id);

  // Respond to a received ConnectionRequest packet with a ConnectionResponse.
  // Assemble the ConnectionResponse using the handle |conn|, id |id|, local
  // channel |local_id|, remote channel |remote_id|, ConnectionResult |result|,
  // and ConnectionStatus |status|. Send with the FakeL2cap instance's
  // SendFrameCallback.
  void SendConnectionResponse(hci_spec::ConnectionHandle conn,
                              l2cap::CommandId id,
                              l2cap::ChannelId local_cid,
                              l2cap::ChannelId remote_id,
                              l2cap::ConnectionResult result,
                              l2cap::ConnectionStatus status);

  // Send a ConfigurationRequest packet following a ConnectionResponse.
  // Assemble the ConfigurationRequest using the handle |conn|, id |id|,
  // and remote ID |remote_cid|. Send with the FakeL2cap instance's
  // SendFrameCallback.
  void SendConfigurationRequest(hci_spec::ConnectionHandle conn,
                                l2cap::CommandId id,
                                l2cap::ChannelId remote_cid);

  // Respond to a received ConfigurationRequest packet with a
  // ConfigurationResposne. Assemble the ConfigurationResposne using the handle
  // |conn|, id |id|, local ID |local_cid|, and ConfigurationResult |result|.
  // Send with the
  /// FakeL2cap instance's SendFrameCallback.
  void SendConfigurationResponse(hci_spec::ConnectionHandle conn,
                                 l2cap::CommandId id,
                                 l2cap::ChannelId local_cid,
                                 l2cap::ConfigurationResult result);

  // Respond to a received DisconnectionRequest packet with a
  // DisconnectionResponse. Assemble the DisconnectionREsponse using the handle
  // |conn|, id |id|, local ID |local_cid|, and remote ID |remote_cid|. Send
  // with the FakeL2cap instance's SendFrameCallback.
  void SendDisconnectionResponse(hci_spec::ConnectionHandle conn,
                                 l2cap::CommandId id,
                                 l2cap::ChannelId local_cid,
                                 l2cap::ChannelId remote_cid);

  // Return the FakeL2cap instance associated with this FakeSignalingServer.
  FakeL2cap* fake_l2cap() { return fake_l2cap_; }

 private:
  // FakeL2cap instance associated with this server, populated after this
  // FakeSignalingServer registers itself with FakeL2cap.
  FakeL2cap* fake_l2cap_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(FakeSignalingServer);
};

}  // namespace bt::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TESTING_FAKE_SIGNALING_SERVER_H_
