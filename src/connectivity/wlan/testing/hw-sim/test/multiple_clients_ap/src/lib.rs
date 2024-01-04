// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_fuchsia_wlan_tap as fidl_tap,
    fidl_test_wlan_realm::WlanConfig,
    fuchsia_async as fasync,
    fuchsia_zircon::DurationNum,
    futures::{channel::oneshot, future, join, FutureExt, StreamExt, TryFutureExt},
    ieee80211::MacAddr,
    lazy_static::lazy_static,
    pin_utils::pin_mut,
    std::{fmt::Display, panic, sync::Arc},
    wlan_common::bss::Protection::Open,
    wlan_hw_sim::{
        event::{action, branch, Handler},
        *,
    },
};

lazy_static! {
    static ref CLIENT1_MAC_ADDR: MacAddr = [0x68, 0x62, 0x6f, 0x6e, 0x69, 0x6c].into();
    static ref CLIENT2_MAC_ADDR: MacAddr = [0x68, 0x62, 0x6f, 0x6e, 0x69, 0x6d].into();
}

#[derive(Debug)]
struct ClientPhy<N> {
    proxy: Arc<fidl_tap::WlantapPhyProxy>,
    identifier: N,
}

// TODO(https://fxbug.dev/91118) - Added to help investigate hw-sim test. Remove later
async fn canary(mut finish_receiver: oneshot::Receiver<()>) {
    let mut interval_stream = fasync::Interval::new(fasync::Duration::from_seconds(1));
    loop {
        futures::select! {
            _ = interval_stream.next() => {
                tracing::info!("1 second canary");
            }
            _ = finish_receiver => {
                return;
            }
        }
    }
}

fn transmit_to_clients<'h>(
    clients: impl IntoIterator<Item = &'h ClientPhy<impl Display + 'h>>,
) -> impl Handler<(), fidl_tap::WlantapPhyEvent> + 'h {
    let handlers: Vec<_> = clients
        .into_iter()
        .map(|client| {
            event::on_transmit(
                action::send_packet(&client.proxy, rx_info_with_default_ap()).context(format!(
                    "failed to send packet from AP to client {}",
                    client.identifier
                )),
            )
        })
        .collect();
    branch::try_and(handlers).expect("failed to transmit from AP")
}

fn scan_and_transmit_to_ap<'h>(
    ap: &'h Arc<fidl_tap::WlantapPhyProxy>,
    client: &'h ClientPhy<impl Display>,
) -> impl Handler<(), fidl_tap::WlantapPhyEvent> + 'h {
    let probes = [ProbeResponse {
        channel: WLANCFG_DEFAULT_AP_CHANNEL.clone(),
        bssid: *AP_MAC_ADDR,
        ssid: AP_SSID.clone(),
        protection: Open,
        rssi_dbm: 0,
        wsc_ie: None,
    }];
    branch::or((
        event::on_transmit(
            action::send_packet(ap, rx_info_with_default_ap())
                .context(format!("failed to send packet from client {} to AP", client.identifier)),
        ),
        event::on_scan(
            action::send_advertisements_and_scan_completion(&client.proxy, probes).context(
                format!(
                    "failed to send probe response and scan completion to client {}",
                    client.identifier
                ),
            ),
        ),
    ))
    .expect("failed to scan and transmit from client")
}

/// Spawn two client and one AP wlantap devices. Verify that both clients connect to the AP by
/// sending ethernet frames.
#[fuchsia::test]
async fn multiple_clients_ap() {
    let network_config = NetworkConfigBuilder::open().ssid(&AP_SSID);

    let mut ap_helper = test_utils::TestHelper::begin_ap_test(
        default_wlantap_config_ap(),
        network_config,
        WlanConfig {
            use_legacy_privacy: Some(false),
            name: Some("ap-realm".to_string()),
            ..Default::default()
        },
    )
    .await;

    let ap_proxy = ap_helper.proxy();

    let mut client1_helper = test_utils::TestHelper::begin_test(
        wlantap_config_client(format!("wlantap-client-1"), *CLIENT1_MAC_ADDR),
        WlanConfig {
            use_legacy_privacy: Some(false),
            name: Some("client1-realm".to_string()),
            ..Default::default()
        },
    )
    .await;

    let client1_proxy = ClientPhy { proxy: client1_helper.proxy(), identifier: 1 };
    let (client1_confirm_sender, client1_confirm_receiver) = oneshot::channel();

    let mut client2_helper = test_utils::TestHelper::begin_test(
        wlantap_config_client(format!("wlantap-client-2"), *CLIENT2_MAC_ADDR),
        WlanConfig {
            use_legacy_privacy: Some(false),
            name: Some("client2-realm".to_string()),
            ..Default::default()
        },
    )
    .await;

    let client2_proxy = ClientPhy { proxy: client2_helper.proxy(), identifier: 2 };
    let (client2_confirm_sender, client2_confirm_receiver) = oneshot::channel();

    let (finish_sender, finish_receiver) = oneshot::channel();
    let ap_fut = ap_helper
        .run_until_complete_or_timeout(
            std::i64::MAX.nanos(),
            "serving as an AP",
            transmit_to_clients([&client1_proxy, &client2_proxy]),
            future::join(client1_confirm_receiver, client2_confirm_receiver).then(|_| {
                finish_sender.send(()).expect("sending finish notification");
                future::ok(())
            }),
        )
        .unwrap_or_else(|oneshot::Canceled| panic!("waiting for connect confirmation"));

    // Start client 1
    let client1_test_realm_proxy = client1_helper.test_realm_proxy();
    let client1_connect_fut = async {
        save_network_and_wait_until_connected(
            &client1_test_realm_proxy,
            &AP_SSID,
            fidl_policy::SecurityType::None,
            fidl_policy::Credential::None(fidl_policy::Empty),
        )
        .await;
        client1_confirm_sender.send(()).expect("sending confirmation");
    };

    pin_mut!(client1_connect_fut);
    let client1_fut = client1_helper.run_until_complete_or_timeout(
        std::i64::MAX.nanos(),
        "connecting to AP",
        scan_and_transmit_to_ap(&ap_proxy, &client1_proxy),
        client1_connect_fut,
    );

    // Start client 2
    let client2_test_realm_proxy = client2_helper.test_realm_proxy();
    let client2_connect_fut = async {
        save_network_and_wait_until_connected(
            &client2_test_realm_proxy,
            &AP_SSID,
            fidl_policy::SecurityType::None,
            fidl_policy::Credential::None(fidl_policy::Empty),
        )
        .await;
        client2_confirm_sender.send(()).expect("sending confirmation");
    };

    pin_mut!(client2_connect_fut);
    let client2_fut = client2_helper.run_until_complete_or_timeout(
        std::i64::MAX.nanos(),
        "connecting to AP",
        scan_and_transmit_to_ap(&ap_proxy, &client2_proxy),
        client2_connect_fut,
    );

    join!(ap_fut, client1_fut, client2_fut, canary(finish_receiver));
}
