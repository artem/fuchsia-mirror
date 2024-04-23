// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod availability;
pub mod component_id_index;
pub mod policy;
pub mod rights;
pub mod storage;
pub mod storage_admin;

use {
    ::component_id_index::InstanceId,
    assert_matches::assert_matches,
    async_trait::async_trait,
    camino::Utf8PathBuf,
    cm_config::{
        AllowlistEntry, AllowlistEntryBuilder, CapabilityAllowlistKey, CapabilityAllowlistSource,
        DebugCapabilityAllowlistEntry, DebugCapabilityKey,
    },
    cm_moniker::InstancedMoniker,
    cm_rust::*,
    cm_rust_testing::*,
    cm_types::Name,
    fidl::endpoints::ProtocolMarker,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as fcrunner,
    fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fuchsia_zircon_status as zx,
    moniker::{ExtendedMoniker, Moniker, MonikerBase},
    routing::{
        capability_source::{
            AggregateCapability, AggregateMember, CapabilitySource, ComponentCapability,
            FilteredAggregateCapabilityRouteData, InternalCapability,
        },
        component_instance::ComponentInstanceInterface,
        error::RoutingError,
        mapper::NoopRouteMapper,
        route_capability, RouteRequest, RouteSource,
    },
    std::{
        collections::HashSet,
        marker::PhantomData,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Construct a capability path for the hippo service.
pub fn default_service_capability() -> cm_types::Path {
    "/svc/hippo".parse().unwrap()
}

/// Construct a capability path for the hippo directory.
pub fn default_directory_capability() -> cm_types::Path {
    "/data/hippo".parse().unwrap()
}

/// Returns an empty component decl for an executable component.
pub fn default_component_decl() -> ComponentDecl {
    ComponentDecl::default()
}

/// Returns an empty component decl set up to have a non-empty program and to use the "test_runner"
/// runner.
pub fn component_decl_with_test_runner() -> ComponentDecl {
    ComponentDecl {
        program: Some(ProgramDecl {
            runner: Some(TEST_RUNNER_NAME.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        }),
        ..Default::default()
    }
}

/// Same as above but with the component also exposing Binder protocol.
pub fn component_decl_with_exposed_binder() -> ComponentDecl {
    ComponentDecl {
        program: Some(ProgramDecl {
            runner: Some(TEST_RUNNER_NAME.parse().unwrap()),
            info: fdata::Dictionary { entries: Some(vec![]), ..Default::default() },
        }),
        exposes: vec![ExposeBuilder::protocol()
            .source(ExposeSource::Framework)
            .name(fcomponent::BinderMarker::DEBUG_NAME)
            .build()],
        ..Default::default()
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ExpectedResult {
    Ok,
    Err(zx::Status),
    ErrWithNoEpitaph,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ComponentEventRoute {
    /// Name of the component this was routed through
    pub component: String,
    /// Downscoping that was applied on behalf of the component
    /// None means that no downscoping was applied.
    /// Each String is the name of a child component to which the downscope
    /// applies.
    pub scope: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ServiceInstance {
    Named(String),
    Aggregated(usize),
}

#[derive(Debug)]
pub enum CheckUse {
    Protocol {
        path: cm_types::Path,
        expected_res: ExpectedResult,
    },
    Service {
        path: cm_types::Path,
        instance: ServiceInstance,
        member: String,
        expected_res: ExpectedResult,
    },
    Directory {
        path: cm_types::Path,
        file: PathBuf,
        expected_res: ExpectedResult,
    },
    Storage {
        path: cm_types::Path,
        // The moniker from the storage declaration to the use declaration. Only
        // used if `expected_res` is Ok.
        storage_relation: Option<InstancedMoniker>,
        // The backing directory for this storage is in component manager's namespace, not the
        // test's isolated test directory.
        from_cm_namespace: bool,
        storage_subdir: Option<String>,
        expected_res: ExpectedResult,
    },
    StorageAdmin {
        // The moniker from the storage declaration to the use declaration.
        storage_relation: InstancedMoniker,
        // The backing directory for this storage is in component manager's namespace, not the
        // test's isolated test directory.
        from_cm_namespace: bool,

        storage_subdir: Option<String>,
        expected_res: ExpectedResult,
    },
    EventStream {
        path: cm_types::Path,
        scope: Vec<ComponentEventRoute>,
        name: Name,
        expected_res: ExpectedResult,
    },
}

impl CheckUse {
    pub fn default_directory(expected_res: ExpectedResult) -> Self {
        Self::Directory {
            path: default_directory_capability(),
            file: PathBuf::from("hippo"),
            expected_res,
        }
    }
}

// This function should reproduce the logic of `crate::storage::generate_storage_path`.
pub fn generate_storage_path(
    subdir: Option<String>,
    moniker: &InstancedMoniker,
    instance_id: Option<&InstanceId>,
) -> PathBuf {
    if let Some(id) = instance_id {
        return id.to_string().into();
    }
    let mut path = moniker.path().iter();
    let mut dir_path = vec![];
    if let Some(subdir) = subdir {
        dir_path.push(subdir);
    }
    if let Some(p) = path.next() {
        dir_path.push(p.to_string());
    }
    while let Some(p) = path.next() {
        dir_path.push("children".to_string());
        dir_path.push(p.to_string());
    }

    // Storage capabilities used to have a hardcoded set of types, which would be appended
    // here. To maintain compatibility with the old paths (and thus not lose data when this was
    // migrated) we append "data" here. This works because this is the only type of storage
    // that was actually used in the wild.
    //
    // This is only temporary, until the storage instance id migration changes this layout.
    dir_path.push("data".to_string());
    dir_path.into_iter().collect()
}

/// A `RoutingTestModel` attempts to use capabilities from instances in a component model
/// and checks the result of the attempt against an expectation.
#[async_trait]
pub trait RoutingTestModel {
    type C: ComponentInstanceInterface + std::fmt::Debug + 'static;

    /// Checks a `use` declaration at `moniker` by trying to use `capability`.
    async fn check_use(&self, moniker: Moniker, check: CheckUse);

    /// Checks using a capability from a component's exposed directory.
    async fn check_use_exposed_dir(&self, moniker: Moniker, check: CheckUse);

    /// Looks up a component instance by its moniker.
    async fn look_up_instance(&self, moniker: &Moniker) -> Result<Arc<Self::C>, anyhow::Error>;

    /// Checks that a use declaration of `path` at `moniker` can be opened with
    /// Fuchsia file operations.
    async fn check_open_node(&self, moniker: Moniker, path: cm_types::Path);

    /// Create a file with the given contents in the test dir, along with any subdirectories
    /// required.
    async fn create_static_file(&self, path: &Path, contents: &str) -> Result<(), anyhow::Error>;

    /// Installs a new directory at `path` in the test's namespace.
    fn install_namespace_directory(&self, path: &str);

    /// Creates a subdirectory in the outgoing dir's /data directory.
    fn add_subdir_to_data_directory(&self, subdir: &str);

    /// Asserts that the subdir given by `path` within the test directory contains exactly the
    /// filenames in `expected`.
    async fn check_test_subdir_contents(&self, path: &str, expected: Vec<String>);

    /// Asserts that the directory at absolute `path` contains exactly the filenames in `expected`.
    async fn check_namespace_subdir_contents(&self, path: &str, expected: Vec<String>);

    /// Asserts that the subdir given by `path` within the test directory contains a file named `expected`.
    async fn check_test_subdir_contains(&self, path: &str, expected: String);

    /// Asserts that the tree in the test directory under `path` contains a file named `expected`.
    async fn check_test_dir_tree_contains(&self, expected: String);
}

/// Builds an implementation of `RoutingTestModel` from a set of `ComponentDecl`s.
#[async_trait]
pub trait RoutingTestModelBuilder {
    type Model: RoutingTestModel;

    /// Create a new builder. Both string arguments refer to component names, not URLs,
    /// ex: "a", not "test:///a" or "test:///a_resolved".
    fn new(root_component: &str, components: Vec<(&'static str, ComponentDecl)>) -> Self;

    /// Set the capabilities that should be available from the top instance's namespace.
    fn set_namespace_capabilities(&mut self, caps: Vec<CapabilityDecl>);

    /// Set the capabilities that should be available as built-in capabilities.
    fn set_builtin_capabilities(&mut self, caps: Vec<CapabilityDecl>);

    /// Register a mock `runner` in the built-in environment.
    fn register_mock_builtin_runner(&mut self, runner: &str);

    /// Add a custom capability security policy to restrict routing of certain caps.
    fn add_capability_policy(
        &mut self,
        key: CapabilityAllowlistKey,
        allowlist: HashSet<AllowlistEntry>,
    );

    /// Add a custom debug capability security policy to restrict routing of certain caps.
    fn add_debug_capability_policy(
        &mut self,
        key: DebugCapabilityKey,
        allowlist: HashSet<DebugCapabilityAllowlistEntry>,
    );

    /// Sets the path to the component ID index for the test model.
    fn set_component_id_index_path(&mut self, path: Utf8PathBuf);

    async fn build(self) -> Self::Model;
}

/// The CommonRoutingTests are run under multiple contexts, e.g. both on Fuchsia under
/// component_manager and on the build host under cm_fidl_analyzer. This macro helps ensure that all
/// tests are run in each context.
#[macro_export]
macro_rules! instantiate_common_routing_tests {
    ($builder_impl:path) => {
        // New CommonRoutingTest tests must be added to this list to run.
        instantiate_common_routing_tests! {
            $builder_impl,
            test_use_from_parent,
            test_use_from_child,
            test_use_from_self,
            test_use_from_grandchild,
            test_use_from_grandparent,
            test_use_from_sibling_no_root,
            test_use_from_sibling_root,
            test_use_from_niece,
            test_use_kitchen_sink,
            test_use_from_component_manager_namespace,
            test_offer_from_component_manager_namespace,
            test_use_not_offered,
            test_use_offer_source_not_exposed,
            test_use_offer_source_not_offered,
            test_use_from_expose,
            test_route_protocol_from_expose,
            test_use_from_expose_to_framework,
            test_offer_from_non_executable,
            test_route_filtered_aggregate_service,
            test_route_filtered_aggregate_service_with_conflicting_filter_fails,
            test_route_anonymized_aggregate_service,
            test_use_directory_with_subdir_from_grandparent,
            test_use_directory_with_subdir_from_sibling,
            test_expose_directory_with_subdir,
            test_expose_from_self_and_child,
            test_use_not_exposed,
            test_use_protocol_denied_by_capability_policy,
            test_use_directory_with_alias_denied_by_capability_policy,
            test_use_protocol_partial_chain_allowed_by_capability_policy,
            test_use_protocol_component_provided_capability_policy,
            test_use_from_component_manager_namespace_denied_by_policy,
            test_event_stream_aliasing,
            test_use_event_stream_from_above_root,
            test_use_event_stream_from_above_root_and_downscoped,
            test_can_offer_capability_requested_event,
            test_route_service_from_parent,
            test_route_service_from_child,
            test_route_service_from_sibling,
            test_route_filtered_service_from_sibling,
            test_route_renamed_service_instance_from_sibling,
            test_use_builtin_from_grandparent,
            test_invalid_use_from_component_manager,
            test_invalid_offer_from_component_manager,
            test_route_runner_from_parent_environment,
            test_route_runner_from_grandparent_environment,
            test_route_runner_from_sibling_environment,
            test_route_runner_from_inherited_environment,
            test_route_runner_from_environment_not_found,
            test_route_builtin_runner,
            test_route_builtin_runner_from_root_env,
            test_route_builtin_runner_not_found,
            test_route_builtin_runner_from_root_env_registration_not_found,
            test_use_runner_from_child,
            test_use_runner_from_parent,
            test_use_runner_from_parent_environment,
            test_use_config_from_self,
            test_use_config_from_parent,
            test_use_config_from_void,
        }
    };
    ($builder_impl:path, $test:ident, $($remaining:ident),+ $(,)?) => {
        instantiate_common_routing_tests! { $builder_impl, $test }
        instantiate_common_routing_tests! { $builder_impl, $($remaining),+ }
    };
    ($builder_impl:path, $test:ident) => {
        // TODO(https://fxbug.dev/42157685): #[fuchsia::test] did not work inside a declarative macro, so this
        // falls back on fuchsia_async and manual logging initialization for now.
        #[fuchsia_async::run_singlethreaded(test)]
        async fn $test() {
            fuchsia::init_logging_for_component_with_executor(
                || {}, fuchsia::LoggingOptions::default())();
            $crate::CommonRoutingTest::<$builder_impl>::new().$test().await
        }
    };
}

pub struct CommonRoutingTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}
impl<T: RoutingTestModelBuilder> CommonRoutingTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory /data/foo from self as /data/bar
    /// a: offers service /svc/foo from self as /svc/bar
    /// a: offers service /svc/file from self as /svc/device
    /// b: uses directory /data/bar as /data/hippo
    /// b: uses service /svc/bar as /svc/hippo
    /// b: uses service /svc/device
    ///
    /// The test related to `/svc/file` is used to verify that services that require
    /// extended flags, like `OPEN_FLAG_DESCRIBE`, work correctly. This often
    /// happens for fuchsia.hardware protocols that compose fuchsia.io protocols,
    /// and expect that `fdio_open` should operate correctly.
    pub async fn test_use_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .protocol_default("file")
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .target_name("bar_data")
                            .source(OfferSource::Self_)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("file")
                            .target_name("device")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("bar_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("bar").path("/svc/hippo"))
                    .use_(UseBuilder::protocol().name("device"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model.check_open_node(vec!["b"].try_into().unwrap(), "/svc/device".parse().unwrap()).await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: uses directory /data/bar from #b as /data/hippo
    /// a: uses service /svc/bar from #b as /svc/hippo
    /// b: exposes directory /data/foo from self as /data/bar
    /// b: exposes service /svc/foo from self as /svc/bar
    pub async fn test_use_from_child(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(
                        UseBuilder::directory()
                            .source_static_child("b")
                            .name("bar_data")
                            .path("/data/hippo"),
                    )
                    .use_(
                        UseBuilder::protocol()
                            .source_static_child("b")
                            .name("bar")
                            .path("/svc/hippo"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    /// a: uses protocol /svc/hippo from self
    pub async fn test_use_from_self(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .protocol_default("hippo")
                .use_(UseBuilder::protocol().source(UseSource::Self_).name("hippo"))
                .build(),
        )];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: uses /data/baz from #b as /data/hippo
    /// a: uses /svc/baz from #b as /svc/hippo
    /// b: exposes directory /data/bar from #c as /data/baz
    /// b: exposes service /svc/bar from #c as /svc/baz
    /// c: exposes directory /data/foo from self as /data/bar
    /// c: exposes service /svc/foo from self as /svc/bar
    pub async fn test_use_from_grandchild(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(
                        UseBuilder::directory()
                            .source_static_child("b")
                            .name("baz_data")
                            .path("/data/hippo"),
                    )
                    .use_(
                        UseBuilder::protocol()
                            .source_static_child("b")
                            .name("baz")
                            .path("/svc/hippo"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::directory()
                            .name("bar_data")
                            .source_static_child("c")
                            .target_name("baz_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("bar")
                            .target_name("baz")
                            .source_static_child("c"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data/foo from self as /data/bar
    /// a: offers service /svc/foo from self as /svc/bar
    /// b: offers directory /data/bar from realm as /data/baz
    /// b: offers service /svc/bar from realm as /svc/baz
    /// c: uses /data/baz as /data/hippo
    /// c: uses /svc/baz as /svc/hippo
    pub async fn test_use_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .target_name("bar_data")
                            .source(OfferSource::Self_)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("bar_data")
                            .target_name("baz_data")
                            .source(OfferSource::Parent)
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("bar")
                            .target_name("baz")
                            .source(OfferSource::Parent)
                            .target_static_child("c"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("baz_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("baz").path("/svc/hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers service /svc/builtin.Echo from realm
    /// b: offers service /svc/builtin.Echo from realm
    /// c: uses /svc/builtin.Echo as /svc/hippo
    pub async fn test_use_builtin_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::protocol()
                            .name("builtin.Echo")
                            .source(OfferSource::Parent)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::protocol()
                            .name("builtin.Echo")
                            .source(OfferSource::Parent)
                            .target_static_child("c"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name("builtin.Echo").path("/svc/hippo"))
                    .build(),
            ),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Protocol(ProtocolDecl {
            name: "builtin.Echo".parse().unwrap(),
            source_path: None,
            delivery: Default::default(),
        })]);
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///     a
    ///    /
    ///   b
    ///  / \
    /// d   c
    ///
    /// d: exposes directory /data/foo from self as /data/bar
    /// b: offers directory /data/bar from d as /data/foobar to c
    /// c: uses /data/foobar as /data/hippo
    pub async fn test_use_from_sibling_no_root(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("bar_data")
                            .target_name("foobar_data")
                            .source_static_child("d")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("bar")
                            .target_name("foobar")
                            .source_static_child("d")
                            .target_static_child("c"),
                    )
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foobar_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("foobar").path("/svc/hippo"))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data/foo from self as /data/bar
    /// a: offers directory /data/bar from b as /data/baz to c
    /// c: uses /data/baz as /data/hippo
    pub async fn test_use_from_sibling_root(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("bar_data")
                            .target_name("baz_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("bar")
                            .target_name("baz")
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("baz_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("baz").path("/svc/hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///     a
    ///    / \
    ///   b   c
    ///  /
    /// d
    ///
    /// d: exposes directory /data/foo from self as /data/bar
    /// b: exposes directory /data/bar from d as /data/baz
    /// a: offers directory /data/baz from b as /data/foobar to c
    /// c: uses /data/foobar as /data/hippo
    pub async fn test_use_from_niece(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("baz_data")
                            .target_name("foobar_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("baz")
                            .target_name("foobar")
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::directory()
                            .name("bar_data")
                            .source_static_child("d")
                            .target_name("baz_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("bar")
                            .target_name("baz")
                            .source_static_child("d"),
                    )
                    .child_default("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foobar_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("foobar").path("/svc/hippo"))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///      a
    ///     / \
    ///    /   \
    ///   b     c
    ///  / \   / \
    /// d   e f   g
    ///            \
    ///             h
    ///
    /// a,d,h: hosts /svc/foo and /data/foo
    /// e: uses /svc/foo as /svc/hippo from a, uses /data/foo as /data/hippo from d
    /// f: uses /data/foo from d as /data/hippo, uses /svc/foo from h as /svc/hippo
    pub async fn test_use_kitchen_sink(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol_default("foo")
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo")
                            .target_name("foo_from_a_svc")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_from_d_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new_empty_component()
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_from_d_data")
                            .source_static_child("d")
                            .target_static_child("e")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo_from_a_svc")
                            .source(OfferSource::Parent)
                            .target_static_child("e"),
                    )
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_from_d_data")
                            .source_static_child("d")
                            .rights(fio::R_STAR_DIR),
                    )
                    .child_default("d")
                    .child_default("e")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new_empty_component()
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_from_d_data")
                            .source(OfferSource::Parent)
                            .target_static_child("f")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo_from_h_svc")
                            .source_static_child("g")
                            .target_static_child("f"),
                    )
                    .child_default("f")
                    .child_default("g")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("foo_from_d_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .build(),
            ),
            (
                "e",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foo_from_d_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("foo_from_a_svc").path("/svc/hippo"))
                    .build(),
            ),
            (
                "f",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foo_from_d_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("foo_from_h_svc").path("/svc/hippo"))
                    .build(),
            ),
            (
                "g",
                ComponentDeclBuilder::new_empty_component()
                    .expose(
                        ExposeBuilder::protocol().name("foo_from_h_svc").source_static_child("h"),
                    )
                    .child_default("h")
                    .build(),
            ),
            (
                "h",
                ComponentDeclBuilder::new()
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("foo_from_h_svc")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "e"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b", "e"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "f"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["c", "f"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///
    /// a: uses directory /use_from_cm_namespace/data/foo as foo_data
    /// a: uses service /use_from_cm_namespace/svc/foo as foo
    pub async fn test_use_from_component_manager_namespace(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::directory().name("foo_data").path("/data/hippo"))
                .use_(UseBuilder::protocol().name("foo").path("/svc/hippo"))
                .build(),
        )];
        let namespace_capabilities = vec![
            CapabilityBuilder::directory()
                .name("foo_data")
                .path("/use_from_cm_namespace/data/foo")
                .build(),
            CapabilityBuilder::protocol()
                .name("foo")
                .path("/use_from_cm_namespace/svc/foo")
                .build(),
        ];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        let model = builder.build().await;

        model.install_namespace_directory("/use_from_cm_namespace");
        model.check_use(Moniker::root(), CheckUse::default_directory(ExpectedResult::Ok)).await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///    \
    ///     b
    ///
    /// a: offers directory /offer_from_cm_namespace/data/foo from realm as bar_data
    /// a: offers service /offer_from_cm_namespace/svc/foo from realm as bar
    /// b: uses directory bar_data as /data/hippo
    /// b: uses service bar as /svc/hippo
    pub async fn test_offer_from_component_manager_namespace(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .target_name("bar_data")
                            .source(OfferSource::Parent)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(OfferSource::Parent)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("bar_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("bar").path("/svc/hippo"))
                    .build(),
            ),
        ];
        let namespace_capabilities = vec![
            CapabilityBuilder::directory()
                .name("foo_data")
                .path("/offer_from_cm_namespace/data/foo")
                .build(),
            CapabilityBuilder::protocol()
                .name("foo")
                .path("/offer_from_cm_namespace/svc/foo")
                .build(),
        ];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        let model = builder.build().await;

        model.install_namespace_directory("/offer_from_cm_namespace");
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Ok),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses directory /data/hippo as /data/hippo, but it's not in its realm
    /// b: uses service /svc/hippo as /svc/hippo, but it's not in its realm
    pub async fn test_use_not_offered(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new_empty_component().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
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
    /// a: offers directory /data/hippo from b as /data/hippo, but it's not exposed by b
    /// a: offers service /svc/hippo from b as /svc/hippo, but it's not exposed by b
    /// c: uses directory /data/hippo as /data/hippo
    /// c: uses service /svc/hippo as /svc/hippo
    pub async fn test_use_offer_source_not_exposed(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .offer(
                        OfferBuilder::directory()
                            .name("hippo_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            ("b", component_decl_with_test_runner()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
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
    ///      \
    ///       c
    ///
    /// b: offers directory /data/hippo from its realm as /data/hippo, but it's not offered by a
    /// b: offers service /svc/hippo from its realm as /svc/hippo, but it's not offfered by a
    /// c: uses directory /data/hippo as /data/hippo
    /// c: uses service /svc/hippo as /svc/hippo
    pub async fn test_use_offer_source_not_offered(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new_empty_component()
                    .offer(
                        OfferBuilder::directory()
                            .name("hippo_data")
                            .source(OfferSource::Parent)
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Parent)
                            .target_static_child("c"),
                    )
                    .child_default("c")
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
        let test = T::new("a", components).build().await;
        test.check_use(
            vec!["b", "c"].try_into().unwrap(),
            CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
        )
        .await;
        test.check_use(
            vec!["b", "c"].try_into().unwrap(),
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
    ///      \
    ///       c
    ///
    /// b: uses directory /data/hippo as /data/hippo, but it's exposed to it, not offered
    /// b: uses service /svc/hippo as /svc/hippo, but it's exposed to it, not offered
    /// c: exposes /data/hippo
    /// c: exposes /svc/hippo
    pub async fn test_use_from_expose(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new_empty_component().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("hippo"))
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("hippo_data").path("/data/foo"))
                    .protocol_default("hippo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("hippo_data")
                            .source(ExposeSource::Self_)
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(ExposeBuilder::protocol().name("hippo").source(ExposeSource::Self_))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
    }

    /// a
    ///  \
    ///   b
    ///
    /// a: exposes "foo" to parent from child
    /// b: exposes "foo" to parent from self
    pub async fn test_route_protocol_from_expose(&self) {
        let expose_decl = ExposeBuilder::protocol()
            .name("foo")
            .source(ExposeSource::Child("b".parse().unwrap()))
            .build();
        let expected_protocol_decl = CapabilityBuilder::protocol().name("foo").build();

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new().expose(expose_decl.clone()).child_default("b").build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::protocol().name("foo").source(ExposeSource::Self_))
                    .capability(expected_protocol_decl.clone())
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let root_instance = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let expected_source_moniker = Moniker::parse_str("/b").unwrap();

        let CapabilityDecl::Protocol(expected_protocol_decl) = expected_protocol_decl else {
            unreachable!();
        };
        let ExposeDecl::Protocol(expose_decl) = expose_decl else {
            unreachable!();
        };
        assert_matches!(
        route_capability(RouteRequest::ExposeProtocol(expose_decl), &root_instance, &mut NoopRouteMapper).await,
            Ok(RouteSource {
                source: CapabilitySource::<
                    <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C
                    >::Component {
                        capability: ComponentCapability::Protocol(capability_decl),
                        component,
                    },
                relative_path,
            }) if capability_decl == expected_protocol_decl && component.moniker == expected_source_moniker && relative_path.is_dot()
        );
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// b: exposes directory /data/foo from self as /data/bar to framework (NOT realm)
    /// a: offers directory /data/bar from b as /data/baz to c, but it is not exposed via realm
    /// c: uses /data/baz as /data/hippo
    pub async fn test_use_from_expose_to_framework(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("bar_data")
                            .target_name("baz_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("bar")
                            .target_name("baz")
                            .source_static_child("b")
                            .target_static_child("c"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("bar_data")
                            .target(ExposeTarget::Framework)
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("bar")
                            .source(ExposeSource::Self_)
                            .target(ExposeTarget::Framework),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("baz_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("baz").path("/svc/hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
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
    ///
    /// a: offers directory /data/hippo to b, but a is not executable
    /// a: offers service /svc/hippo to b, but a is not executable
    /// b: uses directory /data/hippo as /data/hippo, but it's not in its realm
    /// b: uses service /svc/hippo as /svc/hippo, but it's not in its realm
    pub async fn test_offer_from_non_executable(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new_empty_component()
                    .capability(CapabilityBuilder::directory().name("hippo_data").path("/data"))
                    .protocol_default("hippo")
                    .offer(
                        OfferBuilder::directory()
                            .name("hippo_data")
                            .source(OfferSource::Self_)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("hippo_data").path("/data/hippo"))
                    .use_(UseBuilder::protocol().name("hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::NOT_FOUND)),
            )
            .await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service with non-conflicting filters should succeed.
    pub async fn test_route_filtered_aggregate_service(&self) {
        let expected_service_decl = CapabilityBuilder::service().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("b")
                            .target_static_child("d")
                            .source_instance_filter(vec![
                                "instance_0".to_string(),
                                "instance_1".to_string(),
                            ]),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("d")
                            .source_instance_filter(vec![
                                "instance_2".to_string(),
                                "instance_3".to_string(),
                            ]),
                    )
                    .child_default("b")
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .capability(expected_service_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .capability(expected_service_decl.clone())
                    .build(),
            ),
            ("d", ComponentDeclBuilder::new().use_(UseBuilder::service().name("foo")).build()),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");

        let source = route_capability(
            RouteRequest::UseService(UseServiceDecl {
                source: UseSource::Parent,
                source_name: "foo".parse().unwrap(),
                source_dictionary: Default::default(),
                target_path: "/svc/foo".parse().unwrap(),
                dependency_type: DependencyType::Strong,
                availability: Availability::Required,
            }),
            &d_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::FilteredAggregate {
                        capability: AggregateCapability::Service(name),
                        ..
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///   |
    ///   b
    ///  /|
    /// c d
    ///
    /// a: offers "foo" from self to `b`
    /// b: offers "foo" from parent, c, and itself to d, forming an aggregate
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service without specifying a source_instance_filter should fail.
    pub async fn test_route_anonymized_aggregate_service(&self) {
        let expected_service_decl = CapabilityBuilder::service().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .capability(expected_service_decl.clone())
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("d"),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source(OfferSource::Parent)
                            .target_static_child("d"),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source(OfferSource::Self_)
                            .target_static_child("d"),
                    )
                    .capability(expected_service_decl.clone())
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .capability(expected_service_decl.clone())
                    .build(),
            ),
            ("d", ComponentDeclBuilder::new().use_(UseBuilder::service().name("foo")).build()),
        ];
        let test = T::new("a", components).build().await;

        let d_component = test.look_up_instance(&"b/d".parse().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseService(UseServiceDecl {
                source: UseSource::Parent,
                source_name: "foo".parse().unwrap(),
                source_dictionary: Default::default(),
                target_path: "/svc/foo".parse().unwrap(),
                dependency_type: DependencyType::Strong,
                availability: Availability::Required,
            }),
            &d_component,
            &mut NoopRouteMapper,
        )
        .await
        .unwrap();
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::AnonymizedAggregate {
                        capability: AggregateCapability::Service(name),
                        members,
                        ..
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(members.len(), 3);
                for c in [AggregateMember::Child("c".try_into().unwrap()), AggregateMember::Parent,
                AggregateMember::Self_] {
                    assert!(members.contains(&c));
                }
            }
            _ => panic!("bad capability source"),
        }
    }

    ///   a
    /// / | \
    /// b c d
    ///
    /// a: offers "foo" from both b and c to d
    /// b: exposes "foo" to parent from self
    /// c: exposes "foo" to parent from self
    /// d: uses "foo" from parent
    /// routing an aggregate service with conflicting source_instance_filters should fail.
    pub async fn test_route_filtered_aggregate_service_with_conflicting_filter_fails(&self) {
        let expected_service_decl = CapabilityBuilder::service().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("b")
                            .target_static_child("d")
                            .source_instance_filter(vec![
                                "default".to_string(),
                                "other_a".to_string(),
                            ]),
                    )
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("d")
                            .source_instance_filter(vec![
                                "default".to_string(),
                                "other_b".to_string(),
                            ]),
                    )
                    .child_default("b")
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .capability(expected_service_decl.clone())
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .capability(expected_service_decl.clone())
                    .build(),
            ),
            ("d", ComponentDeclBuilder::new().use_(UseBuilder::service().name("foo")).build()),
        ];
        let model = T::new("a", components).build().await;

        let d_component =
            model.look_up_instance(&vec!["d"].try_into().unwrap()).await.expect("b instance");
        assert_matches!(
            route_capability(
                RouteRequest::UseService(UseServiceDecl {
                    source: UseSource::Parent,
                    source_name: "foo".parse().unwrap(),
                    source_dictionary: Default::default(),
                    target_path: "/svc/foo".parse().unwrap(),
                    dependency_type: DependencyType::Strong,
                    availability: Availability::Required,
                }),
                &d_component,
                &mut NoopRouteMapper
            )
            .await,
            Err(RoutingError::UnsupportedRouteSource { source_type: _ })
        );
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: offers directory /data/foo from self with subdir 's1/s2'
    /// b: offers directory /data/foo from realm with subdir 's3'
    /// c: uses /data/foo as /data/hippo
    pub async fn test_use_directory_with_subdir_from_grandparent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .source(OfferSource::Self_)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR)
                            .subdir("s1/s2"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .source(OfferSource::Parent)
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR)
                            .subdir("s3"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foo_data").path("/data/hippo").subdir("s4"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/s4/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: default_directory_capability(),
                    file: PathBuf::from("inner"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///  / \
    /// b   c
    ///
    ///
    /// b: exposes directory /data/foo from self with subdir 's1/s2'
    /// a: offers directory /data/foo from `b` to `c` with subdir 's3'
    /// c: uses /data/foo as /data/hippo
    pub async fn test_use_directory_with_subdir_from_sibling(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .source_static_child("b")
                            .target_static_child("c")
                            .rights(fio::R_STAR_DIR)
                            .subdir("s3"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .rights(fio::R_STAR_DIR)
                            .subdir("s1/s2"),
                    )
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("foo_data").path("/data/hippo"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: default_directory_capability(),
                    file: PathBuf::from("inner"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// c: exposes /data/foo from self
    /// b: exposes /data/foo from `c` with subdir `s1/s2`
    /// a: exposes /data/foo from `b` with subdir `s3` as /data/hippo
    /// use /data/hippo from a's exposed dir
    pub async fn test_expose_directory_with_subdir(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source_static_child("b")
                            .target_name("hippo_data")
                            .subdir("s3"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source_static_child("c")
                            .subdir("s1/s2"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .rights(fio::R_STAR_DIR),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .create_static_file(Path::new("foo/s1/s2/s3/inner"), "hello")
            .await
            .expect("failed to create file");
        model
            .check_use_exposed_dir(
                Moniker::root(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("inner"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    pub async fn test_expose_from_self_and_child(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::directory()
                            .name("hippo_data")
                            .source_static_child("c")
                            .target_name("hippo_bar_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("hippo")
                            .target_name("hippo_bar")
                            .source_static_child("c"),
                    )
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("hippo_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("hippo")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_bar_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo_bar".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    pub async fn test_use_not_exposed(&self) {
        let components = vec![
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            ("b", ComponentDeclBuilder::new().child_default("c").build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .protocol_default("foo")
                    .expose(
                        ExposeBuilder::directory()
                            .name("foo_data")
                            .source(ExposeSource::Self_)
                            .target_name("hippo_data")
                            .rights(fio::R_STAR_DIR),
                    )
                    .expose(
                        ExposeBuilder::protocol()
                            .name("foo")
                            .target_name("hippo")
                            .source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        // Capability is only exposed from "c", so it only be usable from there.

        // When trying to open a capability that's not exposed to realm, there's no node for it in the
        // exposed dir, so no routing takes place.
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Directory {
                    path: "/hippo_data".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
        model
            .check_use_exposed_dir(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/hippo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///   (cm)
    ///    |
    ///    a
    ///
    /// a: uses an invalid service from the component manager.
    pub async fn test_invalid_use_from_component_manager(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("invalid").path("/svc/valid"))
                .build(),
        )];

        // Try and use the service. We expect a failure.
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: "/svc/valid".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
    }

    ///   (cm)
    ///    |
    ///    a
    ///    |
    ///    b
    ///
    /// a: offers an invalid service from the component manager to "b".
    /// b: attempts to use the service
    pub async fn test_invalid_offer_from_component_manager(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::protocol()
                            .name("invalid")
                            .target_name("valid")
                            .source(OfferSource::Parent)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("valid")).build()),
        ];

        // Try and use the service. We expect a failure.
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: "/svc/valid".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx::Status::NOT_FOUND),
                },
            )
            .await;
    }

    /*
        TODO(https://fxbug.dev/42059303): Allow exposing from parent.

        /// Tests exposing an event_stream from a child through its parent down to another
        /// unrelated child.
        ///        a
        ///         \
        ///          b
        ///          /\
        ///          c f
        ///          /\
        ///          d e
        /// c exposes started with a scope of e (but not d)
        /// to b, which then offers that to f.
        pub async fn test_expose_event_stream_with_scope(&self) {
            let components = vec![
                ("a", ComponentDeclBuilder::new().child_default("b").build()),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                            source: OfferSource::Child(ChildRef {
                                name: "c".to_string(),
                                collection: None,
                            }),
                            source_name: "started".parse().unwrap(),
                            scope: None,
                            filter: None,
                            target: OfferTarget::Child(ChildRef {
                                name: "f".to_string(),
                                collection: None,
                            }),
                            target_name: "started".parse().unwrap(),
                            availability: Availability::Required,
                        }))
                        .child_default("c")
                        .child_default("f")
                        .build(),
                ),
                (
                    "c",
                    ComponentDeclBuilder::new()
                        .expose(ExposeDecl::EventStream(ExposeEventStreamDecl {
                            source: ExposeSource::Framework,
                            source_name: "started".parse().unwrap(),
    source_dictionary: Default::default(),
                            scope: Some(vec![EventScope::Child(ChildRef {
                                name: "e".to_string(),
                                collection: None,
                            })]),
                            target: ExposeTarget::Parent,
                            target_name: "started".parse().unwrap(),
                        }))
                        .child_default("d")
                        .child_default("e")
                        .build(),
                ),
                ("d", ComponentDeclBuilder::new().build()),
                ("e", ComponentDeclBuilder::new().build()),
                (
                    "f",
                    ComponentDeclBuilder::new()
                        .use_(UseBuilder::event_stream()
                            .name("started")
                            .path("/event/stream"))
                        .build()
                ),
            ];

            let mut builder = T::new("a", components);
            builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
                name: "started".parse().unwrap(),
            })]);

            let model = builder.build().await;
            model
                .check_use(
                    vec!["b", "f"].into(),
                    CheckUse::EventStream {
                        expected_res: ExpectedResult::Ok,
                        path: "/event/stream".parse().unwrap(),
                        scope: vec![
                            ComponentEventRoute {
                                component: "c".to_string(),
                                scope: Some(vec!["e".to_string()]),
                            },
                            ComponentEventRoute { component: "b".to_string(), scope: None },
                        ],
                        name: "started".parse().unwrap(),
                    },
                )
                .await;
        }*/

    /// Tests event stream aliasing (scoping rules are applied correctly)
    ///        root
    ///        |
    ///        a
    ///       /|\
    ///      b c d
    /// A offers started to b with scope b, A offers started to c with scope d,
    /// A offers started to d with scope c.
    pub async fn test_event_stream_aliasing(&self) {
        let components = vec![
            ("root", ComponentDeclBuilder::new().child_default("a").build()),
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "b".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![EventScope::Child(ChildRef {
                                name: "b".parse().unwrap(),
                                collection: None,
                            })]),
                    )
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "d".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![EventScope::Child(ChildRef {
                                name: "c".parse().unwrap(),
                                collection: None,
                            })]),
                    )
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "c".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![EventScope::Child(ChildRef {
                                name: "d".parse().unwrap(),
                                collection: None,
                            })]),
                    )
                    .child_default("b")
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "started".parse().unwrap(),
        })]);

        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["d".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;

        model
            .check_use(
                vec!["d"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses framework events "started", and "capability_requested"
    pub async fn test_use_event_stream_from_above_root(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                .build(),
        )];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "started".parse().unwrap(),
        })]);

        let model = builder.build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///   /\
    ///  b  c
    ///    / \
    ///   d   e
    /// c: uses framework events "started", and "capability_requested",
    /// scoped to b and c.
    /// d receives started which is scoped to b, c, and e.
    pub async fn test_use_event_stream_from_above_root_and_downscoped(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "b".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![
                                EventScope::Child(ChildRef {
                                    name: "b".parse().unwrap(),
                                    collection: None,
                                }),
                                EventScope::Child(ChildRef {
                                    name: "c".parse().unwrap(),
                                    collection: None,
                                }),
                            ]),
                    )
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "c".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![
                                EventScope::Child(ChildRef {
                                    name: "b".parse().unwrap(),
                                    collection: None,
                                }),
                                EventScope::Child(ChildRef {
                                    name: "c".parse().unwrap(),
                                    collection: None,
                                }),
                            ]),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .offer(
                        OfferBuilder::event_stream()
                            .name("started")
                            .source(OfferSource::Parent)
                            .target(OfferTarget::Child(ChildRef {
                                name: "d".parse().unwrap(),
                                collection: None,
                            }))
                            .scope(vec![EventScope::Child(ChildRef {
                                name: "e".parse().unwrap(),
                                collection: None,
                            })]),
                    )
                    .child_default("d")
                    .child_default("e")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("started").path("/event/stream"))
                    .build(),
            ),
            ("e", ComponentDeclBuilder::new().build()),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "started".parse().unwrap(),
        })]);

        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string(), "c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute {
                        component: "/".to_string(),
                        scope: Some(vec!["b".to_string(), "c".to_string()]),
                    }],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
        model
            .check_use(
                vec!["c", "d"].try_into().unwrap(), // Should get e's event from parent
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![
                        ComponentEventRoute {
                            component: "/".to_string(),
                            scope: Some(vec!["b".to_string(), "c".to_string()]),
                        },
                        ComponentEventRoute {
                            component: "c".to_string(),
                            scope: Some(vec!["e".to_string()]),
                        },
                    ],
                    name: "started".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a; attempts to offer event "capability_requested" to b.
    pub async fn test_can_offer_capability_requested_event(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::event_stream()
                            .name("capability_requested")
                            .target_name("capability_requested_on_a")
                            .source(OfferSource::Parent)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::event_stream().name("capability_requested_on_a"))
                    .build(),
            ),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
            name: "capability_requested".parse().unwrap(),
        })]);
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                    scope: vec![ComponentEventRoute { component: "/".to_string(), scope: None }],
                    name: "capability_requested_on_a".parse().unwrap(),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses service /svc/hippo as /svc/hippo.
    /// a: provides b with the service but policy prevents it.
    pub async fn test_use_protocol_denied_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol_default("hippo")
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
        ];
        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            HashSet::new(),
        );

        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///
    /// b: uses directory /data/foo as /data/bar.
    /// a: provides b with the directory but policy prevents it.
    pub async fn test_use_directory_with_alias_denied_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::directory().name("foo_data").path("/data/foo"))
                    .offer(
                        OfferBuilder::directory()
                            .name("foo_data")
                            .target_name("bar_data")
                            .source(OfferSource::Self_)
                            .target_static_child("b")
                            .rights(fio::R_STAR_DIR),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::directory().name("bar_data").path("/data/hippo"))
                    .build(),
            ),
        ];
        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "foo_data".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Directory,
            },
            HashSet::new(),
        );
        let model = builder.build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::default_directory(ExpectedResult::Err(zx::Status::ACCESS_DENIED)),
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    /// c: uses service /svc/hippo as /svc/hippo.
    /// b: uses service /svc/hippo as /svc/hippo.
    /// a: provides b with the service policy allows it.
    /// b: provides c with the service policy does not allow it.
    pub async fn test_use_protocol_partial_chain_allowed_by_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol_default("hippo")
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Parent)
                            .target_static_child("c"),
                    )
                    .use_(UseBuilder::protocol().name("hippo"))
                    .child_default("c")
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").build());

        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///    \
    ///     b
    ///    /  \
    ///   c    d
    /// b: provides d with the service policy allows denies it.
    /// b: provides c with the service policy allows it.
    /// c: uses service /svc/hippo as /svc/hippo.
    /// Tests component provided caps in the middle of a path
    pub async fn test_use_protocol_component_provided_capability_policy(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .protocol_default("hippo")
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Parent)
                            .target_static_child("c"),
                    )
                    .offer(
                        OfferBuilder::protocol()
                            .name("hippo")
                            .source(OfferSource::Parent)
                            .target_static_child("d"),
                    )
                    .child_default("c")
                    .child_default("d")
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
            ("d", ComponentDeclBuilder::new().use_(UseBuilder::protocol().name("hippo")).build()),
        ];

        let mut allowlist = HashSet::new();
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").build());
        allowlist.insert(AllowlistEntryBuilder::new().exact("b").exact("c").build());

        let mut builder = T::new("a", components);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentInstance(Moniker::root()),
                source_name: "hippo".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            allowlist,
        );
        let model = builder.build().await;

        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;

        model
            .check_use(
                vec!["b", "d"].try_into().unwrap(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///  component manager's namespace
    ///   |
    ///   a
    ///
    /// a: uses service /use_from_cm_namespace/svc/foo as foo
    pub async fn test_use_from_component_manager_namespace_denied_by_policy(&self) {
        let components = vec![(
            "a",
            ComponentDeclBuilder::new()
                .use_(UseBuilder::protocol().name("foo").path("/svc/hippo"))
                .build(),
        )];
        let namespace_capabilities = vec![CapabilityBuilder::protocol()
            .name("foo")
            .path("/use_from_cm_namespace/svc/foo")
            .build()];
        let mut builder = T::new("a", components);
        builder.set_namespace_capabilities(namespace_capabilities);
        builder.add_capability_policy(
            CapabilityAllowlistKey {
                source_moniker: ExtendedMoniker::ComponentManager,
                source_name: "foo".parse().unwrap(),
                source: CapabilityAllowlistSource::Self_,
                capability: CapabilityTypeName::Protocol,
            },
            HashSet::new(),
        );
        let model = builder.build().await;

        model.install_namespace_directory("/use_from_cm_namespace");
        model
            .check_use(
                Moniker::root(),
                CheckUse::Protocol {
                    path: default_service_capability(),
                    expected_res: ExpectedResult::Err(zx::Status::ACCESS_DENIED),
                },
            )
            .await;
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: offer to b from self
    /// b: use from parent
    pub async fn test_route_service_from_parent(&self) {
        let use_decl = UseBuilder::service().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .service_default("foo")
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  /
    /// b
    ///
    /// a: use from #b
    /// b: expose to parent from self
    pub async fn test_route_service_from_child(&self) {
        let use_decl = UseBuilder::service().name("foo").source_static_child("b").build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().use_(use_decl.clone()).child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .service_default("foo")
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("root instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &b_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_service_from_sibling(&self) {
        let use_decl = UseBuilder::service().name("foo").build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("b"),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .service_default("foo")
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Service(ServiceDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(
                    source_path.expect("missing source path"),
                    "/svc/foo".parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &c_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b with service instance filter set from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_filtered_service_from_sibling(&self) {
        let use_decl = UseBuilder::service().name("foo").build();
        let source_instance_filter = vec!["service_instance_0".to_string()];
        let renamed_instances = vec![];
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("b")
                            .source_instance_filter(source_instance_filter)
                            .renamed_instances(renamed_instances),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .service_default("foo")
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::FilteredAggregate {
                        capability: AggregateCapability::Service(name),
                        component,
                        capability_provider,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(component.moniker, "c".parse().unwrap());
                let mut data = capability_provider.route_instances();
                assert_eq!(data.len(), 1);
                let data = data.remove(0).await.unwrap();
                assert_matches!(
                    data,
                    FilteredAggregateCapabilityRouteData {
                        capability_source: CapabilitySource::Component {
                            component,
                            capability,
                        },
                        instance_filter,
                    }
                    if component.moniker == "c".parse().unwrap() &&
                        capability == ComponentCapability::Service(ServiceDecl {
                            name: "foo".parse().unwrap(),
                            source_path: Some("/svc/foo".parse().unwrap()),
                        }) &&
                        instance_filter == vec![
                            NameMapping {
                                source_name: "service_instance_0".parse().unwrap(),
                                target_name: "service_instance_0".parse().unwrap(),
                            }
                        ]
                );
           }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: offer to b with a service instance renamed from child c
    /// b: use from parent
    /// c: expose from self
    pub async fn test_route_renamed_service_instance_from_sibling(&self) {
        let use_decl = UseBuilder::service().name("foo").build();
        let source_instance_filter = vec![];
        let renamed_instances = vec![NameMapping {
            source_name: "instance_0".to_string(),
            target_name: "renamed_instance_0".to_string(),
        }];

        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::service()
                            .name("foo")
                            .source_static_child("c")
                            .target_static_child("b")
                            .source_instance_filter(source_instance_filter)
                            .renamed_instances(renamed_instances),
                    )
                    .child_default("b")
                    .child_default("c")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(use_decl.clone()).build()),
            (
                "c",
                ComponentDeclBuilder::new()
                    .expose(ExposeBuilder::service().name("foo").source(ExposeSource::Self_))
                    .service_default("foo")
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let UseDecl::Service(use_decl) = use_decl else { unreachable!() };
        let source = route_capability(
            RouteRequest::UseService(use_decl),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route service");

        // Verify this source comes from `c`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::FilteredAggregate {
                        capability: AggregateCapability::Service(name),
                        component,
                        capability_provider,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "foo");
                assert_eq!(component.moniker, "c".parse().unwrap());
                let mut data = capability_provider.route_instances();
                assert_eq!(data.len(), 1);
                let data = data.remove(0).await.unwrap();
                assert_matches!(
                    data,
                    FilteredAggregateCapabilityRouteData {
                        capability_source: CapabilitySource::Component {
                            component,
                            capability,
                        },
                        instance_filter,
                    }
                    if component.moniker == "c".parse().unwrap() &&
                        capability == ComponentCapability::Service(ServiceDecl {
                            name: "foo".parse().unwrap(),
                            source_path: Some("/svc/foo".parse().unwrap()),
                        }) &&
                        instance_filter == vec![
                            NameMapping {
                                source_name: "instance_0".parse().unwrap(),
                                target_name: "renamed_instance_0".parse().unwrap(),
                            }
                        ]
                );
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit".
    pub async fn test_route_runner_from_parent_environment(&self) {
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
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: declares runner "elf" at path format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self.
    /// a: offers runner "elf" from self to "b" as "dwarf".
    /// b: registers runner "dwarf" from realm in environment as "hobbit".
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_grandparent_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child_default("b")
                    .offer(
                        OfferBuilder::runner()
                            .name("elf")
                            .target_name("dwarf")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
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
            ("c", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let c_component =
            model.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///  / \
    /// b   c
    ///
    /// a: registers runner "dwarf" from "b" in environment as "hobbit".
    /// b: exposes runner "elf" at path format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self as "dwarf".
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_sibling_environment(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child_default("b")
                    .child(ChildBuilder::new().name("c").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "dwarf".parse().unwrap(),
                        source: RegistrationSource::Child("b".parse().unwrap()),
                        target_name: "hobbit".parse().unwrap(),
                    }))
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::runner()
                            .name("elf")
                            .target_name("dwarf")
                            .source(ExposeSource::Self_),
                    )
                    .runner_default("elf")
                    .build(),
            ),
            ("c", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let c_component =
            model.look_up_instance(&vec!["c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `b`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &b_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///    \
    ///     b
    ///      \
    ///       c
    ///
    /// a: declares runner "elf" at path format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self.
    /// a: registers runner "elf" from realm in environment as "hobbit".
    /// b: creates environment extending from realm.
    /// c: uses runner "hobbit".
    pub async fn test_route_runner_from_inherited_environment(&self) {
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
            ("c", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let c_component =
            model.look_up_instance(&vec!["b", "c"].try_into().unwrap()).await.expect("c instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &c_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit". Fails because "hobbit" was not in environment.
    pub async fn test_route_runner_from_environment_not_found(&self) {
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
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let model = T::new("a", components).build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::UseFromEnvironmentNotFound {
                    moniker,
                    capability_type,
                    capability_name,
                }
            )
                if moniker == *b_component.moniker() &&
                capability_type == "runner" &&
                capability_name == "hobbit"
        );
    }

    ///   a
    ///    \
    ///     b
    ///
    /// a: registers built-in runner "elf" from realm in environment as "hobbit".
    /// b: uses runner "hobbit".
    pub async fn test_route_builtin_runner(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "elf".parse().unwrap(),
                        source: RegistrationSource::Parent,
                        target_name: "hobbit".parse().unwrap(),
                    }))
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "elf".parse().unwrap(),
            source_path: None,
        })]);
        builder.register_mock_builtin_runner("elf");
        let model = builder.build().await;

        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this is a built-in source.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Builtin {
                        capability: InternalCapability::Runner(name),
                        ..
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///   a
    ///
    /// a: uses built-in runner "elf" from the root environment.
    pub async fn test_route_builtin_runner_from_root_env(&self) {
        let components = vec![("a", ComponentDeclBuilder::new().build())];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "elf".parse().unwrap(),
            source_path: None,
        })]);
        builder.register_mock_builtin_runner("elf");
        let model = builder.build().await;

        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "elf".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this is a built-in source.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Builtin {
                        capability: InternalCapability::Runner(name),
                        ..
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: registers built-in runner "elf" from realm in environment as "hobbit". The ELF runner is
    ///    registered in the root environment, but not declared as a built-in capability.
    /// b: uses runner "hobbit"; should fail.
    pub async fn test_route_builtin_runner_not_found(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .child(ChildBuilder::new().name("b").environment("env"))
                    .environment(EnvironmentBuilder::new().name("env").runner(RunnerRegistration {
                        source_name: "elf".parse().unwrap(),
                        source: RegistrationSource::Parent,
                        target_name: "hobbit".parse().unwrap(),
                    }))
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new_empty_component().program_runner("hobbit").build()),
        ];

        let mut builder = T::new("a", components);
        builder.register_mock_builtin_runner("elf");

        let model = builder.build().await;
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let route_result = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::RegisterFromComponentManagerNotFound {
                    capability_id,
                }
            )
                if capability_id == "elf".to_string()
        );
    }

    ///   a
    ///
    /// a: Attempts to use unregistered runner "hobbit" from the root environment.
    ///    The runner is provided as a built-in capability, but not registered in
    ///    the root environment.
    pub async fn test_route_builtin_runner_from_root_env_registration_not_found(&self) {
        let components = vec![("a", ComponentDeclBuilder::new().build())];

        let mut builder = T::new("a", components);
        builder.set_builtin_capabilities(vec![CapabilityDecl::Runner(RunnerDecl {
            name: "hobbit".parse().unwrap(),
            source_path: None,
        })]);
        let model = builder.build().await;

        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let route_result = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await;

        assert_matches!(
            route_result,
            Err(RoutingError::UseFromEnvironmentNotFound {
                    moniker,
                    capability_type,
                    capability_name,
                }
            )
                if moniker == *a_component.moniker()
                && capability_type == "runner".to_string()
                && capability_name == "hobbit"
        );
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: uses runner "elf" from "#b" as "dwarf".
    /// b: exposes runner "elf" at path format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self as "dwarf".
    pub async fn test_use_runner_from_child(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::runner().source_static_child("b").name("dwarf"))
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .expose(
                        ExposeBuilder::runner()
                            .name("elf")
                            .target_name("dwarf")
                            .source(ExposeSource::Self_),
                    )
                    .runner_default("elf")
                    .build(),
            ),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Child("b".parse().unwrap()),
                source_name: "dwarf".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &a_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `b`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &b_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: offers runner "elf" at path format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from self as "dwarf".
    /// b: uses runner "elf" from "parent" as "dwarf".
    pub async fn test_use_runner_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::runner()
                            .name("elf")
                            .target_name("dwarf")
                            .source(OfferSource::Self_)
                            .target_static_child("b"),
                    )
                    .runner_default("elf")
                    .child_default("b")
                    .build(),
            ),
            ("b", ComponentDeclBuilder::new().use_(UseBuilder::runner().name("dwarf")).build()),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Parent,
                source_name: "dwarf".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares runner "elf" with service format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME) from "self".
    /// a: registers runner "elf" from self in environment as "hobbit".
    /// b: uses runner "hobbit" from environment.
    pub async fn test_use_runner_from_parent_environment(&self) {
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
                    .use_(UseBuilder::runner().source(UseSource::Environment).name("hobbit"))
                    .build(),
            ),
        ];

        let model = T::new("a", components).build().await;
        let a_component = model.look_up_instance(&Moniker::root()).await.expect("a instance");
        let b_component =
            model.look_up_instance(&vec!["b"].try_into().unwrap()).await.expect("b instance");
        let source = route_capability(
            RouteRequest::UseRunner(UseRunnerDecl {
                source: UseSource::Environment,
                source_name: "hobbit".parse().unwrap(),
                source_dictionary: Default::default(),
            }),
            &b_component,
            &mut NoopRouteMapper,
        )
        .await
        .expect("failed to route runner");

        // Verify this source comes from `a`.
        match source {
            RouteSource {
                source:
                    CapabilitySource::<
                        <<T as RoutingTestModelBuilder>::Model as RoutingTestModel>::C,
                    >::Component {
                        capability: ComponentCapability::Runner(RunnerDecl { name, source_path }),
                        component,
                    },
                relative_path,
            } if relative_path.is_dot() => {
                assert_eq!(name, "elf");
                assert_eq!(
                    source_path.expect("missing source path"),
                    format!("/svc/{}", fcrunner::ComponentRunnerMarker::DEBUG_NAME).parse::<cm_types::Path>().unwrap()
                );
                assert!(Arc::ptr_eq(&component.upgrade().unwrap(), &a_component));
            }
            _ => panic!("bad capability source"),
        };
    }

    ///  a
    ///   \
    ///    b
    ///
    /// b: declares "fuchsia.MyConfig" capability
    /// b: uses "fuchsia.MyConfig" from self
    pub async fn test_use_config_from_self(&self) {
        let good_value = cm_rust::ConfigSingleValue::Int8(12);
        let use_config = UseBuilder::config()
            .source(cm_rust::UseSource::Self_)
            .name("fuchsia.MyConfig")
            .target_name("my_config")
            .config_type(cm_rust::ConfigValueType::Int8)
            .build();
        let components = vec![
            ("a", ComponentDeclBuilder::new().child_default("b").build()),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::config()
                            .name("fuchsia.MyConfig")
                            .value(good_value.clone().into()),
                    )
                    .use_(use_config.clone())
                    .config(cm_rust::ConfigDecl {
                        fields: vec![cm_rust::ConfigField {
                            key: "my_config".into(),
                            type_: cm_rust::ConfigValueType::Int8,
                            mutability: Default::default(),
                        }],
                        checksum: cm_rust::ConfigChecksum::Sha256([0; 32]),
                        value_source: cm_rust::ConfigValueSource::Capabilities(Default::default()),
                    })
                    .build(),
            ),
        ];

        let model = T::new("a", components).build().await;
        let child_component = model.look_up_instance(&vec!["b"].try_into().unwrap()).await.unwrap();

        let cm_rust::UseDecl::Config(use_config) = use_config else { panic!() };
        let value =
            routing::config::route_config_value(&use_config, &child_component).await.unwrap();
        assert_eq!(value, Some(cm_rust::ConfigValue::Single(good_value)));
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: declares "fuchsia.MyConfig" capability
    /// b: uses "fuchsia.MyConfig" from parent
    pub async fn test_use_config_from_parent(&self) {
        let good_value = cm_rust::ConfigSingleValue::Int8(12);
        let use_config = UseBuilder::config()
            .source(cm_rust::UseSource::Parent)
            .name("fuchsia.MyConfig")
            .target_name("my_config")
            .config_type(cm_rust::ConfigValueType::Int8)
            .build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::config()
                            .name("fuchsia.MyConfig")
                            .value(good_value.clone().into()),
                    )
                    .offer(
                        OfferBuilder::config()
                            .name("fuchsia.MyConfig")
                            .source(cm_rust::OfferSource::Self_)
                            .target(cm_rust::OfferTarget::Child(cm_rust::ChildRef {
                                name: "b".parse().unwrap(),
                                collection: None,
                            })),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(use_config.clone())
                    .config(cm_rust::ConfigDecl {
                        fields: vec![cm_rust::ConfigField {
                            key: "my_config".into(),
                            type_: cm_rust::ConfigValueType::Int8,
                            mutability: Default::default(),
                        }],
                        checksum: cm_rust::ConfigChecksum::Sha256([0; 32]),
                        value_source: cm_rust::ConfigValueSource::Capabilities(Default::default()),
                    })
                    .build(),
            ),
        ];

        let model = T::new("a", components).build().await;
        let child_component = model.look_up_instance(&vec!["b"].try_into().unwrap()).await.unwrap();

        let cm_rust::UseDecl::Config(use_config) = use_config else { panic!() };
        let value =
            routing::config::route_config_value(&use_config, &child_component).await.unwrap();
        assert_eq!(value, Some(cm_rust::ConfigValue::Single(good_value)));
    }

    ///  a
    ///   \
    ///    b
    ///
    /// a: routes "fuchsia.MyConfig" from void
    /// b: uses "fuchsia.MyConfig" from parent
    pub async fn test_use_config_from_void(&self) {
        let use_config = UseBuilder::config()
            .source(cm_rust::UseSource::Parent)
            .name("fuchsia.MyConfig")
            .target_name("my_config")
            .availability(cm_rust::Availability::Optional)
            .config_type(cm_rust::ConfigValueType::Int8)
            .build();
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(
                        OfferBuilder::config()
                            .name("fuchsia.MyConfig")
                            .source(cm_rust::OfferSource::Void)
                            .availability(cm_rust::Availability::Optional)
                            .target(cm_rust::OfferTarget::Child(cm_rust::ChildRef {
                                name: "b".parse().unwrap(),
                                collection: None,
                            })),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(use_config.clone())
                    .config(cm_rust::ConfigDecl {
                        fields: vec![cm_rust::ConfigField {
                            key: "my_config".into(),
                            type_: cm_rust::ConfigValueType::Int8,
                            mutability: Default::default(),
                        }],
                        checksum: cm_rust::ConfigChecksum::Sha256([0; 32]),
                        value_source: cm_rust::ConfigValueSource::Capabilities(Default::default()),
                    })
                    .build(),
            ),
        ];

        let model = T::new("a", components).build().await;
        let child_component = model.look_up_instance(&vec!["b"].try_into().unwrap()).await.unwrap();

        let cm_rust::UseDecl::Config(use_config) = use_config else { panic!() };
        let value =
            routing::config::route_config_value(&use_config, &child_component).await.unwrap();
        assert_eq!(value, None);
    }
}
