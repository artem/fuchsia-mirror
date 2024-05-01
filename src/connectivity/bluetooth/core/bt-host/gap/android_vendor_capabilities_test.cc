// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/android_vendor_capabilities.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/emboss_control_packets.h"

namespace bt::gap {
namespace {

namespace android_emb = pw::bluetooth::vendor::android_hci;
namespace pwemb = pw::bluetooth::emboss;

using android_emb::Capability;

TEST(AndroidVendorCapabilitiesTest, NonSuccess) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_0_55_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pw::bluetooth::emboss::StatusCode::UNKNOWN_COMMAND);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(0u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(false, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(0u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(0u, capabilities.irk_list_size());
  EXPECT_EQ(false, capabilities.supports_filtering());
  EXPECT_EQ(0u, capabilities.max_filters());
  EXPECT_EQ(false, capabilities.supports_activity_energy_info());
  EXPECT_EQ(0u, capabilities.version_major());
  EXPECT_EQ(0u, capabilities.version_minor());
  EXPECT_EQ(0u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(false, capabilities.supports_extended_scan());
  EXPECT_EQ(false, capabilities.supports_debug_logging());
  EXPECT_EQ(false, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(0u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(false, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(0u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version055) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_0_55_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(0u, capabilities.version_major());
  EXPECT_EQ(55u, capabilities.version_minor());
  EXPECT_EQ(0u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(false, capabilities.supports_extended_scan());
  EXPECT_EQ(false, capabilities.supports_debug_logging());
  EXPECT_EQ(false, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(0u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(false, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(0u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version095) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_0_95_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);
  view.version_supported().major_number().Write(0);
  view.version_supported().minor_number().Write(95);

  ASSERT_TRUE(view.has_total_num_of_advt_tracked().ValueOr(false));
  view.total_num_of_advt_tracked().UncheckedWrite(5u);

  ASSERT_TRUE(view.has_extended_scan_support().ValueOr(false));
  view.extended_scan_support().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(view.has_debug_logging_supported().ValueOr(false));
  view.debug_logging_supported().UncheckedWrite(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(0u, capabilities.version_major());
  EXPECT_EQ(95u, capabilities.version_minor());
  EXPECT_EQ(5u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(true, capabilities.supports_extended_scan());
  EXPECT_EQ(true, capabilities.supports_debug_logging());
  EXPECT_EQ(false, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(0u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(false, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(0u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version096) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_0_96_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);
  view.version_supported().major_number().Write(0);
  view.version_supported().minor_number().Write(96);

  ASSERT_TRUE(view.has_total_num_of_advt_tracked().ValueOr(false));
  view.total_num_of_advt_tracked().UncheckedWrite(5u);

  ASSERT_TRUE(view.has_extended_scan_support().ValueOr(false));
  view.extended_scan_support().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(view.has_debug_logging_supported().ValueOr(false));
  view.debug_logging_supported().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(
      view.has_le_address_generation_offloading_support().ValueOr(false));
  view.le_address_generation_offloading_support().Write(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(0u, capabilities.version_major());
  EXPECT_EQ(96u, capabilities.version_minor());
  EXPECT_EQ(5u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(true, capabilities.supports_extended_scan());
  EXPECT_EQ(true, capabilities.supports_debug_logging());
  EXPECT_EQ(true, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(0u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(false, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(0u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version098) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_0_98_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);
  view.version_supported().major_number().Write(0);
  view.version_supported().minor_number().Write(98);

  ASSERT_TRUE(view.has_total_num_of_advt_tracked().ValueOr(false));
  view.total_num_of_advt_tracked().UncheckedWrite(5u);

  ASSERT_TRUE(view.has_extended_scan_support().ValueOr(false));
  view.extended_scan_support().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(view.has_debug_logging_supported().ValueOr(false));
  view.debug_logging_supported().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(
      view.has_le_address_generation_offloading_support().ValueOr(false));
  view.le_address_generation_offloading_support().Write(Capability::CAPABLE);

  ASSERT_TRUE(view.has_a2dp_source_offload_capability_mask().ValueOr(false));
  view.a2dp_source_offload_capability_mask().BackingStorage().WriteUInt(6);

  ASSERT_TRUE(view.has_bluetooth_quality_report_support().ValueOr(false));
  view.bluetooth_quality_report_support().Write(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(0u, capabilities.version_major());
  EXPECT_EQ(98u, capabilities.version_minor());
  EXPECT_EQ(5u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(true, capabilities.supports_extended_scan());
  EXPECT_EQ(true, capabilities.supports_debug_logging());
  EXPECT_EQ(true, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(6u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(true, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(0u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version103) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_1_03_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);
  view.version_supported().major_number().Write(1);
  view.version_supported().minor_number().Write(03);

  ASSERT_TRUE(view.has_total_num_of_advt_tracked().ValueOr(false));
  view.total_num_of_advt_tracked().UncheckedWrite(5u);

  ASSERT_TRUE(view.has_extended_scan_support().ValueOr(false));
  view.extended_scan_support().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(view.has_debug_logging_supported().ValueOr(false));
  view.debug_logging_supported().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(
      view.has_le_address_generation_offloading_support().ValueOr(false));
  view.le_address_generation_offloading_support().Write(Capability::CAPABLE);

  ASSERT_TRUE(view.has_a2dp_source_offload_capability_mask().ValueOr(false));
  view.a2dp_source_offload_capability_mask().BackingStorage().WriteUInt(6);

  ASSERT_TRUE(view.has_bluetooth_quality_report_support().ValueOr(false));
  view.bluetooth_quality_report_support().Write(Capability::CAPABLE);

  ASSERT_TRUE(view.has_dynamic_audio_buffer_support().ValueOr(false));
  view.dynamic_audio_buffer_support().sbc().Write(true);
  view.dynamic_audio_buffer_support().aac().Write(true);
  view.dynamic_audio_buffer_support().aptx().Write(true);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(1u, capabilities.version_major());
  EXPECT_EQ(03u, capabilities.version_minor());
  EXPECT_EQ(5u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(true, capabilities.supports_extended_scan());
  EXPECT_EQ(true, capabilities.supports_debug_logging());
  EXPECT_EQ(true, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(6u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(true, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(7u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(false, capabilities.supports_a2dp_offload_v2());
}

TEST(AndroidVendorCapabilitiesTest, Version104) {
  auto params = hci::EmbossEventPacket::New<
      android_emb::LEGetVendorCapabilitiesCommandCompleteEventWriter>(
      hci_spec::kCommandCompleteEventCode,
      android_emb::LEGetVendorCapabilitiesCommandCompleteEvent::
          version_1_04_size());

  auto view = params.unchecked_view_t();
  view.status().Write(pwemb::StatusCode::SUCCESS);

  view.max_advt_instances().Write(1);
  view.offloaded_resolution_of_private_address().Write(Capability::CAPABLE);
  view.total_scan_results_storage().Write(2);
  view.max_irk_list_sz().Write(3);
  view.filtering_support().Write(Capability::CAPABLE);
  view.max_filter().Write(4);
  view.activity_energy_info_support().Write(Capability::CAPABLE);
  view.version_supported().major_number().Write(1);
  view.version_supported().minor_number().Write(04);

  ASSERT_TRUE(view.has_total_num_of_advt_tracked().ValueOr(false));
  view.total_num_of_advt_tracked().UncheckedWrite(5u);

  ASSERT_TRUE(view.has_extended_scan_support().ValueOr(false));
  view.extended_scan_support().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(view.has_debug_logging_supported().ValueOr(false));
  view.debug_logging_supported().UncheckedWrite(Capability::CAPABLE);

  ASSERT_TRUE(
      view.has_le_address_generation_offloading_support().ValueOr(false));
  view.le_address_generation_offloading_support().Write(Capability::CAPABLE);

  ASSERT_TRUE(view.has_a2dp_source_offload_capability_mask().ValueOr(false));
  view.a2dp_source_offload_capability_mask().BackingStorage().WriteUInt(6);

  ASSERT_TRUE(view.has_bluetooth_quality_report_support().ValueOr(false));
  view.bluetooth_quality_report_support().Write(Capability::CAPABLE);

  ASSERT_TRUE(view.has_dynamic_audio_buffer_support().ValueOr(false));
  view.dynamic_audio_buffer_support().sbc().Write(true);
  view.dynamic_audio_buffer_support().aac().Write(true);
  view.dynamic_audio_buffer_support().aptx().Write(true);

  ASSERT_TRUE(view.has_a2dp_offload_v2_support().ValueOr(false));
  view.a2dp_offload_v2_support().Write(Capability::CAPABLE);

  AndroidVendorCapabilities capabilities = AndroidVendorCapabilities::New(view);
  EXPECT_EQ(1u, capabilities.max_simultaneous_advertisements());
  EXPECT_EQ(true, capabilities.supports_offloaded_rpa());
  EXPECT_EQ(2u, capabilities.scan_results_storage_bytes());
  EXPECT_EQ(3u, capabilities.irk_list_size());
  EXPECT_EQ(true, capabilities.supports_filtering());
  EXPECT_EQ(4u, capabilities.max_filters());
  EXPECT_EQ(true, capabilities.supports_activity_energy_info());
  EXPECT_EQ(1u, capabilities.version_major());
  EXPECT_EQ(04u, capabilities.version_minor());
  EXPECT_EQ(5u, capabilities.max_advertisers_tracked());
  EXPECT_EQ(true, capabilities.supports_extended_scan());
  EXPECT_EQ(true, capabilities.supports_debug_logging());
  EXPECT_EQ(true, capabilities.supports_offloading_le_address_generation());
  EXPECT_EQ(6u, capabilities.a2dp_source_offload_capability_mask());
  EXPECT_EQ(true, capabilities.supports_bluetooth_quality_report());
  EXPECT_EQ(7u, capabilities.supports_dynamic_audio_buffer());
  EXPECT_EQ(true, capabilities.supports_a2dp_offload_v2());
}

}  // namespace
}  // namespace bt::gap
