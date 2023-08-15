// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod client;
mod provider;

use fidl_fuchsia_net_dhcpv6::ClientProviderRequestStream;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use futures::{future, StreamExt as _, TryStreamExt as _};

use anyhow::{Error, Result};
use tracing::info;

enum IncomingService {
    ClientProvider(ClientProviderRequestStream),
}

#[fuchsia::main()]
pub async fn main() -> Result<()> {
    info!("starting");

    let mut fs = ServiceFs::new_local();
    let _: &mut ServiceFsDir<'_, _> =
        fs.dir("svc").add_fidl_service(IncomingService::ClientProvider);
    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle()?;

    fs.then(future::ok::<_, Error>)
        .try_for_each_concurrent(None, |request| async {
            match request {
                IncomingService::ClientProvider(client_provider_request_stream) => {
                    Ok(provider::run_client_provider(
                        client_provider_request_stream,
                        client::serve_client,
                    )
                    .await)
                }
            }
        })
        .await
}
