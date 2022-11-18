// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <endian.h>

#include "src/connectivity/bluetooth/core/bt-host/common/assert.h"

namespace bt::hci_spec {

std::string HCIVersionToString(HCIVersion version) {
  switch (version) {
    case HCIVersion::k1_0b:
      return "1.0b";
    case HCIVersion::k1_1:
      return "1.1";
    case HCIVersion::k1_2:
      return "1.2";
    case HCIVersion::k2_0_EDR:
      return "2.0 + EDR";
    case HCIVersion::k2_1_EDR:
      return "2.1 + EDR";
    case HCIVersion::k3_0_HS:
      return "3.0 + HS";
    case HCIVersion::k4_0:
      return "4.0";
    case HCIVersion::k4_1:
      return "4.1";
    case HCIVersion::k4_2:
      return "4.2";
    case HCIVersion::k5_0:
      return "5.0";
    default:
      break;
  }
  return "(unknown)";
}

// clang-format off
std::string StatusCodeToString(hci_spec::StatusCode code) {
  switch (code) {
    case kSuccess: return "success";
    case kUnknownCommand: return "unknown command";
    case kUnknownConnectionId: return "unknown connection ID";
    case kHardwareFailure: return "hardware failure";
    case kPageTimeout: return "page timeout";
    case kAuthenticationFailure: return "authentication failure";
    case kPinOrKeyMissing: return "pin or key missing";
    case kMemoryCapacityExceeded: return "memory capacity exceeded";
    case kConnectionTimeout: return "connection timeout";
    case kConnectionLimitExceeded: return "connection limit exceeded";
    case kSynchronousConnectionLimitExceeded: return "synchronous connection limit exceeded";
    case kConnectionAlreadyExists: return "connection already exists";
    case kCommandDisallowed: return "command disallowed";
    case kConnectionRejectedLimitedResources: return "connection rejected: limited resources";
    case kConnectionRejectedSecurity: return "connection rejected: security";
    case kConnectionRejectedBadBdAddr: return "connection rejected: bad BD_ADDR";
    case kConnectionAcceptTimeoutExceeded: return "connection accept timeout exceeded";
    case kUnsupportedFeatureOrParameter: return "unsupported feature or parameter";
    case kInvalidHCICommandParameters: return "invalid HCI command parameters";
    case kRemoteUserTerminatedConnection: return "remote user terminated connection";
    case kRemoteDeviceTerminatedConnectionLowResources: return "remote device terminated connection: low resources";
    case kRemoteDeviceTerminatedConnectionPowerOff: return "remote device terminated connection: power off";
    case kConnectionTerminatedByLocalHost: return "connection terminated by local host";
    case kRepeatedAttempts: return "repeated attempts";
    case kPairingNotAllowed: return "pairing not allowed";
    case kUnknownLMPPDU: return "unknown LMP PDU";
    case kUnsupportedRemoteFeature: return "unsupported remote feature";
    case kSCOOffsetRejected: return "SCO offset rejected";
    case kSCOIntervalRejected: return "SCO interval rejected";
    case kSCOAirModeRejected: return "SCO air mode rejected";
    case kInvalidLMPOrLLParameters: return "invalid LMP or LL parameters";
    case kUnspecifiedError: return "unspecified error";
    case kUnsupportedLMPOrLLParameterValue: return "unsupported LMP or LL parameter value";
    case kRoleChangeNotAllowed: return "role change not allowed";
    case kLMPOrLLResponseTimeout: return "LMP or LL response timeout";
    case kLMPErrorTransactionCollision: return "LMP error transaction collision";
    case kLMPPDUNotAllowed: return "LMP PDU not allowed";
    case kEncryptionModeNotAcceptable: return "encryption mode not acceptable";
    case kLinkKeyCannotBeChanged: return "link key cannot be changed";
    case kRequestedQoSNotSupported: return "requested QoS not supported";
    case kInstantPassed: return "instant passed";
    case kPairingWithUnitKeyNotSupported: return "pairing with unit key not supported";
    case kDifferentTransactionCollision: return "different transaction collision";
    case kQoSUnacceptableParameter: return "QoS unacceptable parameter";
    case kQoSRejected: return "QoS rejected";
    case kChannelClassificationNotSupported: return "channel classification not supported";
    case kInsufficientSecurity: return "insufficient security";
    case kParameterOutOfMandatoryRange: return "parameter out of mandatory range";
    case kRoleSwitchPending: return "role switch pending";
    case kReservedSlotViolation: return "reserved slot violation";
    case kRoleSwitchFailed: return "role switch failed";
    case kExtendedInquiryResponseTooLarge: return "extended inquiry response too large";
    case kSecureSimplePairingNotSupportedByHost: return "secure simple pairing not supported by host";
    case kHostBusyPairing: return "host busy pairing";
    case kConnectionRejectedNoSuitableChannelFound: return "connection rejected: no suitable channel found";
    case kControllerBusy: return "controller busy";
    case kUnacceptableConnectionParameters: return "unacceptable connection parameters";
    case kDirectedAdvertisingTimeout: return "directed advertising timeout";
    case kConnectionTerminatedMICFailure: return "connection terminated: MIC failure";
    case kConnectionFailedToBeEstablished: return "connection failed to be established";
    case kMACConnectionFailed: return "MAC connection failed";
    case kCoarseClockAdjustmentRejected: return "coarse clock adjustment rejected";
    case kType0SubmapNotDefined: return "type 0 submap not defined";
    case kUnknownAdvertisingIdentifier: return "unknown advertising identifier";
    case kLimitReached: return "limit reached";
    case kOperationCancelledByHost: return "operation cancelled by host";
    default: break;
  };
  return "unknown status";
}
// clang-format on

std::string LinkTypeToString(hci_spec::LinkType link_type) {
  switch (link_type) {
    case LinkType::kSCO:
      return "SCO";
    case LinkType::kACL:
      return "ACL";
    case LinkType::kExtendedSCO:
      return "eSCO";
    default:
      return "<Unknown LinkType>";
  };
}

std::string ConnectionRoleToString(hci_spec::ConnectionRole role) {
  switch (role) {
    case hci_spec::ConnectionRole::CENTRAL:
      return "central";
    case hci_spec::ConnectionRole::PERIPHERAL:
      return "peripheral";
    default:
      return "<unknown role>";
  }
}

// TODO(fxbug.dev/80048): various parts of the spec call for a 3 byte integer. If we need to in the
// future, we should generalize this logic and make a uint24_t type that makes it easier to work
// with these types of conversions.
void EncodeLegacyAdvertisingInterval(uint16_t input, uint8_t (&result)[3]) {
  MutableBufferView result_view(result, sizeof(result));
  result_view.SetToZeros();

  // Core spec Volume 6, Part B, Section 1.2: Link layer order is little endian, convert to little
  // endian if host order is big endian
  input = htole16(input);
  BufferView input_view(&input, sizeof(input));

  input_view.Copy(&result_view, 0, sizeof(input));
}

// TODO(fxbug.dev/80048): various parts of the spec call for a 3 byte integer. If we need to in the
// future, we should generalize this logic and make a uint24_t type that makes it easier to work
// with these types of conversions.
uint32_t DecodeExtendedAdvertisingInterval(const uint8_t (&input)[3]) {
  uint32_t result = 0;
  MutableBufferView result_view(&result, sizeof(result));

  BufferView input_view(input, sizeof(input));
  input_view.Copy(&result_view);

  // Core spec Volume 6, Part B, Section 1.2: Link layer order is little endian, convert to little
  // endian if host order is big endian
  return letoh32(result);
}

std::optional<AdvertisingEventBits> AdvertisingTypeToEventBits(LEAdvertisingType type) {
  // TODO(fxbug.dev/81470): for backwards compatibility and because supporting extended advertising
  // PDUs is a much larger project, we currently only support legacy PDUs. Without using legacy
  // PDUs, non-Bluetooth 5 devices will not be able to discover extended advertisements.
  uint16_t adv_event_properties = kLEAdvEventPropBitUseLegacyPDUs;

  // Bluetooth Spec Volume 4, Part E, Section 7.8.53, Table 7.2 defines the mapping of legacy PDU
  // types to the corresponding bits within adv_event_properties.
  switch (type) {
    case LEAdvertisingType::kAdvInd:
      adv_event_properties |= kLEAdvEventPropBitConnectable;
      adv_event_properties |= kLEAdvEventPropBitScannable;
      break;
    case LEAdvertisingType::kAdvDirectIndLowDutyCycle:
      adv_event_properties |= kLEAdvEventPropBitConnectable;
      adv_event_properties |= kLEAdvEventPropBitDirected;
      break;
    case LEAdvertisingType::kAdvDirectIndHighDutyCycle:
      adv_event_properties |= kLEAdvEventPropBitConnectable;
      adv_event_properties |= kLEAdvEventPropBitDirected;
      adv_event_properties |= kLEAdvEventPropBitHighDutyCycleDirectedConnectable;
      break;
    case LEAdvertisingType::kAdvScanInd:
      adv_event_properties |= kLEAdvEventPropBitScannable;
      break;
    case LEAdvertisingType::kAdvNonConnInd:
      // no extra bits to set
      break;
    default:
      return std::nullopt;
  }

  return adv_event_properties;
}

}  // namespace bt::hci_spec
