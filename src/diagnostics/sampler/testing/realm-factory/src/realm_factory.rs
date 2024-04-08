// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mocks;
use anyhow::*;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics as fdiagnostics;
use fidl_fuchsia_hardware_power_statecontrol as fpower;
use fidl_fuchsia_inspect as finspect;
use fidl_fuchsia_logger as flogger;
use fidl_fuchsia_metrics as fmetrics;
use fidl_fuchsia_metrics_test as fmetrics_test;
use fidl_fuchsia_mockrebootcontroller as fmockrebootcontroller;
use fidl_fuchsia_samplertestcontroller as fsamplertestcontroller;
use fidl_test_sampler as ftest;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
};
use futures::{channel::mpsc, lock::Mutex, StreamExt};
use std::sync::Arc;

const MOCK_COBALT_URL: &str = "#meta/mock_cobalt.cm";
const SINGLE_COUNTER_URL: &str = "#meta/single_counter_test_component.cm";
const SAMPLER_URL: &str = "#meta/sampler.cm";
const ARCHIVIST_URL: &str = "#meta/archivist-for-embedding.cm";
const SAMPLER_BINDER_ALIAS: &str = "fuchsia.component.SamplerBinder";

pub async fn create_realm(options: ftest::RealmOptions) -> Result<RealmInstance, Error> {
    let sampler_component_name =
        options.sampler_component_name.as_ref().map(|s| s.as_str()).unwrap_or("sampler");
    let single_counter_name =
        options.single_counter_name.as_ref().map(|s| s.as_str()).unwrap_or("single_counter");
    let mock_cobalt_name =
        options.mock_cobalt_name.as_ref().map(|s| s.as_str()).unwrap_or("mock_cobalt");
    let test_archivist_name =
        options.test_archivist_name.as_ref().map(|s| s.as_str()).unwrap_or("test_case_archivist");
    let builder = RealmBuilder::new().await?;
    let mocks_server = builder
        .add_local_child(
            "mocks-server",
            move |handles| Box::pin(serve_mocks(handles)),
            ChildOptions::new(),
        )
        .await?;
    let wrapper_realm = builder.add_child_realm("wrapper", ChildOptions::new()).await?;
    let mock_cobalt =
        wrapper_realm.add_child(mock_cobalt_name, MOCK_COBALT_URL, ChildOptions::new()).await?;
    let single_counter = wrapper_realm
        .add_child(single_counter_name, SINGLE_COUNTER_URL, ChildOptions::new())
        .await?;
    let sampler =
        wrapper_realm.add_child(sampler_component_name, SAMPLER_URL, ChildOptions::new()).await?;
    let test_case_archivist =
        wrapper_realm.add_child(test_archivist_name, ARCHIVIST_URL, ChildOptions::new()).await?;

    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fmetrics_test::MetricEventLoggerQuerierMarker>())
                .from(&mock_cobalt)
                .to(Ref::parent()),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<
                    fsamplertestcontroller::SamplerTestControllerMarker,
                >())
                .from(&single_counter)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fmetrics_test::MetricEventLoggerQuerierMarker>())
                .capability(Capability::protocol::<
                    fsamplertestcontroller::SamplerTestControllerMarker,
                >())
                .from(&wrapper_realm)
                .to(Ref::parent()),
        )
        .await?;

    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fmetrics::MetricEventLoggerFactoryMarker>())
                .from(&mock_cobalt)
                .to(&sampler),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fpower::RebootMethodsWatcherRegisterMarker>())
                .from(&mocks_server)
                .to(&wrapper_realm),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fpower::RebootMethodsWatcherRegisterMarker>())
                .from(Ref::parent())
                .to(&sampler),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<fmockrebootcontroller::MockRebootControllerMarker>(),
                )
                .from(&mocks_server)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<flogger::LogSinkMarker>())
                .from(Ref::parent())
                .to(&wrapper_realm),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<flogger::LogSinkMarker>())
                .from(Ref::parent())
                .to(&test_case_archivist)
                .to(&mock_cobalt)
                .to(&sampler)
                .to(&single_counter),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<finspect::InspectSinkMarker>())
                .from(&test_case_archivist)
                .to(&mock_cobalt)
                .to(&sampler)
                .to(&single_counter),
        )
        .await?;
    // TODO(https://fxbug.dev/42156520): refactor these tests to use the single test archivist and remove
    // this archivist. We can also remove the `wrapper` realm when this is done. The
    // ArchiveAccessor and Log protocols routed here would be routed from AboveRoot instead. To
    // do so, uncomment the following routes and delete all the routes after this comment
    // involving "wrapper/test_case_archivist":
    // .add_route(RouteBuilder::protocol("fuchsia.diagnostics.ArchiveAccessor")
    //     .source(RouteEndpoint::AboveRoot)
    //     .targets(vec![RouteEndpoint::component("wrapper/sampler")])
    // }).await?
    // .add_route(RouteBuilder::protocol("fuchsia.logger.Log")
    //     .source(RouteEndpoint::AboveRoot)
    //     .targets(vec![RouteEndpoint::component("wrapper/sampler")])
    // }).await?
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::event_stream("capability_requested").with_scope(&wrapper_realm),
                )
                .from(Ref::parent())
                .to(&wrapper_realm),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fdiagnostics::ArchiveAccessorMarker>())
                .capability(Capability::protocol::<flogger::LogMarker>())
                .from(&test_case_archivist)
                .to(&sampler),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fdiagnostics::ArchiveAccessorMarker>())
                .from(&test_case_archivist)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fdiagnostics::ArchiveAccessorMarker>())
                .from(&wrapper_realm)
                .to(Ref::parent()),
        )
        .await?;
    wrapper_realm
        .add_route(
            Route::new()
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&test_case_archivist),
        )
        .await?;

    wrapper_realm
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<fcomponent::BinderMarker>().as_(SAMPLER_BINDER_ALIAS),
                )
                .from(&sampler)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(SAMPLER_BINDER_ALIAS))
                .from(&wrapper_realm)
                .to(Ref::parent()),
        )
        .await?;

    let instance = builder.build().await?;

    Ok(instance)
}

async fn serve_mocks(handles: LocalComponentHandles) -> Result<(), Error> {
    let mut fs = ServiceFs::new();

    let (snd, rcv) = mpsc::channel(1);
    let rcv = Arc::new(Mutex::new(rcv));

    fs.dir("svc")
        .add_fidl_service(move |stream| {
            mocks::serve_reboot_server(stream, snd.clone());
        })
        .add_fidl_service(move |stream| {
            mocks::serve_reboot_controller(stream, rcv.clone());
        });
    fs.serve_connection(handles.outgoing_dir)?;
    fs.collect::<()>().await;
    Ok(())
}
