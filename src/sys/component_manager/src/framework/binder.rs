// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, FrameworkCapability, FrameworkCapabilityProvider},
        model::{
            component::{StartReason, WeakComponentInstance},
            error::ModelError,
            routing::report_routing_failure,
        },
    },
    async_trait::async_trait,
    cm_types::Name,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_component as fcomponent, fuchsia_zircon as zx,
    lazy_static::lazy_static,
    routing::capability_source::{ComponentCapability, InternalCapability},
    tracing::warn,
};

lazy_static! {
    static ref BINDER_SERVICE: Name = "fuchsia.component.Binder".parse().unwrap();
    static ref BINDER_CAPABILITY: ComponentCapability =
        ComponentCapability::Protocol(cm_rust::ProtocolDecl {
            name: BINDER_SERVICE.clone(),
            source_path: Some("/svc/fuchsia.component.Binder".parse().unwrap()),
        });
}

/// Implementation of `fuchsia.component.Binder` FIDL protocol.
struct BinderCapabilityProvider {
    source: WeakComponentInstance,
    target: WeakComponentInstance,
}

impl BinderCapabilityProvider {
    pub fn new(source: WeakComponentInstance, target: WeakComponentInstance) -> Self {
        Self { source, target }
    }

    async fn bind(self: Box<Self>, server_end: zx::Channel) -> Result<(), ()> {
        let source = match self.source.upgrade().map_err(|e| ModelError::from(e)) {
            Ok(source) => source,
            Err(err) => {
                report_routing_failure_to_target(self.target, err, server_end).await;
                return Err(());
            }
        };

        let start_reason = StartReason::AccessCapability {
            target: self.target.moniker.clone(),
            name: BINDER_SERVICE.clone(),
        };
        match source.start(&start_reason, None, vec![], vec![]).await {
            Ok(_) => {
                source.scope_to_runtime(server_end).await;
            }
            Err(err) => {
                report_routing_failure_to_target(self.target, err.into(), server_end).await;
                return Err(());
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FrameworkCapabilityProvider for BinderCapabilityProvider {
    type Marker = fcomponent::BinderMarker;
    async fn open_protocol(self: Box<Self>, server_end: ServerEnd<Self::Marker>) {
        let server_end = server_end.into_channel().into();
        let _ = self.bind(server_end).await;
    }
}

pub struct BinderFrameworkCapability {}

impl BinderFrameworkCapability {
    pub fn new() -> Self {
        Self {}
    }
}

impl FrameworkCapability for BinderFrameworkCapability {
    fn matches(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&BINDER_SERVICE)
    }

    fn new_provider(
        &self,
        scope: WeakComponentInstance,
        target: WeakComponentInstance,
    ) -> Box<dyn CapabilityProvider> {
        Box::new(BinderCapabilityProvider::new(scope, target))
    }
}

async fn report_routing_failure_to_target(
    target: WeakComponentInstance,
    err: ModelError,
    server_end: zx::Channel,
) {
    match target.upgrade().map_err(|e| ModelError::from(e)) {
        Ok(target) => {
            report_routing_failure(&target, &*BINDER_CAPABILITY, err, server_end).await;
        }
        Err(err) => {
            warn!(moniker=%target.moniker, error=%err, "failed to upgrade reference");
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            builtin_environment::BuiltinEnvironment,
            capability::CapabilityProvider,
            model::{
                events::{source::EventSource, stream::EventStream},
                hooks::EventType,
                testing::test_helpers::*,
            },
        },
        assert_matches::assert_matches,
        cm_rust::{self, ComponentDecl},
        cm_rust_testing::*,
        cm_util::TaskGroup,
        fidl::{client::Client, handle::AsyncChannel},
        fidl_fuchsia_io as fio, fuchsia_zircon as zx,
        futures::{lock::Mutex, StreamExt},
        moniker::{Moniker, MonikerBase},
        std::path::PathBuf,
        std::sync::Arc,
    };

    struct BinderCapabilityTestFixture {
        builtin_environment: Arc<Mutex<BuiltinEnvironment>>,
    }

    impl BinderCapabilityTestFixture {
        async fn new(components: Vec<(&'static str, ComponentDecl)>) -> Self {
            let TestModelResult { builtin_environment, .. } =
                TestEnvironmentBuilder::new().set_components(components).build().await;

            BinderCapabilityTestFixture { builtin_environment }
        }

        async fn new_event_stream(&self, events: Vec<Name>) -> (EventSource, EventStream) {
            new_event_stream(self.builtin_environment.clone(), events).await
        }

        async fn provider(
            &self,
            source: Moniker,
            target: Moniker,
        ) -> Box<BinderCapabilityProvider> {
            let builtin_environment = self.builtin_environment.lock().await;
            let source = builtin_environment
                .model
                .find_and_maybe_resolve(&source)
                .await
                .expect("failed to look up source moniker");
            let target = builtin_environment
                .model
                .find_and_maybe_resolve(&target)
                .await
                .expect("failed to look up target moniker");

            Box::new(BinderCapabilityProvider::new(
                WeakComponentInstance::new(&source),
                WeakComponentInstance::new(&target),
            ))
        }
    }

    #[fuchsia::test]
    async fn component_starts_on_open() {
        let fixture = BinderCapabilityTestFixture::new(vec![
            (
                "root",
                ComponentDeclBuilder::new()
                    .add_lazy_child("source")
                    .add_lazy_child("target")
                    .build(),
            ),
            ("source", component_decl_with_test_runner()),
            ("target", component_decl_with_test_runner()),
        ])
        .await;
        let (_event_source, mut event_stream) = fixture
            .new_event_stream(vec![EventType::Resolved.into(), EventType::Started.into()])
            .await;
        let (_client_end, mut server_end) = zx::Channel::create();
        let moniker: Moniker = vec!["source"].try_into().unwrap();

        let task_group = TaskGroup::new();
        fixture
            .provider(moniker.clone(), vec!["target"].try_into().unwrap())
            .await
            .open(task_group.clone(), fio::OpenFlags::empty(), PathBuf::new(), &mut server_end)
            .await
            .expect("failed to call open()");
        task_group.join().await;

        assert!(event_stream.wait_until(EventType::Resolved, moniker.clone()).await.is_some());
        assert!(event_stream.wait_until(EventType::Started, moniker.clone()).await.is_some());
    }

    // TODO(fxbug.dev/122226): Figure out a way to test this behavior.
    #[ignore]
    #[fuchsia::test]
    async fn channel_is_closed_if_component_does_not_exist() {
        let fixture = BinderCapabilityTestFixture::new(vec![(
            "root",
            ComponentDeclBuilder::new()
                .add_lazy_child("target")
                .add_lazy_child("unresolvable")
                .build(),
        )])
        .await;
        let (client_end, mut server_end) = zx::Channel::create();
        let moniker: Moniker = vec!["foo"].try_into().unwrap();

        let task_group = TaskGroup::new();
        fixture
            .provider(moniker, Moniker::root())
            .await
            .open(task_group.clone(), fio::OpenFlags::empty(), PathBuf::new(), &mut server_end)
            .await
            .expect("failed to call open()");
        task_group.join().await;

        let client_end =
            AsyncChannel::from_channel(client_end).expect("failed to create AsyncChanel");
        let client = Client::new(client_end, "binder_service");
        let mut event_receiver = client.take_event_receiver();
        assert_matches!(
            event_receiver.next().await,
            Some(Err(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                protocol_name: "binder_service"
            }))
        );
        assert_matches!(event_receiver.next().await, None);
    }
}
