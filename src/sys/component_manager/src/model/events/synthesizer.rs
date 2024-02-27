// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, InstanceState},
        error::ModelError,
        events::{dispatcher::EventDispatcherScope, event::Event, registry::ComponentEventRoute},
        hooks::{Event as HookEvent, EventType},
        model::Model,
    },
    ::routing::event::EventFilter,
    async_trait::async_trait,
    cm_types::Name,
    cm_util::TaskGroup,
    futures::{channel::mpsc, future::join_all, stream, SinkExt, StreamExt},
    moniker::{ExtendedMoniker, Moniker, MonikerBase},
    std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Weak},
    },
    tracing::error,
};

/// A component instance or component manager itself
pub enum ExtendedComponent {
    ComponentManager,
    ComponentInstance(Arc<ComponentInstance>),
}

impl From<Arc<ComponentInstance>> for ExtendedComponent {
    fn from(instance: Arc<ComponentInstance>) -> Self {
        Self::ComponentInstance(instance)
    }
}

impl From<&Arc<ComponentInstance>> for ExtendedComponent {
    fn from(instance: &Arc<ComponentInstance>) -> Self {
        Self::ComponentInstance(instance.clone())
    }
}

/// Implementors of this trait know how to synthesize an event.
#[async_trait]
pub trait EventSynthesisProvider: Send + Sync {
    /// Provides a synthesized event applying the given `filter` under the given `component`.
    async fn provide(&self, component: ExtendedComponent, filter: &EventFilter) -> Vec<HookEvent>;
}

/// Synthesis manager.
pub struct EventSynthesizer {
    /// A reference to the model.
    model: Weak<Model>,

    /// Maps an event name to the provider for synthesis
    providers: HashMap<Name, Arc<dyn EventSynthesisProvider>>,
}

impl EventSynthesizer {
    /// Creates a new event synthesizer.
    pub fn new(model: Weak<Model>) -> Self {
        Self { model, providers: HashMap::new() }
    }

    /// Registers a new provider that will be used when synthesizing events of the type `event`.
    pub fn register_provider(
        &mut self,
        event: EventType,
        provider: Arc<dyn EventSynthesisProvider>,
    ) {
        self.providers.insert(event.into(), provider);
    }

    /// Spawns a synthesis task for the requested `events`. Resulting events will be sent on the
    /// `sender` channel.
    pub async fn spawn_synthesis(
        &self,
        sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        events: HashMap<Name, Vec<EventDispatcherScope>>,
        scope: &TaskGroup,
    ) {
        SynthesisTask::new(&self, sender, events).spawn(scope).await
    }
}

/// Information about an event that will be synthesized.
struct EventSynthesisInfo {
    /// The provider of the synthesized event.
    provider: Arc<dyn EventSynthesisProvider>,

    /// The scopes under which the event will be synthesized.
    scopes: Vec<EventDispatcherScope>,
}

struct SynthesisTask {
    /// A reference to the model.
    model: Weak<Model>,

    /// The sender end of the channel where synthesized events will be sent.
    sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,

    /// Information about the events to synthesize
    event_infos: Vec<EventSynthesisInfo>,
}

impl SynthesisTask {
    /// Creates a new synthesis task from the given events. It will ignore events for which the
    /// `synthesizer` doesn't have a provider.
    pub fn new(
        synthesizer: &EventSynthesizer,
        sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        mut events: HashMap<Name, Vec<EventDispatcherScope>>,
    ) -> Self {
        let event_infos = synthesizer
            .providers
            .iter()
            .filter_map(|(event_name, provider)| {
                events
                    .remove(event_name)
                    .map(|scopes| EventSynthesisInfo { provider: provider.clone(), scopes })
            })
            .collect();
        Self { model: synthesizer.model.clone(), sender, event_infos }
    }

    /// Spawns a task that will synthesize all events that were requested when creating the
    /// `SynthesisTask`
    pub async fn spawn(self, scope: &TaskGroup) {
        if self.event_infos.is_empty() {
            return;
        }
        scope.spawn(async move {
            // If we can't find the component then we can't synthesize events.
            // This isn't necessarily an error as the model or component might've been
            // destroyed in the intervening time, so we just exit early.
            if let Some(model) = self.model.upgrade() {
                let sender = self.sender;
                let futs = self
                    .event_infos
                    .into_iter()
                    .map(|event_info| Self::run(&model, sender.clone(), event_info));
                for result in join_all(futs).await {
                    if let Err(error) = result {
                        error!(?error, "Event synthesis failed");
                    }
                }
            }
        });
    }

    /// Performs a depth-first traversal of the component instance tree. It adds to the stream a
    /// `Running` event for all components that are running. In the case of overlapping scopes,
    /// events are deduped.  It also synthesizes events that were requested which are synthesizable
    /// (there's a provider for them). Those events will only be synthesized if their scope is
    /// within the scope of a Running scope.
    async fn run(
        model: &Arc<Model>,
        mut sender: mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
        info: EventSynthesisInfo,
    ) -> Result<(), ModelError> {
        let mut visited_components = HashSet::new();
        for scope in &info.scopes {
            // If the scope is component manager, synthesize the builtin events first and then
            // proceed to synthesize from the root and down.
            let scope_moniker = match scope.moniker {
                ExtendedMoniker::ComponentManager => {
                    // Ignore this error. This can occur when the event stream is closed in the
                    // middle of synthesis. We can finish synthesizing if an error happens.
                    if let Err(_) = Self::send_events(
                        &info.provider,
                        &scope,
                        ExtendedComponent::ComponentManager,
                        &mut sender,
                    )
                    .await
                    {
                        return Ok(());
                    }
                    Moniker::root()
                }
                ExtendedMoniker::ComponentInstance(ref scope_moniker) => scope_moniker.clone(),
            };
            let root = model.root().find_and_maybe_resolve(&scope_moniker).await?;
            let mut component_stream = get_subcomponents(root, visited_components.clone());
            let mut tasks = vec![];
            while let Some(component) = component_stream.next().await {
                visited_components.insert(component.moniker.clone());
                let provider = info.provider.clone();
                let scope = scope.clone();
                let component = component.clone();
                let mut sender = sender.clone();
                tasks.push(fuchsia_async::Task::spawn(async move {
                    let _ = Self::send_events(
                        &provider,
                        &scope,
                        ExtendedComponent::ComponentInstance(component),
                        &mut sender,
                    )
                    .await;
                }));
            }
            futures::future::join_all(tasks).await;
        }
        Ok(())
    }

    async fn send_events(
        provider: &Arc<dyn EventSynthesisProvider>,
        scope: &EventDispatcherScope,
        target_component: ExtendedComponent,
        sender: &mut mpsc::UnboundedSender<(Event, Option<Vec<ComponentEventRoute>>)>,
    ) -> Result<(), anyhow::Error> {
        let events = provider.provide(target_component, &scope.filter).await;
        for event in events {
            let event = Event { event, scope_moniker: scope.moniker.clone() };
            sender.send((event, None)).await?;
        }
        Ok(())
    }
}

/// Returns all components that are under the given `root` component. Skips the ones whose moniker
/// is contained in the `visited` set.  The visited set is included for early pruning of a tree
/// branch.
fn get_subcomponents(
    root: Arc<ComponentInstance>,
    visited: HashSet<Moniker>,
) -> stream::BoxStream<'static, Arc<ComponentInstance>> {
    let pending = vec![root];
    stream::unfold((pending, visited), move |(mut pending, mut visited)| async move {
        loop {
            match pending.pop() {
                None => return None,
                Some(curr_component) => {
                    if visited.contains(&curr_component.moniker) {
                        continue;
                    }
                    let state_guard = curr_component.lock_state().await;
                    match *state_guard {
                        InstanceState::New
                        | InstanceState::Unresolved(_)
                        | InstanceState::Destroyed => {}
                        InstanceState::Resolved(ref s) => {
                            for (_, child) in s.children() {
                                pending.push(child.clone());
                            }
                        }
                    }
                    drop(state_guard);
                    visited.insert(curr_component.moniker.clone());
                    return Some((curr_component, (pending, visited)));
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::{
            component::WeakExtendedInstance,
            events::{
                registry::{EventRegistry, RoutedEvent},
                stream::EventStream,
            },
            hooks::EventPayload,
            testing::routing_test_helpers::*,
        },
        cm_rust::{CapabilityDecl, ExposeDecl, ExposeDirectoryDecl, ExposeSource, ExposeTarget},
        cm_rust_testing::*,
        fidl_fuchsia_io as fio,
        routing::component_instance::ComponentInstanceInterface,
        std::collections::HashSet,
        vfs::pseudo_directory,
    };

    struct CreateStreamArgs<'a> {
        registry: &'a EventRegistry,
        scope_monikers: Vec<Moniker>,
        events: Vec<EventType>,
    }

    #[fuchsia::test]
    async fn synthesize_directory_ready() {
        let test = setup_synthesis_test().await;

        let registry = test.builtin_environment.event_registry.clone();
        let mut event_stream = create_stream(
            &test,
            CreateStreamArgs {
                registry: &registry,
                scope_monikers: vec![vec!["b"].try_into().unwrap()],
                events: vec![EventType::DirectoryReady],
            },
        )
        .await;

        let mut instances_with_diag_dirs = HashSet::<ExtendedMoniker>::from([
            ExtendedMoniker::from(Moniker::try_from(vec!["b"]).unwrap()),
            ExtendedMoniker::from(Moniker::try_from(vec!["b", "c"]).unwrap()),
            ExtendedMoniker::from(Moniker::try_from(vec!["b", "d"]).unwrap()),
        ]);

        for instance in &instances_with_diag_dirs {
            let ExtendedMoniker::ComponentInstance(ref m) = instance else {
                continue;
            };
            test.start_instance(m).await.unwrap();
        }

        while !instances_with_diag_dirs.is_empty() {
            let (event, _) = event_stream.next().await.unwrap();
            match event.event.payload {
                EventPayload::DirectoryReady { name, .. } if name == "diagnostics" => {
                    instances_with_diag_dirs.remove(&event.event.target_moniker);
                }
                payload => panic!("Expected running or directory ready. Got: {:?}", payload),
            }
        }

        assert!(instances_with_diag_dirs.is_empty());
    }

    async fn create_stream<'a>(test: &RoutingTest, args: CreateStreamArgs<'a>) -> EventStream {
        let scopes = args
            .scope_monikers
            .into_iter()
            .map(|moniker| EventDispatcherScope {
                moniker: moniker.into(),
                filter: EventFilter::debug(),
            })
            .collect::<Vec<_>>();
        let events = args
            .events
            .into_iter()
            .map(|event| RoutedEvent {
                source_name: event.into(),
                scopes: scopes.clone(),
                route: vec![],
            })
            .collect();
        args.registry
            .subscribe_with_routed_events(
                &WeakExtendedInstance::Component(test.model.root().as_weak()),
                events,
            )
            .await
            .expect("subscribe to event stream")
    }

    /// Construct topology for the test. Puts a diagnostics directory in b, c, and d's namespaces.
    async fn setup_synthesis_test() -> RoutingTest {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(diagnostics_decl())
                    .expose(expose_diagnostics_decl())
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(diagnostics_decl())
                    .expose(expose_diagnostics_decl())
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(diagnostics_decl())
                    .expose(expose_diagnostics_decl())
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .capability(diagnostics_decl())
                    .expose(expose_diagnostics_decl())
                    .build(),
            ),
        ];
        RoutingTestBuilder::new("a", components)
            .add_outgoing_path("b", "/diagnostics".parse().unwrap(), pseudo_directory! {})
            .add_outgoing_path("c", "/diagnostics".parse().unwrap(), pseudo_directory! {})
            .add_outgoing_path("d", "/diagnostics".parse().unwrap(), pseudo_directory! {})
            .build()
            .await
    }

    fn diagnostics_decl() -> CapabilityDecl {
        CapabilityBuilder::directory().name("diagnostics").path("/diagnostics").build()
    }

    fn expose_diagnostics_decl() -> ExposeDecl {
        ExposeDecl::Directory(ExposeDirectoryDecl {
            source: ExposeSource::Self_,
            source_name: "diagnostics".parse().unwrap(),
            source_dictionary: None,
            target_name: "diagnostics".parse().unwrap(),
            target: ExposeTarget::Framework,
            rights: Some(fio::Operations::CONNECT),
            subdir: None,
            availability: cm_rust::Availability::Required,
        })
    }
}
