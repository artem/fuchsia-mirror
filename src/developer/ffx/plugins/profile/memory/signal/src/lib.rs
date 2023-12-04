// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the `signal` subcommand.

use {
    anyhow::Result,
    async_trait::async_trait,
    errors::ffx_error,
    ffx_profile_memory_signal_args::SignalCommand,
    fho::{moniker, FfxMain, FfxTool, SimpleWriter},
    fidl_fuchsia_memory_debug::MemoryPressureProxy,
};

#[derive(FfxTool)]
pub struct MemorySignalTool {
    #[command]
    cmd: SignalCommand,
    #[with(moniker("/core/memory_monitor"))]
    debugger_proxy: MemoryPressureProxy,
}

fho::embedded_plugin!(MemorySignalTool);

#[async_trait(?Send)]
impl FfxMain for MemorySignalTool {
    type Writer = SimpleWriter;

    /// Forwards the specified memory pressure level to the fuchsia.memory.debug.MemoryPressure FIDL
    /// interface.
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        self.debugger_proxy
            .signal(self.cmd.level)
            .map_err(|err| ffx_error!("Failed to call MemoryPressure/Signal: {err}"))?;
        Ok(())
    }
}
