// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connection to a directory that can not be modified by the client, no matter what permissions
//! the client has on the FIDL connection.

use crate::{
    directory::{
        connection::{
            io1::{BaseConnection, ConnectionState, DerivedConnection, WithShutdown as _},
            util::OpenDirectory,
        },
        entry::DirectoryEntry,
        entry_container,
        mutable::entry_constructor::NewEntryType,
        DirectoryOptions,
    },
    execution_scope::ExecutionScope,
    path::Path,
    ObjectRequest,
};

use {
    fidl_fuchsia_io as fio,
    fuchsia_zircon::Status,
    futures::{channel::oneshot, TryStreamExt as _},
    std::sync::Arc,
};

pub struct ImmutableConnection {
    base: BaseConnection<Self>,
}

impl ImmutableConnection {
    async fn handle_requests(
        mut self,
        requests: fio::DirectoryRequestStream,
        shutdown: oneshot::Receiver<()>,
    ) {
        let mut requests = requests.with_shutdown(shutdown);
        while let Ok(Some(request)) = requests.try_next().await {
            if !matches!(self.base.handle_request(request).await, Ok(ConnectionState::Alive)) {
                break;
            }
        }
    }

    pub fn create_connection(
        scope: ExecutionScope,
        directory: Arc<dyn entry_container::Directory>,
        options: DirectoryOptions,
        object_request: ObjectRequest,
    ) {
        // Ensure we close the directory if we fail to create the connection.
        let directory = OpenDirectory::new(directory);

        let connection = Self::new(scope.clone(), directory, options);

        // If we fail to send the task to the executor, it is probably shut down or is in the
        // process of shutting down (this is the only error state currently).  So there is nothing
        // for us to do - the connection will be closed automatically when the connection object is
        // dropped.
        let _ = scope.spawn_with_shutdown(move |shutdown| async {
            if let Ok(requests) = object_request.into_request_stream(&connection.base).await {
                connection.handle_requests(requests, shutdown).await;
            }
        });
    }
}

impl DerivedConnection for ImmutableConnection {
    type Directory = dyn entry_container::Directory;
    const MUTABLE: bool = false;

    fn new(
        scope: ExecutionScope,
        directory: OpenDirectory<Self::Directory>,
        options: DirectoryOptions,
    ) -> Self {
        ImmutableConnection { base: BaseConnection::<Self>::new(scope, directory, options) }
    }

    fn entry_not_found(
        _scope: ExecutionScope,
        _parent: Arc<dyn DirectoryEntry>,
        _entry_type: NewEntryType,
        create: bool,
        _name: &str,
        _path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, Status> {
        match create {
            false => Err(Status::NOT_FOUND),
            true => Err(Status::NOT_SUPPORTED),
        }
    }
}
