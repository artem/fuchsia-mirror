// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The "Action" concept represents an asynchronous activity on a component that should eventually
//! complete.
//!
//! Actions decouple the "what" of what needs to happen to a component from the "how". Several
//! client APIs may induce operations on a component's state that complete asynchronously. These
//! operations could depend on each other in various ways.
//!
//! A key property of actions is idempotency. If two equal actions are registered on a component,
//! the work for that action is performed only once. This means that two distinct call sites can
//! register the same action, and be guaranteed the work is not repeated.
//!
//! Here are a couple examples:
//! - A `Shutdown` FIDL call must shut down every component instance in the tree, in
//!   dependency order. For this to happen every component must shut down, but not before its
//!   downstream dependencies have shut down.
//! - A `Realm.DestroyChild` FIDL call returns right after a child component is destroyed.
//!   However, in order to actually delete the child, a sequence of events must happen:
//!     * All instances in the component must be shut down (see above)
//!     * The component instance's persistent storage must be erased, if any.
//!     * The component's parent must remove it as a child.
//!
//! Note the interdependencies here -- destroying a component also requires shutdown, for example.
//!
//! These processes could be implemented through a chain of futures in the vicinity of the API
//! call. However, this doesn't scale well, because it requires distributed state handling and is
//! prone to races. Actions solve this problem by allowing client code to just specify the actions
//! that need to eventually be fulfilled. The actual business logic to perform the actions can be
//! implemented by the component itself in a coordinated manner.
//!
//! `DestroyChild()` is an example of how this can work. For simplicity, suppose it's called on a
//! component with no children of its own. This might cause a chain of events like the following:
//!
//! - Before it returns, the `DestroyChild` FIDL handler registers the `DeleteChild` action on the
//!   parent component for child being destroyed.
//! - This results in a call to `Action::handle` for the component. In response to
//!   `DestroyChild`, `Action::handle()` spawns a future that sets a `Destroy` action on the child.
//!   Note that `Action::handle()` is not async, it always spawns any work that might block
//!   in a future.
//! - `Action::handle()` is called on the child. In response to `Destroy`, it sets a `Shutdown`
//!   action on itself (the component instance must be stopped before it is destroyed).
//! - `Action::handle()` is called on the child again, in response to `Shutdown`. It turns out the
//!   instance is still running, so the `Shutdown` future tells the instance to stop. When this
//!   completes, the `Shutdown` action is finished.
//! - The future that was spawned for `Destroy` is notified that `Shutdown` completes, so it cleans
//!   up the instance's resources and finishes the `Destroy` action.
//! - When the work for `Destroy` completes, the future spawned for `DestroyChild` deletes the
//!   child and marks `DestroyChild` finished, which will notify the client that the action is
//!   complete.

mod destroy;
mod discover;
pub mod resolve;
pub mod shutdown;
pub mod start;
mod stop;
mod unresolve;

// Re-export the actions
pub use {
    destroy::DestroyAction, discover::DiscoverAction, resolve::ResolveAction,
    shutdown::ShutdownAction, shutdown::ShutdownType, start::StartAction, stop::StopAction,
    unresolve::UnresolveAction,
};

use {
    crate::model::component::ComponentInstance,
    async_trait::async_trait,
    cm_util::AbortHandle,
    errors::ActionError,
    fuchsia_async as fasync,
    futures::{
        channel::oneshot,
        future::{join_all, BoxFuture, FutureExt, Shared},
        task::{Context, Poll},
        Future,
    },
    std::collections::{HashMap, HashSet},
    std::fmt::Debug,
    std::hash::Hash,
    std::pin::Pin,
    std::sync::Arc,
};

/// A action on a component that must eventually be fulfilled.
#[async_trait]
pub trait Action: Send + Sync + 'static {
    /// Run the action.
    async fn handle(self, component: Arc<ComponentInstance>) -> Result<(), ActionError>;

    /// `key` identifies the action.
    fn key(&self) -> ActionKey;

    /// If the action supports cooperative cancellation, return a handle for this purpose.
    ///
    /// The action may monitor the handle and bail early when it is safe to do so.
    fn abort_handle(&self) -> Option<AbortHandle> {
        None
    }
}

/// A key that uniquely identifies an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKey {
    Discover,
    Resolve,
    Unresolve,
    Start,
    Stop,
    Shutdown,
    Destroy,
}

/// A future bound to a particular action that completes when that action completes.
///
/// Cloning this type will not duplicate the action, but generate another future that waits on the
/// same action.
#[derive(Debug, Clone)]
pub struct ActionNotifier {
    fut: Shared<BoxFuture<'static, Result<(), ActionError>>>,
}

impl ActionNotifier {
    /// Instantiate an `ActionNotifier` that will complete when a message is received on
    /// `receiver`.
    pub fn new(receiver: oneshot::Receiver<Result<(), ActionError>>) -> Self {
        Self {
            fut: receiver
                .map(|res| res.expect("ActionNotifier sender was unexpectedly closed"))
                .boxed()
                .shared(),
        }
    }

    /// Returns the number of references that exist to the shared future in this notifier. Returns
    /// None if the future has completed.
    #[cfg(test)]
    pub fn get_reference_count(&self) -> Option<usize> {
        self.fut.strong_count()
    }
}

impl Future for ActionNotifier {
    type Output = Result<(), ActionError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = Pin::new(&mut self.fut);
        fut.poll(cx)
    }
}

/// Represents a task that implements an action.
pub(crate) struct ActionTask {
    tx: oneshot::Sender<Result<(), ActionError>>,
    fut: BoxFuture<'static, Result<(), ActionError>>,
}

impl ActionTask {
    fn new(
        tx: oneshot::Sender<Result<(), ActionError>>,
        fut: BoxFuture<'static, Result<(), ActionError>>,
    ) -> Self {
        Self { tx, fut }
    }

    /// Runs the action in a separate task and signals the `ActionNotifier` when it completes.
    pub fn spawn(self) {
        fasync::Task::spawn(async move {
            self.tx.send(self.fut.await).unwrap_or(()); // Ignore closed receiver.
        })
        .detach();
    }
}

struct ActionController {
    /// The notifier by which clients will be informed when the action returns.
    notifier: ActionNotifier,
    /// If supported, a handle to abort the action.
    maybe_abort_handle: Option<AbortHandle>,
}

/// A set of actions on a component that must be completed.
///
/// Each action is mapped to a future that returns when the action is complete.
pub struct ActionSet {
    rep: HashMap<ActionKey, ActionController>,
    history: HashSet<ActionKey>,
    passive_waiters: HashMap<ActionKey, Vec<oneshot::Sender<()>>>,
}

impl ActionSet {
    pub fn new() -> Self {
        ActionSet { rep: HashMap::new(), history: HashSet::new(), passive_waiters: HashMap::new() }
    }

    pub async fn contains(&self, key: ActionKey) -> bool {
        self.rep.contains_key(&key)
    }

    #[cfg(test)]
    pub fn mock_result(&mut self, key: ActionKey, result: Result<(), ActionError>) {
        let (sender, receiver) = oneshot::channel();
        sender.send(result).unwrap();
        let notifier = ActionNotifier::new(receiver);
        self.rep.insert(key, ActionController { notifier, maybe_abort_handle: None });
    }

    #[cfg(test)]
    pub fn remove_notifier(&mut self, key: ActionKey) {
        self.rep.remove(&key).expect("No notifier found with that key");
    }

    /// Returns a oneshot receiver that will receive a message once the component has finished
    /// performing an action with the given key. The oneshot will receive a message immediately if
    /// the component has ever finished such an action. Does not cause any new actions to be
    /// started.
    pub async fn wait_for_action(&mut self, action_key: ActionKey) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        if self.history.contains(&action_key) {
            sender.send(()).unwrap();
            receiver
        } else {
            self.passive_waiters.entry(action_key).or_insert(vec![]).push(sender);
            receiver
        }
    }

    /// Registers an action in the set, returning when the action is finished (which may represent
    /// a task that's already running for this action).
    pub async fn register<A>(
        component: Arc<ComponentInstance>,
        action: A,
    ) -> Result<(), ActionError>
    where
        A: Action,
    {
        let rx = {
            let mut actions = component.lock_actions().await;
            actions.register_no_wait(&component, action).await
        };
        rx.await
    }

    /// Registers an action in the set, but does not wait for it to complete, instead returning a
    /// future that can be used to wait on the task. This function is a no-op if the task is
    /// already registered.
    ///
    /// REQUIRES: `self` is the `ActionSet` contained in `component`.
    pub async fn register_no_wait<A>(
        &mut self,
        component: &Arc<ComponentInstance>,
        action: A,
    ) -> ActionNotifier
    where
        A: Action,
    {
        let (task, rx) = self.register_inner(component, action);
        if let Some(task) = task {
            task.spawn();
        }
        rx
    }

    /// Returns a future that waits for the given action to complete, if one exists.
    pub async fn wait<A>(&self, action: A) -> Option<ActionNotifier>
    where
        A: Action,
    {
        let key = action.key();
        self.rep.get(&key).map(|controller| controller.notifier.clone())
    }

    /// Removes an action from the set, completing it.
    async fn finish<'a>(component: &Arc<ComponentInstance>, key: &'a ActionKey) {
        let mut action_set = component.lock_actions().await;
        action_set.rep.remove(key);
        action_set.history.insert(key.clone());
        for sender in action_set.passive_waiters.entry(key.clone()).or_insert(vec![]).drain(..) {
            let _ = sender.send(());
        }
    }

    /// Registers, but does not execute, an action.
    ///
    /// Returns:
    /// - An object that implements the action if it was scheduled for the first time. The caller
    ///   should call spawn() on it.
    /// - A future to listen on the completion of the action.
    #[must_use]
    pub(crate) fn register_inner<'a, A>(
        &'a mut self,
        component: &Arc<ComponentInstance>,
        action: A,
    ) -> (Option<ActionTask>, ActionNotifier)
    where
        A: Action,
    {
        let key = action.key();
        // If this Action is already running, just subscribe to the result
        if let Some(action_controller) = self.rep.get(&key) {
            return (None, action_controller.notifier.clone());
        }

        // Otherwise we spin up the new Action
        let maybe_abort_handle = action.abort_handle();
        let prereqs = self.get_prereq_action(action.key());
        let abort_handles = self.get_abort_action(action.key());

        let component = component.clone();

        let action_fut = async move {
            for abort in abort_handles {
                abort.abort();
            }
            _ = join_all(prereqs).await;
            let key = action.key();
            let res = action.handle(component.clone()).await;
            Self::finish(&component, &key).await;
            res
        }
        .boxed();

        let (notifier_completer, notifier_receiver) = oneshot::channel();
        let task = ActionTask::new(notifier_completer, action_fut);
        let notifier = ActionNotifier::new(notifier_receiver);
        self.rep.insert(
            key.clone(),
            ActionController { notifier: notifier.clone(), maybe_abort_handle },
        );
        (Some(task), notifier)
    }

    /// Return futures that waits for any Action that must be waited on before
    /// executing the target Action. If none is required the returned vector is
    /// empty.
    fn get_prereq_action(&self, key: ActionKey) -> Vec<ActionNotifier> {
        // Start, Stop, and Shutdown are all serialized with respect to one another.
        match key {
            ActionKey::Shutdown => vec![
                self.rep.get(&ActionKey::Stop).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Start).map(|c| c.notifier.clone()),
            ],
            ActionKey::Stop => vec![
                self.rep.get(&ActionKey::Shutdown).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Start).map(|c| c.notifier.clone()),
            ],
            ActionKey::Start => vec![
                self.rep.get(&ActionKey::Stop).map(|c| c.notifier.clone()),
                self.rep.get(&ActionKey::Shutdown).map(|c| c.notifier.clone()),
            ],
            _ => vec![],
        }
        .into_iter()
        .flatten()
        .collect()
    }

    /// Return abort handles for any Action that may be canceled by the target Action.
    ///
    /// This is useful for stopping unnecessary work e.g. if Stop is requested while
    /// Start is running.
    fn get_abort_action(&self, key: ActionKey) -> Vec<AbortHandle> {
        // Stop and Shutdown will attempt to cancel an in-progress Start.
        match key {
            ActionKey::Shutdown | ActionKey::Stop => {
                vec![self
                    .rep
                    .get(&ActionKey::Start)
                    .and_then(|action_controller| action_controller.maybe_abort_handle.clone())]
            }
            _ => vec![],
        }
        .into_iter()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
pub mod tests {
    use {
        super::*, crate::model::testing::test_helpers::ActionsTest, assert_matches::assert_matches,
        errors::StopActionError, fuchsia_async as fasync,
    };

    async fn register_action_in_new_task<A>(
        action: A,
        component: Arc<ComponentInstance>,
        responder: oneshot::Sender<Result<(), ActionError>>,
        res: Result<(), ActionError>,
    ) where
        A: Action,
    {
        let (starter_tx, starter_rx) = oneshot::channel();
        fasync::Task::spawn(async move {
            let mut action_set = component.lock_actions().await;

            // Register action, and get the future. Use `register_inner` so that we can control
            // when to notify the listener.
            let (task, rx) = action_set.register_inner(&component, action);

            // Signal to test that action is registered.
            starter_tx.send(()).unwrap();

            // Drop `action_set` to release the lock.
            drop(action_set);

            if let Some(task) = task {
                // Notify the listeners, but don't actually run the action since this test tests
                // action registration and not the actions themselves.
                task.tx.send(res).unwrap();
            }
            let res = rx.await;

            // If the future completed successfully then we will get to this point.
            responder.send(res).expect("failed to send response");
        })
        .detach();
        starter_rx.await.expect("Unable to receive start signal");
    }

    #[fuchsia::test]
    async fn action_set() {
        let test = ActionsTest::new("root", vec![], None).await;
        let component = test.model.root().clone();

        let (tx1, rx1) = oneshot::channel();
        register_action_in_new_task(DestroyAction::new(), component.clone(), tx1, Ok(())).await;
        let (tx2, rx2) = oneshot::channel();
        register_action_in_new_task(
            ShutdownAction::new(ShutdownType::Instance),
            component.clone(),
            tx2,
            Err(ActionError::StopError { err: StopActionError::GetParentFailed }), // Some random error.
        )
        .await;
        let (tx3, rx3) = oneshot::channel();
        register_action_in_new_task(DestroyAction::new(), component.clone(), tx3, Ok(())).await;

        // Complete actions, while checking notifications.
        ActionSet::finish(&component, &ActionKey::Destroy).await;
        assert_matches!(rx1.await.expect("Unable to receive result of Notification"), Ok(()));
        assert_matches!(rx3.await.expect("Unable to receive result of Notification"), Ok(()));

        ActionSet::finish(&component, &ActionKey::Shutdown).await;
        assert_matches!(
            rx2.await.expect("Unable to receive result of Notification"),
            Err(ActionError::StopError { err: StopActionError::GetParentFailed })
        );
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use {
        crate::model::component::instance::InstanceState,
        crate::model::component::ComponentInstance,
        moniker::{ChildName, MonikerBase},
        routing::component_instance::ComponentInstanceInterface,
    };

    /// Verifies that a child component is deleted by checking its InstanceState and verifying that
    /// it does not exist in the InstanceState of its parent. Assumes the parent is not destroyed
    /// yet.
    pub async fn is_child_deleted(parent: &ComponentInstance, child: &ComponentInstance) -> bool {
        let instanced_moniker =
            child.instanced_moniker().leaf().expect("Root component cannot be destroyed");

        // Verify the parent-child relationship
        assert_eq!(
            parent.instanced_moniker().child(instanced_moniker.clone()),
            *child.instanced_moniker()
        );

        let parent_state = parent.lock_state().await;
        let parent_resolved_state = parent_state.get_resolved_state().expect("not resolved");

        let child_state = child.lock_state().await;
        let found_child = parent_resolved_state.get_child(child.child_moniker().unwrap());

        found_child.is_none() && matches!(*child_state, InstanceState::Destroyed)
    }

    pub async fn is_stopped(component: &ComponentInstance, moniker: &ChildName) -> bool {
        if let Some(resolved_state) = component.lock_state().await.get_resolved_state() {
            if let Some(child) = resolved_state.get_child(moniker) {
                return !child.is_started().await;
            }
        }
        false
    }

    pub async fn is_destroyed(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Destroyed)
    }

    pub async fn is_resolved(component: &ComponentInstance) -> bool {
        component.lock_state().await.get_resolved_state().is_some()
    }

    pub async fn is_discovered(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Unresolved(_))
    }

    pub async fn is_shutdown(component: &ComponentInstance) -> bool {
        let state = component.lock_state().await;
        matches!(*state, InstanceState::Shutdown(_, _))
    }
}
