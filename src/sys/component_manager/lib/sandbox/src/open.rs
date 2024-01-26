// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::anyhow;
use clonable_error::ClonableError;
use core::fmt;
use fidl::endpoints::{create_endpoints, ClientEnd, ServerEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::{FutureExt, TryStreamExt};
use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::{Arc, OnceLock};
use vfs::{common::send_on_open_with_error, execution_scope::ExecutionScope};

use crate::{registry, Capability, ConversionError, Directory, OneShotHandle};

/// An [Open] capability lets the holder obtain other capabilities by pipelining
/// a [zx::Channel], usually treated as the server endpoint of some FIDL protocol.
/// We call this operation opening the capability.
///
/// ## Open via remoting
///
/// The most straightforward way to open the capability is to convert it to a `fuchsia.io/Openable`
/// client end, via [Into<ClientEnd<fio::Openable>>]. FIDL open requests on this endpoint
/// translate to [OpenFn] calls.
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
#[derive(Capability, Clone)]
pub struct Open {
    open_fn: Arc<OpenFn>,
    entry_type: fio::DirentType,
}

/// The function that will be called when this capability is opened.
pub type OpenFn =
    dyn Fn(ExecutionScope, fio::OpenFlags, vfs::path::Path, zx::Channel) -> () + Send + Sync;

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
        F: Fn(ExecutionScope, fio::OpenFlags, vfs::path::Path, zx::Channel) -> ()
            + Send
            + Sync
            + 'static,
    {
        Open { open_fn: Arc::new(open), entry_type }
    }

    /// Converts the [Open] capability into a [Directory] capability such that it will be
    /// opened with `open_flags`.
    pub fn into_directory(self, open_flags: fio::OpenFlags, scope: ExecutionScope) -> Directory {
        Directory::from_open(self, open_flags, scope)
    }

    /// Opens the corresponding entry.
    ///
    /// If `path` fails validation, the `server_end` will be closed with a corresponding
    /// epitaph and optionally an event.
    pub fn open(
        &self,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: impl ValidatePath,
        server_end: zx::Channel,
    ) {
        let path = path.validate();
        match path {
            Ok(path) => (self.open_fn)(scope.clone(), flags, path, server_end.into()),
            Err(error) => {
                let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
                send_on_open_with_error(describe, server_end.into(), error);
            }
        }
    }

    /// Returns an [`Open`] capability which will open with rights downscoped to
    /// `rights`, from the base [`Open`] object.
    ///
    /// The base capability is lazily exercised when the returned capability is exercised.
    pub fn downscope_rights(self, rights: fio::OpenFlags) -> Open {
        let downscoped: OnceLock<Open> = OnceLock::new();
        let entry_type = self.entry_type;
        Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  path: vfs::path::Path,
                  server_end: zx::Channel| {
                let open = downscoped.get_or_init(|| {
                    let (downscoped_client_end, downscoped_server_end) = zx::Channel::create();
                    self.open(scope.clone(), rights, vfs::path::Path::dot(), downscoped_server_end);
                    let openable: ClientEnd<fio::OpenableMarker> = downscoped_client_end.into();
                    Open::from(openable)
                });
                open.open(scope, flags, path, server_end);
            },
            entry_type,
        )
    }

    /// Returns an [`Open`] capability which will open paths relative to
    /// `relative_path` if non-empty, from the base [`Open`] object.
    ///
    /// The base capability is lazily exercised when the returned capability is exercised.
    pub fn downscope_path(self, relative_path: Path) -> Open {
        Open::new(
            move |scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  new_path: vfs::path::Path,
                  server_end: zx::Channel| {
                let path = join_path(&relative_path, new_path);
                self.open(scope, flags, path.as_str(), server_end);
            },
            fio::DirentType::Unknown,
        )
    }

    /// Turn the [Open] into a remote VFS node.
    ///
    /// Both `into_remote` and FIDL conversion will let us open the capability:
    ///
    /// * `into_remote` returns a [vfs::remote::Remote] that supports
    ///   [DirectoryEntry::open].
    /// * FIDL conversion returns a client endpoint and calls
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
                      relative_path: vfs::path::Path,
                      server_end: ServerEnd<fio::NodeMarker>| {
                    open(scope, flags, relative_path, server_end.into_channel().into())
                },
            ),
            self.entry_type,
        )
    }
}

impl fmt::Debug for Open {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Open")
            .field("open", &"[open function]")
            .field("entry_type", &self.entry_type)
            .finish()
    }
}

impl Capability for Open {
    fn try_into_open(self) -> Result<Open, ConversionError> {
        Ok(self)
    }
}

impl From<ClientEnd<fio::OpenableMarker>> for Open {
    fn from(value: ClientEnd<fio::OpenableMarker>) -> Self {
        // Open is one-way so a synchronous proxy is not going to block.
        let proxy = fio::OpenableSynchronousProxy::new(value.into_channel());
        Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
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

impl From<ClientEnd<fio::DirectoryMarker>> for Open {
    fn from(value: ClientEnd<fio::DirectoryMarker>) -> Self {
        let openable: ClientEnd<fio::OpenableMarker> = value.into_channel().into();
        openable.into()
    }
}

impl TryFrom<OneShotHandle> for Open {
    type Error = ConversionError;

    /// Attempts to convert into an Open that calls `fuchsia.io.Openable/Open` on the handle.
    ///
    /// The handle must be a channel that speaks the `Openable` protocol.
    fn try_from(one_shot: OneShotHandle) -> Result<Self, Self::Error> {
        let handle = one_shot
            .get_handle()
            .map_err(|err| ClonableError::from(anyhow!("could not get handle: {:?}", err)))?;

        let basic_info = handle.basic_info().map_err(|status| {
            ClonableError::from(anyhow!("failed to get handle info: {}", status))
        })?;
        if basic_info.object_type != zx::ObjectType::CHANNEL {
            return Err(ConversionError::NotSupported);
        }

        let openable = ClientEnd::<fio::OpenableMarker>::from(handle).into_proxy().unwrap();

        Ok(Self::new(
            move |_scope: vfs::execution_scope::ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                // TODO(b/306037927): Calling Open on a channel that doesn't speak Openable may
                // inadvertently close the channel.
                let _ = openable.open(
                    flags,
                    fio::ModeType::empty(),
                    relative_path.as_str(),
                    server_end.into(),
                );
            },
            // TODO(b/298112397): Determine a more accurate dirent type.
            fio::DirentType::Unknown,
        ))
    }
}

impl From<Open> for ClientEnd<fio::OpenableMarker> {
    /// Serves the `fuchsia.io.Openable` protocol for this Open and moves it into the registry.
    fn from(open: Open) -> Self {
        let (client_end, server_end) = create_endpoints::<fio::OpenableMarker>();
        let mut request_stream = server_end.into_stream().unwrap();

        let scope = ExecutionScope::new();
        // If this future is dropped, stop serving the connection.
        let guard = scopeguard::guard(scope.clone(), move |scope| {
            scope.shutdown();
        });

        let fut = {
            let open = open.clone();
            async move {
                let _guard = guard;
                while let Ok(Some(request)) = request_stream.try_next().await {
                    match request {
                        fio::OpenableRequest::Open { flags, mode: _mode, path, object, .. } => {
                            open.open(scope.clone(), flags, path, object.into_channel())
                        }
                    }
                }
                scope.wait().await;
            }
            .boxed()
        };

        // Move this capability into the registry.
        let task = fasync::Task::spawn(fut);
        registry::insert_with_task(Box::new(open), client_end.get_koid().unwrap(), task);

        client_end
    }
}

impl From<Open> for fsandbox::Capability {
    fn from(open: Open) -> Self {
        Self::Open(open.into())
    }
}

pub trait ValidatePath {
    fn validate(self) -> Result<vfs::path::Path, zx::Status>;
}

impl ValidatePath for vfs::path::Path {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        Ok(self)
    }
}

impl ValidatePath for String {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

impl ValidatePath for &str {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

/// A path type that supports efficient prepending and appending.
#[derive(Default, Debug, Clone)]
pub struct Path {
    pub segments: VecDeque<String>,
}

impl Path {
    pub fn new(path: &str) -> Path {
        debug_assert!(ValidatePath::validate(path).is_ok());
        let path = Path { segments: path.split("/").map(|s| s.to_owned()).collect() };
        debug_assert!(ValidatePath::validate(path.fuchsia_io_path().as_str()).is_ok());
        path
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn next(&mut self) -> Option<String> {
        self.segments.pop_front()
    }

    pub fn peek(&self) -> Option<&String> {
        self.segments.front()
    }

    pub fn prepend(&mut self, segment: String) {
        debug_assert!(ValidatePath::validate(segment.as_str()).is_ok());
        self.segments.push_front(segment);
        debug_assert!(ValidatePath::validate(self.fuchsia_io_path().as_str()).is_ok());
    }

    pub fn append(&mut self, segment: String) {
        debug_assert!(ValidatePath::validate(segment.as_str()).is_ok());
        self.segments.push_back(segment);
        debug_assert!(ValidatePath::validate(self.fuchsia_io_path().as_str()).is_ok());
    }

    /// Returns a path that will be valid for using in a `fuchsia.io/Directory.Open` operation.
    pub fn fuchsia_io_path(&self) -> String {
        if self.is_empty() {
            ".".to_owned()
        } else {
            self.segments.iter().map(String::as_str).collect::<Vec<&str>>().join("/")
        }
    }
}

fn join_path(base: &Path, mut relative: vfs::path::Path) -> String {
    let mut base = base.clone();
    while let Some(segment) = relative.next() {
        base.append(segment.to_owned());
    }
    base.fuchsia_io_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dict, Directory, OneShotHandle, Receiver};
    use anyhow::Result;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, spawn_stream_handler, ClientEnd, Proxy};
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use fuchsia_zircon::AsHandleRef;
    use futures::channel::{mpsc, oneshot};
    use futures::{StreamExt, TryStreamExt};
    use lazy_static::lazy_static;
    use std::sync::Mutex;
    use test_util::Counter;
    use vfs::{
        directory::{entry::DirectoryEntry, helper::DirectlyMutable, immutable::simple as pfs},
        execution_scope::ExecutionScope,
        name::Name,
    };

    #[fuchsia::test]
    async fn test_into_remote() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
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
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();
        assert_eq!(OPEN_COUNT.get(), 1);
    }

    #[fuchsia::test]
    async fn test_remote() {
        let (open_tx, open_rx) = oneshot::channel::<()>();
        let open_tx = Mutex::new(Some(open_tx));

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "path");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                drop(server_end);
                open_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            },
            fio::DirentType::Directory,
        );

        let client_end: ClientEnd<fio::OpenableMarker> = open.into();
        let client_end: ClientEnd<fio::DirectoryMarker> = client_end.into_channel().into();
        let client = client_end.into_proxy().unwrap();
        let (_client_end, server_end) = create_endpoints();
        client.open(fio::OpenFlags::DIRECTORY, fio::ModeType::empty(), "path", server_end).unwrap();
        drop(client);

        open_rx.await.unwrap();
    }

    #[fuchsia::test]
    async fn test_remote_not_used_if_not_written_to() {
        lazy_static! {
            static ref OPEN_COUNT: Counter = Counter::new(0);
        }

        let open = Open::new(
            move |_scope: ExecutionScope,
                  flags: fio::OpenFlags,
                  relative_path: vfs::path::Path,
                  server_end: zx::Channel| {
                assert_eq!(relative_path.into_string(), "");
                assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                OPEN_COUNT.inc();
                drop(server_end);
            },
            fio::DirentType::Directory,
        );

        assert_eq!(OPEN_COUNT.get(), 0);
        let client_end: ClientEnd<fio::OpenableMarker> = open.into();
        drop(client_end);
        assert_eq!(OPEN_COUNT.get(), 0);
    }

    async fn get_connection_rights(directory: Directory) -> Result<(i32, fio::OpenFlags)> {
        let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
        let client = client_end.into_proxy()?;
        let (status, flags) = client.get_flags().await?;
        Ok((status, flags))
    }

    async fn get_entries(directory: Directory) -> Result<Vec<String>> {
        let client_end: ClientEnd<fio::DirectoryMarker> = directory.into();
        let client = client_end.into_proxy()?;
        let entries = fuchsia_fs::directory::readdir(&client).await?;
        Ok(entries.into_iter().map(|entry| entry.name).collect())
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
        let directory = open.clone().into_directory(
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE,
            ExecutionScope::new(),
        );
        let (status, flags) = get_connection_rights(directory).await.unwrap();
        assert_eq!(status, zx::Status::OK.into_raw());
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        // Downscope the rights to read-only.
        let open = open.downscope_rights(fio::OpenFlags::RIGHT_READABLE);
        let directory = open.into_directory(
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::POSIX_WRITABLE,
            ExecutionScope::new(),
        );

        // Verify that the connection is read-only.
        let (status, flags) = get_connection_rights(directory).await.unwrap();
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
        let directory =
            open.clone().into_directory(fio::OpenFlags::RIGHT_READABLE, ExecutionScope::new());
        let entries = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec!["foo".to_owned()]);

        // Downscope the path to `foo`.
        let open = open.clone().downscope_path(crate::Path::new("foo"));
        let directory =
            Directory::from_open(open, fio::OpenFlags::RIGHT_READABLE, ExecutionScope::new());

        // Verify that the connection does not have anymore children, since `foo` has no children.
        let entries = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec![] as Vec<String>);
    }

    #[fuchsia::test]
    async fn invalid_path() {
        let open = Open::new(
            move |_scope: ExecutionScope,
                  _flags: fio::OpenFlags,
                  _relative_path: vfs::path::Path,
                  _server_end: zx::Channel| {
                panic!("should not reach here");
            },
            fio::DirentType::Directory,
        );

        let client_end: ClientEnd<fio::OpenableMarker> = open.into();
        let openable = client_end.into_proxy().unwrap();

        let (client_end, server_end) = fidl::endpoints::create_endpoints();
        openable.open(fio::OpenFlags::DESCRIBE, fio::ModeType::empty(), "..", server_end).unwrap();
        let proxy = client_end.into_proxy().unwrap();
        let mut event_stream = proxy.take_event_stream();

        let event = event_stream.try_next().await.unwrap().unwrap();
        let on_open = event.into_on_open_().unwrap();
        assert_eq!(on_open.0, zx::Status::INVALID_ARGS.into_raw());

        let event = event_stream.try_next().await;
        let error = event.unwrap_err();
        assert_matches!(
            error,
            fidl::Error::ClientChannelClosed { status, .. }
            if status == zx::Status::INVALID_ARGS
        );
    }

    #[fuchsia::test]
    fn join_path_test() {
        assert_eq!(join_path(&crate::Path::default(), ".".try_into().unwrap()), ".");
        assert_eq!(join_path(&crate::Path::default(), "abc".try_into().unwrap()), "abc");
        assert_eq!(join_path(&crate::Path::new("abc"), ".".try_into().unwrap()), "abc");
        assert_eq!(join_path(&crate::Path::new("abc"), "def".try_into().unwrap()), "abc/def");
    }

    /// Tests that a OneShotHandle to a channel that speaks `Openable` can be converted to Open.
    #[fuchsia::test]
    async fn test_from_openable_one_shot_handle() -> Result<()> {
        let (object_tx, mut object_rx) = mpsc::unbounded();

        let openable_proxy: fio::OpenableProxy = spawn_stream_handler(move |request| {
            let object_tx = object_tx.clone();
            async move {
                match request {
                    fio::OpenableRequest::Open { flags, mode, path, object, control_handle: _ } => {
                        assert_eq!(flags, fio::OpenFlags::DIRECTORY);
                        assert_eq!(mode, fio::ModeType::empty());
                        assert_eq!(&path, "");
                        object_tx.unbounded_send(object).unwrap();
                    }
                }
            }
        })?;

        let zx_handle: zx::Handle = openable_proxy.into_channel().unwrap().into_zx_channel().into();
        let handle: OneShotHandle = zx_handle.into();

        let open: Open = handle.try_into()?;

        // Opening should send a server end to Open.
        let scope = vfs::execution_scope::ExecutionScope::new();
        let (_dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        open.open(
            scope,
            fio::OpenFlags::DIRECTORY,
            ".".to_string(),
            dir_server_end.into_channel().into(),
        );

        let server_end = object_rx.next().await;
        assert!(server_end.is_some());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_sender_into_open() {
        let (receiver, sender) = Receiver::<()>::new();
        let open: Open = sender.try_into_open().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), ".".to_owned(), server_end);
        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.payload.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_sender_into_open_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let (receiver, sender) = Receiver::<()>::new();
        let open: Open = sender.try_into_open().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == zx::Status::NOT_DIR
        );
    }

    #[fuchsia::test]
    async fn test_sender_into_open_via_dict() {
        let dict = Dict::new();
        let (receiver, sender) = Receiver::<()>::new();
        dict.lock_entries().insert("echo".to_owned(), Box::new(sender));

        let open: Open = dict.try_into_open().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "echo".to_owned(), server_end);

        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.payload.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_sender_into_open_via_dict_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let dict = Dict::new();
        let (receiver, sender) = Receiver::<()>::new();
        dict.lock_entries().insert("echo".to_owned(), Box::new(sender));

        let open: Open = dict.try_into_open().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "echo/foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == zx::Status::NOT_DIR
        );
    }
}
