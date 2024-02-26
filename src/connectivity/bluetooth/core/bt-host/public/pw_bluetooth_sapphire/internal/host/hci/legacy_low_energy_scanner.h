// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_SCANNER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_SCANNER_H_

#include <memory>
#include <unordered_map>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci/low_energy_scanner.h"

namespace bt::hci {

class LocalAddressDelegate;

// LegacyLowEnergyScanner implements the LowEnergyScanner interface for
// controllers that do not support the 5.0 Extended Advertising feature. This
// uses the legacy HCI LE scan commands and events:
//
//     - HCI_LE_Set_Scan_Parameters
//     - HCI_LE_Set_Scan_Enable
//     - HCI_LE_Advertising_Report event
class LegacyLowEnergyScanner final : public LowEnergyScanner {
 public:
  LegacyLowEnergyScanner(LocalAddressDelegate* local_addr_delegate,
                         Transport::WeakPtr transport,
                         pw::async::Dispatcher& pw_dispatcher);
  ~LegacyLowEnergyScanner() override;

  bool StartScan(const ScanOptions& options,
                 ScanStatusCallback callback) override;

 private:
  // Build the HCI command packet to set the scan parameters for the flavor of
  // low energy scanning being implemented.
  EmbossCommandPacket BuildSetScanParametersPacket(
      const DeviceAddress& local_address, const ScanOptions& options) override;

  // Build the HCI command packet to enable scanning for the flavor of low
  // energy scanning being implemented.
  EmbossCommandPacket BuildEnablePacket(
      const ScanOptions& options,
      pw::bluetooth::emboss::GenericEnableParam enable) override;

  // Called when a Scan Response is received during an active scan or when we
  // time out waiting
  void HandleScanResponse(const DeviceAddress& address,
                          bool resolved,
                          int8_t rssi,
                          const ByteBuffer& data);

  // Event handler for HCI LE Advertising Report event.
  void OnAdvertisingReportEvent(const EventPacket& event);

  // Our event handler ID for the LE Advertising Report event.
  CommandChannel::EventHandlerId event_handler_id_;

  // Keep this as the last member to make sure that all weak pointers are
  // invalidated before other members get destroyed
  WeakSelf<LegacyLowEnergyScanner> weak_self_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(LegacyLowEnergyScanner);
};

}  // namespace bt::hci

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_LEGACY_LOW_ENERGY_SCANNER_H_
