// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/android_vendor_capabilities.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"

namespace bt::gap {

namespace android_hci = pw::bluetooth::vendor::android_hci;

namespace {
bool AsBool(const android_hci::Capability& capability) {
  return capability == android_hci::Capability::CAPABLE;
}
}  // namespace

bool AndroidVendorCapabilities::SupportsVersion(uint8_t major,
                                                uint8_t minor) const {
  if (version_major_ > major) {
    return true;
  }

  if (version_major_ == major && version_minor_ >= minor) {
    return true;
  }

  return false;
}

AndroidVendorCapabilities AndroidVendorCapabilities::New(
    const android_hci::LEGetVendorCapabilitiesCommandCompleteEventView& e) {
  AndroidVendorCapabilities c;

  if (e.status().Read() != pw::bluetooth::emboss::StatusCode::SUCCESS) {
    bt_log(INFO,
           "android_vendor_extensions",
           "refusing to parse non-success vendor capabilities");
    return c;
  }

  // Version 0.55
  c.max_simultaneous_advertisement_ = e.max_advt_instances().Read();
  c.supports_offloaded_rpa_ =
      AsBool(e.offloaded_resolution_of_private_address().Read());
  c.scan_results_storage_bytes_ = e.total_scan_results_storage().Read();
  c.irk_list_size_ = e.max_irk_list_sz().Read();
  c.supports_filtering_ = AsBool(e.filtering_support().Read());
  c.max_filters_ = e.max_filter().Read();
  c.supports_activity_energy_info_ =
      AsBool(e.activity_energy_info_support().Read());

  // There can be various versions of this command complete event sent by the
  // Controller. As fields are added, the version_supported field is incremented
  // to signify which fields are available. However, version_supported was only
  // introduced into the command in Version 0.95. Controllers that use the base
  // version, Version 0.55, don't have the version_supported field. As such, we
  // must jump through some hoops to figure out which version we received,
  // exactly.
  //
  // NOTE: Android's definition for this command complete event is available in
  // AOSP: LeGetVendorCapabilitiesComplete and friends
  // https://cs.android.com/android/platform/superproject/+/main:packages/modules/Bluetooth/system/gd/hci/hci_packets.pdl
  //
  // NOTE: An example implementation of how this command is filled in by a
  // Controller is available within AOSP:
  // le_get_vendor_capabilities_handler(...)
  // https://cs.android.com/android/platform/superproject/main/+/main:packages/modules/Bluetooth/system/gd/hci/controller.cc
  if (e.version_supported().major_number().IsComplete()) {
    c.version_major_ = e.version_supported().major_number().Read();
  }

  if (e.version_supported().minor_number().IsComplete()) {
    c.version_minor_ = e.version_supported().minor_number().Read();
  }

  // If we didn't receive a version number from the Controller, assume it's the
  // base version, Version 0.55.
  if (c.version_major_ == 0 && c.version_minor_ == 0) {
    c.version_minor_ = 55;
  }

  // Version 0.95
  if (c.SupportsVersion(0, 95)) {
    c.max_advertisers_tracked_ = e.total_num_of_advt_tracked().Read();
    c.supports_extended_scan_ = AsBool(e.extended_scan_support().Read());
    c.supports_debug_logging_ = AsBool(e.debug_logging_supported().Read());
  }

  // Version 0.96
  if (c.SupportsVersion(0, 96)) {
    c.supports_offloading_le_address_generation_ =
        AsBool(e.le_address_generation_offloading_support().Read());
  }

  // Version 0.98
  if (c.SupportsVersion(0, 98)) {
    c.a2dp_source_offload_capability_mask_ =
        e.a2dp_source_offload_capability_mask().BackingStorage().ReadUInt();
    c.supports_bluetooth_quality_report_ =
        AsBool(e.bluetooth_quality_report_support().Read());
  }

  // Version 1.03
  if (c.SupportsVersion(1, 03)) {
    c.supports_dynamic_audio_buffer_ =
        e.dynamic_audio_buffer_support().BackingStorage().ReadUInt();
  }

  // Version 1.04
  if (c.SupportsVersion(1, 04)) {
    c.a2dp_offload_v2_support_ = AsBool(e.a2dp_offload_v2_support().Read());
  }

  return c;
}

}  // namespace bt::gap
