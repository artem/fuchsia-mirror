// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The complexity of a separate struct doesn't seem universally better than having many arguments
#![allow(clippy::too_many_arguments)]
#![recursion_limit = "1024"]

use {
    anyhow::{format_err, Context as _, Error},
    diagnostics_log::PublishOptions,
    fidl_fuchsia_location_namedplace::RegulatoryRegionWatcherMarker,
    fidl_fuchsia_wlan_device_service::DeviceMonitorMarker,
    fidl_fuchsia_wlan_policy as fidl_policy, fuchsia_async as fasync,
    fuchsia_async::DurationExt,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::component,
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_trace_provider as ftrace_provider,
    fuchsia_zircon::prelude::*,
    futures::{
        channel::{mpsc, oneshot},
        future::OptionFuture,
        lock::Mutex,
        prelude::*,
        select, TryFutureExt,
    },
    std::pin::pin,
    std::{convert::Infallible, sync::Arc},
    tracing::{error, info, warn},
    wlan_trace as wtrace,
    wlancfg_lib::{
        access_point::AccessPoint,
        client::{self, connection_selection::*, roaming::local_roam_manager, scan},
        config_management::{SavedNetworksManager, SavedNetworksManagerApi},
        legacy::{self, IfaceRef},
        mode_management::{
            create_iface_manager, device_monitor, iface_manager_api::IfaceManagerApi,
            phy_manager::PhyManager, recovery,
        },
        regulatory_manager::RegulatoryManager,
        telemetry::{
            connect_to_metrics_logger_factory, create_metrics_logger, serve_telemetry,
            TelemetrySender,
        },
        util,
    },
};

const REGULATORY_LISTENER_TIMEOUT_SEC: i64 = 30;

// Service name to persist Inspect data across boots
const PERSISTENCE_SERVICE_PATH: &str = "/svc/fuchsia.diagnostics.persist.DataPersistence-wlan";

async fn serve_fidl(
    ap: AccessPoint,
    configurator: legacy::deprecated_configuration::DeprecatedConfigurator,
    iface_manager: Arc<Mutex<dyn IfaceManagerApi + Send>>,
    legacy_client_ref: IfaceRef,
    saved_networks: Arc<dyn SavedNetworksManagerApi>,
    scan_requester: Arc<dyn scan::ScanRequestApi>,
    client_sender: util::listener::ClientListenerMessageSender,
    client_listener_msgs: mpsc::UnboundedReceiver<util::listener::ClientListenerMessage>,
    ap_listener_msgs: mpsc::UnboundedReceiver<util::listener::ApMessage>,
    regulatory_receiver: oneshot::Receiver<()>,
    telemetry_sender: TelemetrySender,
) -> Result<Infallible, Error> {
    // Wait a bit for the country code to be set before serving the policy APIs.
    let regulatory_listener_timeout =
        fasync::Timer::new(REGULATORY_LISTENER_TIMEOUT_SEC.seconds().after_now());
    select! {
        _ = regulatory_listener_timeout.fuse() => {
            info!(
                "Country code was not set after {} seconds.  Proceeding to serve policy API.",
                REGULATORY_LISTENER_TIMEOUT_SEC,
            );
        },
        result = regulatory_receiver.fuse() => {
            match result {
                Ok(()) => {
                    info!("Country code has been set.  Proceeding to serve policy API.");
                },
                Err(e) => info!("Waiting for initial country code failed: {:?}", e),
            }
        }
    }

    let mut fs = ServiceFs::new();

    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    let client_sender1 = client_sender.clone();
    let client_sender2 = client_sender.clone();

    let second_ap = ap.clone();

    let saved_networks_clone = saved_networks.clone();

    let client_provider_lock = Arc::new(Mutex::new(()));

    // TODO(sakuma): Once the legacy API is deprecated, the interface manager should default to
    // stopped.
    {
        let mut iface_manager = iface_manager.lock().await;
        iface_manager.start_client_connections().await?;
    }

    let _ = fs
        .dir("svc")
        .add_fidl_service(move |reqs| {
            fasync::Task::spawn(client::serve_provider_requests(
                iface_manager.clone(),
                client_sender1.clone(),
                Arc::clone(&saved_networks_clone),
                scan_requester.clone(),
                client_provider_lock.clone(),
                reqs,
                telemetry_sender.clone(),
            ))
            .detach()
        })
        .add_fidl_service(move |reqs| {
            fasync::Task::spawn(client::serve_listener_requests(client_sender2.clone(), reqs))
                .detach()
        })
        .add_fidl_service(move |reqs| {
            fasync::Task::spawn(ap.clone().serve_provider_requests(reqs)).detach()
        })
        .add_fidl_service(move |reqs| {
            fasync::Task::spawn(second_ap.clone().serve_listener_requests(reqs)).detach()
        })
        .add_fidl_service(move |reqs| {
            fasync::Task::spawn(configurator.clone().serve_deprecated_configuration(reqs)).detach()
        })
        .add_fidl_service(|reqs| {
            let fut =
                legacy::deprecated_client::serve_deprecated_client(reqs, legacy_client_ref.clone())
                    .unwrap_or_else(|e| error!("error serving deprecated client API: {}", e));
            fasync::Task::spawn(fut).detach()
        });
    let service_fut = fs.take_and_serve_directory_handle()?.collect::<()>().fuse();
    let mut service_fut = pin!(service_fut);

    let serve_client_policy_listeners = util::listener::serve::<
        fidl_policy::ClientStateUpdatesProxy,
        fidl_policy::ClientStateSummary,
        util::listener::ClientStateUpdate,
    >(client_listener_msgs)
    .fuse();
    let mut serve_client_policy_listeners = pin!(serve_client_policy_listeners);

    let serve_ap_policy_listeners = util::listener::serve::<
        fidl_policy::AccessPointStateUpdatesProxy,
        Vec<fidl_policy::AccessPointState>,
        util::listener::ApStatesUpdate,
    >(ap_listener_msgs)
    .fuse();
    let mut serve_ap_policy_listeners = pin!(serve_ap_policy_listeners);

    loop {
        select! {
            _ = service_fut => (),
            _ = serve_client_policy_listeners => (),
            _ = serve_ap_policy_listeners => (),
        }
    }
}

/// Calls the metric recording function immediately and every 24 hours.
async fn saved_networks_manager_metrics_loop(saved_networks: Arc<dyn SavedNetworksManagerApi>) {
    loop {
        saved_networks.record_periodic_metrics().await;
        fasync::Timer::new(24.hours().after_now()).await;
    }
}

// wlancfg expects to be able to get updates from the RegulatoryRegionWatcher UNLESS the
// service is not present in wlancfg's sandbox OR the product configuration does not offer the
// service to wlancfg.  If the RegulatoryRegionWatcher is not available for either of these
// allowed reasons, wlancfg will continue serving the WLAN policy API in WW mode.
async fn run_regulatory_manager(
    iface_manager: Arc<Mutex<dyn IfaceManagerApi + Send>>,
    regulatory_sender: oneshot::Sender<()>,
) -> Result<(), Error> {
    // This initial connection will always succeed due to the presence of the protocol in the
    // component manifest.
    let req = match fuchsia_component::client::new_protocol_connector::<RegulatoryRegionWatcherMarker>(
    ) {
        Ok(req) => req,
        Err(e) => {
            warn!("error probing RegulatoryRegionWatcher service: {:?}", e);
            return Ok(());
        }
    };

    // An error here indicates that there is a missing `use` statement and the
    // RegulatoryRegionWatcher is not available in this namespace.  This should never happen since
    // the build system checks the capability routes and wlancfg needs this service.
    if !req.exists().await.context("error checking for RegulatoryRegionWatcher existence")? {
        warn!("RegulatoryRegionWatcher is not available per the component manifest");
        return Ok(());
    }

    // The connect call will always succeed because all it does is send the server end of a channel
    // to the directory that holds the capability.
    let regulatory_svc =
        req.connect().context("unable to connect RegulatoryRegionWatcher proxy")?;

    let regulatory_manager = RegulatoryManager::new(regulatory_svc, iface_manager);

    // The only way to test for the presence of the RegulatoryRegionWatcher service is to actually
    // use the handle to poll for updates.  If the RegulatoryManager future exits, simply log the
    // reason and return Ok(()) so that the WLAN policy API will continue to be served.
    if let Some(e) = regulatory_manager.run(regulatory_sender).await.err() {
        warn!("RegulatoryManager exiting: {:?}", e);
    }
    Ok(())
}

async fn run_all_futures() -> Result<(), Error> {
    let monitor_svc = fuchsia_component::client::connect_to_protocol::<DeviceMonitorMarker>()
        .context("failed to connect to device monitor")?;
    let persistence_proxy = fuchsia_component::client::connect_to_protocol_at_path::<
        fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
    >(PERSISTENCE_SERVICE_PATH);
    let (persistence_req_sender, persistence_req_forwarder_fut) = match persistence_proxy {
        Ok(persistence_proxy) => {
            let (s, f) = auto_persist::create_persistence_req_sender(persistence_proxy);
            (s, OptionFuture::from(Some(f)))
        }
        Err(e) => {
            error!("Failed to connect to persistence service: {}", e);
            // To simplify the code, we still create mpsc::Sender, but there's nothing to forward
            // the tag to the Persistence service because we can't connect to it.
            // Note: because we drop the receiver here, be careful about log spam when sending
            //       tags through the `sender` below. This is automatically handled by
            //       `auto_persist::AutoPersist` because it only logs the first time sending
            //       fail, so just use that wrapper type instead of logging directly.
            let (sender, _receiver) = mpsc::channel::<String>(1);
            (sender, OptionFuture::from(None))
        }
    };

    // Cobalt 1.1
    let cobalt_1dot1_svc = connect_to_metrics_logger_factory().await?;
    let cobalt_1dot1_proxy = match create_metrics_logger(&cobalt_1dot1_svc, None).await {
        Ok(proxy) => proxy,
        Err(e) => {
            warn!("Metrics logging is unavailable: {}", e);

            // If it is not possible to acquire a metrics logging proxy, create a disconnected
            // proxy and attempt to serve the policy API with metrics disabled.
            let (proxy, _) =
                fidl::endpoints::create_proxy::<fidl_fuchsia_metrics::MetricEventLoggerMarker>()
                    .context("failed to create MetricEventLoggerMarker endponts")?;
            proxy
        }
    };

    let external_inspect_node = component::inspector().root().create_child("external");
    let (telemetry_sender, telemetry_fut) = serve_telemetry(
        monitor_svc.clone(),
        cobalt_1dot1_proxy,
        Box::new(move |experiments| {
            let cobalt_1dot1_svc = cobalt_1dot1_svc.clone();
            async move {
                create_metrics_logger(&cobalt_1dot1_svc, Some(experiments))
                    .await
                    .map_err(|e| format_err!("failed to update experiment ID: {}", e))
            }
            .boxed()
        }),
        component::inspector().root().create_child("client_stats"),
        external_inspect_node.create_child("client_stats"),
        persistence_req_sender.clone(),
    );
    component::inspector().root().record(external_inspect_node);

    let (scan_request_sender, scan_request_receiver) =
        mpsc::channel(scan::SCAN_REQUEST_BUFFER_SIZE);
    let scan_requester = Arc::new(scan::ScanRequester { sender: scan_request_sender });
    let saved_networks = Arc::new(SavedNetworksManager::new(telemetry_sender.clone()).await);

    let connection_selector = Arc::new(ConnectionSelector::new(
        saved_networks.clone(),
        scan_requester.clone(),
        component::inspector().root().create_child("connection_selector"),
        persistence_req_sender.clone(),
        telemetry_sender.clone(),
    ));
    let (connection_selection_request_sender, connection_selection_request_receiver) =
        mpsc::channel(CONNECTION_SELECTION_REQUEST_BUFFER_SIZE);
    let connection_selection_service = serve_connection_selection_request_loop(
        connection_selector,
        connection_selection_request_receiver,
    );
    let connection_selection_requester =
        ConnectionSelectionRequester::new(connection_selection_request_sender);

    let (roam_stats_sender, roam_stats_receiver) = mpsc::unbounded();
    let local_roam_manager = Arc::new(Mutex::new(local_roam_manager::LocalRoamManager::new(
        roam_stats_sender,
        telemetry_sender.clone(),
    )));
    let roam_manager_service = local_roam_manager::LocalRoamManagerService::new(
        roam_stats_receiver,
        telemetry_sender.clone(),
        connection_selection_requester.clone(),
    );
    let roam_manager_service_fut = roam_manager_service.serve();

    let (recovery_sender, recovery_receiver) =
        mpsc::channel::<recovery::RecoverySummary>(recovery::RECOVERY_SUMMARY_CHANNEL_CAPACITY);
    // Get the recovery settings.
    let cfg = wlancfg_config::Config::take_from_startup_handle();
    info!("Recovery Profile: {}", cfg.recovery_profile);
    info!("Recovery Enabled: {}", cfg.recovery_enabled);

    let phy_manager = Arc::new(Mutex::new(PhyManager::new(
        monitor_svc.clone(),
        recovery::lookup_recovery_profile(&cfg.recovery_profile),
        cfg.recovery_enabled,
        component::inspector().root().create_child("phy_manager"),
        telemetry_sender.clone(),
        recovery_sender,
    )));
    let configurator =
        legacy::deprecated_configuration::DeprecatedConfigurator::new(phy_manager.clone());

    let (watcher_proxy, watcher_server_end) = fidl::endpoints::create_proxy()?;
    monitor_svc.watch_devices(watcher_server_end)?;

    let (client_sender, client_receiver) = mpsc::unbounded();
    let (ap_sender, ap_receiver) = mpsc::unbounded();
    let (iface_manager, iface_manager_service) = create_iface_manager(
        phy_manager.clone(),
        client_sender.clone(),
        ap_sender.clone(),
        monitor_svc.clone(),
        saved_networks.clone(),
        local_roam_manager,
        connection_selection_requester.clone(),
        telemetry_sender.clone(),
        recovery_receiver,
    );

    let scanning_service = scan::serve_scanning_loop(
        iface_manager.clone(),
        saved_networks.clone(),
        telemetry_sender.clone(),
        scan::LocationSensorUpdater {},
        scan_request_receiver,
    );

    let legacy_client = IfaceRef::new();
    let device_event_listener = device_monitor::Listener::new(
        monitor_svc.clone(),
        legacy_client.clone(),
        phy_manager.clone(),
        iface_manager.clone(),
    );

    let (regulatory_sender, regulatory_receiver) = oneshot::channel();
    let ap = AccessPoint::new(iface_manager.clone(), ap_sender, Arc::new(Mutex::new(())));
    let fidl_fut = serve_fidl(
        ap,
        configurator,
        iface_manager.clone(),
        legacy_client,
        saved_networks.clone(),
        scan_requester,
        client_sender,
        client_receiver,
        ap_receiver,
        regulatory_receiver,
        telemetry_sender.clone(),
    );

    let dev_watcher_fut = watcher_proxy
        .take_event_stream()
        .try_for_each(|evt| device_monitor::handle_event(&device_event_listener, evt).map(Ok))
        .err_into()
        .and_then(|_| {
            let result: Result<(), Error> =
                Err(format_err!("Device watcher future exited unexpectedly"));
            future::ready(result)
        });

    let saved_networks_metrics_fut = saved_networks_manager_metrics_loop(saved_networks.clone());
    let regulatory_fut = run_regulatory_manager(iface_manager.clone(), regulatory_sender);

    let _ = futures::try_join!(
        fidl_fut,
        dev_watcher_fut,
        iface_manager_service,
        saved_networks_metrics_fut.map(Ok),
        scanning_service,
        regulatory_fut,
        telemetry_fut.map(Ok),
        persistence_req_forwarder_fut.map(Ok),
        roam_manager_service_fut.map(Ok),
        connection_selection_service.map(Ok)
    )?;
    Ok(())
}

// The return value from main() gets swallowed, including if it returns a Result<Err>. Therefore,
// use this simple wrapper to ensure that any errors from run_all_futures() are printed to the log.
#[fasync::run_singlethreaded]
async fn main() {
    let options =
        PublishOptions::default().tags(&["wlan"]).enable_metatag(diagnostics_log::Metatag::Target);
    diagnostics_log::initialize(options).unwrap();
    fuchsia_trace_provider::trace_provider_create_with_fdio();
    ftrace_provider::trace_provider_create_with_fdio();
    wtrace::instant_wlancfg_start();

    #[allow(clippy::large_futures)]
    if let Err(e) = run_all_futures().await {
        error!("{:?}", e);
    }
}
