// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_test_wlan_realm::WlanConfig,
    fuchsia_zircon::prelude::*, ieee80211::Bssid, wlan_common::bss::Protection, wlan_hw_sim::*,
};

/// Test a client can connect to a network with no protection by simulating an AP that sends out
/// hard coded authentication and association response frames.
#[fuchsia::test]
async fn connect_to_open_network() {
    let bss = Bssid::from([0x62, 0x73, 0x73, 0x66, 0x6f, 0x6f]);

    let mut helper = test_utils::TestHelper::begin_test(
        default_wlantap_config_client(),
        WlanConfig { use_legacy_privacy: Some(false), ..Default::default() },
    )
    .await;
    let () = loop_until_iface_is_found(&mut helper).await;

    let () = connect_or_timeout(
        &mut helper,
        30.seconds(),
        &AP_SSID,
        &bss,
        &Protection::Open,
        None,
        fidl_policy::SecurityType::None,
    )
    .await;
    helper.stop().await;
}
