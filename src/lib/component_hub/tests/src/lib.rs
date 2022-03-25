// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    component_hub::io::Directory,
    component_hub::{list, select, show},
    moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
    regex::Regex,
    std::path::PathBuf,
};

const URL_REGEX: &str = r"fuchsia-pkg://fuchsia\.com/component_hub_integration_tests(\?hash=[0-9a-f]{64})?#meta/test\.cm";

#[fuchsia_async::run_singlethreaded(test)]
async fn list() {
    let hub_path = PathBuf::from("/hub");
    let hub_dir = Directory::from_namespace(hub_path).unwrap();

    let list::Component { name, is_cmx, is_running, url, children, ancestors: _ancestors } =
        list::Component::parse("test_root".to_string(), hub_dir).await.unwrap();

    assert!(!is_cmx);
    assert!(is_running);
    assert_eq!(name, "test_root");
    assert!(Regex::new(URL_REGEX).unwrap().is_match(&url));
    assert_eq!(children.len(), 1);

    let list::Component { name, is_cmx, is_running, url, children, ancestors: _ancestors } =
        children.get(0).unwrap();
    assert_eq!(name, "foo");
    assert!(!is_running);
    assert!(!is_cmx);
    assert_eq!(url, "#meta/foo.cm");
    assert!(children.is_empty());
}

#[fuchsia_async::run_singlethreaded(test)]
async fn show() {
    let hub_path = PathBuf::from("/hub");
    let hub_dir = Directory::from_namespace(&hub_path).unwrap();

    let components = show::find_components("test.cm".to_string(), hub_dir).await.unwrap();

    assert_eq!(components.len(), 1);
    let component = &components[0];

    assert!(Regex::new(URL_REGEX).unwrap().is_match(&component.url));

    assert!(component.moniker.is_root());
    assert_eq!(component.component_type, "CML static component");

    assert!(component.resolved.is_some());
    let resolved = component.resolved.as_ref().unwrap();

    // capabilities are sorted alphabetically
    assert_eq!(
        &vec!["fuchsia.foo.Bar", "fuchsia.logger.LogSink", "hub", "pkg"],
        &resolved.incoming_capabilities
    );

    assert_eq!(resolved.config.len(), 2);
    let field1 = &resolved.config[0];
    let field2 = &resolved.config[1];
    assert_eq!(field1.key, "my_string");
    assert_eq!(field1.value, "\"hello, world!\"");
    assert_eq!(field2.key, "my_uint8");
    assert_eq!(field2.value, "255");

    // We do not verify the contents of the execution, because they are largely dependent on
    // the Rust Test Runner
    assert!(component.execution.is_some());

    let hub_dir = Directory::from_namespace(&hub_path).unwrap();
    let components = show::find_components("foo.cm".to_string(), hub_dir).await.unwrap();
    assert_eq!(components.len(), 1);
    let component = &components[0];
    assert_eq!(component.moniker, AbsoluteMoniker::parse_str("/foo").unwrap());
    assert_eq!(component.url, "#meta/foo.cm");
    assert_eq!(component.component_type, "CML static component");
    assert!(component.resolved.is_none());
    assert!(component.execution.is_none());
}

#[fuchsia_async::run_singlethreaded(test)]
async fn select() {
    let hub_path = PathBuf::from("/hub");
    let hub_dir = Directory::from_namespace(hub_path).unwrap();

    let select::MatchingComponents { mut exposed, mut used } =
        select::find_components("fuchsia.foo.Bar".to_string(), hub_dir).await.unwrap();

    assert_eq!(exposed.len(), 1);
    assert_eq!(used.len(), 1);
    let exposed_component = exposed.remove(0);
    let used_component = used.remove(0);
    assert!(exposed_component.is_root());
    assert!(used_component.is_root());

    let hub_path = PathBuf::from("/hub");
    let hub_dir = Directory::from_namespace(hub_path).unwrap();

    let select::MatchingComponents { mut exposed, used } =
        select::find_components("minfs".to_string(), hub_dir).await.unwrap();

    assert_eq!(exposed.len(), 1);
    assert_eq!(used.len(), 0);
    let exposed_component = exposed.remove(0);
    assert!(exposed_component.is_root());
}

#[fuchsia_async::run_singlethreaded(test)]
async fn clone() {
    let hub_path = PathBuf::from("/hub");
    let hub_dir = Directory::from_namespace(hub_path).unwrap();
    assert!(hub_dir.clone().is_ok());
}
