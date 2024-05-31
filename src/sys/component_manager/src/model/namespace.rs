// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        constants::PKG_PATH,
        model::{
            component::{ComponentInstance, Package, WeakComponentInstance},
            routing::router_ext::{RouterExt, WeakComponentTokenExt},
            routing::{self, route_and_open_capability_with_reporting},
        },
    },
    ::routing::{
        component_instance::ComponentInstanceInterface, mapper::NoopRouteMapper, rights::Rights,
        route_to_storage_decl, verify_instance_in_component_id_index, DictExt, RouteRequest,
    },
    cm_rust::{ComponentDecl, UseDecl, UseEventStreamDecl, UseStorageDecl},
    errors::CreateNamespaceError,
    fidl::{endpoints::ClientEnd, prelude::*},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{
        channel::mpsc::{unbounded, UnboundedSender},
        FutureExt, StreamExt,
    },
    router_error::{Explain, RouterError},
    sandbox::{Capability, Dict, Directory, Open, Request, WeakComponentToken},
    serve_processargs::NamespaceBuilder,
    std::{collections::HashSet, sync::Arc},
    tracing::{error, warn},
    vfs::{
        directory::entry::{
            serve_directory, DirectoryEntry, DirectoryEntryAsync, EntryInfo, OpenRequest,
        },
        execution_scope::ExecutionScope,
    },
};

/// Creates a component's namespace.
///
/// TODO(b/298106231): eventually this should only build a delivery map as
/// the program dict will be fetched from the resolved component state.
pub async fn create_namespace(
    package: Option<&Package>,
    component: &Arc<ComponentInstance>,
    decl: &ComponentDecl,
    program_input_dict: &Dict,
    scope: ExecutionScope,
) -> Result<NamespaceBuilder, CreateNamespaceError> {
    let not_found_sender = not_found_logging(component);
    let mut namespace = NamespaceBuilder::new(scope.clone(), not_found_sender);
    if let Some(package) = package {
        let pkg_dir = fuchsia_fs::directory::clone_no_describe(&package.package_dir, None)
            .map_err(CreateNamespaceError::ClonePkgDirFailed)?;
        add_pkg_directory(&mut namespace, pkg_dir)?;
    }
    let uses = deduplicate_event_stream(decl.uses.iter());
    add_use_decls(&mut namespace, component, uses, program_input_dict).await?;
    Ok(namespace)
}

/// This function transforms a sequence of [`UseDecl`] such that the duplicate event stream
/// uses by paths are removed.
///
/// Different from all other use declarations, multiple event stream capabilities may be used
/// at the same path, the semantics being a single FIDL protocol capability is made available
/// at that path, subscribing to all the specified events:
/// see [`crate::model::events::registry::EventRegistry`].
fn deduplicate_event_stream<'a>(
    iter: std::slice::Iter<'a, UseDecl>,
) -> impl Iterator<Item = &'a UseDecl> {
    let mut paths = HashSet::new();
    iter.filter_map(move |use_decl| match use_decl {
        UseDecl::EventStream(ref event_stream) => {
            if !paths.insert(event_stream.target_path.clone()) {
                None
            } else {
                Some(use_decl)
            }
        }
        _ => Some(use_decl),
    })
}

/// Adds the package directory to the namespace under the path "/pkg".
fn add_pkg_directory(
    namespace: &mut NamespaceBuilder,
    pkg_dir: fio::DirectoryProxy,
) -> Result<(), CreateNamespaceError> {
    // TODO(https://fxbug.dev/42060182): Use Proxy::into_client_end when available.
    let client_end = ClientEnd::new(pkg_dir.into_channel().unwrap().into_zx_channel());
    let directory: sandbox::Directory = client_end.into();
    let path = cm_types::NamespacePath::new(PKG_PATH.to_str().unwrap()).unwrap();
    namespace.add_entry(Capability::Directory(directory), &path)?;
    Ok(())
}

/// Adds namespace entries for a component's use declarations.
async fn add_use_decls(
    namespace: &mut NamespaceBuilder,
    component: &Arc<ComponentInstance>,
    uses: impl Iterator<Item = &UseDecl>,
    program_input_dict: &Dict,
) -> Result<(), CreateNamespaceError> {
    for use_ in uses {
        if let cm_rust::UseDecl::Runner(_) = use_ {
            // The runner is not available in the namespace.
            continue;
        }
        if let cm_rust::UseDecl::Config(_) = use_ {
            // Configuration is not available in the namespace.
            continue;
        }

        let target_path =
            use_.path().ok_or_else(|| CreateNamespaceError::UseDeclWithoutPath(use_.clone()))?;
        let capability: Capability = match use_ {
            cm_rust::UseDecl::Directory(_) => directory_use(&use_, component).into(),
            cm_rust::UseDecl::Storage(storage) => {
                storage_use(storage, use_, component).await?.into()
            }
            cm_rust::UseDecl::Protocol(s) => {
                service_or_protocol_use(UseDecl::Protocol(s.clone()), component, program_input_dict)
                    .into()
            }
            cm_rust::UseDecl::Service(s) => {
                service_or_protocol_use(UseDecl::Service(s.clone()), component, program_input_dict)
                    .into()
            }
            cm_rust::UseDecl::EventStream(s) => service_or_protocol_use(
                UseDecl::EventStream(s.clone()),
                component,
                program_input_dict,
            )
            .into(),
            cm_rust::UseDecl::Runner(_) => {
                std::process::abort();
            }
            cm_rust::UseDecl::Config(_) => {
                std::process::abort();
            }
        };
        match use_ {
            cm_rust::UseDecl::Directory(_) | cm_rust::UseDecl::Storage(_) => {
                namespace.add_entry(capability, &target_path.clone().into())
            }
            cm_rust::UseDecl::Protocol(_)
            | cm_rust::UseDecl::Service(_)
            | cm_rust::UseDecl::EventStream(_) => namespace.add_object(capability, target_path),
            cm_rust::UseDecl::Runner(_) => {
                std::process::abort();
            }
            cm_rust::UseDecl::Config(_) => {
                std::process::abort();
            }
        }?
    }

    Ok(())
}

/// Makes a capability representing the storage described by `use_decl`. Once the channel
/// is readable, the future calls `route_storage` to forward the channel to the source
/// component's outgoing directory and terminates.
async fn storage_use(
    use_storage_decl: &UseStorageDecl,
    use_decl: &UseDecl,
    component: &Arc<ComponentInstance>,
) -> Result<Directory, CreateNamespaceError> {
    // Prevent component from using storage capability if it is restricted to the component ID
    // index, and the component isn't in the index.
    // To check that the storage capability is restricted to the storage decl, we have
    // to resolve the storage source capability. Because storage capabilities are only
    // ever `offer`d down the component tree, and we always resolve parents before
    // children, this resolution will walk the cache-happy path.
    // TODO(dgonyeo): Eventually combine this logic with the general-purpose startup
    // capability check.
    if let Ok(source) =
        route_to_storage_decl(use_storage_decl.clone(), &component, &mut NoopRouteMapper).await
    {
        verify_instance_in_component_id_index(&source, &component)
            .map_err(CreateNamespaceError::InstanceNotInInstanceIdIndex)?;
    }

    Ok(directory_use(use_decl, component))
}

/// Makes a capability representing the directory described by `use_`. Once the
/// channel is readable, the future calls `route_directory` to forward the channel to the
/// source component's outgoing directory and terminates.
///
/// `component` is a weak pointer, which is important because we don't want the task
/// waiting for channel readability to hold a strong pointer to this component lest it
/// create a reference cycle.
fn directory_use(use_: &UseDecl, component: &Arc<ComponentInstance>) -> Directory {
    let flags = match use_ {
        UseDecl::Directory(dir) => Rights::from(dir.rights).into_legacy(),
        UseDecl::Storage(_) => fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        _ => panic!("not a directory or storage capability"),
    };

    // Specify that the capability must be opened as a directory. In particular, this affects
    // how a devfs-based capability will handle the open call. If this flag is not specified,
    // devfs attempts to open the directory as a service, which is not what is desired here.
    let flags = flags | fio::OpenFlags::DIRECTORY;

    struct RouteDirectory {
        component: WeakComponentInstance,
        use_decl: UseDecl,
    }

    impl DirectoryEntry for RouteDirectory {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }

        fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
            request.spawn(self);
            Ok(())
        }
    }

    impl DirectoryEntryAsync for RouteDirectory {
        async fn open_entry_async(
            self: Arc<Self>,
            request: OpenRequest<'_>,
        ) -> Result<(), zx::Status> {
            if request.path().is_empty() {
                if !request.wait_till_ready().await {
                    return Ok(());
                }
            }

            // Hold a guard to prevent this task from being dropped during component destruction.
            let _guard = request.scope().active_guard();

            let target = match self.component.upgrade() {
                Ok(component) => component,
                Err(e) => {
                    error!(
                        "failed to upgrade WeakComponentInstance routing use \
                         decl `{:?}`: {:?}",
                        &self.use_decl, e
                    );
                    return Err(e.as_zx_status());
                }
            };

            route_directory(target, self.use_decl.clone(), request)
                .await
                .map_err(|e| e.as_zx_status())
        }
    }

    // Serve this directory on the component's execution scope rather than the namespace execution
    // scope so that requests don't block block namespace teardown, but they will block component
    // destruction.
    sandbox::Directory::new(
        serve_directory(
            Arc::new(RouteDirectory { component: component.as_weak(), use_decl: use_.clone() }),
            &component.execution_scope.clone(),
            flags,
        )
        .unwrap(),
    )
}

async fn route_directory(
    target: Arc<ComponentInstance>,
    use_: UseDecl,
    open_request: OpenRequest<'_>,
) -> Result<(), RouterError> {
    let (route_request, open_request) = match &use_ {
        UseDecl::Directory(use_dir_decl) => {
            (RouteRequest::UseDirectory(use_dir_decl.clone()), open_request)
        }
        UseDecl::Storage(use_storage_decl) => {
            (RouteRequest::UseStorage(use_storage_decl.clone()), open_request)
        }
        _ => panic!("not a directory or storage capability"),
    };
    route_and_open_capability_with_reporting(&route_request, &target, open_request).await?;
    Ok(())
}

/// Makes a capability for the service/protocol described by `use_`. The service will be
/// proxied to the outgoing directory of the source component.
///
/// `component` is a weak pointer, which is important because we don't want the VFS
/// closure to hold a strong pointer to this component lest it create a reference cycle.
fn service_or_protocol_use(
    use_: UseDecl,
    component: &Arc<ComponentInstance>,
    program_input_dict: &Dict,
) -> Open {
    match use_ {
        // Bedrock routing.
        UseDecl::Protocol(use_protocol_decl) => {
            let request = Request {
                availability: use_protocol_decl.availability.clone(),
                target: WeakComponentToken::new(component.as_weak()),
            };
            let Some(capability) =
                program_input_dict.get_capability(&use_protocol_decl.target_path)
            else {
                panic!(
                    "router for capability {:?} is missing from program input dictionary for \
                     component {}",
                    use_protocol_decl.target_path, component.moniker
                );
            };
            let Capability::Router(router) = &capability else {
                panic!(
                    "program input dictionary for component {} had an entry with an unexpected \
                     type: {:?}",
                    component.moniker, capability
                );
            };
            let router = router.clone();
            let legacy_request = RouteRequest::UseProtocol(use_protocol_decl.clone());

            // When there are router errors, they are sent to the error handler, which reports
            // errors.
            let weak_component = component.as_weak();

            Open::new(router.into_directory_entry(
                request,
                fio::DirentType::Service,
                component.execution_scope.clone(),
                move |error: &RouterError| {
                    let Ok(target) = weak_component.upgrade() else {
                        return None;
                    };
                    let legacy_request = legacy_request.clone();
                    Some(
                        async move {
                            routing::report_routing_failure(&legacy_request, &target, error).await
                        }
                        .boxed(),
                    )
                },
            ))
        }

        // Legacy routing.
        UseDecl::Service(use_service_decl) => {
            struct Service {
                component: WeakComponentInstance,
                scope: ExecutionScope,
                use_service_decl: cm_rust::UseServiceDecl,
            }
            impl DirectoryEntry for Service {
                fn entry_info(&self) -> EntryInfo {
                    EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
                }

                fn open_entry(
                    self: Arc<Self>,
                    mut request: OpenRequest<'_>,
                ) -> Result<(), zx::Status> {
                    // Move this request from the namespace scope to the component's scope so that
                    // we don't block namespace teardown.
                    request.set_scope(self.scope.clone());
                    request.spawn(self);
                    Ok(())
                }
            }
            impl DirectoryEntryAsync for Service {
                async fn open_entry_async(
                    self: Arc<Self>,
                    request: OpenRequest<'_>,
                ) -> Result<(), zx::Status> {
                    if request.path().is_empty() {
                        if !request.wait_till_ready().await {
                            return Ok(());
                        }
                    }

                    let component = match self.component.upgrade() {
                        Ok(component) => component,
                        Err(e) => {
                            error!(
                                "failed to upgrade WeakComponentInstance routing use \
                                 decl `{:?}`: {:?}",
                                self.use_service_decl, e
                            );
                            return Err(e.as_zx_status());
                        }
                    };

                    // Hold a guard to prevent this task from being dropped during component
                    // destruction.
                    let _guard = request.scope().active_guard();

                    let route_request = RouteRequest::UseService(self.use_service_decl.clone());

                    routing::route_and_open_capability_with_reporting(
                        &route_request,
                        &component,
                        request,
                    )
                    .await
                    .map_err(|e| e.as_zx_status())
                }
            }
            Open::new(Arc::new(Service {
                component: component.as_weak(),
                scope: component.execution_scope.clone(),
                use_service_decl,
            }))
        }

        UseDecl::EventStream(stream) => {
            struct UseEventStream {
                component: WeakComponentInstance,
                stream: UseEventStreamDecl,
            }
            impl DirectoryEntry for UseEventStream {
                fn entry_info(&self) -> EntryInfo {
                    EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Service)
                }
                fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
                    if !request.path().is_empty() {
                        return Err(zx::Status::NOT_DIR);
                    }
                    request.spawn(self);
                    Ok(())
                }
            }
            impl DirectoryEntryAsync for UseEventStream {
                async fn open_entry_async(
                    self: Arc<Self>,
                    mut request: OpenRequest<'_>,
                ) -> Result<(), zx::Status> {
                    let component = match self.component.upgrade() {
                        Ok(component) => component,
                        Err(e) => {
                            error!(
                                "failed to upgrade WeakComponentInstance routing use \
                                 decl `{:?}`: {:?}",
                                self.stream, e
                            );
                            return Err(e.as_zx_status());
                        }
                    };

                    request.prepend_path(&self.stream.target_path.to_string().try_into()?);
                    let route_request = RouteRequest::UseEventStream(self.stream.clone());
                    routing::route_and_open_capability_with_reporting(
                        &route_request,
                        &component,
                        request,
                    )
                    .await
                    .map_err(|e| e.as_zx_status())
                }
            }
            Open::new(Arc::new(UseEventStream { component: component.as_weak(), stream }))
        }

        _ => panic!("add_service_or_protocol_use called with non-service or protocol capability"),
    }
}

fn not_found_logging(component: &Arc<ComponentInstance>) -> UnboundedSender<String> {
    let (sender, mut receiver) = unbounded();
    let component_for_logger: WeakComponentInstance = component.as_weak();

    component.nonblocking_task_group().spawn(async move {
        while let Some(path) = receiver.next().await {
            match component_for_logger.upgrade() {
                Ok(target) => {
                    target
                        .with_logger_as_default(|| {
                            warn!(
                                "No capability available at path {} for component {}, \
                                verify the component has the proper `use` declaration.",
                                path, target.moniker
                            );
                        })
                        .await;
                }
                Err(_) => {}
            }
        }
    });

    sender
}
