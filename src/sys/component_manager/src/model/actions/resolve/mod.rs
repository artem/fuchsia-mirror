// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod sandbox_construction;

use {
    crate::model::{
        actions::{Action, ActionKey},
        component::instance::{InstanceState, ResolvedInstanceState},
        component::ComponentInstance,
        component::{Component, WeakComponentInstance},
        error::{ActionError, ResolveActionError},
        hooks::{Event, EventPayload},
        resolver::Resolver,
    },
    ::routing::{component_instance::ComponentInstanceInterface, resolving::ComponentAddress},
    async_trait::async_trait,
    cm_util::io::clone_dir,
    std::{ops::DerefMut, sync::Arc},
};

/// Resolves a component instance's declaration and initializes its state.
pub struct ResolveAction {}

impl ResolveAction {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Action for ResolveAction {
    async fn handle(self, component: &Arc<ComponentInstance>) -> Result<(), ActionError> {
        do_resolve(component).await.map_err(Into::into)
    }
    fn key(&self) -> ActionKey {
        ActionKey::Resolve
    }
}

async fn do_resolve(component: &Arc<ComponentInstance>) -> Result<(), ResolveActionError> {
    {
        let state = component.lock_state().await;
        if state.is_shut_down() {
            return Err(ResolveActionError::InstanceShutDown {
                moniker: component.moniker.clone(),
            });
        }
    }
    // Ensure `Resolved` is dispatched after `Discovered`.
    {
        let discover_completed =
            component.lock_actions().await.wait_for_action(ActionKey::Discover);
        discover_completed.await.unwrap();
    }
    let result = async move {
        let first_resolve = {
            let state = component.lock_state().await;
            match *state {
                InstanceState::New => {
                    panic!("Component should be at least discovered")
                }
                InstanceState::Unresolved(_) => true,
                InstanceState::Resolved(_) | InstanceState::Started(_, _) => false,
                InstanceState::Shutdown(_, _) => {
                    return Err(ResolveActionError::InstanceShutDown {
                        moniker: component.moniker.clone(),
                    });
                }
                InstanceState::Destroyed => {
                    return Err(ResolveActionError::InstanceDestroyed {
                        moniker: component.moniker.clone(),
                    });
                }
            }
        };
        let component_url = &component.component_url;
        let component_address =
            ComponentAddress::from(component_url, component).await.map_err(|err| {
                ResolveActionError::ComponentAddressParseError {
                    url: component.component_url.clone(),
                    moniker: component.moniker.clone(),
                    err,
                }
            })?;
        let component_info =
            component.environment.resolve(&component_address).await.map_err(|err| {
                ResolveActionError::ResolverError { url: component.component_url.clone(), err }
            })?;
        let component_info =
            Component::resolve_with_config(component_info, component.config_parent_overrides())?;
        let policy = component.context.abi_revision_policy();
        policy
            .check_compatibility(
                &version_history::HISTORY,
                &component.moniker,
                component_info.abi_revision,
            )
            .map_err(|err| ResolveActionError::AbiCompatibilityError {
                url: component_url.clone(),
                err,
            })?;
        if first_resolve {
            {
                let mut state = component.lock_state().await;
                let (instance_token_state, component_input_dict) = match state.deref_mut() {
                    InstanceState::Resolved(_) => {
                        panic!("Component was marked Resolved during Resolve action?");
                    }
                    InstanceState::Started(_, _) => {
                        panic!("Component was marked Started during Resolve action?");
                    }
                    InstanceState::Shutdown(_, _) => {
                        return Err(ResolveActionError::InstanceShutDown {
                            moniker: component.moniker.clone(),
                        });
                    }
                    InstanceState::New => {
                        panic!("Component was not marked Discovered before Resolve action?");
                    }
                    InstanceState::Destroyed => {
                        return Err(ResolveActionError::InstanceDestroyed {
                            moniker: component.moniker.clone(),
                        });
                    }
                    InstanceState::Unresolved(unresolved_state) => unresolved_state.to_resolved(),
                };
                let resolved_state = ResolvedInstanceState::new(
                    component,
                    component_info.clone(),
                    component_address,
                    instance_token_state,
                    component_input_dict,
                )
                .await?;
                state.set(InstanceState::Resolved(resolved_state));
            }
        }
        Ok((component_info, first_resolve))
    }
    .await;

    let (component_info, first_resolve) = result?;

    if !first_resolve {
        return Ok(());
    }

    let event = Event::new(
        component,
        EventPayload::Resolved {
            component: WeakComponentInstance::from(component),
            decl: component_info.decl.clone(),
            package_dir: component_info
                .package
                .as_ref()
                .and_then(|pkg| clone_dir(Some(&pkg.package_dir))),
        },
    );
    component.hooks.dispatch(&event).await;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use {
        crate::model::{
            actions::test_utils::{is_resolved, is_stopped},
            actions::{
                ActionSet, ResolveAction, ShutdownAction, ShutdownType, StartAction, StopAction,
            },
            component::{IncomingCapabilities, StartReason},
            error::{ActionError, ResolveActionError},
            testing::test_helpers::{component_decl_with_test_runner, ActionsTest},
        },
        assert_matches::assert_matches,
        cm_rust_testing::ComponentDeclBuilder,
        moniker::{Moniker, MonikerBase},
    };

    /// Check unresolve for _nonrecursive_ case. The system has a root with the child `a` and `a`
    /// has descendants as shown in the diagram below.
    ///  a
    ///   \
    ///    b
    ///
    /// Also tests UnresolveAction on InstanceState::Unresolved.
    #[fuchsia::test]
    async fn resolve_action_test() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        // Resolve and start the components.
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.look_up(Moniker::root()).await;
        let component_a = test.start(vec!["a"].try_into().unwrap()).await;
        assert!(component_a.is_started().await);
        assert!(is_resolved(&component_root).await);
        assert!(is_resolved(&component_a).await);

        // Stop, then it's ok to resolve again.
        ActionSet::register(component_a.clone(), StopAction::new(false)).await.unwrap();
        assert!(is_resolved(&component_a).await);
        assert!(is_stopped(&component_root, &"a".try_into().unwrap()).await);

        ActionSet::register(component_a.clone(), ResolveAction::new()).await.unwrap();
        assert!(is_resolved(&component_a).await);
        assert!(is_stopped(&component_root, &"a".try_into().unwrap()).await);

        // Start it again then shut it down.
        ActionSet::register(
            component_a.clone(),
            StartAction::new(StartReason::Debug, None, IncomingCapabilities::default()),
        )
        .await
        .unwrap();
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .unwrap();

        // Error to resolve a shut-down component.
        assert_matches!(
            ActionSet::register(component_a.clone(), ResolveAction::new()).await,
            Err(ActionError::ResolveError { err: ResolveActionError::InstanceShutDown { .. } })
        );
        assert!(!is_resolved(&component_a).await);
        assert!(is_stopped(&component_root, &"a".try_into().unwrap()).await);
    }
}
