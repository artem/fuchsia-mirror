// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    common::{inherit_rights_for_clone, send_on_open_with_error, IntoAny as _},
    directory::{
        common::check_child_connection_flags,
        entry::DirectoryEntry,
        entry_container::{Directory, DirectoryWatcher},
        mutable::entry_constructor::NewEntryType,
        read_dirents,
        traversal_position::TraversalPosition,
        DirectoryOptions,
    },
    execution_scope::ExecutionScope,
    node::{Node as _, OpenNode},
    object_request::Representation,
    path::Path,
    ObjectRequestRef, ProtocolsExt, ToObjectRequest,
};

use {
    anyhow::Error,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    futures::future::poll_fn,
    std::{convert::TryInto as _, default::Default, sync::Arc, task::Poll},
    storage_trace::{self as trace, TraceFutureExt},
};

/// Return type for `BaseConnection::handle_request` and [`DerivedConnection::handle_request`].
pub enum ConnectionState {
    /// Connection is still alive.
    Alive,
    /// Connection have received Node::Close message and should be closed.
    Closed,
}

/// This is an API a derived directory connection needs to implement, in order for the
/// `BaseConnection` to be able to interact with it.
pub trait DerivedConnection: Send + Sync {
    type Directory: Directory + ?Sized;

    /// Whether these connections support mutable connections.
    const MUTABLE: bool;

    /// Creates entry of the specified type `NewEntryType`.
    fn create_entry(
        scope: ExecutionScope,
        parent: Arc<dyn DirectoryEntry>,
        entry_type: NewEntryType,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, Status>;
}

async fn yield_to_executor() {
    // Yield to the executor now, which should provide an opportunity for the spawned future to
    // run.
    let mut done = false;
    poll_fn(|cx| {
        if done {
            Poll::Ready(())
        } else {
            done = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// Handles functionality shared between mutable and immutable FIDL connections to a directory.  A
/// single directory may contain multiple connections.  Instances of the `BaseConnection`
/// will also hold any state that is "per-connection".  Currently that would be the access flags
/// and the seek position.
pub(in crate::directory) struct BaseConnection<Connection>
where
    Connection: DerivedConnection + 'static,
{
    /// Execution scope this connection and any async operations and connections it creates will
    /// use.
    pub(in crate::directory) scope: ExecutionScope,

    pub(in crate::directory) directory: OpenNode<Connection::Directory>,

    /// Flags set on this connection when it was opened or cloned.
    pub(in crate::directory) options: DirectoryOptions,

    /// Seek position for this connection to the directory.  We just store the element that was
    /// returned last by ReadDirents for this connection.  Next call will look for the next element
    /// in alphabetical order and resume from there.
    ///
    /// An alternative is to use an intrusive tree to have a dual index in both names and IDs that
    /// are assigned to the entries in insertion order.  Then we can store an ID instead of the
    /// full entry name.  This is what the C++ version is doing currently.
    ///
    /// It should be possible to do the same intrusive dual-indexing using, for example,
    ///
    ///     https://docs.rs/intrusive-collections/0.7.6/intrusive_collections/
    ///
    /// but, as, I think, at least for the pseudo directories, this approach is fine, and it simple
    /// enough.
    seek: TraversalPosition,
}

impl<Connection> BaseConnection<Connection>
where
    Connection: DerivedConnection,
{
    /// Constructs an instance of `BaseConnection` - to be used by derived connections, when they
    /// need to create a nested `BaseConnection` "sub-object".  But when implementing
    /// `create_connection`, derived connections should use the [`create_connection`] call.
    pub(in crate::directory) fn new(
        scope: ExecutionScope,
        directory: OpenNode<Connection::Directory>,
        options: DirectoryOptions,
    ) -> Self {
        BaseConnection { scope, directory, options, seek: Default::default() }
    }

    /// Handle a [`DirectoryRequest`].  This function is responsible for handing all the basic
    /// directory operations.
    pub(in crate::directory) async fn handle_request(
        &mut self,
        request: fio::DirectoryRequest,
    ) -> Result<ConnectionState, Error> {
        match request {
            fio::DirectoryRequest::Clone { flags, object, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::Clone");
                self.handle_clone(flags, object);
            }
            fio::DirectoryRequest::Reopen {
                rights_request: _,
                object_request,
                control_handle: _,
            } => {
                trace::duration!(c"storage", c"Directory::Reopen");
                // TODO(https://fxbug.dev/42157659): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `object_request` channel was provided.
                let _: Result<_, _> = object_request.close_with_epitaph(Status::NOT_SUPPORTED);
            }
            fio::DirectoryRequest::Close { responder } => {
                trace::duration!(c"storage", c"Directory::Close");
                responder.send(Ok(()))?;
                return Ok(ConnectionState::Closed);
            }
            fio::DirectoryRequest::GetConnectionInfo { responder } => {
                trace::duration!(c"storage", c"Directory::GetConnectionInfo");
                // TODO(https://fxbug.dev/42157659): Restrict GET_ATTRIBUTES, ENUMERATE, and TRAVERSE.
                // TODO(https://fxbug.dev/42157659): Implement MODIFY_DIRECTORY and UPDATE_ATTRIBUTES.
                responder.send(fio::ConnectionInfo {
                    rights: Some(self.options.rights),
                    ..Default::default()
                })?;
            }
            fio::DirectoryRequest::GetAttr { responder } => {
                async move {
                    let (attrs, status) = match self.directory.get_attrs().await {
                        Ok(attrs) => (attrs, Status::OK.into_raw()),
                        Err(status) => (
                            fio::NodeAttributes {
                                mode: 0,
                                id: fio::INO_UNKNOWN,
                                content_size: 0,
                                storage_size: 0,
                                link_count: 1,
                                creation_time: 0,
                                modification_time: 0,
                            },
                            status.into_raw(),
                        ),
                    };
                    responder.send(status, &attrs)
                }
                .trace(trace::trace_future_args!("storage", "Directory::GetAttr"))
                .await?;
            }
            fio::DirectoryRequest::GetAttributes { query, responder } => {
                async move {
                    let result = self.directory.get_attributes(query).await;
                    responder.send(
                        result
                            .as_ref()
                            .map(|a| {
                                let fio::NodeAttributes2 {
                                    mutable_attributes: m,
                                    immutable_attributes: i,
                                } = a;
                                (m, i)
                            })
                            .map_err(|status| Status::into_raw(*status)),
                    )
                }
                .trace(trace::trace_future_args!("storage", "Directory::GetAttributes"))
                .await?;
            }
            fio::DirectoryRequest::UpdateAttributes { payload: _, responder } => {
                trace::duration!(c"storage", c"Directory::UpdateAttributes");
                // TODO(https://fxbug.dev/42157659): Handle unimplemented io2 method.
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator, .. } => {
                trace::duration!(c"storage", c"Directory::ListExtendedAttributes");
                iterator.close_with_epitaph(Status::NOT_SUPPORTED)?;
            }
            fio::DirectoryRequest::GetExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::GetExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::SetExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { responder, .. } => {
                trace::duration!(c"storage", c"Directory::RemoveExtendedAttribute");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetFlags { responder } => {
                trace::duration!(c"storage", c"Directory::GetFlags");
                responder.send(Status::OK.into_raw(), self.options.to_io1())?;
            }
            fio::DirectoryRequest::SetFlags { flags: _, responder } => {
                trace::duration!(c"storage", c"Directory::SetFlags");
                responder.send(Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Open { flags, mode: _, path, object, control_handle: _ } => {
                {
                    trace::duration!(c"storage", c"Directory::Open");
                    self.handle_open(flags, path, object);
                }
                // Since open typically spawns a task, yield to the executor now to give that task a
                // chance to run before we try and process the next request for this directory.
                yield_to_executor().await;
            }
            fio::DirectoryRequest::Open2 {
                path,
                mut protocols,
                object_request,
                control_handle: _,
            } => {
                {
                    trace::duration!(c"storage", c"Directory::Open2");
                    // Fill in rights from the parent connection if it's absent.
                    if let fio::ConnectionProtocols::Node(fio::NodeOptions {
                        rights,
                        protocols,
                        ..
                    }) = &mut protocols
                    {
                        if rights.is_none() {
                            if matches!(protocols, Some(fio::NodeProtocols { node: Some(_), .. })) {
                                // Only inherit the GET_ATTRIBUTES right for node connections.
                                *rights =
                                    Some(self.options.rights & fio::Operations::GET_ATTRIBUTES);
                            } else {
                                *rights = Some(self.options.rights);
                            }
                        }
                    }
                    // If optional_rights is set, remove any rights that are not present on the
                    // current connection.
                    if let fio::ConnectionProtocols::Node(fio::NodeOptions {
                        protocols:
                            Some(fio::NodeProtocols {
                                directory:
                                    Some(fio::DirectoryProtocolOptions {
                                        optional_rights: Some(rights),
                                        ..
                                    }),
                                ..
                            }),
                        ..
                    }) = &mut protocols
                    {
                        *rights &= self.options.rights;
                    }
                    protocols
                        .to_object_request(object_request)
                        .handle(|req| self.handle_open2(path, protocols, req));
                }
                // Since open typically spawns a task, yield to the executor now to give that task a
                // chance to run before we try and process the next request for this directory.
                yield_to_executor().await;
            }
            fio::DirectoryRequest::AdvisoryLock { request: _, responder } => {
                trace::duration!(c"storage", c"Directory::AdvisoryLock");
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::ReadDirents { max_bytes, responder } => {
                async move {
                    let (status, entries) = self.handle_read_dirents(max_bytes).await;
                    responder.send(status.into_raw(), entries.as_slice())
                }
                .trace(trace::trace_future_args!("storage", "Directory::ReadDirents"))
                .await?;
            }
            fio::DirectoryRequest::Enumerate { options: _, iterator, control_handle: _ } => {
                trace::duration!(c"storage", c"Directory::Enumerate");
                // TODO(https://fxbug.dev/42157659): Handle unimplemented io2 method.
                // Suppress any errors in the event a bad `iterator` channel was provided.
                let _ = iterator.close_with_epitaph(Status::NOT_SUPPORTED);
            }
            fio::DirectoryRequest::Rewind { responder } => {
                trace::duration!(c"storage", c"Directory::Rewind");
                self.seek = Default::default();
                responder.send(Status::OK.into_raw())?;
            }
            fio::DirectoryRequest::Link { src, dst_parent_token, dst, responder } => {
                async move {
                    let status: Status = self.handle_link(&src, dst_parent_token, dst).await.into();
                    responder.send(status.into_raw())
                }
                .trace(trace::trace_future_args!("storage", "Directory::Link"))
                .await?;
            }
            fio::DirectoryRequest::Watch { mask, options, watcher, responder } => {
                trace::duration!(c"storage", c"Directory::Watch");
                let status = if options != 0 {
                    Status::INVALID_ARGS
                } else {
                    let watcher = watcher.try_into()?;
                    self.handle_watch(mask, watcher).into()
                };
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Query { responder } => {
                let () = responder.send(fio::DIRECTORY_PROTOCOL_NAME.as_bytes())?;
            }
            fio::DirectoryRequest::QueryFilesystem { responder } => {
                trace::duration!(c"storage", c"Directory::QueryFilesystem");
                match self.directory.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
            fio::DirectoryRequest::Unlink { name: _, options: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::GetToken { responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw(), None)?;
            }
            fio::DirectoryRequest::Rename { src: _, dst_parent_token: _, dst: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::SetAttr { flags: _, attributes: _, responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw())?;
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::DirectoryRequest::CreateSymlink { responder, .. } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
        }
        Ok(ConnectionState::Alive)
    }

    fn handle_clone(&self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        let flags = match inherit_rights_for_clone(self.options.to_io1(), flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        self.directory.clone().open(self.scope.clone(), flags, Path::dot(), server_end);
    }

    fn handle_open(
        &self,
        mut flags: fio::OpenFlags,
        path: String,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);

        let path = match Path::validate_and_split(path) {
            Ok(path) => path,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };

        if path.is_dir() {
            flags |= fio::OpenFlags::DIRECTORY;
        }

        let flags = match check_child_connection_flags(self.options.to_io1(), flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);
                return;
            }
        };
        if path.is_dot() {
            if flags.intersects(fio::OpenFlags::NOT_DIRECTORY) {
                send_on_open_with_error(describe, server_end, Status::INVALID_ARGS);
                return;
            }
            if flags.intersects(fio::OpenFlags::CREATE_IF_ABSENT) {
                send_on_open_with_error(describe, server_end, Status::ALREADY_EXISTS);
                return;
            }
        }

        // It is up to the open method to handle OPEN_FLAG_DESCRIBE from this point on.
        let directory = self.directory.clone();
        directory.open(self.scope.clone(), flags, path, server_end);
    }

    fn handle_open2(
        &self,
        path: String,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        let path = Path::validate_and_split(path)?;

        if let Some(rights) = protocols.rights() {
            if rights.intersects(!self.options.rights) {
                return Err(Status::ACCESS_DENIED);
            }
        }

        // If requesting attributes, check permission.
        if !object_request.attributes().is_empty()
            && !self.options.rights.contains(fio::Operations::GET_ATTRIBUTES)
        {
            return Err(Status::ACCESS_DENIED);
        }

        // If creating an object, it's not legal to specify more than one protocol.
        //
        // TODO(b/293947862): If we add an additional node type, we will need to update this. See if
        // there is a more generic or robust way to check this so that we don't miss any node types.
        if protocols.open_mode() != fio::OpenMode::OpenExisting
            && ((protocols.is_file_allowed() && protocols.is_dir_allowed())
                || protocols.is_symlink_allowed())
        {
            return Err(Status::INVALID_ARGS);
        }

        if protocols.create_attributes().is_some()
            && protocols.open_mode() == fio::OpenMode::OpenExisting
        {
            return Err(Status::INVALID_ARGS);
        }

        if path.is_dot() {
            if !protocols.is_node() && !protocols.is_dir_allowed() {
                return Err(Status::INVALID_ARGS);
            }
            if protocols.open_mode() == fio::OpenMode::AlwaysCreate {
                return Err(Status::ALREADY_EXISTS);
            }
        }

        self.directory.clone().open2(self.scope.clone(), path, protocols, object_request)
    }

    async fn handle_read_dirents(&mut self, max_bytes: u64) -> (Status, Vec<u8>) {
        async {
            let (new_pos, sealed) =
                self.directory.read_dirents(&self.seek, read_dirents::Sink::new(max_bytes)).await?;
            self.seek = new_pos;
            let read_dirents::Done { buf, status } = *sealed
                .open()
                .downcast::<read_dirents::Done>()
                .map_err(|_: Box<dyn std::any::Any>| {
                    #[cfg(debug)]
                    panic!(
                        "`read_dirents()` returned a `dirents_sink::Sealed`
                        instance that is not an instance of the \
                        `read_dirents::Done`. This is a bug in the \
                        `read_dirents()` implementation."
                    );
                    Status::NOT_SUPPORTED
                })?;
            Ok((status, buf))
        }
        .await
        .unwrap_or_else(|status| (status, Vec::new()))
    }

    async fn handle_link(
        &self,
        source_name: &str,
        target_parent_token: fidl::Handle,
        target_name: String,
    ) -> Result<(), Status> {
        if source_name.contains('/') || target_name.contains('/') {
            return Err(Status::INVALID_ARGS);
        }

        if !self.options.rights.contains(fio::W_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }

        let (target_parent, _flags) = self
            .scope
            .token_registry()
            .get_owner(target_parent_token)?
            .ok_or(Err(Status::NOT_FOUND))?;

        target_parent.link(target_name, self.directory.clone().into_any(), source_name).await
    }

    fn handle_watch(
        &mut self,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        let directory = self.directory.clone();
        directory.register_watcher(self.scope.clone(), mask, watcher)
    }
}

impl<T: DerivedConnection + 'static> Representation for BaseConnection<T> {
    type Protocol = fio::DirectoryMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, Status> {
        Ok(fio::Representation::Directory(fio::DirectoryInfo {
            attributes: Some(self.directory.get_attributes(requested_attributes).await?),
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        Ok(fio::NodeInfoDeprecated::Directory(fio::DirectoryObject))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::directory::immutable::simple::simple, assert_matches::assert_matches,
        fidl_fuchsia_io as fio, fuchsia_zircon_status::Status, futures::prelude::*,
    };

    #[fuchsia::test]
    async fn test_open_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end
            ),
            Ok(())
        );

        // The channel also be closed with a NOT_FOUND epitaph.
        assert_matches!(
            node_proxy.query().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end
            ),
            Ok(())
        );

        // The event stream should be closed with the epitaph.
        let mut event_stream = node_proxy.take_event_stream();
        assert_matches!(
            event_stream.try_next().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
                ..
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }

    #[fuchsia::test]
    async fn test_open_with_describe_not_found() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::DIRECTORY
                    | fio::OpenFlags::DESCRIBE
                    | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end,
            ),
            Ok(())
        );

        // The channel should be closed with a NOT_FOUND epitaph.
        assert_matches!(
            node_proxy.query().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn test_open_describe_not_found_event_stream() {
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");

        let dir = simple();
        dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::DIRECTORY | fio::OpenFlags::RIGHT_READABLE,
            Path::dot(),
            ServerEnd::new(dir_server_end.into_channel()),
        );

        let (node_proxy, node_server_end) =
            fidl::endpoints::create_proxy().expect("Create proxy to succeed");

        // Try to open a file that doesn't exist.
        assert_matches!(
            dir_proxy.open(
                fio::OpenFlags::DIRECTORY
                    | fio::OpenFlags::DESCRIBE
                    | fio::OpenFlags::RIGHT_READABLE,
                fio::ModeType::empty(),
                "foo",
                node_server_end,
            ),
            Ok(())
        );

        // The event stream should return that the file does not exist.
        let mut event_stream = node_proxy.take_event_stream();
        assert_matches!(
            event_stream.try_next().await,
            Ok(Some(fio::NodeEvent::OnOpen_ {
                s,
                info: None,
            }))
            if Status::from_raw(s) == Status::NOT_FOUND
        );
        assert_matches!(
            event_stream.try_next().await,
            Err(fidl::Error::ClientChannelClosed {
                status: Status::NOT_FOUND,
                protocol_name: "(anonymous) Node",
                ..
            })
        );
        assert_matches!(event_stream.try_next().await, Ok(None));
    }
}
