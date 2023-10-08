// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod mocks;
mod realm_factory;
use crate::realm_factory::*;

use {
    anyhow::{Error, Result},
    fidl_test_sampler::{RealmFactoryRequest, RealmFactoryRequestStream},
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    futures::{StreamExt, TryStreamExt},
    tracing::error,
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
                RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                    let realm = create_realm(options).await?;
                    let request_stream = realm_server.into_stream()?;
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream).await.unwrap();
                    });
                    responder.send(Ok(()))?;
                }

                RealmFactoryRequest::_UnknownMethod { .. } => todo!(),
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
