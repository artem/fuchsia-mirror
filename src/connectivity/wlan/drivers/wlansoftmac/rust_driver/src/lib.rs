// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_async as fasync,
    fuchsia_inspect::{self, Inspector},
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{channel::mpsc, Future, FutureExt, StreamExt},
    std::pin::Pin,
    tracing::{error, info},
    wlan_mlme::{
        buffer::BufferProvider,
        device::{
            self,
            completers::{StartStaCompleter, StopStaCompleter},
            Device, DeviceInterface, DeviceOps, WlanSoftmacIfcProtocol,
        },
    },
    wlan_sme::{self, serve::create_sme},
};

pub struct WlanSoftmacHandle(mpsc::UnboundedSender<DriverEvent>);

impl std::fmt::Debug for WlanSoftmacHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("WlanSoftmacHandle").finish()
    }
}

pub type DriverEvent = wlan_mlme::DriverEvent;

impl WlanSoftmacHandle {
    pub fn stop(mut self, stop_sta_completer: StopStaCompleter) {
        let driver_event_sink = &mut self.0;
        if let Err(e) = driver_event_sink.unbounded_send(DriverEvent::Stop(stop_sta_completer)) {
            error!("Failed to signal WlanSoftmac main loop thread to stop: {}", e);
            if let DriverEvent::Stop(stop_sta_completer) = e.into_inner() {
                stop_sta_completer.complete();
            } else {
                unreachable!();
            }
        }
        driver_event_sink.disconnect();
    }

    pub fn queue_eth_frame_tx(&mut self, bytes: Vec<u8>) -> Result<(), zx::Status> {
        let driver_event_sink = &mut self.0;
        driver_event_sink.unbounded_send(DriverEvent::EthFrameTx { bytes }).map_err(|e| {
            error!("Failed to queue ethernet frame: {:?}", e);
            zx::Status::INTERNAL
        })
    }
}

pub fn start_wlansoftmac(
    start_sta_completer: impl FnOnce(Result<WlanSoftmacHandle, zx::Status>) + Send + 'static,
    device: DeviceInterface,
    buf_provider: BufferProvider,
    wlan_softmac_bridge_proxy_raw_handle: fuchsia_zircon::sys::zx_handle_t,
) {
    let wlan_softmac_bridge_proxy = {
        let handle = unsafe { fidl::Handle::from_raw(wlan_softmac_bridge_proxy_raw_handle) };
        let channel = fidl::Channel::from(handle);
        fidl_softmac::WlanSoftmacBridgeSynchronousProxy::new(channel)
    };
    let mut executor = fasync::LocalExecutor::new();
    executor.run_singlethreaded(start_wlansoftmac_async(
        start_sta_completer,
        Device::new(device, wlan_softmac_bridge_proxy),
        buf_provider,
    ))
}

const INSPECT_VMO_SIZE_BYTES: usize = 1000 * 1024;

/// This is a helper function for running wlansoftmac inside a test. For non-test
/// use cases, it should generally be invoked via `start_wlansoftmac`.
///
/// TODO(316928740): This function is no longer async, but removing the async keyword is non-trivial
/// because of the number of tests that treat this function as async.
async fn start_wlansoftmac_async<D: DeviceOps + Send + 'static>(
    start_sta_completer: impl FnOnce(Result<WlanSoftmacHandle, zx::Status>) + Send + 'static,
    device: D,
    buf_provider: BufferProvider,
) {
    let (driver_event_sink, driver_event_stream) = mpsc::unbounded();
    let inspector =
        Inspector::new(fuchsia_inspect::InspectorConfig::default().size(INSPECT_VMO_SIZE_BYTES));
    let inspect_usme_node = inspector.root().create_child("usme");

    info!("Spawning wlansoftmac main loop thread.");

    let driver_event_sink_clone = driver_event_sink.clone();
    wlansoftmac_thread(
        StartStaCompleter::new(move |result: Result<(), zx::Status>| {
            start_sta_completer(result.map(|()| WlanSoftmacHandle(driver_event_sink_clone)))
        }),
        device,
        buf_provider,
        driver_event_sink,
        driver_event_stream,
        inspector,
        inspect_usme_node,
    )
    .await;
}

async fn wlansoftmac_thread<D: DeviceOps>(
    start_sta_completer: StartStaCompleter<impl FnOnce(Result<(), zx::Status>) + Send>,
    mut device: D,
    buf_provider: BufferProvider,
    driver_event_sink: mpsc::UnboundedSender<DriverEvent>,
    driver_event_stream: mpsc::UnboundedReceiver<DriverEvent>,
    inspector: Inspector,
    inspect_usme_node: fuchsia_inspect::Node,
) {
    let mut driver_event_sink = wlan_mlme::DriverEventSink(driver_event_sink);

    let ifc = WlanSoftmacIfcProtocol::new(&mut driver_event_sink);

    let (softmac_ifc_bridge_client, _softmac_ifc_bridge_server) =
        fidl::endpoints::create_endpoints::<fidl_softmac::WlanSoftmacIfcBridgeMarker>();

    // Indicate to the vendor driver that we can start sending and receiving
    // info. Any messages received from the driver before we start our SME will
    // be safely buffered in our driver_event_sink.
    // Note that device.start will copy relevant fields out of ifc, so dropping
    // it after this is fine. The returned value is the MLME server end of the
    // channel wlanmevicemonitor created to connect MLME and SME.
    let usme_bootstrap_handle_via_iface_creation = match device
        .start(&ifc, zx::Handle::from(softmac_ifc_bridge_client.into_channel()).into_raw())
    {
        Ok(handle) => handle,
        Err(e) => {
            // Failure to unwrap indicates a critical failure in the driver init thread.
            error!("device.start failed: {}", e);
            start_sta_completer.complete(Err(e));
            return;
        }
    };
    let channel = zx::Channel::from(usme_bootstrap_handle_via_iface_creation);
    let server = fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(channel);
    let (mut usme_bootstrap_stream, _usme_bootstrap_control_handle) =
        match server.into_stream_and_control_handle() {
            Ok(res) => res,
            Err(e) => {
                // Failure to unwrap indicates a critical failure in the driver init thread.
                error!("Failed to get usme bootstrap stream: {}", e);
                start_sta_completer.complete(Err(zx::Status::INTERNAL));
                return;
            }
        };

    let (generic_sme_server, legacy_privacy_support, responder) =
        match usme_bootstrap_stream.next().await {
            Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
                generic_sme_server,
                legacy_privacy_support,
                responder,
                ..
            })) => (generic_sme_server, legacy_privacy_support, responder),
            Some(Err(e)) => {
                error!("USME bootstrap stream failed: {}", e);
                start_sta_completer.complete(Err(zx::Status::INTERNAL));
                return;
            }
            None => {
                error!("USME bootstrap stream terminated");
                start_sta_completer.complete(Err(zx::Status::INTERNAL));
                return;
            }
        };
    let inspect_vmo = match inspector.duplicate_vmo() {
        Some(vmo) => vmo,
        None => {
            error!("Failed to duplicate inspect VMO");
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };
    if let Err(e) = responder.send(inspect_vmo).into() {
        error!("Failed to respond to USME bootstrap: {}", e);
        start_sta_completer.complete(Err(zx::Status::INTERNAL));
        return;
    }
    let generic_sme_stream = match generic_sme_server.into_stream() {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to get generic SME stream: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };

    let softmac_info = device::try_query(&mut device).unwrap();
    let sta_addr = softmac_info.sta_addr;
    let device_info = match wlan_mlme::mlme_device_info_from_softmac(softmac_info) {
        Ok(info) => info,
        Err(e) => {
            error!("Failed to get MLME device info: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };

    let mac_sublayer_support = match device::try_query_mac_sublayer_support(&mut device) {
        Ok(s) => {
            if s.device.mac_implementation_type != fidl_common::MacImplementationType::Softmac {
                error!("Wrong MAC implementation type: {:?}", s.device.mac_implementation_type);
                start_sta_completer.complete(Err(zx::Status::INTERNAL));
                return;
            }
            s
        }
        Err(e) => {
            error!("Failed to parse device mac sublayer support: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };
    let security_support = match device::try_query_security_support(&mut device) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to parse device security support: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };
    let spectrum_management_support = match device.spectrum_management_support() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to parse device spectrum management support: {}", e);
            start_sta_completer.complete(Err(e));
            return;
        }
    };

    // TODO(https://fxbug.dev/113677): Get persistence working by adding the appropriate configs
    //                         in *.cml files
    let (persistence_proxy, _persistence_server_end) = match fidl::endpoints::create_proxy::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
    >() {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to create persistence proxy: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };
    let (persistence_req_sender, _persistence_req_forwarder_fut) =
        auto_persist::create_persistence_req_sender(persistence_proxy);

    let config = wlan_sme::Config {
        wep_supported: legacy_privacy_support.wep_supported,
        wpa1_supported: legacy_privacy_support.wpa1_supported,
    };

    // TODO(https://fxbug.dev/126324): The MLME event stream should be moved out
    // of DeviceOps entirely.
    let mlme_event_stream = match device.take_mlme_event_stream() {
        Some(mlme_event_stream) => mlme_event_stream,
        None => {
            error!("Failed to take MLME event stream.");
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };
    let (mlme_request_stream, sme_fut) = match create_sme(
        config,
        mlme_event_stream,
        &device_info,
        mac_sublayer_support,
        security_support,
        spectrum_management_support,
        inspect_usme_node,
        persistence_req_sender,
        generic_sme_stream,
    ) {
        Ok((mlme_request_stream, sme_fut)) => (mlme_request_stream, sme_fut),
        Err(e) => {
            error!("Failed to create sme: {}", e);
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            return;
        }
    };

    let mlme_fut: Pin<Box<dyn Future<Output = ()>>> = match device_info.role {
        fidl_common::WlanMacRole::Client => {
            info!("Running wlansoftmac with client role");
            let config = wlan_mlme::client::ClientConfig {
                ensure_on_channel_time: fasync::Duration::from_millis(500).into_nanos(),
            };
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::client::ClientMlme<D>>(
                start_sta_completer,
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
            ))
        }
        fidl_common::WlanMacRole::Ap => {
            info!("Running wlansoftmac with AP role");
            let sta_addr = match sta_addr {
                Some(sta_addr) => sta_addr,
                None => {
                    error!("Driver provided no STA address.");
                    start_sta_completer.complete(Err(zx::Status::INTERNAL));
                    return;
                }
            };
            let config = ieee80211::Bssid::from(sta_addr);
            Box::pin(wlan_mlme::mlme_main_loop::<wlan_mlme::ap::Ap<D>>(
                start_sta_completer,
                config,
                device,
                buf_provider,
                mlme_request_stream,
                driver_event_stream,
            ))
        }
        unsupported => {
            error!("Unsupported mac role: {:?}", unsupported);
            return;
        }
    };

    let mut mlme_fut = mlme_fut.fuse();
    let mut sme_fut = sme_fut.fuse();
    loop {
        futures::select! {
            () = mlme_fut => info!("MLME future complete"),
            sme_result = sme_fut =>
                match sme_result {
                    Ok(()) => {
                        info!("SME future complete");
                    }
                    Err(e) => {
                        error!("SME shut down with error: {}", e);
                    }
                },
            complete => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fidl::endpoints::Proxy,
        futures::{channel::oneshot, task::Poll},
        pin_utils::pin_mut,
        wlan_common::assert_variant,
        wlan_mlme::{self, device::test_utils::FakeDevice},
    };

    fn run_wlansoftmac_setup(
        exec: &mut fasync::TestExecutor,
        handle_sender: oneshot::Sender<Result<WlanSoftmacHandle, zx::Status>>,
    ) -> Result<
        (Option<Pin<Box<impl Future<Output = ()>>>>, fidl_sme::GenericSmeProxy),
        anyhow::Error,
    > {
        run_wlansoftmac_setup_with_device(exec, handle_sender, FakeDevice::new(exec).0)
    }

    fn run_wlansoftmac_setup_with_device(
        exec: &mut fasync::TestExecutor,
        handle_sender: oneshot::Sender<Result<WlanSoftmacHandle, zx::Status>>,
        fake_device: FakeDevice,
    ) -> Result<
        (Option<Pin<Box<impl Future<Output = ()>>>>, fidl_sme::GenericSmeProxy),
        anyhow::Error,
    > {
        let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();
        let main_fut = start_wlansoftmac_async(
            move |result: Result<WlanSoftmacHandle, zx::Status>| {
                handle_sender.send(result).expect("Failed to signal startup completion.")
            },
            fake_device.clone(),
            fake_buf_provider,
        );
        let mut main_fut = Box::pin(main_fut);

        let usme_client_proxy = fake_device
            .state()
            .lock()
            .unwrap()
            .usme_bootstrap_client_end
            .take()
            .unwrap()
            .into_proxy()?;
        let legacy_privacy_support =
            fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false };
        let (generic_sme_proxy, generic_sme_server) =
            fidl::endpoints::create_proxy::<fidl_sme::GenericSmeMarker>()?;

        let inspect_vmo_fut = usme_client_proxy.start(generic_sme_server, &legacy_privacy_support);
        let main_fut = match exec.run_until_stalled(&mut main_fut) {
            Poll::Pending => Some(main_fut),
            Poll::Ready(()) => None,
        };
        let inspect_vmo = exec.run_singlethreaded(inspect_vmo_fut);
        inspect_vmo.expect("Failed to bootstrap USME.");

        Ok((main_fut, generic_sme_proxy))
    }

    #[test]
    fn stop_leads_to_graceful_shutdown() {
        let mut exec = fasync::TestExecutor::new();
        let (handle_sender, handle_receiver) =
            oneshot::channel::<Result<WlanSoftmacHandle, zx::Status>>();
        let (main_fut, generic_sme_proxy) = run_wlansoftmac_setup(&mut exec, handle_sender)
            .expect("Failed to initiate wlansoftmac setup.");
        let mut main_fut = main_fut.unwrap();
        let (sme_telemetry_proxy, sme_telemetry_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        let (client_proxy, client_server) =
            fidl::endpoints::create_proxy().expect("Failed to create_proxy");
        assert_eq!(exec.run_until_stalled(&mut main_fut), Poll::Pending);
        let handle = exec
            .run_singlethreaded(handle_receiver)
            .unwrap()
            .expect("Failed to start wlansoftmac.");

        let resp_fut = generic_sme_proxy.get_sme_telemetry(sme_telemetry_server);
        pin_mut!(resp_fut);
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut main_fut), Poll::Pending);
        exec.run_singlethreaded(resp_fut)
            .expect("Generic SME proxy failed")
            .expect("SME telemetry request failed");

        let resp_fut = generic_sme_proxy.get_client_sme(client_server);
        pin_mut!(resp_fut);
        assert_variant!(exec.run_until_stalled(&mut resp_fut), Poll::Pending);
        assert_eq!(exec.run_until_stalled(&mut main_fut), Poll::Pending);
        exec.run_singlethreaded(resp_fut)
            .expect("Generic SME proxy failed")
            .expect("Client SME request failed");

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        handle.stop(StopStaCompleter::new(Box::new(move || {
            shutdown_sender.send(()).expect("Failed to signal shutdown completion.")
        })));
        assert_variant!(
            exec.run_singlethreaded(async { futures::join!(main_fut, shutdown_receiver) }),
            ((), Ok(()))
        );

        // All SME proxies should shutdown.
        assert!(generic_sme_proxy.is_closed());
        assert!(sme_telemetry_proxy.is_closed());
        assert!(client_proxy.is_closed());
    }

    #[test]
    fn wlansoftmac_bad_mac_role_fails_startup_with_bad_features() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device_state.lock().unwrap().mac_sublayer_support.device.is_synthetic = true;
        fake_device_state.lock().unwrap().mac_sublayer_support.device.mac_implementation_type =
            fidl_common::MacImplementationType::Fullmac;
        let (handle_sender, mut handle_receiver) =
            oneshot::channel::<Result<WlanSoftmacHandle, zx::Status>>();
        let (main_fut, _generic_sme_proxy) =
            run_wlansoftmac_setup_with_device(&mut exec, handle_sender, fake_device)
                .expect("Failed to initiate wlansoftmac setup.");
        assert_variant!(
            main_fut,
            None,
            "Main future did not immediately terminate upon failed setup."
        );

        exec.run_singlethreaded(&mut handle_receiver)
            .unwrap()
            .expect_err("Softmac setup should fail.");
    }

    #[test]
    fn wlansoftmac_startup_fails_on_bad_bootstrap() {
        let mut exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        let fake_buf_provider = wlan_mlme::buffer::FakeBufferProvider::new();
        let (handle_sender, handle_receiver) =
            oneshot::channel::<Result<WlanSoftmacHandle, zx::Status>>();
        let main_fut = start_wlansoftmac_async(
            move |result: Result<WlanSoftmacHandle, zx::Status>| {
                handle_sender.send(result).expect("Failed to signal startup completion.")
            },
            fake_device.clone(),
            fake_buf_provider,
        );
        fake_device_state.lock().unwrap().usme_bootstrap_client_end = None; // Drop the client end.
        assert_variant!(
            exec.run_singlethreaded(async { futures::join!(main_fut, handle_receiver,) }),
            ((), Ok(Err(zx::Status::INTERNAL)))
        )
    }
}
