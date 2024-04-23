// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

pub const NODE_SEPARATOR: &'static str = "#/@";
pub const POLICY_STASH_PREFIX: &str = "config";
/// The StashNode abstraction requires that writing to a StashNode is done as a named field,
/// so we will store the network config's data under the POLICY_DATA_KEY.
pub const POLICY_DATA_KEY: &str = "data";
pub const POLICY_STORAGE_ID: &str = "saved_networks";

pub type StashedSsid = Vec<u8>;

/// The data that will be stored between reboots of a device. Used to convert the data between JSON
/// and network config.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PersistentData {
    pub credential: Credential,
    pub has_ever_connected: bool,
}

/// The network identifier is the SSID and security policy of the network, and it is used to
/// distinguish networks. It mirrors the NetworkIdentifier in fidl_fuchsia_wlan_policy.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct NetworkIdentifier {
    pub ssid: StashedSsid,
    pub security_type: SecurityType,
}

/// The security type of a network connection. It mirrors the fidl_fuchsia_wlan_policy SecurityType
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SecurityType {
    None,
    Wep,
    Wpa,
    Wpa2,
    Wpa3,
}

/// The credential of a network connection. It mirrors the fidl_fuchsia_wlan_policy Credential
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Credential {
    None,
    Password(Vec<u8>),
    Psk(Vec<u8>),
}

/// To deserialize file data into a JSON with a file version and data, a wrapper is needed since
/// the values of the hashmap must be consistent.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum FileContent {
    Version(u8),
    Networks(Vec<PersistentStorageData>),
}

/// The data that will be stored between reboots of a device. Used to convert the data between JSON
/// and network config.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PersistentStorageData {
    pub ssid: StashedSsid,
    pub security_type: SecurityType,
    pub credential: Credential,
    #[serde(default = "has_ever_connected_default")]
    pub has_ever_connected: bool,
}

/// Defines the default value of has_ever_connected in persisted data. This is used so that the
/// config could be loaded even if this field is missing.
fn has_ever_connected_default() -> bool {
    false
}

impl PersistentStorageData {
    /// Used when migrating persisted networks from deprecated stash to the new local storage format.
    pub fn new_from_legacy_data(
        id: NetworkIdentifier,
        data: PersistentData,
    ) -> PersistentStorageData {
        PersistentStorageData {
            ssid: id.ssid.clone(),
            security_type: id.security_type,
            credential: data.credential,
            has_ever_connected: data.has_ever_connected,
        }
    }
}

/// Used when migrating persisted networks from deprecated stash to the new local storage format.
pub fn new_persisted_data_from_old(
    id: NetworkIdentifier,
    data: Vec<PersistentData>,
) -> Vec<PersistentStorageData> {
    data.into_iter()
        .map(|c| PersistentStorageData {
            ssid: id.ssid.clone(),
            security_type: id.security_type,
            credential: c.credential,
            has_ever_connected: c.has_ever_connected,
        })
        .collect()
}
