// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod vmm_launcher;

use {
    anyhow::Context,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component::RealmMarker,
    fidl_fuchsia_virtualization::GuestLifecycleMarker,
    fuchsia_component::{client, server},
    vmm_launcher_config::Config,
};

#[fuchsia::main(logging = true, threads = 1)]
async fn main() -> Result<(), anyhow::Error> {
    let mut fs = server::ServiceFs::new();
    fs.dir("svc").add_service_connector(|server_end: ServerEnd<GuestLifecycleMarker>| server_end);
    fs.take_and_serve_directory_handle().context("Error starting server")?;

    let config = Config::take_from_startup_handle();

    let realm_proxy = client::connect_to_protocol::<RealmMarker>()
        .context("Error connecting to Realm protocol")?;
    let mut launcher =
        crate::vmm_launcher::VmmLauncher::new(config.vmm_component_url.to_string(), realm_proxy);
    launcher.run(fs).await;
    Ok(())
}
