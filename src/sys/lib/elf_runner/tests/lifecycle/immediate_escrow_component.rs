// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_process_lifecycle::{LifecycleMarker, LifecycleOnEscrowRequest},
    fuchsia_async as fasync,
    fuchsia_runtime::{self as fruntime, HandleInfo, HandleType},
    fuchsia_zircon::{self as zx},
    std::process,
    tracing::{error, info},
};

/// This component immediately escrows its outgoing directory and then exits.
#[fuchsia::main]
fn main() {
    let _executor = fasync::LocalExecutor::new();

    let Some(lifecycle_handle) =
        fruntime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0))
    else {
        error!("No lifecycle channel received, exiting.");
        process::abort();
    };

    let Some(outgoing_directory) =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
    else {
        error!("No outgoing directory server endpoint received, exiting.");
        process::abort();
    };

    info!("Lifecycle channel received.");
    let channel: zx::Channel = lifecycle_handle.into();
    let lifecycle: ServerEnd<LifecycleMarker> = channel.into();
    let (_stream, control) = lifecycle.into_stream_and_control_handle().unwrap();
    control
        .send_on_escrow(LifecycleOnEscrowRequest {
            outgoing_dir: Some(outgoing_directory.into()),
            ..Default::default()
        })
        .unwrap();
}
