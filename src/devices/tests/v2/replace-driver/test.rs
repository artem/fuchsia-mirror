// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error, Result},
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_registrar as fdr,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_pkg as fp, fidl_fuchsia_reloaddriver_test as ft,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{ChildOptions, LocalComponentHandles, RealmBuilder},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon as zx,
    futures::{channel::mpsc, StreamExt, TryStreamExt},
    std::collections::{HashMap, HashSet},
};

const WAITER_NAME: &'static str = "waiter";

async fn waiter_serve(
    mut stream: ft::WaiterRequestStream,
    mut sender: mpsc::Sender<(String, String)>,
) {
    while let Some(ft::WaiterRequest::Ack { from_node, from_name, status, .. }) =
        stream.try_next().await.expect("Stream failed")
    {
        assert_eq!(status, zx::Status::OK.into_raw());
        sender.try_send((from_node, from_name)).expect("Sender failed")
    }
}

async fn waiter_component(
    handles: LocalComponentHandles,
    sender: mpsc::Sender<(String, String)>,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream: ft::WaiterRequestStream| {
        fasync::Task::spawn(waiter_serve(stream, sender.clone())).detach()
    });
    fs.serve_connection(handles.outgoing_dir)?;
    Ok(fs.collect::<()>().await)
}

fn send_get_device_info_request(
    service: &fdd::DriverDevelopmentProxy,
    device_filter: &[String],
    exact_match: bool,
) -> Result<fdd::DeviceInfoIteratorProxy> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::DeviceInfoIteratorMarker>()?;

    service
        .get_device_info(device_filter, iterator_server, exact_match)
        .context("FIDL call to get device info failed")?;

    Ok(iterator)
}

async fn get_device_info(
    service: &fdd::DriverDevelopmentProxy,
    device_filter: &[String],
    exact_match: bool,
) -> Result<Vec<fdd::DeviceInfo>> {
    let iterator = send_get_device_info_request(service, device_filter, exact_match)?;

    let mut device_infos = Vec::new();
    loop {
        let mut device_info =
            iterator.get_next().await.context("FIDL call to get device info failed")?;
        if device_info.len() == 0 {
            break;
        }
        device_infos.append(&mut device_info);
    }
    Ok(device_infos)
}

// Wait for the events from the |nodes| to be received. Updates the entries to be Some.
async fn wait_for_nodes(
    nodes: &mut HashMap<String, Option<Option<u64>>>,
    receiver: &mut mpsc::Receiver<(String, String)>,
) -> Result<()> {
    while nodes.values().any(|&x| x.is_none()) {
        let (from_node, _) = receiver.next().await.ok_or(anyhow!("Receiver failed"))?;
        if !nodes.contains_key(&from_node) {
            return Err(anyhow!("Couldn't find node '{}' in 'nodes'.", from_node.to_string()));
        }
        nodes.entry(from_node).and_modify(|x| {
            *x = Some(None);
        });
    }

    Ok(())
}

// Validates the host koids given the device infos.
// Performs the following:
// - Stores the host koid for nodes in changed_or_new as those are expected to be new/changed.
// - Validate bound nodes are not in should_not_exist
// - Validate bound nodes have the same koid as the most recent item in previous.
// - Validate that items in changed_or_new have been changed from the most recent item in previous
//   if they are not new.
async fn validate_host_koids(
    test_stage_name: &str,
    device_infos: Vec<fdd::DeviceInfo>,
    changed_or_new: &mut HashMap<String, Option<Option<u64>>>,
    previous: Vec<&HashMap<String, Option<Option<u64>>>>,
    should_not_exist: Option<&HashSet<String>>,
) -> Result<()> {
    for dev in &device_infos {
        let key = dev.moniker.clone().unwrap().split(".").last().unwrap().to_string();

        // Items in changed_or_new are expected to be different so just save that info and move on.
        if changed_or_new.contains_key(&key) {
            changed_or_new.entry(key).and_modify(|x| {
                *x = Some(dev.driver_host_koid);
            });

            continue;
        }

        // Skip comparison and should_not_exist check as the koid is not valid when its unbound.
        if dev.bound_driver_url == Some("unbound".to_string()) {
            continue;
        }

        // Error if the item is in should not exist.
        if let Some(should_not_exist_value) = &should_not_exist {
            if should_not_exist_value.contains(&key) {
                return Err(anyhow!(
                    "Found node that should not exist after {}: '{}'.",
                    test_stage_name,
                    key
                ));
            }
        }

        // Go through the previous items (which should come in from most to least recent order)
        // and make sure this matches the most recent instance.
        for prev in &previous {
            if let Some(prev_koid) = prev.get(&key) {
                match prev_koid {
                    Some(prev_koid_value) => {
                        if *prev_koid_value != dev.driver_host_koid {
                            return Err(anyhow!(
                                "koid should not have changed for node '{}' after {}.",
                                key,
                                test_stage_name
                            ));
                        }

                        break;
                    }
                    None => {
                        return Err(anyhow!("prev koid not available after."));
                    }
                }
            }
        }
    }

    // Now we can make sure those items in changed_or_new are diffent than their most recent
    // previous item. Skipping ones that are not seen in previous.
    for changed_or_new_node in changed_or_new {
        let key = changed_or_new_node.0;
        let mut koid_before = None;
        for prev in &previous {
            if let Some(prev_at_key) = prev.get(key) {
                match prev_at_key {
                    Some(prev_at_key_value) => {
                        koid_before = Some(prev_at_key_value);
                        break;
                    }
                    None => {
                        return Err(anyhow!("previous map entry cannot have None inner option."));
                    }
                }
            }
        }

        match koid_before {
            Some(koid_before) => match changed_or_new_node.1 {
                Some(koid_after) => {
                    if koid_before == koid_after {
                        return Err(anyhow!(
                            "koid should have changed for node '{}' after {}.",
                            changed_or_new_node.0,
                            test_stage_name
                        ));
                    }
                }
                None => {
                    return Err(anyhow!("changed_or_new_node entry cannot be None."));
                }
            },
            None => {
                continue;
            }
        }
    }

    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_replace_target() -> Result<()> {
    let (sender, mut receiver) = mpsc::channel(1);

    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    let waiter = builder
        .add_local_child(
            WAITER_NAME,
            move |handles: LocalComponentHandles| {
                Box::pin(waiter_component(handles, sender.clone()))
            },
            ChildOptions::new(),
        )
        .await?;
    builder.driver_test_realm_add_offer::<ft::WaiterMarker>((&waiter).into()).await?;
    // Build the Realm.
    let instance = builder.build().await?;

    let offers = vec![
        fdt::Offer {
            protocol_name: ft::WaiterMarker::PROTOCOL_NAME.to_string(),
            collection: fdt::Collection::BootDrivers,
        },
        fdt::Offer {
            protocol_name: ft::WaiterMarker::PROTOCOL_NAME.to_string(),
            collection: fdt::Collection::PackageDrivers,
        },
    ];

    // Start the DriverTestRealm.
    let args = fdt::RealmArgs {
        root_driver: Some("fuchsia-boot:///#meta/root.cm".to_string()),
        use_driver_framework_v2: Some(true),
        offers: Some(offers),
        driver_disable: Some(vec!["fuchsia-boot:///#meta/target_2_replacement.cm".to_string()]),
        ..Default::default()
    };
    instance.driver_test_realm_start(args).await?;

    let driver_dev =
        instance.root.connect_to_protocol_at_exposed_dir::<fdd::DriverDevelopmentMarker>()?;
    let driver_registrar =
        instance.root.connect_to_protocol_at_exposed_dir::<fdr::DriverRegistrarMarker>()?;

    // This maps nodes to Option<Option<u64>>. The outer option is whether the node has been seen
    // yet (if composite parent we start with `Some` for this since we don't receive acks
    // from them). The inner option is the driver host koid.
    let mut nodes = HashMap::from([
        ("dev".to_string(), None),
        ("B".to_string(), None),
        ("C".to_string(), None),
        ("D".to_string(), Some(None)), // composite parent
        ("E".to_string(), Some(None)), // composite parent
        ("F".to_string(), Some(None)), // composite parent
        ("G".to_string(), None),
        ("H".to_string(), None),
        ("I".to_string(), None),
        ("J".to_string(), None),
        ("K".to_string(), None),
    ]);

    // First we want to wait for all the nodes.
    wait_for_nodes(&mut nodes, &mut receiver).await?;

    // Now we collect their initial driver host koids.
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
    validate_host_koids("init", device_infos, &mut nodes, vec![], None).await?;

    // Let's disable the first target driver.
    let target_1_url =
        fp::PackageUrl { url: "fuchsia-boot:///#meta/target_1_no_colocate.cm".to_string() };
    let disable_result = driver_dev.disable_match_with_driver_url(&target_1_url).await;
    if disable_result.is_err() {
        return Err(anyhow!("Failed to disable target_1_no_colocate."));
    }
    // Now we can restart the first target driver with the rematch flag.
    let restart_result = driver_dev
        .restart_driver_hosts(target_1_url.url.as_str(), fdd::RematchFlags::REQUESTED)
        .await?;
    if restart_result.is_err() {
        return Err(anyhow!("Failed to restart target_1."));
    }

    // These are the nodes that should be started.
    // 'G' is the node that was bound to our target.
    // 'Z' is the node that the replacement creates.
    let mut nodes_after_restart = HashMap::from([("G".to_string(), None), ("Z".to_string(), None)]);

    // Wait for them to start.
    wait_for_nodes(&mut nodes_after_restart, &mut receiver).await?;

    // These nodes should not exist anymore.
    // 'I' was the child of the driver being replaced.
    let should_not_exist_after_restart = HashSet::from([("I".to_string())]);

    // Collect the new driver host koids.
    // Ensure same koid if not one of the ones expected to restart.
    // Make sure the host koid has changed from before the restart for the nodes that should have
    // restarted.
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
    validate_host_koids(
        "first restart",
        device_infos,
        &mut nodes_after_restart,
        vec![&nodes],
        Some(&should_not_exist_after_restart),
    )
    .await?;

    // Now let's disable the second target driver.
    let target_2_url = fp::PackageUrl { url: "fuchsia-boot:///#meta/target_2.cm".to_string() };
    let disable_2_result = driver_dev.disable_match_with_driver_url(&target_2_url).await;
    if disable_2_result.is_err() {
        return Err(anyhow!("Failed to disable target_2."));
    }
    // Now we can restart the second target driver with the rematch flag.
    let restart_result = driver_dev
        .restart_driver_hosts(target_2_url.url.as_str(), fdd::RematchFlags::REQUESTED)
        .await?;
    if restart_result.is_err() {
        return Err(anyhow!("Failed to restart target_2."));
    }

    // These are the nodes that should be restarted after the second restart.
    let mut nodes_after_restart_2 = HashMap::from([("H".to_string(), None)]);

    // Wait for them to come back again.
    wait_for_nodes(&mut nodes_after_restart_2, &mut receiver).await?;

    // At this point we should have lost the following nodes as we have disabled the target driver
    // for 'J' and don't have a replacement driver yet.
    let should_not_exist_after_restart_2 = HashSet::from(["J".to_string(), "K".to_string()]);

    // Collect the newer driver host koids.
    // Ensure same koid if not one of the ones expected to restart (comparing to most recent one).
    // Make sure the host koid has changed from before the second restart for the nodes that should
    // have restarted.
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
    validate_host_koids(
        "second restart",
        device_infos,
        &mut nodes_after_restart_2,
        vec![&nodes_after_restart, &nodes],
        Some(&should_not_exist_after_restart_2),
    )
    .await?;

    // Now we can register our target_2 replacement.
    let target_2_replacemnt_url = fp::PackageUrl {
        url: "fuchsia-pkg://fuchsia.com/target_2_replacement#meta/target_2_replacement.cm"
            .to_string(),
    };
    let register_result = driver_registrar.register(&target_2_replacemnt_url).await;
    match register_result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            return Err(anyhow!("Failed to register target_2 replacement: {}.", err));
        }
        Err(err) => {
            return Err(anyhow!("Failed to register target_2 replacement: {}.", err));
        }
    };
    // And now that we have registered the replacement we call to bind all available nodes.
    let bind_result = driver_dev.bind_all_unbound_nodes().await;
    match bind_result {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => {
            return Err(anyhow!("Failed to bind_all_unbound_nodes: {}.", err));
        }
        Err(err) => {
            return Err(anyhow!("Failed to bind_all_unbound_nodes: {}.", err));
        }
    };

    // These are the nodes we should get started now that we have the replacement in for 2.
    let mut nodes_after_register =
        HashMap::from([("J".to_string(), None), ("Y".to_string(), None)]);

    // Wait for them to come up.
    wait_for_nodes(&mut nodes_after_register, &mut receiver).await?;

    // These should not exist after our register call.
    let should_not_exist_after_register = HashSet::from(["K".to_string()]);

    // Collect the newer driver host koids.
    // Ensure same koid if not one of the ones expected to restart (comparing to most recent one).
    // Make sure the host koid has changed from before the register.
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;
    validate_host_koids(
        "register",
        device_infos,
        &mut nodes_after_register,
        vec![&nodes_after_restart_2, &nodes_after_restart, &nodes],
        Some(&should_not_exist_after_register),
    )
    .await?;

    Ok(())
}
