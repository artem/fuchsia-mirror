// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_policy as fidl_policy,
    fidl_test_wlan_realm::WlanConfig,
    ieee80211::{Bssid, MacAddrBytes, Ssid},
    lazy_static::lazy_static,
    pin_utils::pin_mut,
    wlan_common::{
        bss::Protection,
        channel::{Cbw, Channel},
    },
    wlan_hw_sim::{event::action, *},
};

lazy_static! {
    static ref BSS_WPA1: Bssid = Bssid::from([0x62, 0x73, 0x73, 0x66, 0x6f, 0x6f]);
    static ref BSS_WEP: Bssid = Bssid::from([0x62, 0x73, 0x73, 0x66, 0x6f, 0x72]);
    static ref BSS_MIXED: Bssid = Bssid::from([0x62, 0x73, 0x73, 0x66, 0x6f, 0x7a]);
    static ref SSID_WPA1: Ssid = Ssid::try_from("wpa1___how_nice").unwrap();
    static ref SSID_WEP: Ssid = Ssid::try_from("wep_is_soooo_secure").unwrap();
    static ref SSID_MIXED: Ssid = Ssid::try_from("this_is_fine").unwrap();
}

/// Test a client can connect to a wep or wpa network only when configured on.
#[fuchsia::test]
async fn scan_legacy_privacy() {
    let mut helper = test_utils::TestHelper::begin_test(
        default_wlantap_config_client(),
        WlanConfig { use_legacy_privacy: Some(true), ..Default::default() },
    )
    .await;
    let () = loop_until_iface_is_found(&mut helper).await;
    let phy = helper.proxy();

    // Create a client controller.
    let (client_controller, _update_stream) =
        init_client_controller(&helper.test_realm_proxy()).await;

    let scan_result_list_fut = test_utils::policy_scan_for_networks(client_controller);
    pin_mut!(scan_result_list_fut);
    let scan_result_list = helper
        .run_until_complete_or_timeout(
            *SCAN_RESPONSE_TEST_TIMEOUT,
            "receive a scan response",
            // Configure the scan event to return beacon frames corresponding to each `Beacon`
            // specified.
            event::on_scan(action::send_advertisements_and_scan_completion(
                &phy,
                [
                    Beacon {
                        channel: Channel::new(1, Cbw::Cbw20),
                        bssid: *BSS_WPA1,
                        ssid: SSID_WPA1.clone(),
                        protection: Protection::Wpa1,
                        rssi_dbm: -30,
                    },
                    Beacon {
                        channel: Channel::new(1, Cbw::Cbw20),
                        bssid: *BSS_WEP,
                        ssid: SSID_WEP.clone(),
                        protection: Protection::Wep,
                        rssi_dbm: -40,
                    },
                    Beacon {
                        channel: Channel::new(1, Cbw::Cbw20),
                        bssid: *BSS_MIXED,
                        ssid: SSID_MIXED.clone(),
                        protection: Protection::Wpa1Wpa2Personal,
                        rssi_dbm: -50,
                    },
                ],
            )),
            scan_result_list_fut,
        )
        .await;

    let expected_scan_result_list = test_utils::sort_policy_scan_result_list(vec![
        fidl_policy::ScanResult {
            id: Some(fidl_policy::NetworkIdentifier {
                ssid: SSID_MIXED.to_vec(),
                type_: fidl_policy::SecurityType::Wpa2,
            }),
            entries: Some(vec![fidl_policy::Bss {
                bssid: Some(BSS_MIXED.to_array()),
                rssi: Some(-50),
                frequency: Some(2412),
                ..Default::default()
            }]),
            compatibility: Some(fidl_policy::Compatibility::Supported),
            ..Default::default()
        },
        fidl_policy::ScanResult {
            id: Some(fidl_policy::NetworkIdentifier {
                ssid: SSID_WEP.to_vec(),
                type_: fidl_policy::SecurityType::Wep,
            }),
            entries: Some(vec![fidl_policy::Bss {
                bssid: Some(BSS_WEP.to_array()),
                rssi: Some(-40),
                frequency: Some(2412),
                ..Default::default()
            }]),
            compatibility: Some(fidl_policy::Compatibility::Supported),
            ..Default::default()
        },
        fidl_policy::ScanResult {
            id: Some(fidl_policy::NetworkIdentifier {
                ssid: SSID_WPA1.to_vec(),
                type_: fidl_policy::SecurityType::Wpa,
            }),
            entries: Some(vec![fidl_policy::Bss {
                bssid: Some(BSS_WPA1.to_array()),
                rssi: Some(-30),
                frequency: Some(2412),
                ..Default::default()
            }]),
            compatibility: Some(fidl_policy::Compatibility::Supported),
            ..Default::default()
        },
    ]);

    // Compare one at a time for improved debuggability.
    assert_eq!(scan_result_list.len(), expected_scan_result_list.len());
    for i in 0..expected_scan_result_list.len() {
        assert_eq!(scan_result_list[i], expected_scan_result_list[i]);
    }
}
