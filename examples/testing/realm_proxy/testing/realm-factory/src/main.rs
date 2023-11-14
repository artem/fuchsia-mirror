// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::{self, DiscoverableProtocolMarker},
    fidl_fidl_examples_routing_echo as fecho, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_test_echoserver::{RealmFactoryRequest, RealmFactoryRequestStream, RealmOptions},
    fuchsia_async as fasync,
    fuchsia_component::{client, server::ServiceFs},
    fuchsia_component_test::{RealmBuilder, RealmInstance},
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
                RealmFactoryRequest::CreateRealm { options, dict_server, responder } => {
                    let realm = create_realm(options).await?;

                    // Get a dict containing the capabilities exposed by the realm.
                    let (expose_dict, server_end) = endpoints::create_proxy().unwrap();
                    realm.root.controller().get_exposed_dict(server_end)?;
                    let mut output_dict_entries = expose_dict.read().await?.unwrap();

                    // Mix in additional capabilities to the dict.
                    //
                    // TODO(fxbug.dev/298100106): Could RealmInstance expose a higher-level
                    // API for getting the realm's exposed dict, along with APIs that make it
                    // easy to add in more capabilities served by this component? For example:
                    //
                    // let mut bundle = realm.get_root_bundle().extend().await?.unwrap();
                    // let echo_request_stream =
                    //     bundles::add_fidl_service::<fecho::EchoMarker>(&bundle);
                    // "serves" the bundle over `bundle_server` which was sent from the client.
                    // Also moves `bundle` so it can't be modified further.
                    // bundle.serve(bundle_server).await?.unwrap();
                    //
                    // ... code to serve echo here ...

                    let (echo_sender_client, echo_sender_server) =
                        endpoints::create_endpoints::<fsandbox::SenderMarker>();
                    let (echo_receiver_client, echo_receiver_stream) =
                        endpoints::create_request_stream::<fsandbox::ReceiverMarker>()?;
                    let factory = client::connect_to_protocol::<fsandbox::FactoryMarker>()?;
                    let () = factory.create_connector(echo_sender_server, echo_receiver_client)?;
                    output_dict_entries.push(fsandbox::DictItem {
                        key: format!("{}", fecho::EchoMarker::PROTOCOL_NAME),
                        value: echo_sender_client.into(),
                    });

                    // Create the dict containing the capabilities to pass to the test.
                    let () = factory.create_dict(output_dict_entries, dict_server).await?.unwrap();

                    // Serve the mixed-in capability.
                    task_group.spawn(async move {
                        let _ = realm_proxy::service::handle_receiver::<fecho::EchoMarker, _, _>(
                            echo_receiver_stream,
                            handle_echo_request_stream,
                        )
                        .await
                        .map_err(|e| {
                            error!("Failed to serve echo stream: {}", e);
                        });
                    });

                    responder.send(Ok(()))?;
                }
                RealmFactoryRequest::_UnknownMethod { .. } => unimplemented!(),
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

    // FIXME: Copy realm builder code here.

    let realm = builder.build().await?;
    Ok(realm)
}

async fn handle_echo_request_stream(mut stream: fecho::EchoRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fecho::EchoRequest::EchoString { value, responder } => {
                responder.send(value.as_deref()).unwrap();
            }
        }
    }
}
