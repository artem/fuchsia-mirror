// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{CheckUse, ExpectedResult, RoutingTestModel, RoutingTestModelBuilder},
    cm_moniker::InstancedMoniker,
    cm_rust::*,
    cm_rust_testing::*,
    fidl_fuchsia_io as fio,
    moniker::{Moniker, MonikerBase},
    std::{
        convert::{TryFrom, TryInto},
        marker::PhantomData,
        path::PathBuf,
    },
};

pub struct CommonStorageAdminTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}

impl<T: RoutingTestModelBuilder> CommonStorageAdminTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    ///    a
    ///   / \
    ///  b   c
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: offers data storage to b
    /// a: offers a storage admin protocol to c from the "data" storage capability
    /// b: uses data storage as /storage.
    /// c: uses the storage admin protocol to access b's storage
    pub async fn test_storage_to_one_child_admin_to_another(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Capability("data".parse().unwrap()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
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
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["c"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///    a
    ///    |
    ///    b
    ///    |
    ///    c
    ///
    /// a: has directory decl with name "data" with a source of self at path /data subdir "foo"
    /// a: offers data to b
    /// b: has storage decl with name "storage" based on "data" from parent subdir "bar"
    /// b: offers a storage admin protocol to c from the "storage" storage capability
    /// c: uses the storage admin protocol to access its own storage
    pub async fn test_directory_from_grandparent_storage_and_admin_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("data")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .offer(OfferDecl::Directory(OfferDirectoryDecl {
                        source: OfferSource::Self_,
                        source_name: "data".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "data".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
                        rights: Some(fio::RW_STAR_DIR),
                        subdir: Some(PathBuf::from("foo")),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::storage()
                            .name("storage")
                            .backing_dir("data")
                            .source(StorageDirectorySource::Parent)
                            .subdir("bar"),
                    )
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Capability("storage".parse().unwrap()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: OfferTarget::static_child("c".to_string()),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    }))
                    .child_default("c")
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b", "c"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["c:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: Some("foo/bar".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///    a
    ///   / \
    ///  b   c
    ///      |
    ///      d
    ///
    /// c: has storage decl with name "data" with a source of self at path /data
    /// c: has storage admin protocol from the "data" storage admin capability
    /// c: offers data storage to d
    /// d: uses data storage
    /// a: offers storage admin protocol from c to b
    /// b: uses the storage admin protocol
    pub async fn test_storage_admin_from_sibling(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("c".to_string()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
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
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("d".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Capability("data".parse().unwrap()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .child_default("d")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let test = T::new("a", components).build().await;
        test.check_use(
            vec!["b"].try_into().unwrap(),
            CheckUse::StorageAdmin {
                storage_relation: InstancedMoniker::try_from(vec!["d:0"]).unwrap(),
                from_cm_namespace: false,
                storage_subdir: None,
                expected_res: ExpectedResult::Ok,
            },
        )
        .await;
    }

    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: offers data storage to b
    /// a: uses a storage admin protocol from #data
    /// b: uses data storage as /storage.
    pub async fn test_admin_protocol_used_in_the_same_place_storage_is_declared(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Capability("data".parse().unwrap()))
                            .name("fuchsia.sys2.StorageAdmin"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Ok,
                },
            )
            .await;
    }

    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: declares a protocol "unrelated.protocol"
    /// a: offers data storage to b
    /// a: uses a storage admin protocol from "unrelated.protocol"
    /// b: uses data storage as /storage.
    pub async fn test_storage_admin_from_protocol_on_self(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::protocol().name("unrelated.protocol").path("/svc/foo"),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Capability("unrelated.protocol".parse().unwrap()))
                            .name("fuchsia.sys2.StorageAdmin"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }

    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: declares a protocol "unrelated.protocol"
    /// a: offers a storage admin protocol from "unrelated.protocol" to b
    /// b: uses storage admin protocol
    pub async fn test_storage_admin_from_protocol_from_parent(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::protocol().name("unrelated.protocol").path("/svc/foo"),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Capability("unrelated.protocol".parse().unwrap()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
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
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }

    ///    a
    ///   / \
    ///  b   c
    ///      |
    ///      d
    ///
    /// c: has storage decl with name "data" with a source of self at path /data
    /// c: has protocol decl with name "unrelated.protocol"
    /// c: has storage admin protocol from the "unrelated.protocol" capability
    /// c: offers data storage to d
    /// d: uses data storage
    /// a: offers storage admin protocol from c to b
    /// b: uses the storage admin protocol
    pub async fn test_storage_admin_from_protocol_on_sibling(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("c".to_string()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
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
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .capability(
                        CapabilityBuilder::protocol().name("unrelated.protocol").path("/svc/foo"),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("d".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Capability("unrelated.protocol".parse().unwrap()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .child_default("d")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["d:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }

    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: offers data storage to b
    /// a: uses a "unrelated.protocol" protocol from "data"
    /// b: uses data storage as /storage.
    pub async fn test_storage_admin_from_storage_on_self_bad_protocol_name(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::protocol().name("unrelated.protocol").path("/svc/foo"),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("b".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .use_(
                        UseBuilder::protocol()
                            .source(UseSource::Capability("unrelated.protocol".parse().unwrap()))
                            .name("unrelated.protocol")
                            .path("/svc/fuchsia.sys2.StorageAdmin"),
                    )
                    .child_default("b")
                    .build(),
            ),
            (
                "b",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                Moniker::root(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }

    ///    a
    ///    |
    ///    b
    ///
    /// a: has storage decl with name "data" with a source of self at path /data
    /// a: offers a storage admin protocol from "data" to b with a source name of "unrelated.protocol"
    /// b: uses storage admin protocol
    pub async fn test_storage_admin_from_storage_on_parent_bad_protocol_name(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Capability("data".parse().unwrap()),
                        source_name: "unrelated.protocol".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
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
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["b:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }

    ///    a
    ///   / \
    ///  b   c
    ///      |
    ///      d
    ///
    /// c: has storage decl with name "data" with a source of self at path /data
    /// c: exposes storage admin protocol from "data" with a source name of "unrelated.protocol"
    /// c: offers data storage to d
    /// d: uses data storage
    /// a: offers storage admin protocol from c to b
    /// b: uses the storage admin protocol
    pub async fn test_storage_admin_from_protocol_on_sibling_bad_protocol_name(&self) {
        let components = vec![
            (
                "a",
                ComponentDeclBuilder::new()
                    .offer(OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::static_child("c".to_string()),
                        source_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: OfferTarget::static_child("b".to_string()),
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
                    .use_(UseBuilder::protocol().name("fuchsia.sys2.StorageAdmin"))
                    .build(),
            ),
            (
                "c",
                ComponentDeclBuilder::new()
                    .capability(
                        CapabilityBuilder::directory()
                            .name("tmpfs")
                            .path("/data")
                            .rights(fio::RW_STAR_DIR),
                    )
                    .capability(
                        CapabilityBuilder::storage()
                            .name("data")
                            .backing_dir("tmpfs")
                            .source(StorageDirectorySource::Self_),
                    )
                    .offer(OfferDecl::Storage(OfferStorageDecl {
                        source: OfferSource::Self_,
                        target: OfferTarget::static_child("d".to_string()),
                        source_name: "data".parse().unwrap(),
                        target_name: "data".parse().unwrap(),
                        availability: Availability::Required,
                    }))
                    .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Capability("data".parse().unwrap()),
                        source_name: "unrelated.protocol".parse().unwrap(),
                        source_dictionary: None,
                        target_name: "fuchsia.sys2.StorageAdmin".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        availability: cm_rust::Availability::Required,
                    }))
                    .child_default("d")
                    .build(),
            ),
            (
                "d",
                ComponentDeclBuilder::new()
                    .use_(UseBuilder::storage().name("data").path("/storage"))
                    .build(),
            ),
        ];
        let model = T::new("a", components).build().await;
        model
            .check_use(
                vec!["b"].try_into().unwrap(),
                CheckUse::StorageAdmin {
                    storage_relation: InstancedMoniker::try_from(vec!["d:0"]).unwrap(),
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::ErrWithNoEpitaph,
                },
            )
            .await;
    }
}
