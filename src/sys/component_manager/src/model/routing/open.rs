// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        model::{
            component::{ComponentInstance, ExtendedInstance, StartReason, WeakComponentInstance},
            error::{ModelError, OpenError},
            routing::{
                providers::{
                    DefaultComponentCapabilityProvider, DirectoryEntryCapabilityProvider,
                    NamespaceCapabilityProvider,
                },
                service::{
                    AnonymizedAggregateServiceDir, AnonymizedServiceRoute,
                    FilteredAggregateServiceProvider,
                },
                RouteSource,
            },
            storage::{self, BackingDirectoryInfo},
        },
    },
    ::routing::{component_instance::ComponentInstanceInterface, path::PathBufExt},
    cm_moniker::InstancedExtendedMoniker,
    cm_util::channel,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    moniker::MonikerBase,
    std::{path::PathBuf, sync::Arc},
};

/// Contains the options to use when opening a capability.
///
/// See [`fidl_fuchsia_io::Directory::Open`] for how the `flags`, `relative_path`,
/// and `server_chan` parameters are used in the open call.
pub struct OpenOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

/// A request to open a capability at its source.
pub enum OpenRequest<'a> {
    // Open a capability backed by a component's outgoing directory.
    OutgoingDirectory {
        open_options: OpenOptions<'a>,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
    },
    // Open a storage capability.
    Storage {
        open_options: OpenOptions<'a>,
        source: storage::BackingDirectoryInfo,
        target: &'a Arc<ComponentInstance>,
    },
}

impl<'a> OpenRequest<'a> {
    /// Creates a request to open a capability with source `route_source` for `target`.
    pub fn new_from_route_source(
        route_source: RouteSource,
        target: &'a Arc<ComponentInstance>,
        mut open_options: OpenOptions<'a>,
    ) -> Self {
        let RouteSource { source, relative_path } = route_source;
        open_options.relative_path =
            relative_path.attach(open_options.relative_path).to_string_lossy().into();
        Self::OutgoingDirectory { open_options, source, target }
    }

    /// Creates a request to open a storage capability with source `storage_source` for `target`.
    pub fn new_from_storage_source(
        source: BackingDirectoryInfo,
        target: &'a Arc<ComponentInstance>,
        open_options: OpenOptions<'a>,
    ) -> Self {
        Self::Storage { open_options, source, target }
    }

    /// Opens the capability in `self`, triggering a `CapabilityRouted` event and binding
    /// to the source component instance if necessary.
    pub async fn open(self) -> Result<(), OpenError> {
        match self {
            Self::OutgoingDirectory { open_options, source, target } => {
                Self::open_outgoing_directory(open_options, source, target).await
            }
            Self::Storage { open_options, source, target } => {
                Self::open_storage(open_options, &source, target)
                    .await
                    .map_err(|e| OpenError::OpenStorageError { err: Box::new(e) })
            }
        }
    }

    async fn open_outgoing_directory(
        open_options: OpenOptions<'a>,
        source: CapabilitySource,
        target: &Arc<ComponentInstance>,
    ) -> Result<(), OpenError> {
        let capability_provider = if let Some(provider) =
            Self::get_default_provider(target.as_weak(), &source)
                .await
                .map_err(|e| OpenError::GetDefaultProviderError { err: Box::new(e) })?
        {
            provider
        } else {
            target
                .context
                .find_internal_provider(&source, target.as_weak())
                .await
                .ok_or(OpenError::CapabilityProviderNotFound)?
        };

        let OpenOptions { flags, relative_path, mut server_chan } = open_options;

        let source_instance =
            source.source_instance().upgrade().map_err(|_| OpenError::SourceInstanceNotFound)?;
        let task_group = match source_instance {
            ExtendedInstance::AboveRoot(top) => top.task_group(),
            ExtendedInstance::Component(component) => component.nonblocking_task_group(),
        };
        capability_provider
            .open(task_group, flags, PathBuf::from(relative_path), &mut server_chan)
            .await?;
        Ok(())
    }

    async fn open_storage(
        open_options: OpenOptions<'a>,
        source: &storage::BackingDirectoryInfo,
        target: &Arc<ComponentInstance>,
    ) -> Result<(), ModelError> {
        // As of today, the storage component instance must contain the target. This is because it
        // is impossible to expose storage declarations up.
        let moniker =
            target.instanced_moniker().strip_prefix(&source.storage_source_moniker).unwrap();

        let dir_source = source.storage_provider.clone();
        let storage_dir_proxy = storage::open_isolated_storage(
            &source,
            target.persistent_storage,
            moniker.clone(),
            target.instance_id(),
        )
        .await
        .map_err(|e| ModelError::from(e))?;

        let OpenOptions { flags, relative_path, server_chan } = open_options;

        // Open the storage with the provided flags, mode, relative_path and server_chan.
        // We don't clone the directory because we can't specify the mode or path that way.
        let server_chan = channel::take_channel(server_chan);

        // If there is no relative path, assume it is the current directory. We use "."
        // because `fuchsia.io/Directory.Open` does not accept empty paths.
        let relative_path = if relative_path.is_empty() { "." } else { &relative_path };

        storage_dir_proxy
            .open(flags, fio::ModeType::empty(), relative_path, ServerEnd::new(server_chan))
            .map_err(|err| {
                let source_moniker = match &dir_source {
                    Some(r) => {
                        InstancedExtendedMoniker::ComponentInstance(r.instanced_moniker().clone())
                    }
                    None => InstancedExtendedMoniker::ComponentManager,
                };
                ModelError::OpenStorageFailed {
                    source_moniker,
                    moniker,
                    path: relative_path.to_string(),
                    err,
                }
            })?;
        Ok(())
    }

    /// Returns an instance of the default capability provider for the capability at `source`, if
    /// supported.
    async fn get_default_provider(
        target: WeakComponentInstance,
        source: &CapabilitySource,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        match source {
            CapabilitySource::Component { capability, component } => {
                // Route normally for a component capability with a source path
                Ok(match capability.source_path() {
                    Some(path) => Some(Box::new(DefaultComponentCapabilityProvider::new(
                        target,
                        component.clone(),
                        capability
                            .source_name()
                            .expect("capability with source path should have a name")
                            .clone(),
                        path.clone(),
                    ))),
                    _ => None,
                })
            }
            CapabilitySource::Namespace { capability, .. } => match capability.source_path() {
                Some(path) => {
                    Ok(Some(Box::new(NamespaceCapabilityProvider { path: path.clone() })))
                }
                _ => Ok(None),
            },
            CapabilitySource::FilteredAggregate { capability_provider, component, .. } => {
                // TODO(https://fxbug.dev/4776): This should cache the directory
                Ok(Some(Box::new(
                    FilteredAggregateServiceProvider::new(
                        component.clone(),
                        target,
                        capability_provider.clone(),
                    )
                    .await?,
                )))
            }
            CapabilitySource::AnonymizedAggregate {
                capability,
                component,
                aggregate_capability_provider,
                members,
            } => {
                let source_component_instance = component.upgrade()?;

                let route = AnonymizedServiceRoute {
                    source_moniker: source_component_instance.moniker.clone(),
                    members: members.clone(),
                    service_name: capability.source_name().clone(),
                };

                source_component_instance
                    .start(
                        &StartReason::AccessCapability {
                            target: target.moniker.clone(),
                            name: capability.source_name().clone(),
                        },
                        None,
                        vec![],
                        vec![],
                    )
                    .await?;

                // If there is an existing collection service directory, provide it.
                {
                    let state = source_component_instance.lock_resolved_state().await?;
                    if let Some(service_dir) = state.anonymized_services.get(&route) {
                        let provider = DirectoryEntryCapabilityProvider {
                            execution_scope: state.execution_scope().clone(),
                            entry: service_dir.dir_entry().await,
                        };
                        return Ok(Some(Box::new(provider)));
                    }
                }

                // Otherwise, create one. This must be done while the component ResolvedInstanceState
                // is unlocked because the AggregateCapabilityProvider uses locked state.
                let service_dir = Arc::new(AnonymizedAggregateServiceDir::new(
                    component.clone(),
                    route.clone(),
                    aggregate_capability_provider.clone_boxed(),
                ));

                source_component_instance.hooks.install(service_dir.hooks()).await;

                let provider = {
                    let mut state = source_component_instance.lock_resolved_state().await?;
                    let execution_scope = state.execution_scope().clone();
                    let entry = service_dir.dir_entry().await;

                    state.anonymized_services.insert(route, service_dir.clone());

                    DirectoryEntryCapabilityProvider { execution_scope, entry }
                };

                // Populate the service dir with service entries from children that may have been started before the service
                // capability had been routed from the collection.
                service_dir.add_entries_from_children().await?;

                Ok(Some(Box::new(provider)))
            }
            // These capabilities do not have a default provider.
            CapabilitySource::Framework { .. }
            | CapabilitySource::Void { .. }
            | CapabilitySource::Capability { .. }
            | CapabilitySource::Builtin { .. }
            | CapabilitySource::Environment { .. } => Ok(None),
        }
    }
}
