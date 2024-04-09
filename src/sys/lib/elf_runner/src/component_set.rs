// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{ElfComponent, ElfComponentInfo};
use fuchsia_async as fasync;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use id::Id;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

/// [`ComponentSet`] tracks all the components executing inside an ELF runner,
/// and presents an iterator over those components. It does this under the
/// constraint that each component may go out of scope concurrently due to
/// stopping on its own, or being stopped by the `ComponentController` protocol
/// or any other reason.
#[derive(Default)]
pub struct ComponentSet {
    inner: Mutex<HashMap<Id, Weak<ElfComponentInfo>>>,

    /// Callbacks for component insertion and removal.
    ///
    /// These callbacks need to be behind a [Mutex] as it isn't guaranteed they
    /// will be called on the same thread as [ComponentSet].
    callbacks: Mutex<ComponentSetCallbacks>,
}

#[derive(Default)]
struct ComponentSetCallbacks {
    on_new_component: Option<Box<dyn Fn(&ElfComponentInfo) -> () + Send>>,
    on_removed_component: Option<Box<dyn Fn(zx::Event) -> () + Send>>,
}

impl ComponentSet {
    pub fn new() -> Arc<Self> {
        Default::default()
    }

    pub fn set_callbacks(
        &self,
        on_new_component: Option<Box<dyn Fn(&ElfComponentInfo) -> () + Send>>,
        on_removed_component: Option<Box<dyn Fn(zx::Event) -> () + Send>>,
    ) {
        let mut locked_callbacks = self.callbacks.lock();
        locked_callbacks.on_new_component = on_new_component;
        locked_callbacks.on_removed_component = on_removed_component;
    }

    /// Adds a component to the set.
    ///
    /// The component will remove itself from the set when it is dropped.
    pub fn add(self: Arc<Self>, component: &mut ElfComponent) {
        let mut inner = self.inner.lock();
        let component_set = Arc::downgrade(&self.clone());
        let id = Id::new(component.info().get_moniker().clone());
        let id_clone = id.clone();
        component.set_on_drop(ComponentSet::on_component_drop(component_set, id_clone));
        inner.insert(id, Arc::downgrade(component.info()));
        if let Some(cb) = self.callbacks.lock().on_new_component.as_ref() {
            cb(component.info().as_ref());
        }
    }

    /// Invokes `visitor` over all [`ElfComponentInfo`] objects corresponding to
    /// components that are currently running. Note that this is fundamentally racy
    /// as a component could be stopping imminently during or after the visit.
    pub fn visit(&self, mut visitor: impl FnMut(&ElfComponentInfo, Id)) {
        // Cloning the map helps us avoid holding the lock when iterating over it, in
        // particular while calling the callback.
        let components = &self.inner.lock().clone();
        for (id, component) in components.iter() {
            let Some(component) = component.upgrade() else {
                continue;
            };
            visitor(&component, id.clone());
        }
    }

    /// Callback when a component is removed from the set.
    fn on_component_drop(
        component_set: Weak<ComponentSet>,
        id_clone: Id,
    ) -> impl FnOnce(&ElfComponentInfo) {
        move |info| {
            let token = info.copy_instance_token().unwrap();
            fasync::Task::spawn(async move {
                let Some(component_set) = component_set.upgrade() else {
                    return;
                };
                {
                    let mut locked_inner = component_set.inner.lock();
                    locked_inner.remove(&id_clone);
                }
                if let Some(cb) = component_set.callbacks.lock().on_removed_component.as_ref() {
                    cb(token);
                };
            })
            .detach()
        }
    }
}

/// An identifier for running components that is unique within an ELF runner.
pub mod id {
    use moniker::Moniker;
    use std::fmt::Display;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// TODO(https://fxbug.dev/316036032): store a moniker token instead, to eliminate
    /// ELF runner's visibility into component monikers.
    #[derive(Eq, Hash, PartialEq, Clone, Debug)]
    pub struct Id(Moniker, u64);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl Id {
        pub fn new(moniker: Moniker) -> Id {
            Id(moniker, NEXT_ID.fetch_add(1, Ordering::SeqCst))
        }
    }

    impl Display for Id {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_fmt(format_args!("{}, {}", self.0, self.1))
        }
    }

    impl From<Id> for String {
        fn from(value: Id) -> Self {
            format!("{value}")
        }
    }

    impl TryFrom<String> for Id {
        type Error = anyhow::Error;

        fn try_from(value: String) -> Result<Self, Self::Error> {
            let Some((moniker, counter)) = value.split_once(", ") else {
                anyhow::bail!("Expected comma separated string, got {value}");
            };
            let moniker: Moniker = moniker.parse()?;
            let counter: u64 = counter.parse()?;
            Ok(Self(moniker, counter))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_get_id() {
            let id1 = Id::new(Moniker::try_from("foo/bar").unwrap());
            let id2 = Id::new(Moniker::try_from("foo/bar").unwrap());
            assert_ne!(id1, id2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_zircon as zx;
    use futures::FutureExt;
    use moniker::Moniker;
    use std::{
        future,
        sync::atomic::{AtomicUsize, Ordering},
        task::Poll,
    };

    use crate::{runtime_dir::RuntimeDirectory, Job};

    #[test]
    fn test_add_remove_component() {
        // Use a test executor so that we can run until stalled.
        let mut exec = fasync::TestExecutor::new();
        let components = ComponentSet::new();

        // The component set starts out empty.
        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        components_clone.visit(|_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // After adding, it should contain one component.
        let mut fake_component = make_fake_component();
        components.clone().add(&mut fake_component);

        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        components_clone.visit(|_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // After dropping that component, it should eventually contain zero components.
        drop(fake_component);
        let mut fut = async {
            let _: Poll<()> =
                fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        }
        .boxed();
        assert!(exec.run_until_stalled(&mut fut).is_ready());

        let count = Arc::new(AtomicUsize::new(0));
        let components_clone = components.clone();
        components_clone.visit(|_, _| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    fn make_fake_component() -> ElfComponent {
        let runtime_dir = RuntimeDirectory::empty();
        let job = Job::Single(fuchsia_runtime::job_default().create_child_job().unwrap());
        let process = fuchsia_runtime::process_self().duplicate(zx::Rights::SAME_RIGHTS).unwrap();
        let lifecycle_channel = None;
        let main_process_critical = false;
        let tasks = vec![];
        let component_url = "hello".to_string();
        let fake_component = ElfComponent::new(
            runtime_dir,
            Moniker::default(),
            job,
            process,
            lifecycle_channel,
            main_process_critical,
            tasks,
            component_url,
            None,
            Default::default(),
            zx::Event::create(),
        );
        fake_component
    }
}
