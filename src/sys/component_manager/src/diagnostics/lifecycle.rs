// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    errors::ModelError,
    fuchsia_inspect as inspect, fuchsia_inspect_contrib as inspect_contrib,
    fuchsia_sync::Mutex,
    fuchsia_zircon as zx,
    hooks::{Event, EventPayload, EventType, HasEventType, Hook, HooksRegistration},
    moniker::Moniker,
    std::sync::{Arc, Weak},
};

const MAX_NUMBER_OF_LIFECYCLE_EVENTS: usize = 150;
const MONIKER: &str = "moniker";
const TYPE: &str = "type";
const STARTED: &str = "started";
const STOPPED: &str = "stopped";
const TIME: &str = "time";
const EARLY: &str = "early";
const LATE: &str = "late";

/// Tracks start and stop timestamps of components.
pub struct ComponentLifecycleTimeStats {
    // Keeps the inspect node alive.
    _node: inspect::Node,
    inner: Mutex<Inner>,
}

/// `early` maintains the first `MAX_NUMBER_OF_LIFECYCLE_EVENTS` start/stop events of
/// components. After more than `MAX_NUMBER_OF_LIFECYCLE_EVENTS` events have occurred,
/// `early` will stay unchanged, and `late` will maintain the the last
/// `MAX_NUMBER_OF_LIFECYCLE_EVENTS` start/stop events of components. When more events are
/// added, the earliest ones in `late` will be discarded. This enables our feedback
/// snapshots to contain a recent history of started and stopped components.
struct Inner {
    early: inspect_contrib::nodes::BoundedListNode,
    late: inspect_contrib::nodes::BoundedListNode,
}

impl Inner {
    fn new(early: inspect::Node, late: inspect::Node) -> Self {
        let early =
            inspect_contrib::nodes::BoundedListNode::new(early, MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        let late =
            inspect_contrib::nodes::BoundedListNode::new(late, MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        Self { early, late }
    }

    fn add_entry(&mut self, moniker: &Moniker, kind: &str, time: zx::Time) {
        let node =
            if self.early.len() < self.early.capacity() { &mut self.early } else { &mut self.late };
        node.add_entry(|node| {
            node.record_string(MONIKER, moniker.to_string());
            node.record_string(TYPE, kind);
            node.record_int(TIME, time.into_nanos());
        });
    }
}

impl ComponentLifecycleTimeStats {
    /// Creates a new startup time tracker. Data will be written to the given inspect node.
    pub fn new(node: inspect::Node) -> Self {
        let early = node.create_child(EARLY);
        let late = node.create_child(LATE);
        Self { _node: node, inner: Mutex::new(Inner::new(early, late)) }
    }

    /// Provides the hook events that are needed to work.
    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "ComponentLifecycleTimeStats",
            vec![EventType::Started, EventType::Stopped],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    fn on_component_started(self: &Arc<Self>, moniker: &Moniker, start_time: zx::Time) {
        self.inner.lock().add_entry(moniker, STARTED, start_time);
    }

    fn on_component_stopped(self: &Arc<Self>, moniker: &Moniker, stop_time: zx::Time) {
        self.inner.lock().add_entry(moniker, STOPPED, stop_time);
    }
}

#[async_trait]
impl Hook for ComponentLifecycleTimeStats {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match event.event_type() {
            EventType::Started => {
                if let EventPayload::Started { runtime, .. } = &event.payload {
                    self.on_component_started(target_moniker, runtime.start_time);
                }
            }
            EventType::Stopped => {
                if let EventPayload::Stopped { stop_time, .. } = &event.payload {
                    self.on_component_stopped(target_moniker, *stop_time);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        component::{ComponentInstance, StartReason},
        testing::test_helpers::{component_decl_with_test_runner, ActionsTest},
    };
    use cm_rust_testing::ComponentDeclBuilder;
    use diagnostics_assertions::{assert_data_tree, AnyProperty};
    use fuchsia_inspect::DiagnosticsHierarchyGetter;
    use itertools::Itertools;
    use moniker::{ChildName, ChildNameBase, MonikerBase};

    #[fuchsia::test]
    async fn tracks_events() {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", component_decl_with_test_runner()),
        ];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        let inspector = inspect::Inspector::default();
        let stats =
            Arc::new(ComponentLifecycleTimeStats::new(inspector.root().create_child("lifecycle")));
        root.hooks.install(stats.hooks()).await;

        let root_timestamp = start_and_get_timestamp(root, &Moniker::root()).await.into_nanos();
        let a_timestamp = start_and_get_timestamp(root, &"a".parse().unwrap()).await.into_nanos();
        let b_timestamp = start_and_get_timestamp(root, &"a/b".parse().unwrap()).await.into_nanos();
        root.find(&"a/b".parse().unwrap()).await.unwrap().stop().await.unwrap();

        assert_data_tree!(inspector, root: {
            lifecycle: {
                early: {
                    "0": {
                        moniker: ".",
                        time: root_timestamp,
                        "type": "started",
                    },
                    "1": {
                        moniker: "a",
                        time: a_timestamp,
                        "type": "started",
                    },
                    "2": {
                        moniker: "a/b",
                        time: b_timestamp,
                        "type": "started",
                    },
                    "3": contains {
                        moniker: "a/b",
                        "type": "stopped",
                        time: AnyProperty,
                    }
                },
                late: {
                }
            }
        });
    }

    #[fuchsia::test]
    async fn early_doesnt_track_more_than_limit() {
        let inspector = inspect::Inspector::default();
        let stats =
            Arc::new(ComponentLifecycleTimeStats::new(inspector.root().create_child("lifecycle")));

        for i in 0..2 * MAX_NUMBER_OF_LIFECYCLE_EVENTS {
            stats.on_component_started(
                &Moniker::new(vec![ChildName::parse(format!("{}", i)).unwrap()]),
                zx::Time::from_nanos(i as i64),
            );
        }

        let hierarchy = inspector.get_diagnostics_hierarchy();
        let node = &hierarchy.children[0];
        let early = node.children.iter().find_or_first(|c| c.name == "early").unwrap();
        assert_eq!(early.children.len(), MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        assert_eq!(
            early.children.iter().map(|c| c.name.parse::<i32>().unwrap()).sorted().last().unwrap(),
            149
        );
    }

    #[fuchsia::test]
    async fn early_overflow_to_late() {
        let inspector = inspect::Inspector::default();
        let stats =
            Arc::new(ComponentLifecycleTimeStats::new(inspector.root().create_child("lifecycle")));

        for i in 0..MAX_NUMBER_OF_LIFECYCLE_EVENTS + 1 {
            stats.on_component_started(
                &Moniker::new(vec![ChildName::parse(format!("{}", i)).unwrap()]),
                zx::Time::from_nanos(i as i64),
            );
        }

        let hierarchy = inspector.get_diagnostics_hierarchy();
        let node = &hierarchy.children[0];
        let early = node.children.iter().find_or_first(|c| c.name == "early").unwrap();
        let late = node.children.iter().find_or_first(|c| c.name == "late").unwrap();
        assert_eq!(early.children.len(), MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        assert_eq!(
            early.children.iter().map(|c| c.name.parse::<i32>().unwrap()).sorted().last().unwrap(),
            149
        );
        assert_eq!(late.children.len(), 1);
        assert_data_tree!(late, late: {
            "0": contains {
                moniker: "150",
                "type": "started",
            }
        });
    }

    #[fuchsia::test]
    async fn late_doesnt_track_more_than_limit() {
        let inspector = inspect::Inspector::default();
        let stats =
            Arc::new(ComponentLifecycleTimeStats::new(inspector.root().create_child("lifecycle")));

        for i in 0..4 * MAX_NUMBER_OF_LIFECYCLE_EVENTS {
            stats.on_component_started(
                &Moniker::new(vec![ChildName::parse(format!("{}", i)).unwrap()]),
                zx::Time::from_nanos(i as i64),
            );
        }

        let hierarchy = inspector.get_diagnostics_hierarchy();
        let node = &hierarchy.children[0];
        let early = node.children.iter().find_or_first(|c| c.name == "early").unwrap();
        let late = node.children.iter().find_or_first(|c| c.name == "late").unwrap();
        assert_eq!(early.children.len(), MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        assert_eq!(late.children.len(), MAX_NUMBER_OF_LIFECYCLE_EVENTS);
        assert_eq!(
            late.children.iter().map(|c| c.name.parse::<i32>().unwrap()).sorted().last().unwrap(),
            449
        );
    }

    async fn start_and_get_timestamp(
        root_component: &Arc<ComponentInstance>,
        moniker: &Moniker,
    ) -> zx::Time {
        let component = root_component
            .start_instance(moniker, &StartReason::Root)
            .await
            .expect("failed to bind");
        let state = component.lock_state().await;
        state.get_started_state().unwrap().timestamp
    }
}
