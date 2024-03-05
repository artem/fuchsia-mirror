// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_VENDOR_PROTOCOL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_VENDOR_PROTOCOL_H_

// This file contains general opcode/number and static packet definitions for
// extensions to the Bluetooth Host-Controller interface. These extensions
// aren't standardized through the Bluetooth SIG and their documentation is
// available separately (linked below). Each packet payload structure contains
// parameter descriptions based on their respective documentation.
//
// Documentation links:
//
//    - Android: https://source.android.com/devices/bluetooth/hci_requirements
//
// NOTE: The definitions below are incomplete. They get added as needed. This
// list will grow as we support more vendor features.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"

#include <pw_bluetooth/hci_vendor.emb.h>

namespace bt::hci_spec::vendor::android {

// ============================================================================
// LE Get Vendor Capabilities Command
constexpr OpCode kLEGetVendorCapabilities = VendorOpCode(0x153);

// ============================================================================
// A2DP Offload Commands

// The kA2dpOffloadCommand opcode is shared across all a2dp offloading HCI
// commands. To differentiate between the multiple commands, a subopcode field
// is included in the command payload.
constexpr OpCode kA2dpOffloadCommand = VendorOpCode(0x15D);
constexpr uint8_t kStartA2dpOffloadCommandSubopcode = 0x01;
constexpr uint8_t kStopA2dpOffloadCommandSubopcode = 0x02;
constexpr uint32_t kLdacVendorId = 0x0000012D;
constexpr uint16_t kLdacCodecId = 0x00AA;

// ============================================================================
// Multiple Advertising
//
// NOTE: Multiple advertiser support is deprecated in the Google feature spec
// v0.98 and above. Users of the following vendor extension HCI commands should
// first ensure that the controller is using a compatible Google feature spec.

// The kLEMultiAdvt opcode is shared across all multiple advertising HCI
// commands. To differentiate between the multiple commands, a subopcode field
// is included in the command payload.
constexpr OpCode kLEMultiAdvt = VendorOpCode(0x154);
constexpr uint8_t kLEMultiAdvtSetAdvtParamSubopcode = 0x01;
constexpr uint8_t kLEMultiAdvtSetAdvtDataSubopcode = 0x02;
constexpr uint8_t kLEMultiAdvtSetScanRespSubopcode = 0x03;
constexpr uint8_t kLEMultiAdvtSetRandomAddrSubopcode = 0x4;
constexpr uint8_t kLEMultiAdvtEnableSubopcode = 0x05;
constexpr EventCode kLEMultiAdvtStateChangeSubeventCode = 0x55;

// ============================================================================
// Advertising Packet Content Filtering

// The kLEApcf opcode is shared across all advertising packet content filtering
// HCI commands. To differentiate between the multiple commands, a subopcode
// field is included in the command payload. These subopcode fields must be set
// to a specific value.
constexpr OpCode kLEApcf = VendorOpCode(0x157);
constexpr uint8_t kLEApcfEnableSubopcode = 0x00;
constexpr uint8_t kLEApcfSetFilteringParametersSubopcode = 0x01;
constexpr uint8_t kLEApcfBroadcastAddressSubopcode = 0x02;
constexpr uint8_t kLEApcfServiceUUIDSubopcode = 0x03;
constexpr uint8_t kLEApcfServiceSolicitationUUIDSubopcode = 0x04;
constexpr uint8_t kLEApcfLocalNameSubopcode = 0x05;
constexpr uint8_t kLEApcfManufacturerDataSubopcode = 0x06;
constexpr uint8_t kLEApcfServiceDataSubopcode = 0x07;
constexpr uint8_t kLEApcfTransportDiscoveryService = 0x08;
constexpr uint8_t kLEApcfAdTypeFilter = 0x09;
constexpr uint8_t kLEApcfReadExtendedFeatures = 0xFF;

// The maximum length of an advertising data field. The value 29 is selected
// because advertising data can be between 0 - 31 bytes wide.
//
//    - Byte 0: length of the advertising data itself.
//    - Byte 1: advertising data type (e.g. 0xFF for manufacturer data)
//
// With two bytes used, the rest of the payload size can only take up a maximum
// of 29 bytes.
constexpr uint8_t kLEApcfMaxPDUValueLength = 29;

}  // namespace bt::hci_spec::vendor::android

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_HCI_SPEC_VENDOR_PROTOCOL_H_
