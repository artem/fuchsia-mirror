// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl_fuchsia_io as fio,
    fidl_test_systemactivitygovernor::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
        DEFAULT_COLLECTION_NAME,
    },
    fuchsia_zircon as zx,
    futures::{FutureExt, StreamExt, TryStreamExt},
    tracing::*,
};

const ACTIVITY_GOVERNOR_CHILD_NAME: &str = "system-activity-governor";

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(stream: RealmFactoryRequestStream) {
    if let Err(err) = handle_request_stream(stream).await {
        error!("{:?}", err);
    }
}

async fn handle_request_stream(mut stream: RealmFactoryRequestStream) -> Result<()> {
    let mut task_group = fasync::TaskGroup::new();
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                let realm = create_realm(options).await?;
                let moniker = format!(
                    "{}:{}/{}",
                    DEFAULT_COLLECTION_NAME,
                    realm.root.child_name(),
                    ACTIVITY_GOVERNOR_CHILD_NAME
                );

                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok(&moniker))?;
            }

            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

async fn serve_suspend_dir(
    handles: LocalComponentHandles,
    handler: SuspenderHandlerProxy,
) -> Result<()> {
    let mut fs = ServiceFs::new();
    fs.dir("dev").add_service_at("000", move |channel: zx::Channel| {
        handler.handle(channel.into()).unwrap();
        None
    });

    fs.serve_connection(handles.outgoing_dir.into_channel().into()).unwrap();
    fs.collect::<()>().await;
    Ok(())
}

async fn create_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let builder = RealmBuilder::new().await?;
    let suspender_handler = options.suspender_handler.unwrap().into_proxy().unwrap();

    // TODO(mbrunson): Replace this implementation with driver-test-realm.
    let suspend_devices = builder
        .add_local_child(
            "suspend-devices",
            move |h| serve_suspend_dir(h, suspender_handler.clone()).boxed(),
            ChildOptions::new(),
        )
        .await?;

    let component_ref = builder
        .add_child(
            ACTIVITY_GOVERNOR_CHILD_NAME,
            "#meta/system-activity-governor.cm",
            ChildOptions::new(),
        )
        .await?;

    let power_broker_ref =
        builder.add_child("power-broker", "#meta/power-broker.cm", ChildOptions::new()).await?;

    // Expose capabilities from power-broker.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(Ref::parent()),
        )
        .await?;

    // Expose capabilities from power-broker to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from suspend-devices to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("dev-class-suspend").path("/dev").rights(fio::R_STAR_DIR),
                )
                .from(&suspend_devices)
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.suspend.Stats"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.ActivityGovernor"))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}
