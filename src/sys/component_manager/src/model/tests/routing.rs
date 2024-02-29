// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Tests of capability routing in ComponentManager.
///
/// Most routing tests should be defined as methods on the ::routing_test_helpers::CommonRoutingTest
/// type and should be run both in this file (using a CommonRoutingTest<RoutingTestBuilder>) and in
/// the cm_fidl_analyzer_tests crate (using a specialization of CommonRoutingTest for the static
/// routing analyzer). This ensures that the static analyzer's routing verification is consistent
/// with ComponentManager's intended routing behavior.
///
/// However, tests of behavior that is out-of-scope for the static analyzer (e.g. routing to/from
/// dynamic component instances) should be defined here.
use {
    crate::{
        capability::CapabilitySource,
        model::{
            actions::{ActionSet, DestroyAction, ShutdownAction, ShutdownType},
            component::StartReason,
            error::{
                ActionError, ModelError, ResolveActionError, RouteOrOpenError, StartActionError,
            },
            routing::{Route, RouteRequest, RouteSource, RoutingError},
            testing::{echo_service::EchoProtocol, routing_test_helpers::*, test_helpers::*},
        },
    },
    ::routing::{
        capability_source::{
            AggregateCapability, AggregateInstance, AggregateMember, ComponentCapability,
        },
        error::ComponentInstanceError,
        resolving::ResolverError,
    },
    assert_matches::assert_matches,
    cm_rust::*,
    cm_rust_testing::*,
    fidl::endpoints::{ClientEnd, ProtocolMarker, ServerEnd},
    fidl_fidl_examples_routing_echo::{self as echo},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fidl_fuchsia_mem as fmem,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::{channel::oneshot, join, StreamExt},
    maplit::btreemap,
    moniker::{ChildName, ChildNameBase, Moniker, MonikerBase},
    routing_test_helpers::{
        default_service_capability, instantiate_common_routing_tests, RoutingTestModel,
    },
    std::{
        collections::HashSet,
        convert::{TryFrom, TryInto},
        sync::Arc,
    },
    tracing::warn,
    vfs::{pseudo_directory, service},
};

instantiate_common_routing_tests! { RoutingTestBuilder }

///   a
///    \
///     b
///
/// a: offers service /svc/foo from self as /svc/bar
/// b: uses service /svc/bar as /svc/hippo
///
/// This test verifies that the parent, if subscribed to the CapabilityRequested event will receive
/// if when the child connects to /svc/hippo.
#[fuchsia::test]
async fn capability_requested_event_at_parent() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "bar".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .use_(UseBuilder::event_stream()
                    .name("capability_requested")
                    .path("/events/capability_requested")
                    .filter(
                        btreemap! { "name".to_string() => DictionaryValue::Str("foo".to_string()) },
                    )
                )
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .name("bar")
                        .path("/svc/hippo")
                )
                .build(),
        ),
    ];
    let test = RoutingTestBuilder::new("a", components)
        .set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "capability_requested".parse().unwrap(),
        })])
        .build()
        .await;

    let namespace_root = test.bind_and_get_namespace(Moniker::root()).await;
    let event_stream =
        capability_util::connect_to_svc_in_namespace::<fcomponent::EventStreamMarker>(
            &namespace_root,
            &"/events/capability_requested".parse().unwrap(),
        )
        .await;

    let namespace_b = test.bind_and_get_namespace(vec!["b"].try_into().unwrap()).await;
    let _echo_proxy = capability_util::connect_to_svc_in_namespace::<echo::EchoMarker>(
        &namespace_b,
        &"/svc/hippo".parse().unwrap(),
    )
    .await;

    let event = event_stream.get_next().await.unwrap().into_iter().next().unwrap();

    // 'b' is the target and 'a' is receiving the event so the moniker
    // is '/b'.
    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
            moniker: Some(moniker), .. }), ..
        } if *moniker == "b".to_string() );

    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
            component_url: Some(component_url), .. }), ..
        } if *component_url == "test:///b".to_string() );

    assert_matches!(&event,
        fcomponent::Event {
            payload:
                    Some(fcomponent::EventPayload::CapabilityRequested(
                        fcomponent::CapabilityRequestedPayload { name: Some(name), .. })), ..}

    if *name == "foo".to_string()
    );
}

///   a
///    \
///     b
///    / \
///  [c] [d]
/// a: offers service /svc/hippo to b
/// b: offers service /svc/hippo to collection, creates [c]
/// [c]: instance in collection uses service /svc/hippo
/// [d]: ditto, but with /data/hippo
#[fuchsia::test]
async fn use_in_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .protocol_default("foo")
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "foo_data".parse().unwrap(),
                    source: OfferSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo_data".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo".parse().unwrap(),
                    source: OfferSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "hippo_data".parse().unwrap(),
                    source: OfferSource::Parent,
                    source_dictionary: None,
                    target_name: "hippo_data".parse().unwrap(),
                    target: OfferTarget::Collection("coll".parse().unwrap()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: OfferSource::Parent,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: OfferTarget::Collection("coll".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .collection_default("coll")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("hippo").path("/svc/hippo"))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "d".to_string(),
            url: "test:///d".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Ok),
    )
    .await;
    test.check_use(
        vec!["b", "coll:d"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///   a
///    \
///     b
///      \
///      [c]
/// a: offers service /svc/hippo to b
/// b: creates [c]
/// [c]: tries to use /svc/hippo, but can't because service was not offered to its collection
#[fuchsia::test]
async fn use_in_collection_not_offered() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .protocol_default("foo")
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source_name: "foo_data".parse().unwrap(),
                    source: OfferSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo_data".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo".parse().unwrap(),
                    source: OfferSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection_default("coll")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                .use_(UseBuilder::protocol().name("hippo"))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

///   a
///    \
///     b
///    / \
///  [c] [d]
/// a: offers service /svc/hippo to b
/// b: creates [c] and [d], dynamically offers service /svc/hippo to [c], but not [d].
/// [c]: instance in collection uses service /svc/hippo
/// [d]: instance in collection tries and fails to use service /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_from_parent() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "foo".parse().unwrap(),
                    source: OfferSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: OfferTarget::static_child("b".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
        ("d", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child_with_args(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo".to_string()),
                source: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                target_name: Some("hippo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.create_dynamic_child(
        &vec!["b"].try_into().unwrap(),
        "coll",
        ChildDecl {
            name: "d".to_string(),
            url: "test:///d".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.check_use(
        vec!["b", "coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
    test.check_use(
        vec!["b", "coll:d"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /svc/hippo
/// [c]: instance in collection uses /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_siblings_same_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol_default("hippo")
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: ExposeSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in a different collection, with a dynamic offer from [b].
/// [b]: instance in `source_coll` exposes /svc/hippo
/// [c]: instance in `target_coll` uses /svc/hippo
#[fuchsia::test]
async fn dynamic_offer_siblings_cross_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection_default("source_coll")
                .collection(
                    CollectionBuilder::new()
                        .name("target_coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol_default("hippo")
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: ExposeSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &Moniker::root(),
        "source_coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "target_coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("source_coll".to_string()),
                })),
                source_name: Some("hippo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                target_name: Some("hippo".to_string()),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["target_coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /svc/hippo
/// [c]: instance in collection uses /svc/hippo. Can't use it after [b] is destroyed and recreated.
#[fuchsia::test]
async fn dynamic_offer_destroyed_on_source_destruction() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol_default("hippo")
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: ExposeSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;

    test.destroy_dynamic_child(Moniker::root(), "coll", "b").await;
    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

///    a
///   / \
/// [b] [c]
/// a: creates [b]. creates [c] in the same collection, with a dynamic offer from [b].
/// [b]: instance in collection exposes /data/hippo
/// [c]: instance in collection uses /data/hippo. Can't use it after [c] is destroyed and recreated.
#[fuchsia::test]
async fn dynamic_offer_destroyed_on_target_destruction() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("hippo_data").path("/data/foo"))
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source_name: "hippo_data".parse().unwrap(),
                    source: ExposeSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo_data".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;
    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Directory(fdecl::OfferDirectory {
                source_name: Some("hippo_data".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: Some("coll".to_string()),
                })),
                target_name: Some("hippo_data".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Ok),
    )
    .await;

    test.destroy_dynamic_child(Moniker::root(), "coll", "c").await;
    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    test.check_use(
        vec!["coll:c"].try_into().unwrap(),
        CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
    )
    .await;
}

///    a
///   / \
///  b  [c]
///       \
///        d
/// a: creates [c], with a dynamic offer from b.
/// b: exposes /svc/hippo
/// [c]: instance in collection, offers /svc/hippo to d.
/// d: static child of dynamic instance [c]. uses /svc/hippo.
#[fuchsia::test]
async fn dynamic_offer_to_static_offer() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection(
                    CollectionBuilder::new()
                        .name("coll")
                        .allowed_offers(cm_types::AllowedOffers::StaticAndDynamic),
                )
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .protocol_default("hippo")
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: ExposeSource::Self_,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source_name: "hippo".parse().unwrap(),
                    source: OfferSource::Parent,
                    source_dictionary: None,
                    target_name: "hippo".parse().unwrap(),
                    target: OfferTarget::static_child("d".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("d")
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("hippo"))
                .child_default("d")
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "c".to_string(),
            url: "test:///c".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dynamic_offers: Some(vec![fdecl::Offer::Protocol(fdecl::OfferProtocol {
                source_name: Some("hippo".to_string()),
                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                    name: "b".to_string(),
                    collection: None,
                })),
                target_name: Some("hippo".to_string()),
                dependency_type: Some(fdecl::DependencyType::Strong),
                ..Default::default()
            })]),
            ..Default::default()
        },
    )
    .await;
    test.check_use(
        vec!["coll:c", "d"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

/// Tests that a dynamic component can use a capability from the dict passed in CreateChildArgs.
///
///    a
///   /
/// [b]
///
/// a: creates [b], with a dict that contains an Open for `hippo`
/// [b]: instance in collection, uses the `hippo` protocol.
#[fuchsia::test]
async fn create_child_with_dict() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .collection_default("coll")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
    ];

    // Create a dictionary with a sender for the `hippo` protocol.
    let dict = sandbox::Dict::new();

    let (receiver, sender) = sandbox::Receiver::new();

    // Serve the `fidl.examples.routing.echo.Echo` protocol on the receiver.
    let _task = fasync::Task::spawn(async move {
        let mut tasks = fasync::TaskGroup::new();
        loop {
            let Some(message) = receiver.receive().await else {
                return;
            };
            let server_end = ServerEnd::<echo::EchoMarker>::new(message.payload.channel);
            let stream: echo::EchoRequestStream = server_end.into_stream().unwrap();
            tasks.add(fasync::Task::spawn(async move {
                EchoProtocol::serve(stream).await.expect("failed to serve Echo");
            }));
        }
    });

    // CreateChild dictionary entries must be Open capabilities.
    // TODO(https://fxbug.dev/319542502): Insert the external Router type, once it exists
    let open: sandbox::Open = sender.into();
    dict.lock_entries().insert("hippo".to_string(), sandbox::Capability::Open(open));

    let dictionary_client_end: ClientEnd<fsandbox::DictionaryMarker> = dict.into();

    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child_with_args(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
        fcomponent::CreateChildArgs {
            dictionary: Some(dictionary_client_end),
            ..Default::default()
        },
    )
    .await;

    test.check_use(
        vec!["coll:b"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;
}

#[fuchsia::test]
async fn destroying_instance_kills_framework_service_task() {
    let components = vec![
        ("a", ComponentDeclBuilder::new().child_default("b").build()),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;

    // Connect to `Realm`, which is a framework service.
    let namespace = test.bind_and_get_namespace(vec!["b"].try_into().unwrap()).await;
    let proxy = capability_util::connect_to_svc_in_namespace::<fcomponent::RealmMarker>(
        &namespace,
        &"/svc/fuchsia.component.Realm".parse().unwrap(),
    )
    .await;

    // Destroy `b`. This should cause the task hosted for `Realm` to be cancelled.
    test.model.root().destroy_child("b".try_into().unwrap(), 0).await.expect("destroy failed");
    let mut event_stream = proxy.take_event_stream();
    assert_matches!(event_stream.next().await, None);
}

#[fuchsia::test]
async fn destroying_instance_blocks_on_routing() {
    // Directories and protocols have a different routing flow so test both.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("c".into()),
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: OfferTarget::static_child("b".into()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Directory(OfferDirectoryDecl {
                    source: OfferSource::static_child("c".into()),
                    source_name: "foo_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo_data".parse().unwrap(),
                    target: OfferTarget::static_child("b".into()),
                    rights: None,
                    subdir: None,
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("b")
                .child_default("c")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("foo").path("/svc/echo"))
                .use_(UseBuilder::directory().name("foo_data").path("/data"))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo_data".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo_data".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    rights: Some(fio::R_STAR_DIR),
                    subdir: None,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
    ];
    // Cause resolution for `c` to block until we explicitly tell it to proceed. This is useful
    // for coordinating the progress of `echo`'s routing task with destruction.
    let builder = RoutingTestBuilder::new("a", components);
    let (resolved_tx, resolved_rx) = oneshot::channel::<()>();
    let (continue_tx, continue_rx) = oneshot::channel::<()>();
    let test = builder.add_blocker("c", resolved_tx, continue_rx).build().await;

    // Connect to `echo` in `b`'s namespace to kick off a protocol routing task.
    let (_, component_name) = test
        .start_and_get_instance(&vec!["b"].try_into().unwrap(), StartReason::Eager, true)
        .await
        .unwrap();
    let component_resolved_url = RoutingTest::resolved_url(&component_name);
    let namespace = test.mock_runner.get_namespace(&component_resolved_url).unwrap();
    let echo_proxy = capability_util::connect_to_svc_in_namespace::<echo::EchoMarker>(
        &namespace,
        &"/svc/echo".parse().unwrap(),
    )
    .await;

    // Connect to `data` in `b`'s namespace to kick off a directory routing task.
    let dir_proxy = capability_util::take_dir_from_namespace(&namespace, "/data").await;
    let file_proxy = fuchsia_fs::directory::open_file_no_describe(
        &dir_proxy,
        "hippo",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .unwrap();
    capability_util::add_dir_to_namespace(&namespace, "/data", dir_proxy).await;

    // Destroy `b`.
    let root = test.model.root().find_and_maybe_resolve(&Moniker::root()).await.unwrap();
    let root_clone = root.clone();
    let destroy_nf =
        fasync::Task::spawn(
            async move { root_clone.destroy_child("b".try_into().unwrap(), 0).await },
        );

    // Give the destroy action some time to complete. Sleeping is not an ideal testing strategy,
    // but it helps add confidence to the test because it makes it more likely the test would
    // fail if the destroy action is not correctly blocking on the routing task.
    fasync::Timer::new(fasync::Time::after(zx::Duration::from_seconds(5))).await;

    // Wait until routing reaches resolution. It should get here because `Destroy` should not
    // cancel the routing task.
    let _ = resolved_rx.await.unwrap();

    // `b` is not yet destroyed.
    let state = root.lock_resolved_state().await.unwrap();
    state.get_child(&ChildName::parse("b").unwrap()).expect("b was destroyed");
    drop(state);

    // Let routing complete. This should allow destruction to complete.
    continue_tx.send(()).unwrap();
    destroy_nf.await.unwrap();

    // Verify the connection to `echo` and `data` was bound by the provider.
    capability_util::call_echo_and_validate_result(echo_proxy, ExpectedResult::Ok).await;
    assert_eq!(fuchsia_fs::file::read_to_string(&file_proxy).await.unwrap(), "hello");
}

///  a
///   \
///    b
///
/// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_parent_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("b").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "elf".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    target_name: "hobbit".parse().unwrap(),
                }))
                .runner_default("elf")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "b" exposes a runner service.
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///b_resolved".to_string())
            );
        }
    );
}

///  a
///   \
///   [b]
///
/// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: instance in collection uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_environment_in_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .collection(CollectionBuilder::new().name("coll").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "elf".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    target_name: "hobbit".parse().unwrap(),
                }))
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .runner_default("elf")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;
    universe
        .create_dynamic_child(
            &Moniker::root(),
            "coll",
            ChildDecl {
                name: "b".to_string(),
                url: "test:///b".to_string(),
                startup: fdecl::StartupMode::Lazy,
                environment: None,
                on_terminate: None,
                config_overrides: None,
            },
        )
        .await;

    join!(
        // Bind "coll:b". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["coll:b"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///b_resolved".to_string())
            );
        }
    );
}

///   a
///    \
///     b
///      \
///       c
///
/// a: declares runner "elf" as service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self.
/// a: offers runner "elf" from self to "b" as "dwarf".
/// b: registers runner "dwarf" from realm in environment as "hobbit".
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_grandparent_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child_default("b")
                .offer(OfferDecl::Runner(OfferRunnerDecl {
                    source: OfferSource::Self_,
                    source_name: "elf".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("b".to_string()),
                    target_name: "dwarf".parse().unwrap(),
                }))
                .runner_default("elf")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("c").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "dwarf".parse().unwrap(),
                    source: RegistrationSource::Parent,
                    target_name: "hobbit".parse().unwrap(),
                }))
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b", "c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///   a
///  / \
/// b   c
///
/// a: registers runner "dwarf" from "b" in environment as "hobbit".
/// b: exposes runner "elf" as service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self as "dwarf".
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_sibling_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child_default("b")
                .child(ChildBuilder::new().name("c").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "dwarf".parse().unwrap(),
                    source: RegistrationSource::Child("b".to_string()),
                    target_name: "hobbit".parse().unwrap(),
                }))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Runner(ExposeRunnerDecl {
                    source: ExposeSource::Self_,
                    source_name: "elf".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "dwarf".parse().unwrap(),
                }))
                .runner_default("elf")
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path(
            "b",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///   a
///    \
///     b
///      \
///       c
///
/// a: declares runner "elf" as service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self.
/// a: registers runner "elf" from realm in environment as "hobbit".
/// b: creates environment extending from realm.
/// c: uses runner "hobbit".
#[fuchsia::test]
async fn use_runner_from_inherited_environment() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("b").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "elf".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    target_name: "hobbit".parse().unwrap(),
                }))
                .runner_default("elf")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("c").environment("env"))
                .environment(EnvironmentBuilder::new().name("env"))
                .build(),
        ),
        ("c", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, mut receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our runner service for the new component.
        async move {
            universe.start_instance(&vec!["b", "c"].try_into().unwrap()).await.unwrap();
        },
        // Wait for a request, and ensure it has the correct URL.
        async move {
            assert_eq!(
                wait_for_runner_request(&mut receiver).await.resolved_url,
                Some("test:///c_resolved".to_string())
            );
        }
    );
}

///  a
///   \
///    b
///
/// a: declares runner "runner" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
/// a: registers runner "runner" from self in environment as "hobbit".
/// b: uses runner "runner". Fails due to a FIDL error, conveyed through a Stop after the
///    bind succeeds.
#[fuchsia::test]
async fn use_runner_from_environment_failed() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("b").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "runner".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    target_name: "runner".parse().unwrap(),
                }))
                .runner_default("runner")
                // For Stopped event
                .use_(UseBuilder::event_stream().name("stopped").path("/events/stopped"))
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("runner").build()),
    ];

    let runner_service = service::endpoint(|_scope, _channel| {});

    // Set a capability provider for the runner that closes the server end.
    // `ComponentRunner.Start` to fail.
    let test = RoutingTestBuilder::new("a", components)
        .set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "stopped".parse().unwrap(),
        })])
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    let namespace_root = test.bind_and_get_namespace(Moniker::root()).await;
    let event_stream =
        capability_util::connect_to_svc_in_namespace::<fcomponent::EventStreamMarker>(
            &namespace_root,
            &"/events/stopped".parse().unwrap(),
        )
        .await;

    // Even though we expect the runner to fail, bind should succeed. This is because the failure
    // is propagated via the controller channel, separately from the Start action.
    test.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap();

    // Since the controller should have closed, expect a Stopped event.
    let event = event_stream.get_next().await.unwrap().into_iter().next().unwrap();
    assert_matches!(&event,
        fcomponent::Event {
            header: Some(fcomponent::EventHeader {
                moniker: Some(moniker),
                ..
            }),
            payload:
                    Some(fcomponent::EventPayload::Stopped(
                        fcomponent::StoppedPayload {
                            status: Some(status),
                            ..
                        }
                    ))
            ,
            ..
        }
        if *moniker == "b".to_string()
            && *status == zx::Status::PEER_CLOSED.into_raw() as i32
    );
}

///  a
///   \
///    b
///
/// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
/// a: registers runner "elf" from self in environment as "hobbit".
/// b: uses runner "hobbit". Fails because "hobbit" was not in environment.
#[fuchsia::test]
async fn use_runner_from_environment_not_found() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("b").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                    source_name: "elf".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    target_name: "dwarf".parse().unwrap(),
                }))
                .runner_default("elf")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new_empty_component().add_program("hobbit").build()),
    ];

    // Set up the system.
    let (runner_service, _receiver) =
        create_service_directory_entry::<fcrunner::ComponentRunnerMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a runner service.
        .add_outgoing_path(
            "a",
            format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse().unwrap(),
            runner_service,
        )
        .build()
        .await;

    // Bind "b". We expect it to fail because routing failed.
    let err = universe.start_instance(&vec!["b"].try_into().unwrap()).await.unwrap_err();
    let err = match err {
        ModelError::ActionError {
            err:
                ActionError::StartError {
                    err: StartActionError::ResolveRunnerError { err, moniker, .. },
                },
        } if moniker == vec!["b"].try_into().unwrap() => err,
        err => panic!("Unexpected error trying to start b: {}", err),
    };

    assert_matches!(
        *err,
        RouteOrOpenError::RoutingError(
            RoutingError::UseFromEnvironmentNotFound {
                moniker,
                capability_type,
                capability_name,
            },
        )
        if moniker == Moniker::try_from(vec!["b"]).unwrap() &&
        capability_type == "runner" &&
        capability_name == "hobbit");
}

// TODO: Write a test for environment that extends from None. Currently, this is not
// straightforward because resolver routing is not implemented yet, which makes it impossible to
// register a new resolver and have it be usable.

///   a
///    \
///    [b]
///      \
///       c
///
/// a: offers service /svc/foo from self
/// [b]: offers service /svc/foo to c
/// [b]: is destroyed
/// c: uses service /svc/foo, which should fail
#[fuchsia::test]
async fn use_with_destroyed_parent() {
    let use_decl = UseBuilder::protocol().name("foo").path("/svc/hippo").build();
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .protocol_default("foo")
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: OfferTarget::Collection("coll".parse().unwrap()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .collection_default("coll")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: OfferTarget::static_child("c".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("c")
                .build(),
        ),
        ("c", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
    ];
    let test = RoutingTest::new("a", components).await;
    test.create_dynamic_child(
        &Moniker::root(),
        "coll",
        ChildDecl {
            name: "b".to_string(),
            url: "test:///b".to_string(),
            startup: fdecl::StartupMode::Lazy,
            environment: None,
            on_terminate: None,
            config_overrides: None,
        },
    )
    .await;

    // Confirm we can use service from "c".
    test.check_use(
        vec!["coll:b", "c"].try_into().unwrap(),
        CheckUse::Protocol { path: default_service_capability(), expected_res: ExpectedResult::Ok },
    )
    .await;

    // Destroy "b", but preserve a reference to "c" so we can route from it below.
    let moniker = vec!["coll:b", "c"].try_into().unwrap();
    let realm_c = test
        .model
        .root()
        .find_and_maybe_resolve(&moniker)
        .await
        .expect("failed to look up realm b");
    test.destroy_dynamic_child(Moniker::root(), "coll", "b").await;

    // Now attempt to route the service from "c". Should fail because "b" does not exist so we
    // cannot follow it.
    let UseDecl::Protocol(use_decl) = use_decl else {
        unreachable!();
    };
    let err = RouteRequest::UseProtocol(use_decl)
        .route(&realm_c)
        .await
        .expect_err("routing unexpectedly succeeded");
    assert_matches!(
        err,
        RoutingError::ComponentInstanceError(
            ComponentInstanceError::InstanceNotFound { moniker }
        ) if moniker == vec!["coll:b"].try_into().unwrap()
    );
}

///   a
///  / \
/// b   c
///
/// b: exposes directory /data/foo from self as /data/bar
/// a: offers directory /data/bar from b as /data/baz to c, which was destroyed (but not removed
///    from the tree yet)
/// c: uses /data/baz as /data/hippo
#[fuchsia::test]
async fn use_from_destroyed_but_not_removed() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Protocol(OfferProtocolDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "bar".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "baz".parse().unwrap(),
                    target: OfferTarget::static_child("c".to_string()),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }))
                .child_default("b")
                .child_default("c")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                .protocol_default("foo")
                .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "bar".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("baz").path("/svc/hippo"))
                .build(),
        ),
    ];
    let test = RoutingTest::new("a", components).await;
    let component_b = test
        .model
        .root()
        .find_and_maybe_resolve(&vec!["b"].try_into().unwrap())
        .await
        .expect("failed to look up realm b");
    // Destroy `b` but keep alive its reference from the parent.
    // TODO: If we had a "pre-destroy" event we could delete the child through normal means and
    // block on the event instead of explicitly registering actions.
    ActionSet::register(component_b.clone(), ShutdownAction::new(ShutdownType::Instance))
        .await
        .expect("shutdown failed");
    ActionSet::register(component_b, DestroyAction::new()).await.expect("destroy failed");
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Protocol {
            path: default_service_capability(),
            expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
        },
    )
    .await;
}

///   a
///  / \
/// b   c
///
/// a: creates environment "env" and registers resolver "base" from c.
/// b: resolved by resolver "base" through "env".
/// b: exposes resolver "base" from self.
#[fuchsia::test]
async fn use_resolver_from_parent_environment() {
    // Note that we do not define a component "b". This will be resolved by our custom resolver.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("b").url("base://b").environment("env"))
                .child(ChildBuilder::new().name("c"))
                .environment(EnvironmentBuilder::new().name("env").resolver(ResolverRegistration {
                    resolver: "base".parse().unwrap(),
                    source: RegistrationSource::Child("c".to_string()),
                    scheme: "base".parse().unwrap(),
                }))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Resolver(ExposeResolverDecl {
                    source: ExposeSource::Self_,
                    source_name: "base".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "base".parse().unwrap(),
                }))
                .resolver_default("base")
                .build(),
        ),
    ];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "c",
            "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a call to our resolver service for the new component.
        async move {
            universe
                .start_instance(&vec!["b"].try_into().unwrap())
                .await
                .expect("failed to start instance b");
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::HISTORY
                                        .get_example_supported_version_for_tests()
                                        .abi_revision
                                        .into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///    \
///     b
///      \
///       c
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
/// c: is resolved by resolver from grandarent.
#[fuchsia::test]
async fn use_resolver_from_grandparent_environment() {
    // Note that we do not define a component "c". This will be resolved by our custom resolver.
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .child(ChildBuilder::new().name("b").environment("env"))
                .environment(EnvironmentBuilder::new().name("env").resolver(ResolverRegistration {
                    resolver: "base".parse().unwrap(),
                    source: RegistrationSource::Self_,
                    scheme: "base".parse().unwrap(),
                }))
                .resolver_default("base")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new_empty_component()
                .child(ChildBuilder::new().name("c").url("base://c"))
                .build(),
        ),
    ];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "a",
            "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a call to our resolver service for the new component.
        async move {
            universe
                .start_instance(&vec!["b", "c"].try_into().unwrap())
                .await
                .expect("failed to start instance c");
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://c");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://c".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::HISTORY
                                        .get_example_supported_version_for_tests()
                                        .abi_revision
                                        .into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///  / \
/// b   c
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
/// c: does NOT have environment "env".
#[fuchsia::test]
async fn resolver_is_not_available() {
    // Note that we do not define a component "b" or "c". This will be resolved by our custom resolver.
    let components = vec![(
        "a",
        ComponentDeclBuilder::new()
            .child(ChildBuilder::new().name("b").url("base://b").environment("env"))
            .child(ChildBuilder::new().name("c").url("base://c"))
            .environment(EnvironmentBuilder::new().name("env").resolver(ResolverRegistration {
                resolver: "base".parse().unwrap(),
                source: RegistrationSource::Self_,
                scheme: "base".parse().unwrap(),
            }))
            .resolver_default("base")
            .build(),
    )];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "c" exposes a resolver service.
        .add_outgoing_path(
            "a",
            "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "c". We expect to see a failure that the scheme is not registered.
        async move {
            match universe.start_instance(&vec!["c"].try_into().unwrap()).await {
                Err(ModelError::ActionError {
                    err:
                        ActionError::ResolveError {
                            err:
                                ResolveActionError::ResolverError {
                                    url,
                                    err: ResolverError::SchemeNotRegistered,
                                },
                        },
                }) => {
                    assert_eq!(url, "base://c");
                }
                _ => {
                    panic!("expected ResolverError");
                }
            };
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes(
                                    fidl::persist(&default_component_decl().native_into_fidl())
                                        .unwrap(),
                                )),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::HISTORY
                                        .get_example_supported_version_for_tests()
                                        .abi_revision
                                        .into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

///   a
///  /
/// b
/// a: creates environment "env" and registers resolver "base" from self.
/// b: has environment "env".
#[fuchsia::test]
async fn resolver_component_decl_is_validated() {
    // Note that we do not define a component "b". This will be resolved by our custom resolver.
    let components = vec![(
        "a",
        ComponentDeclBuilder::new()
            .child(ChildBuilder::new().name("b").url("base://b").environment("env"))
            .environment(EnvironmentBuilder::new().name("env").resolver(ResolverRegistration {
                resolver: "base".parse().unwrap(),
                source: RegistrationSource::Self_,
                scheme: "base".parse().unwrap(),
            }))
            .resolver_default("base")
            .build(),
    )];

    // Set up the system.
    let (resolver_service, mut receiver) =
        create_service_directory_entry::<fresolution::ResolverMarker>();
    let universe = RoutingTestBuilder::new("a", components)
        // Component "a" exposes a resolver service.
        .add_outgoing_path(
            "a",
            "/svc/fuchsia.component.resolution.Resolver".parse().unwrap(),
            resolver_service,
        )
        .build()
        .await;

    join!(
        // Bind "b". We expect to see a ResolverError.
        async move {
            match universe.start_instance(&vec!["b"].try_into().unwrap()).await {
                Err(ModelError::ActionError {
                    err:
                        ActionError::ResolveError {
                            err:
                                ResolveActionError::ResolverError {
                                    url,
                                    err: ResolverError::ManifestInvalid(_),
                                },
                        },
                }) => {
                    assert_eq!(url, "base://b");
                }
                _ => {
                    panic!("expected ResolverError");
                }
            };
        },
        // Wait for a request, and resolve it.
        async {
            while let Some(request) = receiver.next().await {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        assert_eq!(component_url, "base://b");
                        responder
                            .send(Ok(fresolution::Component {
                                url: Some("test://b".into()),
                                decl: Some(fmem::Data::Bytes({
                                    let fidl = fdecl::Component {
                                        exposes: Some(vec![fdecl::Expose::Protocol(
                                            fdecl::ExposeProtocol {
                                                source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                                                ..Default::default()
                                            },
                                        )]),
                                        ..Default::default()
                                    };
                                    fidl::persist(&fidl).unwrap()
                                })),
                                package: None,
                                // this test only resolves one component_url
                                resolution_context: None,
                                abi_revision: Some(
                                    version_history::HISTORY
                                        .get_example_supported_version_for_tests()
                                        .abi_revision
                                        .into(),
                                ),
                                ..Default::default()
                            }))
                            .expect("failed to send resolve response");
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url,
                        context,
                        responder,
                    } => {
                        warn!(
                            "ResolveWithContext({}, {:?}) request is unexpected in this test",
                            component_url, context
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .expect("failed to send resolve response");
                    }
                }
            }
        }
    );
}

enum RouteType {
    Offer,
    Expose,
}

async fn verify_service_route(
    test: &RoutingTest,
    use_decl: UseDecl,
    target_moniker: &str,
    agg_moniker: &str,
    child_monikers: &[&str],
    route_type: RouteType,
) {
    let UseDecl::Service(use_decl) = use_decl else {
        unreachable!();
    };
    let target_moniker: Moniker = target_moniker.try_into().unwrap();
    let agg_moniker: Moniker = agg_moniker.try_into().unwrap();
    let child_monikers: Vec<_> =
        child_monikers.into_iter().map(|m| ChildName::parse(m).unwrap()).collect();

    // Test routing directly.
    let root = test.model.root();
    let target_component = root.find_and_maybe_resolve(&target_moniker).await.unwrap();
    let agg_component = root.find_and_maybe_resolve(&agg_moniker).await.unwrap();
    let source = RouteRequest::UseService(use_decl).route(&target_component).await.unwrap();
    match source {
        RouteSource {
            source: CapabilitySource::AnonymizedAggregate { members, capability, component, .. },
            relative_path: _,
        } => {
            let collections: Vec<_> = members
                .into_iter()
                .map(|m| match m {
                    AggregateMember::Collection(c) => c.to_string(),
                    _ => panic!("not expected"),
                })
                .collect();
            let unique_colls: HashSet<_> =
                child_monikers.iter().map(|c| c.collection().unwrap().to_string()).collect();
            let mut unique_colls: Vec<_> = unique_colls.into_iter().collect();
            unique_colls.sort();
            assert_eq!(collections, unique_colls);
            assert_matches!(capability, AggregateCapability::Service(_));
            assert_eq!(*capability.source_name(), "foo");
            assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &agg_component));
        }
        _ => panic!("wrong capability source"),
    };

    // Populate the collection(s) with dynamic children.
    for child_moniker in &child_monikers {
        let coll = child_moniker.collection().unwrap();
        let name = child_moniker.name();
        test.create_dynamic_child(&agg_moniker, coll.as_str(), ChildBuilder::new().name(name))
            .await;
        test.start_instance_and_wait_start(&agg_moniker.child(child_moniker.clone()))
            .await
            .unwrap();
    }

    // Use the aggregated service from `target_moniker`.
    test.check_use(
        target_moniker,
        CheckUse::Service {
            path: "/svc/foo".parse().unwrap(),
            instance: ServiceInstance::Aggregated(child_monikers.len()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    match route_type {
        RouteType::Offer => {}
        RouteType::Expose => {
            test.check_use_exposed_dir(
                agg_moniker,
                CheckUse::Service {
                    path: "/foo".parse().unwrap(),
                    instance: ServiceInstance::Aggregated(child_monikers.len()),
                    member: "echo".to_string(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        }
    }
}

///   a
///  / \
/// b   coll
///
/// root: offer service `foo` from `coll` to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collection() {
    let use_decl = UseBuilder::service().name("foo").build();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Collection("coll".parse().unwrap()),
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: None,
                    renamed_instances: None,
                    target_name: "foo".parse().unwrap(),
                    target: OfferTarget::static_child("b".into()),
                    availability: Availability::Required,
                }))
                .collection_default("coll")
                .child_default("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/b",
        "/",
        &["coll:c1", "coll:c2", "coll:c3"],
        RouteType::Offer,
    )
    .await;
}

///   a
///  / \
/// b   (coll1, coll2, coll3)
///
/// root: offer service `foo` from collections to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collections() {
    let use_decl = UseBuilder::service().name("foo").build();
    let mut offers: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.parse().unwrap()),
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".parse().unwrap(),
                target: OfferTarget::static_child("b".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .collection_default("coll1")
                .collection_default("coll2")
                .collection_default("coll3")
                .child_default("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/b",
        "/",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Offer,
    )
    .await;
}

///     a
///    / \
///   m  (coll1, coll2, coll3)
///  /
/// b
///
/// root: offer service `foo` from coll to b
/// b: route `use service`
#[fuchsia::test]
async fn offer_service_from_collections_multilevel() {
    let use_decl = UseBuilder::service().name("foo").build();
    let mut offers: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.parse().unwrap()),
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".parse().unwrap(),
                target: OfferTarget::static_child("m".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .collection_default("coll1")
                .collection_default("coll2")
                .collection_default("coll3")
                .child_default("m")
                .build(),
        ),
        (
            "m",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: None,
                    renamed_instances: None,
                    target_name: "foo".parse().unwrap(),
                    target: OfferTarget::static_child("b".into()),
                    availability: Availability::Required,
                }))
                .child_default("b")
                .build(),
        ),
        ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/m/b",
        "/",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Offer,
    )
    .await;
}

/// a
///  \
///   b
///    \
///    coll
///
/// b: expose service `foo` from `coll` to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collection() {
    let use_decl = UseBuilder::service().name("foo").source(UseSource::Child("b".into())).build();
    let mut components = vec![
        ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("b").build()),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Collection("coll".parse().unwrap()),
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "foo".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .collection_default("coll")
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/b",
        &["coll:c1", "coll:c2", "coll:c3"],
        RouteType::Expose,
    )
    .await;
}

/// a
///  \
///   b
///    \
///    (coll1, coll2, coll3)
///
/// b: expose service `foo` from collections to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collections() {
    let use_decl = UseBuilder::service().name("foo").source(UseSource::Child("b".into())).build();
    let mut exposes: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            ExposeDecl::Service(ExposeServiceDecl {
                source: ExposeSource::Collection(coll.parse().unwrap()),
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                target: ExposeTarget::Parent,
                target_name: "foo".parse().unwrap(),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("b").build()),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .collection_default("coll1")
                .collection_default("coll2")
                .collection_default("coll3")
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/b",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Expose,
    )
    .await;
}

/// a
///  \
///   m
///    \
///     b
///      \
///      (coll1, coll2, coll3)
///
/// b: expose service `foo` from collections to b
/// a: route `use service`
#[fuchsia::test]
async fn expose_service_from_collections_multilevel() {
    let use_decl = UseBuilder::service().name("foo").source(UseSource::Child("m".into())).build();
    let mut exposes: Vec<_> = ["coll1", "coll2", "coll3"]
        .into_iter()
        .map(|coll| {
            ExposeDecl::Service(ExposeServiceDecl {
                source: ExposeSource::Collection(coll.parse().unwrap()),
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                target: ExposeTarget::Parent,
                target_name: "foo".parse().unwrap(),
                availability: Availability::Required,
            })
        })
        .collect();
    let mut components = vec![
        ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("m").build()),
        (
            "m",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Child("b".into()),
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .child_default("b")
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_realm()
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .expose(exposes.remove(0))
                .collection_default("coll1")
                .collection_default("coll2")
                .collection_default("coll3")
                .build(),
        ),
    ];
    components.extend(["c1", "c2", "c3"].into_iter().map(|ch| {
        (
            ch,
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "foo".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        )
    }));
    let test = RoutingTestBuilder::new("a", components).build().await;

    verify_service_route(
        &test,
        use_decl,
        "/",
        "/m/b",
        &["coll1:c1", "coll2:c2", "coll3:c3"],
        RouteType::Expose,
    )
    .await;
}

///      root
///      /  \
/// client   (coll1: [service_child_a, non_service_child], coll2: [service_child_b])
///
/// root: offer service `foo` from `(coll1,coll2)` to `client`
/// client: use service
#[fuchsia::test]
async fn list_service_instances_from_collections() {
    let use_decl = UseBuilder::service().name("foo").build();
    let mut offers: Vec<_> = ["coll1", "coll2"]
        .into_iter()
        .map(|coll| {
            OfferDecl::Service(OfferServiceDecl {
                source: OfferSource::Collection(coll.parse().unwrap()),
                source_name: "foo".parse().unwrap(),
                source_dictionary: None,
                source_instance_filter: None,
                renamed_instances: None,
                target_name: "foo".parse().unwrap(),
                target: OfferTarget::static_child("client".into()),
                availability: Availability::Required,
            })
        })
        .collect();
    let components = vec![
        (
            "root",
            ComponentDeclBuilder::new()
                .use_realm()
                .offer(offers.remove(0))
                .offer(offers.remove(0))
                .collection_default("coll1")
                .collection_default("coll2")
                .child_default("client")
                .build(),
        ),
        ("client", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        (
            "service_child_a",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "foo".parse().unwrap(),
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        ),
        (
            "service_child_b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: None,
                    target: ExposeTarget::Parent,
                    target_name: "foo".parse().unwrap(),
                    availability: cm_rust::Availability::Required,
                }))
                .capability(CapabilityBuilder::service().name("foo").path("/svc/foo.service"))
                .build(),
        ),
        ("non_service_child", ComponentDeclBuilder::new().build()),
    ];
    let test = RoutingTestBuilder::new("root", components).build().await;

    // Start a few dynamic children in the collections.
    test.create_dynamic_child(
        &Moniker::root(),
        "coll1",
        ChildBuilder::new().name("service_child_a"),
    )
    .await;
    test.create_dynamic_child(
        &Moniker::root(),
        "coll1",
        ChildBuilder::new().name("non_service_child"),
    )
    .await;
    test.create_dynamic_child(
        &Moniker::root(),
        "coll2",
        ChildBuilder::new().name("service_child_b"),
    )
    .await;

    let client_component = test
        .model
        .root()
        .find_and_maybe_resolve(&vec!["client"].try_into().unwrap())
        .await
        .expect("client instance");
    let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
    let source = RouteRequest::UseService(use_decl)
        .route(&client_component)
        .await
        .expect("failed to route service");
    let aggregate_capability_provider = match source {
        RouteSource {
            source: CapabilitySource::AnonymizedAggregate { aggregate_capability_provider, .. },
            relative_path: _,
        } => aggregate_capability_provider,
        _ => panic!("bad capability source"),
    };

    // Check that only the instances that expose the service are listed.
    let instances: HashSet<ChildName> = aggregate_capability_provider
        .list_instances()
        .await
        .unwrap()
        .into_iter()
        .map(|m| match m {
            AggregateInstance::Child(c) => c.clone(),
            _ => panic!("not expected"),
        })
        .collect();
    assert_eq!(instances.len(), 2);
    assert!(instances.contains(&"coll1:service_child_a".try_into().unwrap()));
    assert!(instances.contains(&"coll2:service_child_b".try_into().unwrap()));

    // Try routing to one of the instances.
    let source = aggregate_capability_provider
        .route_instance(&AggregateInstance::Child("coll1:service_child_a".try_into().unwrap()))
        .await
        .expect("failed to route to child");
    match source {
        CapabilitySource::Component {
            capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
            component,
        } => {
            assert_eq!(name, "foo");
            assert_eq!(
                source_path.expect("source path"),
                "/svc/foo.service".parse::<cm_types::Path>().unwrap()
            );
            assert_eq!(component.moniker, vec!["coll1:service_child_a"].try_into().unwrap());
        }
        _ => panic!("bad child capability source"),
    }
}

///   a
///  / \
/// b   c
///      \
///       coll: [foo, bar, baz]
///
/// a: offer service from c to b
/// b: use service
/// c: expose service from collection `coll`
/// foo, bar: expose service to parent
/// baz: does not expose service
#[fuchsia::test]
async fn use_service_from_sibling_collection() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("c".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: None,
                    renamed_instances: None,
                    target: OfferTarget::static_child("b".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .child(ChildBuilder::new().name("b"))
                .child(ChildBuilder::new().name("c"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::service().name("my.service.Service"))
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(
                    UseBuilder::protocol()
                        .source(UseSource::Framework)
                        .name("fuchsia.component.Realm"),
                )
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Collection("coll".parse().unwrap()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "my.service.Service".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .collection_default("coll")
                .build(),
        ),
        (
            "foo",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "my.service.Service".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service_default("my.service.Service")
                .build(),
        ),
        (
            "bar",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "my.service.Service".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service_default("my.service.Service")
                .build(),
        ),
        ("baz", ComponentDeclBuilder::new().build()),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "foo",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path("bar", "/svc/my.service.Service/default".parse().unwrap(), instance_dir)
        .build()
        .await;

    // Populate the collection with dynamic children.
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildBuilder::new().name("foo"),
    )
    .await;
    test.start_instance_and_wait_start(&vec!["c", "coll:foo"].try_into().unwrap())
        .await
        .expect("failed to start `foo`");
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildBuilder::new().name("bar"),
    )
    .await;
    test.start_instance_and_wait_start(&vec!["c", "coll:bar"].try_into().unwrap())
        .await
        .expect("failed to start `bar`");
    test.create_dynamic_child(
        &vec!["c"].try_into().unwrap(),
        "coll",
        ChildBuilder::new().name("baz"),
    )
    .await;

    join!(
        async move {
            test.check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Service {
                    path: "/svc/my.service.Service".parse().unwrap(),
                    instance: ServiceInstance::Aggregated(2),
                    member: "echo".to_string(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        },
        async move {
            while let Some(echo::EchoRequest::EchoString { value, responder }) =
                receiver.next().await
            {
                responder.send(value.as_ref().map(|v| v.as_str())).expect("failed to send reply")
            }
        }
    );
}

///   a
/// / | \
/// b c d
///
/// a: offer service from c to b with filter parameters set on offer
/// b: expose service
/// c: use service (with filter)
/// d: use service (with instance renamed)
#[fuchsia::test]
async fn use_filtered_service_from_sibling() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: Some(vec!["variantinstance".to_string()]),
                    renamed_instances: None,
                    target: OfferTarget::static_child("c".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: None,
                    renamed_instances: Some(vec![NameMapping {
                        source_name: "default".to_string(),
                        target_name: "renamed_default".to_string(),
                    }]),
                    target: OfferTarget::static_child("d".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .child(ChildBuilder::new().name("b"))
                .child(ChildBuilder::new().name("c"))
                .child(ChildBuilder::new().name("d"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "my.service.Service".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service_default("my.service.Service")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::service().name("my.service.Service"))
                .build(),
        ),
        (
            "d",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::service().name("my.service.Service"))
                .build(),
        ),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/variantinstance".parse().unwrap(),
            instance_dir,
        )
        .build()
        .await;

    // Check that instance c only has access to the filtered service instance.
    let namespace_c = test.bind_and_get_namespace(vec!["c"].try_into().unwrap()).await;
    let dir_c = capability_util::take_dir_from_namespace(&namespace_c, "/svc").await;
    let service_dir_c = fuchsia_fs::directory::open_directory(
        &dir_c,
        "my.service.Service",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service");
    let entries: HashSet<String> = fuchsia_fs::directory::readdir(&service_dir_c)
        .await
        .expect("failed to read entries")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries.contains("variantinstance"));
    capability_util::add_dir_to_namespace(&namespace_c, "/svc", dir_c).await;

    // Check that instance d connects to the renamed instances correctly
    let namespace_d = test.bind_and_get_namespace(vec!["d"].try_into().unwrap()).await;
    let dir_d = capability_util::take_dir_from_namespace(&namespace_d, "/svc").await;
    let service_dir_d = fuchsia_fs::directory::open_directory(
        &dir_d,
        "my.service.Service",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service");
    let entries: HashSet<String> = fuchsia_fs::directory::readdir(&service_dir_d)
        .await
        .expect("failed to read entries")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries.contains("renamed_default"));
    capability_util::add_dir_to_namespace(&namespace_d, "/svc", dir_d).await;

    let _server_handle = fasync::Task::spawn(async move {
        while let Some(echo::EchoRequest::EchoString { value, responder }) = receiver.next().await {
            responder.send(value.as_ref().map(|v| v.as_str())).expect("failed to send reply");
        }
    });
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".parse().unwrap(),
            instance: ServiceInstance::Named("variantinstance".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    test.check_use(
        vec!["d"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".parse().unwrap(),
            instance: ServiceInstance::Named("renamed_default".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
}

#[fuchsia::test]
async fn use_filtered_aggregate_service_from_sibling() {
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: Some(vec!["variantinstance".to_string()]),
                    renamed_instances: None,
                    target: OfferTarget::static_child("c".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("b".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    source_instance_filter: Some(vec!["renamed_default".to_string()]),
                    renamed_instances: Some(vec![NameMapping {
                        source_name: "default".to_string(),
                        target_name: "renamed_default".to_string(),
                    }]),
                    target: OfferTarget::static_child("c".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                }))
                .child(ChildBuilder::new().name("b"))
                .child(ChildBuilder::new().name("c"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .expose(ExposeDecl::Service(ExposeServiceDecl {
                    source: ExposeSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target_name: "my.service.Service".parse().unwrap(),
                    target: ExposeTarget::Parent,
                    availability: cm_rust::Availability::Required,
                }))
                .service_default("my.service.Service")
                .build(),
        ),
        (
            "c",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::service().name("my.service.Service"))
                .build(),
        ),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/variantinstance".parse().unwrap(),
            instance_dir,
        )
        .build()
        .await;

    // Check that instance c only has access to the filtered service instance.
    let namespace_c = test.bind_and_get_namespace(vec!["c"].try_into().unwrap()).await;
    let dir_c = capability_util::take_dir_from_namespace(&namespace_c, "/svc").await;
    let service_dir_c = fuchsia_fs::directory::open_directory(
        &dir_c,
        "my.service.Service",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .expect("failed to open service");
    let entries: HashSet<String> = fuchsia_fs::directory::readdir(&service_dir_c)
        .await
        .expect("failed to read entries")
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.contains("variantinstance"));
    assert!(entries.contains("renamed_default"));
    capability_util::add_dir_to_namespace(&namespace_c, "/svc", dir_c).await;

    let _server_handle = fasync::Task::spawn(async move {
        while let Some(echo::EchoRequest::EchoString { value, responder }) = receiver.next().await {
            responder.send(value.as_ref().map(|v| v.as_str())).expect("failed to send reply");
        }
    });
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".parse().unwrap(),
            instance: ServiceInstance::Named("variantinstance".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
    test.check_use(
        vec!["c"].try_into().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".parse().unwrap(),
            instance: ServiceInstance::Named("renamed_default".to_string()),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
}

#[fuchsia::test]
async fn use_anonymized_aggregate_service() {
    let expose_service_decl = ComponentDeclBuilder::new()
        .expose(ExposeDecl::Service(ExposeServiceDecl {
            source: ExposeSource::Self_,
            source_name: "my.service.Service".parse().unwrap(),
            source_dictionary: None,
            target_name: "my.service.Service".parse().unwrap(),
            target: ExposeTarget::Parent,
            availability: cm_rust::Availability::Required,
        }))
        .service_default("my.service.Service")
        .build();
    let components = vec![
        (
            "a",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("b".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                    source_instance_filter: None,
                    renamed_instances: None,
                }))
                .service_default("my.service.Service")
                .child(ChildBuilder::new().name("b"))
                .build(),
        ),
        (
            "b",
            ComponentDeclBuilder::new()
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("c".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("e".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                    source_instance_filter: None,
                    renamed_instances: None,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::static_child("d".to_string()),
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("e".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                    source_instance_filter: None,
                    renamed_instances: None,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Parent,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("e".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                    source_instance_filter: None,
                    renamed_instances: None,
                }))
                .offer(OfferDecl::Service(OfferServiceDecl {
                    source: OfferSource::Self_,
                    source_name: "my.service.Service".parse().unwrap(),
                    source_dictionary: None,
                    target: OfferTarget::static_child("e".to_string()),
                    target_name: "my.service.Service".parse().unwrap(),
                    availability: Availability::Required,
                    source_instance_filter: None,
                    renamed_instances: None,
                }))
                .service_default("my.service.Service")
                .child(ChildBuilder::new().name("c"))
                .child(ChildBuilder::new().name("d"))
                .child(ChildBuilder::new().name("e"))
                .build(),
        ),
        ("c", expose_service_decl.clone()),
        ("d", expose_service_decl),
        (
            "e",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::service().name("my.service.Service"))
                .build(),
        ),
    ];

    let (directory_entry, mut receiver) = create_service_directory_entry::<echo::EchoMarker>();
    let instance_dir = pseudo_directory! {
        "echo" => directory_entry,
    };
    let test = RoutingTestBuilder::new("a", components)
        .add_outgoing_path(
            "a",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "b",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "c",
            "/svc/my.service.Service/default".parse().unwrap(),
            instance_dir.clone(),
        )
        .add_outgoing_path(
            "d",
            "/svc/my.service.Service/variantinstance".parse().unwrap(),
            instance_dir,
        )
        .build()
        .await;
    let _server_handle = fasync::Task::spawn(async move {
        while let Some(echo::EchoRequest::EchoString { value, responder }) = receiver.next().await {
            responder.send(value.as_ref().map(|v| v.as_str())).unwrap();
        }
    });

    test.check_use(
        "b/e".parse().unwrap(),
        CheckUse::Service {
            path: "/svc/my.service.Service".parse().unwrap(),
            instance: ServiceInstance::Aggregated(4),
            member: "echo".to_string(),
            expected_res: ExpectedResult::Ok,
        },
    )
    .await;
}
