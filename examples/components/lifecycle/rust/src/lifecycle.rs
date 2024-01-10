// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::{handle::AsyncChannel, prelude::*},
    fuchsia_runtime::{HandleInfo, HandleType},
    futures_util::stream::TryStreamExt,
    std::process,
    tracing::{error, info},
};

// [START imports]
use fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream};
// [END imports]

// [START lifecycle_handler]
#[fuchsia::main(logging_tags = ["lifecycle", "example"])]
async fn main() {
    // Take the lifecycle handle provided by the runner
    match fuchsia_runtime::take_startup_handle(HandleInfo::new(HandleType::Lifecycle, 0)) {
        Some(lifecycle_handle) => {
            info!("Lifecycle channel received.");
            // Begin listening for lifecycle requests on this channel
            let x: fuchsia_zircon::Channel = lifecycle_handle.into();
            let async_x = AsyncChannel::from(fuchsia_async::Channel::from_channel(x));
            let mut req_stream = LifecycleRequestStream::from_channel(async_x);
            info!("Awaiting request to close...");
            if let Some(request) =
                req_stream.try_next().await.expect("Failure receiving lifecycle FIDL message")
            {
                match request {
                    LifecycleRequest::Stop { control_handle: c } => {
                        info!("Received request to stop. Shutting down.");
                        c.shutdown();
                        process::exit(0);
                    }
                }
            }

            // We only arrive here if the lifecycle channel closed without
            // first sending the shutdown event, which is unexpected.
            process::abort();
        }
        None => {
            // We did not receive a lifecycle channel, exit abnormally.
            error!("No lifecycle channel received, exiting.");
            process::abort();
        }
    }
}
// [END lifecycle_handler]
