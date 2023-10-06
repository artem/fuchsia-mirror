// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
    diagnostics_log::PublishOptions,
    fidl::{endpoints::ClientEnd, prelude::*},
    fidl_fuchsia_developer_remotecontrol as rcs,
    fidl_fuchsia_overnet::{ServiceProviderRequest, ServiceProviderRequestStream},
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    futures::channel::mpsc::unbounded,
    futures::join,
    futures::prelude::*,
    remote_control::RemoteControlService,
    std::rc::Rc,
    std::sync::Arc,
    tracing::{debug, error, info},
};

mod args;
mod usb;

async fn exec_server() -> Result<(), Error> {
    diagnostics_log::initialize(PublishOptions::default().tags(&["remote-control"]))?;

    let router = overnet_core::Router::new(None)?;

    let connector = {
        let router = Arc::clone(&router);
        move |socket| {
            let router = Arc::clone(&router);
            fasync::Task::spawn(async move {
                match fidl::AsyncSocket::from_socket(socket) {
                    Ok(socket) => {
                        let (mut rx, mut tx) = socket.split();
                        let (errors_sender, errors) = unbounded();
                        if let Err(e) = futures::future::join(
                            circuit::multi_stream::multi_stream_node_connection_to_async(
                                router.circuit_node(),
                                &mut rx,
                                &mut tx,
                                true,
                                circuit::Quality::NETWORK,
                                errors_sender,
                                "client".to_owned(),
                            ),
                            errors
                                .map(|e| {
                                    tracing::warn!("A client circuit stream failed: {e:?}");
                                })
                                .collect::<()>(),
                        )
                        .map(|(result, ())| result)
                        .await
                        {
                            if let circuit::Error::ConnectionClosed(msg) = e {
                                debug!("Overnet link closed: {:?}", msg);
                            } else {
                                error!("Error handling Overnet link: {:?}", e);
                            }
                        }
                    }
                    Err(e) => error!("Could not handle incoming link socket: {:?}", e),
                }
            })
            .detach();
        }
    };

    let service = Rc::new(RemoteControlService::new(connector).await);

    let onet_fut = {
        let (s, p) = fidl::Channel::create();
        let chan = fidl::AsyncChannel::from_channel(s)
            .context("creating ServiceProvider async channel")?;
        let stream = ServiceProviderRequestStream::from_channel(chan);
        router
            .register_service_legacy(
                rcs::RemoteControlMarker::PROTOCOL_NAME.to_owned(),
                ClientEnd::new(p),
            )
            .await?;
        let sc = service.clone();
        async move {
            let fut = stream.for_each_concurrent(None, move |svc| {
                let ServiceProviderRequest::ConnectToService {
                    chan,
                    info: _,
                    control_handle: _control_handle,
                } = svc.unwrap();
                let chan = fidl::AsyncChannel::from_channel(chan)
                    .context("failed to make async channel")
                    .unwrap();

                sc.clone().serve_stream(rcs::RemoteControlRequestStream::from_channel(chan))
            });
            info!("published remote control service to overnet");
            let res = fut.await;
            info!("connection to overnet lost: {:?}", res);
        }
    };

    let weak_router = Arc::downgrade(&router);
    std::mem::drop(router);
    let usb_fut = async move {
        // TODO: Change this info! to error! Once we can return normally if USB support is disabled
        // (fxbug.dev/95554)
        if let Err(e) = usb::run_usb_links(weak_router.clone()).await {
            info!("USB scanner failed with error {e:?}");
        }
    };

    let sc1 = service.clone();
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(move |req| {
        fasync::Task::local(sc1.clone().serve_stream(req)).detach();
    });

    fs.take_and_serve_directory_handle()?;
    let fidl_fut = fs.collect::<()>();

    join!(fidl_fut, onet_fut, usb_fut);
    Ok(())
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let args::RemoteControl { cmd } = argh::from_env();

    let res = match cmd {
        args::Command::RemoteControl(_) => exec_server().await,
    };

    if let Err(err) = res {
        error!(%err, "Error running command");
        std::process::exit(1);
    }
    Ok(())
}
