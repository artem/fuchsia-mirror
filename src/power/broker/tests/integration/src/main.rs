// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, Credential, Dependency, ElementLevel, LessorMarker,
    LevelControlMarker, LevelControlProxy, Permissions, PowerLevel, StatusMarker, TopologyMarker,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::{self as zx, HandleBased};

async fn build_power_broker_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LevelControlMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
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

#[fuchsia::test]
async fn test_direct() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a topology with only two elements and a single dependency:
    // P <- C
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (child_token, child_broker_token) = zx::EventPair::create();
    let child_cred = Credential {
        broker_token: child_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    let child = topology.add_element("C", vec![child_cred]).await?.expect("add_element failed");
    let (parent_token, parent_broker_token) = zx::EventPair::create();
    let parent_cred = Credential {
        broker_token: parent_broker_token,
        permissions: Permissions::MODIFY_DEPENDENT,
    };
    let parent = topology.add_element("P", vec![parent_cred]).await?.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: child_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for P should be OFF.
    // Update P's current level to OFF with PowerBroker.
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let power_level = status.get_power_level(&parent).await?.expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Acquire lease for C, P should now have required level ON
    let lease_id = lessor
        .lease(&child, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let parent_req_level =
        level_control.watch_required_level(&parent, Some(&parent_req_level)).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update P's current level to ON. Lease should now be active.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease, P should now have required level OFF
    lessor.drop_lease(&lease_id).expect("drop failed");
    let parent_req_level =
        level_control.watch_required_level(&parent, Some(&parent_req_level)).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    Ok(())
}

#[fuchsia::test]
async fn test_transitive() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology with the following dependencies:
    // C depends on B, which in turn depends on A.
    // D has no dependencies or dependents.
    // A <- B <- C   D
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (element_a_token, element_a_broker_token) = zx::EventPair::create();
    let element_a_cred = Credential {
        broker_token: element_a_broker_token,
        permissions: Permissions::MODIFY_DEPENDENT,
    };
    let element_a =
        topology.add_element("A", vec![element_a_cred]).await?.expect("add_element failed");
    let (element_b_token, element_b_broker_token) = zx::EventPair::create();
    let element_b_cred = Credential {
        broker_token: element_b_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
    };
    let element_b =
        topology.add_element("B", vec![element_b_cred]).await?.expect("add_element failed");
    let (element_c_token, element_c_broker_token) = zx::EventPair::create();
    let element_c_cred = Credential {
        broker_token: element_c_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    let element_c =
        topology.add_element("C", vec![element_c_cred]).await?.expect("add_element failed");
    let (_, element_d_broker_token) = zx::EventPair::create();
    let element_d_cred =
        Credential { broker_token: element_d_broker_token, permissions: Permissions::empty() };
    let element_d =
        topology.add_element("D", vec![element_d_cred]).await?.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: element_b_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: element_a_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: element_c_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: element_b_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for each element should be OFF.
    // Update managed elements' current level to OFF with PowerBroker.
    for element_id in [&element_a, &element_b, &element_d] {
        let req_level = level_control.watch_required_level(element_id, None).await?;
        assert_eq!(req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        level_control
            .update_current_power_level(&element_id, &PowerLevel::Binary(BinaryPowerLevel::Off))
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C with PB, Initially, A should have required level ON
    // and B should have required level OFF because C has a dependency on B
    // and B has a dependency on A.
    // D should still have required level OFF.
    let lease_id = lessor
        .lease(&element_c, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control.watch_required_level(&element_b, None).await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update A's current level to ON. Now B's required level should become ON
    // because its dependency on A is unblocked.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_a, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control
        .watch_required_level(&element_a, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control.watch_required_level(&element_b, None).await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update B's current level to ON.
    // Both A and B should have required_level ON.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_b, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease for C with PB, B should have required level OFF.
    // A should still have required level ON.
    // D should still have required level OFF.
    lessor.drop_lease(&lease_id).expect("drop failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Lower B's current level to OFF
    // Both A and B should have required level OFF.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_b, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    Ok(())
}

#[fuchsia::test]
async fn test_shared() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a topology of two child elements with a shared
    // parent and grandparent
    // C1 \
    //     > P -> GP
    // C2 /
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (child1_token, child1_broker_token) = zx::EventPair::create();
    let child1_cred = Credential {
        broker_token: child1_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    let child1 = topology.add_element("C1", vec![child1_cred]).await?.expect("add_element failed");
    let (child2_token, child2_broker_token) = zx::EventPair::create();
    let child2_cred = Credential {
        broker_token: child2_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    let child2 = topology.add_element("C2", vec![child2_cred]).await?.expect("add_element failed");
    let (parent_token, parent_broker_token) = zx::EventPair::create();
    let parent_cred = Credential {
        broker_token: parent_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY | Permissions::MODIFY_DEPENDENT,
    };
    let parent = topology.add_element("P", vec![parent_cred]).await?.expect("add_element failed");
    let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
    let grandparent_cred = Credential {
        broker_token: grandparent_broker_token,
        permissions: Permissions::MODIFY_DEPENDENT,
    };
    let grandparent =
        topology.add_element("GP", vec![grandparent_cred]).await?.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: child1_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: child2_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: grandparent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for each element should be OFF.
    // Update all elements' current level to OFF with PowerBroker.
    for element_id in [&parent, &grandparent] {
        let req_level = level_control.watch_required_level(element_id, None).await?;
        assert_eq!(req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        level_control
            .update_current_power_level(&element_id, &PowerLevel::Binary(BinaryPowerLevel::Off))
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C1. Initially, GP should have required level ON
    // and P should have required level OFF because C1 has a dependency on P
    // and P has a dependency on GP. GP has no dependencies so it should be
    // turned on first.
    let lease_child_1 = lessor
        .lease(&child1, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let grandparent_req_level = level_control
        .watch_required_level(&grandparent, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update GP's current level to ON. Now P's required level should become ON
    // because its dependency on GP is unblocked.
    level_control
        .update_current_power_level(&grandparent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update P's current level to ON.
    // Both P and GP should have required_level ON.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should now be active.

    // Acquire lease for C2, P and GP should still have required_level ON.
    let lease_child_2 = lessor
        .lease(&child2, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should immediately be active.

    // Drop lease for C1, P and GP should still have required_level ON
    // because of lease on C2.
    lessor.drop_lease(&lease_child_1).expect("drop failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease 2 should still be active.

    // Drop lease for C2, P should have required level OFF.
    // GP should still have required level ON.
    lessor.drop_lease(&lease_child_2).expect("drop failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Lower P's current level to OFF
    // Both P and GP should have required level OFF.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    Ok(())
}

#[fuchsia::test]
async fn test_topology() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology.
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (water_token, water_broker_token) = zx::EventPair::create();
    let water_cred = Credential {
        broker_token: water_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    topology.add_element("Water", vec![water_cred]).await?.expect("add_element failed");
    let (earth_token, earth_broker_token) = zx::EventPair::create();
    let earth_cred =
        Credential { broker_token: earth_broker_token, permissions: Permissions::MODIFY_DEPENDENT };
    topology.add_element("Earth", vec![earth_cred]).await?.expect("add_element failed");
    let (fire_token, fire_broker_token) = zx::EventPair::create();
    let fire_cred =
        Credential { broker_token: fire_broker_token, permissions: Permissions::REMOVE_ELEMENT };
    topology.add_element("Fire", vec![fire_cred]).await?.expect("add_element failed");
    let (air_token, air_broker_token) = zx::EventPair::create();
    let air_cred = Credential { broker_token: air_broker_token, permissions: Permissions::all() };
    let (air_token_no_perms, air_broker_token_no_perms) = zx::EventPair::create();
    let air_cred_no_perms =
        Credential { broker_token: air_broker_token_no_perms, permissions: Permissions::empty() };
    topology
        .add_element("Air", vec![air_cred, air_cred_no_perms])
        .await?
        .expect("add_element failed");
    topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let extra_add_dep_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_add_dep_res, Err(fpb::AddDependencyError::AlreadyExists { .. })));

    topology
        .remove_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("remove_dependency failed");

    let extra_remove_dep_res = topology
        .remove_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_remove_dep_res, Err(fpb::RemoveDependencyError::NotFound { .. })));

    topology
        .remove_element(fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"))
        .await?
        .expect("remove_element failed");
    let remove_element_not_authorized_res = topology
        .remove_element(
            air_token_no_perms.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
        )
        .await?;
    assert!(matches!(
        remove_element_not_authorized_res,
        Err(fpb::RemoveElementError::NotAuthorized)
    ));
    topology
        .remove_element(air_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"))
        .await?
        .expect("remove_element failed");

    let add_dep_element_not_found_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: air_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(add_dep_element_not_found_res, Err(fpb::AddDependencyError::NotAuthorized)));

    let add_dep_req_element_not_found_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(
        add_dep_req_element_not_found_res,
        Err(fpb::AddDependencyError::NotAuthorized)
    ));

    Ok(())
}

#[fuchsia::test]
async fn test_register_unregister_credentials() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (token_element_owner, token_element_broker) = zx::EventPair::create();
    let broker_credential = fpb::Credential {
        broker_token: token_element_broker,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT
            | Permissions::MODIFY_DEPENDENCY
            | Permissions::MODIFY_CREDENTIAL
            | Permissions::REMOVE_ELEMENT,
    };
    topology.add_element("element", vec![broker_credential]).await?.expect("add_element failed");
    let (token_new_owner, token_new_broker) = zx::EventPair::create();
    let credential_to_register = fpb::Credential {
        broker_token: token_new_broker,
        permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
    };
    topology
        .register_credentials(
            token_element_owner
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("duplicate_handle failed"),
            vec![credential_to_register],
        )
        .await?
        .expect("register_credentials failed");

    let (_, token_not_authorized_broker) = zx::EventPair::create();
    let credential_not_authorized = fpb::Credential {
        broker_token: token_not_authorized_broker,
        permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
    };
    let res_not_authorized = topology
        .register_credentials(
            token_new_owner
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("duplicate_handle failed"),
            vec![credential_not_authorized],
        )
        .await?;
    assert!(matches!(res_not_authorized, Err(fpb::RegisterCredentialsError::NotAuthorized)));

    topology
        .unregister_credentials(token_element_owner, vec![token_new_owner])
        .await?
        .expect("unregister_credentials failed");

    Ok(())
}
