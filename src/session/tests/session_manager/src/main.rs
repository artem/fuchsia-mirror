// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    diagnostics_hierarchy::DiagnosticsHierarchy,
    diagnostics_reader::{ArchiveReader, Inspect},
    fidl::endpoints::{create_endpoints, create_proxy, ServerEnd},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_hardware_suspend as fhsuspend,
    fidl_fuchsia_io as fio, fidl_fuchsia_power_broker as fbroker,
    fidl_fuchsia_power_suspend as fsuspend, fidl_fuchsia_power_system as fsystem,
    fidl_fuchsia_session as fsession, fidl_fuchsia_session_power as fpower,
    fidl_fuchsia_sys2 as fsys2, fidl_test_suspendcontrol as tsc,
    fidl_test_systemactivitygovernor as ftest, fuchsia_async as fasync,
    fuchsia_component::{client::connect_to_protocol, server::ServiceFs},
    fuchsia_component_test::{
        Capability, ChildOptions, ChildRef, DirectoryContents, RealmBuilder, RealmInstance, Ref,
        Route,
    },
    futures::{select, FutureExt, StreamExt},
    realm_proxy_client::RealmProxyClient,
    std::sync::{Arc, Mutex},
    tsc::DeviceProxy,
};

const SESSION_URL: &'static str = "hello-world-session#meta/hello-world-session.cm";

/// Passes if the root session launches successfully. This tells us:
///     - session_manager is able to use the Realm service to launch a component.
///     - the root session was started in the "session" collection.
#[fuchsia::test]
async fn launch_root_session() {
    let realm =
        connect_to_protocol::<fcomponent::RealmMarker>().expect("could not connect to Realm");

    let session_url = String::from(SESSION_URL);
    println!("Session url: {}", &session_url);

    // `launch_session()` requires an initial exposed-directory request, so create, pass and
    // immediately close a `Directory` channel.
    let (_exposed_dir, exposed_dir_server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();

    session_manager_lib::startup::launch_session(&session_url, exposed_dir_server_end, &realm)
        .await
        .expect("Failed to launch session");
}

// Add a `session_manager` component to the `RealmBuilder`, with the given
// config options set.
async fn add_session_manager(
    builder: &RealmBuilder,
    session_url: String,
    autolaunch: bool,
    suspend_enabled: bool,
) -> anyhow::Result<ChildRef> {
    let child_ref = builder
        .add_child("session-manager", "#meta/session_manager.cm", ChildOptions::new().eager())
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.session.SessionUrl".to_string().parse()?,
            value: session_url.into(),
        }))
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.session.AutoLaunch".to_string().parse()?,
            value: autolaunch.into(),
        }))
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.power.SuspendEnabled".to_string().parse()?,
            value: suspend_enabled.into(),
        }))
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.session.SessionUrl"))
                .capability(Capability::configuration("fuchsia.session.AutoLaunch"))
                .capability(Capability::configuration("fuchsia.power.SuspendEnabled"))
                .from(Ref::self_())
                .to(&child_ref),
        )
        .await?;
    Ok(child_ref)
}

async fn get_session_manager_inspect(
    realm: &fuchsia_component_test::RealmInstance,
    selector: &str,
) -> anyhow::Result<DiagnosticsHierarchy> {
    ArchiveReader::new()
        .add_selector(format!(
            "realm_builder\\:{}/session-manager:{}",
            realm.root.child_name(),
            selector
        ))
        .snapshot::<Inspect>()
        .await?
        .pop()
        .ok_or(anyhow::anyhow!("inspect data had no snapshot"))?
        .payload
        .ok_or(anyhow::anyhow!("inspect snapshot had no payload"))
}

#[fuchsia::test]
async fn test_autolaunch_launches() -> anyhow::Result<()> {
    let builder = RealmBuilder::new().await?;

    add_session_manager(
        &builder,
        "hello-world-session#meta/hello-world-session.cm".to_string(),
        true,
        false,
    )
    .await?;

    let realm = builder.build().await?;

    let inspect = get_session_manager_inspect(&realm, "root/session_started_at/0").await?;

    // Assert the session has been launched once.
    assert_eq!(
        1,
        inspect
            .get_child("session_started_at")
            .expect("session_started_at is not none")
            .children
            .len()
    );

    realm.destroy().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_noautolaunch_does_not_launch() -> anyhow::Result<()> {
    let builder = RealmBuilder::new().await?;

    add_session_manager(
        &builder,
        "hello-world-session#meta/hello-world-session.cm".to_string(),
        false,
        false,
    )
    .await?;

    let realm = builder.build().await?;

    let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;

    // No sessions should have launched.
    assert_eq!(0, inspect.get_child("session_started_at").unwrap().children.len());

    realm.destroy().await?;
    Ok(())
}

#[fuchsia::test]
async fn noautolaunch_file_overrides_structured_config() -> anyhow::Result<()> {
    let builder = RealmBuilder::new().await?;

    let session_manager = add_session_manager(
        &builder,
        "hello-world-session#meta/hello-world-session.cm".to_string(),
        true,
        false,
    )
    .await?;

    builder
        .read_only_directory(
            "root-data",
            vec![&session_manager],
            DirectoryContents::new().add_file("session-manager/noautolaunch", ""),
        )
        .await?;

    let realm = builder.build().await?;

    let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;

    // No sessions should have launched.
    assert_eq!(0, inspect.get_child("session_started_at").unwrap().children.len());

    realm.destroy().await?;
    Ok(())
}

async fn create_system_activity_governor_realm(
) -> anyhow::Result<(RealmProxyClient, String, tsc::DeviceProxy)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let result = realm_factory
        .create_realm(server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    let client = RealmProxyClient::from(client);
    let suspend_control = client.connect_to_protocol::<tsc::DeviceMarker>().await?;
    Ok((client, result, suspend_control))
}

async fn set_up_default_suspender(device: &tsc::DeviceProxy) {
    device
        .set_suspend_states(&tsc::DeviceSetSuspendStatesRequest {
            suspend_states: Some(vec![fhsuspend::SuspendState {
                resume_latency: Some(0),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap()
}

struct SessionManagerPowerTest {
    sag_realm: Arc<RealmProxyClient>,
    suspend_device: DeviceProxy,
    session_manager_realm: RealmInstance,
    handoff: fpower::HandoffProxy,
}

impl SessionManagerPowerTest {
    async fn new(suspend_enabled: bool) -> anyhow::Result<Self> {
        let (sag_realm, _, suspend_device) = create_system_activity_governor_realm().await?;
        let sag_realm = Arc::new(sag_realm);
        set_up_default_suspender(&suspend_device).await;

        // Build the `session-manager` realm.
        let builder = RealmBuilder::new().await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.sys2.RealmQuery"))
                    .from(Ref::framework())
                    .to(Ref::parent()),
            )
            .await?;
        let session_manager =
            add_session_manager(&builder, "".to_string(), false, suspend_enabled).await?;

        // Route power protocols to `session-manager` using a proxying local child.
        let sag_realm_1 = sag_realm.clone();
        let sag_realm_2 = sag_realm.clone();
        let mut fs = ServiceFs::new();
        fs.dir("svc")
            .add_service_connector(move |server_end: ServerEnd<fbroker::TopologyMarker>| {
                let sag_realm_1 = sag_realm_1.clone();
                fasync::Task::spawn(async move {
                    sag_realm_1.connect_server_end_to_protocol(server_end).await.unwrap()
                })
                .detach();
            })
            .add_service_connector(
                move |server_end: ServerEnd<fsystem::ActivityGovernorMarker>| {
                    let sag_realm_2 = sag_realm_2.clone();
                    fasync::Task::spawn(async move {
                        sag_realm_2.connect_server_end_to_protocol(server_end).await.unwrap()
                    })
                    .detach();
                },
            );
        let fs_holder = Mutex::new(Some(fs));
        let sag_proxy = builder
            .add_local_child(
                "sag_proxy",
                move |handles| {
                    let mut rfs = fs_holder
                        .lock()
                        .unwrap()
                        .take()
                        .expect("mock component should only be launched once");
                    async {
                        rfs.serve_connection(handles.outgoing_dir).unwrap();
                        Ok(rfs.collect().await)
                    }
                    .boxed()
                },
                ChildOptions::new(),
            )
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                    .capability(Capability::protocol_by_name(
                        "fuchsia.power.system.ActivityGovernor",
                    ))
                    .from(&sag_proxy)
                    .to(&session_manager),
            )
            .await?;

        // Launch a session that gives us access to the `fuchsia.session.power/Handoff` protocol.
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.session.Launcher"))
                    .capability(Capability::protocol_by_name("fuchsia.session.Restarter"))
                    .from(&session_manager)
                    .to(Ref::parent()),
            )
            .await?;
        let realm = builder.build().await?;
        let launcher =
            realm.root.connect_to_protocol_at_exposed_dir::<fsession::LauncherMarker>().unwrap();
        launcher
            .launch(&fsession::LaunchConfiguration {
                session_url: Some("#meta/use-power-fidl.cm".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        let inspect = get_session_manager_inspect(&realm, "root/session_started_at").await?;
        assert_eq!(
            1,
            inspect
                .get_child("session_started_at")
                .expect("session_started_at is not none")
                .children
                .len()
        );

        // Open the session's namespace dir.
        let realm_query =
            realm.root.connect_to_protocol_at_exposed_dir::<fsys2::RealmQueryMarker>().unwrap();
        let (namespace, server_end) = create_endpoints::<fio::DirectoryMarker>();
        realm_query
            .open(
                "session-manager/session:session",
                fsys2::OpenDirType::NamespaceDir,
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end.into_channel().into(),
            )
            .await
            .unwrap()
            .unwrap();
        let handoff = fuchsia_component::client::connect_to_protocol_at_dir_svc::<
            fpower::HandoffMarker,
        >(&namespace)
        .unwrap();

        Ok(Self { sag_realm, suspend_device, session_manager_realm: realm, handoff })
    }
}

// If session manager is configured to not support suspend, taking the power lease should error.
#[fuchsia::test]
async fn take_power_lease_unsupported() -> anyhow::Result<()> {
    let test = SessionManagerPowerTest::new(false).await?;
    assert_eq!(test.handoff.take().await.unwrap().unwrap_err(), fpower::HandoffError::Unavailable);
    Ok(())
}

/// An integration test that verifies a session can take the power lease from
/// `session-manager`. Furthermore, dropping the lease will cause the system
/// to suspend.
#[fuchsia::test]
async fn take_power_lease_and_suspend() -> anyhow::Result<()> {
    let test = SessionManagerPowerTest::new(true).await?;

    // Take the power lease.
    let lease = test.handoff.take().await.unwrap().unwrap();

    // Taking again should error.
    assert_eq!(test.handoff.take().await.unwrap().unwrap_err(), fpower::HandoffError::AlreadyTaken);

    // The system should not suspend. Wait a while for a suspend that should not happen.
    // A timeout is not the most ideal. But if SAG incorrectly requested suspend, this test
    // will flake. Having a flakily failing test should be better than no test.
    let mut await_suspend = Box::pin(test.suspend_device.await_suspend().fuse());
    select! {
        suspended = &mut await_suspend => panic!("Unexpected suspend event {suspended:?}"),
        _ = fasync::Timer::new(fasync::Duration::from_millis(400)).fuse() => {},
    };
    let stats = test.sag_realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    // Drop the power lease.
    drop(lease);

    // The system should then suspend.
    assert_eq!(0, await_suspend.await.unwrap().unwrap().state_index.unwrap());
    test.suspend_device
        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
            suspend_duration: Some(2i64),
            suspend_overhead: Some(1i64),
            ..Default::default()
        }))
        .await
        .unwrap()
        .unwrap();

    // When resumed we should see updated stats.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);
    Ok(())
}

/// If the session restarted after the lease is taken, the system should still not suspend.
#[fuchsia::test]
async fn take_power_lease_and_restart() -> anyhow::Result<()> {
    let test = SessionManagerPowerTest::new(true).await?;

    // Take the power lease.
    let lease = test.handoff.take().await.unwrap().unwrap();

    // Restart the session.
    let restarter = test
        .session_manager_realm
        .root
        .connect_to_protocol_at_exposed_dir::<fsession::RestarterMarker>()
        .unwrap();
    restarter.restart().await.unwrap().unwrap();

    // Drop the power lease. This simulates the session getting killed as part of restart.
    drop(lease);

    // The system should not suspend. Wait a while for a suspend that should not happen.
    // A timeout is not the most ideal. But if SAG incorrectly requested suspend, this test
    // will flake. Having a flakily failing test should be better than no test.
    let mut await_suspend = Box::pin(test.suspend_device.await_suspend().fuse());
    select! {
        suspended = &mut await_suspend => panic!("Unexpected suspend event {suspended:?}"),
        _ = fasync::Timer::new(fasync::Duration::from_millis(400)).fuse() => {},
    };
    let stats = test.sag_realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);
    Ok(())
}
