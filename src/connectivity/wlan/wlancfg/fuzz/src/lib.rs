// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    futures::channel::mpsc,
    fuzz::fuzz,
    tempfile::TempDir,
    wlan_common::assert_variant,
    wlancfg_lib::{
        config_management::{
            network_config::{Credential, NetworkIdentifier},
            SavedNetworksManager, SavedNetworksManagerApi,
        },
        telemetry::{TelemetryEvent, TelemetrySender},
    },
};

#[fuzz]
async fn fuzz_saved_networks_manager_store(id: NetworkIdentifier, credential: Credential) {
    // Test with fuzzed inputs that if a network is stored, we can look it up again and if it is
    // loaded from stash when SavedNetworksManager is initialized from stash the values of the
    // saved network are correct.
    let stash_id = "store_and_lookup";

    // Expect the store to be constructed successfully even if the file doesn't
    // exist yet
    let saved_networks = create_saved_networks(stash_id).await;

    assert!(saved_networks.lookup(&id).await.is_empty());
    assert_eq!(0, saved_networks.known_network_count().await);

    // Store a fuzzed network identifier and credential.
    assert!(saved_networks
        .store(id.clone(), credential.clone())
        .await
        .expect("storing network failed")
        .is_none());
    assert_variant!(saved_networks.lookup(&id).await.as_slice(),
        [network_config] => {
            assert_eq!(network_config.ssid, id.ssid);
            assert_eq!(network_config.security_type, id.security_type);
            assert_eq!(network_config.credential, credential);
        }
    );
    assert_eq!(1, saved_networks.known_network_count().await);

    // Saved networks should persist when we create a saved networks manager with the same ID.
    let saved_networks = create_saved_networks(stash_id).await;
    assert_variant!(saved_networks.lookup(&id).await.as_slice(),
        [network_config] => {
            assert_eq!(network_config.ssid, id.ssid);
            assert_eq!(network_config.security_type, id.security_type);
            assert_eq!(network_config.credential, credential);
        }
    );
    assert_eq!(1, saved_networks.known_network_count().await);
}

/// Create a saved networks manager with the specified stash ID. Stash ID should be different for
/// each test so that they don't interfere.
async fn create_saved_networks(stash_id: impl AsRef<str>) -> SavedNetworksManager {
    // This file doesn't really matter, it will only be deleted if it exists.
    let temp_dir = TempDir::new().expect("failed to create temporary directory");
    let path = temp_dir.path().join("networks.json");
    let (telemetry_sender, _telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
    let telemetry_sender = TelemetrySender::new(telemetry_sender);
    let stash = wlan_stash::policy::PolicyStash::new_with_id(stash_id.as_ref())
        .expect("failed to initialize WLAN policy stash");

    let saved_networks =
        SavedNetworksManager::new_with_stash_or_paths(Some(stash), path, telemetry_sender).await;
    saved_networks
}
