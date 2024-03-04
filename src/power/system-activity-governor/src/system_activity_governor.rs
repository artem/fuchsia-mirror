// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_utils::hanging_get::server::{HangingGet, Publisher};
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system::{
    self as fsystem, APPLICATION_ACTIVITY_ACTIVE, APPLICATION_ACTIVITY_INACTIVE,
    EXECUTION_STATE_ACTIVE, EXECUTION_STATE_INACTIVE, EXECUTION_STATE_WAKE_HANDLING,
    WAKE_HANDLING_ACTIVE, WAKE_HANDLING_INACTIVE,
};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{ArrayProperty, Property};
use fuchsia_zircon as zx;
use futures::{future::LocalBoxFuture, prelude::*};
use power_broker_client::PowerElementContext;
use std::{cell::RefCell, rc::Rc};

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;
type StatsPublisher = Publisher<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

enum IncomingRequest {
    ActivityGovernor(fsystem::ActivityGovernorRequestStream),
    Stats(fsuspend::StatsRequestStream),
}

struct SuspendStatsManager {
    hanging_get: RefCell<StatsHangingGet>,
    stats_publisher: StatsPublisher,
    inspect_node: fuchsia_inspect::Node,
    success_count_node: fuchsia_inspect::UintProperty,
    fail_count_node: fuchsia_inspect::UintProperty,
    last_failed_error_node: fuchsia_inspect::IntProperty,
    last_time_in_suspend_node: fuchsia_inspect::IntProperty,
    last_time_in_suspend_operations_node: fuchsia_inspect::IntProperty,
}

impl SuspendStatsManager {
    fn new(inspect_node: fuchsia_inspect::Node) -> Self {
        let stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        let success_count_node =
            inspect_node.create_uint("success_count", *stats.success_count.as_ref().unwrap_or(&0));
        let fail_count_node =
            inspect_node.create_uint("fail_count", *stats.fail_count.as_ref().unwrap_or(&0));
        let last_failed_error_node = inspect_node.create_int(
            "last_failed_error",
            (*stats.last_failed_error.as_ref().unwrap_or(&0i32)).into(),
        );
        let last_time_in_suspend_node = inspect_node.create_int(
            "last_time_in_suspend",
            *stats.last_time_in_suspend.as_ref().unwrap_or(&-1i64),
        );
        let last_time_in_suspend_operations_node = inspect_node.create_int(
            "last_time_in_suspend_operations",
            *stats.last_time_in_suspend_operations.as_ref().unwrap_or(&-1i64),
        );

        let hanging_get = StatsHangingGet::new(
            stats,
            Box::new(
                |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                    if let Err(error) = res.send(stats) {
                        tracing::warn!(?error, "Failed to send suspend stats to client");
                    }
                    true
                },
            ),
        );

        let stats_publisher = hanging_get.new_publisher();

        Self {
            hanging_get: RefCell::new(hanging_get),
            stats_publisher,
            inspect_node,
            success_count_node,
            fail_count_node,
            last_failed_error_node,
            last_time_in_suspend_node,
            last_time_in_suspend_operations_node,
        }
    }

    fn update<UpdateFn>(&self, update: UpdateFn)
    where
        UpdateFn: FnOnce(&mut Option<fsuspend::SuspendStats>) -> bool,
    {
        self.stats_publisher.update(|stats_opt| {
            let success = update(stats_opt);

            self.inspect_node.atomic_update(|_| {
                let stats = stats_opt.as_ref().expect("stats is uninitialized");
                self.success_count_node.set(*stats.success_count.as_ref().unwrap_or(&0));
                self.fail_count_node.set(*stats.fail_count.as_ref().unwrap_or(&0));
                self.last_failed_error_node
                    .set((*stats.last_failed_error.as_ref().unwrap_or(&0i32)).into());
                self.last_time_in_suspend_node
                    .set(*stats.last_time_in_suspend.as_ref().unwrap_or(&-1i64));
                self.last_time_in_suspend_operations_node
                    .set(*stats.last_time_in_suspend_operations.as_ref().unwrap_or(&-1i64));
            });

            tracing::debug!(?stats_opt, "suspend stats");
            success
        });
    }
}

/// SystemActivityGovernor runs the server for fuchsia.power.suspend FIDL APIs.
pub struct SystemActivityGovernor {
    inspect_root: fuchsia_inspect::Node,
    execution_state: PowerElementContext,
    application_activity: PowerElementContext,
    wake_handling: PowerElementContext,
    execution_resume_latency: PowerElementContext,
    suspend_stats: SuspendStatsManager,
    suspender: fhsuspend::SuspenderProxy,
    resume_latencies: Vec<zx::sys::zx_duration_t>,
    selected_suspend_state_index: RefCell<Option<u64>>,
}

impl SystemActivityGovernor {
    pub async fn new(
        topology: &fbroker::TopologyProxy,
        inspect_root: fuchsia_inspect::Node,
        suspender: fhsuspend::SuspenderProxy,
    ) -> Result<Rc<Self>> {
        let execution_state = PowerElementContext::builder(
            topology,
            "execution_state",
            &[EXECUTION_STATE_INACTIVE, EXECUTION_STATE_WAKE_HANDLING, EXECUTION_STATE_ACTIVE],
        )
        .build()
        .await?;

        let application_activity = PowerElementContext::builder(
            topology,
            "application_activity",
            &[APPLICATION_ACTIVITY_INACTIVE, APPLICATION_ACTIVITY_ACTIVE],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: APPLICATION_ACTIVITY_ACTIVE,
            requires_token: execution_state.active_dependency_token(),
            requires_level: EXECUTION_STATE_ACTIVE,
        }])
        .build()
        .await?;

        let wake_handling = PowerElementContext::builder(
            topology,
            "wake_handling",
            &[WAKE_HANDLING_INACTIVE, WAKE_HANDLING_ACTIVE],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: WAKE_HANDLING_ACTIVE,
            requires_token: execution_state.active_dependency_token(),
            requires_level: EXECUTION_STATE_WAKE_HANDLING,
        }])
        .build()
        .await?;

        let resp = suspender
            .get_suspend_states()
            .await
            .expect("FIDL error encountered while calling Suspend HAL")
            .expect("Suspend HAL returned error when getting suspend states");
        let suspend_states =
            resp.suspend_states.expect("Suspend HAL did not return any suspend states");
        tracing::info!(?suspend_states, "Got suspend states from suspend HAL");

        let resume_latencies: Vec<_> = suspend_states
            .iter()
            .map(|s| s.resume_latency.expect("resume_latency not given"))
            .collect();

        let latency_count = resume_latencies.len().try_into()?;

        let execution_resume_latency = PowerElementContext::builder(
            topology,
            "execution_resume_latency",
            &Vec::from_iter(0..latency_count),
        )
        .build()
        .await?;

        let suspend_stats = SuspendStatsManager::new(inspect_root.create_child("suspend_stats"));

        Ok(Rc::new(Self {
            inspect_root,
            execution_state,
            application_activity,
            wake_handling,
            execution_resume_latency,
            resume_latencies,
            suspend_stats,
            suspender,
            selected_suspend_state_index: RefCell::new(None),
        }))
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    pub async fn run(self: Rc<Self>) -> Result<()> {
        tracing::info!("Handling power elements");
        self.inspect_root.atomic_update(|node| {
            node.record_child("power_elements", |elements_node| {
                self.run_execution_state(&elements_node);
                self.run_application_activity(&elements_node);
                self.run_wake_handling(&elements_node);
                self.run_execution_resume_latency(elements_node);
            });
        });

        tracing::info!("Starting FIDL server");
        self.run_fidl_server().await
    }

    fn run_execution_state(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let execution_state_node = inspect_node.create_child("execution_state");
        let this = self.clone();

        fasync::Task::local(async move {
            let sag = this.clone();
            Self::run_power_element(
                &this.execution_state,
                EXECUTION_STATE_INACTIVE,
                execution_state_node,
                None,
                Some(Box::new(move |new_power_level: fbroker::PowerLevel| {
                    // After other elements have been informed of new_power_level for
                    // execution_state, check whether the system should be suspended.
                    let sag = sag.clone();
                    async move {
                        if new_power_level == EXECUTION_STATE_INACTIVE {
                            tracing::info!("Triggering suspend");
                            let resp = sag
                                .suspender
                                .suspend(&fhsuspend::SuspenderSuspendRequest {
                                    state_index: sag.selected_suspend_state_index.borrow().clone(),
                                    ..Default::default()
                                })
                                .await;
                            tracing::info!(?resp, "Handling suspend result");

                            sag.suspend_stats.update(
                                |stats_opt: &mut Option<fsuspend::SuspendStats>| {
                                    let stats = stats_opt.as_mut().expect("stats is uninitialized");

                                    match resp {
                                        Ok(Ok(res)) => {
                                            stats.last_time_in_suspend = res.suspend_duration;
                                            stats.last_time_in_suspend_operations =
                                                res.suspend_overhead;

                                            if stats.last_time_in_suspend.is_some() {
                                                stats.success_count =
                                                    stats.success_count.map(|c| c + 1);
                                            } else {
                                                tracing::warn!("Failed to suspend");
                                                stats.fail_count = stats.fail_count.map(|c| c + 1);
                                            }
                                        }
                                        error => {
                                            tracing::warn!(?error, "Failed to suspend");
                                            stats.fail_count = stats.fail_count.map(|c| c + 1);

                                            if let Ok(Err(error)) = error {
                                                stats.last_failed_error = Some(error);
                                            }
                                        }
                                    }

                                    tracing::info!(?stats, "Updated suspend stats");
                                    true
                                },
                            );
                        }
                    }
                    .boxed_local()
                })),
            )
            .await;
        })
        .detach();
    }

    fn run_application_activity(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let application_activity_node = inspect_node.create_child("application_activity");
        let this = self.clone();

        fasync::Task::local(async move {
            Self::run_power_element(
                &this.application_activity,
                APPLICATION_ACTIVITY_INACTIVE,
                application_activity_node,
                None,
                None,
            )
            .await;
        })
        .detach();
    }

    fn run_wake_handling(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let wake_handling_node = inspect_node.create_child("wake_handling");
        let this = self.clone();

        fasync::Task::local(async move {
            Self::run_power_element(
                &this.wake_handling,
                WAKE_HANDLING_INACTIVE,
                wake_handling_node,
                None,
                None,
            )
            .await;
        })
        .detach();
    }

    fn run_execution_resume_latency(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let execution_resume_latency_node = inspect_node.create_child("execution_resume_latency");
        let initial_level = 0;
        let this = self.clone();

        let resume_latencies_node = execution_resume_latency_node
            .create_int_array("resume_latencies", self.resume_latencies.len());
        for (i, val) in self.resume_latencies.iter().enumerate() {
            resume_latencies_node.set(i, *val);
        }
        execution_resume_latency_node.record(resume_latencies_node);
        let resume_latency_node =
            execution_resume_latency_node.create_int("resume_latency", self.resume_latencies[0]);

        fasync::Task::local(async move {
            let sag = this.clone();
            let sag2 = this.clone();

            Self::run_power_element(
                &this.execution_resume_latency,
                initial_level,
                execution_resume_latency_node,
                Some(Box::new(move |new_power_level: fbroker::PowerLevel| {
                    // new_power_level for execution_resume_latency is an index into
                    // the list of resume latencies returned by the suspend HAL.
                    // Before other power elements are informed of the new power level,
                    // update the value that will be sent to the suspend HAL when suspension
                    // is triggered to avoid data races.
                    if (new_power_level as usize) < sag.resume_latencies.len() {
                        sag.selected_suspend_state_index
                            .borrow_mut()
                            .replace(new_power_level.into());
                    }
                    future::ready(()).boxed_local()
                })),
                Some(Box::new(move |new_power_level: fbroker::PowerLevel| {
                    // new_power_level for execution_resume_latency is an index into
                    // the list of resume latencies returned by the suspend HAL.
                    // After other power elements are informed of the new power level,
                    // update Inspect to account for the new resume latency value.
                    let power_level = new_power_level as usize;
                    if power_level < sag2.resume_latencies.len() {
                        resume_latency_node.set(sag2.resume_latencies[power_level]);
                    }
                    future::ready(()).boxed_local()
                })),
            )
            .await;
        })
        .detach();
    }

    async fn run_power_element<'a>(
        power_element: &'a PowerElementContext,
        initial_level: fbroker::PowerLevel,
        inspect_node: fuchsia_inspect::Node,
        pre_update_fn: Option<Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()>>>,
        post_update_fn: Option<Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()>>>,
    ) {
        let element_name = power_element.name();
        let mut last_required_level = initial_level;
        let power_level_node = inspect_node.create_uint("power_level", last_required_level.into());

        loop {
            tracing::debug!(
                ?element_name,
                ?last_required_level,
                "run_power_element: waiting for new level"
            );
            match power_element.level_control.watch_required_level(last_required_level).await {
                Ok(Ok(required_level)) => {
                    tracing::debug!(
                        ?element_name,
                        ?required_level,
                        ?last_required_level,
                        "run_power_element: new level requested"
                    );

                    if let Some(pre_update_fn) = &pre_update_fn {
                        pre_update_fn(required_level).await;
                    }

                    let res = power_element
                        .level_control
                        .update_current_power_level(required_level)
                        .await;
                    if let Err(error) = res {
                        tracing::warn!(
                            ?element_name,
                            ?error,
                            "run_power_element: update_current_power_level failed"
                        );
                    }

                    power_level_node.set(required_level.into());
                    if let Some(post_update_fn) = &post_update_fn {
                        post_update_fn(required_level).await;
                    }
                    last_required_level = required_level;
                }
                error => {
                    tracing::warn!(
                        ?element_name,
                        ?error,
                        "run_power_element: watch_required_level failed"
                    )
                }
            }
        }
    }

    async fn run_fidl_server(self: &Rc<Self>) -> Result<()> {
        let mut service_fs = ServiceFs::new_local();

        service_fs
            .dir("svc")
            .add_fidl_service(IncomingRequest::ActivityGovernor)
            .add_fidl_service(IncomingRequest::Stats);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let sag = self.clone();
                async move {
                    match request {
                        IncomingRequest::ActivityGovernor(stream) => {
                            fasync::Task::local(sag.handle_activity_governor_request(stream))
                                .detach()
                        }
                        IncomingRequest::Stats(stream) => {
                            fasync::Task::local(sag.handle_stats_request(stream)).detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_activity_governor_request(
        self: Rc<Self>,
        mut stream: fsystem::ActivityGovernorRequestStream,
    ) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsystem::ActivityGovernorRequest::GetPowerElements { responder } => {
                    let result = responder.send(fsystem::PowerElements {
                        execution_state: Some(fsystem::ExecutionState {
                            passive_dependency_token: Some(
                                self.execution_state.passive_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        application_activity: Some(fsystem::ApplicationActivity {
                            passive_dependency_token: Some(
                                self.application_activity.passive_dependency_token(),
                            ),
                            active_dependency_token: Some(
                                self.application_activity.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        wake_handling: Some(fsystem::WakeHandling {
                            passive_dependency_token: Some(
                                self.wake_handling.passive_dependency_token(),
                            ),
                            active_dependency_token: Some(
                                self.wake_handling.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        execution_resume_latency: Some(fsystem::ExecutionResumeLatency {
                            passive_dependency_token: Some(
                                self.execution_resume_latency.passive_dependency_token(),
                            ),
                            active_dependency_token: Some(
                                self.execution_resume_latency.active_dependency_token(),
                            ),
                            resume_latencies: Some(self.resume_latencies.clone()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });

                    if let Err(error) = result {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetPowerElements request"
                        );
                    }
                }
                fsystem::ActivityGovernorRequest::RegisterListener { responder, .. } => {
                    // TODO(mbrunson): Implement RegisterListener.
                    let _ = responder.send();
                }
                fsystem::ActivityGovernorRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown ActivityGovernorRequest method");
                }
            }
        }
    }

    async fn handle_stats_request(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
        let sub = self.suspend_stats.hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    if let Err(error) = sub.register(responder) {
                        tracing::warn!(?error, "Failed to register for Watch call");
                    }
                }
                fsuspend::StatsRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown StatsRequest method");
                }
            }
        }
    }
}
