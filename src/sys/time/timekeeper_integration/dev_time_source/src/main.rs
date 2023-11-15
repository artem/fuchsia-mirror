// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_time_external::PushSourceMarker;
use fidl_test_time::TimeSourceControlMarker;
use fuchsia_component::{client::connect_to_protocol, server::ServiceFs};
use fuchsia_zircon as zx;
use futures::StreamExt;
use tracing::info;

#[fuchsia::main(logging_tags=["time", "dev_time_source"])]
async fn main() {
    let time_source_control = connect_to_protocol::<TimeSourceControlMarker>()
        .expect("failed to connect to control service");

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_service_at(PushSourceMarker::PROTOCOL_NAME, |chan| Some(chan));

    fs.take_and_serve_directory_handle().expect("Failed to serve directory");
    fs.for_each_concurrent(None, |chan: zx::Channel| async {
        info!("Forwarding a PushSource channel");
        time_source_control
            .connect_push_source(ServerEnd::new(chan))
            .expect("Failed to forward a PushSource channel");
    })
    .await;
}
