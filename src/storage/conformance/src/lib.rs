// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(missing_docs)]

//! Crate to provide fidl logging and test setup helpers for conformance tests
//! for fuchsia.io.

use {
    async_trait::async_trait,
    fidl::{
        endpoints::{create_proxy, ClientEnd, ProtocolMarker, Proxy},
        prelude::*,
    },
    fidl_fuchsia_io as fio, fidl_fuchsia_io_test as io_test,
    fuchsia_async::{DurationExt, TimeoutExt},
    fuchsia_zircon as zx,
    futures::{StreamExt as _, TryStreamExt as _},
};

/// Test harness helper struct.
pub mod test_harness;

/// Utility functions for getting combinations of flags.
pub mod flags;

/// A common name for a file to create in a conformance test.
pub const TEST_FILE: &str = "testing.txt";

/// A common set of file contents to write into a test file in a conformance test.
pub const TEST_FILE_CONTENTS: &[u8] = "abcdef".as_bytes();

/// A default value for NodeAttributes, with zeros set for all fields.
pub const EMPTY_NODE_ATTRS: fio::NodeAttributes = fio::NodeAttributes {
    mode: 0,
    id: 0,
    content_size: 0,
    storage_size: 0,
    link_count: 0,
    creation_time: 0,
    modification_time: 0,
};

/// Wait for [`fio::NodeEvent::OnOpen_`] to be sent via `node_proxy` and returns its [`zx::Status`].
pub async fn get_open_status(node_proxy: &fio::NodeProxy) -> zx::Status {
    let mut events = Clone::clone(node_proxy).take_event_stream();
    if let Some(result) = events.next().await {
        match result.expect("FIDL error") {
            fio::NodeEvent::OnOpen_ { s, info: _ } => zx::Status::from_raw(s),
            fio::NodeEvent::OnRepresentation { .. } => panic!(
                "This function should only be used with fuchsia.io/Directory.Open, *not* Open2!"
            ),
        }
    } else {
        zx::Status::PEER_CLOSED
    }
}

/// Asserts that no [`fio::NodeEvent::OnOpen_`] event is sent on an opened proxy.
pub async fn assert_on_open_not_received(node_proxy: &fio::NodeProxy) {
    let mut events = Clone::clone(node_proxy).take_event_stream();
    // Wait at most 200ms for an OnOpen event to appear.
    let event =
        events.next().on_timeout(zx::Duration::from_millis(200).after_now(), || Option::None).await;
    assert!(event.is_none(), "Unexpected OnOpen event received");
}

/// Converts a generic [`fio::NodeProxy`] to either [`fio::FileProxy`] or [`fio::DirectoryProxy`].
/// **WARNING**: This function does _not_ verify that the conversion is valid.
pub fn convert_node_proxy<T: Proxy>(proxy: fio::NodeProxy) -> T {
    T::from_channel(proxy.into_channel().expect("Cannot convert node proxy to channel"))
}

/// Helper function to open the desired node in the root folder.
/// Asserts that open_node_status succeeds.
pub async fn open_node<T: ProtocolMarker>(
    dir: &fio::DirectoryProxy,
    flags: fio::OpenFlags,
    path: &str,
) -> T::Proxy {
    open_node_status::<T>(dir, flags, path)
        .await
        .unwrap_or_else(|e| panic!("open_node_status failed for {path} (flags={flags:?}): {e:?}"))
}

/// Helper function to open the desired node in the root folder.
pub async fn open_node_status<T: ProtocolMarker>(
    dir: &fio::DirectoryProxy,
    flags: fio::OpenFlags,
    path: &str,
) -> Result<T::Proxy, zx::Status> {
    let flags = flags | fio::OpenFlags::DESCRIBE;
    let (node_proxy, node_server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy");
    dir.open(flags, fio::ModeType::empty(), path, node_server).expect("Cannot open node");
    let status = get_open_status(&node_proxy).await;

    if status != zx::Status::OK {
        Err(status)
    } else {
        Ok(convert_node_proxy(node_proxy))
    }
}

/// Helper function to open a file with the given flags. Only use this if testing something other
/// than the open call directly.
pub async fn open_file_with_flags(
    parent_dir: &fio::DirectoryProxy,
    flags: fio::OpenFlags,
    path: &str,
) -> fio::FileProxy {
    open_node::<fio::FileMarker>(&parent_dir, flags | fio::OpenFlags::NOT_DIRECTORY, path).await
}

/// Helper function to open a sub-directory with the given flags. Only use this if testing
/// something other than the open call directly.
pub async fn open_dir_with_flags(
    parent_dir: &fio::DirectoryProxy,
    flags: fio::OpenFlags,
    path: &str,
) -> fio::DirectoryProxy {
    open_node::<fio::DirectoryMarker>(&parent_dir, flags | fio::OpenFlags::DIRECTORY, path).await
}

/// Helper function to open a sub-directory as readable and writable. Only use this if testing
/// something other than the open call directly.
pub async fn open_rw_dir(parent_dir: &fio::DirectoryProxy, path: &str) -> fio::DirectoryProxy {
    open_dir_with_flags(
        parent_dir,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        path,
    )
    .await
}

/// Helper function to call `get_token` on a directory. Only use this if testing something
/// other than the `get_token` call directly.
pub async fn get_token(dir: &fio::DirectoryProxy) -> fidl::Handle {
    let (status, token) = dir.get_token().await.expect("get_token failed");
    assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
    token.expect("handle missing")
}

/// Helper function to read a file and return its contents. Only use this if testing something other
/// than the read call directly.
pub async fn read_file(dir: &fio::DirectoryProxy, path: &str) -> Vec<u8> {
    let file = open_file_with_flags(dir, fio::OpenFlags::RIGHT_READABLE, path).await;
    file.read(100).await.expect("read failed").map_err(zx::Status::from_raw).expect("read error")
}

/// Attempts to open the given file, and checks the status is `NOT_FOUND`.
pub async fn assert_file_not_found(dir: &fio::DirectoryProxy, path: &str) {
    let (file_proxy, file_server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy");
    dir.open(
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::DESCRIBE,
        fio::ModeType::empty(),
        path,
        file_server,
    )
    .expect("Cannot open file");
    assert_eq!(get_open_status(&file_proxy).await, zx::Status::NOT_FOUND);
}

/// Returns the .name field from a given DirectoryEntry, otherwise panics.
pub fn get_directory_entry_name(dir_entry: &io_test::DirectoryEntry) -> String {
    use io_test::DirectoryEntry;
    match dir_entry {
        DirectoryEntry::Directory(entry) => entry.name.as_ref(),
        DirectoryEntry::RemoteDirectory(entry) => entry.name.as_ref(),
        DirectoryEntry::File(entry) => entry.name.as_ref(),
        DirectoryEntry::VmoFile(entry) => entry.name.as_ref(),
        DirectoryEntry::ExecutableFile(entry) => entry.name.as_ref(),
    }
    .expect("DirectoryEntry name is None!")
    .clone()
}

/// Asserts that the given `vmo_rights` align with the `expected_vmo_rights` passed to a
/// get_backing_memory call. We check that the returned rights align with and do not exceed those
/// in the given flags, that we have at least basic VMO rights, and that the flags align with the
/// expected sharing mode.
pub fn validate_vmo_rights(vmo: &zx::Vmo, expected_vmo_rights: fio::VmoFlags) {
    let vmo_rights: zx::Rights = vmo.basic_info().expect("failed to get VMO info").rights;

    // Ensure that we have at least some basic rights.
    assert!(vmo_rights.contains(zx::Rights::BASIC));
    assert!(vmo_rights.contains(zx::Rights::MAP));
    assert!(vmo_rights.contains(zx::Rights::GET_PROPERTY));

    // Ensure the returned rights match and do not exceed those we requested in `expected_vmo_rights`.
    assert!(
        vmo_rights.contains(zx::Rights::READ) == expected_vmo_rights.contains(fio::VmoFlags::READ)
    );
    assert!(
        vmo_rights.contains(zx::Rights::WRITE)
            == expected_vmo_rights.contains(fio::VmoFlags::WRITE)
    );
    assert!(
        vmo_rights.contains(zx::Rights::EXECUTE)
            == expected_vmo_rights.contains(fio::VmoFlags::EXECUTE)
    );

    // Make sure we get SET_PROPERTY if we specified a private copy.
    if expected_vmo_rights.contains(fio::VmoFlags::PRIVATE_CLONE) {
        assert!(vmo_rights.contains(zx::Rights::SET_PROPERTY));
    }
}

/// Creates a directory with the given DirectoryEntry, opening the file with the given
/// file flags, and returning a Buffer object initialized with the given vmo_flags.
pub async fn create_file_and_get_backing_memory(
    dir_entry: io_test::DirectoryEntry,
    test_harness: &test_harness::TestHarness,
    file_flags: fio::OpenFlags,
    vmo_flags: fio::VmoFlags,
) -> Result<(zx::Vmo, (fio::DirectoryProxy, fio::FileProxy)), zx::Status> {
    let file_path = get_directory_entry_name(&dir_entry);
    let root = root_directory(vec![dir_entry]);
    let dir_proxy = test_harness.get_directory(root, file_flags);
    let file_proxy = open_node_status::<fio::FileMarker>(
        &dir_proxy,
        file_flags | fio::OpenFlags::NOT_DIRECTORY,
        &file_path,
    )
    .await?;
    let vmo = file_proxy
        .get_backing_memory(vmo_flags)
        .await
        .expect("get_backing_memory failed")
        .map_err(zx::Status::from_raw)?;
    Ok((vmo, (dir_proxy, file_proxy)))
}

/// Constructs a directory from a set of directory entries.
pub fn root_directory(entries: Vec<io_test::DirectoryEntry>) -> io_test::Directory {
    // Convert the simple vector of entries into the convoluted FIDL field type.
    let entries: Vec<Option<Box<io_test::DirectoryEntry>>> =
        entries.into_iter().map(|e| Some(Box::new(e))).collect();
    io_test::Directory { name: None, entries: Some(entries), ..Default::default() }
}

/// Makes a subdirectory with a name and a set of entries.
pub fn directory(name: &str, entries: Vec<io_test::DirectoryEntry>) -> io_test::DirectoryEntry {
    let mut dir = root_directory(entries);
    dir.name = Some(name.to_string());
    io_test::DirectoryEntry::Directory(dir)
}

/// Makes a remote directory with a name, which forwards the requests to the given directory proxy.
pub fn remote_directory(name: &str, remote_dir: fio::DirectoryProxy) -> io_test::DirectoryEntry {
    let remote_client = ClientEnd::<fio::DirectoryMarker>::new(
        remote_dir.into_channel().unwrap().into_zx_channel(),
    );

    io_test::DirectoryEntry::RemoteDirectory(io_test::RemoteDirectory {
        name: Some(name.to_string()),
        remote_client: Some(remote_client),
        ..Default::default()
    })
}

/// Makes a file to be placed in the test directory.
pub fn file(name: &str, contents: Vec<u8>) -> io_test::DirectoryEntry {
    io_test::DirectoryEntry::File(io_test::File {
        name: Some(name.to_string()),
        contents: Some(contents),
        ..Default::default()
    })
}

/// Makes a vmo file to be placed in the test directory.
pub fn vmo_file(name: &str, contents: &[u8], capacity: u64) -> io_test::DirectoryEntry {
    let vmo = zx::Vmo::create(capacity).expect("Cannot create VMO");
    let () = vmo.write(contents, 0).expect("Cannot write to VMO");
    let () = vmo.set_content_size(&(contents.len() as u64)).expect("Cannot set VMO content size");
    io_test::DirectoryEntry::VmoFile(io_test::VmoFile {
        name: Some(name.to_string()),
        vmo: Some(vmo),
        ..Default::default()
    })
}

/// Makes an executable file to be placed in the test directory.
pub fn executable_file(name: &str) -> io_test::DirectoryEntry {
    io_test::DirectoryEntry::ExecutableFile(io_test::ExecutableFile {
        name: Some(name.to_string()),
        ..Default::default()
    })
}

/// Extension trait for [`fio::DirectoryProxy`] to make interactions with the fuchsia.io protocol
/// less verbose.
#[async_trait]
pub trait DirectoryProxyExt {
    /// Open `path` using `node_options`, returning a proxy to the remote resource.
    ///
    /// Waits for [`fio::NodeEvent::OnRepresentation`] if [`fio::NodeFlags::GET_REPRESENTATION`]
    /// is specified, otherwise calls `fuchsia.io/Node.GetConnectionInfo` to verify the result.
    async fn open2_node<T: ProtocolMarker>(
        &self,
        path: &str,
        node_options: fio::NodeOptions,
    ) -> Result<T::Proxy, zx::Status>;

    /// Similar to [`DirectoryProxyExt::open2_node`], but waits for and returns the
    /// [`fio::NodeEvent::OnRepresentation`] event sent when opening a resource.
    ///
    /// Requires [`fio::NodeFlags::GET_REPRESENTATION`] to be specified in `node_options`.
    async fn open2_node_get_representation<T: ProtocolMarker>(
        &self,
        path: &str,
        node_options: fio::NodeOptions,
    ) -> Result<(T::Proxy, fio::Representation), zx::Status>;
}

#[async_trait]
impl DirectoryProxyExt for fio::DirectoryProxy {
    async fn open2_node<T: ProtocolMarker>(
        &self,
        path: &str,
        node_options: fio::NodeOptions,
    ) -> Result<T::Proxy, zx::Status> {
        Ok(open2_node_impl::<T>(self, path, node_options).await?.0)
    }

    async fn open2_node_get_representation<T: ProtocolMarker>(
        &self,
        path: &str,
        node_options: fio::NodeOptions,
    ) -> Result<(T::Proxy, fio::Representation), zx::Status> {
        let get_representation = node_options
            .flags
            .is_some_and(|flags| flags.contains(fio::NodeFlags::GET_REPRESENTATION));
        assert!(
            get_representation,
            "node_options must specify the GET_REPRESENTATION flag to use this function!"
        );
        let (proxy, on_representation) = open2_node_impl::<T>(self, path, node_options).await?;
        Ok((proxy, on_representation.unwrap()))
    }
}

async fn open2_node_impl<T: ProtocolMarker>(
    dir: &fio::DirectoryProxy,
    path: &str,
    node_options: fio::NodeOptions,
) -> Result<(T::Proxy, Option<fio::Representation>), zx::Status> {
    let get_representation =
        node_options.flags.is_some_and(|flags| flags.contains(fio::NodeFlags::GET_REPRESENTATION));
    let (proxy, server) = create_proxy::<fio::NodeMarker>().expect("Cannot create proxy");
    dir.open2(path, &fio::ConnectionProtocols::Node(node_options), server.into_channel())
        .expect("Failed to call open2");
    if get_representation {
        // Wait for the OnRepresentation event to verify if opening the resource succeeded.
        let representation = Some(get_on_representation_event(&proxy).await?);
        return Ok((convert_node_proxy(proxy), representation));
    }
    // Protocols didn't specify GET_REPRESENTATION, call GetConnectionInfo to test that opening the
    // resource succeeded. If that fails, return the epitaph from the channel closure.
    proxy.get_connection_info().await.map_err(|e| {
        if let fidl::Error::ClientChannelClosed { status, .. } = e {
            status
        } else {
            panic!("Unhandled FIDL error: {:?}", e);
        }
    })?;
    Ok((convert_node_proxy(proxy), None))
}

/// Wait for and return a [`fio::NodeEvent::OnRepresentation`] event sent via `node_proxy`.
async fn get_on_representation_event(
    node_proxy: &fio::NodeProxy,
) -> Result<fio::Representation, zx::Status> {
    // Try to extract the expected NodeEvent, but map channel epitaphs to zx::Status.
    let event = Clone::clone(node_proxy)
        .take_event_stream()
        .try_next()
        .await
        .map_err(|e| {
            if let fidl::Error::ClientChannelClosed { status, .. } = e {
                status
            } else {
                panic!("Unhandled FIDL error: {:?}", e);
            }
        })?
        .expect("Missing NodeEvent in stream!");
    let representation = match event {
        fio::NodeEvent::OnRepresentation { payload } => payload,
        _ => panic!("Found unexpected NodeEvent type in stream!"),
    };
    Ok(representation)
}
