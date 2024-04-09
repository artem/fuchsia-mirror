// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use attribution::AttributionServer;
use fidl::endpoints::RequestStream;
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_memory_attribution as fattribution;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, HandleBased};
use futures::TryStreamExt;
use std::sync::Arc;
use zx::AsHandleRef;

use crate::{component::ElfComponentInfo, ComponentSet};

pub struct MemoryReporter {
    server: AttributionServer,
    components: Arc<ComponentSet>,
}

impl Drop for MemoryReporter {
    fn drop(&mut self) {
        self.components.set_callbacks(None, None);
    }
}

impl MemoryReporter {
    pub(crate) fn new(components: Arc<ComponentSet>) -> MemoryReporter {
        let components_clone = components.clone();
        let server = AttributionServer::new(Box::new(move || {
            MemoryReporter::get_attribution(components_clone.as_ref())
        }));
        let new_component_publisher = server.new_publisher();
        let deleted_component_publisher = server.new_publisher();
        components.set_callbacks(
            Some(Box::new(move |info| {
                new_component_publisher.on_update(Self::build_new_attribution(info)).unwrap();
            })),
            Some(Box::new(move |token| {
                deleted_component_publisher
                    .on_update(vec![fattribution::AttributionUpdate::Remove(
                        fattribution::Identifier::Component(
                            token.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                        ),
                    )])
                    .unwrap();
            })),
        );
        MemoryReporter { server, components }
    }

    fn get_attribution(components: &ComponentSet) -> Vec<fattribution::AttributionUpdate> {
        let mut attributions: Vec<fattribution::AttributionUpdate> = vec![];
        components.visit(|component: &ElfComponentInfo, _id| {
            let mut component_attributions = Self::build_new_attribution(component);
            attributions.append(&mut component_attributions);
        });
        attributions
    }

    pub fn serve(&self, mut stream: fattribution::ProviderRequestStream) {
        let subscriber = self.server.new_observer(stream.control_handle());
        fasync::Task::spawn(async move {
            while let Ok(Some(request)) = stream.try_next().await {
                match request {
                    fattribution::ProviderRequest::Get { responder } => {
                        subscriber.next(responder);
                    }
                    fattribution::ProviderRequest::_UnknownMethod {
                        ordinal,
                        control_handle,
                        ..
                    } => {
                        tracing::error!("Invalid request to AttributionProvider: {ordinal}");
                        control_handle.shutdown_with_epitaph(zx::Status::INVALID_ARGS);
                    }
                }
            }
        })
        .detach();
    }

    fn build_new_attribution(component: &ElfComponentInfo) -> Vec<fattribution::AttributionUpdate> {
        let new_principal = fattribution::NewPrincipal {
            identifier: Some(fattribution::Identifier::Component(
                component.copy_instance_token().unwrap(),
            )),
            type_: Some(fattribution::Type::Runnable),
            detailed_attribution: component.get_outgoing_directory().and_then(
                |outgoing_directory| {
                    let (server, client) = fidl::Channel::create();
                    fdio::open_at(
                        outgoing_directory.channel(),
                        &format!("svc/{}", fattribution::ProviderMarker::PROTOCOL_NAME),
                        fio::OpenFlags::empty(),
                        server,
                    )
                    .unwrap();
                    let provider =
                        fidl::endpoints::ClientEnd::<fattribution::ProviderMarker>::new(client);
                    Some(provider)
                },
            ),
            ..Default::default()
        };
        let attribution = fattribution::UpdatedPrincipal {
            identifier: Some(fattribution::Identifier::Component(
                component.copy_instance_token().unwrap(),
            )),
            resources: Some(fattribution::Resources::Data(vec![
                fattribution::Resource::KernelObject(
                    component.copy_job().proc().get_koid().unwrap().raw_koid(),
                ),
            ])),
            ..Default::default()
        };
        vec![
            fattribution::AttributionUpdate::Add(new_principal),
            fattribution::AttributionUpdate::Update(attribution),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{lifecycle_startinfo, new_elf_runner_for_test};
    use cm_config::SecurityPolicy;
    use fidl_fuchsia_component_runner as fcrunner;
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use futures::FutureExt;
    use moniker::Moniker;
    use routing::policy::ScopedPolicyChecker;

    /// Test that the ELF runner can tell us about the resources used by the component it runs.
    #[test]
    fn test_attribute_memory() {
        let mut exec = fasync::TestExecutor::new();
        let (_runtime_dir, runtime_dir_server) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let start_info = lifecycle_startinfo(runtime_dir_server);

        let runner = new_elf_runner_for_test();
        let (snapshot_provider, snapshot_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fattribution::ProviderMarker>().unwrap();
        runner.serve_memory_reporter(snapshot_request_stream);

        // Run a component.
        let runner = runner.get_scoped_runner(ScopedPolicyChecker::new(
            Arc::new(SecurityPolicy::default()),
            Moniker::try_from("foo/bar").unwrap(),
        ));
        let (_controller, server_controller) =
            fidl::endpoints::create_proxy::<fcrunner::ComponentControllerMarker>()
                .expect("could not create component controller endpoints");
        exec.run_singlethreaded(&mut runner.start(start_info, server_controller).boxed());

        // Ask about the memory usage of components.
        let attributions =
            exec.run_singlethreaded(snapshot_provider.get()).unwrap().unwrap().attributions;
        assert!(attributions.is_some());

        let attributions_vec = attributions.unwrap();
        // It should contain one component, the one we just launched.
        assert_eq!(attributions_vec.len(), 2);
        let new_attrib = attributions_vec.get(0).unwrap();
        let fattribution::AttributionUpdate::Add(added_principal) = new_attrib else {
            panic!("Not a new principal");
        };
        assert_eq!(added_principal.type_, Some(fattribution::Type::Runnable));

        // Its resource is a single job.
        let update_attrib = attributions_vec.get(1).unwrap();
        let fattribution::AttributionUpdate::Update(_) = update_attrib else {
            panic!("Not an update");
        };
    }
}
