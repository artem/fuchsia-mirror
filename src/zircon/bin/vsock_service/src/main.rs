// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

use {
    anyhow::{Context as _, Error},
    fidl_fuchsia_hardware_vsock::DeviceMarker,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_fs::{directory::Watcher, OpenFlags},
    fuchsia_zircon as zx,
    futures::{StreamExt, TryFutureExt},
};

use vsock_service_lib as service;

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    tracing::info!("Starting vsock service");

    let vsock_device_path = std::path::Path::new("/dev/class/vsock");
    let (client, device) = zx::Channel::create();
    let vsock_dir = fuchsia_fs::directory::open_in_namespace(
        vsock_device_path.to_str().unwrap(),
        OpenFlags::RIGHT_READABLE,
    )
    .context("Open vsock dir")?;
    let mut watcher = Watcher::new(&vsock_dir).await.context("Create watcher")?;

    let event = loop {
        let event = watcher.next().await.unwrap().context("Get watch event")?;
        if event.filename.as_path().to_str().unwrap() == "."
            || event.filename.as_path().to_str().unwrap() == ".."
        {
            continue;
        }
        break event;
    };

    let device_path = vsock_device_path.join(&event.filename);
    fdio::service_connect(device_path.as_path().to_str().unwrap(), device)
        .context("open service")?;
    let dev = fidl::endpoints::ClientEnd::<DeviceMarker>::new(client)
        .into_proxy()
        .context("Failed to make channel")?;

    let (service, event_loop) =
        service::Vsock::new(dev).await.context("Failed to initialize vsock service")?;

    let service_clone = service.clone();
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream| {
        fasync::Task::spawn(
            service_clone
                .clone()
                .run_client_connection(stream)
                .unwrap_or_else(|err| tracing::info!("Error {} during client connection", err)),
        )
        .detach();
    });
    fs.take_and_serve_directory_handle()?;

    // Spawn the services server with a wrapper to discard the return value.
    fasync::Task::spawn(fs.collect()).detach();

    // Run the event loop until completion. The event loop only terminates
    // with an error.
    event_loop.await?;
    Ok(())
}
