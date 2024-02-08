// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl_test_rolemanager::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    futures::{StreamExt, TryStreamExt},
    tracing::*,
};

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
                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok(()))?;
            }

            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

async fn create_realm(_options: RealmOptions) -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let component_ref =
        builder.add_child("role_manager", "#meta/role_manager.cm", ChildOptions::new()).await?;

    // Expose capabilities from RoleManager.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.scheduler.RoleManager"))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await?;

    // Route the profile resource to RoleManager.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.kernel.ProfileResource"))
                .from(Ref::parent())
                .to(&component_ref),
        )
        .await?;

    // Route our /pkg/profiles directory as the /config/profiles directory RoleManager needs.
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("pkg").subdir("profiles").as_("config-profiles"))
                .from(Ref::framework())
                .to(&component_ref),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}
