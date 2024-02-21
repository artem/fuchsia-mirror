// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    proc_macro::TokenStream,
    version_history_shared::{version_history, Status},
};

#[proc_macro]
pub fn declare_version_history(_tokens: TokenStream) -> TokenStream {
    let versions = version_history().expect("version-history.json to be parsed");
    if versions.is_empty() {
        panic!("version-history.json did not contain any versions");
    }

    let mut tokens = String::from("[");
    for version in versions {
        tokens.push_str(&format!(
            "Version {{ api_level: ApiLevel({}), abi_revision: AbiRevision({}), status: Status::{:?} }},",
            version.api_level, version.abi_revision.value, version.status
        ));
    }
    tokens.push_str("]");

    tokens.parse().unwrap()
}

#[proc_macro]
pub fn latest_sdk_version(_tokens: TokenStream) -> TokenStream {
    let versions = version_history().expect("version-history.json to be parsed");
    let latest_version =
        versions.last().expect("version-history.json did not contain any versions");
    format!(
        "Version {{ api_level: ApiLevel({}), abi_revision: AbiRevision({}), status: Status::{:?} }}",
        latest_version.api_level, latest_version.abi_revision.value, latest_version.status
    )
    .parse()
    .unwrap()
}

#[proc_macro]
pub fn get_supported_versions(_tokens: TokenStream) -> TokenStream {
    let versions = version_history().expect("version-history.json to be parsed");

    let supported_versions = versions
        .iter()
        .filter(|version| matches!(version.status, Status::InDevelopment | Status::Supported))
        .collect::<Vec<_>>();

    if supported_versions.is_empty() {
        panic!("version-history.json did not contain any supported versions");
    } else if supported_versions.len() < 2 {
        panic!("version-history.json did not contain at least two supported versions");
    }

    let mut tokens = String::from("[");
    for version in supported_versions {
        tokens.push_str(&format!(
            "Version {{ api_level: ApiLevel({}), abi_revision: AbiRevision({}), status: Status::{:?} }}",
            version.api_level, version.abi_revision.value, version.status
        ));
        tokens.push_str(",");
    }
    tokens.push_str("]");
    tokens.parse().unwrap()
}
