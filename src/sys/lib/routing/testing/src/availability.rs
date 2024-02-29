// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        CheckUse, ComponentEventRoute, ExpectedResult, RoutingTestModel, RoutingTestModelBuilder,
        ServiceInstance,
    },
    cm_moniker::InstancedMoniker,
    cm_rust::*,
    cm_rust_testing::*,
    fidl_fuchsia_io as fio, fuchsia_zircon_status as zx_status,
    moniker::{Moniker, MonikerBase},
    std::{
        marker::PhantomData,
        path::{Path, PathBuf},
    },
};

pub struct CommonAvailabilityTest<T: RoutingTestModelBuilder> {
    builder: PhantomData<T>,
}

#[derive(Debug)]
struct TestCase {
    /// The availability of either an `Offer` or `Expose` declaration.
    provider_availability: Availability,
    use_availability: Availability,
}

impl<T: RoutingTestModelBuilder> CommonAvailabilityTest<T> {
    pub fn new() -> Self {
        Self { builder: PhantomData }
    }

    const VALID_AVAILABILITY_PAIRS: &'static [TestCase] = &[
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Required,
        },
        TestCase {
            provider_availability: Availability::Optional,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Required,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Optional,
        },
        TestCase {
            provider_availability: Availability::Required,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::Optional,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::Transitional,
            use_availability: Availability::Transitional,
        },
        TestCase {
            provider_availability: Availability::SameAsTarget,
            use_availability: Availability::Transitional,
        },
    ];

    pub async fn test_offer_availability_successful_routes(&self) {
        for test_case in Self::VALID_AVAILABILITY_PAIRS {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .offer(OfferDecl::Service(OfferServiceDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            source_instance_filter: None,
                            renamed_instances: None,
                            availability: test_case.provider_availability,
                        }))
                        .offer(OfferDecl::Protocol(OfferProtocolDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.provider_availability,
                        }))
                        .offer(OfferDecl::Directory(OfferDirectoryDecl {
                            source: OfferSource::static_child("b".to_string()),
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target: OfferTarget::static_child("c".to_string()),
                            target_name: "dir".parse().unwrap(),
                            rights: Some(fio::R_STAR_DIR),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.provider_availability,
                        }))
                        .capability(
                            CapabilityBuilder::directory()
                                .name("data")
                                .path("/data")
                                .rights(fio::RW_STAR_DIR),
                        )
                        .capability(
                            CapabilityBuilder::storage()
                                .name("cache")
                                .backing_dir("data")
                                .source(StorageDirectorySource::Self_)
                                .subdir("cache"),
                        )
                        .offer(OfferDecl::Storage(OfferStorageDecl {
                            source: OfferSource::Self_,
                            target: OfferTarget::static_child("c".to_string()),
                            source_name: "cache".parse().unwrap(),
                            target_name: "cache".parse().unwrap(),
                            availability: test_case.provider_availability,
                        }))
                        .offer(OfferDecl::EventStream(OfferEventStreamDecl {
                            source: OfferSource::Parent,
                            source_name: "started".parse().unwrap(),
                            scope: None,
                            target: OfferTarget::Child(ChildRef {
                                name: "c".into(),
                                collection: None,
                            }),
                            target_name: "started".parse().unwrap(),
                            availability: test_case.provider_availability,
                        }))
                        .child_default("b")
                        .child_default("c")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .capability(
                            CapabilityBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/foo.service"),
                        )
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .capability(
                            CapabilityBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .path("/svc/foo"),
                        )
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .capability(CapabilityBuilder::directory().name("dir").path("/data/dir"))
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: cm_rust::Availability::Required,
                        }))
                        .build(),
                ),
                (
                    "c",
                    ComponentDeclBuilder::new()
                        .use_(
                            UseBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::directory()
                                .name("dir")
                                .path("/dir")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::storage()
                                .name("cache")
                                .path("/storage")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::event_stream()
                                .name("started")
                                .path("/event/stream")
                                .availability(test_case.use_availability),
                        )
                        .build(),
                ),
            ];
            let mut builder = T::new("a", components);
            builder.set_builtin_capabilities(vec![CapabilityDecl::EventStream(EventStreamDecl {
                name: "started".parse().unwrap(),
            })]);
            let model = builder.build().await;
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Storage {
                    path: "/storage".parse().unwrap(),
                    storage_relation: Some(InstancedMoniker::try_from(vec!["c:0"]).unwrap()),
                    from_cm_namespace: false,
                    storage_subdir: Some("cache".to_string()),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::EventStream {
                    expected_res: ExpectedResult::Ok,
                    path: "/event/stream".parse().unwrap(),
                    scope: vec![ComponentEventRoute { component: "/".to_string(), scope: None }],
                    name: "started".parse().unwrap(),
                },
            ] {
                model.check_use(vec!["c"].try_into().unwrap(), check_use).await;
            }
        }
    }

    pub async fn test_offer_availability_invalid_routes(&self) {
        struct TestCase {
            source: OfferSource,
            storage_source: Option<OfferSource>,
            offer_availability: Availability,
            use_availability: Availability,
        }
        for test_case in &[
            TestCase {
                source: OfferSource::static_child("b".to_string()),
                storage_source: Some(OfferSource::Self_),
                offer_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Optional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Transitional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: OfferSource::Void,
                storage_source: None,
                offer_availability: Availability::Transitional,
                use_availability: Availability::Required,
            },
        ] {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .offer(OfferDecl::Service(OfferServiceDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            source_instance_filter: None,
                            renamed_instances: None,
                            availability: test_case.offer_availability,
                        }))
                        .offer(OfferDecl::Protocol(OfferProtocolDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            dependency_type: DependencyType::Strong,
                            availability: test_case.offer_availability,
                        }))
                        .offer(OfferDecl::Directory(OfferDirectoryDecl {
                            source: test_case.source.clone(),
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target: OfferTarget::static_child("c".to_string()),
                            target_name: "dir".parse().unwrap(),
                            rights: Some(fio::Operations::CONNECT),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: test_case.offer_availability,
                        }))
                        .offer(OfferDecl::Storage(OfferStorageDecl {
                            source: test_case
                                .storage_source
                                .as_ref()
                                .map(Clone::clone)
                                .unwrap_or(test_case.source.clone()),
                            source_name: "data".parse().unwrap(),
                            target_name: "data".parse().unwrap(),
                            target: OfferTarget::static_child("c".to_string()),
                            availability: test_case.offer_availability,
                        }))
                        .capability(
                            CapabilityBuilder::storage()
                                .name("data")
                                .backing_dir("dir")
                                .source(StorageDirectorySource::Child("b".into())),
                        )
                        .child_default("b")
                        .child_default("c")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .capability(
                            CapabilityBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/foo.service"),
                        )
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .capability(
                            CapabilityBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .path("/svc/foo"),
                        )
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: cm_rust::Availability::Required,
                        }))
                        .capability(
                            CapabilityBuilder::directory()
                                .name("dir")
                                .path("/dir")
                                .rights(fio::Operations::CONNECT),
                        )
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: cm_rust::Availability::Required,
                        }))
                        .build(),
                ),
                (
                    "c",
                    ComponentDeclBuilder::new()
                        .use_(
                            UseBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::directory()
                                .name("dir")
                                .path("/dir")
                                .rights(fio::Operations::CONNECT)
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::storage()
                                .name("data")
                                .path("/data")
                                .availability(test_case.use_availability),
                        )
                        .build(),
                ),
            ];
            let model = T::new("a", components).build().await;
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
                CheckUse::Storage {
                    path: "/data".parse().unwrap(),
                    storage_relation: None,
                    from_cm_namespace: false,
                    storage_subdir: None,
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
            ] {
                model.check_use(vec!["c"].try_into().unwrap(), check_use).await;
            }
        }
    }

    /// Creates the following topology:
    ///
    ///           a
    ///          /
    ///         /
    ///        b
    ///
    /// And verifies exposing a variety of capabilities from `b`, testing the combination of
    /// availability settings and capability types.
    ///
    /// Storage and event stream capabilities cannot be exposed, hence omitted.
    pub async fn test_expose_availability_successful_routes(&self) {
        for test_case in Self::VALID_AVAILABILITY_PAIRS {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .use_(
                            UseBuilder::service()
                                .source(UseSource::Child("b".to_owned()))
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/fuchsia.examples.EchoService_a")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::protocol()
                                .source(UseSource::Child("b".to_owned()))
                                .name("fuchsia.examples.Echo")
                                .path("/svc/fuchsia.examples.Echo_a")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::directory()
                                .source(UseSource::Child("b".to_owned()))
                                .name("dir")
                                .path("/dir_a")
                                .availability(test_case.use_availability),
                        )
                        .child_default("b")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .capability(
                            CapabilityBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/foo.service"),
                        )
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.provider_availability,
                        }))
                        .capability(
                            CapabilityBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .path("/svc/foo"),
                        )
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.provider_availability,
                        }))
                        .capability(CapabilityBuilder::directory().name("dir").path("/data/dir"))
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Self_,
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: test_case.provider_availability,
                        }))
                        .build(),
                ),
            ];
            let builder = T::new("a", components);
            let model = builder.build().await;

            // Add a file to the directory capability in the component that declared it, so "b".
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");

            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService_a".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir_a".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            ] {
                model.check_use(Moniker::root(), check_use).await;
            }

            for check_use in vec![
                CheckUse::Service {
                    path: "/fuchsia.examples.EchoService".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Protocol {
                    path: "/fuchsia.examples.Echo".parse().unwrap(),
                    expected_res: ExpectedResult::Ok,
                },
                CheckUse::Directory {
                    path: "/dir".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Ok,
                },
            ] {
                model.check_use_exposed_dir(vec!["b"].try_into().unwrap(), check_use).await;
            }
        }
    }

    /// Creates the following topology:
    ///
    ///           a
    ///          /
    ///         /
    ///        b
    ///
    /// And verifies exposing a variety of capabilities from `b`. Except that either the route is
    /// broken, or the rules around availability are broken.
    pub async fn test_expose_availability_invalid_routes(&self) {
        struct TestCase {
            source: ExposeSource,
            expose_availability: Availability,
            use_availability: Availability,
        }
        for test_case in &[
            TestCase {
                source: ExposeSource::Self_,
                expose_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Optional,
                use_availability: Availability::Required,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Optional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Transitional,
                use_availability: Availability::Optional,
            },
            TestCase {
                source: ExposeSource::Void,
                expose_availability: Availability::Transitional,
                use_availability: Availability::Required,
            },
        ] {
            let components = vec![
                (
                    "a",
                    ComponentDeclBuilder::new()
                        .use_(
                            UseBuilder::service()
                                .source(UseSource::Child("b".to_owned()))
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/fuchsia.examples.EchoService_a")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::protocol()
                                .source(UseSource::Child("b".to_owned()))
                                .name("fuchsia.examples.Echo")
                                .path("/svc/fuchsia.examples.Echo_a")
                                .availability(test_case.use_availability),
                        )
                        .use_(
                            UseBuilder::directory()
                                .source(UseSource::Child("b".to_owned()))
                                .name("dir")
                                .path("/dir_a")
                                .availability(test_case.use_availability),
                        )
                        .child_default("b")
                        .build(),
                ),
                (
                    "b",
                    ComponentDeclBuilder::new()
                        .capability(
                            CapabilityBuilder::service()
                                .name("fuchsia.examples.EchoService")
                                .path("/svc/foo.service"),
                        )
                        .expose(ExposeDecl::Service(ExposeServiceDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.EchoService".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability,
                        }))
                        .capability(
                            CapabilityBuilder::protocol()
                                .name("fuchsia.examples.Echo")
                                .path("/svc/foo"),
                        )
                        .expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: test_case.source.clone(),
                            source_name: "fuchsia.examples.Echo".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "fuchsia.examples.Echo".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: test_case.expose_availability,
                        }))
                        .capability(CapabilityBuilder::directory().name("dir").path("/data/dir"))
                        .expose(ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: test_case.source.clone(),
                            source_name: "dir".parse().unwrap(),
                            source_dictionary: None,
                            target_name: "dir".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: None,
                            subdir: None,
                            availability: test_case.expose_availability,
                        }))
                        .build(),
                ),
            ];
            let builder = T::new("a", components);
            let model = builder.build().await;

            // Add a file to the directory capability in the component that declared it, so "b".
            model
                .create_static_file(Path::new("dir/hippo"), "hello")
                .await
                .expect("failed to create file");
            for check_use in vec![
                CheckUse::Service {
                    path: "/svc/fuchsia.examples.EchoService_a".parse().unwrap(),
                    instance: ServiceInstance::Named("default".to_owned()),
                    member: "echo".to_owned(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
                CheckUse::Protocol {
                    path: "/svc/fuchsia.examples.Echo_a".parse().unwrap(),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
                CheckUse::Directory {
                    path: "/dir_a".parse().unwrap(),
                    file: PathBuf::from("hippo"),
                    expected_res: ExpectedResult::Err(zx_status::Status::NOT_FOUND),
                },
            ] {
                model.check_use(Moniker::root(), check_use).await;
            }
        }
    }
}
