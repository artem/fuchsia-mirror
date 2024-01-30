// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::constants;

use anyhow::{Context, Error, Result};
use fidl::endpoints::{create_endpoints, DiscoverableProtocolMarker, ProtocolMarker};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_testing_harness as fharness;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component_test::{
    Capability, ChildOptions, ChildRef, RealmBuilder, RealmBuilderParams, Ref, Route,
    SubRealmBuilder,
};
use realm_proxy_client::RealmProxyClient;

/// Options for creating a test topology.
pub struct Options {
    /// The URL of the archivist to be used in the test.
    pub archivist_url: &'static str,
    pub realm_name: Option<&'static str>,
}

impl Default for Options {
    fn default() -> Self {
        Self { archivist_url: constants::INTEGRATION_ARCHIVIST_URL, realm_name: None }
    }
}

/// Creates a new test realm with an archivist inside.
/// `options_fn` is called with a default RealmOptions struct and can modify any options
/// before the realm is created.
pub async fn create_realm(options: &ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints::<fharness::RealmProxy_Marker>();
    realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}

/// Connects to the puppet in the test realm with the given name.
pub async fn connect_to_puppet(
    realm_proxy: &RealmProxyClient,
    puppet_name: &str,
) -> Result<<ftest::PuppetMarker as ProtocolMarker>::Proxy> {
    let puppet_protocol_alias = format!("{}.{puppet_name}", ftest::PuppetMarker::PROTOCOL_NAME);
    realm_proxy
        .connect_to_named_protocol::<ftest::PuppetMarker>(&puppet_protocol_alias)
        .await
        .with_context(|| format!("failed to connect to {puppet_name}"))
}

/// Creates a new topology for tests with an archivist inside.
pub async fn create(opts: Options) -> Result<(RealmBuilder, SubRealmBuilder), Error> {
    let mut params = RealmBuilderParams::new();
    if let Some(realm_name) = opts.realm_name {
        params = params.realm_name(realm_name);
    }
    let builder = RealmBuilder::with_params(params).await?;
    let test_realm = builder.add_child_realm("test", ChildOptions::new().eager()).await?;
    let archivist =
        test_realm.add_child("archivist", opts.archivist_url, ChildOptions::new().eager()).await?;

    let parent_to_archivist = Route::new()
        .capability(Capability::protocol_by_name("fuchsia.boot.ReadOnlyLog"))
        .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
        .capability(Capability::protocol_by_name("fuchsia.tracing.provider.Registry").optional());

    builder
        .add_route(
            Route::new()
                .capability(Capability::event_stream("directory_ready").with_scope(&test_realm))
                .capability(
                    Capability::event_stream("capability_requested").with_scope(&test_realm),
                )
                .from(Ref::parent())
                .to(&test_realm),
        )
        .await?;

    test_realm
        .add_route(
            Route::new()
                .capability(Capability::event_stream("directory_ready"))
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await?;

    builder.add_route(parent_to_archivist.clone().from(Ref::parent()).to(&test_realm)).await?;
    test_realm.add_route(parent_to_archivist.from(Ref::parent()).to(&archivist)).await?;

    let archivist_to_parent = Route::new()
        .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
        .capability(Capability::protocol_by_name("fuchsia.diagnostics.FeedbackArchiveAccessor"))
        .capability(Capability::protocol_by_name("fuchsia.diagnostics.LoWPANArchiveAccessor"))
        .capability(Capability::protocol_by_name("fuchsia.diagnostics.LogSettings"))
        .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
        .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
        .capability(Capability::protocol_by_name("fuchsia.logger.Log"));
    test_realm.add_route(archivist_to_parent.clone().from(&archivist).to(Ref::parent())).await?;
    builder.add_route(archivist_to_parent.from(&test_realm).to(Ref::parent())).await?;

    Ok((builder, test_realm))
}

pub async fn add_eager_child(
    test_realm: &SubRealmBuilder,
    name: &str,
    url: &str,
) -> Result<ChildRef, Error> {
    let child_ref = test_realm.add_child(name, url, ChildOptions::new().eager()).await?;
    test_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(Ref::child("archivist"))
                .to(&child_ref),
        )
        .await?;
    Ok(child_ref)
}

pub async fn add_collection(test_realm: &SubRealmBuilder, name: &str) -> Result<(), Error> {
    let mut decl = test_realm.get_realm_decl().await?;
    decl.collections.push(cm_rust::CollectionDecl {
        name: name.parse().unwrap(),
        durability: fdecl::Durability::Transient,
        environment: None,
        allowed_offers: cm_types::AllowedOffers::StaticOnly,
        allow_long_names: false,
        persistent_storage: None,
    });
    test_realm.replace_realm_decl(decl).await?;
    test_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(Ref::child("archivist"))
                .to(Ref::collection(name)),
        )
        .await?;
    Ok(())
}

pub async fn expose_test_realm_protocol(builder: &RealmBuilder, test_realm: &SubRealmBuilder) {
    test_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.component.Realm"))
                .from(Ref::framework())
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.component.Realm"))
                .from(Ref::child("test"))
                .to(Ref::parent()),
        )
        .await
        .unwrap();
}
