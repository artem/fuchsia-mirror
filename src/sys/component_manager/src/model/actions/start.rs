// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::bedrock::program::{Program, StartInfo},
    crate::framework::controller,
    crate::model::{
        actions::{Action, ActionKey},
        component::instance::{InstanceState, StartedInstanceState},
        component::{ComponentInstance, IncomingCapabilities, StartReason},
        namespace::create_namespace,
        routing::{open_capability, RouteRequest},
    },
    crate::runner::RemoteRunner,
    ::namespace::Entry as NamespaceEntry,
    ::routing::component_instance::ComponentInstanceInterface,
    async_trait::async_trait,
    cm_logger::scoped::ScopedLogger,
    cm_rust::ComponentDecl,
    cm_util::{AbortError, AbortFutureExt, AbortHandle, AbortableScope},
    config_encoder::ConfigFields,
    errors::{ActionError, CreateNamespaceError, StartActionError, StructuredConfigError},
    fidl::{endpoints::DiscoverableProtocolMarker, Vmo},
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata,
    fidl_fuchsia_logger as flogger, fidl_fuchsia_mem as fmem, fidl_fuchsia_process as fprocess,
    fuchsia_zircon as zx,
    futures::channel::oneshot,
    hooks::{EventPayload, RuntimeInfo},
    moniker::Moniker,
    sandbox::{Capability, Dict},
    serve_processargs::NamespaceBuilder,
    std::sync::Arc,
    tracing::warn,
    vfs::execution_scope::ExecutionScope,
};

/// Starts a component instance.
pub struct StartAction {
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    incoming: IncomingCapabilities,
    abort_handle: AbortHandle,
    namespace_scope: ExecutionScope,
    abortable_scope: AbortableScope,
}

impl StartAction {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        incoming: IncomingCapabilities,
    ) -> Self {
        Self::new_inner(start_reason, execution_controller_task, incoming, ExecutionScope::new())
    }

    fn new_inner(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        incoming: IncomingCapabilities,
        namespace_scope: ExecutionScope,
    ) -> Self {
        let (abortable_scope, abort_handle) = AbortableScope::new();
        Self {
            start_reason,
            execution_controller_task,
            incoming,
            namespace_scope,
            abort_handle,
            abortable_scope,
        }
    }

    #[cfg(test)]
    pub fn new_with_scope(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        incoming: IncomingCapabilities,
        namespace_scope: ExecutionScope,
    ) -> Self {
        Self::new_inner(start_reason, execution_controller_task, incoming, namespace_scope)
    }
}

#[async_trait]
impl Action for StartAction {
    async fn handle(self, component: Arc<ComponentInstance>) -> Result<(), ActionError> {
        do_start(
            &component,
            &self.start_reason,
            self.execution_controller_task,
            self.incoming,
            self.namespace_scope,
            self.abortable_scope,
        )
        .await
        .map_err(Into::into)
    }

    fn key(&self) -> ActionKey {
        ActionKey::Start
    }

    fn abort_handle(&self) -> Option<AbortHandle> {
        Some(self.abort_handle.clone())
    }
}

struct StartContext {
    runner: Option<RemoteRunner>,
    url: String,
    namespace_builder: NamespaceBuilder,
    namespace_scope: ExecutionScope,
    numbered_handles: Vec<fprocess::HandleInfo>,
    encoded_config: Option<fmem::Data>,
    program_input_dict_additions: Dict,
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    logger: Option<ScopedLogger>,
}

async fn do_start(
    component: &Arc<ComponentInstance>,
    start_reason: &StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    incoming: IncomingCapabilities,
    namespace_scope: ExecutionScope,
    abortable_scope: AbortableScope,
) -> Result<(), StartActionError> {
    // Translates the error when a long running future is aborted.
    let abort_error =
        |_: AbortError| StartActionError::Aborted { moniker: component.moniker.clone() };

    // Resolve the component and find the runner to use.
    let (runner, resolved_component, program_input_dict) = {
        // Obtain the runner declaration under a short lock, as `open_runner` may lock the
        // resolved state re-entrantly.
        let resolved_state = component
            .lock_resolved_state()
            .with(&abortable_scope)
            .await
            .map_err(abort_error)?
            .map_err(|err| StartActionError::ResolveActionError {
                moniker: component.moniker.clone(),
                err: Box::new(err),
            })?;
        (
            resolved_state.decl().get_runner(),
            resolved_state.resolved_component.clone(),
            resolved_state.sandbox.program_input_dict.clone(),
        )
    };
    let runner = match runner {
        Some(runner) => open_runner(component, runner)
            .with(&abortable_scope)
            .await
            .map_err(abort_error)?
            .map_err(|err| {
                warn!(moniker = %component.moniker, %err, "Failed to resolve runner.");
                err
            })?,
        None => None,
    };

    let IncomingCapabilities {
        numbered_handles,
        additional_namespace_entries,
        dict: program_input_dict_additions,
    } = incoming;

    // Create the component's namespace.
    let mut namespace_builder = create_namespace(
        resolved_component.package.as_ref(),
        component,
        &resolved_component.decl,
        &program_input_dict,
        namespace_scope.clone(),
    )
    .await
    .map_err(|err| StartActionError::CreateNamespaceError {
        moniker: component.moniker.clone(),
        err,
    })?;
    for NamespaceEntry { directory, path } in additional_namespace_entries {
        let directory: sandbox::Directory = directory.into();
        namespace_builder.add_entry(Capability::Directory(directory), &path).map_err(|err| {
            StartActionError::CreateNamespaceError {
                moniker: component.moniker.clone(),
                err: CreateNamespaceError::BuildNamespaceError(err),
            }
        })?;
    }

    // Check policy.
    // TODO(https://fxbug.dev/42071809): Consider moving this check to ComponentInstance::add_child
    match component.on_terminate {
        fdecl::OnTerminate::Reboot => {
            component.policy_checker().reboot_on_terminate_allowed(&component.moniker).map_err(
                |err| StartActionError::RebootOnTerminateForbidden {
                    moniker: component.moniker.clone(),
                    err,
                },
            )?;
        }
        fdecl::OnTerminate::None => {}
    }

    // Create a component-scoped logger if the component uses LogSink.
    let decl = &resolved_component.decl;
    let logger = if let Some(logsink_decl) = get_logsink_decl(&decl) {
        match create_scoped_logger(component, logsink_decl.clone()).await {
            Ok(logger) => Some(logger),
            Err(err) => {
                warn!(moniker = %component.moniker, %err, "Could not create logger for component. Logs will be attributed to component_manager");
                None
            }
        }
    } else {
        None
    };

    let encoded_config = match decl.config {
        None => None,
        Some(ref config_decl) => match config_decl.value_source {
            cm_rust::ConfigValueSource::PackagePath(_) => {
                let Some(mut config) = resolved_component.config.clone() else {
                    return Err(StartActionError::StructuredConfigError {
                        moniker: component.moniker.clone(),
                        err: StructuredConfigError::ConfigValuesMissing,
                    });
                };
                if has_config_capabilities(decl) {
                    update_config_with_capabilities(&mut config, decl, &component)
                        .with(&abortable_scope)
                        .await
                        .map_err(abort_error)??;
                    update_component_config(&component, config.clone()).await?;
                }
                Some(encode_config(config, &component.moniker).await?)
            }
            cm_rust::ConfigValueSource::Capabilities(_) => {
                let config = create_config_with_capabilities(decl, &component)
                    .with(&abortable_scope)
                    .await
                    .map_err(abort_error)??;
                match config {
                    Some(c) => {
                        update_component_config(&component, c.clone()).await?;
                        Some(encode_config(c, &component.moniker).await?)
                    }
                    None => None,
                }
            }
        },
    };
    let start_context = StartContext {
        runner,
        url: resolved_component.resolved_url.clone(),
        namespace_builder,
        namespace_scope,
        numbered_handles,
        encoded_config,
        program_input_dict_additions: program_input_dict_additions.unwrap_or_default(),
        start_reason: start_reason.clone(),
        execution_controller_task,
        logger,
    };

    start_component(&component, resolved_component.decl, start_context).await
}

/// Set the Runtime in the Execution and start the exit watcher. From component manager's
/// perspective, this indicates that the component has started. If this returns an error, the
/// component was shut down and the Runtime is not set, otherwise the function returns the
/// start context with the runtime set. This function acquires the state and execution locks on
/// `Component`.
async fn start_component(
    component: &Arc<ComponentInstance>,
    decl: ComponentDecl,
    start_context: StartContext,
) -> Result<(), StartActionError> {
    let runtime_info;
    let timestamp;
    let runtime_dir;
    let break_on_start_left;
    let break_on_start_right;

    {
        let mut state = component.lock_state().await;

        if let Some(r) = should_return_early(&state, &component.moniker) {
            return r;
        }

        let (diagnostics_sender, diagnostics_receiver) = oneshot::channel();
        (break_on_start_left, break_on_start_right) = zx::EventPair::create();

        let StartContext {
            runner,
            url,
            namespace_builder,
            numbered_handles,
            namespace_scope,
            encoded_config,
            program_input_dict_additions,
            start_reason,
            execution_controller_task,
            logger,
        } = start_context;

        let program = if let Some(runner) = runner {
            let moniker = &component.moniker;
            let component_instance = state
                .instance_token(moniker, &component.context)
                .ok_or(StartActionError::InstanceDestroyed { moniker: moniker.clone() })?;
            let escrowed_state = state
                .reap_escrowed_state_during_start()
                .await
                .ok_or(StartActionError::InstanceDestroyed { moniker: moniker.clone() })?;

            let start_info = StartInfo {
                resolved_url: url,
                program: decl
                    .program
                    .as_ref()
                    .map(|p| p.info.clone())
                    .unwrap_or_else(|| fdata::Dictionary::default()),
                namespace: namespace_builder,
                numbered_handles,
                encoded_config,
                break_on_start: Some(break_on_start_left),
                component_instance,
            };

            Some(
                Program::start(
                    &runner,
                    start_info,
                    escrowed_state,
                    diagnostics_sender,
                    namespace_scope,
                )
                .map_err(|err| StartActionError::StartProgramError {
                    moniker: moniker.clone(),
                    err,
                })?,
            )
        } else {
            None
        };

        let started = StartedInstanceState::new(
            program,
            component.as_weak(),
            start_reason,
            execution_controller_task,
            logger,
        );
        timestamp = started.timestamp;
        runtime_info = RuntimeInfo::new(timestamp, diagnostics_receiver);
        runtime_dir = started.runtime_dir().cloned();
        state.replace(|instance_state| match instance_state {
            InstanceState::Resolved(resolved) => InstanceState::Started(resolved, started),
            other_state => panic!("starting an unresolved component: {:?}", other_state),
        });

        // TODO(b/322564390): Move program_input_dict_additions into `StartedInstanceState`.
        let component_program_input_dict_additions = &state
            .get_resolved_state()
            .expect("expected component to be resolved")
            .program_input_dict_additions;
        let _ = component_program_input_dict_additions.drain();
        for (key, value) in program_input_dict_additions.drain() {
            component_program_input_dict_additions.insert(key, value).unwrap();
        }
    }

    // Dispatch Started and DebugStarted events outside of the state lock, but under the
    // actions lock, so that:
    //
    // - Hooks implementations can use the state (such as re-entrantly ensuring the component is
    //   started).
    // - The Started events will be ordered before any other component lifecycle transitions.
    //
    component
        .hooks
        .dispatch(&component.new_event_with_timestamp(
            EventPayload::Started { runtime: runtime_info, component_decl: decl },
            timestamp,
        ))
        .await;
    component
        .hooks
        .dispatch(&component.new_event_with_timestamp(
            EventPayload::DebugStarted {
                runtime_dir,
                break_on_start: Arc::new(break_on_start_right),
            },
            timestamp,
        ))
        .await;

    Ok(())
}

/// Determines if `start` should return early. Returns the following values:
/// - None: `start` should not return early because the component is not currently running
/// - Some(Ok(())): `start` should return early because the component has already been started
/// - Some(Err(err)): `start` should return early because it will be unable to start the component
/// due to `err` (for example, perhaps the component is destroyed or shut down).
pub fn should_return_early(
    component: &InstanceState,
    moniker: &Moniker,
) -> Option<Result<(), StartActionError>> {
    match component {
        InstanceState::Started(_, _) => Some(Ok(())),
        InstanceState::New | InstanceState::Unresolved(_) | InstanceState::Resolved(_) => None,
        InstanceState::Shutdown(_, _) => {
            Some(Err(StartActionError::InstanceShutDown { moniker: moniker.clone() }))
        }
        InstanceState::Destroyed => {
            Some(Err(StartActionError::InstanceDestroyed { moniker: moniker.clone() }))
        }
    }
}

/// Returns a RemoteRunner routed to the component's runner, if it specifies one.
///
/// Returns None if the component's decl does not specify a runner.
async fn open_runner(
    component: &Arc<ComponentInstance>,
    runner: cm_rust::UseRunnerDecl,
) -> Result<Option<RemoteRunner>, StartActionError> {
    // Open up a channel to the runner.
    let proxy = open_capability(&RouteRequest::UseRunner(runner.clone()), component)
        .await
        .map_err(|err| StartActionError::ResolveRunnerError {
            moniker: component.moniker.clone(),
            err: Box::new(err),
            runner: runner.source_name,
        })?;
    Ok(Some(RemoteRunner::new(proxy)))
}

/// Returns true if the decl uses configuration capabilities.
fn has_config_capabilities(decl: &cm_rust::ComponentDecl) -> bool {
    decl.uses.iter().find(|u| matches!(u, cm_rust::UseDecl::Config(_))).is_some()
}

/// Update the component's configuration fields.
async fn update_component_config(
    component: &Arc<ComponentInstance>,
    config: ConfigFields,
) -> Result<(), StartActionError> {
    let mut resolved_state = component.lock_resolved_state().await.unwrap();
    resolved_state.resolved_component.config = Some(config);
    Ok(())
}

async fn create_config_with_capabilities(
    decl: &cm_rust::ComponentDecl,
    component: &Arc<ComponentInstance>,
) -> Result<Option<ConfigFields>, StartActionError> {
    let Some(ref config_decl) = decl.config else {
        return Ok(None);
    };
    let mut fields = ConfigFields { fields: Vec::new(), checksum: config_decl.checksum.clone() };
    for field in &config_decl.fields {
        let Some(use_config) = routing::config::get_use_config_from_key(&field.key, decl) else {
            return Err(StartActionError::StructuredConfigError {
                moniker: component.moniker.clone(),
                err: StructuredConfigError::KeyNotFound { key: field.key.clone() },
            });
        };
        let value = match routing::config::route_config_value(use_config, component).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Err(StartActionError::StructuredConfigError {
                    moniker: component.moniker.clone(),
                    err: StructuredConfigError::KeyNotFound { key: field.key.clone() },
                })
            }
            Err(err) => {
                return Err(StartActionError::StructuredConfigError {
                    moniker: component.moniker.clone(),
                    err: err.into(),
                })
            }
        };
        fields.fields.push(config_encoder::ConfigField {
            key: field.key.clone(),
            value: value,
            mutability: Default::default(),
        });
    }
    Ok(Some(fields))
}

/// Update config fields with the values received through configuration
/// capabilities.  This will perform routing on each of the configuration `use`
/// decls to get the values. Updating the fields is fine because configuration
/// capabilities take precedence over both the CVF value and "mutability: parent".
async fn update_config_with_capabilities(
    config: &mut ConfigFields,
    decl: &cm_rust::ComponentDecl,
    component: &Arc<ComponentInstance>,
) -> Result<(), StartActionError> {
    for field in config.fields.iter_mut() {
        let Some(use_config) = routing::config::get_use_config_from_key(&field.key, decl) else {
            continue;
        };
        let value =
            routing::config::route_config_value(use_config, component).await.map_err(|e| {
                StartActionError::StructuredConfigError {
                    moniker: component.moniker.clone(),
                    err: e.into(),
                }
            })?;

        let Some(value) = value else {
            continue;
        };
        if !field.value.matches_type(&value) {
            return Err(StartActionError::StructuredConfigError {
                moniker: component.moniker.clone(),
                err: StructuredConfigError::ValueMismatch { key: field.key.clone() },
            });
        }
        field.value = value;
    }
    Ok(())
}

/// Encode the configuration into a VMO.
async fn encode_config(
    config: ConfigFields,
    moniker: &Moniker,
) -> Result<fmem::Data, StartActionError> {
    let (vmo, size) = (|| {
        let encoded = config.encode_as_fidl_struct();
        let size = encoded.len() as u64;
        let vmo = Vmo::create(size)?;
        vmo.write(&encoded, 0)?;
        Ok((vmo, size))
    })()
    .map_err(|s| StartActionError::StructuredConfigError {
        err: StructuredConfigError::VmoCreateFailed(s),
        moniker: moniker.clone(),
    })?;
    Ok(fmem::Data::Buffer(fmem::Buffer { vmo, size }))
}

/// Returns the UseProtocolDecl for the LogSink protocol, if any.
fn get_logsink_decl<'a>(decl: &'a cm_rust::ComponentDecl) -> Option<&'a cm_rust::UseProtocolDecl> {
    decl.uses.iter().find_map(|use_| match use_ {
        cm_rust::UseDecl::Protocol(decl) => {
            (decl.source_name == flogger::LogSinkMarker::PROTOCOL_NAME).then_some(decl)
        }
        _ => None,
    })
}

/// Returns a ScopedLogger attributed to the component, given its use declaration for the
/// `fuchsia.logger.LogSink` protocol.
async fn create_scoped_logger(
    component: &Arc<ComponentInstance>,
    logsink_decl: cm_rust::UseProtocolDecl,
) -> Result<ScopedLogger, anyhow::Error> {
    let logsink = open_capability(&RouteRequest::UseProtocol(logsink_decl), component).await?;
    Ok(ScopedLogger::create(logsink)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use {
        crate::model::{
            actions::{ActionsManager, ShutdownAction, ShutdownType, StopAction},
            component::instance::{ResolvedInstanceState, UnresolvedInstanceState},
            component::{Component, WeakComponentInstance},
            testing::{
                routing_test_helpers::RoutingTestBuilder,
                test_helpers::{self, ActionsTest},
                test_hook::Lifecycle,
            },
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        cm_rust_testing::{ChildBuilder, ComponentDeclBuilder},
        errors::ModelError,
        fuchsia_async as fasync, fuchsia_zircon as zx,
        futures::{channel::mpsc, stream::FuturesUnordered, FutureExt, StreamExt},
        hooks::Event,
        hooks::{EventType, Hook, HooksRegistration},
        rand::seq::SliceRandom,
        routing::{bedrock::structured_dict::ComponentInput, resolving::ComponentAddress},
        std::sync::{Mutex, Weak},
    };

    // Child name for test child components instantiated during tests.
    const TEST_CHILD_NAME: &str = "child";

    struct ShutdownOnStartHook {
        component: Arc<ComponentInstance>,
        done: Mutex<mpsc::Sender<()>>,
    }

    #[async_trait]
    impl Hook for ShutdownOnStartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            fasync::Task::spawn(async move {
                ActionsManager::register(
                    self.component.clone(),
                    ShutdownAction::new(ShutdownType::Instance),
                )
                .await
                .expect("shutdown failed");
                self.done.lock().unwrap().try_send(()).unwrap();
            })
            .detach();
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component shuts down
    /// the action completes we see a Stop event emitted.
    async fn start_issues_shutdown() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let (done, mut hook_done_receiver) = mpsc::channel(1);
        let start_hook =
            Arc::new(ShutdownOnStartHook { component: child.clone(), done: Mutex::new(done) });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        ActionsManager::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .unwrap();

        // Wait until the action in the hook is done.
        hook_done_receiver.next().await.unwrap();

        let events: Vec<_> = test_topology
            .test_hook
            .lifecycle()
            .into_iter()
            .filter(|event| match event {
                Lifecycle::Start(_) | Lifecycle::Stop(_) => true,
                _ => false,
            })
            .collect();
        assert_eq!(
            events,
            vec![
                Lifecycle::Start(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap()),
                Lifecycle::Stop(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap())
            ]
        );
    }

    struct StopOnStartHook {
        component: Arc<ComponentInstance>,
        done: Mutex<mpsc::Sender<()>>,
    }

    #[async_trait]
    impl Hook for StopOnStartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            fasync::Task::spawn(async move {
                ActionsManager::register(self.component.clone(), StopAction::new(false))
                    .await
                    .expect("stop failed");
                self.done.lock().unwrap().try_send(()).unwrap();
            })
            .detach();
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component stops
    /// the action completes we see a Stop event emitted.
    async fn start_issues_stop() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let (done, mut hook_done_receiver) = mpsc::channel(1);
        let start_hook =
            Arc::new(StopOnStartHook { component: child.clone(), done: Mutex::new(done) });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        ActionsManager::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .unwrap();

        // Wait until the action in the hook is done.
        hook_done_receiver.next().await.unwrap();

        let events: Vec<_> = test_topology
            .test_hook
            .lifecycle()
            .into_iter()
            .filter(|event| match event {
                Lifecycle::Start(_) | Lifecycle::Stop(_) => true,
                _ => false,
            })
            .collect();
        assert_eq!(
            events,
            vec![
                Lifecycle::Start(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap()),
                Lifecycle::Stop(vec![format!("{}", TEST_CHILD_NAME).as_str()].try_into().unwrap())
            ]
        );
    }

    #[fuchsia::test]
    /// If start and stop happens concurrently, the component should either end up as
    /// started or stopped, without deadlocking.
    async fn concurrent_start_stop() {
        let (_test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        // Run start and stop in random order.
        let start_fut = ActionsManager::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        );
        let stop_fut = ActionsManager::register(child.clone(), StopAction::new(false));
        let mut futs = vec![start_fut.boxed(), stop_fut.boxed()];
        futs.shuffle(&mut rand::thread_rng());
        let stream: FuturesUnordered<_> = futs.into_iter().collect();
        let _: Vec<_> = stream.collect().await;

        // Both actions have completed, which demonstrates that the component did not deadlock.
    }

    /// If start is blocked during resolving then stop can interrupt it.
    #[fuchsia::test]
    async fn start_aborted_by_stop() {
        let root_name = "root";
        let components = vec![
            (root_name, ComponentDeclBuilder::new().child_default(TEST_CHILD_NAME).build()),
            (TEST_CHILD_NAME, test_helpers::component_decl_with_test_runner()),
        ];
        let builder = RoutingTestBuilder::new(components[0].0, components);

        // Add a blocker to the resolver that will cause resolving the child to block.
        let (resolved_tx, resolved_rx) = oneshot::channel::<()>();
        let (continue_tx, continue_rx) = oneshot::channel::<()>();
        let test_topology =
            builder.add_blocker(TEST_CHILD_NAME, resolved_tx, continue_rx).build().await;

        let _root =
            test_topology.model.root().find_and_maybe_resolve(&Moniker::default()).await.unwrap();
        let child =
            test_topology.model.root().find(&TEST_CHILD_NAME.try_into().unwrap()).await.unwrap();

        let start_fut = child
            .actions()
            .register_no_wait(StartAction::new(
                StartReason::Debug,
                None,
                IncomingCapabilities::default(),
            ))
            .await;

        // Wait until start is blocked.
        resolved_rx.await.unwrap();

        // Stop should cancel start.
        let stop_fut = child.actions().register_no_wait(StopAction::new(false)).await;
        assert_matches!(
            start_fut.await.unwrap_err(),
            ActionError::StartError { err: StartActionError::Aborted { .. } }
        );

        continue_tx.send(()).unwrap();
        stop_fut.await.unwrap();
    }

    #[fuchsia::test]
    async fn restart_set_execution_runtime() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionsManager::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
            )
            .await
            .expect("failed to start child");
            let state = child.lock_state().await;
            let runtime = state.get_started_state().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            ActionsManager::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let state = child.lock_state().await;
            assert!(state.get_started_state().is_none());
        }

        {
            let timestamp = zx::Time::get_monotonic();
            ActionsManager::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
            )
            .await
            .expect("failed to start child");
            let state = child.lock_state().await;
            let runtime = state.get_started_state().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }
    }

    #[fuchsia::test]
    async fn restart_does_not_refresh_resolved_state() {
        let (mut test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionsManager::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
            )
            .await
            .expect("failed to start child");
            let state = child.lock_state().await;
            let runtime = state.get_started_state().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            let () = ActionsManager::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let state = child.lock_state().await;
            assert!(state.get_started_state().is_none());
        }

        let resolver = test_harness.resolver.as_mut();
        let original_decl =
            resolver.get_component_decl(TEST_CHILD_NAME).expect("child decl not stored");
        let mut modified_decl = original_decl.clone();
        modified_decl.children.push(ChildBuilder::new().name("foo").build());
        resolver.add_component(TEST_CHILD_NAME, modified_decl.clone());

        ActionsManager::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .expect("failed to start child");

        let resolved_decl = get_resolved_decl(&child).await;
        assert_ne!(resolved_decl, modified_decl);
        assert_eq!(resolved_decl, original_decl);
    }

    async fn build_tree_with_single_child(
        child_name: &'static str,
    ) -> (ActionsTest, Arc<ComponentInstance>) {
        let root_name = "root";
        let components = vec![
            (root_name, ComponentDeclBuilder::new().child_default(child_name).build()),
            (child_name, test_helpers::component_decl_with_test_runner()),
        ];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let child = test_topology.look_up(vec![child_name].try_into().unwrap()).await;

        (test_topology, child)
    }

    async fn get_resolved_decl(component: &Arc<ComponentInstance>) -> ComponentDecl {
        let state = component.lock_state().await;
        state.get_resolved_state().expect("expected component to be resolved").decl().clone()
    }

    #[fuchsia::test]
    async fn check_should_return_early() {
        let m = Moniker::try_from(vec!["foo"]).unwrap();

        // Checks based on InstanceState:
        assert!(should_return_early(&InstanceState::New, &m).is_none());
        assert!(should_return_early(
            &InstanceState::Unresolved(UnresolvedInstanceState::new(ComponentInput::default())),
            &m
        )
        .is_none());
        assert_matches!(
            should_return_early(&InstanceState::Destroyed, &m),
            Some(Err(StartActionError::InstanceDestroyed { moniker: _ }))
        );
        let (_, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let decl = ComponentDeclBuilder::new().child_default(TEST_CHILD_NAME).build();
        let resolved_component = Component {
            resolved_url: "".to_string(),
            context_to_resolve_children: None,
            decl,
            package: None,
            config: None,
            abi_revision: None,
        };
        let ris = ResolvedInstanceState::new(
            &child,
            resolved_component.clone(),
            ComponentAddress::from_absolute_url(&child.component_url).unwrap(),
            Default::default(),
            ComponentInput::default(),
        )
        .await
        .unwrap();
        assert!(should_return_early(&InstanceState::Resolved(ris), &m).is_none());

        // Check for already_started:
        {
            let ris = ResolvedInstanceState::new(
                &child,
                resolved_component,
                ComponentAddress::from_absolute_url(&child.component_url).unwrap(),
                Default::default(),
                ComponentInput::default(),
            )
            .await
            .unwrap();
            let started_state = StartedInstanceState::new(
                None,
                WeakComponentInstance::invalid(),
                StartReason::Debug,
                None,
                None,
            );
            assert_matches!(
                should_return_early(&InstanceState::Started(ris, started_state), &m),
                Some(Ok(()))
            );
        }

        // Check for shut_down:
        let _ = child.stop_instance_internal(true).await;
        assert!(child.lock_state().await.is_shut_down());
        let state = child.lock_state().await;
        assert_matches!(
            should_return_early(&*state, &m),
            Some(Err(StartActionError::InstanceShutDown { moniker: _ }))
        );
    }

    #[fuchsia::test]
    async fn check_already_started() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        ActionsManager::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .expect("failed to start child");

        let m = Moniker::try_from(vec!["TEST_CHILD_NAME"]).unwrap();
        assert_matches!(should_return_early(&*child.lock_state().await, &m), Some(Ok(())));
    }
}
