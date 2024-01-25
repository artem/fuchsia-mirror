// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::WlanSoftmacBandCapabilityExt as _,
    anyhow::{format_err, Error},
    banjo_fuchsia_wlan_common as banjo_common, banjo_fuchsia_wlan_softmac as banjo_wlan_softmac,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_softmac as fidl_softmac,
    std::fmt::Display,
};

#[macro_export]
macro_rules! zeroed_array_from_prefix {
    ($slice:expr, $size:expr $(,)?) => {{
        assert!($slice.len() <= $size);
        let mut a = [0; $size];
        a[..$slice.len()].clone_from_slice(&$slice);
        a
    }};
}

pub fn softmac_key_configuration_from_mlme(
    key_descriptor: fidl_mlme::SetKeyDescriptor,
) -> fidl_softmac::WlanKeyConfiguration {
    fidl_softmac::WlanKeyConfiguration {
        protection: Some(fidl_softmac::WlanProtection::RxTx),
        cipher_oui: Some(key_descriptor.cipher_suite_oui),
        cipher_type: Some(fidl_ieee80211::CipherSuiteType::into_primitive(
            key_descriptor.cipher_suite_type,
        ) as u8),
        key_type: Some(match key_descriptor.key_type {
            fidl_mlme::KeyType::Pairwise => fidl_common::WlanKeyType::Pairwise,
            fidl_mlme::KeyType::PeerKey => fidl_common::WlanKeyType::Peer,
            fidl_mlme::KeyType::Igtk => fidl_common::WlanKeyType::Igtk,
            fidl_mlme::KeyType::Group => fidl_common::WlanKeyType::Group,
        }),
        peer_addr: Some(key_descriptor.address),
        key_idx: Some(key_descriptor.key_id as u8),
        key: Some(key_descriptor.key),
        rsc: Some(key_descriptor.rsc),
        ..Default::default()
    }
}

pub fn mlme_band_cap_from_softmac(
    band_cap: fidl_softmac::WlanSoftmacBandCapability,
) -> Result<fidl_mlme::BandCapability, anyhow::Error> {
    fn required<T>(field: Option<T>, name: impl Display) -> Result<T, anyhow::Error> {
        field.ok_or_else(|| {
            format_err!("Required band capability field unset in SoftMAC driver FIDL: `{}`.", name)
        })
    }

    // TODO(https://fxbug.dev/42084991): The predicate fields in `WlanSoftmacBandCapability` have been
    //                         deprecated. As such, this function raises no errors when the
    //                         predicate is set `true` but the predicated field is unset, because
    //                         servers working against the deprecated fields are expected to always
    //                         set them to `true`. Once the predicate fields have been removed,
    //                         remove this function and map the fields directly below.
    fn predicated<T>(predicate: Option<bool>, field: Option<T>) -> Option<T> {
        // Do not read the predicated fields if the predicate is unset or set `false`.
        predicate.unwrap_or(false).then_some(field).flatten()
    }

    Ok(fidl_mlme::BandCapability {
        band: required(band_cap.band, "band")?,
        basic_rates: required(band_cap.basic_rates(), "basic_rates")?.into(),
        operating_channels: required(band_cap.operating_channels(), "operating_channels")?.into(),
        ht_cap: predicated(band_cap.ht_supported, band_cap.ht_caps).map(Box::new),
        vht_cap: predicated(band_cap.vht_supported, band_cap.vht_caps).map(Box::new),
    })
}

pub fn mlme_device_info_from_softmac(
    query_response: fidl_softmac::WlanSoftmacQueryResponse,
) -> Result<fidl_mlme::DeviceInfo, anyhow::Error> {
    fn required<T>(field: Option<T>, name: impl Display) -> Result<T, anyhow::Error> {
        field.ok_or_else(|| {
            format_err!("Required query field unset in SoftMAC driver FIDL: `{}`.", name)
        })
    }

    let band_caps = query_response
        .band_caps
        .as_ref()
        .map(|band_caps| {
            band_caps.iter().cloned().map(mlme_band_cap_from_softmac).collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    Ok(fidl_mlme::DeviceInfo {
        sta_addr: required(query_response.sta_addr, "sta_addr")?,
        role: required(query_response.mac_role, "mac_role")?,
        bands: required(band_caps, "band_caps")?,
        qos_capable: false,
        softmac_hardware_capability: required(
            query_response.hardware_capability,
            "hardware_capability",
        )?,
    })
}

pub fn ddk_channel_from_fidl(
    fc: fidl_common::WlanChannel,
) -> Result<banjo_common::WlanChannel, Error> {
    let cbw = match fc.cbw {
        fidl_common::ChannelBandwidth::Cbw20 => banjo_common::ChannelBandwidth::CBW20,
        fidl_common::ChannelBandwidth::Cbw40 => banjo_common::ChannelBandwidth::CBW40,
        fidl_common::ChannelBandwidth::Cbw40Below => banjo_common::ChannelBandwidth::CBW40BELOW,
        fidl_common::ChannelBandwidth::Cbw80 => banjo_common::ChannelBandwidth::CBW80,
        fidl_common::ChannelBandwidth::Cbw160 => banjo_common::ChannelBandwidth::CBW160,
        fidl_common::ChannelBandwidth::Cbw80P80 => banjo_common::ChannelBandwidth::CBW80P80,
        fidl_common::ChannelBandwidthUnknown!() => {
            return Err(format_err!("Unknown channel bandwidth {:?}", fc.cbw));
        }
    };
    Ok(banjo_common::WlanChannel { primary: fc.primary, cbw, secondary80: fc.secondary80 })
}

pub fn fidl_channel_from_ddk(bc: banjo_common::WlanChannel) -> fidl_common::WlanChannel {
    let cbw = match bc.cbw {
        banjo_common::ChannelBandwidth::CBW20 => fidl_common::ChannelBandwidth::Cbw20,
        banjo_common::ChannelBandwidth::CBW40 => fidl_common::ChannelBandwidth::Cbw40,
        banjo_common::ChannelBandwidth::CBW40BELOW => fidl_common::ChannelBandwidth::Cbw40Below,
        banjo_common::ChannelBandwidth::CBW80 => fidl_common::ChannelBandwidth::Cbw80,
        banjo_common::ChannelBandwidth::CBW160 => fidl_common::ChannelBandwidth::Cbw160,
        banjo_common::ChannelBandwidth::CBW80P80 => fidl_common::ChannelBandwidth::Cbw80P80,
        _ => unreachable!(),
    };
    fidl_common::WlanChannel { primary: bc.primary, cbw, secondary80: bc.secondary80 }
}

pub fn get_rssi_dbm(rx_info: banjo_wlan_softmac::WlanRxInfo) -> Option<i8> {
    match rx_info.valid_fields & banjo_wlan_softmac::WlanRxInfoValid::RSSI
        != banjo_wlan_softmac::WlanRxInfoValid(0)
        && rx_info.rssi_dbm != 0
    {
        true => Some(rx_info.rssi_dbm),
        false => None,
    }
}

// TODO(b/308634817): Remove this conversion once CSsid is no longer used for
//                    SSIDs specified for active scan requests.
pub fn cssid_from_ssid_unchecked(ssid: &Vec<u8>) -> fidl_ieee80211::CSsid {
    let mut cssid = fidl_ieee80211::CSsid {
        len: ssid.len() as u8,
        data: [0; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize],
    };
    // Ssid never exceeds fidl_ieee80211::MAX_SSID_BYTE_LEN bytes, so this assignment will never panic
    cssid.data[..ssid.len()].copy_from_slice(&ssid[..]);
    cssid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_rx_info() -> banjo_wlan_softmac::WlanRxInfo {
        banjo_wlan_softmac::WlanRxInfo {
            rx_flags: banjo_wlan_softmac::WlanRxInfoFlags(0),
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
            phy: banjo_common::WlanPhyType::DSSS,
            data_rate: 0,
            channel: banjo_common::WlanChannel {
                primary: 0,
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            mcs: 0,
            rssi_dbm: 0,
            snr_dbh: 0,
        }
    }

    #[test]
    fn test_get_rssi_dbm_field_not_valid() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
            rssi_dbm: 20,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), None);
    }

    #[test]
    fn test_get_rssi_dbm_zero_dbm() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid::RSSI,
            rssi_dbm: 0,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), None);
    }

    #[test]
    fn test_get_rssi_dbm_all_good() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid::RSSI,
            rssi_dbm: 20,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), Some(20));
    }

    #[test]
    fn test_mlme_band_cap_from_softmac() {
        let softmac_band_cap = fidl_softmac::WlanSoftmacBandCapability {
            band: Some(fidl_common::WlanBand::TwoGhz),
            basic_rates: Some(vec![
                0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c,
            ]),
            operating_channels: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
            ht_supported: Some(true),
            ht_caps: Some(fidl_ieee80211::HtCapabilities {
                bytes: [
                    0x63, 0x00, // HT capability info
                    0x17, // AMPDU params
                    0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                    0x01, 0x00, 0x00, 0x00, // Tx parameters
                    0x00, 0x00, // HT extended capabilities
                    0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                    0x00, // ASEL capabilities
                ],
            }),
            vht_supported: Some(false),
            vht_caps: Some(fidl_ieee80211::VhtCapabilities { bytes: Default::default() }),
            ..Default::default()
        };
        let mlme_band_cap = mlme_band_cap_from_softmac(softmac_band_cap)
            .expect("failed to convert band capability");
        assert_eq!(mlme_band_cap.band, fidl_common::WlanBand::TwoGhz);
        assert_eq!(
            mlme_band_cap.basic_rates,
            vec![0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c]
        );
        assert_eq!(
            mlme_band_cap.operating_channels,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        assert!(mlme_band_cap.ht_cap.is_some());
        assert!(mlme_band_cap.vht_cap.is_none());
    }
}
