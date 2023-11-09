// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::runner::RemoteRunner;
use fidl::endpoints;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_data as fdata;
use fidl_fuchsia_diagnostics_types as fdiagnostics;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_mem as fmem;
use fidl_fuchsia_process as fprocess;
use fuchsia_async as fasync;
use fuchsia_async::Task;
use fuchsia_zircon as zx;
use futures::{channel::oneshot, future::BoxFuture};
use lazy_static::lazy_static;
use moniker::Moniker;
use serve_processargs::{BuildNamespaceError, NamespaceBuilder};
use std::{collections::HashMap, sync::Mutex};
use thiserror::Error;
use zx::{AsHandleRef, Koid};

mod component_controller;
use component_controller::ComponentController;

/// A [Program] is a unit of execution.
///
/// After a program starts running, it may optionally publish diagnostics,
/// and may eventually terminate.
pub struct Program {
    controller: ComponentController,

    // The KOID of the controller server endpoint.
    koid: Koid,

    // Only here to keep the diagnostics task alive. Never read.
    _send_diagnostics: Task<()>,

    // Dropping the task will stop serving the namespace.
    namespace_task: Mutex<Option<fasync::Task<()>>>,
}

impl Program {
    /// Starts running a program using the `runner`.
    ///
    /// After successfully starting the program, it will serve the namespace given to the
    /// program. If [Program] is dropped or [Program::kill] is called, the namespace will no
    /// longer be served.
    ///
    /// TODO(fxbug.dev/122024): Change `start_info` to a `Dict` and `DeliveryMap` as runners
    /// migrate to use sandboxes.
    ///
    /// TODO(fxbug.dev/122024): This API allows users to create orphaned programs that's not
    /// associated with anything else. Once we have a bedrock component concept, we might
    /// want to require a containing component to start a program.
    ///
    /// TODO(fxbug.dev/122024): Since diagnostic information is only available once,
    /// the framework should be the one that get it. That's another reason to limit this API.
    pub fn start(
        moniker: Moniker,
        runner: &RemoteRunner,
        start_info: StartInfo,
        diagnostics_sender: oneshot::Sender<fdiagnostics::ComponentDiagnostics>,
    ) -> Result<Program, StartError> {
        let (controller, server_end) =
            endpoints::create_proxy::<fcrunner::ComponentControllerMarker>()
                .expect("creating FIDL proxy should not fail");
        let koid = server_end
            .as_handle_ref()
            .basic_info()
            .expect("basic info should not require any rights")
            .koid;
        MONIKER_LOOKUP.add(koid, moniker);

        let (start_info, fut) = start_info.into_fidl()?;

        runner.start(start_info, server_end);
        let mut controller = ComponentController::new(controller);
        let diagnostics_receiver = controller.take_diagnostics_receiver().unwrap();
        let send_diagnostics = Task::spawn(async move {
            let diagnostics = diagnostics_receiver.await;
            if let Ok(diagnostics) = diagnostics {
                _ = diagnostics_sender.send(diagnostics);
            }
        });
        Ok(Program {
            controller,
            koid,
            _send_diagnostics: send_diagnostics,
            namespace_task: Mutex::new(Some(fasync::Task::spawn(fut))),
        })
    }

    /// Request to stop the program.
    pub fn stop(&self) -> Result<(), StopError> {
        self.controller.stop().map_err(StopError::Internal)?;
        Ok(())
    }

    /// Request to stop this program immediately.
    pub fn kill(&self) -> Result<(), StopError> {
        self.controller.kill().map_err(StopError::Internal)?;
        _ = self.namespace_task.lock().unwrap().take();
        Ok(())
    }

    /// Wait for the program to terminate, with an epitaph specified in the
    /// `fuchsia.component.runner/ComponentController` FIDL protocol documentation.
    pub fn on_terminate(&self) -> BoxFuture<'static, zx::Status> {
        self.controller.wait_for_epitaph()
    }

    /// Gets a [`Koid`] that will uniquely identify this program.
    #[cfg(test)]
    pub fn koid(&self) -> Koid {
        self.koid
    }

    /// Creates a program that does nothing but let us intercept requests to control its lifecycle.
    #[cfg(test)]
    pub fn mock_from_controller(
        controller: endpoints::ClientEnd<fcrunner::ComponentControllerMarker>,
    ) -> Program {
        let koid = controller
            .as_handle_ref()
            .basic_info()
            .expect("basic info should not require any rights")
            .related_koid;

        let controller = ComponentController::new(controller.into_proxy().unwrap());
        let send_diagnostics = Task::spawn(async {});
        Program {
            controller,
            koid,
            _send_diagnostics: send_diagnostics,
            namespace_task: Mutex::new(None),
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        MONIKER_LOOKUP.remove(self.koid);
    }
}

struct MonikerLookup {
    koid_to_moniker: Mutex<HashMap<Koid, Moniker>>,
}

impl MonikerLookup {
    fn new() -> Self {
        Self { koid_to_moniker: Mutex::new(HashMap::new()) }
    }

    fn add(&self, koid: Koid, moniker: Moniker) {
        self.koid_to_moniker.lock().unwrap().insert(koid, moniker);
    }

    fn get(&self, koid: Koid) -> Option<Moniker> {
        self.koid_to_moniker.lock().unwrap().get(&koid).cloned()
    }

    fn remove(&self, koid: Koid) {
        self.koid_to_moniker.lock().unwrap().remove(&koid);
    }
}

lazy_static! {
    static ref MONIKER_LOOKUP: MonikerLookup = MonikerLookup::new();
}

/// Looks up the moniker of a component based on the KOID of the `ComponentController`
/// server endpoint given to its runner.
///
/// If this method returns `None`, then the `ComponentController` server endpoint is
/// not minted by component_manager as part of starting a component.
pub fn moniker_from_controller_koid(koid: Koid) -> Option<Moniker> {
    MONIKER_LOOKUP.get(koid)
}

#[derive(Error, Debug, Clone)]
pub enum StopError {
    /// Internal errors are not meant to be meaningfully handled by the user.
    #[error("internal error: {0}")]
    Internal(fidl::Error),
}

#[derive(Error, Debug, Clone)]
pub enum StartError {
    #[error("failed to serve namespace: {0}")]
    ServeNamespace(BuildNamespaceError),
}

/// Information and capabilities used to start a program.
pub struct StartInfo {
    /// The resolved URL of the component.
    ///
    /// This is the canonical URL obtained by the component resolver after
    /// following redirects and resolving relative paths.
    pub resolved_url: String,

    /// The component's program declaration.
    /// This information originates from `ComponentDecl.program`.
    pub program: fdata::Dictionary,

    /// The namespace to provide to the component instance.
    ///
    /// A namespace specifies the set of directories that a component instance
    /// receives at start-up. Through the namespace directories, a component
    /// may access capabilities available to it. The contents of the namespace
    /// are mainly determined by the component's `use` declarations but may
    /// also contain additional capabilities automatically provided by the
    /// framework.
    ///
    /// By convention, a component's namespace typically contains some or all
    /// of the following directories:
    ///
    /// - "/svc": A directory containing services that the component requested
    ///           to use via its "import" declarations.
    /// - "/pkg": A directory containing the component's package, including its
    ///           binaries, libraries, and other assets.
    ///
    /// The mount points specified in each entry must be unique and
    /// non-overlapping. For example, [{"/foo", ..}, {"/foo/bar", ..}] is
    /// invalid.
    ///
    /// TODO(b/298106231): eventually this should become a sandbox and delivery map.
    pub namespace: NamespaceBuilder,

    /// The directory this component serves.
    pub outgoing_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The directory served by the runner to present runtime information about
    /// the component. The runner must either serve it, or drop it to avoid
    /// blocking any consumers indefinitely.
    pub runtime_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The numbered handles that were passed to the component.
    ///
    /// If the component does not support numbered handles, the runner is expected
    /// to close the handles.
    pub numbered_handles: Vec<fprocess::HandleInfo>,

    /// Binary representation of the component's configuration.
    ///
    /// # Layout
    ///
    /// The first 2 bytes of the data should be interpreted as an unsigned 16-bit
    /// little-endian integer which denotes the number of bytes following it that
    /// contain the configuration checksum. After the checksum, all the remaining
    /// bytes are a persistent FIDL message of a top-level struct. The struct's
    /// fields match the configuration fields of the component's compiled manifest
    /// in the same order.
    pub encoded_config: Option<fmem::Data>,

    /// An eventpair that debuggers can use to defer the launch of the component.
    ///
    /// For example, ELF runners hold off from creating processes in the component
    /// until ZX_EVENTPAIR_PEER_CLOSED is signaled on this eventpair. They also
    /// ensure that runtime_dir is served before waiting on this eventpair.
    /// ELF debuggers can query the runtime_dir to decide whether to attach before
    /// they drop the other side of the eventpair, which is sent in the payload of
    /// the DebugStarted event in fuchsia.component.events.
    pub break_on_start: Option<zx::EventPair>,
}

impl StartInfo {
    pub fn into_fidl(
        self,
    ) -> Result<(fcrunner::ComponentStartInfo, BoxFuture<'static, ()>), StartError> {
        let (ns, fut) = self.namespace.serve().map_err(StartError::ServeNamespace)?;
        Ok((
            fcrunner::ComponentStartInfo {
                resolved_url: Some(self.resolved_url),
                program: Some(self.program),
                ns: Some(ns.into()),
                outgoing_dir: self.outgoing_dir,
                runtime_dir: self.runtime_dir,
                numbered_handles: Some(self.numbered_handles),
                encoded_config: self.encoded_config,
                break_on_start: self.break_on_start,
                ..Default::default()
            },
            fut,
        ))
    }
}
