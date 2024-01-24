// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_BONDING_DATA_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_BONDING_DATA_H_

#include <vector>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/uuid.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/peer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/types.h"

namespace bt::gap {

// A |BondingData| struct can be passed to the peer cache and allows for
// flexibility in adding new fields to cache.
struct BondingData {
  PeerId identifier;
  DeviceAddress address;
  std::optional<std::string> name;

  // TODO(https://fxbug.dev/2761): This should be optional to represent whether
  // the FIDL bonding data has LE data, instead of using DeviceAddresss's type()
  sm::PairingData le_pairing_data;
  std::optional<sm::LTK> bredr_link_key;
  std::vector<UUID> bredr_services;
};

}  // namespace bt::gap

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_GAP_BONDING_DATA_H_
