// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// core
mod cpu_manager;
mod error;
mod message;
mod node;

#[path = "../../common/lib/common_utils.rs"]
mod common_utils;
#[path = "../../common/lib/types.rs"]
mod types;

// nodes
mod cpu_control_handler;
mod cpu_device_handler;
mod cpu_manager_main;
mod cpu_stats_handler;
mod dev_control_handler;
mod syscall_handler;
mod thermal_watcher;

#[cfg(test)]
mod test;

use crate::cpu_manager::CpuManager;
use anyhow::Error;
use fuchsia_trace_provider;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    tracing::info!("started");

    // Setup tracing
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    // Set up the CpuManager
    let mut cm = CpuManager::new();

    // This future should never complete
    let result = cm.run().await;
    tracing::error!("Unexpected exit with result: {:?}", result);
    result
}
