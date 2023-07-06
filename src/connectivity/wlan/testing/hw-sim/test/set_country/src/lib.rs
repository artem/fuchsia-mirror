// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_device_service::{
        DeviceMonitorMarker, DeviceMonitorProxy, SetCountryRequest,
    },
    fidl_fuchsia_wlan_tap::WlantapPhyEvent,
    fuchsia_component::client::connect_to_protocol,
    fuchsia_zircon::sys::ZX_OK,
    fuchsia_zircon::DurationNum,
    futures::channel::oneshot,
    pin_utils::pin_mut,
    wlan_hw_sim::*,
};

async fn set_country_helper<'a>(
    receiver: oneshot::Receiver<()>,
    svc: &'a DeviceMonitorProxy,
    req: &'a mut SetCountryRequest,
) {
    let status = svc.set_country(req).await.expect("calling set_country");
    assert_eq!(status, ZX_OK);
    receiver.await.expect("error receiving set_country_helper mpsc message");
}

/// Issue service.fidl:SetCountry() protocol to Wlanstack's service with a test country code.
/// Test two things:
///  - If wlantap PHY device received the specified test country code
///  - If the SetCountry() returned successfully (ZX_OK).
#[fuchsia_async::run_singlethreaded(test)]
async fn set_country() {
    init_syslog();

    const ALPHA2: &[u8; 2] = b"RS";

    let mut helper = test_utils::TestHelper::begin_test(default_wlantap_config_client()).await;
    let svc = connect_to_protocol::<DeviceMonitorMarker>()
        .expect("Failed to connect to wlandevicemonitor");

    let resp = svc.list_phys().await.unwrap();

    assert!(resp.len() > 0, "WLAN PHY device is created but ListPhys returned empty.");
    let phy_id = resp[0];
    let mut req = SetCountryRequest { phy_id, alpha2: *ALPHA2 };

    let (sender, receiver) = oneshot::channel();
    // Employ a future to make sure this test does not end before WlantanPhyEvent is captured.
    let set_country_fut = set_country_helper(receiver, &svc, &mut req);
    pin_mut!(set_country_fut);

    let mut sender = Some(sender);
    helper
        .run_until_complete_or_timeout(
            std::i64::MAX.nanos(), // Unlimited timeout since set_country must be called.
            "wlanstack_dev_svc set_country",
            event::matched(|_, event| {
                if let WlantapPhyEvent::SetCountry { ref args } = event {
                    assert_eq!(args.alpha2, *ALPHA2);
                    sender.take().map(|s| s.send(()));
                }
            }),
            set_country_fut,
        )
        .await;
    helper.stop().await;
}
