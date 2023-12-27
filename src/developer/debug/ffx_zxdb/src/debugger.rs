// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use errors::ffx_error;
use fidl_fuchsia_debugger as fdebugger;
use fuchsia_async::{unblock, Task};
use signal_hook::{consts::signal::SIGUSR1, iterator::Signals};
use std::{path::PathBuf, process::Command};

use crate::{
    debug_agent::{DebugAgentSocket, DebuggerProxy},
    spawn_forward_task, CommandBuilder,
};

pub struct Debugger {
    socket: DebugAgentSocket,
    path: PathBuf,
    pub command: CommandBuilder,
}

impl Debugger {
    pub async fn launch(debugger_proxy: fdebugger::LauncherProxy) -> Result<Self> {
        let socket = DebugAgentSocket::create(DebuggerProxy::LauncherProxy(debugger_proxy))?;
        Debugger::from_socket(socket).await
    }

    /// Create a debugger from an existing socket connection to a debug agent.
    pub async fn from_socket(socket: DebugAgentSocket) -> Result<Self> {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await?;
        if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk).await {
            eprintln!("ensure_symbol_index_registered failed, error was: {:#?}", e);
        }

        let path = sdk.get_host_tool("zxdb")?;

        let mut command = CommandBuilder::new(path.clone());
        command.connect(&socket);

        Ok(Self { socket, path, command })
    }

    /// The path of the zxdb binary on the local system.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Run the debugger with the given `command` instead of `self.command`.
    pub async fn run_with_command(self, mut command: Command) -> Result<()> {
        let session = DebuggerSession {
            child: command.spawn()?,
            _forward_task: spawn_forward_task(self.socket),
        };
        session.wait().await
    }

    /// Spawn an interactive session with the debugger.
    fn spawn(self) -> Result<DebuggerSession> {
        Ok(DebuggerSession {
            child: self.command.build().spawn()?,
            _forward_task: spawn_forward_task(self.socket),
        })
    }

    /// Start an interactive session with the debugger.
    ///
    /// The future returned by this function resolves when the debugger is ready for interactive
    /// user input.
    ///
    /// To wait for the interactive session with the user to complete, use the `wait` function on
    /// the returned `DebuggerSession`.
    pub async fn start(mut self) -> Result<DebuggerSession> {
        let mut signals = Signals::new(&[SIGUSR1]).unwrap();

        let pid = std::process::id();
        self.command.push_str(&format!("--signal-when-ready={pid}"));

        let session = self.spawn()?;

        unblock(move || {
            let signal = signals.forever().next();
            assert_eq!(signal, Some(SIGUSR1));
        })
        .await;

        Ok(session)
    }

    /// Run the debugger.
    ///
    /// The future returned by this function resolves when the user's interactive session with the
    /// debugger is complete.
    pub async fn run(self) -> Result<()> {
        self.spawn()?.wait().await
    }
}

pub struct DebuggerSession {
    child: std::process::Child,
    _forward_task: Task<()>,
}

impl DebuggerSession {
    pub async fn wait(mut self) -> Result<()> {
        if let Some(exit_code) = unblock(move || self.child.wait()).await?.code() {
            if exit_code == 0 {
                Ok(())
            } else {
                Err(ffx_error!("zxdb exited with code {}", exit_code).into())
            }
        } else {
            Err(ffx_error!("zxdb terminated by signal").into())
        }
    }
}
