// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! fuchsia io conformance testing harness for the rust pseudo-fs-mt library

use {
    anyhow::{anyhow, Context as _, Error},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_io_test::{
        self as io_test, Io1Config, Io1HarnessRequest, Io1HarnessRequestStream,
    },
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
    std::sync::Arc,
    tracing::error,
    vfs::{
        directory::{
            entry_container::Directory,
            helper::DirectlyMutable,
            mutable::{connection::MutableConnection, simple},
            simple::Simple,
        },
        execution_scope::ExecutionScope,
        file::vmo,
        path::Path,
        remote::remote_dir,
    },
};

struct Harness(Io1HarnessRequestStream);

const HARNESS_EXEC_PATH: &'static str = "/pkg/bin/io_conformance_harness_rustvfs";

/// Creates and returns a Rust VFS VmoFile-backed executable file using the contents of the
/// conformance test harness binary itself.
fn new_executable_file() -> Result<Arc<vmo::VmoFile>, Error> {
    let file = fdio::open_fd(
        HARNESS_EXEC_PATH,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
    )?;
    let exec_vmo = fdio::get_vmo_exec_from_file(&file)?;
    let exec_file = vmo::VmoFile::new(
        exec_vmo, /*readable*/ true, /*writable*/ false, /*executable*/ true,
    );
    Ok(exec_file)
}

fn add_entry(
    entry: io_test::DirectoryEntry,
    dest: &Arc<Simple<MutableConnection>>,
) -> Result<(), Error> {
    match entry {
        io_test::DirectoryEntry::Directory(io_test::Directory { name, entries, .. }) => {
            let new_dir = simple();
            for entry in entries {
                let entry = *entry.expect("Directory entries must not be null");
                add_entry(entry, &new_dir)?;
            }
            dest.add_entry(name, new_dir)?;
        }
        io_test::DirectoryEntry::RemoteDirectory(io_test::RemoteDirectory {
            name,
            remote_client,
            ..
        }) => {
            dest.add_entry(name, remote_dir(remote_client.into_proxy()?))?;
        }
        io_test::DirectoryEntry::File(io_test::File { name, contents, .. }) => {
            let new_file = vmo::read_write(contents);
            dest.add_entry(name, new_file)?;
        }
        io_test::DirectoryEntry::ExecutableFile(io_test::ExecutableFile { name, .. }) => {
            let executable_file = new_executable_file()?;
            dest.add_entry(name, executable_file)?;
        }
    }
    Ok(())
}

async fn run(mut stream: Io1HarnessRequestStream) -> Result<(), Error> {
    while let Some(request) = stream.try_next().await.context("error running harness server")? {
        let (dir, flags, directory_request) = match request {
            Io1HarnessRequest::GetConfig { responder } => {
                let config = Io1Config {
                    // Supported options:
                    supports_create: true,
                    supports_executable_file: true,
                    supports_get_backing_memory: true,
                    supports_remote_dir: true,
                    supports_rename: true,
                    supports_get_token: true,
                    supports_unlink: true,
                    supports_get_attributes: true,
                    supports_open2: true,
                    supports_directory_watchers: true,
                    supports_append: true,
                    supported_attributes: fio::NodeAttributesQuery::PROTOCOLS
                        | fio::NodeAttributesQuery::ABILITIES
                        | fio::NodeAttributesQuery::CONTENT_SIZE
                        | fio::NodeAttributesQuery::STORAGE_SIZE
                        | fio::NodeAttributesQuery::LINK_COUNT
                        | fio::NodeAttributesQuery::ID,

                    // Unsupported options:
                    supports_link: false, // Link is not supported using a pseudo filesystem.
                    supports_link_into: false,
                    // Pseudo-files don't support mutable attributes.
                    supports_update_attributes: false,
                };
                responder.send(&config)?;
                continue;
            }
            Io1HarnessRequest::GetDirectory {
                root,
                flags,
                directory_request,
                control_handle: _,
            } => {
                let dir = simple();
                for entry in root.entries {
                    if let Some(entry) = entry {
                        add_entry(*entry, &dir)?;
                    }
                }
                (dir, flags, directory_request)
            }
        };

        let scope = ExecutionScope::build()
            .entry_constructor(simple::tree_constructor(|_parent, _filename| {
                Ok(vmo::read_write(""))
            }))
            .new();

        dir.open(scope, flags, Path::dot(), directory_request.into_channel().into());
    }

    Ok(())
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Harness);
    fs.take_and_serve_directory_handle()?;

    let fut = fs.for_each_concurrent(10_000, |Harness(stream)| {
        run(stream).unwrap_or_else(|e| error!("Error processing request: {:?}", anyhow!(e)))
    });

    fut.await;
    Ok(())
}
