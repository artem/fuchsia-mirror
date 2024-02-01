// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_policy as fidl_policy,
    fidl_test_wlan_realm::WlanConfig,
    fuchsia_zircon::DurationNum,
    ieee80211::Bssid,
    lazy_static::lazy_static,
    netdevice_client,
    pin_utils::pin_mut,
    wlan_common::{
        bss::Protection,
        buffer_reader::BufferReader,
        channel::{Cbw, Channel},
        mac,
    },
    wlan_hw_sim::{
        connect_or_timeout, default_wlantap_config_client,
        event::{
            self,
            buffered::{Buffered, DataFrame},
        },
        loop_until_iface_is_found, netdevice_helper, rx_wlan_data_frame, test_utils, AP_SSID,
        CLIENT_MAC_ADDR, ETH_DST_MAC,
    },
};

lazy_static! {
    static ref BSS: Bssid = [0x65, 0x74, 0x68, 0x6e, 0x65, 0x74].into();
}

async fn send_and_receive<'a>(
    session: &'a netdevice_client::Session,
    port: &'a netdevice_client::Port,
    buf: &'a [u8],
) -> (mac::EthernetIIHdr, Vec<u8>) {
    netdevice_helper::send(session, port, &buf).await;
    let recv_buf = netdevice_helper::recv(session).await;
    let mut buf_reader = BufferReader::new(&recv_buf[..]);
    let header = buf_reader
        .read::<mac::EthernetIIHdr>()
        .expect("bytes received too short for ethernet header");
    let payload = buf_reader.into_remaining().to_vec();
    (*header, payload)
}

async fn verify_tx_and_rx(
    session: &netdevice_client::Session,
    port: &netdevice_client::Port,
    helper: &mut test_utils::TestHelper,
    payload_size: usize,
) {
    let phy = helper.proxy();
    let mock_payload = vec![7; payload_size];
    for _ in 0..25 {
        let mut buf: Vec<u8> = Vec::new();
        netdevice_helper::write_fake_frame(
            *ETH_DST_MAC,
            *CLIENT_MAC_ADDR,
            &mock_payload[..],
            &mut buf,
        );
        let tx_rx_fut = send_and_receive(session, port, &buf);
        pin_mut!(tx_rx_fut);

        let mut sent_payload = Vec::new();
        let (header, received_payload) = helper
            .run_until_complete_or_timeout(
                5.seconds(),
                "verify ethernet_tx_rx",
                event::on_transmit(event::extract(|frame: Buffered<DataFrame>| {
                    for mac::Msdu { dst_addr, src_addr, llc_frame } in frame.msdus() {
                        if dst_addr == *ETH_DST_MAC && src_addr == *CLIENT_MAC_ADDR {
                            assert_eq!(llc_frame.hdr.protocol_id.to_native(), mac::ETHER_TYPE_IPV4);
                            sent_payload.clear();
                            sent_payload.extend_from_slice(llc_frame.body);
                            rx_wlan_data_frame(
                                &Channel::new(1, Cbw::Cbw20),
                                &CLIENT_MAC_ADDR,
                                &(*BSS).into(),
                                &ETH_DST_MAC,
                                &mock_payload[..],
                                mac::ETHER_TYPE_IPV4,
                                &phy,
                            )
                            .expect("sending wlan data frame");
                        }
                    }
                })),
                tx_rx_fut,
            )
            .await;
        assert_eq!(&sent_payload[..], &mock_payload[..]);
        assert_eq!(header.da, *CLIENT_MAC_ADDR);
        assert_eq!(header.sa, *ETH_DST_MAC);
        assert_eq!(header.ether_type.to_native(), mac::ETHER_TYPE_IPV4);
        assert_eq!(&received_payload[..], &mock_payload[..]);
    }
}

/// Test an ethernet device using netdevice backed by WLAN device and send and receive data
/// frames by verifying frames are delivered without any change in both directions.
#[fuchsia::test]
async fn ethernet_tx_rx() {
    let mut helper = test_utils::TestHelper::begin_test(
        default_wlantap_config_client(),
        WlanConfig { use_legacy_privacy: Some(false), ..Default::default() },
    )
    .await;
    let () = loop_until_iface_is_found(&mut helper).await;

    connect_or_timeout(
        &mut helper,
        30.seconds(),
        &AP_SSID,
        &BSS,
        &Protection::Open,
        None,
        fidl_policy::SecurityType::None,
    )
    .await;

    let (session, port) = helper.start_netdevice_session(*CLIENT_MAC_ADDR).await;

    // 15 byte MTUs
    verify_tx_and_rx(&session, &port, &mut helper, 15).await;
    // 100 byte MTUs
    verify_tx_and_rx(&session, &port, &mut helper, 100).await;
    // 1KB MTUs
    verify_tx_and_rx(&session, &port, &mut helper, 1000).await;
    // Maximum size 1500KB MTUs
    verify_tx_and_rx(&session, &port, &mut helper, 1500).await;
}
