// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::FullmacDriverFixture,
    drivers_only_common::sme_helpers::{
        self, random_password, random_ssid, DEFAULT_OPEN_AP_CONFIG,
    },
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fullmac_helpers::{config::FullmacDriverConfig, recorded_request_stream::FullmacRequest},
    rand::{seq::SliceRandom, Rng},
    wlan_common::{assert_variant, ie::rsn::rsne},
};

fn vec_to_cssid(input: &Vec<u8>) -> fidl_ieee80211::CSsid {
    let mut cssid = fidl_ieee80211::CSsid { len: input.len() as u8, data: [0; 32] };

    for i in 0..input.len() {
        cssid.data[i] = input[i];
    }

    cssid
}

/// Many tests require a started BSS. This helper function creates and starts an AP in the test
/// realm and returns the ApSmeProxy, FullmacDriverFixture, and GenericSmeProxy.
async fn setup_test_bss_started(
    driver_config: FullmacDriverConfig,
    ap_config: &fidl_sme::ApConfig,
) -> (fidl_sme::ApSmeProxy, FullmacDriverFixture, fidl_sme::GenericSmeProxy) {
    // This is wrapped in a Box::pin because otherwise the compiler complains about the future
    // being too large.
    Box::pin(async {
        let (mut fullmac_driver, generic_sme_proxy) =
            FullmacDriverFixture::create_and_get_generic_sme(driver_config).await;
        let ap_sme_proxy = sme_helpers::get_ap_sme(&generic_sme_proxy).await;

        let ap_fut = ap_sme_proxy.start(&ap_config);
        let driver_fut = async {
            assert_variant!(fullmac_driver.request_stream.next().await,
                fidl_fullmac::WlanFullmacImplBridgeRequest::StartBss { payload: _, responder } => {
                    responder.send().expect("Could not respond to StartBss");
            });

            fullmac_driver
                .ifc_proxy
                .start_conf(&fidl_fullmac::WlanFullmacStartConfirm {
                    result_code: fidl_fullmac::WlanStartResult::Success,
                })
                .await
                .expect("Could not send StartConf");

            assert_variant!(fullmac_driver.request_stream.next().await,
                fidl_fullmac::WlanFullmacImplBridgeRequest::OnLinkStateChanged { online:_ , responder } => {
                    responder.send().expect("Could not respond to OnLinkStateChanged");
            });
        };

        let (_, _) = futures::join!(ap_fut, driver_fut);

        fullmac_driver.request_stream.clear_history();
        (ap_sme_proxy, fullmac_driver, generic_sme_proxy)
    }).await
}

#[fuchsia::test]
async fn test_start_2ghz_bss_success() {
    let (mut fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig::default_ap()).await;
    let ap_sme_proxy = sme_helpers::get_ap_sme(&generic_sme_proxy).await;

    let phy_types = [
        fidl_common::WlanPhyType::Dsss,
        fidl_common::WlanPhyType::Hr,
        fidl_common::WlanPhyType::Ofdm,
        fidl_common::WlanPhyType::Erp,
        fidl_common::WlanPhyType::Ht,
    ];

    // channel support is defined by fullmac driver config
    let driver_band_cap = &fullmac_driver.config.query_info.band_cap_list[0];
    let supported_channels = &driver_band_cap.operating_channel_list
        [0..driver_band_cap.operating_channel_count as usize];

    let sme_ap_config = fidl_sme::ApConfig {
        ssid: random_ssid(),
        password: random_password(),
        radio_cfg: fidl_sme::RadioConfig {
            phy: *phy_types.choose(&mut rand::thread_rng()).unwrap(),
            channel: fidl_common::WlanChannel {
                primary: *supported_channels.choose(&mut rand::thread_rng()).unwrap(),
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
        },
    };

    let ap_fut = ap_sme_proxy.start(&sme_ap_config);
    let driver_fut = async {
        assert_variant!(fullmac_driver.request_stream.next().await,
            fidl_fullmac::WlanFullmacImplBridgeRequest::StartBss { payload: _, responder } => {
                responder.send().expect("Could not respond to StartBss");
        });

        fullmac_driver
            .ifc_proxy
            .start_conf(&fidl_fullmac::WlanFullmacStartConfirm {
                result_code: fidl_fullmac::WlanStartResult::Success,
            })
            .await
            .expect("Could not send StartConf");

        assert_variant!(fullmac_driver.request_stream.next().await,
            fidl_fullmac::WlanFullmacImplBridgeRequest::OnLinkStateChanged { online:_ , responder } => {
                responder.send().expect("Could not respond to OnLinkStateChanged");
        });
    };

    let (sme_start_result, _) = futures::join!(ap_fut, driver_fut);
    assert_eq!(
        sme_start_result.expect("Error on call to SME start"),
        fidl_sme::StartApResultCode::Success
    );

    let fullmac_request_history = fullmac_driver.request_stream.history();

    assert_eq!(
        fullmac_request_history[0],
        FullmacRequest::StartBss(fidl_fullmac::WlanFullmacImplBaseStartBssRequest {
            ssid: Some(vec_to_cssid(&sme_ap_config.ssid)),
            bss_type: Some(fidl_common::BssType::Infrastructure),
            beacon_period: Some(100),
            dtim_period: Some(2),
            channel: Some(sme_ap_config.radio_cfg.channel.primary),
            rsne: Some(rsne::Rsne::wpa2_rsne_with_caps(rsne::RsnCapabilities(0)).into_bytes()),
            vendor_ie: Some(vec![]),
            ..Default::default()
        })
    );

    assert_eq!(fullmac_request_history[1], FullmacRequest::OnLinkStateChanged(true));

    // Check AP status to see that SME reports that an AP is running.
    // Driver does not take part in this interaction.
    let running_ap =
        ap_sme_proxy.status().await.expect("Could not get ApSme status").running_ap.unwrap();
    assert_eq!(
        running_ap.as_ref(),
        &fidl_sme::Ap {
            ssid: sme_ap_config.ssid.clone(),
            channel: sme_ap_config.radio_cfg.channel.primary,
            num_clients: 0,
        }
    );
}

#[fuchsia::test]
async fn test_start_bss_fail_non_ascii_password() {
    let (_fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig::default_ap()).await;
    let ap_sme_proxy = sme_helpers::get_ap_sme(&generic_sme_proxy).await;

    let sme_ap_config = fidl_sme::ApConfig {
        ssid: random_ssid(),
        password: vec![1, 2, 3],
        radio_cfg: fidl_sme::RadioConfig {
            phy: fidl_common::WlanPhyType::Ofdm,
            channel: fidl_common::WlanChannel {
                primary: 1,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
        },
    };

    assert_eq!(
        ap_sme_proxy.start(&sme_ap_config).await.expect("Could not start AP"),
        fidl_sme::StartApResultCode::InvalidArguments
    );
}

#[fuchsia::test]
async fn test_start_bss_fail_bad_channel() {
    let (_fullmac_driver, generic_sme_proxy) =
        FullmacDriverFixture::create_and_get_generic_sme(FullmacDriverConfig::default_ap()).await;
    let ap_sme_proxy = sme_helpers::get_ap_sme(&generic_sme_proxy).await;

    let sme_ap_config = fidl_sme::ApConfig {
        ssid: random_ssid(),
        password: vec![],
        radio_cfg: fidl_sme::RadioConfig {
            phy: fidl_common::WlanPhyType::Ofdm,
            channel: fidl_common::WlanChannel {
                primary: 27,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
        },
    };

    assert_eq!(
        ap_sme_proxy.start(&sme_ap_config).await.expect("Could not start AP"),
        fidl_sme::StartApResultCode::InvalidArguments
    );
}

#[fuchsia::test]
async fn test_remote_client_connected_open() {
    let (ap_sme_proxy, mut fullmac_driver, _generic_sme_proxy) =
        setup_test_bss_started(FullmacDriverConfig::default_ap(), &DEFAULT_OPEN_AP_CONFIG).await;

    let remote_sta_address: [u8; 6] = rand::thread_rng().gen();
    fullmac_driver
        .ifc_proxy
        .auth_ind(&fidl_fullmac::WlanFullmacAuthInd {
            peer_sta_address: remote_sta_address.clone(),
            auth_type: fidl_fullmac::WlanAuthType::OpenSystem,
        })
        .await
        .expect("Could not send AuthInd");

    assert_variant!(fullmac_driver.request_stream.next().await,
        fidl_fullmac::WlanFullmacImplBridgeRequest::AuthResp { payload: _, responder } => {
            responder.send().expect("Could not respond to AuthResp");
    });

    fullmac_driver
        .ifc_proxy
        .assoc_ind(&fidl_fullmac::WlanFullmacAssocInd {
            peer_sta_address: remote_sta_address.clone(),
            listen_interval: 100,
            ssid: vec_to_cssid(&DEFAULT_OPEN_AP_CONFIG.ssid),
            rsne_len: 0,
            rsne: [0; fidl_ieee80211::WLAN_IE_MAX_LEN as usize],
            vendor_ie_len: 0,
            vendor_ie: [0; fidl_fullmac::WLAN_VIE_MAX_LEN as usize],
        })
        .await
        .expect("Could not send AssocInd");

    assert_variant!(fullmac_driver.request_stream.next().await,
        fidl_fullmac::WlanFullmacImplBridgeRequest::AssocResp { payload: _, responder } => {
            responder.send().expect("Could not respond to AssocResp");
    });

    // Check AP status to see that SME reports that an AP is running.
    // Driver does not take part in this interaction.
    let running_ap =
        ap_sme_proxy.status().await.expect("Could not get ApSme status").running_ap.unwrap();
    assert_eq!(
        running_ap.as_ref(),
        &fidl_sme::Ap {
            ssid: DEFAULT_OPEN_AP_CONFIG.ssid.clone(),
            channel: DEFAULT_OPEN_AP_CONFIG.radio_cfg.channel.primary,
            num_clients: 1,
        }
    );

    let fullmac_request_history = fullmac_driver.request_stream.history();
    assert_eq!(
        fullmac_request_history[0],
        FullmacRequest::AuthResp(fidl_fullmac::WlanFullmacImplBaseAuthRespRequest {
            peer_sta_address: Some(remote_sta_address.clone()),
            result_code: Some(fidl_fullmac::WlanAuthResult::Success),
            ..Default::default()
        })
    );

    let assoc_resp =
        assert_variant!(&fullmac_request_history[1], FullmacRequest::AssocResp(resp) => resp);
    assert_eq!(assoc_resp.peer_sta_address, Some(remote_sta_address.clone()));
    assert_eq!(assoc_resp.result_code, Some(fidl_fullmac::WlanAssocResult::Success));
    // Note: association id is not checked since SME can pick any value.
}
