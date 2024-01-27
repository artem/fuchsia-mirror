// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{resolve::sandbox_construction::ComponentInput, ActionKey, DiscoverAction},
        component::{
            ComponentInstance, ComponentManagerInstance, IncomingCapabilities, InstanceState,
            StartReason,
        },
        context::ModelContext,
        environment::Environment,
        error::ModelError,
        token::InstanceRegistry,
    },
    cm_config::RuntimeConfig,
    moniker::{Moniker, MonikerBase},
    std::sync::Arc,
    tracing::warn,
};

/// Parameters for initializing a component model, particularly the root of the component
/// instance tree.
pub struct ModelParams {
    // TODO(viktard): Merge into RuntimeConfig
    /// The URL of the root component.
    pub root_component_url: String,
    /// The environment provided to the root.
    pub root_environment: Environment,
    /// Global runtime configuration for the component_manager.
    pub runtime_config: Arc<RuntimeConfig>,
    /// The instance at the top of the tree, representing component manager.
    pub top_instance: Arc<ComponentManagerInstance>,
}

/// The component model holds authoritative state about a tree of component instances, including
/// each instance's identity, lifecycle, capabilities, and topological relationships.  It also
/// provides operations for instantiating, destroying, querying, and controlling component
/// instances at runtime.
pub struct Model {
    /// The instance at the top of the tree, i.e. the instance representing component manager
    /// itself.
    top_instance: Arc<ComponentManagerInstance>,
    /// The instance representing the root component. Owned by `top_instance`, but cached here for
    /// efficiency.
    root: Arc<ComponentInstance>,
    /// The context shared across the model.
    context: Arc<ModelContext>,
}

impl Model {
    /// Creates a new component model and initializes its topology.
    pub async fn new(
        params: ModelParams,
        instance_registry: Arc<InstanceRegistry>,
    ) -> Result<Arc<Model>, ModelError> {
        let context = Arc::new(ModelContext::new(params.runtime_config, instance_registry)?);
        let root = ComponentInstance::new_root(
            params.root_environment,
            context.clone(),
            Arc::downgrade(&params.top_instance),
            params.root_component_url,
        );
        let model =
            Arc::new(Model { root: root.clone(), context, top_instance: params.top_instance });
        model.top_instance.init(root).await;
        Ok(model)
    }

    /// Returns a reference to the instance at the top of the tree (component manager's own
    /// instance).
    pub fn top_instance(&self) -> &Arc<ComponentManagerInstance> {
        &self.top_instance
    }

    /// Returns a reference to the root component instance.
    pub fn root(&self) -> &Arc<ComponentInstance> {
        &self.root
    }

    pub fn context(&self) -> &ModelContext {
        &self.context
    }

    pub fn component_id_index(&self) -> &component_id_index::Index {
        self.context.component_id_index()
    }

    /// Looks up a component by moniker. The component instance in the component will be
    /// resolved if that has not already happened.
    pub async fn find_and_maybe_resolve(
        &self,
        look_up_moniker: &Moniker,
    ) -> Result<Arc<ComponentInstance>, ModelError> {
        let mut cur = self.root.clone();
        for moniker in look_up_moniker.path().iter() {
            cur = {
                let cur_state = cur.lock_resolved_state().await?;
                if let Some(c) = cur_state.get_child(moniker) {
                    c.clone()
                } else {
                    return Err(ModelError::instance_not_found(look_up_moniker.clone()));
                }
            };
        }
        cur.lock_resolved_state().await?;
        Ok(cur)
    }

    /// Finds a component matching the moniker, if such a component exists.
    /// This function has no side-effects.
    pub async fn find(&self, look_up_moniker: &Moniker) -> Option<Arc<ComponentInstance>> {
        let mut cur = self.root.clone();
        for moniker in look_up_moniker.path().iter() {
            cur = {
                let state = cur.lock_state().await;
                match &*state {
                    InstanceState::Resolved(r) => {
                        if let Some(c) = r.get_child(moniker) {
                            c.clone()
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                }
            };
        }
        Some(cur)
    }

    /// Finds a resolved component matching the moniker, if such a component exists.
    /// This function has no side-effects.
    #[cfg(test)]
    pub async fn find_resolved(&self, find_moniker: &Moniker) -> Option<Arc<ComponentInstance>> {
        let mut cur = self.root.clone();
        for moniker in find_moniker.path().iter() {
            cur = {
                let state = cur.lock_state().await;
                match &*state {
                    InstanceState::Resolved(r) => match r.get_child(moniker) {
                        Some(c) => c.clone(),
                        _ => return None,
                    },
                    _ => return None,
                }
            };
        }
        // Found the moniker, the last child in the chain of resolved parents. Is it resolved?
        let state = cur.lock_state().await;
        match &*state {
            crate::model::component::InstanceState::Resolved(_) => Some(cur.clone()),
            _ => None,
        }
    }

    /// Discovers the root component, providing it with `dict_for_root`.
    pub async fn discover_root_component(self: &Arc<Model>, input_for_root: ComponentInput) {
        let mut actions = self.root.lock_actions().await;
        // This returns a Future that does not need to be polled.
        let _ = actions.register_no_wait(&self.root, DiscoverAction::new(input_for_root));
    }

    /// Starts root, starting the component tree. If `discover_root_component` has already been
    /// called, then `dict_for_root` is unused.
    pub async fn start(self: &Arc<Model>, input_for_root: ComponentInput) {
        // Normally the Discovered event is dispatched when an instance is added as a child, but
        // since the root isn't anyone's child we need to dispatch it here.
        self.discover_root_component(input_for_root).await;

        // In debug mode, we don't start the component root. It must be started manually from
        // the lifecycle controller.
        if self.context.runtime_config().debug {
            warn!(
                "In debug mode, the root component will not be started. Use the LifecycleController
                protocol to start the root component."
            );
        } else {
            if let Err(e) =
                self.root.start(&StartReason::Root, None, IncomingCapabilities::default()).await
            {
                // `root.start` may take a long time as it will be resolving and starting
                // eager children. If graceful shutdown is initiated, that will cause those
                // children to fail to resolve or fail to start, and for `start` to fail.
                //
                // If we fail to start the root, but the root is being shutdown, or already
                // shutdown, that's ok. The system is tearing down, so it doesn't matter any more
                // if we never got everything started that we wanted to.
                let action_set = self.root.lock_actions().await;
                if !action_set.contains(&ActionKey::Shutdown) {
                    if !self.root.lock_execution().await.is_shut_down() {
                        panic!(
                            "failed to start root component {}: {:?}",
                            self.root.component_url, e
                        );
                    }
                }
            }
        }
    }

    /// Starts the component instance in the given component if it's not already running.
    /// Returns the component that was bound to.
    #[cfg(test)]
    pub async fn start_instance<'a>(
        self: &Arc<Model>,
        moniker: &'a Moniker,
        reason: &StartReason,
    ) -> Result<Arc<ComponentInstance>, ModelError> {
        let component = self.find_and_maybe_resolve(moniker).await?;
        component.start(reason, None, IncomingCapabilities::default()).await?;
        Ok(component)
    }
}

#[cfg(test)]
pub mod tests {
    use {
        crate::model::{
            actions::test_utils::is_discovered,
            actions::{
                resolve::sandbox_construction::ComponentInput, ActionSet, ShutdownAction,
                ShutdownType, UnresolveAction,
            },
            error::ModelError,
            hooks::{Event, EventType, Hook, HooksRegistration},
            model::Model,
            testing::test_helpers::{
                component_decl_with_test_runner, ActionsTest, TestEnvironmentBuilder,
                TestModelResult,
            },
        },
        assert_matches::assert_matches,
        async_trait::async_trait,
        cm_rust_testing::ComponentDeclBuilder,
        fidl_fuchsia_component_decl as fdecl,
        moniker::{Moniker, MonikerBase},
        std::sync::{Arc, Weak},
    };

    #[fuchsia::test]
    async fn already_shut_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .add_child(cm_rust::ChildDecl {
                    name: "bad-scheme".to_string(),
                    url: "bad-scheme://sdf".to_string(),
                    startup: fdecl::StartupMode::Eager,
                    environment: None,
                    on_terminate: None,
                    config_overrides: None,
                })
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        let _ =
            ActionSet::register(model.root.clone(), ShutdownAction::new(ShutdownType::Instance))
                .await
                .unwrap();

        model.start(ComponentInput::empty()).await;
    }

    #[fuchsia::test]
    async fn shutting_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .add_child(cm_rust::ChildDecl {
                    name: "bad-scheme".to_string(),
                    url: "bad-scheme://sdf".to_string(),
                    startup: fdecl::StartupMode::Eager,
                    environment: None,
                    on_terminate: None,
                    config_overrides: None,
                })
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        // Wait for the child to be discovered -- this happens in the middle of starting
        // the root component (eagerly starting children), then register the shutdown
        // action. This means the root will already be scheduled for shutdown after
        // start inevitably fails.
        struct RegisterShutdown {
            model: Arc<Model>,
        }

        #[async_trait]
        impl Hook for RegisterShutdown {
            async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
                if event.target_moniker != "bad-scheme".parse::<Moniker>().unwrap().into() {
                    return Ok(());
                }
                let _ =
                    self.model.root().lock_actions().await.register_inner(
                        &self.model.root,
                        ShutdownAction::new(ShutdownType::Instance),
                    );
                Ok(())
            }
        }

        let root = model.find(&Moniker::default()).await.unwrap();
        let hook = Arc::new(RegisterShutdown { model: model.clone() });
        root.hooks
            .install(vec![HooksRegistration::new(
                "shutdown_root_on_child_discover",
                vec![EventType::Discovered],
                Arc::downgrade(&hook) as Weak<dyn Hook>,
            )])
            .await;

        model.start(ComponentInput::empty()).await;
    }

    #[should_panic]
    #[fuchsia::test]
    async fn not_shutting_down_when_start_fails() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new()
                .add_child(cm_rust::ChildDecl {
                    name: "bad-scheme".to_string(),
                    url: "bad-scheme://sdf".to_string(),
                    startup: fdecl::StartupMode::Eager,
                    environment: None,
                    on_terminate: None,
                    config_overrides: None,
                })
                .build(),
        )];

        let TestModelResult { model, .. } =
            TestEnvironmentBuilder::new().set_components(components).build().await;

        model.start(ComponentInput::empty()).await;
    }

    #[fuchsia::test]
    async fn find_resolved_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().add_lazy_child("a").build()),
            ("a", ComponentDeclBuilder::new().add_eager_child("b").build()),
            ("b", ComponentDeclBuilder::new().add_eager_child("c").add_eager_child("d").build()),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;

        // Not resolved, so not found.
        assert_matches!(test.model.find_resolved(&vec!["a"].try_into().unwrap()).await, None);
        assert_matches!(test.model.find_resolved(&vec!["a", "b"].try_into().unwrap()).await, None);
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await,
            None
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await,
            None
        );

        // Resolve each component.
        test.look_up(Moniker::root()).await;
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Now they can all be found.
        assert_matches!(test.model.find_resolved(&vec!["a"].try_into().unwrap()).await, Some(_));
        assert_eq!(
            test.model.find_resolved(&vec!["a"].try_into().unwrap()).await.unwrap().component_url,
            "test:///a",
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b"].try_into().unwrap()).await,
            Some(_)
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await,
            Some(_)
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await,
            Some(_)
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "nonesuch"].try_into().unwrap()).await,
            None
        );

        // Unresolve, recursively.
        ActionSet::register(component_a.clone(), UnresolveAction::new())
            .await
            .expect("unresolve failed");

        // Unresolved recursively, so children in Discovered state.
        assert!(is_discovered(&component_a).await);
        assert!(is_discovered(&component_b).await);
        assert!(is_discovered(&component_c).await);
        assert!(is_discovered(&component_d).await);

        assert_matches!(test.model.find_resolved(&vec!["a"].try_into().unwrap()).await, None);
        assert_matches!(test.model.find_resolved(&vec!["a", "b"].try_into().unwrap()).await, None);
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "c"].try_into().unwrap()).await,
            None
        );
        assert_matches!(
            test.model.find_resolved(&vec!["a", "b", "d"].try_into().unwrap()).await,
            None
        );
    }
}
