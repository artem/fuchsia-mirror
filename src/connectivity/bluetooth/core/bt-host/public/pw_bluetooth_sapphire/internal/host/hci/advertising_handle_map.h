// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_ADVERTISING_HANDLE_MAP_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_ADVERTISING_HANDLE_MAP_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"

namespace bt::hci {

// Extended advertising HCI commands refer to a particular advertising set via
// an AdvertisingHandle. An AdvertisingHandle is an eight bit unsigned integer
// that uniquely identifies an advertising set and vice versa. This means that
// we frequently need to convert between a DeviceAddress and an
// AdvertisingHandle. AdvertisingHandleMap provides a 1:1 bidirectional mapping
// between a DeviceAddress and an AdvertisingHandle, allocating the next
// available AdvertisingHandle to new DeviceAddresses.
//
// When using extended advertising, there are two types of advertising PDU
// formats available: legacy PDUs and extended PDUs. Legacy advertising PDUs are
// currently the most widely compatible type, discoverable by devices deployed
// prior to the adoption of Bluetooth 5.0. Devices deployed more recently are
// also able to discover this type of advertising packet. Conversely, extended
// advertising PDUs are a newer format that offers a number of improvements,
// including the ability to advertise larger amounts of data. However, devices
// not specifically scanning for them or who are running on an older version of
// Bluetooth (pre-5.0) won't be able to see them.
//
// When advertising using extended advertising PDUs, users often choose to emit
// legacy advertising PDUs as well in order to maintain backwards compatibility
// with older Bluetooth devices. As such, advertisers such as
// ExtendedLowEnergyAdvertiser may need to track two real AdvertisingHandles
// for each logical advertisement, one for legacy advertising PDUs and one for
// extended advertising PDUs. Along with DeviceAddress, AdvertisingHandleMap
// tracks whether the mapping is for an extended PDU or a legacy PDU.
//
// Users shouldn't rely on any particular ordering of the next available
// mapping. Any available AdvertisingHandle may be used.
class AdvertisingHandleMap {
 public:
  // Instantiate an AdvertisingHandleMap. The capacity parameter specifies the
  // maximum number of mappings that this instance will support. Setting the
  // capacity also restricts the range of advertising handles
  // AdvertisingHandleMap will return: [0, capacity).
  explicit AdvertisingHandleMap(
      uint8_t capacity = hci_spec::kMaxAdvertisingHandle + 1)
      : capacity_(capacity) {}

  // Convert a DeviceAddress to an AdvertisingHandle, creating the mapping if it
  // doesn't already exist. The conversion may fail if there are already
  // hci_spec::kMaxAdvertisingHandles in the container.
  std::optional<hci_spec::AdvertisingHandle> MapHandle(
      const DeviceAddress& address, bool extended_pdu);

  // Convert a DeviceAddress to an AdvertisingHandle. The conversion may fail if
  // there is no AdvertisingHandle currently mapping to the provided device
  // address.
  std::optional<hci_spec::AdvertisingHandle> GetHandle(
      const DeviceAddress& address, bool extended_pdu) const;

  // Convert an AdvertisingHandle to a DeviceAddress. The conversion may fail if
  // there is no DeviceAddress currently mapping to the provided handle.
  std::optional<DeviceAddress> GetAddress(
      hci_spec::AdvertisingHandle handle) const;

  // Remove the mapping between an AdvertisingHandle and the DeviceAddress it
  // maps to. The container may reuse the AdvertisingHandle for other
  // DeviceAddresses in the future. Immediate future calls to GetAddress(...)
  // with the same AdvertisingHandle will fail because the mapping no longer
  // exists. Immediate future calls to GetHandle(...) will result in a new
  // mapping with a new AdvertisingHandle.
  //
  // If the given handle doesn't map to any (DeviceAddress, bool) tuple, this
  // function does nothing.
  void RemoveHandle(hci_spec::AdvertisingHandle handle);

  // Remove the mapping between a DeviceAddress and the AdvertisingHandle it
  // maps to. The container may reuse the AdvertisingHandle for other
  // DeviceAddresses in the future. Immediate future calls to GetAddress(...)
  // with the preivously mapped AdvertisingHandle will fail because the mapping
  // no longer exists. Immediate future calls to GetHandle(...) will result in a
  // new mapping with a new AdvertisingHandle.
  //
  // If the given (DeviceAddress, bool) tuple doesn't map to any
  // AdvertisingHandle, this function does nothing.
  void RemoveAddress(const DeviceAddress& address, bool extended_pdu);

  // Get the maximum number of mappings the AdvertisingHandleMap will support.
  uint8_t capacity() const { return capacity_; }

  // Retrieve the advertising handle that was most recently generated. This
  // function is primarily used by unit tests so as to avoid hardcoding values
  // or making assumptions about the starting point or ordering of advertising
  // handle generation.
  std::optional<hci_spec::AdvertisingHandle> LastUsedHandleForTesting() const;

  // Get the number of unique mappings in the container
  std::size_t Size() const;

  // Return true if the container has no mappings, false otherwise
  bool Empty() const;

  // Remove all mappings in the container
  void Clear();

 private:
  // Although not in the range of valid advertising handles (0x00 to 0xEF),
  // kStartHandle is chosen to be 0xFF because adding one to it will overflow to
  // 0, the first valid advertising handle.
  constexpr static hci_spec::AdvertisingHandle kStartHandle = 0xFF;

  // Tracks the maximum number of elements that can be stored in this container.
  //
  // NOTE: AdvertisingHandles have a range of [0, capacity_). This value isn't
  // set using default member initialization because it is set within the
  // constructor itself.
  uint8_t capacity_;

  // Generate the next valid, available, and within range AdvertisingHandle.
  // This function may fail if there are already
  // hci_spec::kMaxAdvertisingHandles in the container: there are no more valid
  // AdvertisingHandles.
  std::optional<hci_spec::AdvertisingHandle> NextHandle();

  // The last generated advertising handle used as a hint to generate the next
  // handle; defaults to kStartHandle if no handles have been generated yet.
  hci_spec::AdvertisingHandle last_handle_ = kStartHandle;

  struct TupleKeyHasher {
    size_t operator()(const std::tuple<DeviceAddress, bool>& t) const {
      std::hash<DeviceAddress> device_address_hasher;
      std::hash<bool> bool_hasher;
      const auto& [address, extended_pdu] = t;
      return device_address_hasher(address) ^ bool_hasher(extended_pdu);
    }
  };

  std::unordered_map<std::tuple<DeviceAddress, bool>,
                     hci_spec::AdvertisingHandle,
                     TupleKeyHasher>
      addr_to_handle_;

  std::unordered_map<hci_spec::AdvertisingHandle,
                     std::tuple<DeviceAddress, bool>>
      handle_to_addr_;
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_ADVERTISING_HANDLE_MAP_H_
