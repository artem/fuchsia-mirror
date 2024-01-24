// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LOCAL_ADDRESS_DELEGATE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LOCAL_ADDRESS_DELEGATE_H_

#include <lib/fit/function.h>

#include <optional>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/device_address.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/uint128.h"

namespace bt::hci {

// Delegate interface for obtaining the host-maintained local address and
// identity information for the system.
class LocalAddressDelegate {
 public:
  virtual ~LocalAddressDelegate() = default;

  // Returns the currently assigned IRK, if any.
  virtual std::optional<UInt128> irk() const = 0;

  // Returns the identity address.
  virtual DeviceAddress identity_address() const = 0;

  // Asynchronously returns the local LE controller address used by all LE link
  // layer procedures with the exception of 5.0 advertising sets. These include:
  //   - Legacy and extended scan requests;
  //   - Legacy and extended connection initiation;
  //   - Legacy advertising.
  //
  // There are two kinds of address that can be returned by this function:
  //   - A public device address (BD_ADDR) shared with the BR/EDR transport and
  //     typically factory-assigned.
  //   - A random device address that has been assigned to the controller by the
  //     host using the HCI_LE_Set_Random_Address command.
  //
  // This method runs |callback| when the procedure ends. |callback| may run
  // synchronously or asynchronously.
  using AddressCallback = fit::function<void(const DeviceAddress&)>;
  virtual void EnsureLocalAddress(AddressCallback callback) = 0;
};

// Interface to be implemented by all objects that are interested in and/or can
// prevent the configuration of a local private address.
class LocalAddressClient {
 public:
  virtual ~LocalAddressClient() = default;

  // Returns true if the procedures managed by this client do not currently
  // prevent the reconfiguration of the controller LE random address.
  virtual bool AllowsRandomAddressChange() const = 0;
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LOCAL_ADDRESS_DELEGATE_H_
