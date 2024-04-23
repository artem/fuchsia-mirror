// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::{create_endpoints, DiscoverableProtocolMarker},
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_driver_testing as ftest, fidl_fuchsia_io as fio,
    fidl_fuchsia_nodegroup_test as ft, fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon as zx,
    futures::prelude::*,
    futures::{channel::mpsc, StreamExt, TryStreamExt},
    realm_client::{extend_namespace, InstalledNamespace},
    tracing::info,
};

async fn run_waiter_server(mut stream: ft::WaiterRequestStream, mut sender: mpsc::Sender<()>) {
    while let Some(ft::WaiterRequest::Ack { status, .. }) =
        stream.try_next().await.expect("Stream failed")
    {
        assert_eq!(status, zx::Status::OK.into_raw());
        info!("Received Ack request");
        sender.try_send(()).expect("Sender failed")
    }
}

async fn run_offers_server(
    offers_server: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    sender: mpsc::Sender<()>,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream: ft::WaiterRequestStream| {
        fasync::Task::spawn(run_waiter_server(stream, sender.clone())).detach()
    });
    // Serve the outgoing services
    fs.serve_connection(offers_server)?;
    Ok(fs.collect::<()>().await)
}

async fn create_realm(options: ftest::RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();
    realm_factory
        .create_realm2(options, dict_server)
        .await?
        .map_err(realm_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;
    Ok(ns)
}

#[fuchsia::test]
async fn test_nodegroup() -> Result<()> {
    let (offers_client, offers_server) = create_endpoints();
    let (pkg_client, pkg_server) = create_endpoints();

    fuchsia_fs::directory::open_channel_in_namespace(
        "/pkg",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        pkg_server,
    )
    .expect("Could not open /pkg");

    let realm_options = ftest::RealmOptions {
        driver_test_realm_start_args: Some(fdt::RealmArgs {
            pkg: Some(pkg_client),
            offers: Some(vec![fdt::Offer {
                protocol_name: ft::WaiterMarker::PROTOCOL_NAME.to_string(),
                collection: fdt::Collection::PackageDrivers,
            }]),
            ..Default::default()
        }),
        offers_client: Some(offers_client),
        ..Default::default()
    };

    let _test_ns = create_realm(realm_options).await?;
    info!("connected to the test realm!");

    let (sender, mut receiver) = mpsc::channel(1);
    let offers_server = run_offers_server(offers_server, sender).fuse();
    futures::pin_mut!(offers_server);

    // We expect 4 acks from the drivers.
    for _ in 0..4 {
        let receiver_next = receiver.next().fuse();
        futures::pin_mut!(receiver_next);

        futures::select! {
            _ = receiver_next => {}
            _ = offers_server => { panic!("should not quit offers_server."); }
        }
    }

    Ok(())
}
