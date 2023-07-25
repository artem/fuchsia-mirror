// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/74991): This test should be implemented using the Policy API (instead of directly
//                        interacting with SME). However, Policy cannot yet manage multiple client
//                        interfaces. Parts of the test pause so that Policy can detect the SME
//                        clients and request startup disconnects before the test continues to
//                        manipulate their state. When the Policy API can be used, reimplement this
//                        test with it (and remove the pauses).

use {
    anyhow::format_err,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_common_security as fidl_security,
    fidl_fuchsia_wlan_device_service::DeviceMonitorMarker,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
    fidl_fuchsia_wlan_sme::{self as fidl_sme, ClientSmeProxy, ConnectRequest},
    fidl_fuchsia_wlan_tap as fidl_tap,
    fidl_test_wlan_realm::WlanConfig,
    fuchsia_async as fasync,
    fuchsia_zircon::DurationNum,
    futures::{
        channel::oneshot, future, join, stream::TryStreamExt, FutureExt, StreamExt, TryFutureExt,
    },
    pin_utils::pin_mut,
    std::{fmt::Display, panic, sync::Arc, thread, time},
    wlan_common::{bss::Protection::Open, fake_fidl_bss_description, TimeUnit},
    wlan_hw_sim::{
        event::{action, branch, Handler},
        *,
    },
};

pub const CLIENT1_MAC_ADDR: [u8; 6] = [0x68, 0x62, 0x6f, 0x6e, 0x69, 0x6c];
pub const CLIENT2_MAC_ADDR: [u8; 6] = [0x68, 0x62, 0x6f, 0x6e, 0x69, 0x6d];

#[derive(Debug)]
struct ClientPhy<N> {
    proxy: Arc<fidl_tap::WlantapPhyProxy>,
    identifier: N,
}

async fn connect(
    client_sme: &ClientSmeProxy,
    req: &mut ConnectRequest,
) -> Result<(), anyhow::Error> {
    let (local, remote) = fidl::endpoints::create_proxy()?;
    client_sme.connect(req, Some(remote))?;
    let mut stream = local.take_event_stream();
    if let Some(event) = stream.try_next().await? {
        match event {
            fidl_sme::ConnectTransactionEvent::OnConnectResult { result } => {
                if result.code == fidl_ieee80211::StatusCode::Success {
                    return Ok(());
                }
                return Err(format_err!("connect failed with error code: {:?}", result.code));
            }
            other => {
                return Err(format_err!(
                    "Expected ConnectTransactionEvent::OnConnectResult event, got {:?}",
                    other
                ))
            }
        }
    }
    Err(format_err!("Server closed the ConnectTransaction channel before sending a response"))
}

// TODO(fxbug.dev/91118) - Added to help investigate hw-sim test. Remove later
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
        bssid: AP_MAC_ADDR,
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
    let ctx = test_utils::TestRealmContext::new(WlanConfig {
        use_legacy_privacy: Some(false),
        ..Default::default()
    })
    .await;

    let wlan_monitor_svc = ctx
        .test_realm_proxy()
        .connect_to_protocol::<DeviceMonitorMarker>()
        .await
        .expect("connecting to device monitor service");

    let network_config = NetworkConfigBuilder::open().ssid(&AP_SSID);

    let mut dc = CreateDeviceHelper::new(ctx, &wlan_monitor_svc);

    let (mut ap_helper, _) = dc
        .create_device(default_wlantap_config_ap(), Some(network_config))
        .await
        .expect("create ap");
    let ap_proxy = ap_helper.proxy();

    let (mut client1_helper, client1_iface_id) = dc
        .create_device(wlantap_config_client(format!("wlantap-client-1"), CLIENT1_MAC_ADDR), None)
        .await
        .expect("create client1");
    let client1_proxy = ClientPhy { proxy: client1_helper.proxy(), identifier: 1 };
    let client1_sme = get_client_sme(&wlan_monitor_svc, client1_iface_id).await;
    let (client1_confirm_sender, client1_confirm_receiver) = oneshot::channel();

    let (mut client2_helper, client2_iface_id) = dc
        .create_device(wlantap_config_client(format!("wlantap-client-2"), CLIENT2_MAC_ADDR), None)
        .await
        .expect("create client2");
    let client2_proxy = ClientPhy { proxy: client2_helper.proxy(), identifier: 2 };
    let client2_sme = get_client_sme(&wlan_monitor_svc, client2_iface_id).await;
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
    let mut client1_connect_req = ConnectRequest {
        ssid: AP_SSID.to_vec(),
        bss_description: fake_fidl_bss_description!(
            Open,
            ssid: AP_SSID.clone(),
            bssid: AP_MAC_ADDR.0,
            // Unrealistically long beacon period so that connect doesn't timeout on slow bots.
            beacon_period: TimeUnit::DEFAULT_BEACON_INTERVAL.0 * 20u16,
            channel: WLANCFG_DEFAULT_AP_CHANNEL.into(),
        ),
        authentication: fidl_security::Authentication {
            protocol: fidl_security::Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: fidl_common::ScanType::Passive,
        multiple_bss_candidates: false, // only used for metrics, select arbitrary value
    };
    let client1_connect_fut = connect(&client1_sme, &mut client1_connect_req);
    pin_mut!(client1_connect_fut);
    thread::sleep(time::Duration::from_secs(3)); // Wait for the policy layer. See fxbug.dev/74991.
    let client1_fut = client1_helper
        .run_until_complete_or_timeout(
            std::i64::MAX.nanos(),
            "connecting to AP",
            scan_and_transmit_to_ap(&ap_proxy, &client1_proxy),
            client1_connect_fut.and_then(|()| {
                client1_confirm_sender.send(()).expect("sending confirmation");
                future::ok(())
            }),
        )
        .unwrap_or_else(|e| panic!("waiting for connect confirmation: {:?}", e));

    // Start client 2
    let mut client2_connect_req = ConnectRequest {
        ssid: AP_SSID.to_vec(),
        bss_description: fake_fidl_bss_description!(
            Open,
            ssid: AP_SSID.clone(),
            bssid: AP_MAC_ADDR.0,
            // Unrealistically long beacon period so that connect doesn't timeout on slow bots.
            beacon_period: TimeUnit::DEFAULT_BEACON_INTERVAL.0 * 20u16,
            channel: WLANCFG_DEFAULT_AP_CHANNEL.into(),
        ),
        authentication: fidl_security::Authentication {
            protocol: fidl_security::Protocol::Open,
            credentials: None,
        },
        deprecated_scan_type: fidl_common::ScanType::Passive,
        multiple_bss_candidates: false, // only used for metrics, select arbitrary value
    };
    let client2_connect_fut = connect(&client2_sme, &mut client2_connect_req);
    pin_mut!(client2_connect_fut);
    thread::sleep(time::Duration::from_secs(3)); // Wait for the policy layer. See fxbug.dev/74991.
    let client2_fut = client2_helper
        .run_until_complete_or_timeout(
            std::i64::MAX.nanos(),
            "connecting to AP",
            scan_and_transmit_to_ap(&ap_proxy, &client2_proxy),
            client2_connect_fut.and_then(|()| {
                client2_confirm_sender.send(()).expect("sending confirmation");
                future::ok(())
            }),
        )
        .unwrap_or_else(|e| panic!("waiting for connect confirmation: {:?}", e));

    join!(ap_fut, client1_fut, client2_fut, canary(finish_receiver));
    client1_helper.stop().await;
    client2_helper.stop().await;
    ap_helper.stop().await;
}
