// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        builtin_environment::{BuiltinEnvironment, BuiltinEnvironmentBuilder},
        framework::realm::RealmCapabilityHost,
        model::{
            component::instance::InstanceState,
            component::{ComponentInstance, StartReason, WeakComponentInstance},
            events::{registry::EventSubscription, source::EventSource, stream::EventStream},
            model::Model,
            testing::{
                mocks::{ControlMessage, MockResolver, MockRunner},
                test_hook::TestHook,
            },
        },
    },
    camino::Utf8PathBuf,
    cm_config::RuntimeConfig,
    cm_rust::{
        Availability, CapabilityDecl, ChildDecl, ComponentDecl, ConfigValuesData, EventStreamDecl,
        NativeIntoFidl, RunnerDecl, UseEventStreamDecl, UseSource,
    },
    cm_types::Name,
    cm_types::Url,
    fidl::endpoints,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, Koid},
    futures::{channel::mpsc::Receiver, lock::Mutex, StreamExt, TryStreamExt},
    hooks::HooksRegistration,
    moniker::{ChildName, Moniker},
    std::collections::HashSet,
    std::sync::Arc,
    vfs::{directory::entry::DirectoryEntry, service},
};

pub const TEST_RUNNER_NAME: &str = cm_rust_testing::TEST_RUNNER_NAME;

// TODO(https://fxbug.dev/42140194): remove function wrappers once the routing_test_helpers
// lib has a stable API.
pub fn default_component_decl() -> ComponentDecl {
    ::routing_test_helpers::default_component_decl()
}

pub fn component_decl_with_test_runner() -> ComponentDecl {
    ::routing_test_helpers::component_decl_with_test_runner()
}

pub struct ComponentInfo {
    pub component: Arc<ComponentInstance>,
    pub channel_id: Koid,
}

impl ComponentInfo {
    /// Given a `ComponentInstance` which has been bound, look up the resolved URL
    /// and package into a `ComponentInfo` struct.
    pub async fn new(component: Arc<ComponentInstance>) -> ComponentInfo {
        // The koid is the only unique piece of information we have about
        // a component start request. Two start requests for the same
        // component URL look identical to the Runner, the only difference
        // being the Channel passed to the Runner to use for the
        // ComponentController protocol.
        let koid = {
            component
                .lock_state()
                .await
                .get_started_state()
                .expect("expected component to be running")
                .program_koid()
                .expect("program is unexpectedly missing")
        };

        ComponentInfo { component, channel_id: koid }
    }

    /// Checks that the component is shut down, panics if this is not true.
    pub async fn check_is_shut_down(&self, runner: &MockRunner) {
        // Check the list of requests for this component
        let request_map = runner.get_request_map();
        let unlocked_map = request_map.lock().await;
        let request_vec = unlocked_map
            .get(&self.channel_id)
            .expect("request map didn't have channel id, perhaps the controller wasn't started?");
        assert_eq!(*request_vec, vec![ControlMessage::Stop]);

        assert!(self.component.lock_state().await.is_shut_down());
    }

    /// Checks that the component has not been shut down, panics if it has.
    pub async fn check_not_shut_down(&self, runner: &MockRunner) {
        // If the MockController has started, check that no stop requests have
        // been received.
        let request_map = runner.get_request_map();
        let unlocked_map = request_map.lock().await;
        if let Some(request_vec) = unlocked_map.get(&self.channel_id) {
            assert_eq!(*request_vec, vec![]);
        }

        assert!(!self.component.lock_state().await.is_shut_down());
    }
}

pub async fn execution_is_shut_down(component: &ComponentInstance) -> bool {
    component.lock_state().await.is_shut_down()
}

/// Returns true if the given child (live or deleting) exists.
pub async fn has_child<'a>(component: &'a ComponentInstance, moniker: &'a str) -> bool {
    match *component.lock_state().await {
        InstanceState::Resolved(ref s) | InstanceState::Started(ref s, _) => {
            s.children().map(|(k, _)| k.clone()).any(|m| m == moniker.try_into().unwrap())
        }
        InstanceState::Shutdown(ref state, _) => {
            state.children.iter().map(|(k, _)| k.clone()).any(|m| m == moniker.try_into().unwrap())
        }
        InstanceState::Destroyed => false,
        _ => panic!("not resolved"),
    }
}

/// Return the incarnation id of the given child.
pub async fn get_incarnation_id<'a>(component: &'a ComponentInstance, moniker: &'a str) -> u32 {
    component
        .lock_state()
        .await
        .get_resolved_state()
        .expect("not resolved")
        .get_child(&moniker.try_into().unwrap())
        .unwrap()
        .incarnation_id()
}

/// Return all monikers of the live children of the given `component`.
pub async fn get_live_children(component: &ComponentInstance) -> HashSet<ChildName> {
    match *component.lock_state().await {
        InstanceState::Resolved(ref s) | InstanceState::Started(ref s, _) => {
            s.children().map(|(m, _)| m.clone()).collect()
        }
        InstanceState::Shutdown(ref s, _) => s.children.iter().map(|(m, _)| m.clone()).collect(),
        InstanceState::Destroyed => HashSet::new(),
        _ => panic!("not resolved"),
    }
}

/// Return the child of the given `component` with moniker `child`.
pub async fn get_live_child<'a>(
    component: &'a ComponentInstance,
    child: &'a str,
) -> Arc<ComponentInstance> {
    component
        .lock_state()
        .await
        .get_resolved_state()
        .expect("not resolved")
        .get_child(&child.try_into().unwrap())
        .unwrap()
        .clone()
}

pub async fn list_directory<'a>(root_proxy: &'a fio::DirectoryProxy) -> Vec<String> {
    let entries = fuchsia_fs::directory::readdir(&root_proxy).await.expect("readdir failed");
    let mut items = entries.iter().map(|entry| entry.name.clone()).collect::<Vec<String>>();
    items.sort();
    items
}

pub async fn list_directory_recursive<'a>(root_proxy: &'a fio::DirectoryProxy) -> Vec<String> {
    let entries = fuchsia_fs::directory::readdir_recursive(&root_proxy, /*timeout=*/ None);
    let mut items = entries
        .map(|result| result.map(|entry| entry.name.clone()))
        .try_collect::<Vec<_>>()
        .await
        .expect("readdir failed");
    items.sort();
    items
}

pub async fn write_file<'a>(root_proxy: &'a fio::DirectoryProxy, path: &'a str, contents: &'a str) {
    let file_proxy = fuchsia_fs::directory::open_file_no_describe(
        &root_proxy,
        path,
        fio::OpenFlags::RIGHT_WRITABLE | fio::OpenFlags::CREATE,
    )
    .expect("Failed to open file.");
    let _: u64 = file_proxy
        .write(contents.as_bytes())
        .await
        .expect("Unable to write file.")
        .map_err(zx::Status::from_raw)
        .expect("Write failed");
}

/// Create a `DirectoryEntry` and `Channel` pair. The created `DirectoryEntry`
/// provides the service `P`, sending all requests to the returned channel.
pub fn create_service_directory_entry<P>(
) -> (Arc<dyn DirectoryEntry>, futures::channel::mpsc::Receiver<fidl::endpoints::Request<P>>)
where
    P: fidl::endpoints::ProtocolMarker,
    fidl::endpoints::Request<P>: Send,
{
    use futures::sink::SinkExt;
    let (sender, receiver) = futures::channel::mpsc::channel(0);
    let entry = service::host(move |mut stream: P::RequestStream| {
        let mut sender = sender.clone();
        async move {
            while let Ok(Some(request)) = stream.try_next().await {
                sender.send(request).await.unwrap();
            }
        }
    });
    (entry, receiver)
}

/// Wait for a ComponentRunnerStart request, acknowledge it, and return
/// the start info.
///
/// Panics if the channel closes before we receive a request.
pub async fn wait_for_runner_request(
    recv: &mut Receiver<fcrunner::ComponentRunnerRequest>,
) -> fcrunner::ComponentStartInfo {
    let fcrunner::ComponentRunnerRequest::Start { start_info, .. } =
        recv.next().await.expect("Channel closed before request was received.");
    start_info
}

/// Contains test model and ancillary objects.
pub struct TestModelResult {
    pub model: Arc<Model>,
    pub builtin_environment: Arc<Mutex<BuiltinEnvironment>>,
    pub realm_proxy: Option<fcomponent::RealmProxy>,
    pub mock_runner: Arc<MockRunner>,
    pub mock_resolver: Box<MockResolver>,
}

pub struct TestEnvironmentBuilder {
    root_component: String,
    components: Vec<(&'static str, ComponentDecl)>,
    config_values: Vec<(&'static str, ConfigValuesData)>,
    runtime_config: RuntimeConfig,
    component_id_index_path: Option<Utf8PathBuf>,
    realm_moniker: Option<Moniker>,
    hooks: Vec<HooksRegistration>,
    front_hooks: Vec<HooksRegistration>,
}

impl TestEnvironmentBuilder {
    pub fn new() -> Self {
        Self {
            root_component: "root".to_owned(),
            components: vec![],
            config_values: vec![],
            runtime_config: Default::default(),
            component_id_index_path: None,
            realm_moniker: None,
            hooks: vec![],
            front_hooks: vec![],
        }
    }

    pub fn set_root_component(mut self, root_component: &str) -> Self {
        self.root_component = root_component.to_owned();
        self
    }

    pub fn set_components(mut self, components: Vec<(&'static str, ComponentDecl)>) -> Self {
        self.components = components;
        self
    }

    pub fn set_config_values(
        mut self,
        config_values: Vec<(&'static str, ConfigValuesData)>,
    ) -> Self {
        self.config_values = config_values;
        self
    }

    pub fn set_component_id_index_path(mut self, path: Utf8PathBuf) -> Self {
        self.component_id_index_path = Some(path);
        self
    }

    pub fn set_runtime_config(mut self, runtime_config: RuntimeConfig) -> Self {
        self.runtime_config = runtime_config;
        self
    }

    pub fn set_realm_moniker(mut self, moniker: Moniker) -> Self {
        self.realm_moniker = Some(moniker);
        self
    }

    pub fn set_hooks(mut self, hooks: Vec<HooksRegistration>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn set_front_hooks(mut self, hooks: Vec<HooksRegistration>) -> Self {
        self.front_hooks = hooks;
        self
    }

    /// Returns a `Model` and `BuiltinEnvironment` suitable for most tests.
    pub async fn build(mut self) -> TestModelResult {
        let mock_runner = Arc::new(MockRunner::new());

        let mut mock_resolver = MockResolver::new();
        for (name, decl) in &self.components {
            mock_resolver.add_component(name, decl.clone());
        }

        for (path, config) in &self.config_values {
            mock_resolver.add_config_values(path, config.clone());
        }

        self.runtime_config.root_component_url =
            Some(Url::new(format!("test:///{}", self.root_component)).unwrap());
        self.runtime_config.builtin_capabilities.push(CapabilityDecl::Runner(RunnerDecl {
            name: TEST_RUNNER_NAME.parse().unwrap(),
            source_path: None,
        }));
        self.runtime_config.builtin_capabilities.push(CapabilityDecl::EventStream(
            EventStreamDecl { name: "started".parse().unwrap() },
        ));
        self.runtime_config.component_id_index_path = self.component_id_index_path;
        self.runtime_config.enable_introspection = true;

        let mock_resolver = Box::new(mock_resolver);
        let builtin_environment = Arc::new(Mutex::new(
            BuiltinEnvironmentBuilder::new()
                .add_resolver("test".to_string(), mock_resolver.clone())
                .add_runner(TEST_RUNNER_NAME.parse().unwrap(), mock_runner.clone())
                .set_runtime_config(self.runtime_config)
                .build()
                .await
                .expect("builtin environment setup failed"),
        ));
        let model = builtin_environment.lock().await.model.clone();

        model.root().hooks.install(self.hooks).await;
        model.root().hooks.install_front_for_test(self.front_hooks).await;
        builtin_environment.lock().await.discover_root_component().await;

        // Host framework service for `moniker`, if requested.
        let realm_proxy = if let Some(moniker) = self.realm_moniker {
            let (realm_proxy, stream) =
                endpoints::create_proxy_and_stream::<fcomponent::RealmMarker>().unwrap();
            let component = WeakComponentInstance::from(
                &model
                    .root()
                    .find_and_maybe_resolve(&moniker)
                    .await
                    .unwrap_or_else(|e| panic!("could not look up {}: {:?}", moniker, e)),
            );
            let realm_capability_host = RealmCapabilityHost::new_for_test(
                Arc::downgrade(&model),
                model.context().runtime_config().clone(),
            );
            fasync::Task::spawn(async move {
                realm_capability_host
                    .serve(component, stream)
                    .await
                    .expect("failed serving realm service");
            })
            .detach();
            Some(realm_proxy)
        } else {
            None
        };

        TestModelResult { model, builtin_environment, realm_proxy, mock_runner, mock_resolver }
    }
}

/// A test harness for tests that wish to register or verify actions.
pub struct ActionsTest {
    pub model: Arc<Model>,
    pub builtin_environment: Arc<Mutex<BuiltinEnvironment>>,
    pub test_hook: Arc<TestHook>,
    pub realm_proxy: Option<fcomponent::RealmProxy>,
    pub runner: Arc<MockRunner>,
    pub resolver: Box<MockResolver>,
}

impl ActionsTest {
    pub async fn new(
        root_component: &'static str,
        components: Vec<(&'static str, ComponentDecl)>,
        moniker: Option<Moniker>,
    ) -> Self {
        Self::new_with_hooks(root_component, components, moniker, vec![]).await
    }

    pub async fn new_with_hooks(
        root_component: &'static str,
        components: Vec<(&'static str, ComponentDecl)>,
        moniker: Option<Moniker>,
        extra_hooks: Vec<HooksRegistration>,
    ) -> Self {
        let test_hook = Arc::new(TestHook::new());
        let mut hooks = test_hook.hooks();
        hooks.extend(extra_hooks);
        let builder = TestEnvironmentBuilder::new()
            .set_root_component(root_component)
            .set_components(components)
            .set_hooks(hooks);
        let builder =
            if let Some(moniker) = moniker { builder.set_realm_moniker(moniker) } else { builder };
        let TestModelResult { model, builtin_environment, realm_proxy, mock_runner, mock_resolver } =
            builder.build().await;

        Self {
            model,
            builtin_environment,
            test_hook,
            realm_proxy,
            runner: mock_runner,
            resolver: mock_resolver,
        }
    }

    pub async fn look_up(&self, moniker: Moniker) -> Arc<ComponentInstance> {
        self.model
            .root()
            .find_and_maybe_resolve(&moniker)
            .await
            .unwrap_or_else(|e| panic!("could not look up {}: {:?}", moniker, e))
    }

    pub async fn start(&self, moniker: Moniker) -> Arc<ComponentInstance> {
        self.model
            .root()
            .start_instance(&moniker, &StartReason::Eager)
            .await
            .unwrap_or_else(|e| panic!("could not start {}: {:?}", moniker, e))
    }

    /// Add a dynamic child to the given collection, with the given name to the
    /// component that our proxy member variable corresponds to. Passes no
    /// `CreateChildArgs`.
    pub async fn create_dynamic_child(&self, coll: &str, name: &str) {
        self.create_dynamic_child_with_args(coll, name, fcomponent::CreateChildArgs::default())
            .await
            .expect("failed to create child")
    }

    /// Add a dynamic child to the given collection, with the given name to the
    /// component that our proxy member variable corresponds to.
    pub async fn create_dynamic_child_with_args(
        &self,
        coll: &str,
        name: &str,
        args: fcomponent::CreateChildArgs,
    ) -> Result<(), fcomponent::Error> {
        let collection_ref = fdecl::CollectionRef { name: coll.to_string() };
        let child_decl = ChildDecl {
            name: name.parse().unwrap(),
            url: format!("test:///{}", name).parse().unwrap(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        }
        .native_into_fidl();
        let res = self
            .realm_proxy
            .as_ref()
            .expect("realm service not started")
            .create_child(&collection_ref, &child_decl, args)
            .await;
        res.expect("failed to create child")
    }
}

/// Create a new event stream for the provided environment.
pub async fn new_event_stream(
    builtin_environment: Arc<Mutex<BuiltinEnvironment>>,
    events: Vec<Name>,
) -> (EventSource, EventStream) {
    let mut event_source =
        builtin_environment.as_ref().lock().await.event_source_factory.create_for_above_root();
    let event_stream = event_source
        .subscribe(
            events
                .into_iter()
                .map(|event| EventSubscription {
                    event_name: UseEventStreamDecl {
                        source_name: event,
                        source: UseSource::Parent,
                        scope: None,
                        target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                        filter: None,
                        availability: Availability::Required,
                    },
                })
                .collect(),
        )
        .await
        .expect("subscribe to event stream");
    (event_source, event_stream)
}
