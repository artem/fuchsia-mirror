// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints::{create_endpoints, create_proxy, Proxy};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, DependencyType, ElementSchema, LeaseStatus, LevelDependency,
    StatusMarker, TopologyMarker,
};
use fuchsia_async as fasync;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::{self as zx, HandleBased};
use power_broker_client::BINARY_POWER_LEVELS;

async fn build_power_broker_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<TopologyMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;
    Ok(realm)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fpb::{CurrentLevelMarker, RequiredLevelMarker},
    };

    #[test]
    fn test_direct() -> Result<()> {
        let mut executor = fasync::TestExecutor::new();
        let realm = executor.run_singlethreaded(async { build_power_broker_realm().await })?;

        // Create a topology with only two elements and a single dependency:
        // P <- C
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let parent_token = zx::Event::create();
        let (parent_current, parent_current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (parent_required, parent_required_server) = create_proxy::<RequiredLevelMarker>()?;
        let parent_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("P".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: parent_current_server,
                        required: parent_required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let parent_element_control = parent_element_control.into_proxy()?;
        let parent_status = {
            let (client, server) = create_proxy::<StatusMarker>()?;
            parent_element_control.open_status_channel(server)?;
            client
        };
        let (child_current, child_current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (child_required, child_required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (child_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let child_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: child_current_server,
                        required: child_required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let child_element_control = child_element_control.into_proxy()?;
        let child_status = {
            let (client, server) = create_proxy::<StatusMarker>()?;
            child_element_control.open_status_channel(server)?;
            client
        };

        // Initial required level for P & C should be OFF.
        // Update P & C's current level to OFF with PowerBroker.
        executor.run_singlethreaded(async {
            assert_eq!(
                parent_required.watch().await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
            assert_eq!(
                child_required.watch().await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        executor.run_singlethreaded(async {
            parent_current
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            child_current
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(
                parent_status.watch_power_level().await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
            assert_eq!(
                child_status.watch_power_level().await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        let parent_required_fut = parent_required.watch();
        let mut child_required_fut = child_required.watch();

        // Acquire lease for C.
        // P's required level should become ON.
        // C's required level should remain OFF until P turns ON.
        // Lease should be pending.
        let lease = executor.run_singlethreaded(async {
            child_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(
                parent_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });
        let mut parent_required_fut = parent_required.watch();
        assert!(executor.run_until_stalled(&mut child_required_fut).is_pending());

        // Update P's current level to ON.
        // P's required level should remain ON.
        // C's required level should become ON.
        // Lease should now be satisfied.
        executor.run_singlethreaded(async {
            parent_current
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(
                parent_status.watch_power_level().await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
            assert_eq!(
                child_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });
        let child_required_fut = child_required.watch();
        assert!(executor.run_until_stalled(&mut parent_required_fut).is_pending());

        // Drop lease.
        // P's required level should become OFF.
        // C's required level should become OFF.
        executor.run_singlethreaded(async {
            drop(lease);
            assert_eq!(
                parent_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
            assert_eq!(
                child_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        let mut parent_required_fut = parent_required.watch();
        let mut child_required_fut = child_required.watch();

        // Update P's current level to OFF
        // P's required level should remain OFF.
        // C's required level should remain OFF.
        executor.run_singlethreaded(async {
            parent_current
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(
                parent_status.watch_power_level().await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        assert!(executor.run_until_stalled(&mut parent_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child_required_fut).is_pending());

        // Remove P's element. Status channel should be closed.
        executor.run_singlethreaded(async {
            drop(parent_element_control);
            let status_after_remove = parent_status.watch_power_level().await;
            assert!(matches!(status_after_remove, Err(fidl::Error::ClientChannelClosed { .. })));
            assert!(matches!(
                parent_required_fut.await,
                Err(fidl::Error::ClientChannelClosed { .. })
            ));
        });
        // Remove C's element. Status channel should be closed.
        executor.run_singlethreaded(async {
            drop(child_element_control);
            let status_after_remove = child_status.watch_power_level().await;
            assert!(matches!(status_after_remove, Err(fidl::Error::ClientChannelClosed { .. })));
            assert!(matches!(
                child_required_fut.await,
                Err(fidl::Error::ClientChannelClosed { .. })
            ));
        });

        Ok(())
    }

    #[test]
    fn test_transitive() -> Result<()> {
        let mut executor = fasync::TestExecutor::new();
        let realm = executor.run_singlethreaded(async { build_power_broker_realm().await })?;

        // Create a four element topology with the following dependencies:
        // C depends on B, which in turn depends on A.
        // D has no dependencies or dependents.
        // A <- B <- C   D
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let element_a_token = zx::Event::create();
        let (element_a_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_a_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_a_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("A".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![element_a_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_a_element_control = element_a_element_control.into_proxy()?;
        let element_a_status = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_a_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };
        let element_b_token = zx::Event::create();
        let (element_b_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_b_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_b_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("B".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: element_a_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    active_dependency_tokens_to_register: Some(vec![element_b_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_b_element_control = element_b_element_control.into_proxy()?;
        let element_b_status: fpb::StatusProxy = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_b_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };
        let (element_c_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_c_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (element_c_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let element_c_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: element_b_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_c_element_control = element_c_element_control.into_proxy()?;
        let element_c_status: fpb::StatusProxy = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_c_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };
        let (element_d_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_d_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_d_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("D".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_d_element_control = element_d_element_control.into_proxy()?;
        let element_d_status: fpb::StatusProxy = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_d_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };

        // Initial required level for each element should be OFF.
        // Update managed elements' current level to OFF with PowerBroker.
        for (status, current, required) in [
            (&element_a_status, &element_a_current, &element_a_required),
            (&element_b_status, &element_b_current, &element_b_required),
            (&element_c_status, &element_c_current, &element_c_required),
            (&element_d_status, &element_d_current, &element_d_required),
        ] {
            executor.run_singlethreaded(async {
                let req_level_fut = required.watch();
                assert_eq!(
                    req_level_fut.await.unwrap(),
                    Ok(BinaryPowerLevel::Off.into_primitive())
                );
                current
                    .update(BinaryPowerLevel::Off.into_primitive())
                    .await
                    .unwrap()
                    .expect("update_current_power_level failed");
                let power_level =
                    status.watch_power_level().await.unwrap().expect("watch_power_level failed");
                assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());
            });
        }
        let element_a_required_fut = element_a_required.watch();
        let mut element_b_required_fut = element_b_required.watch();
        let mut element_c_required_fut = element_c_required.watch();
        let mut element_d_required_fut = element_d_required.watch();

        // Acquire lease for C.
        // A's required level should become ON.
        // B's required level should remain OFF because A is not yet ON.
        // C's required level should remain OFF because B is not yet ON.
        // D's required level should remain OFF.
        let lease = executor.run_singlethreaded(async {
            element_c_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(
                element_a_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
        });
        let mut element_a_required_fut = element_a_required.watch();
        assert!(executor.run_until_stalled(&mut element_b_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_c_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_d_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because A is now ON.
        // C's required level should remain OFF because B is not yet ON.
        // D's required level should remain OFF.
        executor.run_singlethreaded(async {
            element_a_current
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut element_a_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                element_b_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
        });
        let mut element_b_required_fut = element_b_required.watch();
        assert!(executor.run_until_stalled(&mut element_c_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_d_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update B's current level to ON.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become ON because B is now ON.
        // D's required level should remain OFF.
        executor.run_singlethreaded(async {
            element_b_current
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut element_a_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_b_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                element_c_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
        });
        let element_c_required_fut = element_c_required.watch();
        assert!(executor.run_until_stalled(&mut element_d_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for C with PB.
        // A's required level should remain ON.
        // B's required level should become OFF because C dropped its claim.
        // C's required level should become OFF because the lease was dropped.
        // D's required level should remain OFF.
        drop(lease);
        assert!(executor.run_until_stalled(&mut element_a_required_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                element_b_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        let mut element_b_required_fut = element_b_required.watch();
        executor.run_singlethreaded(async {
            assert_eq!(
                element_c_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        let mut element_c_required_fut = element_c_required.watch();
        assert!(executor.run_until_stalled(&mut element_d_required_fut).is_pending());

        // Lower B's current level to OFF.
        // A's required level should become OFF because B is no longer dependent.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        // D's required level should remain OFF.
        executor.run_singlethreaded(async {
            element_b_current
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        executor.run_singlethreaded(async {
            assert_eq!(
                element_a_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        assert!(executor.run_until_stalled(&mut element_b_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_c_required_fut).is_pending());
        assert!(executor.run_until_stalled(&mut element_d_required_fut).is_pending());

        Ok(())
    }

    #[test]
    fn test_shared() -> Result<()> {
        // Create a topology of two child elements (C1 & C2) with a shared
        // parent (P) and grandparent (GP)
        // C1 \
        //     > P -> GP
        // C2 /
        // Child 1 requires Parent at 50 to support its own level of 5.
        // Parent requires Grandparent at 200 to support its own level of 50.
        // C1 -> P -> GP
        //  5 -> 50 -> 200
        // Child 2 requires Parent at 30 to support its own level of 3.
        // Parent requires Grandparent at 90 to support its own level of 30.
        // C2 -> P -> GP
        //  3 -> 30 -> 90
        // Grandparent has a default minimum level of 10.
        // All other elements have a default of 0.
        let mut executor = fasync::TestExecutor::new();
        let realm = executor.run_singlethreaded(async { build_power_broker_realm().await })?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let grandparent_token = zx::Event::create();
        let (grandparent_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (grandparent_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _grandparent_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("GP".into()),
                    initial_current_level: Some(10),
                    valid_levels: Some(vec![10, 90, 200]),
                    active_dependency_tokens_to_register: Some(vec![grandparent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let parent_token = zx::Event::create();
        let (parent_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (parent_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _parent_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("P".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 30, 50]),
                    dependencies: Some(vec![
                        LevelDependency {
                            dependency_type: DependencyType::Active,
                            dependent_level: 50,
                            requires_token: grandparent_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            requires_level: 200,
                        },
                        LevelDependency {
                            dependency_type: DependencyType::Active,
                            dependent_level: 30,
                            requires_token: grandparent_token
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .expect("dup failed"),
                            requires_level: 90,
                        },
                    ]),
                    active_dependency_tokens_to_register: Some(vec![parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (child1_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (child1_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (child1_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _child1_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C1".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 5]),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 5,
                        requires_token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 50,
                    }]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (child2_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (child2_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (child2_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _child2_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C2".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 3]),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 3,
                        requires_token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 30,
                    }]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });

        // GP should have a initial required level of 10
        // P, C1 and C2 should have initial required levels of 0.
        executor.run_singlethreaded(async {
            let grandparent_req_level_fut = grandparent_required.watch();
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(10));
            grandparent_current
                .update(10)
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            let parent_req_level_fut = parent_required.watch();
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(0));
            parent_current.update(0).await.unwrap().expect("update_current_power_level failed");
            let child1_req_level_fut = child1_required.watch();
            assert_eq!(child1_req_level_fut.await.unwrap(), Ok(0));
            child1_current.update(0).await.unwrap().expect("update_current_power_level failed");
            let child2_req_level_fut = child2_required.watch();
            assert_eq!(child2_req_level_fut.await.unwrap(), Ok(0));
            child2_current.update(0).await.unwrap().expect("update_current_power_level failed");
        });
        let grandparent_req_level_fut = grandparent_required.watch();
        let mut parent_req_level_fut = parent_required.watch();
        let mut child1_req_level_fut = child1_required.watch();
        let mut child2_req_level_fut = child2_required.watch();

        // Acquire lease for C1 @ 5.
        // GP's required level should become 200 because C1 @ 5 has a
        // dependency on P @ 50 and P @ 50 has a dependency on GP @ 200.
        // GP @ 200 has no dependencies so its level should be raised first.
        // P's required level should remain 0 because GP is not yet at 200.
        // C1's required level should remain 0 because P is not yet at 50.
        // C2's required level should remain 0.
        let lease_child_1 = executor.run_singlethreaded(async {
            child1_lessor.lease(5).await.unwrap().expect("Lease response not ok").into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(200));
        });
        let mut grandparent_req_level_fut = grandparent_required.watch();
        assert!(executor.run_until_stalled(&mut parent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Raise GP's current level to 200.
        // GP's required level should remain 200.
        // P's required level should become 50 because GP is now at 200.
        // C1's required level should remain 0 because P is not yet at 50.
        // C2's required level should remain 0.
        executor.run_singlethreaded(async {
            grandparent_current
                .update(200)
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut grandparent_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(50));
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });
        let mut parent_req_level_fut = parent_required.watch();
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());

        // Update P's current level to 50.
        // GP's required level should remain 200.
        // P's required level should remain 50.
        // C1's required level should become 5 because P is now at 50.
        // C2's required level should remain 0.
        // C1's lease @ 5 is now satisfied.
        executor.run_singlethreaded(async {
            parent_current.update(50).await.unwrap().expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut grandparent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut parent_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(child1_req_level_fut.await.unwrap(), Ok(5));
        });
        let mut child1_req_level_fut = child1_required.watch();
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Acquire lease for C2 @ 3.
        // Though C2 @ 3 has nominal requirements of P @ 30 and GP @ 100,
        // they are superseded by C1's requirements of 50 and 200.
        // GP's required level should remain 200.
        // P's required level should remain 50.
        // C1's required level should remain 5.
        // C2's required level should become 3 because its dependencies are already satisfied.
        // C1's lease @ 5 is still satisfied.
        // C2's lease @ 3 is immediately satisfied.
        let lease_child_2 = executor.run_singlethreaded(async {
            child2_lessor
                .lease(3)
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
                .unwrap()
        });
        assert!(executor.run_until_stalled(&mut grandparent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut parent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(child2_req_level_fut.await.unwrap(), Ok(3));
        });
        let mut child2_req_level_fut = child2_required.watch();
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for C1.
        // Parent's required level should immediately drop to 30.
        // Grandparent's required level will remain at 200 for now.
        // GP's required level should remain 200 because P is still at 50.
        // P's required level should become 30 because C1 has dropped its claim.
        // C1's required level should become 0 because its lease has been dropped.
        // C2's required level should remain 3.
        // C2's lease @ 3 is still satisfied.
        drop(lease_child_1);
        assert!(executor.run_until_stalled(&mut grandparent_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(30));
        });
        let mut parent_req_level_fut = parent_required.watch();
        executor.run_singlethreaded(async {
            assert_eq!(child1_req_level_fut.await.unwrap(), Ok(0));
        });
        let mut child1_req_level_fut = child1_required.watch();
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Lower Parent's current level to 30.
        // GP's required level should become 90 because P has dropped to 30.
        // P's required level should remain 30.
        // C1's required level should remain 0.
        // C2's required level should remain 3.
        // C2's lease @ 3 is still satisfied.
        executor.run_singlethreaded(async {
            parent_current.update(30).await.unwrap().expect("update_current_power_level failed");
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(90));
        });
        assert!(executor.run_until_stalled(&mut parent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for Child 2.
        // Parent should have required level 0.
        // Grandparent should still have required level 90.
        // GP's required level should remain 90 because P is still at 30.
        // P's required level should become 0 because C2's claim has been dropped.
        // C1's required level should remain 0.
        // C2's required level should become 0 because its lease has been dropped.
        drop(lease_child_2);
        let mut grandparent_req_level_fut = grandparent_required.watch();
        assert!(executor.run_until_stalled(&mut grandparent_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(0));
        });
        let mut parent_req_level_fut = parent_required.watch();
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(child2_req_level_fut.await.unwrap(), Ok(0));
        });
        let mut child2_req_level_fut = child2_required.watch();

        // Lower Parent's current level to 0.
        // GP's required level should become its minimum level of 10 because P is now at 0.
        // P's required level should remain 0.
        // C1's required level should remain 0.
        // C2's required level should remain 0.
        executor.run_singlethreaded(async {
            parent_current.update(0).await.unwrap().expect("update_current_power_level failed");
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(10));
        });
        assert!(executor.run_until_stalled(&mut parent_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child1_req_level_fut).is_pending());
        assert!(executor.run_until_stalled(&mut child2_req_level_fut).is_pending());

        Ok(())
    }

    #[test]
    fn test_passive() -> Result<()> {
        // B has an active dependency on A.
        // C has a passive dependency on B (and transitively, a passive dependency on A)
        //   and an active dependency on E.
        // D has an active dependency on B (and transitively, an active dependency on A).
        //  A     B     C     D     E
        // ON <= ON
        //       ON <- ON =======> ON
        //       ON <======= ON
        let mut executor = fasync::TestExecutor::new();
        let realm =
            executor.run_singlethreaded(async { build_power_broker_realm().await }).unwrap();
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let token_a = zx::Event::create();
        let (current_a, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_a, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_a_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("A".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![token_a
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let token_b_active = zx::Event::create();
        let token_b_passive = zx::Event::create();
        let (current_b, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_b, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_b_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("B".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_a.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    active_dependency_tokens_to_register: Some(vec![token_b_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    passive_dependency_tokens_to_register: Some(vec![token_b_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let token_e_active = zx::Event::create();
        let (current_e, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_e, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_e_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("E".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![token_e_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (current_c, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_c, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (element_c_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _element_c_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![
                        LevelDependency {
                            dependency_type: DependencyType::Passive,
                            dependent_level: BinaryPowerLevel::On.into_primitive(),
                            requires_token: token_b_passive
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .unwrap(),
                            requires_level: BinaryPowerLevel::On.into_primitive(),
                        },
                        LevelDependency {
                            dependency_type: DependencyType::Active,
                            dependent_level: BinaryPowerLevel::On.into_primitive(),
                            requires_token: token_e_active
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .unwrap(),
                            requires_level: BinaryPowerLevel::On.into_primitive(),
                        },
                    ]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (current_d, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_d, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let (element_d_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _element_d_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("D".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_b_active
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .unwrap(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });

        // Initial required level for all elements should be OFF.
        // Set all current levels to OFF.
        executor.run_singlethreaded(async {
            for required in [&required_a, &required_b, &required_c, &required_d, &required_e] {
                assert_eq!(
                    required.watch().await.unwrap(),
                    Ok(BinaryPowerLevel::Off.into_primitive())
                );
            }
            for current in [&current_a, &current_b, &current_c, &current_d, &current_e] {
                current
                    .update(BinaryPowerLevel::Off.into_primitive())
                    .await
                    .unwrap()
                    .expect("update_current_power_level failed");
            }
        });
        let mut required_a_fut = required_a.watch();
        let mut required_b_fut = required_b.watch();
        let mut required_c_fut = required_c.watch();
        let mut required_d_fut = required_d.watch();
        let mut required_e_fut = required_e.watch();

        // Lease C.
        // A & B's required level should remain OFF because C's passive claim
        // does not raise the level of A or B.
        // C's required level should remain OFF because its lease is still pending.
        // D's required level should remain OFF.
        // E's required level should remain OFF because C's passive claim on B
        // has no other active claims to satisfy it (the lease is contingent)
        // and hence its active claim on E should remain pending and should not
        // raise the level of E.
        // Lease C should be Pending.
        let lease_c = executor.run_singlethreaded(async {
            element_c_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Lease D.
        // A's required level should become ON because of D's transitive active claim.
        // B's required level should remain OFF because A is not yet ON.
        // C's required level should remain OFF because B and E are not yet ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should become ON because it C's lease is no longer
        // contingent on an active claim that would satisfy its passive claim.
        // Lease C & D should be pending.
        let lease_d = executor.run_singlethreaded(async {
            element_d_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(required_a_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_a_fut = required_a.watch();
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_e_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_e_fut = required_e.watch();
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update A's current level to ON.
        // A's required level should remain ON.
        // B's required level should become ON because of D's active claim and
        // its dependency on A being satisfied.
        // C's required level should remain OFF because B and E are not yet ON.
        // D's required level should remain OFF because B is not yet ON.
        // E's required level should remain ON.
        // Lease C & D should remain pending.
        executor.run_singlethreaded(async {
            current_a
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_b_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_b_fut = required_b.watch();
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update B's current level to ON.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should remain OFF because E is not yet ON.
        // D's required level should become ON.
        // E's required level should remain ON.
        // Lease C should remain pending.
        // Lease D should become satisfied.
        executor.run_singlethreaded(async {
            current_b
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_d_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_d_fut = required_d.watch();
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Update E's current level to ON.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become ON because E is now ON.
        // D's required level should remain ON.
        // E's required level should remain ON.
        // Lease C should become satisfied.
        // Lease D should remain pending.
        executor.run_singlethreaded(async {
            current_e
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_c_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let required_c_fut = required_c.watch();
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop Lease on D.
        // A's required level should remain ON.
        // B's required level should remain ON.
        // C's required level should become OFF because its lease is now pending and contingent.
        // D's required level should become OFF because its lease was dropped.
        // E's required level should remain ON.
        // Lease C should now be Pending.
        // A, B & E's required level should remain ON.
        drop(lease_d);
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_c_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_c_fut = required_c.watch();
        executor.run_singlethreaded(async {
            assert_eq!(required_d_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_d_fut = required_d.watch();
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Drop Lease on C.
        // A's required level should remain ON.
        // B's required level should become OFF because C has dropped its claim.
        // C's required level should remain OFF because its lease was dropped.
        // D's required level should remain OFF.
        // E's required level should become OFF because C has dropped its claim.
        drop(lease_c);
        assert!(executor.run_until_stalled(&mut required_a_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_b_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_b_fut = required_b.watch();
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        executor.run_singlethreaded(async {
            assert_eq!(required_e_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_e_fut = required_e.watch();

        // Update B's current level to OFF.
        // A's required level should become OFF.
        // B's required level should remain OFF.
        // C's required level should remain OFF.
        // D's required level should remain OFF.
        // E's required level should remain OFF.
        executor.run_singlethreaded(async {
            current_b
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(required_a_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        assert!(executor.run_until_stalled(&mut required_b_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_c_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_d_fut).is_pending());
        assert!(executor.run_until_stalled(&mut required_e_fut).is_pending());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_topology() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;

        // Create a four element topology.
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let earth_token = zx::Event::create();
        let earth_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Earth".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let earth_element_control = earth_element_control.into_proxy()?;
        let water_token = zx::Event::create();
        let water_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Water".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                dependencies: Some(vec![LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: earth_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }]),
                active_dependency_tokens_to_register: Some(vec![
                    water_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let water_element_control = water_element_control.into_proxy()?;
        let fire_token = zx::Event::create();
        let fire_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Fire".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let fire_element_control = fire_element_control.into_proxy()?;
        let air_token = zx::Event::create();
        let air_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Air".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    air_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let air_element_control = air_element_control.into_proxy()?;

        let extra_add_dep_res = water_element_control
            .add_dependency(
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                BinaryPowerLevel::On.into_primitive(),
            )
            .await?;
        assert!(matches!(extra_add_dep_res, Err(fpb::ModifyDependencyError::AlreadyExists { .. })));

        water_element_control
            .remove_dependency(
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                BinaryPowerLevel::On.into_primitive(),
            )
            .await?
            .expect("remove_dependency failed");

        let extra_remove_dep_res = water_element_control
            .remove_dependency(
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                BinaryPowerLevel::On.into_primitive(),
            )
            .await?;
        assert!(matches!(extra_remove_dep_res, Err(fpb::ModifyDependencyError::NotFound { .. })));

        drop(air_element_control);

        // Confirm the element has been removed and Status channels have been
        // closed.
        let fire_status = {
            let (client, server) = create_proxy::<StatusMarker>()?;
            fire_element_control.open_status_channel(server)?;
            client
        };
        drop(fire_element_control);
        fire_status.as_channel().on_closed().await?;

        let add_dep_req_invalid = earth_element_control
            .add_dependency(
                DependencyType::Active,
                BinaryPowerLevel::On.into_primitive(),
                fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                BinaryPowerLevel::On.into_primitive(),
            )
            .await?;
        assert!(matches!(add_dep_req_invalid, Err(fpb::ModifyDependencyError::NotAuthorized),));

        Ok(())
    }
}
