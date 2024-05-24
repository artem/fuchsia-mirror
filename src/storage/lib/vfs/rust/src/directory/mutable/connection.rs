// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connection to a directory that can be modified by the client though a FIDL connection.

use crate::{
    common::{
        decode_extended_attribute_value, encode_extended_attribute_value,
        extended_attributes_sender,
    },
    directory::{
        connection::{BaseConnection, ConnectionState, DerivedConnection},
        entry_container::MutableDirectory,
    },
    execution_scope::ExecutionScope,
    name::validate_name,
    node::OpenNode,
    path::Path,
    token_registry::{TokenInterface, TokenRegistry, Tokenizable},
    ObjectRequestRef, ProtocolsExt,
};

use {
    anyhow::Error,
    fidl::{endpoints::ServerEnd, Handle},
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    futures::{pin_mut, TryStreamExt as _},
    pin_project::pin_project,
    std::{future::Future, pin::Pin, sync::Arc},
    storage_trace::{self as trace, TraceFutureExt},
};

#[pin_project]
pub struct MutableConnection {
    base: BaseConnection<Self>,
}

impl DerivedConnection for MutableConnection {
    type Directory = dyn MutableDirectory;
    const MUTABLE: bool = true;
}

impl MutableConnection {
    pub fn create(
        scope: ExecutionScope,
        directory: Arc<impl MutableDirectory>,
        protocols: impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        // Ensure we close the directory if we fail to prepare the connection.
        let directory = OpenNode::new(directory as Arc<dyn MutableDirectory>);

        let connection = MutableConnection {
            base: BaseConnection::<Self>::new(scope, directory, protocols.to_directory_options()?),
        };

        let object_request = object_request.take();
        Ok(async move {
            if let Ok(requests) = object_request.into_request_stream(&connection.base).await {
                connection.handle_requests(requests).await
            }
        })
    }

    async fn handle_request(
        this: Pin<&mut Tokenizable<Self>>,
        request: fio::DirectoryRequest,
    ) -> Result<ConnectionState, Error> {
        match request {
            fio::DirectoryRequest::Unlink { name, options, responder } => {
                let result = this.as_mut().handle_unlink(name, options).await;
                responder.send(result.map_err(Status::into_raw))?;
            }
            fio::DirectoryRequest::GetToken { responder } => {
                let (status, token) = match Self::handle_get_token(this.into_ref()) {
                    Ok(token) => (Status::OK, Some(token)),
                    Err(status) => (status, None),
                };
                responder.send(status.into_raw(), token)?;
            }
            fio::DirectoryRequest::Rename { src, dst_parent_token, dst, responder } => {
                let result = this.handle_rename(src, Handle::from(dst_parent_token), dst).await;
                responder.send(result.map_err(Status::into_raw))?;
            }
            fio::DirectoryRequest::SetAttr { flags, attributes, responder } => {
                let status = match this.as_mut().handle_setattr(flags, attributes).await {
                    Ok(()) => Status::OK,
                    Err(status) => status,
                };
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder.send(this.base.directory.sync().await.map_err(Status::into_raw))?;
            }
            request @ (fio::DirectoryRequest::AdvisoryLock { .. }
            | fio::DirectoryRequest::Clone { .. }
            | fio::DirectoryRequest::Close { .. }
            | fio::DirectoryRequest::GetConnectionInfo { .. }
            | fio::DirectoryRequest::GetAttr { .. }
            | fio::DirectoryRequest::GetAttributes { .. }
            | fio::DirectoryRequest::GetFlags { .. }
            | fio::DirectoryRequest::Link { .. }
            | fio::DirectoryRequest::Open { .. }
            | fio::DirectoryRequest::Open2 { .. }
            | fio::DirectoryRequest::Query { .. }
            | fio::DirectoryRequest::QueryFilesystem { .. }
            | fio::DirectoryRequest::ReadDirents { .. }
            | fio::DirectoryRequest::Reopen { .. }
            | fio::DirectoryRequest::Rewind { .. }
            | fio::DirectoryRequest::SetFlags { .. }
            | fio::DirectoryRequest::Watch { .. }) => {
                return this.as_mut().base.handle_request(request).await;
            }
            fio::DirectoryRequest::CreateSymlink {
                responder, name, target, connection, ..
            } => {
                if !this.base.options.rights.contains(fio::Operations::MODIFY_DIRECTORY) {
                    responder.send(Err(Status::ACCESS_DENIED.into_raw()))?;
                } else if validate_name(&name).is_err() {
                    responder.send(Err(Status::INVALID_ARGS.into_raw()))?;
                } else {
                    responder.send(
                        this.as_mut()
                            .base
                            .directory
                            .create_symlink(name, target, connection)
                            .await
                            .map_err(Status::into_raw),
                    )?;
                }
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator, control_handle: _ } => {
                this.handle_list_extended_attribute(iterator)
                    .trace(trace::trace_future_args!(
                        c"storage",
                        c"Directory::ListExtendedAttributes"
                    ))
                    .await;
            }
            fio::DirectoryRequest::GetExtendedAttribute { name, responder } => {
                async move {
                    let res =
                        this.handle_get_extended_attribute(name).await.map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::GetExtendedAttribute"))
                .await?;
            }
            fio::DirectoryRequest::SetExtendedAttribute { name, value, mode, responder } => {
                async move {
                    let res = this
                        .handle_set_extended_attribute(name, value, mode)
                        .await
                        .map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::SetExtendedAttribute"))
                .await?;
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { name, responder } => {
                async move {
                    let res =
                        this.handle_remove_extended_attribute(name).await.map_err(Status::into_raw);
                    responder.send(res)
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::RemoveExtendedAttribute"))
                .await?;
            }
            fio::DirectoryRequest::UpdateAttributes { payload, responder } => {
                async move {
                    responder.send(
                        this.as_mut()
                            .handle_update_attributes(payload)
                            .await
                            .map_err(Status::into_raw),
                    )
                }
                .trace(trace::trace_future_args!(c"storage", c"Directory::UpdateAttributes"))
                .await?;
            }
        }
        Ok(ConnectionState::Alive)
    }

    async fn handle_setattr(
        self: Pin<&mut Self>,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> Result<(), Status> {
        if !self.base.options.rights.contains(fio::W_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }

        // TODO(jfsulliv): Consider always permitting attributes to be deferrable. The risk with
        // this is that filesystems would require a background flush of dirty attributes to disk.
        self.base.directory.set_attrs(flags, attributes).await
    }

    async fn handle_update_attributes(
        self: Pin<&mut Self>,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        if !self.base.options.rights.contains(fio::Operations::UPDATE_ATTRIBUTES) {
            return Err(Status::BAD_HANDLE);
        }

        self.base.directory.update_attributes(attributes).await
    }

    async fn handle_unlink(
        self: Pin<&mut Self>,
        name: String,
        options: fio::UnlinkOptions,
    ) -> Result<(), Status> {
        if !self.base.options.rights.contains(fio::W_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }

        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(Status::INVALID_ARGS);
        }

        self.base
            .directory
            .clone()
            .unlink(
                &name,
                options
                    .flags
                    .map(|f| f.contains(fio::UnlinkFlags::MUST_BE_DIRECTORY))
                    .unwrap_or(false),
            )
            .await
    }

    fn handle_get_token(this: Pin<&Tokenizable<Self>>) -> Result<Handle, Status> {
        if !this.base.options.rights.contains(fio::W_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }
        Ok(TokenRegistry::get_token(this)?)
    }

    async fn handle_rename(
        &self,
        src: String,
        dst_parent_token: Handle,
        dst: String,
    ) -> Result<(), Status> {
        if !self.base.options.rights.contains(fio::W_STAR_DIR) {
            return Err(Status::BAD_HANDLE);
        }

        let src = Path::validate_and_split(src)?;
        let dst = Path::validate_and_split(dst)?;

        if !src.is_single_component() || !dst.is_single_component() {
            return Err(Status::INVALID_ARGS);
        }

        let (dst_parent, _flags) =
            match self.base.scope.token_registry().get_owner(dst_parent_token)? {
                None => return Err(Status::NOT_FOUND),
                Some(entry) => entry,
            };

        dst_parent.clone().rename(self.base.directory.clone(), src, dst).await
    }

    async fn handle_list_extended_attribute(
        &self,
        iterator: ServerEnd<fio::ExtendedAttributeIteratorMarker>,
    ) {
        let attributes = match self.base.directory.list_extended_attributes().await {
            Ok(attributes) => attributes,
            Err(status) => {
                tracing::error!(?status, "list extended attributes failed");
                iterator
                    .close_with_epitaph(status)
                    .unwrap_or_else(|error| tracing::error!(?error, "failed to send epitaph"));
                return;
            }
        };
        self.base.scope.spawn(extended_attributes_sender(iterator, attributes));
    }

    async fn handle_get_extended_attribute(
        &self,
        name: Vec<u8>,
    ) -> Result<fio::ExtendedAttributeValue, Status> {
        let value = self.base.directory.get_extended_attribute(name).await?;
        encode_extended_attribute_value(value)
    }

    async fn handle_set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: fio::ExtendedAttributeValue,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), Status> {
        if name.iter().any(|c| *c == 0) {
            return Err(Status::INVALID_ARGS);
        }
        let val = decode_extended_attribute_value(value)?;
        self.base.directory.set_extended_attribute(name, val, mode).await
    }

    async fn handle_remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Status> {
        self.base.directory.remove_extended_attribute(name).await
    }

    async fn handle_requests(self, mut requests: fio::DirectoryRequestStream) {
        let this = Tokenizable::new(self);
        pin_mut!(this);
        while let Ok(Some(request)) = requests.try_next().await {
            let _guard = this.base.scope.active_guard();
            if !matches!(
                Self::handle_request(Pin::as_mut(&mut this), request).await,
                Ok(ConnectionState::Alive)
            ) {
                break;
            }
        }
    }
}

impl TokenInterface for MutableConnection {
    fn get_node_and_flags(&self) -> (Arc<dyn MutableDirectory>, fio::OpenFlags) {
        (self.base.directory.clone(), self.base.options.to_io1())
    }

    fn token_registry(&self) -> &TokenRegistry {
        self.base.scope.token_registry()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            directory::{
                dirents_sink,
                entry_container::{Directory, DirectoryWatcher},
                traversal_position::TraversalPosition,
            },
            node::{IsDirectory, Node},
            ToObjectRequest,
        },
        async_trait::async_trait,
        std::{
            any::Any,
            sync::{Mutex, Weak},
        },
    };

    #[derive(Debug, PartialEq)]
    enum MutableDirectoryAction {
        Link { id: u32, path: String },
        Unlink { id: u32, name: String },
        Rename { id: u32, src_name: String, dst_dir: u32, dst_name: String },
        SetAttr { id: u32, flags: fio::NodeAttributeFlags, attrs: fio::NodeAttributes },
        UpdateAttributes { id: u32, attributes: fio::MutableNodeAttributes },
        Sync,
        Close,
    }

    #[derive(Debug)]
    struct MockDirectory {
        id: u32,
        fs: Arc<MockFilesystem>,
    }

    impl MockDirectory {
        pub fn new(id: u32, fs: Arc<MockFilesystem>) -> Arc<Self> {
            Arc::new(MockDirectory { id, fs })
        }
    }

    impl PartialEq for MockDirectory {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }

    #[async_trait]
    impl Node for MockDirectory {
        async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
            unimplemented!("Not implemented");
        }

        async fn get_attributes(
            &self,
            _query: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, Status> {
            unimplemented!("Not implemented");
        }

        fn close(self: Arc<Self>) {
            let _ = self.fs.handle_event(MutableDirectoryAction::Close);
        }
    }

    #[async_trait]
    impl Directory for MockDirectory {
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            _path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            unimplemented!("Not implemented!");
        }

        fn open2(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _path: Path,
            _protocols: fio::ConnectionProtocols,
            _object_request: ObjectRequestRef<'_>,
        ) -> Result<(), Status> {
            unimplemented!("Not implemented!");
        }

        async fn read_dirents<'a>(
            &'a self,
            _pos: &'a TraversalPosition,
            _sink: Box<dyn dirents_sink::Sink>,
        ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
            unimplemented!("Not implemented");
        }

        fn register_watcher(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _mask: fio::WatchMask,
            _watcher: DirectoryWatcher,
        ) -> Result<(), Status> {
            unimplemented!("Not implemented");
        }

        fn unregister_watcher(self: Arc<Self>, _key: usize) {
            unimplemented!("Not implemented");
        }
    }

    #[async_trait]
    impl MutableDirectory for MockDirectory {
        async fn link(
            self: Arc<Self>,
            path: String,
            _source_dir: Arc<dyn Any + Send + Sync>,
            _source_name: &str,
        ) -> Result<(), Status> {
            self.fs.handle_event(MutableDirectoryAction::Link { id: self.id, path })
        }

        async fn unlink(
            self: Arc<Self>,
            name: &str,
            _must_be_directory: bool,
        ) -> Result<(), Status> {
            self.fs.handle_event(MutableDirectoryAction::Unlink {
                id: self.id,
                name: name.to_string(),
            })
        }

        async fn set_attrs(
            &self,
            flags: fio::NodeAttributeFlags,
            attrs: fio::NodeAttributes,
        ) -> Result<(), Status> {
            self.fs.handle_event(MutableDirectoryAction::SetAttr { id: self.id, flags, attrs })
        }

        async fn update_attributes(
            &self,
            attributes: fio::MutableNodeAttributes,
        ) -> Result<(), Status> {
            self.fs
                .handle_event(MutableDirectoryAction::UpdateAttributes { id: self.id, attributes })
        }

        async fn sync(&self) -> Result<(), Status> {
            self.fs.handle_event(MutableDirectoryAction::Sync)
        }

        async fn rename(
            self: Arc<Self>,
            src_dir: Arc<dyn MutableDirectory>,
            src_name: Path,
            dst_name: Path,
        ) -> Result<(), Status> {
            let src_dir = src_dir.into_any().downcast::<MockDirectory>().unwrap();
            self.fs.handle_event(MutableDirectoryAction::Rename {
                id: src_dir.id,
                src_name: src_name.into_string(),
                dst_dir: self.id,
                dst_name: dst_name.into_string(),
            })
        }
    }

    impl IsDirectory for MockDirectory {}

    struct Events(Mutex<Vec<MutableDirectoryAction>>);

    impl Events {
        fn new() -> Arc<Self> {
            Arc::new(Events(Mutex::new(vec![])))
        }
    }

    struct MockFilesystem {
        cur_id: Mutex<u32>,
        scope: ExecutionScope,
        events: Weak<Events>,
    }

    impl MockFilesystem {
        pub fn new(events: &Arc<Events>) -> Self {
            let scope = ExecutionScope::new();
            MockFilesystem { cur_id: Mutex::new(0), scope, events: Arc::downgrade(events) }
        }

        pub fn handle_event(&self, event: MutableDirectoryAction) -> Result<(), Status> {
            self.events.upgrade().map(|x| x.0.lock().unwrap().push(event));
            Ok(())
        }

        pub fn make_connection(
            self: &Arc<Self>,
            flags: fio::OpenFlags,
        ) -> (Arc<MockDirectory>, fio::DirectoryProxy) {
            let mut cur_id = self.cur_id.lock().unwrap();
            let dir = MockDirectory::new(*cur_id, self.clone());
            *cur_id += 1;
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            flags.to_object_request(server_end).handle(|object_request| {
                object_request.spawn_connection(
                    self.scope.clone(),
                    dir.clone(),
                    flags,
                    MutableConnection::create,
                )
            });
            (dir, proxy)
        }
    }

    impl std::fmt::Debug for MockFilesystem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockFilesystem").field("cur_id", &self.cur_id).finish()
        }
    }

    #[fuchsia::test]
    async fn test_rename() {
        use fidl::Event;

        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));

        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let (dir2, proxy2) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        let (status, token) = proxy2.get_token().await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);

        let status = proxy.rename("src", Event::from(token.unwrap()), "dest").await.unwrap();
        assert!(status.is_ok());

        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::Rename {
                id: 0,
                src_name: "src".to_owned(),
                dst_dir: dir2.id,
                dst_name: "dest".to_owned(),
            },]
        );
    }

    #[fuchsia::test]
    async fn test_setattr() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let attrs = fio::NodeAttributes {
            mode: 0,
            id: 0,
            content_size: 0,
            storage_size: 0,
            link_count: 0,
            creation_time: 30,
            modification_time: 100,
        };
        let status = proxy
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME | fio::NodeAttributeFlags::MODIFICATION_TIME,
                &attrs,
            )
            .await
            .unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);

        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::SetAttr {
                id: 0,
                flags: fio::NodeAttributeFlags::CREATION_TIME
                    | fio::NodeAttributeFlags::MODIFICATION_TIME,
                attrs
            }]
        );
    }

    #[fuchsia::test]
    async fn test_update_attributes() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let attributes = fio::MutableNodeAttributes {
            creation_time: Some(30),
            modification_time: Some(100),
            mode: Some(200),
            ..Default::default()
        };
        let () =
            proxy.update_attributes(&attributes).await.unwrap().map_err(Status::from_raw).unwrap();

        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::UpdateAttributes { id: 0, attributes }]);
    }

    #[fuchsia::test]
    async fn test_link() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let (_dir2, proxy2) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        let (status, token) = proxy2.get_token().await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);

        let status = proxy.link("src", token.unwrap(), "dest").await.unwrap();
        assert_eq!(Status::from_raw(status), Status::OK);
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Link { id: 1, path: "dest".to_owned() },]);
    }

    #[fuchsia::test]
    async fn test_unlink() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        proxy
            .unlink("test", &fio::UnlinkOptions::default())
            .await
            .expect("fidl call failed")
            .expect("unlink failed");
        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::Unlink { id: 0, name: "test".to_string() },]
        );
    }

    #[fuchsia::test]
    async fn test_sync() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let () = proxy.sync().await.unwrap().map_err(Status::from_raw).unwrap();
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Sync]);
    }

    #[fuchsia::test]
    async fn test_close() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let () = proxy.close().await.unwrap().map_err(Status::from_raw).unwrap();
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Close]);
    }

    #[fuchsia::test]
    async fn test_implicit_close() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, _proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        fs.scope.shutdown();
        fs.scope.wait().await;

        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Close]);
    }
}
