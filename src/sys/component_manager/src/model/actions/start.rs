// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::bedrock::program::{Program, StartInfo},
    crate::framework::controller,
    crate::model::{
        actions::{Action, ActionKey},
        component::{
            ComponentInstance, ExecutionState, InstanceState, Package, ResolvedInstanceState,
            Runtime, SandboxDispatcher, StartReason, WeakComponentInstance,
        },
        error::{StartActionError, StructuredConfigError},
        hooks::{Event, EventPayload, RuntimeInfo},
        namespace::create_namespace,
        routing::{route_and_open_capability, OpenOptions, RouteRequest},
    },
    crate::runner::RemoteRunner,
    ::namespace::Entry as NamespaceEntry,
    ::routing::{component_instance::ComponentInstanceInterface, policy::GlobalPolicyChecker},
    async_trait::async_trait,
    cm_logger::scoped::ScopedLogger,
    config_encoder::ConfigFields,
    fidl::{
        endpoints::{create_proxy, DiscoverableProtocolMarker},
        Vmo,
    },
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_data as fdata, fidl_fuchsia_diagnostics_types as fdiagnostics,
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger, fidl_fuchsia_mem as fmem,
    fidl_fuchsia_process as fprocess, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx,
    futures::channel::oneshot,
    moniker::Moniker,
    std::sync::Arc,
    tracing::warn,
};

/// Starts a component instance.
pub struct StartAction {
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<NamespaceEntry>,
}

impl StartAction {
    pub fn new(
        start_reason: StartReason,
        execution_controller_task: Option<controller::ExecutionControllerTask>,
        numbered_handles: Vec<fprocess::HandleInfo>,
        additional_namespace_entries: Vec<NamespaceEntry>,
    ) -> Self {
        Self {
            start_reason,
            execution_controller_task,
            numbered_handles,
            additional_namespace_entries,
        }
    }
}

#[async_trait]
impl Action for StartAction {
    type Output = Result<fsys::StartResult, StartActionError>;
    async fn handle(self, component: &Arc<ComponentInstance>) -> Self::Output {
        do_start(
            component,
            &self.start_reason,
            self.execution_controller_task,
            self.numbered_handles,
            self.additional_namespace_entries,
        )
        .await
    }

    fn key(&self) -> ActionKey {
        ActionKey::Start
    }
}

struct StartContext {
    runner: Option<RemoteRunner>,
    start_info: StartInfo,
    diagnostics_sender: oneshot::Sender<fdiagnostics::ComponentDiagnostics>,
}

async fn do_start(
    component: &Arc<ComponentInstance>,
    start_reason: &StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<NamespaceEntry>,
) -> Result<fsys::StartResult, StartActionError> {
    // Pre-flight check: if the component is already started, or was shut down, return now. Note
    // that `start` also performs this check before scheduling the action here. We do it again
    // while the action is registered to avoid the risk of dispatching the Started event twice.
    {
        let state = component.lock_state().await;
        let execution = component.lock_execution().await;
        if let Some(res) = should_return_early(&state, &execution, &component.moniker) {
            return res;
        }
    }

    let (diagnostics_sender, diagnostics_receiver) = oneshot::channel();

    // Resolve the component.
    let resolved_component = component.resolve().await.map_err(|err| {
        StartActionError::ResolveActionError { moniker: component.moniker.clone(), err }
    })?;

    // Find the runner to use.
    let runner = {
        let resolved_state = component.lock_resolved_state().await.map_err(|err| {
            StartActionError::ResolveActionError { moniker: component.moniker.clone(), err }
        })?;
        resolve_runner(component, &*resolved_state).await.map_err(|err| {
            warn!(moniker = %component.moniker, %err, "Failed to resolve runner.");
            err
        })?
    };

    // Generate the Runtime which will be set in the Execution.
    let (pending_runtime, start_info, break_on_start) = make_execution_runtime(
        &component,
        component.policy_checker(),
        resolved_component.resolved_url.clone(),
        resolved_component.package.as_ref(),
        &resolved_component.decl,
        resolved_component.config,
        start_reason.clone(),
        execution_controller_task,
        numbered_handles,
        additional_namespace_entries,
    )
    .await?;

    let start_context = StartContext { runner, start_info, diagnostics_sender };

    // Dispatch the Started and the DebugStarted event.
    component
        .hooks
        .dispatch(&Event::new_with_timestamp(
            component,
            EventPayload::Started {
                runtime: RuntimeInfo::from_runtime(&pending_runtime, diagnostics_receiver),
                component_decl: resolved_component.decl,
            },
            pending_runtime.timestamp,
        ))
        .await;
    component
        .hooks
        .dispatch(&Event::new_with_timestamp(
            component,
            EventPayload::DebugStarted {
                runtime_dir: pending_runtime.runtime_dir.clone(),
                break_on_start: Arc::new(break_on_start),
            },
            pending_runtime.timestamp,
        ))
        .await;

    let res = start_component(&component, pending_runtime, start_context).await;
    match res {
        Ok(fsys::StartResult::AlreadyStarted) => {}
        Ok(fsys::StartResult::Started) => {}
        Err(ref _e) => {
            // Since we dispatched a start event, dispatch a stop event
            // TODO(fxbug.dev/87507): It is possible this issues Stop after
            // Destroyed is issued.
            component
                .hooks
                .dispatch(&Event::new(component, EventPayload::Stopped { status: zx::Status::OK }))
                .await;
        }
    };
    res
}

/// Set the Runtime in the Execution and start the exit watcher. From component manager's
/// perspective, this indicates that the component has started. If this returns an error, the
/// component was shut down and the Runtime is not set, otherwise the function returns the
/// start context with the runtime set. This function acquires the state and execution locks on
/// `Component`.
async fn start_component(
    component: &Arc<ComponentInstance>,
    mut pending_runtime: Runtime,
    start_context: StartContext,
) -> Result<fsys::StartResult, StartActionError> {
    let state = component.lock_state().await;
    let mut execution = component.lock_execution().await;

    if let Some(r) = should_return_early(&state, &execution, &component.moniker) {
        return r;
    }

    let StartContext { runner, start_info, diagnostics_sender } = start_context;
    if let Some(runner) = runner {
        pending_runtime.set_program(
            Program::start(&runner, start_info, diagnostics_sender).map_err(|err| {
                StartActionError::StartProgramError { moniker: component.moniker.clone(), err }
            })?,
            component.as_weak(),
        );
    }
    execution.runtime = Some(pending_runtime);
    Ok(fsys::StartResult::Started)
}

/// Returns `Some(Result)` if `start` should return early due to any of the following:
/// - The component instance is destroyed.
/// - The component instance is shut down.
/// - The component instance is already started.
pub fn should_return_early(
    component: &InstanceState,
    execution: &ExecutionState,
    moniker: &Moniker,
) -> Option<Result<fsys::StartResult, StartActionError>> {
    match component {
        InstanceState::New | InstanceState::Unresolved(_) | InstanceState::Resolved(_) => {}
        InstanceState::Destroyed => {
            return Some(Err(StartActionError::InstanceDestroyed { moniker: moniker.clone() }));
        }
    }
    if execution.is_shut_down() {
        Some(Err(StartActionError::InstanceShutDown { moniker: moniker.clone() }))
    } else if execution.runtime.is_some() {
        Some(Ok(fsys::StartResult::AlreadyStarted))
    } else {
        None
    }
}

/// Returns a RemoteRunner routed to the component's runner, if it specifies one.
///
/// Returns None if the component's decl does not specify a runner.
async fn resolve_runner(
    component: &Arc<ComponentInstance>,
    resolved_state: &ResolvedInstanceState,
) -> Result<Option<RemoteRunner>, StartActionError> {
    let Some(runner) = resolved_state.decl().get_runner() else {
        return Ok(None);
    };

    // Open up a channel to the runner.
    let (client, server) = create_proxy::<fcrunner::ComponentRunnerMarker>().unwrap();
    let mut server_channel = server.into_channel();
    let options = OpenOptions {
        flags: fio::OpenFlags::NOT_DIRECTORY,
        relative_path: "".into(),
        server_chan: &mut server_channel,
    };
    route_and_open_capability(RouteRequest::UseRunner(runner.clone()), component, options)
        .await
        .map_err(|err| StartActionError::ResolveRunnerError {
            moniker: component.moniker.clone(),
            err: Box::new(err),
            runner: runner.source_name,
        })?;

    return Ok(Some(RemoteRunner::new(client)));
}

/// Returns a configured Runtime for a component and the start info (without actually starting
/// the component).
async fn make_execution_runtime(
    component: &Arc<ComponentInstance>,
    checker: &GlobalPolicyChecker,
    url: String,
    package: Option<&Package>,
    decl: &cm_rust::ComponentDecl,
    config: Option<ConfigFields>,
    start_reason: StartReason,
    execution_controller_task: Option<controller::ExecutionControllerTask>,
    numbered_handles: Vec<fprocess::HandleInfo>,
    additional_namespace_entries: Vec<NamespaceEntry>,
) -> Result<(Runtime, StartInfo, zx::EventPair), StartActionError> {
    // TODO(https://fxbug.dev/120713): Consider moving this check to ComponentInstance::add_child
    match component.on_terminate {
        fdecl::OnTerminate::Reboot => {
            checker.reboot_on_terminate_allowed(&component.moniker).map_err(|err| {
                StartActionError::RebootOnTerminateForbidden {
                    moniker: component.moniker.clone(),
                    err,
                }
            })?;
        }
        fdecl::OnTerminate::None => {}
    }

    // Create the component's namespace.
    let namespace = create_namespace(package, component, decl, additional_namespace_entries)
        .await
        .map_err(|err| StartActionError::CreateNamespaceError {
            moniker: component.moniker.clone(),
            err,
        })?;

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

    // Set up channels into/out of the new component. These are absent from non-executable
    // components.
    let (outgoing_dir, outgoing_dir_server) = if decl.get_runner().is_some() {
        let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        (Some(proxy), Some(server_end))
    } else {
        (None, None)
    };

    let (runtime_dir, runtime_dir_server) = if decl.get_runner().is_some() {
        let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        (Some(proxy), Some(server_end))
    } else {
        (None, None)
    };

    let encoded_config = if let Some(config) = config {
        let (vmo, size) = (|| {
            let encoded = config.encode_as_fidl_struct();
            let size = encoded.len() as u64;
            let vmo = Vmo::create(size)?;
            vmo.write(&encoded, 0)?;
            Ok((vmo, size))
        })()
        .map_err(|s| StartActionError::StructuredConfigError {
            err: StructuredConfigError::VmoCreateFailed(s),
            moniker: component.moniker.clone(),
        })?;
        Some(fmem::Data::Buffer(fmem::Buffer { vmo, size }))
    } else {
        None
    };

    let resolved_state = component
        .lock_resolved_state()
        .await
        .expect("component must be resolved in order to start");

    let sandbox_dispatcher = SandboxDispatcher::new(
        resolved_state.program_sandbox.clone(),
        decl.capabilities.clone(),
        outgoing_dir.clone(),
        WeakComponentInstance::new(&component),
    );

    let runtime = Runtime::new(
        outgoing_dir,
        runtime_dir,
        start_reason,
        execution_controller_task,
        logger,
        sandbox_dispatcher,
    );
    let (break_on_start_left, break_on_start_right) = zx::EventPair::create();

    let start_info = StartInfo {
        resolved_url: url,
        program: decl
            .program
            .as_ref()
            .map(|p| p.info.clone())
            .unwrap_or_else(|| fdata::Dictionary::default()),
        namespace,
        outgoing_dir: outgoing_dir_server,
        runtime_dir: runtime_dir_server,
        numbered_handles,
        encoded_config,
        break_on_start: Some(break_on_start_left),
    };

    Ok((runtime, start_info, break_on_start_right))
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
    let (logsink, logsink_server_end) = create_proxy::<flogger::LogSinkMarker>().unwrap();
    let route_request = RouteRequest::UseProtocol(logsink_decl);
    let open_options = OpenOptions {
        flags: fio::OpenFlags::empty(),
        relative_path: String::new(),
        server_chan: &mut logsink_server_end.into_channel(),
    };
    route_and_open_capability(route_request, component, open_options).await?;
    Ok(ScopedLogger::create(logsink)?)
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            model::{
                actions::{
                    resolve::sandbox_construction::ComponentSandboxes, start::should_return_early,
                    ActionSet, ShutdownAction, StartAction, StopAction,
                },
                component::{
                    ComponentInstance, ExecutionState, InstanceState, ResolvedInstanceState,
                    Runtime, SandboxDispatcher, StartReason, UnresolvedInstanceState,
                    WeakComponentInstance,
                },
                error::{ModelError, StartActionError},
                hooks::{Event, EventType, Hook, HooksRegistration},
                testing::{
                    test_helpers::{self, ActionsTest},
                    test_hook::Lifecycle,
                },
            },
            sandbox_util::Sandbox,
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        cm_rust::ComponentDecl,
        cm_rust_testing::{ChildDeclBuilder, ComponentDeclBuilder},
        cm_types::Name,
        fidl_fuchsia_sys2 as fsys, fuchsia, fuchsia_zircon as zx,
        moniker::Moniker,
        routing::resolving::ComponentAddress,
        std::sync::{Arc, Weak},
    };

    // Child name for test child components instantiated during tests.
    const TEST_CHILD_NAME: &str = "child";

    struct StartHook {
        component: Arc<ComponentInstance>,
    }

    #[async_trait]
    impl Hook for StartHook {
        async fn on(self: Arc<Self>, _event: &Event) -> Result<(), ModelError> {
            ActionSet::register(self.component.clone(), ShutdownAction::new())
                .await
                .expect("shutdown failed");
            Ok(())
        }
    }

    #[fuchsia::test]
    /// Validate that if a start action is issued and the component stops
    /// the action completes we see a Stop event emitted.
    async fn start_issues_stop() {
        let (test_topology, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let start_hook = Arc::new(StartHook { component: child.clone() });
        child
            .hooks
            .install(vec![HooksRegistration::new(
                "my_start_hook",
                vec![EventType::Started],
                Arc::downgrade(&start_hook) as Weak<dyn Hook>,
            )])
            .await;

        match ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
        )
        .await
        {
            Err(StartActionError::InstanceShutDown { moniker: m }) => {
                assert_eq!(Moniker::try_from(vec![TEST_CHILD_NAME]).unwrap(), m);
            }
            e => panic!("Unexpected result from component start: {:?}", e),
        }

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
    async fn restart_set_execution_runtime() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            ActionSet::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let execution = child.lock_execution().await;
            assert!(execution.runtime.is_none());
        }

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }
    }

    #[fuchsia::test]
    async fn restart_does_not_refresh_resolved_state() {
        let (mut test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        {
            let timestamp = zx::Time::get_monotonic();
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![]),
            )
            .await
            .expect("failed to start child");
            let execution = child.lock_execution().await;
            let runtime = execution.runtime.as_ref().expect("child runtime is unexpectedly empty");
            assert!(runtime.timestamp > timestamp);
        }

        {
            let () = ActionSet::register(child.clone(), StopAction::new(false))
                .await
                .expect("failed to stop child");
            let execution = child.lock_execution().await;
            assert!(execution.runtime.is_none());
        }

        let resolver = test_harness.resolver.as_mut();
        let original_decl =
            resolver.get_component_decl(TEST_CHILD_NAME).expect("child decl not stored");
        let mut modified_decl = original_decl.clone();
        modified_decl.children.push(ChildDeclBuilder::new().name("foo").build());
        resolver.add_component(TEST_CHILD_NAME, modified_decl.clone());

        ActionSet::register(
            child.clone(),
            StartAction::new(StartReason::Debug, None, vec![], vec![]),
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
            (root_name, ComponentDeclBuilder::new().add_lazy_child(child_name).build()),
            (child_name, test_helpers::component_decl_with_test_runner()),
        ];
        let test_topology = ActionsTest::new(components[0].0, components, None).await;

        let child = test_topology.look_up(vec![child_name].try_into().unwrap()).await;

        (test_topology, child)
    }

    async fn get_resolved_decl(component: &Arc<ComponentInstance>) -> ComponentDecl {
        let state = component.lock_state().await;
        let resolved_state = match &*state {
            InstanceState::Resolved(resolve_state) => resolve_state,
            _ => panic!("expected component to be resolved"),
        };

        resolved_state.decl().clone()
    }

    #[fuchsia::test]
    async fn check_should_return_early() {
        let m = Moniker::try_from(vec!["foo"]).unwrap();
        let es = ExecutionState::new();

        // Checks based on InstanceState:
        assert!(should_return_early(&InstanceState::New, &es, &m).is_none());
        assert!(should_return_early(
            &InstanceState::Unresolved(UnresolvedInstanceState::new(Sandbox::new())),
            &es,
            &m
        )
        .is_none());
        assert_matches!(
            should_return_early(&InstanceState::Destroyed, &es, &m),
            Some(Err(StartActionError::InstanceDestroyed { moniker: _ }))
        );
        let (_, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;
        let decl = ComponentDeclBuilder::new().add_lazy_child(TEST_CHILD_NAME).build();
        let mut sandbox_finalization_output = ComponentSandboxes::default();
        let name = Name::new(TEST_CHILD_NAME).unwrap();
        sandbox_finalization_output.child_sandboxes.insert(name, Sandbox::new());
        let ris = ResolvedInstanceState::new(
            &child,
            decl,
            None,
            None,
            ComponentAddress::from_absolute_url(&child.component_url).unwrap(),
            None,
            sandbox_finalization_output,
        )
        .await
        .unwrap();
        assert!(should_return_early(&InstanceState::Resolved(ris), &es, &m).is_none());

        // Check for already_started:
        {
            let mut es = ExecutionState::new();
            es.runtime = Some(Runtime::new(
                None,
                None,
                StartReason::Debug,
                None,
                None,
                SandboxDispatcher::new(
                    Sandbox::new(),
                    vec![],
                    None,
                    WeakComponentInstance::invalid(),
                ),
            ));
            assert!(!es.is_shut_down());
            assert_matches!(
                should_return_early(&InstanceState::New, &es, &m),
                Some(Ok(fsys::StartResult::AlreadyStarted))
            );
        }

        // Check for shut_down:
        let _ = child.stop_instance_internal(true).await;
        let execution = child.lock_execution().await;
        assert!(execution.is_shut_down());
        assert_matches!(
            should_return_early(&InstanceState::New, &execution, &m),
            Some(Err(StartActionError::InstanceShutDown { moniker: _ }))
        );
    }

    #[fuchsia::test]
    async fn check_already_started() {
        let (_test_harness, child) = build_tree_with_single_child(TEST_CHILD_NAME).await;

        assert_eq!(
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![])
            )
            .await
            .expect("failed to start child"),
            fsys::StartResult::Started
        );

        assert_eq!(
            ActionSet::register(
                child.clone(),
                StartAction::new(StartReason::Debug, None, vec![], vec![])
            )
            .await
            .expect("failed to start child"),
            fsys::StartResult::AlreadyStarted
        );
    }
}
