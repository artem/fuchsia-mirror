// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::reverser::ReverserServerFactory,
    anyhow::{Context, Error},
    fidl_fuchsia_examples_inspect::FizzBuzzMarker,
    fuchsia_component::{client, server::ServiceFs},
    futures::{future::try_join, FutureExt, StreamExt},
    tracing::info,
};

// [START part_1_use_inspect]
use fuchsia_inspect::component;
// [END part_1_use_inspect]

mod reverser;

#[fuchsia::main(logging_tags = ["inspect_rust_codelab", "part2"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();

    info!("starting up...");

    // [START part_1_serve_inspect]
    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    // [END part_1_serve_inspect]

    // Create a version string. We use record_ rather than create_ to tie the lifecyle of the
    // inspector root with the string property.
    // It is an error to not retain the created property.
    // [START part_1_write_version]
    component::inspector().root().record_string("version", "part2");
    // [END part_1_write_version]

    // Create a new Reverser Server factory. The factory holds global stats for the reverser
    // server.
    // [START part_1_new_child]
    let reverser_factory =
        ReverserServerFactory::new(component::inspector().root().create_child("reverser_service"));
    // [END part_1_new_child]

    // Serve the reverser service
    fs.dir("svc").add_fidl_service(move |stream| reverser_factory.spawn_new(stream));
    fs.take_and_serve_directory_handle()?;

    // Send a request to the FizzBuzz service and print the response when it arrives.
    // CODELAB: Instrument our connection to FizzBuzz using Inspect. Is there an error?
    // [START instrument_fizzbuzz]
    let fizzbuzz_fut = async move {
        let fizzbuzz = client::connect_to_protocol::<FizzBuzzMarker>()
            .context("failed to connect to fizzbuzz")?;
        match fizzbuzz.execute(30u32).await {
            Ok(result) => {
                // CODELAB: Add Inspect here to see if there is a response.
                info!(%result, "Got FizzBuzz");
            }
            Err(_) => {
                // CODELAB: Add Inspect here to see if there is an error
            }
        };
        Ok(())
    };
    // [END instrument_fizzbuzz]

    let running_service_fs = fs.collect::<()>().map(Ok);
    try_join(running_service_fs, fizzbuzz_fut).await.map(|((), ())| ())
}
