// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TRANSPORT_ACL_DATA_PACKET_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TRANSPORT_ACL_DATA_PACKET_H_

#include <lib/fit/function.h>

#include <memory>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/macros.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/packet_view.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/trace.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/packet.h"

namespace bt::hci {

// Packet template specialization for ACL data packets. This cannot be directly
// instantiated. Represents a HCI ACL data packet.
using ACLDataPacket = Packet<hci_spec::ACLDataHeader>;
using ACLDataPacketPtr = std::unique_ptr<ACLDataPacket>;
using ACLPacketHandler = fit::function<void(ACLDataPacketPtr data_packet)>;

template <>
class Packet<hci_spec::ACLDataHeader> : public PacketBase<hci_spec::ACLDataHeader, ACLDataPacket> {
 public:
  // Slab-allocates a new ACLDataPacket with the given payload size without
  // initializing its contents.
  static ACLDataPacketPtr New(uint16_t payload_size);

  // Slab-allocates a new ACLDataPacket with the given payload size and
  // initializes the packet's header field with the given data.
  static ACLDataPacketPtr New(hci_spec::ConnectionHandle connection_handle,
                              hci_spec::ACLPacketBoundaryFlag packet_boundary_flag,
                              hci_spec::ACLBroadcastFlag broadcast_flag,
                              uint16_t payload_size = 0u);

  // Getters for the header fields.
  hci_spec::ConnectionHandle connection_handle() const;
  hci_spec::ACLPacketBoundaryFlag packet_boundary_flag() const;
  hci_spec::ACLBroadcastFlag broadcast_flag() const;

  // Initializes the internal PacketView by reading the header portion of the
  // underlying buffer.
  void InitializeFromBuffer();

  // ID to trace packet flow
  void set_trace_id(trace_flow_id_t id) { async_id_ = id; }
  trace_flow_id_t trace_id() { return async_id_; }

 protected:
  using PacketBase<hci_spec::ACLDataHeader, ACLDataPacket>::PacketBase;

 private:
  // Writes the given header fields into the underlying buffer.
  void WriteHeader(hci_spec::ConnectionHandle connection_handle,
                   hci_spec::ACLPacketBoundaryFlag packet_boundary_flag,
                   hci_spec::ACLBroadcastFlag broadcast_flag);

  trace_flow_id_t async_id_;
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_TRANSPORT_ACL_DATA_PACKET_H_
