// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A networking stack.
#![deny(clippy::unused_async)]
#![deny(missing_docs, unreachable_patterns, unused)]
#![recursion_limit = "256"]

#[cfg(feature = "instrumented")]
extern crate netstack3_core_instrumented as netstack3_core;

mod bindings;

use argh::FromArgs;
use fuchsia_component::server::{ServiceFs, ServiceFsDir};

use bindings::{NetstackSeed, Service};

/// Netstack3.
///
/// The networking stack for Fuchsia.
#[derive(FromArgs)]
struct Options {
    /// run with debug logging.
    #[argh(switch)]
    debug: bool,
}

/// Runs Netstack3.
pub fn main() {
    // TOOD(https://fxbug.dev/125388): Support running with multiple threads.
    // This is currently blocked on fixing race conditions when concurrent
    // operations are allowed.
    let mut executor = fuchsia_async::SendExecutor::new(1 /* num_threads */);

    let Options { debug } = argh::from_env();
    let mut log_options = diagnostics_log::PublishOptions::default();
    if debug {
        log_options = log_options.minimum_severity(diagnostics_log::Severity::Debug);
    }
    diagnostics_log::initialize(log_options).expect("failed to initialize log");

    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let mut fs = ServiceFs::new();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        // TODO(https://fxbug.dev/125773): This is transitional. Once the
        // out-of-stack DHCP client is being used by both netstacks, it
        // should be moved out of the netstack realm and into the network
        // realm. The trip through Netstack3 allows for availability of DHCP
        // client to be dependent on Netstack version when using
        // netstack-proxy.
        .add_proxy_service::<fidl_fuchsia_net_dhcp::ClientProviderMarker, _>()
        .add_service_connector(Service::DebugDiagnostics)
        .add_fidl_service(Service::DebugInterfaces)
        .add_fidl_service(Service::DnsServerWatcher)
        .add_fidl_service(Service::Stack)
        .add_fidl_service(Service::Socket)
        .add_fidl_service(Service::PacketSocket)
        .add_fidl_service(Service::RawSocket)
        .add_fidl_service(Service::RootInterfaces)
        .add_fidl_service(Service::RoutesState)
        .add_fidl_service(Service::RoutesStateV4)
        .add_fidl_service(Service::RoutesStateV6)
        .add_fidl_service(Service::RoutesAdminV4)
        .add_fidl_service(Service::RoutesAdminV6)
        .add_fidl_service(Service::RootRoutesV4)
        .add_fidl_service(Service::RootRoutesV6)
        .add_fidl_service(Service::Interfaces)
        .add_fidl_service(Service::InterfacesAdmin)
        .add_fidl_service(Service::Filter)
        .add_fidl_service(Service::Neighbor)
        .add_fidl_service(Service::NeighborController)
        .add_fidl_service(Service::Verifier);

    let seed = NetstackSeed::default();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default())
            .expect("publish Inspect task");

    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle().expect("directory handle");

    executor.run(seed.serve(fs, inspector))
}
