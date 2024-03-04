// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use diagnostics_assertions::tree_assertion;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::{create_endpoints, ClientEnd};
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fidl_test_systemactivitygovernor as ftest;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::{self as zx, HandleBased};
use futures::StreamExt;
use power_broker_client::PowerElementContext;
use realm_proxy_client::RealmProxyClient;

const REALM_FACTORY_CHILD_NAME: &str = "test_realm_factory";

fn run_suspender(
    get_suspend_stats_resp: Result<fhsuspend::SuspenderGetSuspendStatesResponse, i32>,
    suspend_resp: Result<fhsuspend::SuspenderSuspendResponse, i32>,
    expected_suspend_request: Option<fhsuspend::SuspenderSuspendRequest>,
) -> ClientEnd<ftest::SuspenderHandlerMarker> {
    let (client, mut handler_stream) =
        fidl::endpoints::create_request_stream::<ftest::SuspenderHandlerMarker>().unwrap();

    fasync::Task::local(async move {
        while let Some(Ok(req)) = handler_stream.next().await {
            if let ftest::SuspenderHandlerRequest::Handle { suspender, .. } = req {
                let mut stream = suspender.into_stream().unwrap();
                let get_suspend_stats_resp = get_suspend_stats_resp.clone();
                let suspend_resp = suspend_resp.clone();
                let expected_suspend_request = expected_suspend_request.clone();

                fasync::Task::local(async move {
                    tracing::debug!("Serving suspend HAL");
                    while let Some(Ok(req)) = stream.next().await {
                        tracing::debug!(?req, "New request");
                        match req {
                            fhsuspend::SuspenderRequest::GetSuspendStates { responder } => {
                                responder
                                    .send(get_suspend_stats_resp.as_ref().map_err(|e| *e))
                                    .unwrap();
                            }
                            fhsuspend::SuspenderRequest::Suspend { payload, responder } => {
                                if let Some(expected) = &expected_suspend_request {
                                    assert_eq!(*expected, payload);
                                }

                                responder.send(suspend_resp.as_ref().map_err(|e| *e)).unwrap();
                            }
                            fhsuspend::SuspenderRequest::_UnknownMethod { .. } => unreachable!(),
                        }
                    }
                })
                .detach();
            }
        }
    })
    .detach();

    client
}

fn run_default_suspender() -> ClientEnd<ftest::SuspenderHandlerMarker> {
    run_suspender(
        Ok(fhsuspend::SuspenderGetSuspendStatesResponse {
            suspend_states: Some(vec![fhsuspend::SuspendState {
                resume_latency: Some(0),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        Ok(fhsuspend::SuspenderSuspendResponse {
            suspend_duration: Some(2),
            suspend_overhead: Some(1),
            ..Default::default()
        }),
        None,
    )
}

async fn create_realm(options: ftest::RealmOptions) -> Result<(RealmProxyClient, String)> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    let result = realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok((RealmProxyClient::from(client), result))
}

#[fuchsia::test]
async fn test_stats_returns_default_values() -> Result<()> {
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_default_suspender()),
        ..Default::default()
    };
    let (realm, _) = create_realm(realm_options).await?;

    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);
    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_returns_expected_power_elements() -> Result<()> {
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_default_suspender()),
        ..Default::default()
    };
    let (realm, _) = create_realm(realm_options).await?;

    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let es_token = power_elements.execution_state.unwrap().passive_dependency_token.unwrap();
    assert!(!es_token.is_invalid_handle());

    let aa_element = power_elements.application_activity.unwrap();
    let aa_passive_token = aa_element.passive_dependency_token.unwrap();
    assert!(!aa_passive_token.is_invalid_handle());
    let aa_active_token = aa_element.active_dependency_token.unwrap();
    assert!(!aa_active_token.is_invalid_handle());

    let wh_element = power_elements.wake_handling.unwrap();
    let wh_passive_token = wh_element.passive_dependency_token.unwrap();
    assert!(!wh_passive_token.is_invalid_handle());
    let wh_active_token = wh_element.active_dependency_token.unwrap();
    assert!(!wh_active_token.is_invalid_handle());

    Ok(())
}

async fn create_suspend_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let aa_token = power_elements.application_activity.unwrap().active_dependency_token.unwrap();

    let suspend_controller = PowerElementContext::builder(&topology, "suspend_controller", &[0, 1])
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: 1,
            requires_token: aa_token,
            requires_level: 1,
        }])
        .build()
        .await?;

    Ok(suspend_controller)
}

async fn create_wake_topology(realm: &RealmProxyClient) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;
    let wh_token = power_elements.wake_handling.unwrap().active_dependency_token.unwrap();

    let wake_controller = PowerElementContext::builder(&topology, "wake_controller", &[0, 1])
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: 1,
            requires_token: wh_token,
            requires_level: 1,
        }])
        .build()
        .await?;

    Ok(wake_controller)
}

async fn create_latency_topology(
    realm: &RealmProxyClient,
    expected_latencies: &Vec<i64>,
) -> Result<PowerElementContext> {
    let topology = realm.connect_to_protocol::<fbroker::TopologyMarker>().await?;
    let activity_governor = realm.connect_to_protocol::<fsystem::ActivityGovernorMarker>().await?;
    let power_elements = activity_governor.get_power_elements().await?;

    let erl = power_elements.execution_resume_latency.unwrap();
    let erl_token = erl.active_dependency_token.unwrap();
    assert_eq!(*expected_latencies, erl.resume_latencies.unwrap());

    let levels = Vec::from_iter(0..expected_latencies.len().try_into().unwrap());

    let erl_controller = PowerElementContext::builder(&topology, "erl_controller", &levels)
        .dependencies(
            levels
                .iter()
                .map(|level| fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: *level,
                    requires_token: erl_token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                    requires_level: *level,
                })
                .collect(),
        )
        .build()
        .await?;

    Ok(erl_controller)
}

async fn lease(controller: &PowerElementContext, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control = controller
        .lessor
        .lease(level)
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy()?;

    let mut lease_status = LeaseStatus::Unknown;
    while lease_status != LeaseStatus::Satisfied {
        lease_status = lease_control.watch_status(lease_status).await.unwrap();
    }

    Ok(lease_control)
}

macro_rules! block_until_inspect_matches {
    ($sag_moniker:expr, $($tree:tt)+) => {{
        let mut reader = ArchiveReader::new();

        reader
            .select_all_for_moniker(&format!("{}/{}", REALM_FACTORY_CHILD_NAME, $sag_moniker))
            .with_minimum_schema_count(1);

        for i in 0.. {
            let Ok(data) = reader
                .snapshot::<Inspect>()
                .await?
                .into_iter()
                .next()
                .and_then(|result| result.payload)
                .ok_or(anyhow::anyhow!("expected one inspect hierarchy")) else {
                continue;
            };

            let tree_assertion = $crate::tree_assertion!($($tree)+);
            match tree_assertion.run(&data) {
                Ok(_) => break,
                Err(error) => {
                    if i == 10 {
                        tracing::warn!(?error, "Still awaiting inspect match after 10 tries");
                    }
                }
            }
        }
    }};
}

#[fuchsia::test]
async fn test_activity_governor_increments_suspend_success_on_application_activity_lease_drop(
) -> Result<()> {
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_default_suspender()),
        ..Default::default()
    };
    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    let current_stats = stats.watch().await?;
    assert_eq!(Some(1), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Continue incrementing success count on falling edge of Execution State level transitions.
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    let current_stats = stats.watch().await?;
    assert_eq!(Some(2), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(Some(2), current_stats.last_time_in_suspend);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 2u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_raises_execution_state_power_level_on_wake_handling_claim(
) -> Result<()> {
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_default_suspender()),
        ..Default::default()
    };
    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let wake_controller = create_wake_topology(&realm).await?;
    let wake_handling_lease_control = lease(&wake_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 1u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 1u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(wake_handling_lease_control);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: contains {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 2u64,
                last_time_in_suspend_operations: 1u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_shows_resume_latency_in_inspect() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(1000), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(100), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(10), ..Default::default() },
    ];
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_suspender(
            Ok(fhsuspend::SuspenderGetSuspendStatesResponse {
                suspend_states: Some(suspend_states),
                ..Default::default()
            }),
            Ok(Default::default()),
            None,
        )),
        ..Default::default()
    };

    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;
    let stats = realm.connect_to_protocol::<fsuspend::StatsMarker>().await?;

    // First watch should return immediately with default values.
    let current_stats = stats.watch().await?;
    assert_eq!(Some(0), current_stats.success_count);
    assert_eq!(Some(0), current_stats.fail_count);
    assert_eq!(None, current_stats.last_failed_error);
    assert_eq!(None, current_stats.last_time_in_suspend);

    let expected_latencies = vec![1000i64, 100i64, 10i64];
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;

    for i in 0..expected_latencies.len() {
        let _erl_lease_control = lease(&erl_controller, i.try_into().unwrap()).await?;

        block_until_inspect_matches!(
            activity_governor_moniker,
            root: {
                power_elements: {
                    execution_state: {
                        power_level: 0u64,
                    },
                    application_activity: {
                        power_level: 0u64,
                    },
                    wake_handling: {
                        power_level: 0u64,
                    },
                    execution_resume_latency: {
                        power_level: i as u64,
                        resume_latency: expected_latencies[i] as u64,
                        resume_latencies: expected_latencies.clone(),
                    },
                },
                suspend_stats: {
                    success_count: 0u64,
                    fail_count: 0u64,
                    last_failed_error: 0u64,
                    last_time_in_suspend: -1i64,
                    last_time_in_suspend_operations: -1i64,
                },
                "fuchsia.inspect.Health": contains {
                    status: "OK",
                },
            }
        );
    }

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_forwards_resume_latency_to_suspender() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(430), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(320), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(21), ..Default::default() },
    ];
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_suspender(
            Ok(fhsuspend::SuspenderGetSuspendStatesResponse {
                suspend_states: Some(suspend_states),
                ..Default::default()
            }),
            Ok(fhsuspend::SuspenderSuspendResponse {
                suspend_duration: Some(1000i64),
                suspend_overhead: Some(50i64),
                ..Default::default()
            }),
            Some(fhsuspend::SuspenderSuspendRequest { state_index: Some(1), ..Default::default() }),
        )),
        ..Default::default()
    };
    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    let expected_latencies = vec![430i64, 320i64, 21i64];
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;
    let _erl_lease_control = lease(&erl_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 1u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: 1000u64,
                last_time_in_suspend_operations: 50u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_activity_governor_increments_fail_count_on_suspend_error() -> Result<()> {
    let suspend_states = vec![
        fhsuspend::SuspendState { resume_latency: Some(430), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(320), ..Default::default() },
        fhsuspend::SuspendState { resume_latency: Some(21), ..Default::default() },
    ];
    let realm_options = ftest::RealmOptions {
        suspender_handler: Some(run_suspender(
            Ok(fhsuspend::SuspenderGetSuspendStatesResponse {
                suspend_states: Some(suspend_states),
                ..Default::default()
            }),
            Err(7),
            Some(fhsuspend::SuspenderSuspendRequest { state_index: Some(1), ..Default::default() }),
        )),
        ..Default::default()
    };
    let (realm, activity_governor_moniker) = create_realm(realm_options).await?;

    let suspend_controller = create_suspend_topology(&realm).await?;
    let suspend_lease_control = lease(&suspend_controller, 1).await?;

    let expected_latencies = vec![430i64, 320i64, 21i64];
    let erl_controller = create_latency_topology(&realm, &expected_latencies).await?;
    let _erl_lease_control = lease(&erl_controller, 1).await?;

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    drop(suspend_lease_control);

    block_until_inspect_matches!(
        activity_governor_moniker,
        root: {
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
                execution_resume_latency: {
                    power_level: 1u64,
                    resume_latency: 320u64,
                    resume_latencies: expected_latencies.clone(),
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 1u64,
                last_failed_error: 7u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

// TODO(b/306171083): Add more test cases when passive dependencies are supported.
