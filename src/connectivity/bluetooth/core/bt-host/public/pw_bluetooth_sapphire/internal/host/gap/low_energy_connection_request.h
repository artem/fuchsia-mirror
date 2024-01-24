// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_CONNECTION_REQUEST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_CONNECTION_REQUEST_H_

#include <lib/fit/function.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/device_address.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/inspectable.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/low_energy_connection_handle.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/low_energy_discovery_manager.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/peer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/types.h"

namespace bt::gap {

// Connection options for a LowEnergyConnectionRequest.
// TODO(https://fxbug.dev/66696): Move back into LowEnergyConnectionManager
// after dependency removed from LowEnergyConnection.
struct LowEnergyConnectionOptions {
  // The sm::BondableMode to connect with.
  sm::BondableMode bondable_mode = sm::BondableMode::Bondable;

  // When present, service discovery performed following the connection is
  // restricted to primary services that match this field. Otherwise, by default
  // all available services are discovered.
  std::optional<UUID> service_uuid = std::nullopt;

  // When true, skip scanning before connecting. This should only be true when
  // the connection is initiated as a result of a directed advertisement.
  bool auto_connect = false;
};

namespace internal {
// LowEnergyConnectionRequest is used to model queued outbound connection and
// interrogation requests in both LowEnergyConnectionManager and
// LowEnergyConnection. Duplicate connection request callbacks are added with
// |AddCallback|, and |NotifyCallbacks| is called when the request is completed.
class LowEnergyConnectionRequest final {
 public:
  using ConnectionResult =
      fit::result<HostError, std::unique_ptr<LowEnergyConnectionHandle>>;
  using ConnectionResultCallback = fit::function<void(ConnectionResult)>;

  // |peer_conn_state_token| is a token generated by the peer with ID |peer_id|,
  // and is used to synchronize connection state.
  LowEnergyConnectionRequest(PeerId peer_id,
                             ConnectionResultCallback first_callback,
                             LowEnergyConnectionOptions connection_options,
                             Peer::InitializingConnectionToken peer_conn_token);
  ~LowEnergyConnectionRequest() = default;

  LowEnergyConnectionRequest(LowEnergyConnectionRequest&&) = default;
  LowEnergyConnectionRequest& operator=(LowEnergyConnectionRequest&&) = default;

  void AddCallback(ConnectionResultCallback cb) {
    callbacks_.Mutable()->push_back(std::move(cb));
  }

  // Notifies all elements in |callbacks| with |status| and the result of
  // |func|.
  using RefFunc = fit::function<std::unique_ptr<LowEnergyConnectionHandle>()>;
  void NotifyCallbacks(fit::result<HostError, RefFunc> result);

  // Attach request inspect node as a child node of |parent| with the name
  // |name|.
  void AttachInspect(inspect::Node& parent, std::string name);

  PeerId peer_id() const { return *peer_id_; }

  LowEnergyConnectionOptions connection_options() const {
    return connection_options_;
  }

  void set_discovery_session(LowEnergyDiscoverySessionPtr session) {
    session_ = std::move(session);
  }

  LowEnergyDiscoverySession* discovery_session() { return session_.get(); }

 private:
  StringInspectable<PeerId> peer_id_;
  IntInspectable<std::list<ConnectionResultCallback>> callbacks_;
  LowEnergyConnectionOptions connection_options_;
  LowEnergyDiscoverySessionPtr session_;
  inspect::Node inspect_node_;

  // This object's destructor notifies Peer of request destruction.
  std::optional<Peer::InitializingConnectionToken> peer_conn_token_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LowEnergyConnectionRequest);
};

}  // namespace internal
}  // namespace bt::gap

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_LOW_ENERGY_CONNECTION_REQUEST_H_
