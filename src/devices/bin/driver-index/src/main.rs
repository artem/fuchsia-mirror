// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::driver_loading_fuzzer::Session,
    crate::indexer::*,
    crate::load_driver::*,
    crate::resolved_driver::ResolvedDriver,
    anyhow::Context,
    driver_index_config::Config,
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_driver_development as fdd,
    fidl_fuchsia_driver_framework as fdf,
    fidl_fuchsia_driver_index::{DriverIndexRequest, DriverIndexRequestStream},
    fidl_fuchsia_driver_registrar as fdr, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_component::client,
    fuchsia_component::server::ServiceFs,
    fuchsia_zircon::Status,
    futures::prelude::*,
    std::{
        collections::HashMap,
        collections::HashSet,
        rc::Rc,
        sync::{Arc, Mutex},
    },
};

mod composite_node_spec_manager;
mod driver_loading_fuzzer;
mod indexer;
mod load_driver;
mod match_common;
mod resolved_driver;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    DriverIndexProtocol(DriverIndexRequestStream),
    DriverDevelopmentProtocol(fdd::DriverIndexRequestStream),
    DriverRegistrarProtocol(fdr::DriverRegistrarRequestStream),
}

fn ignore_peer_closed(err: fidl::Error) -> Result<(), fidl::Error> {
    if err.is_closed() {
        Ok(())
    } else {
        Err(err)
    }
}

fn log_error(err: anyhow::Error) -> anyhow::Error {
    tracing::error!("{:#?}", err);
    err
}

fn create_and_setup_index(boot_drivers: Vec<ResolvedDriver>, config: &Config) -> Rc<Indexer> {
    if !config.enable_driver_load_fuzzer {
        return Rc::new(Indexer::new(
            boot_drivers,
            BaseRepo::NotResolved,
            config.delay_fallback_until_base_drivers_indexed,
        ));
    }

    let indexer = Rc::new(Indexer::new(
        vec![],
        BaseRepo::NotResolved,
        config.delay_fallback_until_base_drivers_indexed,
    ));

    // TODO(fxb/126225): Pass in a seed from the input, if available.
    let (sender, receiver) = futures::channel::mpsc::unbounded::<Vec<ResolvedDriver>>();
    indexer.clone().start_driver_load(
        receiver,
        Session::new(
            sender,
            boot_drivers,
            fuchsia_zircon::Duration::from_millis(config.driver_load_fuzzer_max_delay_ms),
            None,
        ),
    );
    indexer
}

async fn run_driver_info_iterator_server(
    driver_info: Arc<Mutex<Vec<fdf::DriverInfo>>>,
    stream: fdd::DriverInfoIteratorRequestStream,
) -> Result<(), anyhow::Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            let driver_info_clone = driver_info.clone();
            match request {
                fdd::DriverInfoIteratorRequest::GetNext { responder } => {
                    let result = {
                        let mut driver_info = driver_info_clone.lock().unwrap();
                        let len = driver_info.len();
                        driver_info.split_off(len - std::cmp::min(100, len))
                    };

                    responder
                        .send(&result)
                        .or_else(ignore_peer_closed)
                        .context("error responding to GetDriverInfo")?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn run_composite_node_specs_iterator_server(
    specs: Arc<Mutex<Vec<fdf::CompositeInfo>>>,
    stream: fdd::CompositeNodeSpecIteratorRequestStream,
) -> Result<(), anyhow::Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            let specs_clone = specs.clone();
            match request {
                fdd::CompositeNodeSpecIteratorRequest::GetNext { responder } => {
                    let result = {
                        let mut specs = specs_clone.lock().unwrap();
                        let len = specs.len();
                        specs.split_off(len - std::cmp::min(10, len))
                    };

                    responder
                        .send(&result)
                        .or_else(ignore_peer_closed)
                        .context("error responding to GetNodeGroups")?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn run_driver_development_server(
    indexer: Rc<Indexer>,
    stream: fdd::DriverIndexRequestStream,
) -> Result<(), anyhow::Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            let indexer = indexer.clone();
            match request {
                fdd::DriverIndexRequest::GetDriverInfo { driver_filter, iterator, .. } => {
                    let driver_info = indexer.get_driver_info(driver_filter);
                    if driver_info.len() == 0 {
                        iterator.close_with_epitaph(Status::NOT_FOUND)?;
                        return Ok(());
                    }
                    let driver_info = Arc::new(Mutex::new(driver_info));
                    let iterator = iterator.into_stream()?;
                    fasync::Task::spawn(async move {
                        run_driver_info_iterator_server(driver_info, iterator)
                            .await
                            .expect("Failed to run driver info iterator");
                    })
                    .detach();
                }
                fdd::DriverIndexRequest::GetCompositeNodeSpecs {
                    name_filter, iterator, ..
                } => {
                    let composite_node_spec_manager = indexer.composite_node_spec_manager.borrow();
                    let specs = composite_node_spec_manager.get_specs(name_filter);
                    if specs.is_empty() {
                        iterator.close_with_epitaph(Status::NOT_FOUND)?;
                        return Ok(());
                    }
                    let specs = Arc::new(Mutex::new(specs));
                    let iterator = iterator.into_stream()?;
                    fasync::Task::spawn(async move {
                        run_composite_node_specs_iterator_server(specs, iterator)
                            .await
                            .expect("Failed to run specs iterator");
                    })
                    .detach();
                }
                fdd::DriverIndexRequest::DisableMatchWithDriverUrl { driver_url, responder } => {
                    indexer.disable_driver(driver_url);
                    responder
                        .send()
                        .or_else(ignore_peer_closed)
                        .context("error responding to Disable")?;
                }
                fdd::DriverIndexRequest::ReEnableMatchWithDriverUrl { driver_url, responder } => {
                    let result = indexer.re_enable_driver(driver_url);
                    responder
                        .send(result)
                        .or_else(ignore_peer_closed)
                        .context("error responding to Disable")?;
                }
                fdd::DriverIndexRequest::_UnknownMethod { ordinal, method_type, .. } => {
                    tracing::warn!(
                        "DriverIndexRequest::UnknownMethod {:?} with ordinal {}",
                        method_type,
                        ordinal
                    );
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn run_driver_registrar_server(
    indexer: Rc<Indexer>,
    stream: fdr::DriverRegistrarRequestStream,
    full_resolver: &Option<fresolution::ResolverProxy>,
) -> Result<(), anyhow::Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            let indexer = indexer.clone();
            match request {
                fdr::DriverRegistrarRequest::Register { package_url, responder } => {
                    match full_resolver {
                        None => {
                            responder
                                .send(Err(Status::PROTOCOL_NOT_SUPPORTED.into_raw()))
                                .or_else(ignore_peer_closed)
                                .context("error responding to Register")?;
                        }
                        Some(resolver) => {
                            let register_result =
                                indexer.register_driver(package_url, resolver).await;

                            responder
                                .send(register_result)
                                .or_else(ignore_peer_closed)
                                .context("error responding to Register")?;
                        }
                    }
                }
                fdr::DriverRegistrarRequest::_UnknownMethod { ordinal, method_type, .. } => {
                    tracing::warn!(
                        "DriverRegistrarRequest::UnknownMethod {:?} with ordinal {}",
                        method_type,
                        ordinal
                    );
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn run_index_server(
    indexer: Rc<Indexer>,
    stream: DriverIndexRequestStream,
) -> Result<(), anyhow::Error> {
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            let indexer = indexer.clone();
            match request {
                DriverIndexRequest::MatchDriver { args, responder } => {
                    responder
                        .send(indexer.match_driver(args).as_ref().map_err(|e| *e))
                        .or_else(ignore_peer_closed)
                        .context("error responding to MatchDriver")?;
                }
                DriverIndexRequest::WatchForDriverLoad { responder } => {
                    indexer.watch_for_driver_load(responder);
                }
                DriverIndexRequest::AddCompositeNodeSpec { payload, responder } => {
                    responder
                        .send(indexer.add_composite_node_spec(payload))
                        .or_else(ignore_peer_closed)
                        .context("error responding to AddCompositeNodeSpec")?;
                }
                DriverIndexRequest::RebindCompositeNodeSpec {
                    spec,
                    driver_url_suffix,
                    responder,
                } => {
                    responder
                        .send(indexer.rebind_composite(spec, driver_url_suffix))
                        .or_else(ignore_peer_closed)
                        .context("error responding to RebindCompositeNodeSpec")?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

// Merge the two boot driver lists, skipping duplicates that can occur during the
// soft-transition of boot drivers to packages.
fn merge_boot_drivers(
    packaged_boot_drivers: Vec<ResolvedDriver>,
    unpackaged_boot_drivers: Vec<ResolvedDriver>,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let mut drivers = Vec::new();
    let mut unpackaged_boot_drivers = unpackaged_boot_drivers
        .into_iter()
        .map(|d| (d.component_url.to_string(), d))
        .collect::<HashMap<String, ResolvedDriver>>();
    for driver in packaged_boot_drivers {
        let unpackaged_url = format!(
            "fuchsia-boot:///#{}",
            driver
                .component_url
                .fragment()
                .context("Packaged boot driver url is missing fragment")?
        );
        // Remove the packaged driver from unpackaged_boot_drivers if present.
        if unpackaged_boot_drivers.remove(&unpackaged_url).is_some() {
            tracing::info!(
                "BootFs file is still generated for packaged driver: {}",
                driver.component_url
            );
        }
        drivers.push(driver);
    }
    // Add the remaining unpackaged drivers to the final drivers list.
    drivers.extend(unpackaged_boot_drivers.into_values());
    Ok(drivers)
}

// NOTE: This tag is load-bearing to make sure that the output
// shows up in serial.
#[fuchsia::main(logging_tags = ["driver"])]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();

    service_fs.dir("svc").add_fidl_service(IncomingRequest::DriverIndexProtocol);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::DriverDevelopmentProtocol);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::DriverRegistrarProtocol);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    let config = Config::take_from_startup_handle();

    let full_resolver = if config.enable_ephemeral_drivers {
        Some(
            client::connect_to_protocol_at_path::<fresolution::ResolverMarker>(
                "/svc/fuchsia.component.resolution.Resolver-full",
            )
            .context("Failed to connect to full package resolver")?,
        )
    } else {
        None
    };

    let eager_drivers: HashSet<url::Url> = config
        .bind_eager
        .iter()
        .filter(|url| !url.is_empty())
        .filter_map(|url| url::Url::parse(url).ok())
        .collect();
    let disabled_drivers: HashSet<url::Url> = config
        .disabled_drivers
        .iter()
        .filter(|url| !url.is_empty())
        .filter_map(|url| url::Url::parse(url).ok())
        .collect();
    for driver in disabled_drivers.iter() {
        tracing::info!("Disabling driver {}", driver);
    }
    for driver in eager_drivers.iter() {
        tracing::info!("Marking driver {} as eager", driver);
    }

    let boot = fuchsia_fs::directory::open_in_namespace("/boot", fio::OpenFlags::RIGHT_READABLE)
        .context("Failed to open /boot")?;
    let boot_resolver = client::connect_to_protocol_at_path::<fresolution::ResolverMarker>(
        "/svc/fuchsia.component.resolution.Resolver-boot",
    )
    .context("Failed to connect to boot resolver")?;

    let packaged_boot_drivers =
        load_boot_drivers(&boot, &boot_resolver, &eager_drivers, &disabled_drivers)
            .await
            .context("Failed to load boot drivers")
            .map_err(log_error)?;

    // TODO(fxbug.dev/97517): Resolve unpackaged boot drivers until they have been fully migrated.
    let unpackaged_boot_drivers =
        load_unpackaged_boot_drivers(&boot, &eager_drivers, &disabled_drivers)
            .await
            .context("Failed to load unpackaged boot drivers")
            .map_err(log_error)?;

    // Combine packaged and unpackaged drivers into the final drivers list used to load drivers.
    let drivers = merge_boot_drivers(packaged_boot_drivers, unpackaged_boot_drivers)?;

    let mut should_load_base_drivers = true;

    for argument in std::env::args() {
        if argument == "--no-base-drivers" {
            should_load_base_drivers = false;
            tracing::info!("Not loading base drivers");
        }
    }

    let index = create_and_setup_index(drivers, &config);

    let (res1, _) = futures::future::join(
        async {
            if should_load_base_drivers {
                let base_resolver =
                    client::connect_to_protocol_at_path::<fresolution::ResolverMarker>(
                        "/svc/fuchsia.component.resolution.Resolver-base",
                    )
                    .context("Failed to connect to base component resolver")?;
                load_base_drivers(
                    index.clone(),
                    &boot,
                    &base_resolver,
                    &eager_drivers,
                    &disabled_drivers,
                )
                .await
                .context("Error loading base packages")
                .map_err(log_error)
            } else {
                Ok(())
            }
        },
        async {
            service_fs
                .for_each_concurrent(None, |request: IncomingRequest| async {
                    // match on `request` and handle each protocol.
                    match request {
                        IncomingRequest::DriverIndexProtocol(stream) => {
                            run_index_server(index.clone(), stream).await
                        }
                        IncomingRequest::DriverDevelopmentProtocol(stream) => {
                            run_driver_development_server(index.clone(), stream).await
                        }
                        IncomingRequest::DriverRegistrarProtocol(stream) => {
                            run_driver_registrar_server(index.clone(), stream, &full_resolver).await
                        }
                    }
                    .unwrap_or_else(|e| tracing::error!("Error running index_server: {:?}", e))
                })
                .await;
        },
    )
    .await;

    res1?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use {
        crate::resolved_driver::{DriverPackageType, ResolvedDriver},
        bind::{
            compiler::{
                CompiledBindRules, CompositeBindRules, CompositeNode, Symbol, SymbolicInstruction,
                SymbolicInstructionInfo,
            },
            interpreter::decode_bind_rules::DecodedRules,
            parser::bind_library::ValueType,
        },
        fidl::endpoints::{ClientEnd, Proxy},
        fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_data as fdata,
        fidl_fuchsia_driver_framework as fdf, fidl_fuchsia_driver_index as fdi,
        fidl_fuchsia_mem as fmem,
        std::collections::HashMap,
    };

    fn create_driver_info(
        url: String,
        colocate: bool,
        device_categories: Vec<fdf::DeviceCategory>,
        package_type: DriverPackageType,
        fallback: bool,
    ) -> fdf::DriverInfo {
        fdf::DriverInfo {
            url: Some(url),
            colocate: Some(colocate),
            device_categories: Some(device_categories),
            package_type: fdf::DriverPackageType::from_primitive(package_type as u8),
            is_fallback: Some(fallback),
            bind_rules_bytecode: Some(vec![]),
            ..Default::default()
        }
    }

    fn create_default_device_category() -> fdf::DeviceCategory {
        fdf::DeviceCategory {
            category: Some(resolved_driver::DEFAULT_DEVICE_CATEGORY.to_string()),
            subcategory: None,
            ..Default::default()
        }
    }

    async fn get_driver_info_proxy(
        development_proxy: &fdd::DriverIndexProxy,
        driver_filter: &[String],
    ) -> Vec<fdf::DriverInfo> {
        let (info_iterator, info_iterator_server) =
            fidl::endpoints::create_proxy::<fdd::DriverInfoIteratorMarker>().unwrap();
        development_proxy.get_driver_info(driver_filter, info_iterator_server).unwrap();

        let mut driver_infos = Vec::new();
        loop {
            let driver_info = info_iterator.get_next().await;
            if driver_info.is_err() {
                break;
            }
            let mut driver_info = driver_info.unwrap();
            if driver_info.len() == 0 {
                break;
            }
            driver_infos.append(&mut driver_info)
        }

        return driver_infos;
    }

    async fn resolve_component_from_namespace(
        component_url: &str,
    ) -> Result<fresolution::Component, anyhow::Error> {
        let (client_end, server_end) = fidl::endpoints::create_endpoints();
        fuchsia_fs::directory::open_channel_in_namespace(
            "/pkg",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            server_end,
        )?;
        let proxy = client_end.into_proxy()?;
        let component_url = url::Url::parse(component_url)?;
        let decl_file = fuchsia_fs::directory::open_file_no_describe(
            &proxy,
            component_url.fragment().unwrap(),
            fio::OpenFlags::RIGHT_READABLE,
        )?;
        let decl: fdecl::Component = fuchsia_fs::file::read_fidl(&decl_file).await?;
        Ok(fresolution::Component {
            decl: Some(fmem::Data::Bytes(fidl::persist(&decl).unwrap())),
            package: Some(fresolution::Package {
                directory: Some(ClientEnd::new(proxy.into_channel().unwrap().into())),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    async fn run_resolver_server(
        stream: fresolution::ResolverRequestStream,
    ) -> Result<(), anyhow::Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    fresolution::ResolverRequest::Resolve { component_url, responder } => {
                        let component = resolve_component_from_namespace(&component_url).await?;
                        responder.send(Ok(component)).context("error sending response")?;
                    }
                    fresolution::ResolverRequest::ResolveWithContext {
                        component_url: _,
                        context: _,
                        responder,
                    } => {
                        tracing::error!(
                            "ResolveWithContext is not currently implemented in driver-index"
                        );
                        responder
                            .send(Err(fresolution::ResolverError::Internal))
                            .context("error sending response")?;
                    }
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn execute_driver_index_test(
        index: Indexer,
        stream: fdi::DriverIndexRequestStream,
        test: impl Future<Output = ()>,
    ) {
        let index = Rc::new(index);
        let index_task = run_index_server(index.clone(), stream).fuse();
        let test = test.fuse();

        futures::pin_mut!(index_task, test);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test => {},
        }
    }

    fn create_always_match_bind_rules() -> DecodedRules {
        let bind_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(bind_rules).unwrap(),
        )
        .unwrap()
    }

    // This test depends on '/pkg/config/drivers_for_test.json' existing in the test package.
    // The test reads that json file to determine which bind rules to read and index.
    #[fuchsia::test]
    async fn read_from_json() {
        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let eager_drivers = HashSet::new();
        let disabled_drivers = HashSet::new();

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        // Run two tasks: the fake resolver and the task that loads the base drivers.
        let load_base_drivers_task =
            load_base_drivers(index.clone(), &boot, &resolver, &eager_drivers, &disabled_drivers)
                .fuse();
        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_base_drivers_task, resolver_task);
        futures::select! {
            result = load_base_drivers_task => {
                result.unwrap();
            },
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
        };

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            // Check the value from the 'test-bind' binary. This should match my-driver.cm
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(1),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_url =
                "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm"
                    .to_string();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!(expected_url, d.url.unwrap());
                    assert_eq!(true, d.colocate.unwrap());
                    assert_eq!(false, d.is_fallback.unwrap());
                    assert_eq!(fdf::DriverPackageType::Base, d.package_type.unwrap());
                    assert_eq!(
                        vec![create_default_device_category()],
                        d.device_categories.unwrap()
                    );
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            // Check the value from the 'test-bind2' binary. This should match my-driver2.cm
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_url =
                "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind2-component.cm"
                    .to_string();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!(expected_url, d.url.unwrap());
                    assert_eq!(false, d.colocate.unwrap());
                    assert_eq!(false, d.is_fallback.unwrap());
                    assert_eq!(fdf::DriverPackageType::Base, d.package_type.unwrap());
                    assert_eq!(
                        vec![create_default_device_category()],
                        d.device_categories.unwrap()
                    );
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            // Check an unknown value. This should return the NOT_FOUND error.
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(3),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_bind_string() {
        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("my-key".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("test-value".to_string()),
                },
            }],
            symbol_table: HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        // Make our driver.
        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm")
                .unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("my-key".to_string()),
                value: fdf::NodePropertyValue::StringValue("test-value".to_string()),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_result = fdi::MatchDriverResult::Driver(fdf::DriverInfo {
                url: Some("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm".to_string()),
                colocate: Some(false),
                device_categories: Some(vec![]),
                package_type: Some(fdf::DriverPackageType::Base),
                is_fallback: Some(false),
                bind_rules_bytecode: Some(vec![]),
                ..Default::default()
            });

            assert_eq!(expected_result, result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_bind_enum() {
        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("my-key".to_string(), ValueType::Enum),
                    rhs: Symbol::EnumValue("test-value".to_string()),
                },
            }],
            symbol_table: HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        // Make our driver.
        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm")
                .unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("my-key".to_string()),
                value: fdf::NodePropertyValue::EnumValue("test-value".to_string()),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_result = fdi::MatchDriverResult::Driver(fdf::DriverInfo {
                url: Some("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm".to_string()),
                colocate: Some(false),
                package_type: Some(fdf::DriverPackageType::Base),
                is_fallback: Some(false),
                device_categories: Some(vec![]),
                bind_rules_bytecode: Some(vec![]),
                ..Default::default()
            });

            assert_eq!(expected_result, result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_multiple_non_fallbacks() {
        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        let boot_repo = vec![
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-1.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-2.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
        ];

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(boot_repo, BaseRepo::Resolved(std::vec![]), false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            let result = proxy.match_driver(&args).await.unwrap();

            assert_eq!(result, Err(Status::NOT_SUPPORTED.into_raw()));
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_url_matching() {
        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        let boot_repo = vec![
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-1.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-2.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
        ];

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(boot_repo, BaseRepo::Resolved(std::vec![]), false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property.clone()]),
                driver_url_suffix: Some("driver-1.cm".to_string()),
                ..Default::default()
            };

            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!("fuchsia-boot:///#meta/driver-1.cm", d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property.clone()]),
                driver_url_suffix: Some("driver-2.cm".to_string()),
                ..Default::default()
            };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!("fuchsia-boot:///#meta/driver-2.cm", d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property]),
                driver_url_suffix: Some("bad_driver.cm".to_string()),
                ..Default::default()
            };

            let result = proxy.match_driver(&args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_url_disabled() {
        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        let boot_repo = vec![
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-1.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
            ResolvedDriver {
                component_url: url::Url::parse("fuchsia-boot:///#meta/driver-2.cm").unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
        ];

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let (development_proxy, development_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdd::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(boot_repo, BaseRepo::Resolved(std::vec![]), false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let development_task =
            run_driver_development_server(index.clone(), development_stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property.clone()]),
                driver_url_suffix: Some("driver-1.cm".to_string()),
                ..Default::default()
            };

            // Ask for driver-1 and it should give that to us.
            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!("fuchsia-boot:///#meta/driver-1.cm", d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            // Disable driver-1.
            development_proxy
                .disable_match_with_driver_url(&fidl_fuchsia_pkg::PackageUrl {
                    url: "fuchsia-boot:///#meta/driver-1.cm".to_string(),
                })
                .await
                .unwrap();

            // Ask for driver-1 again and it should return not found since its been disabled.
            let result = proxy.match_driver(&args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));

            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property.clone()]),
                ..Default::default()
            };

            // Ask for any and we should get driver-2, since driver-1 is disabled.
            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!("fuchsia-boot:///#meta/driver-2.cm", d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            development_proxy
                .re_enable_match_with_driver_url(&fidl_fuchsia_pkg::PackageUrl {
                    url: "fuchsia-boot:///#meta/driver-1.cm".to_string(),
                })
                .await
                .unwrap()
                .unwrap();

            let args = fdi::MatchDriverArgs {
                properties: Some(vec![property.clone()]),
                driver_url_suffix: Some("driver-1.cm".to_string()),
                ..Default::default()
            };

            // Ask for driver-1 and it should give that to us since it's not disabled anymore.
            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!("fuchsia-boot:///#meta/driver-1.cm", d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }
        }
        .fuse();

        futures::pin_mut!(index_task, development_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            result = development_task => {
                panic!("Development task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_non_fallback_boot_priority() {
        const FALLBACK_BOOT_DRIVER_COMPONENT_URL: &str =
            "fuchsia-pkg://fuchsia.com/package#driver/fallback-boot.cm";
        const NON_FALLBACK_BOOT_DRIVER_COMPONENT_URL: &str =
            "fuchsia-pkg://fuchsia.com/package#driver/non-fallback-base.cm";

        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        let boot_repo = vec![
            ResolvedDriver {
                component_url: url::Url::parse(FALLBACK_BOOT_DRIVER_COMPONENT_URL).unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: true,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
            ResolvedDriver {
                component_url: url::Url::parse(NON_FALLBACK_BOOT_DRIVER_COMPONENT_URL).unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Boot,
                package_hash: None,
                is_dfv2: None,
            },
        ];

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(boot_repo, BaseRepo::Resolved(std::vec![]), false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_result = fdi::MatchDriverResult::Driver(create_driver_info(
                NON_FALLBACK_BOOT_DRIVER_COMPONENT_URL.to_string(),
                false,
                vec![],
                DriverPackageType::Boot,
                false,
            ));

            // The non-fallback boot driver should be returned and not the
            // fallback boot driver.
            assert_eq!(result, expected_result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_non_fallback_base_priority() {
        const FALLBACK_BOOT_DRIVER_COMPONENT_URL: &str =
            "fuchsia-pkg://fuchsia.com/package#driver/fallback-boot.cm";
        const NON_FALLBACK_BASE_DRIVER_COMPONENT_URL: &str =
            "fuchsia-pkg://fuchsia.com/package#driver/non-fallback-base.cm";
        const FALLBACK_BASE_DRIVER_COMPONENT_URL: &str =
            "fuchsia-pkg://fuchsia.com/package#driver/fallback-base.cm";

        // Make the bind instructions.
        let always_match = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match).unwrap(),
        )
        .unwrap();

        let boot_repo = vec![ResolvedDriver {
            component_url: url::Url::parse(FALLBACK_BOOT_DRIVER_COMPONENT_URL).unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: true,
            package_type: DriverPackageType::Boot,
            package_hash: None,
            is_dfv2: None,
        }];

        let base_repo = BaseRepo::Resolved(std::vec![
            ResolvedDriver {
                component_url: url::Url::parse(FALLBACK_BASE_DRIVER_COMPONENT_URL).unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: true,
                package_type: DriverPackageType::Base,
                package_hash: None,
                is_dfv2: None,
            },
            ResolvedDriver {
                component_url: url::Url::parse(NON_FALLBACK_BASE_DRIVER_COMPONENT_URL).unwrap(),
                bind_rules: always_match.clone(),
                bind_bytecode: vec![],
                colocate: false,
                device_categories: vec![],
                fallback: false,
                package_type: DriverPackageType::Base,
                package_hash: None,
                is_dfv2: None,
            },
        ]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(boot_repo, base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_result = fdi::MatchDriverResult::Driver(create_driver_info(
                NON_FALLBACK_BASE_DRIVER_COMPONENT_URL.to_string(),
                false,
                vec![],
                DriverPackageType::Base,
                false,
            ));

            // The non-fallback base driver should be returned and not the
            // fallback boot driver even though boot drivers get priority
            // because non-fallback drivers get even higher priority.
            assert_eq!(result, expected_result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_load_packaged_boot_drivers() {
        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();
        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        let eager_drivers = HashSet::new();
        let disabled_drivers = HashSet::new();

        let load_boot_drivers_task =
            load_boot_drivers(&boot, &resolver, &eager_drivers, &disabled_drivers).fuse();

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_boot_drivers_task, resolver_task);
        let drivers = futures::select! {
            result = load_boot_drivers_task => result.unwrap(),
            result = resolver_task => panic!("Resolver task finished: {:?}", result),
        };

        // Expect package qualifiers are set for all packaged drivers.
        assert!(drivers.iter().all(|driver| driver
            .component_url
            .as_str()
            .starts_with("fuchsia-boot:///driver-index-unittests")));
        assert!(drivers
            .iter()
            .all(|driver| driver.package_type == resolved_driver::DriverPackageType::Boot));
        assert!(drivers.iter().all(|driver| driver.package_hash.is_some()));
    }

    #[fuchsia::test]
    async fn test_load_eager_fallback_boot_driver() {
        let eager_driver_component_url = url::Url::parse(
            "fuchsia-boot:///driver-index-unittests#meta/test-fallback-component.cm",
        )
        .unwrap();
        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();
        let eager_drivers = HashSet::from([eager_driver_component_url.clone()]);
        let disabled_drivers = HashSet::new();

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        let load_boot_drivers_task =
            load_boot_drivers(&boot, &resolver, &eager_drivers, &disabled_drivers).fuse();

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_boot_drivers_task, resolver_task);
        let drivers = futures::select! {
            result = load_boot_drivers_task => {
                result.unwrap()
            },
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
        };
        assert!(
            !drivers
                .iter()
                .find(|driver| driver.component_url == eager_driver_component_url)
                .expect("Fallback driver did not load")
                .fallback
        );
    }

    #[fuchsia::test]
    async fn test_load_eager_fallback_base_driver() {
        let eager_driver_component_url = url::Url::parse(
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-fallback-component.cm",
        )
        .unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let eager_drivers = HashSet::from([eager_driver_component_url.clone()]);
        let disabled_drivers = HashSet::new();

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        let load_base_drivers_task = load_base_drivers(
            Rc::clone(&index),
            &boot,
            &resolver,
            &eager_drivers,
            &disabled_drivers,
        )
        .fuse();
        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_base_drivers_task, resolver_task);
        futures::select! {
            result = load_base_drivers_task => {
                result.unwrap();
            },
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
        };

        let base_repo = index.base_repo.borrow();
        match *base_repo {
            BaseRepo::Resolved(ref drivers) => {
                assert!(
                    !drivers
                        .iter()
                        .find(|driver| driver.component_url == eager_driver_component_url)
                        .expect("Fallback driver did not load")
                        .fallback
                );
            }
            _ => {
                panic!("Base repo was not resolved");
            }
        }
    }

    #[fuchsia::test]
    async fn test_dont_load_disabled_fallback_unpackaged_boot_driver() {
        let disabled_driver_component_url =
            url::Url::parse("fuchsia-boot:///#meta/test-fallback-component.cm").unwrap();

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();
        let drivers = load_unpackaged_boot_drivers(
            &boot,
            &HashSet::new(),
            &HashSet::from([disabled_driver_component_url.clone()]),
        )
        .await
        .unwrap();
        assert!(drivers
            .iter()
            .find(|driver| driver.component_url == disabled_driver_component_url)
            .is_none());
    }

    #[fuchsia::test]
    async fn test_dont_load_disabled_fallback_boot_driver() {
        let disabled_driver_component_url = url::Url::parse(
            "fuchsia-boot:///driver-index-unittests#meta/test-fallback-component.cm",
        )
        .unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();
        let eager_drivers = HashSet::new();
        let disabled_drivers = HashSet::from([disabled_driver_component_url.clone()]);

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        let load_boot_drivers_task =
            load_boot_drivers(&boot, &resolver, &eager_drivers, &disabled_drivers).fuse();

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_boot_drivers_task, resolver_task);
        let drivers = futures::select! {
            result = load_boot_drivers_task => {
                result.unwrap()
            },
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
        };
        assert!(drivers
            .iter()
            .find(|driver| driver.component_url == disabled_driver_component_url)
            .is_none());
    }

    #[fuchsia::test]
    async fn test_dont_load_disabled_fallback_base_driver() {
        let disabled_driver_component_url = url::Url::parse(
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-fallback-component.cm",
        )
        .unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let eager_drivers = HashSet::new();
        let disabled_drivers = HashSet::from([disabled_driver_component_url.clone()]);

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();

        let load_base_drivers_task = load_base_drivers(
            Rc::clone(&index),
            &boot,
            &resolver,
            &eager_drivers,
            &disabled_drivers,
        )
        .fuse();
        let resolver_task = run_resolver_server(resolver_stream).fuse();
        futures::pin_mut!(load_base_drivers_task, resolver_task);
        futures::select! {
            result = load_base_drivers_task => {
                result.unwrap();
            },
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
        };

        let base_repo = index.base_repo.borrow();
        match *base_repo {
            BaseRepo::Resolved(ref drivers) => {
                assert!(drivers
                    .iter()
                    .find(|driver| driver.component_url == disabled_driver_component_url)
                    .is_none());
            }
            _ => {
                panic!("Base repo was not resolved");
            }
        }
    }

    #[fuchsia::test]
    async fn test_match_driver_when_require_system_true_and_base_repo_not_resolved() {
        let always_match = create_always_match_bind_rules();
        let boot_repo = vec![ResolvedDriver {
            component_url: url::Url::parse("fuchsia-boot:///#driver/fallback-boot.cm").unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: true,
            package_type: DriverPackageType::Boot,
            package_hash: None,
            is_dfv2: None,
        }];
        let index = Indexer::new(boot_repo, BaseRepo::NotResolved, true);
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        execute_driver_index_test(index, stream, async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap();

            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));
        })
        .await;
    }

    #[fuchsia::test]
    async fn test_match_driver_when_require_system_false_and_base_repo_not_resolved() {
        const FALLBACK_BOOT_DRIVER_COMPONENT_URL: &str = "fuchsia-boot:///#driver/fallback-boot.cm";

        let always_match = create_always_match_bind_rules();
        let boot_repo = vec![ResolvedDriver {
            component_url: url::Url::parse(FALLBACK_BOOT_DRIVER_COMPONENT_URL).unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: true,
            package_type: DriverPackageType::Boot,
            package_hash: None,
            is_dfv2: None,
        }];
        let index = Indexer::new(boot_repo, BaseRepo::NotResolved, false);
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        execute_driver_index_test(index, stream, async move {
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_result = fdi::MatchDriverResult::Driver(create_driver_info(
                FALLBACK_BOOT_DRIVER_COMPONENT_URL.to_string(),
                false,
                vec![],
                DriverPackageType::Boot,
                true,
            ));
            assert_eq!(result, expected_result);
        })
        .await;
    }

    // This test relies on two drivers existing in the /pkg/ directory of the
    // test package.
    #[fuchsia::test]
    async fn test_boot_drivers() {
        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();
        let drivers =
            load_unpackaged_boot_drivers(&boot, &HashSet::new(), &HashSet::new()).await.unwrap();

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(drivers, BaseRepo::NotResolved, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            // Check the value from the 'test-bind' binary. This should match my-driver.cm
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(1),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_url = "fuchsia-boot:///#meta/test-bind-component.cm".to_string();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!(expected_url, d.url.unwrap());
                    assert_eq!(true, d.colocate.unwrap());
                    assert_eq!(false, d.is_fallback.unwrap());
                    assert_eq!(fdf::DriverPackageType::Boot, d.package_type.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            // Check the value from the 'test-bind2' binary. This should match my-driver2.cm
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(2),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap().unwrap();

            let expected_url = "fuchsia-boot:///#meta/test-bind2-component.cm".to_string();
            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!(expected_url, d.url.unwrap());
                    assert_eq!(false, d.colocate.unwrap());
                    assert_eq!(false, d.is_fallback.unwrap());
                    assert_eq!(fdf::DriverPackageType::Boot, d.package_type.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }

            // Check an unknown value. This should return the NOT_FOUND error.
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(3),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };
            let result = proxy.match_driver(&args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_parent_spec_match() {
        let base_repo = BaseRepo::Resolved(std::vec![]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();
        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));
        let index_task = run_index_server(index.clone(), stream).fuse();

        let test_task = async move {
            let bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let properties = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let composite_sepc = fdf::CompositeNodeSpec {
                name: Some("test_group".to_string()),
                parents: Some(vec![fdf::ParentSpec {
                    bind_rules: bind_rules,
                    properties: properties,
                }]),
                ..Default::default()
            };

            assert_eq!(Ok(()), proxy.add_composite_node_spec(&composite_sepc).await.unwrap());

            let device_properties_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::IntValue(1),
                    value: fdf::NodePropertyValue::IntValue(200),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    value: fdf::NodePropertyValue::StringValue("plover".to_string()),
                },
            ];
            let match_args = fdi::MatchDriverArgs {
                properties: Some(device_properties_match),
                ..Default::default()
            };

            let result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            assert_eq!(
                fdi::MatchDriverResult::CompositeParents(vec![fdf::CompositeParent {
                    composite: Some(fdf::CompositeInfo {
                        spec: Some(composite_sepc.clone()),
                        ..Default::default()
                    }),
                    index: Some(0),
                    ..Default::default()
                }]),
                result
            );

            let device_properties_mismatch = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::IntValue(1),
                    value: fdf::NodePropertyValue::IntValue(200),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    value: fdf::NodePropertyValue::StringValue("dotterel".to_string()),
                },
            ];
            let mismatch_args = fdi::MatchDriverArgs {
                properties: Some(device_properties_mismatch),
                ..Default::default()
            };

            let result = proxy.match_driver(&mismatch_args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_matched_composite() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();
        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url.clone(),
            bind_rules: rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_match".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match.clone(),
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let node_1_props_nonmatch = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
            }];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_non_match_1".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_nonmatch,
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let node_2_props_nonmatch = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(1),
                value: fdf::NodePropertyValue::IntValue(10),
            }];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_non_match_2".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match,
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_nonmatch,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_no_optional_matched_composite_with_optional() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let optional_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("trembler".to_string()),
            },
        }];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![CompositeNode {
                name: "lapwing".to_string(),
                instructions: optional_node_inst,
            }],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();
        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url.clone(),
            bind_rules: rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_match".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match.clone(),
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let node_1_props_nonmatch = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
            }];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_non_match_1".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_nonmatch,
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let node_2_props_nonmatch = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(1),
                value: fdf::NodePropertyValue::IntValue(10),
            }];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_non_match_2".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match,
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_nonmatch,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_with_optional_matched_composite_with_optional() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let optional_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("trembler".to_string()),
            },
        }];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![CompositeNode {
                name: "lapwing".to_string(),
                instructions: optional_node_inst,
            }],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();
        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url.clone(),
            bind_rules: rules,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let optional_1_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(2),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let optional_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                value: fdf::NodePropertyValue::StringValue("trembler".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("spec_match".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: optional_1_bind_rules.clone(),
                                properties: optional_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match.clone(),
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_then_driver() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        // Start our index out without any drivers.
        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            // When we add the spec it should get not found since there's no drivers.
            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("test_group".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let device_properties_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::IntValue(1),
                    value: fdf::NodePropertyValue::IntValue(200),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    value: fdf::NodePropertyValue::StringValue("plover".to_string()),
                },
            ];
            let match_args = fdi::MatchDriverArgs {
                properties: Some(device_properties_match),
                ..Default::default()
            };

            // We can see the spec comes back without a matched driver.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(None, info[0].composite.as_ref().unwrap().matched_driver);
            } else {
                assert!(false, "Did not get back a spec.");
            }

            // Notify the spec manager of a new composite driver.
            {
                let mut composite_node_spec_manager =
                    index.composite_node_spec_manager.borrow_mut();
                composite_node_spec_manager.new_driver_available(ResolvedDriver {
                    component_url: url.clone(),
                    bind_rules: rules,
                    bind_bytecode: vec![],
                    colocate: false,
                    device_categories: vec![],
                    fallback: false,
                    package_type: DriverPackageType::Base,
                    package_hash: None,
                    is_dfv2: None,
                });
            }

            // Now when we get it back, it has the matching composite driver on it.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(
                    &"mimid".to_string(),
                    info[0]
                        .composite
                        .as_ref()
                        .unwrap()
                        .matched_driver
                        .as_ref()
                        .unwrap()
                        .composite_driver
                        .as_ref()
                        .unwrap()
                        .composite_name
                        .as_ref()
                        .unwrap()
                );
            } else {
                assert!(false, "Did not get back a spec.");
            }
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_no_optional_then_driver_with_optional() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let optional_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("trembler".to_string()),
            },
        }];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![CompositeNode {
                name: "lapwing".to_string(),
                instructions: optional_node_inst,
            }],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        // Start our index out without any drivers.
        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            // When we add the spec it should get not found since there's no drivers.
            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("test_group".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let device_properties_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::IntValue(1),
                    value: fdf::NodePropertyValue::IntValue(200),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    value: fdf::NodePropertyValue::StringValue("plover".to_string()),
                },
            ];
            let match_args = fdi::MatchDriverArgs {
                properties: Some(device_properties_match),
                ..Default::default()
            };

            // We can see the spec comes back without a matched drive.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(None, info[0].composite.as_ref().unwrap().matched_driver);
            } else {
                assert!(false, "Did not get back a spec.");
            }

            // Notify the spec manager of a new composite driver.
            {
                let mut composite_node_spec_manager =
                    index.composite_node_spec_manager.borrow_mut();
                composite_node_spec_manager.new_driver_available(ResolvedDriver {
                    component_url: url.clone(),
                    bind_rules: rules,
                    bind_bytecode: vec![],
                    colocate: false,
                    device_categories: vec![],
                    fallback: false,
                    package_type: DriverPackageType::Base,
                    package_hash: None,
                    is_dfv2: None,
                });
            }

            // Now when we get it back, it has the matching composite driver on it.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(
                    &"mimid".to_string(),
                    info[0]
                        .composite
                        .as_ref()
                        .unwrap()
                        .matched_driver
                        .as_ref()
                        .unwrap()
                        .composite_driver
                        .as_ref()
                        .unwrap()
                        .composite_name
                        .as_ref()
                        .unwrap()
                );
            } else {
                assert!(false, "Did not get back a spec.");
            }
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_with_optional_then_driver_with_optional() {
        // Create the Composite Bind rules.
        let primary_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("trembler".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("thrasher".to_string()),
            },
        }];

        let additional_node_inst = vec![
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                    rhs: Symbol::StringValue("catbird".to_string()),
                },
            },
            SymbolicInstructionInfo {
                location: None,
                instruction: SymbolicInstruction::AbortIfNotEqual {
                    lhs: Symbol::Key("catbird".to_string(), ValueType::Number),
                    rhs: Symbol::NumberValue(1),
                },
            },
        ];

        let optional_node_inst = vec![SymbolicInstructionInfo {
            location: None,
            instruction: SymbolicInstruction::AbortIfNotEqual {
                lhs: Symbol::Key("thrasher".to_string(), ValueType::Str),
                rhs: Symbol::StringValue("trembler".to_string()),
            },
        }];

        let bind_rules = CompositeBindRules {
            device_name: "mimid".to_string(),
            symbol_table: HashMap::new(),
            primary_node: CompositeNode {
                name: "catbird".to_string(),
                instructions: primary_node_inst,
            },
            additional_nodes: vec![CompositeNode {
                name: "mockingbird".to_string(),
                instructions: additional_node_inst,
            }],
            optional_nodes: vec![CompositeNode {
                name: "lapwing".to_string(),
                instructions: optional_node_inst,
            }],
            enable_debug: false,
        };

        let bytecode = CompiledBindRules::CompositeBind(bind_rules).encode_to_bytecode().unwrap();
        let rules = DecodedRules::new(bytecode).unwrap();

        // Make the composite driver.
        let url =
            url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/dg_matched_composite.cm")
                .unwrap();

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        // Start our index out without any drivers.
        let index = Rc::new(Indexer::new(std::vec![], BaseRepo::NotResolved, false));

        let index_task = run_index_server(index.clone(), stream).fuse();
        let test_task = async move {
            let node_1_bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let node_2_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(1),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let node_2_props_match = vec![
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("catbird".to_string()),
                    value: fdf::NodePropertyValue::IntValue(1),
                },
                fdf::NodeProperty {
                    key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                    value: fdf::NodePropertyValue::StringValue("catbird".to_string()),
                },
            ];

            let optional_1_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(2),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(10)],
            }];

            let optional_1_props_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("thrasher".to_string()),
                value: fdf::NodePropertyValue::StringValue("trembler".to_string()),
            }];

            // When we add the spec it should get not found since there's no drivers.
            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("test_group".to_string()),
                        parents: Some(vec![
                            fdf::ParentSpec {
                                bind_rules: node_1_bind_rules.clone(),
                                properties: node_1_props_match.clone(),
                            },
                            fdf::ParentSpec {
                                bind_rules: node_2_bind_rules.clone(),
                                properties: node_2_props_match,
                            },
                            fdf::ParentSpec {
                                bind_rules: optional_1_bind_rules.clone(),
                                properties: optional_1_props_match,
                            },
                        ]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let device_properties_match = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(2),
                value: fdf::NodePropertyValue::IntValue(10),
            }];
            let match_args = fdi::MatchDriverArgs {
                properties: Some(device_properties_match),
                ..Default::default()
            };

            // We can see the spec comes back without a matched composite.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(None, info[0].composite.as_ref().unwrap().matched_driver);
            } else {
                assert!(false, "Did not get back a spec.");
            }

            // Notify the spec manager of a new composite driver.
            {
                let mut composite_node_spec_manager =
                    index.composite_node_spec_manager.borrow_mut();
                composite_node_spec_manager.new_driver_available(ResolvedDriver {
                    component_url: url.clone(),
                    bind_rules: rules,
                    bind_bytecode: vec![],
                    colocate: false,
                    device_categories: vec![],
                    fallback: false,
                    package_type: DriverPackageType::Base,
                    package_hash: None,
                    is_dfv2: None,
                });
            }

            // Now when we get it back, it has the matching composite driver on it.
            let match_result = proxy.match_driver(&match_args).await.unwrap().unwrap();
            if let fdi::MatchDriverResult::CompositeParents(info) = match_result {
                assert_eq!(
                    &"mimid".to_string(),
                    info[0]
                        .composite
                        .as_ref()
                        .unwrap()
                        .matched_driver
                        .as_ref()
                        .unwrap()
                        .composite_driver
                        .as_ref()
                        .unwrap()
                        .composite_name
                        .as_ref()
                        .unwrap()
                );
            } else {
                assert!(false, "Did not get back a spec.");
            }
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_duplicate_path() {
        let always_match_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match_rules).unwrap(),
        )
        .unwrap();

        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm")
                .unwrap(),
            bind_rules: always_match,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();
        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));
        let index_task = run_index_server(index.clone(), stream).fuse();

        let test_task = async move {
            let bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(1),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::StringValue("lapwing".to_string()),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            assert_eq!(
                Ok(()),
                proxy
                    .add_composite_node_spec(&fdf::CompositeNodeSpec {
                        name: Some("test_group".to_string()),
                        parents: Some(vec![fdf::ParentSpec {
                            bind_rules: bind_rules,
                            properties: vec![fdf::NodeProperty {
                                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
                            }]
                        }]),
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            );

            let duplicate_bind_rules = vec![fdf::BindRule {
                key: fdf::NodePropertyKey::IntValue(200),
                condition: fdf::Condition::Accept,
                values: vec![fdf::NodePropertyValue::IntValue(2)],
            }];

            let node_transform = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let result = proxy
                .add_composite_node_spec(&fdf::CompositeNodeSpec {
                    name: Some("test_group".to_string()),
                    parents: Some(vec![fdf::ParentSpec {
                        bind_rules: duplicate_bind_rules,
                        properties: node_transform,
                    }]),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(Err(Status::ALREADY_EXISTS.into_raw()), result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_add_composite_node_spec_duplicate_key() {
        let always_match_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match_rules).unwrap(),
        )
        .unwrap();

        let base_repo = BaseRepo::Resolved(std::vec![ResolvedDriver {
            component_url: url::Url::parse("fuchsia-pkg://fuchsia.com/package#driver/my-driver.cm")
                .unwrap(),
            bind_rules: always_match,
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        },]);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();
        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));
        let index_task = run_index_server(index.clone(), stream).fuse();

        let test_task = async move {
            let bind_rules = vec![
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(20),
                    condition: fdf::Condition::Accept,
                    values: vec![
                        fdf::NodePropertyValue::IntValue(200),
                        fdf::NodePropertyValue::IntValue(150),
                    ],
                },
                fdf::BindRule {
                    key: fdf::NodePropertyKey::IntValue(20),
                    condition: fdf::Condition::Accept,
                    values: vec![fdf::NodePropertyValue::StringValue("plover".to_string())],
                },
            ];

            let node_transform = vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::StringValue("trembler".to_string()),
                value: fdf::NodePropertyValue::StringValue("thrasher".to_string()),
            }];

            let result = proxy
                .add_composite_node_spec(&fdf::CompositeNodeSpec {
                    name: Some("test_group".to_string()),
                    parents: Some(vec![fdf::ParentSpec {
                        bind_rules: bind_rules,
                        properties: node_transform,
                    }]),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(Err(Status::INVALID_ARGS.into_raw()), result);
        }
        .fuse();

        futures::pin_mut!(index_task, test_task);
        futures::select! {
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_register_and_match_ephemeral_driver() {
        let component_manifest_url =
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm";

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fdi::DriverIndexMarker>().unwrap();

        let (registrar_proxy, registrar_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdr::DriverRegistrarMarker>().unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let full_resolver = Some(resolver);

        let base_repo = BaseRepo::Resolved(std::vec![]);
        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        let index_task = run_index_server(index.clone(), stream).fuse();
        let registrar_task =
            run_driver_registrar_server(index.clone(), registrar_stream, &full_resolver).fuse();

        let test_task = async {
            // Short delay since the resolver server is starting up at the same time.
            fasync::Timer::new(std::time::Duration::from_millis(100)).await;

            // These properties match the bind rules for the "test-bind-componenet.cm".
            let property = fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(1),
            };
            let args =
                fdi::MatchDriverArgs { properties: Some(vec![property]), ..Default::default() };

            // First attempt should fail since we haven't registered it.
            let result = proxy.match_driver(&args).await.unwrap();
            assert_eq!(result, Err(Status::NOT_FOUND.into_raw()));

            // Now register the ephemeral driver.
            let pkg_url = fidl_fuchsia_pkg::PackageUrl { url: component_manifest_url.to_string() };
            registrar_proxy.register(&pkg_url).await.unwrap().unwrap();

            // Match succeeds now.
            let result = proxy.match_driver(&args).await.unwrap().unwrap();
            let expected_url =
                "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm"
                    .to_string();

            match result {
                fdi::MatchDriverResult::Driver(d) => {
                    assert_eq!(expected_url, d.url.unwrap());
                }
                fdi::MatchDriverResult::CompositeParents(p) => {
                    panic!("Bad match driver: {:#?}", p);
                }
                _ => panic!("Bad case"),
            }
        }
        .fuse();

        futures::pin_mut!(resolver_task, index_task, registrar_task, test_task);
        futures::select! {
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
            result = index_task => {
                panic!("Index task finished: {:?}", result);
            },
            result = registrar_task => {
                panic!("Registrar task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_register_and_get_ephemeral_driver() {
        let component_manifest_url =
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm";

        let (registrar_proxy, registrar_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdr::DriverRegistrarMarker>().unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let (development_proxy, development_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdd::DriverIndexMarker>().unwrap();

        let full_resolver = Some(resolver);

        let base_repo = BaseRepo::Resolved(std::vec![]);
        let index = Rc::new(Indexer::new(std::vec![], base_repo, false));

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        let development_task =
            run_driver_development_server(index.clone(), development_stream).fuse();
        let registrar_task =
            run_driver_registrar_server(index.clone(), registrar_stream, &full_resolver).fuse();
        let test_task = async move {
            // We should not find this before registering it.
            let driver_infos =
                get_driver_info_proxy(&development_proxy, &[component_manifest_url.to_string()])
                    .await;
            assert_eq!(0, driver_infos.len());

            // Short delay since the resolver server starts at the same time.
            fasync::Timer::new(std::time::Duration::from_millis(100)).await;

            // Register the ephemeral driver.
            let pkg_url = fidl_fuchsia_pkg::PackageUrl { url: component_manifest_url.to_string() };
            registrar_proxy.register(&pkg_url).await.unwrap().unwrap();

            // Now that it's registered we should find it.
            let driver_infos =
                get_driver_info_proxy(&development_proxy, &[component_manifest_url.to_string()])
                    .await;
            assert_eq!(1, driver_infos.len());
            assert_eq!(&component_manifest_url.to_string(), driver_infos[0].url.as_ref().unwrap());
        }
        .fuse();

        futures::pin_mut!(resolver_task, development_task, registrar_task, test_task);
        futures::select! {
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
            result = development_task => {
                panic!("Development task finished: {:?}", result);
            },
            result = registrar_task => {
                panic!("Registrar task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_register_exists_in_base() {
        let always_match_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match_rules).unwrap(),
        )
        .unwrap();

        let component_manifest_url =
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm";

        let (registrar_proxy, registrar_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdr::DriverRegistrarMarker>().unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let full_resolver = Some(resolver);

        let boot_repo = std::vec![];
        let base_repo = BaseRepo::Resolved(vec![ResolvedDriver {
            component_url: url::Url::parse(component_manifest_url).unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Base,
            package_hash: None,
            is_dfv2: None,
        }]);

        let index = Rc::new(Indexer::new(boot_repo, base_repo, false));

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        let registrar_task =
            run_driver_registrar_server(index.clone(), registrar_stream, &full_resolver).fuse();
        let test_task = async move {
            // Try to register the driver.
            let pkg_url = fidl_fuchsia_pkg::PackageUrl { url: component_manifest_url.to_string() };
            let register_result = registrar_proxy.register(&pkg_url).await.unwrap();

            // The register should have failed.
            assert_eq!(fuchsia_zircon::sys::ZX_ERR_ALREADY_EXISTS, register_result.err().unwrap());
        }
        .fuse();

        futures::pin_mut!(resolver_task, registrar_task, test_task);
        futures::select! {
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
            result = registrar_task => {
                panic!("Registrar task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_register_exists_in_boot() {
        let always_match_rules = bind::compiler::BindRules {
            instructions: vec![],
            symbol_table: std::collections::HashMap::new(),
            use_new_bytecode: true,
            enable_debug: false,
        };
        let always_match = DecodedRules::new(
            bind::bytecode_encoder::encode_v2::encode_to_bytecode_v2(always_match_rules).unwrap(),
        )
        .unwrap();

        let component_manifest_url =
            "fuchsia-pkg://fuchsia.com/driver-index-unittests#meta/test-bind-component.cm";

        let (registrar_proxy, registrar_stream) =
            fidl::endpoints::create_proxy_and_stream::<fdr::DriverRegistrarMarker>().unwrap();

        let (resolver, resolver_stream) =
            fidl::endpoints::create_proxy_and_stream::<fresolution::ResolverMarker>().unwrap();

        let full_resolver = Some(resolver);

        let boot_repo = vec![ResolvedDriver {
            component_url: url::Url::parse(component_manifest_url).unwrap(),
            bind_rules: always_match.clone(),
            bind_bytecode: vec![],
            colocate: false,
            device_categories: vec![],
            fallback: false,
            package_type: DriverPackageType::Boot,
            package_hash: None,
            is_dfv2: None,
        }];

        let base_repo = BaseRepo::Resolved(std::vec![]);
        let index = Rc::new(Indexer::new(boot_repo, base_repo, false));

        let resolver_task = run_resolver_server(resolver_stream).fuse();
        let registrar_task =
            run_driver_registrar_server(index.clone(), registrar_stream, &full_resolver).fuse();
        let test_task = async move {
            // Try to register the driver.
            let pkg_url = fidl_fuchsia_pkg::PackageUrl { url: component_manifest_url.to_string() };
            let register_result = registrar_proxy.register(&pkg_url).await.unwrap();

            // The register should have failed.
            assert_eq!(fuchsia_zircon::sys::ZX_ERR_ALREADY_EXISTS, register_result.err().unwrap());
        }
        .fuse();

        futures::pin_mut!(resolver_task, registrar_task, test_task);
        futures::select! {
            result = resolver_task => {
                panic!("Resolver task finished: {:?}", result);
            },
            result = registrar_task => {
                panic!("Registrar task finished: {:?}", result);
            },
            () = test_task => {},
        }
    }

    #[fuchsia::test]
    async fn test_get_device_categories_from_component_data() {
        assert_eq!(
            resolved_driver::get_device_categories_from_component_data(&vec![
                fdata::Dictionary {
                    entries: Some(vec![fdata::DictionaryEntry {
                        key: "category".to_string(),
                        value: Some(Box::new(fdata::DictionaryValue::Str("usb".to_string())))
                    }]),
                    ..Default::default()
                },
                fdata::Dictionary {
                    entries: Some(vec![
                        fdata::DictionaryEntry {
                            key: "category".to_string(),
                            value: Some(Box::new(fdata::DictionaryValue::Str(
                                "connectivity".to_string()
                            ))),
                        },
                        fdata::DictionaryEntry {
                            key: "subcategory".to_string(),
                            value: Some(Box::new(fdata::DictionaryValue::Str(
                                "ethernet".to_string()
                            ))),
                        }
                    ]),
                    ..Default::default()
                }
            ]),
            vec![
                fdf::DeviceCategory {
                    category: Some("usb".to_string()),
                    subcategory: None,
                    ..Default::default()
                },
                fdf::DeviceCategory {
                    category: Some("connectivity".to_string()),
                    subcategory: Some("ethernet".to_string()),
                    ..Default::default()
                }
            ]
        );
    }
}
