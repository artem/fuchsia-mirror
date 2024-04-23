// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::{create_endpoints, create_proxy},
    fidl_test_wlan_realm as fidl_realm, fidl_test_wlan_testcontroller as fidl_testcontroller,
    fuchsia_component::client::connect_to_protocol,
    realm_client::{extend_namespace, InstalledNamespace},
    test_realm_helpers::{constants::TESTCONTROLLER_DRIVER_TOPOLOGICAL_PATH, tracing::Tracing},
};

pub struct DriversOnlyTestRealm {
    pub testcontroller_proxy: fidl_testcontroller::TestControllerProxy,
    _tracing: Option<Tracing>,
    _test_ns: InstalledNamespace,
}

impl DriversOnlyTestRealm {
    pub async fn new() -> Self {
        let realm_factory = connect_to_protocol::<fidl_realm::RealmFactoryMarker>()
            .expect("Could not connect to realm factory protocol");

        let (dict_client, dict_server) = create_endpoints();
        let (dev_topological, dev_topological_server) =
            create_proxy().expect("Could not create proxy");
        let (_dev_class, dev_class_server) = create_proxy().expect("Could not create proxy");

        let (pkg_client, pkg_server) = create_endpoints();
        fuchsia_fs::directory::open_channel_in_namespace(
            "/pkg",
            fidl_fuchsia_io::OpenFlags::RIGHT_READABLE
                | fidl_fuchsia_io::OpenFlags::RIGHT_EXECUTABLE,
            pkg_server,
        )
        .expect("Could not open /pkg");

        let options = fidl_realm::RealmOptions {
            topology: Some(fidl_realm::Topology::DriversOnly(fidl_realm::DriversOnly {
                driver_config: Some(fidl_realm::DriverConfig {
                    dev_topological: Some(dev_topological_server),
                    dev_class: Some(dev_class_server),
                    driver_test_realm_start_args: Some(fidl_fuchsia_driver_test::RealmArgs {
                        pkg: Some(pkg_client),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };

        realm_factory
            .create_realm2(options, dict_server)
            .await
            .expect("FIDL error on create_realm")
            .expect("create_realm returned an error");

        let testcontroller_proxy = device_watcher::recursive_wait_and_open::<
            fidl_testcontroller::TestControllerMarker,
        >(
            &dev_topological, TESTCONTROLLER_DRIVER_TOPOLOGICAL_PATH
        )
        .await
        .expect("Could not open testcontroller_proxy");

        let test_ns =
            extend_namespace(realm_factory, dict_client).await.expect("Failed to extend ns");

        let tracing = Tracing::create_and_initialize_tracing(test_ns.prefix())
            .await
            .map_err(|e| tracing::warn!("{e:?}"))
            .ok();

        Self { testcontroller_proxy, _tracing: tracing, _test_ns: test_ns }
    }
}
