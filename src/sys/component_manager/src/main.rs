// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO Follow 2018 idioms
#![allow(elided_lifetimes_in_paths)]
// This is needed for the pseudo_directory nesting in crate::model::tests
#![recursion_limit = "256"]
// Printing to stdout and stderr directly is discouraged for component_manager.
// Instead, the tracing library, e.g. through macros like `info!`, and `error!`,
// should be used.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr,))]

use {
    crate::{
        bootfs::BootfsSvc,
        builtin_environment::{BuiltinEnvironment, BuiltinEnvironmentBuilder},
    },
    ::cm_logger::klog,
    anyhow::Error,
    cm_config::RuntimeConfig,
    fidl_fuchsia_component_internal as finternal, fuchsia_async as fasync,
    fuchsia_runtime::{job_default, process_self},
    fuchsia_zircon::JobCriticalOptions,
    std::path::PathBuf,
    std::{panic, process},
    tracing::{error, info},
};

mod bedrock;
mod bootfs;
mod builtin;
mod builtin_environment;
mod capability;
mod constants;
mod diagnostics;
mod directory_ready_notifier;
mod elf_runner;
mod framework;
mod inspect_sink_provider;
mod model;
mod root_stop_notifier;
mod runner;
mod sandbox_util;
mod startup;

extern "C" {
    fn dl_set_loader_service(
        handle: fuchsia_zircon::sys::zx_handle_t,
    ) -> fuchsia_zircon::sys::zx_handle_t;
}

fn main() {
    // Set ourselves as critical to our job. If we do not fail gracefully, our
    // job will be killed.
    if let Err(err) =
        job_default().set_critical(JobCriticalOptions::RETCODE_NONZERO, &process_self())
    {
        panic!("Component manager failed to set itself as critical: {}", err);
    }

    // Close any loader service passed to component manager so that the service session can be
    // freed, as component manager won't make use of a loader service such as by calling dlopen.
    // If userboot invoked component manager directly, this service was the only reason userboot
    // continued to run and closing it will let userboot terminate.
    let ldsvc = unsafe {
        fuchsia_zircon::Handle::from_raw(dl_set_loader_service(
            fuchsia_zircon::sys::ZX_HANDLE_INVALID,
        ))
    };
    drop(ldsvc);

    let args = startup::Arguments::from_args()
        .unwrap_or_else(|err| panic!("{}\n{}", err, startup::Arguments::usage()));
    let (runtime_config, bootfs_svc) = build_runtime_config(&args);
    let mut executor = fasync::SendExecutor::new(runtime_config.num_threads);

    match runtime_config.log_destination {
        finternal::LogDestination::Syslog => {
            diagnostics_log::initialize(diagnostics_log::PublishOptions::default()).unwrap();
        }
        finternal::LogDestination::Klog => {
            klog::KernelLogger::init();
        }
    };

    info!("Component manager is starting up...");
    if args.boot {
        info!("Component manager was started with boot defaults");
    }

    let run_root_fut = async move {
        let mut builtin_environment =
            match build_environment(&args, runtime_config, bootfs_svc).await {
                Ok(environment) => environment,
                Err(error) => {
                    error!(%error, "Component manager setup failed");
                    process::exit(1);
                }
            };

        if let Err(error) = builtin_environment.run_root().await {
            error!(%error, "Failed to start root component");
            process::exit(1);
        }
    };

    executor.run(run_root_fut);
}

/// Loads component_manager's config.
///
/// This function panics on failure because the logger is not initialized yet.
fn build_runtime_config(args: &startup::Arguments) -> (RuntimeConfig, Option<BootfsSvc>) {
    let bootfs_svc =
        args.host_bootfs.then(|| BootfsSvc::new().expect("Failed to create Rust bootfs"));
    let config_bytes = if let Some(ref bootfs_svc) = bootfs_svc {
        // The Rust bootfs VFS has not been brought up yet, so to find the component manager's
        // config we must find the config's offset and size in the bootfs VMO, and read from it
        // directly.
        let canonicalized =
            if args.config.starts_with("/boot/") { &args.config[6..] } else { &args.config };
        bootfs_svc.read_config_from_uninitialized_vfs(canonicalized).unwrap_or_else(|err| {
            panic!("Failed to read config from uninitialized vfs with error {}.", err)
        })
    } else {
        // This is the legacy path where bootsvc is hosting a C++ bootfs VFS,
        // and component manager can read its config using standard filesystem APIs.
        let path = PathBuf::from(&args.config);
        std::fs::read(path).expect("failed to read config file")
    };

    let mut config = RuntimeConfig::new_from_bytes(&config_bytes)
        .unwrap_or_else(|err| panic!("Failed to load runtime config: {}", err));

    match (config.root_component_url.as_ref(), args.root_component_url.as_ref()) {
        (Some(_url), None) => (config, bootfs_svc),
        (None, Some(url)) => {
            config.root_component_url = Some(url.clone());
            (config, bootfs_svc)
        }
        (None, None) => {
            panic!(
                "`root_component_url` not provided. This field must be provided either as a \
                command line argument or config file parameter."
            );
        }
        (Some(_), Some(_)) => {
            panic!(
                "`root_component_url` set in two places: as a command line argument \
                and a config file parameter. This field can only be set in one of those places."
            );
        }
    }
}

async fn build_environment(
    args: &startup::Arguments,
    config: RuntimeConfig,
    bootfs_svc: Option<BootfsSvc>,
) -> Result<BuiltinEnvironment, Error> {
    let mut builder = BuiltinEnvironmentBuilder::new()
        .set_runtime_config(config)
        .create_utc_clock(&bootfs_svc)
        .await?
        .add_elf_runner()?
        .include_namespace_resolvers();

    if let Some(bootfs_svc) = bootfs_svc {
        builder = builder.set_bootfs_svc(bootfs_svc);
    }

    if args.add_builtin_runner {
        builder = builder.add_builtin_runner()?;
    }

    builder.build().await
}
