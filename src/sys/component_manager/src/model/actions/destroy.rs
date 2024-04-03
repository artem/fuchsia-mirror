// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        actions::{
            resolve::sandbox_construction::ComponentInput, Action, ActionKey, ActionSet,
            DiscoverAction, ResolveAction, ShutdownAction, ShutdownType, StartAction,
        },
        component::{ComponentInstance, IncomingCapabilities, InstanceState, StartReason},
        error::{ActionError, DestroyActionError},
        hooks::{Event, EventPayload},
    },
    ::routing::component_instance::ExtendedInstanceInterface,
    async_trait::async_trait,
    futures::{
        future::{join_all, BoxFuture},
        Future,
    },
    moniker::MonikerBase,
    std::sync::Arc,
};

/// Destroy this component instance, including all instances nested in its component.
pub struct DestroyAction {}

impl DestroyAction {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Action for DestroyAction {
    async fn handle(self, component: &Arc<ComponentInstance>) -> Result<(), ActionError> {
        do_destroy(component).await.map_err(Into::into)
    }
    fn key(&self) -> ActionKey {
        ActionKey::Destroy
    }
}

async fn do_destroy(component: &Arc<ComponentInstance>) -> Result<(), ActionError> {
    loop {
        // Do nothing if already destroyed.
        {
            if let InstanceState::Destroyed = *component.lock_state().await {
                return Ok(());
            }
        }

        // Require the component to be discovered before deleting it so a Destroyed event is
        // always preceded by a Discovered.
        // TODO: wait for a discover, don't register a new one
        ActionSet::register(component.clone(), DiscoverAction::new(ComponentInput::default()))
            .await?;

        // For destruction to behave correctly, the component has to be shut down first.
        // NOTE: This will recursively shut down the whole subtree. If this component has children,
        // we'll call DestroyChild on them which in turn will call Shutdown on the child. Because
        // the parent's subtree was shutdown, this shutdown is a no-op.
        ActionSet::register(component.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .map_err(|e| DestroyActionError::ShutdownFailed { err: Box::new(e) })?;

        let nfs = {
            match *component.lock_state().await {
                InstanceState::Shutdown(ref state, _) => {
                    let mut nfs = vec![];
                    for (m, c) in state.children.iter() {
                        let component = component.clone();
                        let m = m.clone();
                        let incarnation = c.incarnation_id();
                        let nf = async move { component.destroy_child(m, incarnation).await };
                        nfs.push(nf);
                    }
                    nfs
                }
                InstanceState::Unresolved(_) | InstanceState::Resolved(_) => {
                    // The instance is not shut down, we must have raced with an unresolve action
                    // (potentially followed by a resolve action). Let's try again.
                    continue;
                }
                InstanceState::New => {
                    panic!("discover action returned above but the component is undiscovered, this should be impossible");
                }
                InstanceState::Destroyed => {
                    panic!(
                        "component was destroyed earlier but is not now, this should be impossible"
                    );
                }
            }
        };
        let results = join_all(nfs).await;
        ok_or_first_error(results)?;

        // Now that all children have been destroyed, destroy the parent.
        component.destroy_instance().await?;

        // Wait for any remaining blocking tasks and actions finish up.
        fn wait(nf: Option<impl Future + Send + 'static>) -> BoxFuture<'static, ()> {
            Box::pin(async {
                if let Some(nf) = nf {
                    nf.await;
                }
            })
        }
        let task_shutdown = Box::pin(component.blocking_task_group().join());
        let nfs = {
            let actions = component.lock_actions().await;
            vec![
                wait(actions.wait(ResolveAction::new())),
                wait(actions.wait(StartAction::new(
                    StartReason::Debug,
                    None,
                    IncomingCapabilities::default(),
                ))),
                task_shutdown,
            ]
        };
        join_all(nfs.into_iter()).await;

        // Only consider the component fully destroyed once it's no longer executing any lifecycle
        // transitions.
        component.lock_state().await.set(InstanceState::Destroyed);

        // Send the Destroyed event for the component
        let event = Event::new(&component, EventPayload::Destroyed);
        component.hooks.dispatch(&event).await;

        // Remove this component from the parent's list of children
        if let Some(child_name) = component.moniker.leaf() {
            if let Ok(ExtendedInstanceInterface::Component(parent)) = component.parent.upgrade() {
                match *parent.lock_state().await {
                    InstanceState::Resolved(ref mut resolved_state) => {
                        resolved_state.remove_child(child_name);
                    }
                    InstanceState::Shutdown(ref mut state, _) => {
                        state.children.remove(child_name);
                    }
                    _ => (),
                }
            }
        }

        return Ok(());
    }
}

fn ok_or_first_error(results: Vec<Result<(), ActionError>>) -> Result<(), ActionError> {
    results.into_iter().fold(Ok(()), |acc, r| acc.and_then(|_| r))
}

#[cfg(test)]
pub mod tests {
    use {
        super::*,
        crate::model::{
            actions::test_utils::{is_child_deleted, is_destroyed},
            testing::{
                test_helpers::{
                    component_decl_with_test_runner, execution_is_shut_down, get_incarnation_id,
                    has_child, ActionsTest,
                },
                test_hook::Lifecycle,
            },
        },
        cm_rust_testing::*,
        fuchsia_async as fasync, fuchsia_zircon as zx,
        futures::{channel::mpsc, StreamExt},
        moniker::{ChildName, Moniker},
        std::sync::atomic::Ordering,
    };

    #[fuchsia::test]
    async fn destroy_one_component() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        // Start the component. This should cause the component to have an `Execution`.
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started());

        // Register shutdown first because DestroyChild requires the component to be shut down.
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        // Destroy the child, and wait for it. Component should be destroyed.
        component_root.destroy_child("a".try_into().unwrap(), 0).await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ],
            );
        }

        // Trying to start the component should fail because it's shut down.
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect_err("successfully bound to a after shutdown");

        // Destroy the component again. This succeeds, but has no additional effect.
        component_root.destroy_child("a".try_into().unwrap(), 0).await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_collection() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("container").build()),
            ("container", ComponentDeclBuilder::new().collection_default("coll").build()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];
        let test =
            ActionsTest::new("root", components, Some(vec!["container"].try_into().unwrap())).await;

        // Create dynamic instances in "coll".
        test.create_dynamic_child("coll", "a").await;
        test.create_dynamic_child("coll", "b").await;

        // Start the components. This should cause them to have an `Execution`.
        let component_root = test.model.root();
        let component_container = test.look_up(vec!["container"].try_into().unwrap()).await;
        let component_a = test.look_up(vec!["container", "coll:a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["container", "coll:b"].try_into().unwrap()).await;
        component_root
            .start_instance(&component_container.moniker, &StartReason::Eager)
            .await
            .expect("could not start container");
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:a");
        component_root
            .start_instance(&component_b.moniker, &StartReason::Eager)
            .await
            .expect("could not start coll:b");
        assert!(component_container.is_started());
        assert!(component_a.is_started());
        assert!(component_b.is_started());

        // Destroy the child, and wait for it. Components should be destroyed.
        let component_container = test.look_up(vec!["container"].try_into().unwrap()).await;
        component_root
            .destroy_child("container".try_into().unwrap(), 0)
            .await
            .expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_container).await);
        assert!(is_destroyed(&component_container).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
    }

    #[fuchsia::test]
    async fn destroy_already_shut_down() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;

        // Register shutdown action on "a", and wait for it. This should cause all components
        // to shut down, in bottom-up order.
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        assert!(execution_is_shut_down(&component_a.clone()).await);
        assert!(execution_is_shut_down(&component_b.clone()).await);

        // Now delete child "a". This should cause all components to be destroyed.
        component_root.destroy_child("a".try_into().unwrap(), 0).await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);

        // Check order of events.
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap()),
                ]
            );
        }
    }

    // An action that blocks until it receives a value on an mpsc channel.
    pub struct MockAction {
        rx: mpsc::Receiver<()>,
        key: ActionKey,
        result: Result<(), ActionError>,
    }

    impl MockAction {
        pub fn new(key: ActionKey, result: Result<(), ActionError>) -> (Self, mpsc::Sender<()>) {
            let (tx, rx) = mpsc::channel::<()>(0);
            let action = Self { rx, key, result };
            (action, tx)
        }
    }

    #[async_trait]
    impl Action for MockAction {
        async fn handle(mut self, _: &Arc<ComponentInstance>) -> Result<(), ActionError> {
            self.rx.next().await.unwrap();
            self.result
        }

        fn key(&self) -> ActionKey {
            self.key.clone()
        }
    }

    async fn run_destroy_waits_test(
        mock_action_key: ActionKey,
        mock_action_result: Result<(), ActionError>,
    ) {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        test.model.start(ComponentInput::default()).await;

        let component_root = test.model.root().clone();
        let component_a = match *component_root.lock_state().await {
            InstanceState::Resolved(ref s) => {
                s.get_child(&ChildName::try_from("a").unwrap()).expect("child a not found").clone()
            }
            _ => panic!("not resolved"),
        };

        let (mock_action, mut mock_action_unblocker) =
            MockAction::new(mock_action_key.clone(), mock_action_result);

        // Spawn a mock action on 'a' that stalls
        {
            let mut actions = component_a.lock_actions().await;
            let _ = actions.register_no_wait(&component_a, mock_action);
        }

        // Spawn a task to destroy the child `a` under root.
        // This eventually leads to a destroy action on `a`.
        let component_root_clone = component_root.clone();
        let destroy_child_fut = fasync::Task::spawn(async move {
            component_root_clone.destroy_child("a".try_into().unwrap(), 0).await
        });

        // Check that the destroy action is waiting on the mock action.
        loop {
            let actions = component_a.lock_actions().await;
            assert!(actions.contains(&mock_action_key));

            // Check the reference count on the notifier of the mock action
            let rx = &actions.rep[&mock_action_key];
            let refcount = rx.refcount.load(Ordering::Relaxed);

            // expected reference count:
            // - 1 for the ActionSet that owns the notifier
            // - 1 for destroy action to wait on the mock action
            if refcount == 2 {
                assert!(actions.contains(&ActionKey::Destroy));
                break;
            }

            // The destroy action hasn't blocked on the mock action yet.
            // Wait for that to happen and check again.
            drop(actions);
            fasync::Timer::new(fasync::Time::after(zx::Duration::from_millis(100))).await;
        }

        // Unblock the mock action, causing destroy to complete as well
        mock_action_unblocker.try_send(()).unwrap();
        destroy_child_fut.await.unwrap();
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_waits_on_discover() {
        run_destroy_waits_test(
            ActionKey::Discover,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(()),
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_waits_on_resolve() {
        run_destroy_waits_test(
            ActionKey::Resolve,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(()),
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_waits_on_start() {
        run_destroy_waits_test(
            ActionKey::Start,
            // The mocked action must return a result, even though the result is not used
            // by the Destroy action.
            Ok(()),
        )
        .await;
    }

    #[fuchsia::test]
    async fn destroy_marks_destroyed_after_blocking_tasks() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        test.model.start(ComponentInput::default()).await;

        let component_root = test.model.root().clone();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;

        // Run a blocking task that panics if the component has Destroyed state.
        // The task does the check once it receives a value on the `task_start` channel.
        let (mut task_start_tx, mut task_start_rx) = mpsc::channel::<()>(0);
        let (mut task_done_tx, mut task_done_rx) = mpsc::channel::<()>(0);
        let a = component_a.clone();
        let fut = async move {
            task_start_rx.next().await;
            if matches!(*a.lock_state().await, InstanceState::Destroyed) {
                panic!("component state was set to destroyed before blocking task finished");
            }
            task_done_tx.try_send(()).unwrap();
        };
        component_a.blocking_task_group().spawn(fut);

        let mock_action_key = ActionKey::Start;
        let (mock_action, mut mock_action_unblocker) =
            MockAction::new(mock_action_key.clone(), Ok(()));

        // Spawn a mock action on 'a' that stalls
        {
            let mut actions = component_a.lock_actions().await;
            let _ = actions.register_no_wait(&component_a, mock_action);
        }

        // Spawn a task to destroy the child `a` under root.
        // This eventually leads to a destroy action on `a`.
        let component_root_clone = component_root.clone();
        let destroy_child_fut = fasync::Task::spawn(async move {
            component_root_clone.destroy_child("a".try_into().unwrap(), 0).await
        });

        // Check that the destroy action is waiting on the mock action.
        loop {
            let actions = component_a.lock_actions().await;
            assert!(actions.contains(&mock_action_key));

            // Check the reference count on the notifier of the mock action
            let rx = &actions.rep[&mock_action_key];
            let refcount = rx.refcount.load(Ordering::Relaxed);

            // expected reference count:
            // - 1 for the ActionSet that owns the notifier
            // - 1 for destroy action to wait on the mock action
            if refcount == 2 {
                assert!(actions.contains(&ActionKey::Destroy));
                break;
            }

            // The destroy action hasn't blocked on the mock action yet.
            // Wait for that to happen and check again.
            drop(actions);
            fasync::Timer::new(fasync::Time::after(zx::Duration::from_millis(100))).await;
        }

        // Now that the Destroy action is waiting on the Start action, it should also
        // be waiting on the blocking task, so start the blocking task to verity instance state.
        task_start_tx.try_send(()).unwrap();

        // Wait for the blocking task to finish. It should finish without panicking.
        task_done_rx.next().await;

        // Unblock the mock action, causing destroy to complete as well
        mock_action_unblocker.try_send(()).unwrap();

        destroy_child_fut.await.unwrap();
        assert!(is_child_deleted(&component_root, &component_a).await);
    }

    #[fuchsia::test]
    async fn destroy_not_resolved() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", ComponentDeclBuilder::new().child_default("c").build()),
            ("c", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started());
        // Get component_b without resolving it.
        let component_b = match *component_a.lock_state().await {
            InstanceState::Resolved(ref s) => {
                s.get_child(&ChildName::try_from("b").unwrap()).expect("child b not found").clone()
            }
            _ => panic!("not resolved"),
        };

        // Register destroy action on "a", and wait for it.
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        component_root.destroy_child("a".try_into().unwrap(), 0).await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_b).await);

        // Now "a" is destroyed. Expect destroy events for "a" and "b".
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    ///  Delete "a" as child of root:
    ///
    ///  /\
    /// x  a*
    ///     \
    ///      b
    ///     / \
    ///    c   d
    #[fuchsia::test]
    async fn destroy_hierarchy() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").child_default("x").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager().build())
                    .child(ChildBuilder::new().name("d").eager().build())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
            ("x", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;
        let component_x = test.look_up(vec!["x"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        component_root
            .start_instance(&component_x.moniker, &StartReason::Eager)
            .await
            .expect("could not start x");
        assert!(component_a.is_started());
        assert!(component_b.is_started());
        assert!(component_c.is_started());
        assert!(component_d.is_started());
        assert!(component_x.is_started());

        // Register destroy action on "a", and wait for it. This should cause all components
        // in "a"'s component to be shut down and destroyed, in bottom-up order, but "x" is still
        // running.
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        component_root
            .destroy_child("a".try_into().unwrap(), 0)
            .await
            .expect("delete child failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(is_destroyed(&component_d).await);
        assert!(component_x.is_started());
        {
            // Expect only "x" as child of root.
            let state = component_root.lock_state().await;
            let children: Vec<_> = match *state {
                InstanceState::Resolved(ref s) => s.children().map(|(k, _)| k.clone()).collect(),
                _ => {
                    panic!("not resolved");
                }
            };
            assert_eq!(children, vec!["x".try_into().unwrap()]);
        }
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();

            // The leaves could be stopped in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            assert_eq!(
                first,
                vec![
                    Lifecycle::Stop(vec!["a", "b", "c"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a", "b", "d"].try_into().unwrap())
                ]
            );
            let next: Vec<_> = events.drain(0..2).collect();
            assert_eq!(
                next,
                vec![
                    Lifecycle::Stop(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a"].try_into().unwrap())
                ]
            );

            // The leaves could be destroyed in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            assert_eq!(
                first,
                vec![
                    Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b", "d"].try_into().unwrap())
                ]
            );
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Destroy `b`:
    ///  a
    ///   \
    ///    b
    ///     \
    ///      b
    ///       \
    ///      ...
    ///
    /// `b` is a child of itself, but destruction should still be able to complete.
    #[fuchsia::test]
    async fn destroy_self_referential() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", ComponentDeclBuilder::new().child_default("b").build()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_b2 = test.look_up(vec!["a", "b", "b"].try_into().unwrap()).await;

        // Start the second `b`.
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        component_root
            .start_instance(&component_b.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        component_root
            .start_instance(&component_b2.moniker, &StartReason::Eager)
            .await
            .expect("could not start b2");
        assert!(component_a.is_started());
        assert!(component_b.is_started());
        assert!(component_b2.is_started());

        // Register destroy action on "a", and wait for it. This should cause all components
        // that were started to be destroyed, in bottom-up order.
        ActionSet::register(component_a.clone(), ShutdownAction::new(ShutdownType::Instance))
            .await
            .expect("shutdown failed");
        component_root
            .destroy_child("a".try_into().unwrap(), 0)
            .await
            .expect("delete child failed");
        assert!(is_child_deleted(&component_root, &component_a).await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_b2).await);
        {
            let state = component_root.lock_state().await;
            let children: Vec<_> = match *state {
                InstanceState::Resolved(ref s) => s.children().map(|(k, _)| k.clone()).collect(),
                _ => panic!("not resolved"),
            };
            assert_eq!(children, Vec::<ChildName>::new());
        }
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["a", "b", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Stop(vec!["a"].try_into().unwrap()),
                    // This component instance is never resolved but we still invoke the Destroy
                    // hook on it.
                    Lifecycle::Destroy(vec!["a", "b", "b", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    /// Destroy `a`:
    ///
    ///    a*
    ///     \
    ///      b
    ///     / \
    ///    c   d
    ///
    /// `a` fails to destroy the first time, but succeeds the second time.
    #[fuchsia::test]
    async fn destroy_error() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").eager().build())
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("c").eager().build())
                    .child(ChildBuilder::new().name("d").eager().build())
                    .build(),
            ),
            ("c", component_decl_with_test_runner()),
            ("d", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let component_root = test.model.root();
        let component_a = test.look_up(vec!["a"].try_into().unwrap()).await;
        let component_b = test.look_up(vec!["a", "b"].try_into().unwrap()).await;
        let component_c = test.look_up(vec!["a", "b", "c"].try_into().unwrap()).await;
        let component_d = test.look_up(vec!["a", "b", "d"].try_into().unwrap()).await;

        // Component startup was eager, so they should all have an `Execution`.
        component_root
            .start_instance(&component_a.moniker, &StartReason::Eager)
            .await
            .expect("could not start a");
        assert!(component_a.is_started());
        assert!(component_b.is_started());
        assert!(component_c.is_started());
        assert!(component_d.is_started());

        // Mock a failure to delete "d".
        {
            let mut actions = component_d.lock_actions().await;
            actions.mock_result(
                ActionKey::Destroy,
                Err(ActionError::DestroyError {
                    err: DestroyActionError::InstanceNotFound {
                        moniker: component_d.moniker.clone(),
                    },
                }) as Result<(), ActionError>,
            );
        }

        component_b
            .destroy_child("d".try_into().unwrap(), 0)
            .await
            .expect_err("d's destroy succeeded unexpectedly");

        // Register delete action on "a", and wait for it. but "d"
        // returns an error so the delete action on "a" does not succeed.
        //
        // In this state, "d" is marked destroyed but hasn't been removed from the
        // children list of "b". "c" is destroyed and has been removed from the children
        // list of "b".
        component_root
            .destroy_child("a".try_into().unwrap(), 0)
            .await
            .expect_err("destroy succeeded unexpectedly");
        assert!(has_child(&component_root, "a").await);
        assert!(has_child(&component_a, "b").await);
        assert!(!has_child(&component_b, "c").await);
        assert!(has_child(&component_b, "d").await);
        assert!(!is_destroyed(&component_a).await);
        assert!(!is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(!is_destroyed(&component_d).await);
        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            let expected: Vec<_> =
                vec![Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap())];
            assert_eq!(events, expected);
        }

        // Remove the mock from "d"
        {
            let mut actions = component_d.lock_actions().await;
            actions.remove_notifier(ActionKey::Destroy);
        }

        // Register destroy action on "a" again. "d"'s delete succeeds, and "a" is deleted
        // this time.
        component_root.destroy_child("a".try_into().unwrap(), 0).await.expect("destroy failed");
        assert!(!has_child(&component_root, "a").await);
        assert!(is_destroyed(&component_a).await);
        assert!(is_destroyed(&component_b).await);
        assert!(is_destroyed(&component_c).await);
        assert!(is_destroyed(&component_d).await);
        {
            let mut events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            // The leaves could be stopped in any order.
            let mut first: Vec<_> = events.drain(0..2).collect();
            first.sort_unstable();
            let expected: Vec<_> = vec![
                Lifecycle::Destroy(vec!["a", "b", "c"].try_into().unwrap()),
                Lifecycle::Destroy(vec!["a", "b", "d"].try_into().unwrap()),
            ];
            assert_eq!(first, expected);
            assert_eq!(
                events,
                vec![
                    Lifecycle::Destroy(vec!["a", "b"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["a"].try_into().unwrap())
                ]
            );
        }
    }

    #[fuchsia::test]
    async fn destroy_runs_after_new_instance_created() {
        // We want to demonstrate calling destroy child for the same child instance, which should
        // be idempotent, works correctly if a new instance of the child under the same name is
        // created between them.
        let components = vec![
            ("root", ComponentDeclBuilder::new().collection_default("coll").build()),
            ("a", component_decl_with_test_runner()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, Some(Moniker::root())).await;

        // Create dynamic instance in "coll".
        test.create_dynamic_child("coll", "a").await;

        // Start the component so we can witness it getting stopped.
        test.start(vec!["coll:a"].try_into().unwrap()).await;

        // We're going to run the destroy action for `a` twice. One after the other finishes, so
        // the actions semantics don't dedup them to the same work item.
        let component_root = test.look_up(Moniker::root()).await;
        let component_root_clone = component_root.clone();
        let destroy_fut_1 = fasync::Task::spawn(async move {
            component_root_clone.destroy_child("coll:a".try_into().unwrap(), 1).await
        });
        let component_root_clone = component_root.clone();
        let destroy_fut_2 = fasync::Task::spawn(async move {
            component_root_clone.destroy_child("coll:a".try_into().unwrap(), 1).await
        });

        let component_a = test.look_up(vec!["coll:a"].try_into().unwrap()).await;
        assert!(!is_child_deleted(&component_root, &component_a).await);

        destroy_fut_1.await.expect("destroy failed");
        assert!(is_child_deleted(&component_root, &component_a).await);

        // Now recreate `a`
        test.create_dynamic_child("coll", "a").await;
        test.start(vec!["coll:a"].try_into().unwrap()).await;

        // Run the second destroy fut, it should leave the newly created `a` alone
        destroy_fut_2.await.expect("destroy failed");
        let component_a = test.look_up(vec!["coll:a"].try_into().unwrap()).await;
        assert_eq!(get_incarnation_id(&component_root, "coll:a").await, 2);
        assert!(!is_child_deleted(&component_root, &component_a).await);

        {
            let events: Vec<_> = test
                .test_hook
                .lifecycle()
                .into_iter()
                .filter(|e| match e {
                    Lifecycle::Stop(_) | Lifecycle::Destroy(_) => true,
                    _ => false,
                })
                .collect();
            assert_eq!(
                events,
                vec![
                    Lifecycle::Stop(vec!["coll:a"].try_into().unwrap()),
                    Lifecycle::Destroy(vec!["coll:a"].try_into().unwrap()),
                ],
            );
        }
    }
}
