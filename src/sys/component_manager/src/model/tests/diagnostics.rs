// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module tests the diagnostics code with the actual component hierarchy.
#[cfg(test)]
mod tests {
    use {
        crate::model::{
            component::{testing::wait_until_event_get_timestamp, ComponentInstance, StartReason},
            events::registry::EventSubscription,
            start::Start,
            testing::routing_test_helpers::RoutingTest,
            testing::test_helpers::{component_decl_with_test_runner, ActionsTest, ComponentInfo},
        },
        cm_rust::{Availability, UseEventStreamDecl, UseSource},
        cm_rust_testing::*,
        cm_types::Name,
        diagnostics::escrow::DurationStats,
        diagnostics::lifecycle::ComponentLifecycleTimeStats,
        diagnostics_assertions::{assert_data_tree, AnyProperty, HistogramAssertion},
        diagnostics_hierarchy::DiagnosticsHierarchy,
        fidl::endpoints::ServerEnd,
        fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_inspect as inspect,
        fuchsia_inspect::DiagnosticsHierarchyGetter,
        fuchsia_sync as fsync, fuchsia_zircon as zx,
        fuchsia_zircon::AsHandleRef,
        futures::{channel::mpsc, StreamExt},
        hooks::EventType,
        inspect::{ExponentialHistogramParams, LinearHistogramParams},
        moniker::Moniker,
        std::future,
        std::sync::Arc,
    };

    fn get_data(
        hierarchy: &DiagnosticsHierarchy,
        moniker: &str,
        task: Option<&str>,
    ) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let mut path = vec!["stats", "measurements", "components", moniker];
        if let Some(task) = task {
            path.push(task);
        }
        get_data_at(&hierarchy, &path)
    }

    fn get_data_at(
        hierarchy: &DiagnosticsHierarchy,
        path: &[&str],
    ) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let node = hierarchy.get_child_by_path(&path).expect("found stats node");
        let cpu_times = node
            .get_property("cpu_times")
            .expect("found cpu")
            .int_array()
            .expect("cpu are ints")
            .raw_values();
        let queue_times = node
            .get_property("queue_times")
            .expect("found queue")
            .int_array()
            .expect("queue are ints")
            .raw_values();
        let timestamps = node
            .get_property("timestamps")
            .expect("found timestamps")
            .int_array()
            .expect("timestamps are ints")
            .raw_values();
        (timestamps.into_owned(), cpu_times.into_owned(), queue_times.into_owned())
    }

    #[fuchsia::test]
    async fn component_manager_stats_are_tracked() {
        // Set up the test
        let test = RoutingTest::new(
            "root",
            vec![(
                "root",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("a").eager().build())
                    .build(),
            )],
        )
        .await;

        let koid =
            fuchsia_runtime::job_default().basic_info().expect("got basic info").koid.raw_koid();

        let hierarchy = test.builtin_environment.inspector().get_diagnostics_hierarchy();
        let (timestamps, cpu_times, queue_times) =
            get_data(&hierarchy, "<component_manager>", Some(&koid.to_string()));
        assert_eq!(timestamps.len(), 1);
        assert_eq!(cpu_times.len(), 1);
        assert_eq!(queue_times.len(), 1);
    }

    // If a component stops without escrow, we should not record anything into the histograms.
    #[fuchsia::test]
    async fn no_escrow_no_values() {
        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        let inspector = inspect::Inspector::default();
        let stats = Arc::new(DurationStats::new(inspector.root().create_child("escrow")));
        root.hooks.install(stats.hooks()).await;
        root.ensure_started(&StartReason::Debug).await.unwrap();
        root.stop().await.unwrap();

        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {},
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });
    }

    // Tests the escrow stats as a component transitions between some lifecycles:
    //
    // - Start -> no records
    // - Stop with escrow once -> one started record
    // - Start again -> one started and one stopped record
    // - Stop again without escrow -> one started and two stopped records
    //
    #[fuchsia::test(allow_stalls = false)]
    async fn escrow_progression() {
        let (out_dir_tx, mut out_dir_rx) = mpsc::channel(1);
        let out_dir_tx = fsync::Mutex::new(out_dir_tx);

        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let url = "test:///root_resolved";
        let test = ActionsTest::new(components[0].0, components, None).await;
        test.runner.add_host_fn(
            url,
            Box::new(move |server_end: ServerEnd<fio::DirectoryMarker>| {
                out_dir_tx.lock().try_send(server_end).unwrap();
            }),
        );
        let root = test.model.root();
        let inspector = inspect::Inspector::default();
        let stats = Arc::new(DurationStats::new(inspector.root().create_child("escrow")));
        root.hooks.install(stats.hooks()).await;

        let mut event_source =
            test.builtin_environment.lock().await.event_source_factory.create_for_above_root();
        let mut event_stream = event_source
            .subscribe(
                vec![EventType::Started.into(), EventType::Stopped.into()]
                    .into_iter()
                    .map(|event: Name| {
                        EventSubscription::new(UseEventStreamDecl {
                            source_name: event,
                            source: UseSource::Parent,
                            scope: None,
                            target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                            filter: None,
                            availability: Availability::Required,
                        })
                    })
                    .collect(),
            )
            .await
            .expect("subscribe to event stream");

        // Start -> no records
        root.ensure_started(&StartReason::Debug).await.unwrap();
        test.runner.wait_for_url(url).await;
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let start_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {},
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });

        let mut started_assertion = HistogramAssertion::exponential(ExponentialHistogramParams {
            floor: 4,
            initial_step: 3,
            step_multiplier: 2,
            buckets: 12,
        });
        let mut stopped_assertion = HistogramAssertion::linear(LinearHistogramParams {
            floor: 10,
            step_size: 10,
            buckets: 24,
        });

        // Stop with escrow once -> one started record
        let info = ComponentInfo::new(root.clone()).await;
        let outgoing_server_end = out_dir_rx.next().await.unwrap();
        test.runner.send_on_escrow(
            &info.channel_id,
            fcrunner::ComponentControllerOnEscrowRequest {
                outgoing_dir: Some(outgoing_server_end),
                ..Default::default()
            },
        );
        test.runner.reset_wait_for_url(url);
        test.runner.abort_controller(&info.channel_id);
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let stop_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Stopped).await;
        started_assertion.insert_values(vec![(stop_timestamp - start_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion.clone(),
                    },
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });

        // Start again -> one started and one stopped record
        root.ensure_started(&StartReason::Debug).await.unwrap();
        test.runner.wait_for_url(url).await;
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let start_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        stopped_assertion.insert_values(vec![(start_timestamp - stop_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion.clone(),
                    },
                },
                stopped_durations: {
                    histogram: {
                        ".": stopped_assertion.clone(),
                    },
                }
            }
        });

        // Stop again without escrow -> one started and two stopped records
        root.stop().await.unwrap();
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let stop_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Stopped).await;
        started_assertion.insert_values(vec![(stop_timestamp - start_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion,
                    },
                },
                stopped_durations: {
                    histogram: {
                        ".": stopped_assertion,
                    },
                }
            }
        });
    }

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
