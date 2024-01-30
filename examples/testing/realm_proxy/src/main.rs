// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fidl_examples_routing_echo as fecho;
use fidl_test_echoserver as ftest;
use fuchsia_component::client::{
    connect_to_protocol, connect_to_protocol_at, connect_to_protocol_at_path,
};
use realm_proxy_client::{extend_namespace, InstalledNamespace};
use tracing::info;

async fn create_realm(options: ftest::RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();

    realm_factory
        .create_realm(options, dict_server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;

    Ok(ns)
}

#[fuchsia::test]
async fn test_example() {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let test_ns = create_realm(realm_options).await.unwrap();

    info!("connected to the test realm!");

    let echo = connect_to_protocol_at::<fecho::EchoMarker>(test_ns.prefix()).unwrap();
    let response = echo.echo_string(Some("hello")).await.unwrap().unwrap();
    assert_eq!(response, "hello");

    let echo = connect_to_protocol_at_path::<fecho::EchoMarker>(&format!(
        "{}/reverse-echo",
        test_ns.prefix()
    ))
    .unwrap();
    let response = echo.echo_string(Some("hello")).await.unwrap().unwrap();
    assert_eq!(response, "olleh");
}
