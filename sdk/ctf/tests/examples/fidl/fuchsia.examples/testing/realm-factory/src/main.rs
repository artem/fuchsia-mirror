// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl_fuchsia_examples::EchoMarker,
    fidl_test_example::{RealmFactoryRequest, RealmFactoryRequestStream, RealmOptions},
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

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::_UnknownMethod { .. } => unimplemented!(),
                RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                    let realm = create_realm(options).await?;
                    let request_stream = realm_server.into_stream()?;
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream).await.unwrap();
                    });
                    responder.send(Ok(()))?;
                }
            }
        }
        task_group.join().await;
        Ok(())
    }
    .await;
    if let Err(err) = result {
        error!("{:?}", err);
    }
}

async fn create_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);
    let builder = RealmBuilder::new().await?;
    let echo = builder.add_child("echo", "#meta/echo_server.cm", ChildOptions::new()).await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<EchoMarker>())
                .from(&echo)
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;
    Ok(realm)
}
