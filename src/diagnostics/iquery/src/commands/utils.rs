// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    commands::{types::DiagnosticsProvider, Command, ListCommand},
    types::Error,
};
use anyhow::anyhow;
use cm_rust::SourceName;
use component_debug::realm::*;
use fidl_fuchsia_sys2 as fsys2;
use lazy_static::lazy_static;
use moniker::{Moniker, MonikerBase};
use regex::Regex;

lazy_static! {
    static ref EXPECTED_PROTOCOL_RE: Regex =
        Regex::new(r".*fuchsia\.diagnostics\..*ArchiveAccessor$").unwrap();
}

/// Returns the selectors for a component whose url contains the `manifest` string.
pub async fn get_selectors_for_manifest<P: DiagnosticsProvider>(
    manifest: &Option<String>,
    tree_selectors: &Vec<String>,
    accessor: &Option<String>,
    provider: &P,
) -> Result<Vec<String>, Error> {
    let Some(manifest) = manifest.as_ref() else {
        return Ok(tree_selectors.clone());
    };
    let list_command = ListCommand {
        manifest: Some(manifest.clone()),
        with_url: false,
        accessor: accessor.clone(),
    };
    let monikers = list_command
        .execute(provider)
        .await?
        .into_inner()
        .into_iter()
        .map(|item| item.into_moniker())
        .collect::<Vec<_>>();
    if monikers.is_empty() {
        Err(Error::ManifestNotFound(manifest.clone()))
    } else if tree_selectors.is_empty() {
        Ok(monikers.into_iter().map(|moniker| format!("{}:root", moniker)).collect())
    } else {
        Ok(monikers
            .into_iter()
            .flat_map(|moniker| {
                tree_selectors
                    .iter()
                    .map(move |tree_selector| format!("{}:{}", moniker, tree_selector))
            })
            .collect())
    }
}

/// Expand selectors.
pub fn expand_selectors(selectors: Vec<String>) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    for selector in selectors {
        match selectors::tokenize_string(&selector, selectors::SELECTOR_DELIMITER) {
            Ok(tokens) => {
                if tokens.len() > 1 {
                    result.push(selector);
                } else if tokens.len() == 1 {
                    result.push(format!("{}:*", selector));
                } else {
                    return Err(Error::InvalidArguments(format!(
                        "Iquery selectors cannot be empty strings: {:?}",
                        selector
                    )));
                }
            }
            Err(e) => {
                return Err(Error::InvalidArguments(format!(
                    "Tokenizing a provided selector failed. Error: {:?} Selector: {:?}",
                    e, selector
                )));
            }
        }
    }
    Ok(result)
}

/// Helper method to get all `InstanceInfo` from the `RealmExplorer`.
pub(crate) async fn get_instance_infos(
    realm_query: &fsys2::RealmQueryProxy,
) -> Result<Vec<Instance>, Error> {
    component_debug::realm::get_all_instances(realm_query)
        .await
        .map_err(|e| Error::CommunicatingWith("RealmExplorer".to_owned(), anyhow!("{:?}", e)))
}

/// Helper method to normalize a moniker into its canonical string form. Returns
/// the input moniker unchanged if it cannot be parsed.
pub fn normalize_moniker(moniker: &str) -> String {
    Moniker::parse_str(moniker).map_or(String::from(moniker), |m| m.to_string())
}

/// Get all the exposed `ArchiveAccessor` from any child component which
/// directly exposes them or places them in its outgoing directory.
pub async fn get_accessor_selectors(
    realm_query: &fsys2::RealmQueryProxy,
) -> Result<Vec<String>, Error> {
    let mut result = vec![];
    let instances = get_all_instances(realm_query).await?;
    for instance in instances {
        match get_resolved_declaration(&instance.moniker, realm_query).await {
            Ok(decl) => {
                for capability in decl.capabilities {
                    let capability_name = capability.name().to_string();
                    if !EXPECTED_PROTOCOL_RE.is_match(&capability_name) {
                        continue;
                    }
                    // Skip .host accessors.
                    if capability_name.contains(".host") {
                        continue;
                    }
                    if decl.exposes.iter().any(|expose| expose.source_name() == capability.name()) {
                        let moniker_str = instance.moniker.to_string();
                        let moniker = selectors::sanitize_moniker_for_selectors(&moniker_str);
                        result.push(format!("{moniker}:expose:{capability_name}"));
                    }
                }
            }
            Err(GetDeclarationError::InstanceNotFound(_))
            | Err(GetDeclarationError::InstanceNotResolved(_)) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod test {
    use {
        super::*, assert_matches::assert_matches, iquery_test_support::MockRealmQuery,
        std::sync::Arc,
    };

    #[fuchsia::test]
    async fn test_get_accessors() {
        let fake_realm_query = Arc::new(MockRealmQuery::default());
        let realm_query = Arc::clone(&fake_realm_query).get_proxy().await;

        let res = get_accessor_selectors(&realm_query).await;

        assert_matches!(res, Ok(_));

        assert_eq!(
            res.unwrap(),
            vec![
                String::from("example/component:expose:fuchsia.diagnostics.ArchiveAccessor"),
                String::from(
                    "foo/bar/thing\\:instance:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"
                ),
                String::from("foo/component:expose:fuchsia.diagnostics.FeedbackArchiveAccessor"),
            ]
        );
    }
}
