// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use component_events::{
    events::{EventStream, ExitStatus, Stopped},
    matcher::EventMatcher,
};
use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fidl_fuchsia_hardware_power_statecontrol::{
    AdminMarker, AdminRequest, AdminRequestStream, RebootReason,
};
use fidl_fuchsia_io::FileProxy;
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
};
use fuchsia_fs::OpenFlags;
use futures::StreamExt;
use tracing::info;

// LINT.IfChange
const SYSRQ_PANIC_MESSAGE: &str = "crashing from SysRq";
// LINT.ThenChange(src/starnix/kernel/fs/proc/sysrq.rs)

#[fuchsia::test]
async fn c_crash() {
    let mut events = EventStream::open().await.unwrap();
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new()
            .realm_name("c_crash")
            .from_relative_url("#meta/kernel_with_container.cm"),
    )
    .await
    .unwrap();

    info!("starting realm");
    let kernel_with_container = builder.build().await.unwrap();
    let realm_moniker = format!("realm_builder:{}", kernel_with_container.root.child_name());
    info!(%realm_moniker, "started");
    let container_moniker = format!("{realm_moniker}/debian_container");
    let kernel_moniker = format!("{realm_moniker}/kernel");

    let mut kernel_logs = ArchiveReader::new()
        .select_all_for_moniker(&kernel_moniker)
        .snapshot_then_subscribe::<Logs>()
        .unwrap();

    // Open sysrq-trigger to start the kernel, then make sure we see its logs.
    let sysrq = open_sysrq_trigger(&kernel_with_container).await;
    let first_kernel_log = kernel_logs.next().await.unwrap().unwrap();
    info!(?first_kernel_log, "receiving logs from starnix kernel now that it's started");

    info!("writing c to sysrq");
    fuchsia_fs::file::write(&sysrq, "c")
        .await
        .expect_err("kernel should close channel before replying");

    assert_matches!(
        wait_for_exit_status(&mut events, &[&container_moniker, &kernel_moniker]).await,
        ExitStatus::Crash(..)
    );

    info!("looking for panic message");
    let kernel_panic_msg = loop {
        let next = kernel_logs
            .next()
            .await
            .expect("must see desired messages before end")
            .expect("must not see errors in stream");
        if next.severity() != Severity::Error {
            continue;
        }
        if let Some(m) = next.msg() {
            if m.contains("STARNIX KERNEL PANIC") {
                info!(?next, "found panic message");
                break next;
            }
        }
    };

    let panic_keys = kernel_panic_msg.payload_keys().expect("should have structured k/v pairs");
    let panic_info = panic_keys.get_property("info").expect("panic info is under `info` key");
    let panic_info = panic_info.string().expect("panic info stored as a string");
    assert!(
        panic_info.ends_with(SYSRQ_PANIC_MESSAGE),
        "\"{panic_info}\" must end with \"{SYSRQ_PANIC_MESSAGE}\""
    );
}

#[fuchsia::test]
async fn c_reboot() {
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new()
            .realm_name("c_reboot")
            .from_relative_url("#meta/kernel_with_container.cm"),
    )
    .await
    .unwrap();

    let (admin_send, mut admin_requests) = futures::channel::mpsc::unbounded();
    let power_admin_mock = builder
        .add_local_child(
            "power_admin",
            move |handles| {
                let admin_send = admin_send.clone();
                Box::pin(async move {
                    let mut fs = ServiceFs::new();
                    fs.serve_connection(handles.outgoing_dir).unwrap();
                    fs.dir("svc").add_fidl_service(|h: AdminRequestStream| Ok(h));
                    fs.forward(admin_send).await.unwrap();
                    Ok(())
                })
            },
            ChildOptions::new(),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<AdminMarker>())
                .from(&power_admin_mock)
                .to(Ref::child("kernel")),
        )
        .await
        .unwrap();
    info!("starting realm");
    let kernel_with_container = builder.build().await.unwrap();
    let realm_moniker = format!("realm_builder:{}", kernel_with_container.root.child_name());
    info!(%realm_moniker, "started");

    let sysrq = open_sysrq_trigger(&kernel_with_container).await;

    // Spawn a task to send the initial message, without blocking for a return since this won't.
    let _writer = Task::spawn(async move {
        info!("writing c to sysrq");
        fuchsia_fs::file::write(&sysrq, "c").await
    });

    info!("waiting for power admin request");
    let mut admin_client = admin_requests.next().await.unwrap();
    assert_matches!(
        admin_client.next().await.unwrap().unwrap(),
        AdminRequest::Reboot { reason: RebootReason::CriticalComponentFailure, .. }
    );
}

async fn open_sysrq_trigger(realm: &RealmInstance) -> FileProxy {
    info!("opening sysrq trigger");
    // Some clients of the file[0] truncate it on open[1] despite not having contents.
    // [0] https://cs.android.com/android/platform/superproject/main/+/main:system/core/init/reboot.cpp;l=391;drc=97047b54e952e2d08b10e6d37d510ca653cace00
    // [1] https://cs.android.com/android/platform/superproject/main/+/main:system/libbase/file.cpp;l=274;drc=4b992a8da56ea5777f9364033a85ad89af680e10
    let flags = OpenFlags::RIGHT_WRITABLE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    fuchsia_fs::directory::open_file(
        realm.root.get_exposed_dir(),
        "/fs_root/proc/sysrq-trigger",
        flags,
    )
    .await
    .unwrap()
}

async fn wait_for_exit_status(events: &mut EventStream, monikers: &[&str]) -> ExitStatus {
    info!(monikers = %monikers.join(","), "waiting for exit status");
    EventMatcher::ok()
        .monikers(monikers)
        .wait::<Stopped>(events)
        .await
        .unwrap()
        .result()
        .unwrap()
        .status
}
