// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_device as fidl_device,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_tap as wlantap,
    ieee80211::{MacAddr, MacAddrBytes},
    std::convert::TryInto,
    wlan_common::ie::*,
    zerocopy::AsBytes,
};

pub(crate) fn create_wlantap_config(
    name: String,
    sta_addr: MacAddr,
    mac_role: fidl_common::WlanMacRole,
) -> wlantap::WlantapPhyConfig {
    wlantap::WlantapPhyConfig {
        // TODO(https://fxbug.dev/64628): wlantap will configure all of its ifaces to use the same MAC address
        sta_addr: sta_addr.to_array(),
        supported_phys: vec![
            fidl_common::WlanPhyType::Dsss,
            fidl_common::WlanPhyType::Hr,
            fidl_common::WlanPhyType::Ofdm,
            fidl_common::WlanPhyType::Erp,
            fidl_common::WlanPhyType::Ht,
        ],
        mac_role: mac_role,
        hardware_capability: 0,
        bands: vec![create_2_4_ghz_band_info()],
        name,
        quiet: false,
        discovery_support: fidl_common::DiscoverySupport {
            scan_offload: fidl_common::ScanOffloadExtension {
                supported: true,
                scan_cancel_supported: false,
            },
            probe_response_offload: fidl_common::ProbeResponseOffloadExtension { supported: false },
        },
        mac_sublayer_support: fidl_common::MacSublayerSupport {
            rate_selection_offload: fidl_common::RateSelectionOffloadExtension { supported: false },
            data_plane: fidl_common::DataPlaneExtension {
                data_plane_type: fidl_common::DataPlaneType::EthernetDevice,
            },
            device: fidl_common::DeviceExtension {
                is_synthetic: true,
                mac_implementation_type: fidl_common::MacImplementationType::Softmac,
                tx_status_report_supported: true,
            },
        },
        security_support: fidl_common::SecuritySupport {
            sae: fidl_common::SaeFeature {
                driver_handler_supported: false,
                sme_handler_supported: true,
            },
            mfp: fidl_common::MfpFeature { supported: true },
        },
        spectrum_management_support: fidl_common::SpectrumManagementSupport {
            dfs: fidl_common::DfsFeature { supported: false },
        },
    }
}

fn create_2_4_ghz_band_info() -> fidl_device::BandInfo {
    fidl_device::BandInfo {
        band: fidl_common::WlanBand::TwoGhz,
        ht_caps: Some(Box::new(fidl_ieee80211::HtCapabilities {
            bytes: fake_ht_capabilities().as_bytes().try_into().unwrap(),
        })),
        vht_caps: None,
        rates: vec![2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108],
        operating_channels: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
    }
}
