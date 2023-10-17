// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{
        AnyCast, AsRouter, Capability, CloneError, Completer, ConversionError, Directory, Request,
        Router,
    },
    core::fmt,
    fidl::endpoints::{create_endpoints, ClientEnd, ServerEnd},
    fidl::epitaph::ChannelEpitaphExt,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::future::BoxFuture,
    futures::FutureExt,
    futures::TryStreamExt,
    std::sync::Arc,
    vfs::{execution_scope::ExecutionScope, path::Path},
};

/// An [Open] capability lets the holder obtain other capabilities by pipelining
/// a [zx::Channel], usually treated as the server endpoint of some FIDL protocol.
/// We call this operation opening the capability.
///
/// ## Open via remoting
///
/// The most straightforward way to open the capability is via [Capability::to_zx_handle].
/// This will return a `fuchsia.io/Openable` client endpoint, where FIDL open requests on
/// that endpoint translate to [OpenFn] calls.
///
/// Intuitively this is opening a new connection to the current object.
///
/// ## Open via `Dict` integration
///
/// When converting a `Dict` capability to [Open], all the dictionary entries will be
/// recursively converted to [Open] capabilities. An open capability within the `Dict`
/// functions similarly to a directory entry:
///
/// * Remoting the [Open] from the `Dict` gives access to a `fuchsia.io/Directory`.
/// * Within this directory, each member [Open] will show up as a directory entry, whose
///   type is `entry_type`.
/// * When a `fuchsia.io/Directory.Open` request hits the directory, the [OpenFn] of the
///   entry matching the first path segment of the open request will be invoked, passing
///   the remaining relative path if any, or "." if the path terminates at that entry.
///
/// Intuitively this is akin to mounting a remote VFS node in a directory.
///
/// ## Cloning
///
/// [Open] implements [Clone] because logically one agent opening two capabilities
/// is equivalent to two agents each opening one capability through their respective
/// clones of the open capability.
#[derive(Capability, Clone)]
#[capability(as_trait(AsRouter))]
pub struct Open {
    open_fn: Arc<OpenFn>,
    entry_type: fio::DirentType,
}

/// The function that will be called when this capability is opened.
pub type OpenFn = dyn Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync;

impl fmt::Debug for Open {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Open")
            .field("open", &"[open function]")
            .field("entry_type", &self.entry_type)
            .finish()
    }
}

impl Open {
    /// Creates an [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `open` - The function that will be called when this capability is opened.
    ///
    /// * `entry_type` - The type of the node that will be returned when the [Open] is placed
    ///   within a `Dict` and the user enumerates the `fuchsia.io/Directory` representation
    ///   of the dictionary.
    ///
    pub fn new<F>(open: F, entry_type: fio::DirentType) -> Self
    where
        F: Fn(ExecutionScope, fio::OpenFlags, Path, zx::Channel) -> () + Send + Sync + 'static,
    {
        Open { open_fn: Arc::new(open), entry_type }
    }

    /// Converts the [Open] capability into a [Directory] capability such that it will be
    /// opened with `open_flags` during [Capability::to_zx_handle].
    pub fn into_directory(self, open_flags: fio::OpenFlags) -> Directory {
        Directory::from_open(self, open_flags, crate::router::Path::default())
    }

    /// Validates that `path` is a valid `fuchsia.io` path and opens the corresponding entry.
    pub fn open(
        &self,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: String,
        server_end: zx::Channel,
    ) {
        match Path::validate_and_split(path) {
            Ok(path) => (self.open_fn)(scope.clone(), flags, path, server_end.into()),
            Err(status) => {
                let _ = server_end.close_with_epitaph(status);
            }
        }
    }

    /// Returns a capability which will open with rights downscoped to
    /// `rights` if specified, and open paths relative to
    /// `request.relative_path` if non-empty, from the base [`Open`] object.
    pub fn downscoped(
        self,
        rights: Option<fio::OpenFlags>,
        path: crate::router::Path,
    ) -> Directory {
        // Downscoping to just a subdir without also downscoping rights is not something
        // that is directly supported in `fuchsia.io` v1, because there is no way to open
        // and inherit the parent rights. This flag combination is the closest thing.
        //
        // TODO(fxbug.dev/293947862): when we migrate to v2 of `fuchsia.io`, the custom
        // flags here can be replaced by opening and inheriting the parent rights.
        let rights = rights.unwrap_or(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
        );
        Directory::from_open(self, rights, path)
    }

    /// Turn the [Open] into a remote VFS node.
    ///
    /// Both `into_remote` and [Capability::to_zx_handle] will let us open the capability:
    ///
    /// * `into_remote` returns a [vfs::remote::Remote] that supports
    ///   [DirectoryEntry::open].
    /// * [Capability::to_zx_handle] returns a client endpoint and calls
    ///   [DirectoryEntry::open] with the server endpoint given open requests.
    ///
    /// `into_remote` avoids a round trip through FIDL and channels, and is used as an
    /// internal performance optimization by `Dict` when building a directory tree.
    pub(crate) fn into_remote(self) -> Arc<vfs::remote::Remote> {
        let open = self.open_fn;
        vfs::remote::remote_boxed_with_type(
            Box::new(
                move |scope: ExecutionScope,
                      flags: fio::OpenFlags,
                      relative_path: Path,
                      server_end: ServerEnd<fio::NodeMarker>| {
                    open(scope, flags, relative_path, server_end.into_channel().into())
                },
            ),
            self.entry_type,
        )
    }
}

impl From<ClientEnd<fio::DirectoryMarker>> for Open {
    fn from(value: ClientEnd<fio::DirectoryMarker>) -> Self {
        // Open is one-way so a synchronous proxy is not going to block.
        let proxy = fio::DirectorySynchronousProxy::new(value.into_channel());
        Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                let _ = proxy.open(
                    flags,
                    fio::ModeType::empty(),
                    relative_path.as_ref(),
                    server_end.into(),
                );
            },
            fio::DirentType::Directory,
        )
    }
}

impl Capability for Open {
    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }

    fn try_into_capability(
        self,
        type_id: std::any::TypeId,
    ) -> Result<Box<dyn std::any::Any>, ConversionError> {
        if type_id == std::any::TypeId::of::<Self>() {
            return Ok(Box::new(self).into_any());
        }
        Err(ConversionError::NotSupported)
    }

    /// Returns a `fuchsia.io/Openable` client endpoint that can open this capability.
    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_endpoints::<fio::OpenableMarker>();
        // If this future is dropped, stop serving the connection.
        let guard = scopeguard::guard(scope.clone(), move |scope| {
            scope.shutdown();
        });
        let mut request_stream = server_end.into_stream().unwrap();
        let fut = async move {
            let _guard = guard;
            while let Ok(Some(request)) = request_stream.try_next().await {
                match request {
                    fio::OpenableRequest::Open { flags, mode: _mode, path, object, .. } => {
                        self.open(scope.clone(), flags, path, object.into_channel())
                    }
                }
            }
            scope.wait().await;
        }
        .boxed();

        (client_end.into_handle(), Some(fut))
    }
}

/// [`Open`] can vend out routers. Each request from the router will yield a [`Directory`] object
/// which will with rights downscoped to `request.rights` and open paths relative to
/// `request.relative_path` from the base [`Open`] object.
impl AsRouter for Open {
    fn as_router(&self) -> Router {
        let open = self.clone();
        let route_fn = move |request: Request, completer: Completer| {
            completer.complete(Ok(Box::new(
                open.clone().downscoped(request.rights, request.relative_path),
            )));
        };
        Router::new(route_fn)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fuchsia_async as fasync, fuchsia_zircon as zx,
        lazy_static::lazy_static,
        test_util::Counter,
        vfs::{
            directory::{entry::DirectoryEntry, helper::DirectlyMutable, immutable::simple as pfs},
            name::Name,
        },
    };

    #[fuchsia::test]
    async fn test_into_remote() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "bar");
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );
        let remote = open.into_remote();
        let dir = pfs::simple();
        dir.get_or_insert(Name::from("foo").unwrap(), || remote);

        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        dir.clone().open(
            scope.clone(),
            fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, server_end) = zx::Channel::create();
        let dir = dir_client_end.channel();
        fdio::service_connect_at(dir, "foo/bar", server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "path");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, fut) = open.to_zx_handle();
        let client_end =
            fidl::endpoints::ClientEnd::<fio::DirectoryMarker>::from(zx::Channel::from(client_end));
        let client = client_end.into_proxy().unwrap();
        let (_client_end, server_end) = create_endpoints();
        client.open(fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), "path", server_end).unwrap();
        drop(client);
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote_not_used_if_not_written_to() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let (client_end, fut) = open.to_zx_handle();
        drop(client_end);
        fut.unwrap().await;
        assert_eq!(OPEN_COUNT.get(), 0);
    }

    async fn get_connection_rights(
        directory: Directory,
    ) -> anyhow::Result<(fasync::Task<()>, i32, fio::OpenFlags)> {
        let (client_end, fut) = directory.to_zx_handle();
        let fut = fasync::Task::spawn(fut.unwrap());
        let client_end: ClientEnd<fio::DirectoryMarker> = client_end.into();
        let client = client_end.into_proxy()?;
        let (status, flags) = client.get_flags().await?;
        Ok((fut, status, flags))
    }

    async fn get_entries(directory: Directory) -> anyhow::Result<(fasync::Task<()>, Vec<String>)> {
        let (client_end, fut) = directory.to_zx_handle();
        let fut = fasync::Task::spawn(fut.unwrap());
        let client_end: ClientEnd<fio::DirectoryMarker> = client_end.into();
        let client = client_end.into_proxy()?;
        let entries = fuchsia_fs::directory::readdir(&client).await?;
        Ok((fut, entries.into_iter().map(|entry| entry.name).collect()))
    }

    #[fuchsia::test]
    async fn downscope_rights() {
        // Make an [Open] that corresponds to `/` with read-write rights.
        let dir = pfs::simple();
        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        dir.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );
        let open = Open::from(dir_client_end);

        // Verify that the connection is read-write.
        let directory = open
            .clone()
            .into_directory(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE);
        let (_, status, flags) = get_connection_rights(directory).await.unwrap();
        assert_eq!(status, zx::Status::OK.into_raw());
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        // Downscope the rights to read-only.
        let directory = open
            .clone()
            .downscoped(Some(fio::OpenFlags::RIGHT_READABLE), crate::router::Path::default());

        // Verify that the connection is read-only.
        let (_, status, flags) = get_connection_rights(directory).await.unwrap();
        assert_eq!(status, zx::Status::OK.into_raw());
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
    }

    #[fuchsia::test]
    async fn downscope_path() {
        // Build a directory tree with `/foo`.
        let dir = pfs::simple();
        dir.add_entry_impl("foo".to_owned().try_into().unwrap(), pfs::simple(), false).unwrap();

        // Make an [Open] that corresponds to `/`.
        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        dir.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );
        let open = Open::from(dir_client_end);

        // Verify that the connection has a directory named `foo`.
        let directory = open.clone().into_directory(fio::OpenFlags::RIGHT_READABLE);
        let (_, entries) = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec!["foo".to_owned()]);

        // Downscope the path to `foo`.
        let directory = open.clone().downscoped(None, crate::router::Path::new("foo"));

        // Verify that the connection does not have anymore children, since `foo` has no children.
        let (_, entries) = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec![] as Vec<String>);
    }
}
