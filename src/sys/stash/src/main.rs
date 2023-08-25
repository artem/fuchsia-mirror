// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(missing_docs)]

//! `stash` provides key/value storage to components.

use anyhow::{format_err, Context as _, Error};
use fidl::prelude::*;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{self as inspect, health::Reporter};
use futures::lock::Mutex;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use std::convert::{TryFrom, TryInto};
use std::env;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use tracing::error;

mod accessor;
mod instance;
mod store;

use fidl_fuchsia_stash::{
    SecureStoreMarker, Store2Marker, StoreMarker, StoreRequest, StoreRequestStream,
};

#[derive(Debug, PartialEq)]
struct StashSettings {
    backing_file: String,
    secure_mode: bool,
    secondary_store: bool,
}

impl Default for StashSettings {
    fn default() -> StashSettings {
        StashSettings {
            backing_file: "/data/stash.store".to_string(),
            secure_mode: false,
            secondary_store: false,
        }
    }
}

impl TryFrom<Vec<String>> for StashSettings {
    type Error = anyhow::Error;
    fn try_from(args: Vec<String>) -> Result<StashSettings, Error> {
        // ignore args[0]
        let mut iter = args.iter().skip(1);
        let mut res = StashSettings::default();
        while let Some(flag) = iter.next() {
            match flag.as_str() {
                "--backing_file" => {
                    if let Some(f) = iter.next() {
                        res.backing_file = f.to_string();
                    } else {
                        return Err(format_err!("Must specify argument to --backing_file"));
                    }
                }
                "--secure" => res.secure_mode = true,
                "--secondary_store" => res.secondary_store = true,
                _ => return Err(format_err!("Unknown flag: {}", flag)),
            }
        }
        if res.secure_mode && res.secondary_store {
            return Err(format_err!("--secure and --secondary_store are mutually exclusive"));
        }
        return Ok(res);
    }
}

#[fuchsia::main(logging_tags = ["stash"])]
async fn main() -> Result<(), Error> {
    let r_opts: Result<StashSettings, Error> = env::args().collect::<Vec<String>>().try_into();

    match r_opts {
        Err(msg) => {
            println!("{}", msg);
            print_help();
            process::exit(1);
        }
        Ok(opts) => {
            let store_manager =
                Arc::new(Mutex::new(store::StoreManager::new(PathBuf::from(&opts.backing_file))?));

            let name = if opts.secure_mode {
                SecureStoreMarker::PROTOCOL_NAME
            } else if opts.secondary_store {
                Store2Marker::PROTOCOL_NAME
            } else {
                StoreMarker::PROTOCOL_NAME
            };
            inspect::component::inspector().root().record_bool("secure_mode", opts.secure_mode);

            inspect::component::health().set_starting_up();
            let _inspect_server_task = inspect_runtime::publish(
                inspect::component::inspector(),
                inspect_runtime::PublishOptions::default(),
            );

            let mut fs = ServiceFs::new();
            fs.dir("svc").add_fidl_service_at(name, |stream| {
                stash_server(store_manager.clone(), !opts.secure_mode, stream)
            });

            fs.take_and_serve_directory_handle()?;
            inspect::component::health().set_ok();
            fs.collect::<()>().await;
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        r"stash
garnet service for storing key/value pairs

USAGE:
    stash [FLAGS]

FLAGS:
        --secure                Disables support for handling raw bytes. This flag Should be used
                                when running in critical path of verified boot.
        --secondary_store       Serves fuchsia.store.Store2 instead of fuchsia.store.Store.
                                Specifying both --secondary_store and --secure is an error.
        --backing_file <FILE>   location of backing file for the store"
    )
}

fn stash_server(
    store_manager: Arc<Mutex<store::StoreManager>>,
    enable_bytes: bool,
    mut stream: StoreRequestStream,
) {
    fasync::Task::spawn(
        async move {
            let mut state = instance::Instance {
                client_name: None,
                enable_bytes: enable_bytes,
                store_manager: store_manager,
            };

            while let Some(req) = stream.try_next().await.context("error running stash server")? {
                match req {
                    StoreRequest::Identify { name, control_handle } => {
                        if let Err(e) = state.identify(name.clone()) {
                            control_handle.shutdown();
                            return Err(e);
                        }
                    }
                    StoreRequest::CreateAccessor {
                        read_only,
                        control_handle,
                        accessor_request,
                    } => {
                        if let Err(e) = state.create_accessor(read_only, accessor_request) {
                            control_handle.shutdown();
                            return Err(e);
                        }
                    }
                }
            }
            Ok(())
        }
        .unwrap_or_else(|err: anyhow::Error| error!(?err, "couldn't run stash service")),
    )
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_to_settings(args: &str) -> Result<StashSettings, Error> {
        args.split_whitespace().map(|s| s.to_string()).collect::<Vec<String>>().try_into()
    }

    fn assert_args_to_settings(args: &str, settings: StashSettings) {
        assert_eq!(args_to_settings(args).unwrap(), settings);
    }

    fn assert_args_error(args: &str) {
        assert!(args_to_settings(args).is_err());
    }

    #[test]
    fn test_args() {
        assert_args_to_settings("", StashSettings::default());
        assert_args_to_settings("stash", StashSettings::default());
        assert_args_to_settings(
            "stash --secure",
            StashSettings { secure_mode: true, ..Default::default() },
        );
        assert_args_to_settings(
            "stash --secondary_store",
            StashSettings { secondary_store: true, ..Default::default() },
        );
        assert_args_to_settings(
            "stash --backing_file foo",
            StashSettings { backing_file: "foo".to_string(), ..Default::default() },
        );
        assert_args_to_settings(
            "stash --secure --backing_file foo",
            StashSettings {
                secure_mode: true,
                backing_file: "foo".to_string(),
                ..Default::default()
            },
        );
        assert_args_to_settings(
            "stash --secondary_store --backing_file foo",
            StashSettings {
                secondary_store: true,
                backing_file: "foo".to_string(),
                ..Default::default()
            },
        );
        assert_args_error("stash --secure --secondary_store");
        assert_args_error("stash --backing_file");
        assert_args_error("stash --secure --secondary_store --backing_file");
    }
}
