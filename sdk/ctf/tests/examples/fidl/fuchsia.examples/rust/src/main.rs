// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// [START example]
use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_examples::EchoMarker;
use fidl_test_example as ftest;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at};
use realm_client::{extend_namespace, InstalledNamespace};
use tracing::info;

async fn create_realm(options: ftest::RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();

    realm_factory
        .create_realm(options, dict_server)
        .await?
        .map_err(realm_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;

    Ok(ns)
}

#[fuchsia::test]
async fn test_example() -> Result<()> {
    let realm_options = ftest::RealmOptions { ..Default::default() };
    let test_ns = create_realm(realm_options).await?;

    let echo = connect_to_protocol_at::<EchoMarker>(&test_ns)?;
    let response = echo.echo_string("hello").await?;
    info!("response: {:?}", response);

    Ok(())
}
// [END example]
