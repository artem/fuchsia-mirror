// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, format_err, Error, Result},
    fidl::endpoints::ControlHandle,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_testing_harness::OperationError,
    fidl_test_wlan_realm::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon_status as zx_status,
    futures::{StreamExt, TryStreamExt},
    tracing::{error, info, warn},
};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let mut realms = vec![];
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
                RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                    match create_realm(options).await {
                        Ok(realm) => {
                            let request_stream = realm_server.into_stream()?;
                            task_group.spawn(async move {
                                realm_proxy::service::serve(realm, request_stream).await.unwrap();
                            });
                            responder.send(Ok(()))?;
                        }
                        Err(e) => {
                            error!("Failed to create realm: {:?}", e);
                            responder.send(Err(OperationError::Failed))?;
                        }
                    }
                }
                RealmFactoryRequest::CreateRealm2 { options, dictionary, responder } => {
                    let realm = create_realm(options).await?;
                    realm.root.controller().get_exposed_dictionary(dictionary).await?.unwrap();
                    realms.push(realm);
                    responder.send(Ok(()))?;
                }
            }
        }

        task_group.join().await;
        Ok(())
    }
    .await;

    if let Err(err) = result {
        // hw-sim tests allow error logs so we panic to ensure test failure.
        panic!("{:?}", err);
    }
}

async fn create_realm(mut options: RealmOptions) -> Result<RealmInstance, Error> {
    if let Some(topology) = options.topology {
        info!("Building the realm using topology {:#?}", topology);
        let builder = RealmBuilder::new().await?;
        match topology {
            Topology::DriversOnly(config, ..) => {
                builder.driver_test_realm_setup().await?;
                let realm = builder.build().await?;
                let driver_config = config.driver_config.ok_or(format_err!(
                    "DriversOnly topology requires driver_config, but none found"
                ))?;
                start_and_connect_to_driver_test_realm(&realm, driver_config).await?;
                Ok(realm)
            }
            TopologyUnknown!() => bail!("Unknown topology"),
        }
    } else if let Some(wlan_config) = options.wlan_config {
        // TODO(b/317255344): Remove this branch when no CTF tests depend on the deprecated API.
        warn!("Building the realm using deprecated wlan_config {:#?}", wlan_config);
        let mut params = RealmBuilderParams::new();
        if let Some(ref name) = wlan_config.name {
            params = params.realm_name(name);
        }
        let builder = RealmBuilder::with_params(params).await?;

        builder.driver_test_realm_setup().await?;
        create_wlan_components(&builder, wlan_config).await?;
        let realm = builder.build().await?;

        let devfs = options.devfs_server_end.take().unwrap();
        realm.root.get_exposed_dir().open(
            fidl_fuchsia_io::OpenFlags::DIRECTORY,
            fidl_fuchsia_io::ModeType::empty(),
            "dev-topological",
            ServerEnd::new(devfs.into_channel()),
        )?;

        Ok(realm)
    } else {
        error!("RealmOptions must include either topology or wlan_config: {:#?}", options);
        bail!("RealmOptions missing topology and wlan_config");
    }
}

/// Starts and connects to the driver test realm based on |driver_config|.
async fn start_and_connect_to_driver_test_realm(
    realm: &RealmInstance,
    driver_config: DriverConfig,
) -> Result<()> {
    let start_args = driver_config
        .driver_test_realm_start_args
        .ok_or(format_err!("DriverConfig requires driver_test_realm_start_args, but none found"))?;

    realm.driver_test_realm_start(start_args).await?;

    let dev_topological =
        driver_config.dev_topological.ok_or(format_err!("DriverConfig missing dev_topological"))?;
    realm.root.get_exposed_dir().open(
        fidl_fuchsia_io::OpenFlags::DIRECTORY,
        fidl_fuchsia_io::ModeType::empty(),
        "dev-topological",
        ServerEnd::new(dev_topological.into_channel()),
    )?;

    let dev_class = driver_config.dev_class.ok_or(format_err!("DriverConfig missing dev_class"))?;
    realm.root.get_exposed_dir().open(
        fidl_fuchsia_io::OpenFlags::DIRECTORY,
        fidl_fuchsia_io::ModeType::empty(),
        "dev-class",
        ServerEnd::new(dev_class.into_channel()),
    )?;

    Ok(())
}

async fn create_wlan_components(builder: &RealmBuilder, config: WlanConfig) -> Result<(), Error> {
    // Create child components.
    let archivist = builder
        .add_child("archivist", "#meta/archivist-for-embedding.cm", ChildOptions::new())
        .await?;

    let trace_manager =
        builder.add_child("trace_manager", "#meta/trace_manager.cm", ChildOptions::new()).await?;

    let wlandevicemonitor = builder
        .add_child("wlandevicemonitor", "#meta/wlandevicemonitor.cm", ChildOptions::new())
        .await?;

    // Start wlancfg as eager so that it automatically starts up without requiring the user to
    // connect to it.
    let wlancfg =
        builder.add_child("wlancfg", "#meta/wlancfg.cm", ChildOptions::new().eager()).await?;

    let stash = builder.add_child("stash", "#meta/stash_secure.cm", ChildOptions::new()).await?;

    // Configure components
    let use_legacy_privacy = config.use_legacy_privacy.unwrap_or(false);
    builder.init_mutable_config_to_empty(&wlandevicemonitor).await?;
    builder
        .set_config_value(&wlandevicemonitor, "wep_supported", use_legacy_privacy.into())
        .await?;
    builder
        .set_config_value(&wlandevicemonitor, "wpa1_supported", use_legacy_privacy.into())
        .await?;

    builder.init_mutable_config_to_empty(&wlancfg).await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.wlan.RecoveryProfile".parse()?,
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String("".into())),
        }))
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.wlan.RecoveryEnabled".parse()?,
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(false)),
        }))
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.wlan.RecoveryProfile"))
                .capability(Capability::configuration("fuchsia.wlan.RecoveryEnabled"))
                .from(Ref::self_())
                .to(&wlancfg),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.ClientProvider"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.AccessPointProvider"))
                .from(&wlancfg)
                .to(Ref::parent()),
        )
        .await?;

    // fuchsia.wlan.device.service.DeviceMonitor is used by set_country
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.wlan.device.service.DeviceMonitor",
                ))
                .from(&wlandevicemonitor)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(&archivist)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&archivist)
                .to(&wlancfg)
                .to(&wlandevicemonitor)
                .to(&stash),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.tracing.provider.Registry"))
                .from(&trace_manager)
                .to(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(&archivist)
                .to(&wlancfg)
                .to(&wlandevicemonitor)
                .to(&stash),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.tracing.controller.Controller"))
                .from(&trace_manager)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::event_stream("directory_ready"))
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::storage("data"))
                .from(Ref::parent())
                .to(&stash)
                .to(&wlancfg),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(&archivist)
                .to(&wlancfg)
                .to(&wlandevicemonitor),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name("fuchsia.wlan.device.service.DeviceMonitor")
                        .weak(),
                )
                .from(&wlandevicemonitor)
                .to(&wlancfg),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-class").subdir("wlanphy").as_("dev-wlanphy"))
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(&wlandevicemonitor),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.stash.SecureStore"))
                .from(&stash)
                .to(&wlancfg),
        )
        .await?;

    // Handle optional components based on config
    if config.with_regulatory_region.unwrap_or(true) {
        let regulatory_region = builder
            .add_child("regulatory_region", "#meta/regulatory_region.cm", ChildOptions::new())
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::protocol_by_name(
                            "fuchsia.location.namedplace.RegulatoryRegionWatcher",
                        )
                        .weak(),
                    )
                    .from(&regulatory_region)
                    .to(&wlancfg),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .capability(Capability::storage("cache"))
                    .from(Ref::parent())
                    .to(&regulatory_region),
            )
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl::endpoints::create_endpoints, fidl_fuchsia_driver_test::RealmArgs,
        test_case::test_case,
    };

    // RealmOptions without specific topology or wlan_config are invalid
    #[test_case(RealmOptions { ..Default::default() })]
    #[test_case(RealmOptions { topology: None, wlan_config: None, ..Default::default() })]
    #[fuchsia::test]
    async fn reject_invalid_realm_options(opts: RealmOptions) {
        assert!(create_realm(opts).await.is_err());
    }

    // DriversOnly topology with missing or unspecified fields is invalid
    #[test_case(DriversOnly { ..Default::default() })]
    #[test_case(
        DriversOnly {
            driver_config: None,
            ..Default::default()
        }
    )]
    #[test_case(
        DriversOnly {
            driver_config: Some(DriverConfig { ..Default::default() }),
            ..Default::default()
        }
    )]
    #[test_case(
        DriversOnly {
            driver_config: Some(
                DriverConfig {
                    dev_topological: None,
                    dev_class: None,
                    driver_test_realm_start_args: Some(RealmArgs { ..Default::default() }),
                    ..Default::default()
                }
            ),
            ..Default::default()
        }
    )]
    #[fuchsia::test]
    async fn reject_invalid_drivers_only_topology(drivers_only: DriversOnly) {
        let opts = RealmOptions {
            topology: Some(Topology::DriversOnly(drivers_only)),
            ..Default::default()
        };
        assert!(create_realm(opts).await.is_err());
    }

    #[fuchsia::test]
    async fn accept_valid_drivers_only_config() {
        let (_dev_topological_client, dev_topological) = create_endpoints();
        let (_dev_class_client, dev_class) = create_endpoints();
        let opts = RealmOptions {
            topology: Some(Topology::DriversOnly(DriversOnly {
                driver_config: Some(DriverConfig {
                    dev_topological: Some(dev_topological),
                    dev_class: Some(dev_class),
                    driver_test_realm_start_args: Some(RealmArgs { ..Default::default() }),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(create_realm(opts).await.is_ok());
    }
}
