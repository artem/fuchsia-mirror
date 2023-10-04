// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Context, Error},
    async_trait::async_trait,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_common_security as fidl_security,
    fidl_fuchsia_wlan_device_service as fidl_device_service,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_async::TimeoutExt,
    fuchsia_zircon as zx,
    futures::TryStreamExt,
    ieee80211::Bssid,
    parking_lot::Mutex,
    std::{collections::HashMap, convert::TryFrom, sync::Arc},
    tracing::info,
    wlan_common::bss::BssDescription,
};

#[async_trait]
pub(crate) trait IfaceManager: Send + Sync {
    type Client: ClientIface;

    async fn list_interfaces(&self) -> Result<Vec<fidl_device_service::QueryIfaceResponse>, Error>;
    async fn get_client_iface(&self, iface_id: u16) -> Result<Arc<Self::Client>, Error>;
}

pub struct DeviceMonitorIfaceManager {
    monitor_svc: fidl_device_service::DeviceMonitorProxy,
    ifaces: Mutex<HashMap<u16, Arc<SmeClientIface>>>,
}

impl DeviceMonitorIfaceManager {
    pub fn new() -> Result<Self, Error> {
        let monitor_svc = fuchsia_component::client::connect_to_protocol::<
            fidl_device_service::DeviceMonitorMarker,
        >()
        .context("failed to connect to device monitor")?;
        Ok(Self { monitor_svc, ifaces: Mutex::new(HashMap::new()) })
    }
}

#[async_trait]
impl IfaceManager for DeviceMonitorIfaceManager {
    type Client = SmeClientIface;

    async fn list_interfaces(&self) -> Result<Vec<fidl_device_service::QueryIfaceResponse>, Error> {
        let ifaces = self.monitor_svc.list_ifaces().await?;
        let mut result = Vec::with_capacity(ifaces.len());
        for iface_id in ifaces {
            let iface_info = self
                .monitor_svc
                .query_iface(iface_id)
                .await?
                .map_err(zx::Status::from_raw)
                .context("Could not query iface info")?;
            result.push(iface_info);
        }
        Ok(result)
    }

    async fn get_client_iface(&self, iface_id: u16) -> Result<Arc<SmeClientIface>, Error> {
        if let Some(iface) = self.ifaces.lock().get(&iface_id) {
            return Ok(iface.clone());
        }
        let (sme_proxy, server) = create_proxy::<fidl_sme::ClientSmeMarker>()?;
        self.monitor_svc.get_client_sme(iface_id, server).await?.map_err(zx::Status::from_raw)?;
        let mut ifaces = self.ifaces.lock();
        if let Some(iface) = ifaces.get(&iface_id) {
            Ok(iface.clone())
        } else {
            let iface = Arc::new(SmeClientIface {
                sme_proxy,
                last_scan_results: Arc::new(Mutex::new(vec![])),
            });
            ifaces.insert(iface_id, iface.clone());
            Ok(iface)
        }
    }
}

pub(crate) struct ConnectedResult {
    pub ssid: Vec<u8>,
    pub bssid: Bssid,
}

#[async_trait]
pub(crate) trait ClientIface: Sync + Send {
    async fn trigger_scan(&self) -> Result<(), Error>;
    fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult>;
    async fn connect_to_network(&self, ssid: &[u8]) -> Result<ConnectedResult, Error>;
}

#[derive(Clone, Debug)]
pub(crate) struct SmeClientIface {
    sme_proxy: fidl_sme::ClientSmeProxy,
    last_scan_results: Arc<Mutex<Vec<fidl_sme::ScanResult>>>,
}

#[async_trait]
impl ClientIface for SmeClientIface {
    async fn trigger_scan(&self) -> Result<(), Error> {
        let scan_request = fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest);
        let scan_result_vmo = self
            .sme_proxy
            .scan(&scan_request)
            .await
            .context("Failed to request scan")?
            .map_err(|e| format_err!("Scan ended with error: {:?}", e))?;
        info!("Got scan results from SME.");
        *self.last_scan_results.lock() = wlan_common::scan::read_vmo(scan_result_vmo)?;
        Ok(())
    }

    fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult> {
        self.last_scan_results.lock().clone()
    }

    async fn connect_to_network(&self, ssid: &[u8]) -> Result<ConnectedResult, Error> {
        let last_scan_results = self.last_scan_results.lock().clone();
        let selected_bss_description = last_scan_results
            .iter()
            .filter_map(|r| {
                // TODO(fxbug.dev/128604): handle the case when there are multiple BSS candidates
                BssDescription::try_from(r.bss_description.clone())
                    .ok()
                    .filter(|bss_description| bss_description.ssid == *ssid)
            })
            .next();

        let bss_description = match selected_bss_description {
            Some(bss_description) => bss_description,
            None => {
                return Err(format_err!("Requested network not found"));
            }
        };

        info!("Selected BSS to connect to");
        let (connect_txn, remote) = create_proxy()?;
        let bssid = bss_description.bssid;
        let connect_req = fidl_sme::ConnectRequest {
            ssid: bss_description.ssid.clone().into(),
            bss_description: bss_description.into(),
            multiple_bss_candidates: false,
            authentication: fidl_security::Authentication {
                protocol: fidl_security::Protocol::Open,
                credentials: None,
            },
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };
        self.sme_proxy.connect(&connect_req, Some(remote))?;

        info!("Waiting for connect result from SME");
        let stream = connect_txn.take_event_stream();
        let sme_result = wait_for_connect_result(stream)
            .on_timeout(zx::Duration::from_seconds(30), || {
                Err(format_err!("Timed out waiting for connect result from SME."))
            })
            .await?;

        info!("Received connect result from SME: {:?}", sme_result);
        if sme_result.code == fidl_ieee80211::StatusCode::Success {
            Ok(ConnectedResult { ssid: ssid.to_vec(), bssid })
        } else {
            Err(format_err!("Connect failed with status code: {:?}", sme_result.code))
        }
    }
}

/// Wait until stream returns an OnConnectResult event or None. Ignore other event types.
async fn wait_for_connect_result(
    mut stream: fidl_sme::ConnectTransactionEventStream,
) -> Result<fidl_sme::ConnectResult, Error> {
    loop {
        let stream_fut = stream.try_next();
        match stream_fut
            .await
            .map_err(|e| format_err!("Failed to receive connect result from sme: {:?}", e))?
        {
            Some(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                return Ok(result)
            }
            Some(other) => {
                info!(
                    "Expected ConnectTransactionEvent::OnConnectResult, got {}. Ignoring.",
                    connect_txn_event_name(&other)
                );
            }
            None => {
                return Err(format_err!(
                    "Server closed the ConnectTransaction channel before sending a response"
                ));
            }
        };
    }
}

fn connect_txn_event_name(event: &fidl_sme::ConnectTransactionEvent) -> &'static str {
    match event {
        fidl_sme::ConnectTransactionEvent::OnConnectResult { .. } => "OnConnectResult",
        fidl_sme::ConnectTransactionEvent::OnDisconnect { .. } => "OnDisconnect",
        fidl_sme::ConnectTransactionEvent::OnSignalReport { .. } => "OnSignalReport",
        fidl_sme::ConnectTransactionEvent::OnChannelSwitched { .. } => "OnChannelSwitched",
    }
}

#[cfg(test)]
pub mod test_utils {
    use {super::*, fidl_fuchsia_wlan_internal as fidl_internal};

    pub static FAKE_IFACE_RESPONSE: fidl_device_service::QueryIfaceResponse =
        fidl_device_service::QueryIfaceResponse {
            role: fidl_fuchsia_wlan_common::WlanMacRole::Client,
            id: 1,
            phy_id: 10,
            phy_assigned_id: 100,
            sta_addr: [1, 2, 3, 4, 5, 6],
        };

    pub fn fake_scan_result() -> fidl_sme::ScanResult {
        fidl_sme::ScanResult {
            compatibility: None,
            timestamp_nanos: 1000,
            bss_description: fidl_internal::BssDescription {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: fidl_common::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 123,
                ies: vec![1, 2, 3, 2, 1],
                channel: fidl_common::WlanChannel {
                    primary: 1,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                rssi_dbm: -40,
                snr_db: -50,
            },
        }
    }

    pub struct TestClientIface {
        pub connected_ssid: Mutex<Option<Vec<u8>>>,
    }

    impl TestClientIface {
        pub fn new() -> Self {
            Self { connected_ssid: Mutex::new(None) }
        }
    }

    #[async_trait]
    impl ClientIface for TestClientIface {
        async fn trigger_scan(&self) -> Result<(), Error> {
            Ok(())
        }
        fn get_last_scan_results(&self) -> Vec<fidl_sme::ScanResult> {
            vec![fake_scan_result()]
        }
        async fn connect_to_network(&self, ssid: &[u8]) -> Result<ConnectedResult, Error> {
            *self.connected_ssid.lock() = Some(ssid.to_vec());
            Ok(ConnectedResult { ssid: ssid.to_vec(), bssid: Bssid([42, 42, 42, 42, 42, 42]) })
        }
    }

    pub struct TestIfaceManager {
        pub client_iface: Option<Arc<TestClientIface>>,
    }

    impl TestIfaceManager {
        pub fn new() -> Self {
            Self { client_iface: None }
        }

        pub fn new_with_client() -> Self {
            Self { client_iface: Some(Arc::new(TestClientIface::new())) }
        }
    }

    #[async_trait]
    impl IfaceManager for TestIfaceManager {
        type Client = TestClientIface;

        async fn list_interfaces(
            &self,
        ) -> Result<Vec<fidl_device_service::QueryIfaceResponse>, Error> {
            Ok(vec![FAKE_IFACE_RESPONSE.clone()])
        }

        async fn get_client_iface(&self, _iface_id: u16) -> Result<Arc<TestClientIface>, Error> {
            match self.client_iface.as_ref() {
                Some(iface) => Ok(Arc::clone(iface)),
                None => panic!("Requested client iface but none configured"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::create_proxy_and_stream,
        fuchsia_async as fasync,
        futures::{task::Poll, StreamExt},
        wlan_common::assert_variant,
    };

    fn setup_test() -> (
        fasync::TestExecutor,
        fidl_device_service::DeviceMonitorRequestStream,
        DeviceMonitorIfaceManager,
    ) {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (monitor_svc, monitor_stream) =
            create_proxy_and_stream::<fidl_device_service::DeviceMonitorMarker>()
                .expect("Failed to create device monitor service");
        (
            exec,
            monitor_stream,
            DeviceMonitorIfaceManager { monitor_svc, ifaces: Mutex::new(HashMap::new()) },
        )
    }

    #[test]
    fn test_list_interfaces() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.list_interfaces();

        // First query device monitor for the list of ifaces.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::ListIfaces { responder })) => responder);
        responder.send(&[1]).expect("Failed to respond to ListIfaces");

        // Second query device monitor for more info on each iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::QueryIface { iface_id: 1, responder })) => responder);
        responder
            .send(Ok(&test_utils::FAKE_IFACE_RESPONSE))
            .expect("Failed to respond to QueryIfaceResponse");

        let results =
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(results)) => results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], test_utils::FAKE_IFACE_RESPONSE);
    }

    #[test]
    fn test_get_client_iface() {
        let (mut exec, mut monitor_stream, manager) = setup_test();
        let mut fut = manager.get_client_iface(1);

        // First query device monitor for the list of ifaces.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let (_sme_server, responder) = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::GetClientSme { iface_id: 1, sme_server, responder })) => (sme_server, responder));
        responder.send(Ok(())).expect("Failed to respond to GetClientSme");

        let _iface =
            assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(iface)) => iface);
    }

    #[test]
    fn test_trigger_scan() {
        let (mut exec, _monitor_stream, manager) = setup_test();
        let (sme_proxy, mut sme_stream) = create_proxy_and_stream::<fidl_sme::ClientSmeMarker>()
            .expect("Failed to create device monitor service");
        manager.ifaces.lock().insert(
            1,
            Arc::new(SmeClientIface { sme_proxy, last_scan_results: Arc::new(Mutex::new(vec![])) }),
        );
        let mut client_fut = manager.get_client_iface(1);
        let iface = assert_variant!(exec.run_until_stalled(&mut client_fut), Poll::Ready(Ok(iface)) => iface);
        assert!(iface.get_last_scan_results().is_empty());
        let mut scan_fut = iface.trigger_scan();
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Pending);
        let (_req, responder) = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.next()),
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Scan { req, responder }))) => (req, responder));
        let result = wlan_common::scan::write_vmo(vec![test_utils::fake_scan_result()])
            .expect("Failed to write scan VMO");
        responder.send(Ok(result)).expect("Failed to send result");
        assert_variant!(exec.run_until_stalled(&mut scan_fut), Poll::Ready(Ok(())));
        assert_eq!(iface.get_last_scan_results().len(), 1);
    }
}
