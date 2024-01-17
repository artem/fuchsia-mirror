// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::ControlHandle,
    fidl_fuchsia_logger::LogSinkMarker,
    fidl_fuchsia_scheduler::ProfileProviderMarker,
    fidl_fuchsia_sysmem::AllocatorMarker,
    fidl_fuchsia_tracing_provider::RegistryMarker,
    fidl_fuchsia_ui_composition::FlatlandMarker,
    fidl_fuchsia_ui_display_singleton::InfoMarker,
    fidl_fuchsia_ui_focus::FocusChainListenerRegistryMarker,
    fidl_fuchsia_ui_input3::KeyboardMarker,
    fidl_fuchsia_ui_test_context as ui_test_context, fidl_fuchsia_ui_test_input as ui_input,
    fidl_fuchsia_ui_test_scene as test_scene,
    fidl_fuchsia_vulkan_loader::LoaderMarker,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_zircon_status as zx_status,
    futures::{StreamExt, TryStreamExt},
};

// TODO(https://fxbug.dev/117852): Use subpackages here.
const TEST_UI_STACK: &str = "ui";
const TEST_UI_STACK_URL: &str = "#meta/test-ui-stack.cm";

/// All FIDL services that are exposed by this component's ServiceFs.
enum Service {
    RealmFactoryServer(ui_test_context::RealmFactoryRequestStream),
}

#[fuchsia::main(logging_tags = ["ui_launcher"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Service::RealmFactoryServer);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |conn| async move {
        match conn {
            Service::RealmFactoryServer(stream) => run_realm_factory_server(stream).await,
        }
    })
    .await;
    Ok(())
}

async fn run_realm_factory_server(stream: ui_test_context::RealmFactoryRequestStream) {
    stream
        .try_for_each_concurrent(None, |request| async {
            tracing::debug!("received a request: {:?}", &request);
            let mut task_group = fasync::TaskGroup::new();
            match request {
                ui_test_context::RealmFactoryRequest::CreateRealm { payload, responder } => {
                    let realm_server = payload.realm_server.expect("missing realm_server");

                    // Create the puppet + ui stack for this test context.
                    let realm = assemble_puppet_realm().await;

                    let request_stream = realm_server.into_stream().expect("into stream");
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream)
                            .await
                            .expect("invalid realm proxy server");
                    });
                    responder.send(Ok(())).expect("failed to response");
                }
                ui_test_context::RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    tracing::warn!("realm factory receive an unknown request");
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
            }

            task_group.join().await;
            Ok(())
        })
        .await
        .expect("failed to serve test realm factory request stream");
}

async fn assemble_puppet_realm() -> RealmInstance {
    let builder = RealmBuilder::new().await.expect("Failed to create RealmBuilder.");

    // Add test UI stack component.
    builder
        .add_child(TEST_UI_STACK, TEST_UI_STACK_URL, ChildOptions::new())
        .await
        .expect("Failed to add UI realm.");

    // Route capabilities to the test UI stack.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .capability(Capability::protocol::<ProfileProviderMarker>())
                .capability(Capability::protocol::<AllocatorMarker>())
                .capability(Capability::protocol::<LoaderMarker>())
                .capability(Capability::protocol::<RegistryMarker>())
                .from(Ref::parent())
                .to(Ref::child(TEST_UI_STACK)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Expose UI capabilities.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<test_scene::ControllerMarker>())
                .capability(Capability::protocol::<FlatlandMarker>())
                .capability(Capability::protocol::<KeyboardMarker>())
                .capability(Capability::protocol::<InfoMarker>())
                .capability(Capability::protocol::<FocusChainListenerRegistryMarker>())
                .capability(Capability::protocol::<ui_input::RegistryMarker>())
                .from(Ref::child(TEST_UI_STACK))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    builder
        .add_route(
            Route::new()
                .capability(Capability::storage("tmp"))
                .from(Ref::parent())
                .to(Ref::child(TEST_UI_STACK)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Create the test realm.
    builder.build().await.expect("Failed to create test realm.")
}
