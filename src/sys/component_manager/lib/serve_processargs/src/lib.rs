// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fuchsia_runtime::{HandleInfo, HandleType};
use futures::channel::mpsc::UnboundedSender;
use process_builder::StartupHandle;
use processargs::ProcessArgs;
use sandbox::{Capability, Dict, DictKey};
use std::collections::HashMap;
use std::iter::once;
use thiserror::Error;
use vfs::execution_scope::ExecutionScope;

mod namespace;

pub use crate::namespace::{ignore_not_found, BuildNamespaceError, NamespaceBuilder};

/// How to deliver a particular capability from a dict to an Elf process. Broadly speaking,
/// one could either deliver a capability using namespace entries, or using numbered handles.
pub enum Delivery {
    /// Install the capability as a `fuchsia.io` object, within some parent directory serviced by
    /// the framework, and discoverable at a path such as "/svc/foo/bar".
    ///
    /// As a result, a namespace entry will be created in the resulting processargs, corresponding
    /// to the parent directory, e.g. "/svc/foo".
    ///
    /// For example, installing a `sandbox::Open` at "/svc/fuchsia.examples.Echo" will
    /// cause the framework to spin up a `fuchsia.io/Directory` implementation backing "/svc",
    /// containing a filesystem object named "fuchsia.examples.Echo".
    ///
    /// Not all capability types are installable as `fuchsia.io` objects. A one-shot handle is not
    /// supported because `fuchsia.io` does not have a protocol for delivering one-shot handles.
    /// Use [Delivery::Handle] for those.
    NamespacedObject(cm_types::Path),

    /// Install the capability as a `fuchsia.io` object by creating a namespace entry at the
    /// provided path. The difference between [Delivery::NamespacedObject] and
    /// [Delivery::NamespaceEntry] is that the former will create a namespace entry at the parent
    /// directory.
    ///
    /// For example, installing a `sandbox::Open` at "/data" will result in a namespace entry
    /// at "/data". A request will be sent to the capability when the user writes to the
    /// namespace entry.
    NamespaceEntry(cm_types::Path),

    /// Installs the Zircon handle representation of this capability at the processargs slot
    /// described by [HandleInfo].
    ///
    /// The following handle types are disallowed because they will collide with the implementation
    /// of incoming namespace and outgoing directory:
    ///
    /// - [HandleType::NamespaceDirectory]
    /// - [HandleType::DirectoryRequest]
    ///
    Handle(HandleInfo),
}

pub enum DeliveryMapEntry {
    Delivery(Delivery),
    Dict(DeliveryMap),
}

/// A nested dictionary mapping capability names to delivery method.
///
/// Each entry in a [Dict] should have a corresponding entry here describing how the
/// capability will be delivered to the process. If a [Dict] has a nested [Dict], then there
/// will be a corresponding nested [DeliveryMapEntry::Dict] containing the [DeliveryMap] for the
/// capabilities in the nested [Dict].
pub type DeliveryMap = HashMap<DictKey, DeliveryMapEntry>;

/// Visits `dict` and installs its capabilities into appropriate locations in the
/// `processargs`, as determined by a `delivery_map`.
///
/// If the process opens non-existent paths within one of the namespace entries served
/// by the framework, that path will be sent down `not_found`. Callers should either monitor
/// the stream, or drop the receiver, to prevent unbounded buffering.
///
/// On success, returns a future that services the namespace.
pub fn add_to_processargs(
    scope: ExecutionScope,
    dict: Dict,
    processargs: &mut ProcessArgs,
    delivery_map: &DeliveryMap,
    not_found: UnboundedSender<String>,
) -> Result<(), DeliveryError> {
    let mut namespace = NamespaceBuilder::new(scope, not_found);

    // Iterate over the delivery map.
    // Take entries away from dict and install them accordingly.
    visit_map(delivery_map, dict, &mut |cap: Capability, delivery: &Delivery| match delivery {
        Delivery::NamespacedObject(path) => {
            namespace.add_object(cap, path.as_ref()).map_err(DeliveryError::NamespaceError)
        }
        Delivery::NamespaceEntry(path) => {
            namespace.add_entry(cap, path.as_ref()).map_err(DeliveryError::NamespaceError)
        }
        Delivery::Handle(info) => {
            processargs.add_handles(once(translate_handle(cap, info)?));
            Ok(())
        }
    })?;

    let namespace = namespace.serve().map_err(DeliveryError::NamespaceError)?;
    let namespace: Vec<_> = namespace.into();
    processargs.namespace_entries.extend(namespace);

    Ok(())
}

#[derive(Error, Debug)]
pub enum DeliveryError {
    #[error("the key `{0}` is not found in the dict")]
    NotInDict(DictKey),

    #[error("wrong type: the delivery map expected `{0}` to be a nested Dict in the dict")]
    NotADict(DictKey),

    #[error("unused capabilities in dict: `{0:?}`")]
    UnusedCapabilities(Vec<DictKey>),

    #[error("handle type `{0:?}` is not allowed to be installed into processargs")]
    UnsupportedHandleType(HandleType),

    #[error("namespace configuration error: `{0}`")]
    NamespaceError(namespace::BuildNamespaceError),

    #[error("capability `{0:?}` is not allowed to be installed into processargs")]
    UnsupportedCapability(Capability),
}

fn translate_handle(cap: Capability, info: &HandleInfo) -> Result<StartupHandle, DeliveryError> {
    validate_handle_type(info.handle_type())?;

    let one_shot = match cap.clone() {
        Capability::OneShotHandle(h) => h,
        c => return Err(DeliveryError::UnsupportedCapability(c)),
    };
    let handle = one_shot.get_handle().map_err(|_| DeliveryError::UnsupportedCapability(cap))?;

    Ok(StartupHandle { handle, info: *info })
}

fn visit_map(
    map: &DeliveryMap,
    dict: Dict,
    f: &mut impl FnMut(Capability, &Delivery) -> Result<(), DeliveryError>,
) -> Result<(), DeliveryError> {
    let mut entries = dict.lock_entries();
    for (key, entry) in map {
        match entries.remove(key) {
            Some(value) => match entry {
                DeliveryMapEntry::Delivery(delivery) => f(value, delivery)?,
                DeliveryMapEntry::Dict(sub_map) => {
                    let nested_dict: Dict = match value {
                        Capability::Dictionary(d) => d,
                        _ => return Err(DeliveryError::NotADict(key.to_owned())),
                    };
                    visit_map(sub_map, nested_dict, f)?;
                }
            },
            None => return Err(DeliveryError::NotInDict(key.to_owned())),
        }
    }
    if !entries.is_empty() {
        let keys = entries.keys().cloned().collect();
        return Err(DeliveryError::UnusedCapabilities(keys));
    }
    Ok(())
}

fn validate_handle_type(handle_type: HandleType) -> Result<(), DeliveryError> {
    match handle_type {
        HandleType::NamespaceDirectory | HandleType::DirectoryRequest => {
            Err(DeliveryError::UnsupportedHandleType(handle_type))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod test_util {
    use {
        fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
        fuchsia_zircon::HandleBased,
        sandbox::{OneShotHandle, Open},
        vfs::{
            directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path, service,
        },
    };

    pub struct Receiver(pub async_channel::Receiver<OneShotHandle>);

    pub fn multishot() -> (Open, Receiver) {
        let (sender, receiver) = async_channel::unbounded::<OneShotHandle>();

        let open_fn = move |scope: ExecutionScope, channel: fasync::Channel| {
            let sender = sender.clone();
            scope.spawn(async move {
                let capability = OneShotHandle::from(channel.into_zx_channel().into_handle());
                let _ = sender.send(capability).await;
            });
        };
        let service = service::endpoint(open_fn);

        let open_fn = move |scope: ExecutionScope,
                            flags: fio::OpenFlags,
                            path: vfs::path::Path,
                            server_end: zx::Channel| {
            service.clone().open(scope, flags, path, server_end.into());
        };

        let open = Open::new(open_fn, fio::DirentType::Service);

        (open, Receiver(receiver))
    }

    pub fn open() -> (Open, async_channel::Receiver<(Path, zx::Channel)>) {
        let (sender, receiver) = async_channel::unbounded::<(Path, zx::Channel)>();
        let open_fn = move |scope: ExecutionScope,
                            _flags: fio::OpenFlags,
                            relative_path: Path,
                            server_end: zx::Channel| {
            let sender = sender.clone();
            scope.spawn(async move {
                sender.send((relative_path, server_end)).await.unwrap();
            })
        };
        let open = Open::new(open_fn, fio::DirentType::Directory);

        (open, receiver)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::namespace::{ignore_not_found as ignore, BuildNamespaceError},
        anyhow::Result,
        assert_matches::assert_matches,
        fidl::endpoints::{Proxy, ServerEnd},
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon as zx,
        fuchsia_zircon::{AsHandleRef, HandleBased, Peered, Signals, Time},
        futures::TryStreamExt,
        maplit::hashmap,
        sandbox::OneShotHandle,
        std::pin::pin,
        std::str::FromStr,
        test_util::{multishot, open},
    };

    #[fuchsia::test]
    async fn test_empty() -> Result<()> {
        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        let delivery_map = DeliveryMap::new();
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 0);

        drop(processargs);
        scope.wait().await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_handle() -> Result<()> {
        let (sock0, sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        dict.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle().into_handle())),
        );
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
            )
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 1);

        assert_eq!(processargs.handles[0].info.handle_type(), HandleType::FileDescriptor);
        assert_eq!(processargs.handles[0].info.arg(), 0);

        // Test connectivity.
        const PAYLOAD: &'static [u8] = b"Hello";
        let handles = std::mem::take(&mut processargs.handles);
        let sock0 = zx::Socket::from(handles.into_iter().next().unwrap().handle);
        assert_eq!(sock0.write(PAYLOAD).unwrap(), 5);
        let mut buf = [0u8; PAYLOAD.len() + 1];
        assert_eq!(sock1.read(&mut buf[..]), Ok(PAYLOAD.len()));
        assert_eq!(&buf[..PAYLOAD.len()], PAYLOAD);

        drop(processargs);
        scope.wait().await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_create_nested_dict() -> Result<()> {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();

        // Put a socket at "/handles/stdin". This implements a capability bundling pattern.
        let handles = Dict::new();
        handles.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle())),
        );
        let dict = Dict::new();
        dict.lock_entries().insert("handles".to_string(), Capability::Dictionary(handles));

        let delivery_map = hashmap! {
            "handles".to_string() => DeliveryMapEntry::Dict(hashmap! {
                "stdin".to_string() => DeliveryMapEntry::Delivery(
                    Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
                )
            })
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 1);

        assert_eq!(processargs.handles[0].info.handle_type(), HandleType::FileDescriptor);
        assert_eq!(processargs.handles[0].info.arg(), 0);

        drop(processargs);
        scope.wait().await;
        Ok(())
    }

    /// Test accessing capabilities from a Dict inside a Dict.
    #[fuchsia::test]
    async fn test_access_in_nested_dict() -> Result<()> {
        let (ep0, ep1) = zx::EventPair::create();

        let mut processargs = ProcessArgs::new();

        let handles = Dict::new();
        handles.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(ep0.into_handle())),
        );
        let dict = Dict::new();
        dict.lock_entries().insert("handles".to_string(), Capability::Dictionary(handles));

        let delivery_map = hashmap! {
            "handles".to_string() => DeliveryMapEntry::Dict(hashmap! {
                "stdin".to_string() => DeliveryMapEntry::Delivery(
                    Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
                )
            })
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 1);

        assert_eq!(processargs.handles[0].info.handle_type(), HandleType::FileDescriptor);
        assert_eq!(processargs.handles[0].info.arg(), 0);

        let ep0 = processargs.handles.pop().unwrap().handle;
        ep1.signal_peer(Signals::NONE, Signals::USER_1).unwrap();
        assert_eq!(ep0.wait_handle(Signals::USER_1, Time::INFINITE).unwrap(), Signals::USER_1);

        drop(ep0);
        drop(processargs);
        scope.wait().await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_wrong_dict_destructuring() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();

        // The type of "/handles" is a socket capability but we try to open it as a dict and extract
        // a "stdin" inside. This should fail.
        let dict = Dict::new();
        dict.lock_entries().insert(
            "handles".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle())),
        );

        let delivery_map = hashmap! {
            "handles".to_string() => DeliveryMapEntry::Dict(hashmap! {
                "stdin".to_string() => DeliveryMapEntry::Delivery(
                    Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
                )
            })
        };

        assert_matches!(
            add_to_processargs(ExecutionScope::new(), dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::NotADict(name)
            if &name == "handles"
        );
    }

    #[fuchsia::test]
    async fn test_handle_unused() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        dict.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle())),
        );
        let delivery_map = DeliveryMap::new();

        assert_matches!(
            add_to_processargs(ExecutionScope::new(), dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::UnusedCapabilities(keys)
            if keys == vec![DictKey::from("stdin")]
        );
    }

    #[fuchsia::test]
    async fn test_handle_unsupported() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        dict.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle())),
        );
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::DirectoryRequest, 0))
            )
        };

        assert_matches!(
            add_to_processargs(ExecutionScope::new(), dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::UnsupportedHandleType(handle_type)
            if handle_type == HandleType::DirectoryRequest
        );
    }

    #[fuchsia::test]
    async fn test_handle_not_found() {
        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
            )
        };

        assert_matches!(
            add_to_processargs(ExecutionScope::new(), dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::NotInDict(name)
            if &name == "stdin"
        );
    }

    /// Two protocol capabilities in `/svc`. One of them has a receiver waiting for incoming
    /// requests. The other is disconnected from the receiver, which should close all incoming
    /// connections to that protocol.
    #[fuchsia::test]
    async fn test_namespace_object_end_to_end() -> Result<()> {
        let (open, receiver) = multishot();
        let peer_closed_open = multishot().0;

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        {
            let mut entries = dict.lock_entries();
            entries.insert("normal".to_string(), Capability::Open(open));
            entries.insert("closed".to_string(), Capability::Open(peer_closed_open));
        }
        let delivery_map = hashmap! {
            "normal".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Normal").unwrap())
            ),
            "closed".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Closed").unwrap())
            )
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.handles.len(), 0);
        assert_eq!(processargs.namespace_entries.len(), 1);
        let entry = processargs.namespace_entries.pop().unwrap();
        assert_eq!(entry.path.to_str().unwrap(), "/svc");

        // Check that there are the expected two protocols inside the svc directory.
        let dir = entry.directory.into_proxy().unwrap();
        let mut entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();
        let mut expectation = vec![
            DirEntry { name: "fuchsia.Normal".to_string(), kind: fio::DirentType::Service },
            DirEntry { name: "fuchsia.Closed".to_string(), kind: fio::DirentType::Service },
        ];
        entries.sort();
        expectation.sort();
        assert_eq!(entries, expectation);

        let dir = dir.into_channel().unwrap().into_zx_channel();

        // Connect to the protocol using namespace functionality.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Normal", server_end).unwrap();

        // Make sure the server_end is received, and test connectivity.
        let server_end: zx::Channel = receiver.0.recv().await.unwrap().get_handle().unwrap().into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).unwrap();

        // Connect to the closed protocol. Because the receiver is discarded, anything we send
        // should get peer-closed.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Closed", server_end).unwrap();
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();

        drop(dir);
        drop(processargs);
        scope.wait().await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_namespace_scope_shutdown() -> Result<()> {
        let (open, receiver) = multishot();

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        {
            let mut entries = dict.lock_entries();
            entries.insert("normal".to_string(), Capability::Open(open));
        }
        let delivery_map = hashmap! {
            "normal".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Normal").unwrap())
            ),
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.handles.len(), 0);
        assert_eq!(processargs.namespace_entries.len(), 1);
        let entry = processargs.namespace_entries.pop().unwrap();
        assert_eq!(entry.path.to_str().unwrap(), "/svc");

        let dir = entry.directory.into_proxy().unwrap();
        let dir = dir.into_channel().unwrap().into_zx_channel();

        // Connect to the protocol using namespace functionality.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Normal", server_end).unwrap();

        // Make sure the server_end is received, and test connectivity.
        let server_end: zx::Channel = receiver.0.recv().await.unwrap().get_handle().unwrap().into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).unwrap();

        // Shutdown the execution scope.
        scope.shutdown();
        scope.wait().await;

        // Connect to the protocol again. This time, because the namespace was shutdown, anything we send
        // should get peer-closed.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Normal", server_end).unwrap();
        fasync::Channel::from_channel(client_end).on_closed().await.unwrap();

        drop(dir);
        drop(processargs);
        Ok(())
    }

    /// Install an `Open` capability at "/data". Test that opening "/data/abc" means the
    /// open capability receives a request to open the current node, and the server endpoint
    /// in that request has buffered an open request for "abc". This replicates what
    /// component_manager does for directory capabilities.
    #[test]
    fn test_namespace_entry_end_to_end() -> Result<()> {
        use futures::task::Poll;
        let mut exec = fasync::TestExecutor::new();
        let (open, receiver) = open();
        let directory = open.into_directory(fio::OpenFlags::DIRECTORY, ExecutionScope::new());

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        dict.lock_entries().insert("data".to_string(), Capability::Directory(directory));
        let delivery_map = hashmap! {
            "data".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespaceEntry(cm_types::Path::from_str("/data").unwrap())
            ),
        };
        let scope = ExecutionScope::new();
        add_to_processargs(scope.clone(), dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.handles.len(), 0);
        assert_eq!(processargs.namespace_entries.len(), 1);
        let entry = processargs.namespace_entries.pop().unwrap();
        assert_eq!(entry.path.to_str().unwrap(), "/data");

        // No request yet. Not until we write to the client endpoint.
        assert_matches!(exec.run_until_stalled(&mut receiver.recv()), Poll::Pending);

        let dir = entry.directory.into_proxy().unwrap();
        let dir = dir.into_channel().unwrap().into_zx_channel();
        let (client_end, server_end) = zx::Channel::create();

        // Test that the flags are passed correctly.
        let flags_for_abc = fio::OpenFlags::DIRECTORY | fio::OpenFlags::DESCRIBE;
        fdio::open_at(&dir, "abc", flags_for_abc, server_end).unwrap();

        // Capability is opened with "." and a `server_end`.
        let (relative_path, server_end) = exec.run_singlethreaded(&mut receiver.recv()).unwrap();
        assert!(relative_path.is_dot());

        // Verify there is an open message for "abc" with `flags_for_abc` on `server_end`.
        let server_end: ServerEnd<fio::DirectoryMarker> = server_end.into();
        let mut stream = server_end.into_stream().unwrap();
        let request = exec.run_singlethreaded(&mut stream.try_next()).unwrap().unwrap();
        assert_matches!(
            &request,
            fio::DirectoryRequest::Open { flags, path, .. }
            if path == "abc" && *flags == flags_for_abc
        );

        let client_end = fasync::Channel::from_channel(client_end.into());
        assert_matches!(exec.run_until_stalled(&mut client_end.on_closed()), Poll::Pending);
        // Drop the request, including the server endpoint.
        drop(request);
        // Client should observe that the server endpoint was dropped.
        exec.run_singlethreaded(&mut client_end.on_closed()).unwrap();

        drop(dir);
        drop(processargs);
        exec.run_singlethreaded(pin!(scope.wait()));
        Ok(())
    }

    #[fuchsia::test]
    async fn test_handle_unsupported_in_namespace() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        dict.lock_entries().insert(
            "stdin".to_string(),
            Capability::OneShotHandle(OneShotHandle::from(sock0.into_handle())),
        );
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Normal").unwrap())
            )
        };

        assert_matches!(
            add_to_processargs(ExecutionScope::new(), dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::NamespaceError(BuildNamespaceError::Conversion {
                path, ..
            })
            if path.as_ref() == "/svc"
        );
    }
}
