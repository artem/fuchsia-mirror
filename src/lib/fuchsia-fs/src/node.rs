// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utility functions for fuchsia.io nodes.

use {
    fidl_fuchsia_io as fio, fuchsia_zircon_status as zx_status, futures::prelude::*,
    thiserror::Error,
};

#[cfg(target_os = "fuchsia")]
use fuchsia_zircon as zx;

/// An error encountered while opening a node
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum OpenError {
    #[error("while making a fidl proxy: {0}")]
    CreateProxy(#[source] fidl::Error),

    #[error("while opening from namespace: {0}")]
    Namespace(#[source] zx_status::Status),

    #[error("while sending open request: {0}")]
    SendOpenRequest(#[source] fidl::Error),

    #[error("node event stream closed prematurely")]
    OnOpenEventStreamClosed,

    #[error("while reading OnOpen event: {0}")]
    OnOpenDecode(#[source] fidl::Error),

    #[error("open failed with status: {0}")]
    OpenError(#[source] zx_status::Status),

    #[error("remote responded with success but provided no node info")]
    MissingOnOpenInfo,

    #[error("expected node to be a {expected:?}, but got a {actual:?}")]
    UnexpectedNodeKind { expected: Kind, actual: Kind },
}

/// An error encountered while cloning a node
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum CloneError {
    #[error("while making a fidl proxy: {0}")]
    CreateProxy(#[source] fidl::Error),

    #[error("while sending clone request: {0}")]
    SendCloneRequest(#[source] fidl::Error),
}

/// An error encountered while closing a node
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum CloseError {
    #[error("while sending close request: {0}")]
    SendCloseRequest(#[source] fidl::Error),

    #[error("close failed with status: {0}")]
    CloseError(#[source] zx_status::Status),
}

/// An error encountered while renaming a node
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum RenameError {
    #[error("while sending rename request")]
    SendRenameRequest(#[source] fidl::Error),

    #[error("while sending get_token request")]
    SendGetTokenRequest(#[source] fidl::Error),

    #[error("rename failed with status")]
    RenameError(#[source] zx_status::Status),

    #[error("while opening subdirectory")]
    OpenError(#[from] OpenError),

    #[error("get_token failed with status")]
    GetTokenError(#[source] zx_status::Status),

    #[error("no handle from get token")]
    NoHandleError,
}

/// The type of a filesystem node
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Kind {
    Service,
    File,
    Directory,
    Pipe,
    Vmofile,
    Device,
    Tty,
    SynchronousDatagramSocket,
    StreamSocket,
    RawSocket,
    PacketSocket,
    DatagramSocket,
    Unknown,
}

impl Kind {
    fn kind_of(info: &fio::NodeInfo) -> Kind {
        match info {
            fio::NodeInfo::Service(_) => Kind::Service,
            fio::NodeInfo::File(_) => Kind::File,
            fio::NodeInfo::Directory(_) => Kind::Directory,
            fio::NodeInfo::Pipe(_) => Kind::Pipe,
            fio::NodeInfo::Vmofile(_) => Kind::Vmofile,
            fio::NodeInfo::Device(_) => Kind::Device,
            fio::NodeInfo::Tty(_) => Kind::Tty,
            fio::NodeInfo::SynchronousDatagramSocket(_) => Kind::SynchronousDatagramSocket,
            fio::NodeInfo::StreamSocket(_) => Kind::StreamSocket,
            fio::NodeInfo::RawSocket(_) => Kind::RawSocket,
            fio::NodeInfo::PacketSocket(_) => Kind::PacketSocket,
            fio::NodeInfo::DatagramSocket(_) => Kind::DatagramSocket,
        }
    }

    fn expect_file(info: fio::NodeInfo) -> Result<(), Kind> {
        match info {
            fio::NodeInfo::File(fio::FileObject { event: _, stream: None })
            | fio::NodeInfo::Vmofile(fio::Vmofile { .. }) => Ok(()),
            other => Err(Kind::kind_of(&other)),
        }
    }

    fn expect_directory(info: fio::NodeInfo) -> Result<(), Kind> {
        match info {
            fio::NodeInfo::Directory(fio::DirectoryObject) => Ok(()),
            other => Err(Kind::kind_of(&other)),
        }
    }

    fn kind_of2(representation: &fio::Representation) -> Kind {
        match representation {
            fio::Representation::Connector(_) => Kind::Service,
            fio::Representation::Directory(_) => Kind::Directory,
            fio::Representation::File(_) => Kind::File,
            fio::Representation::Memory(_) => Kind::Vmofile,
            fio::Representation::Pipe(_) => Kind::Pipe,
            fio::Representation::Device(_) => Kind::Device,
            fio::Representation::Tty(_) => Kind::Tty,
            fio::Representation::SynchronousDatagramSocket(_) => Kind::SynchronousDatagramSocket,
            fio::Representation::StreamSocket(_) => Kind::StreamSocket,
            fio::Representation::RawSocket(_) => Kind::RawSocket,
            fio::Representation::PacketSocket(_) => Kind::PacketSocket,
            fio::Representation::DatagramSocket(_) => Kind::DatagramSocket,
            _ => Kind::Unknown,
        }
    }

    fn expect_file2(representation: &fio::Representation) -> Result<(), Kind> {
        match representation {
            fio::Representation::File(fio::FileInfo { stream: None, .. })
            | fio::Representation::Memory(_) => Ok(()),
            other => Err(Kind::kind_of2(other)),
        }
    }

    fn expect_directory2(representation: &fio::Representation) -> Result<(), Kind> {
        match representation {
            fio::Representation::Directory(_) => Ok(()),
            other => Err(Kind::kind_of2(other)),
        }
    }
}

// TODO namespace.connect is synchronous and may involve fdio making synchronous fidl calls to
// remote directories.  If/when fdio exposes the root namespace mapping or an API to connect to
// nodes asynchronously, this function should be updated to use that.
/// Connect a zx::Channel to a path in the current namespace.
#[cfg(target_os = "fuchsia")]
pub fn connect_in_namespace(
    path: &str,
    flags: fio::OpenFlags,
    chan: zx::Channel,
) -> Result<(), zx_status::Status> {
    let namespace = fdio::Namespace::installed()?;
    namespace.open(path, flags, chan)
}

/// Opens the given `path` from the current namespace as a [`NodeProxy`]. The target is not
/// verified to be any particular type and may not implement the fuchsia.io.Node protocol.
#[cfg(target_os = "fuchsia")]
pub fn open_in_namespace(path: &str, flags: fio::OpenFlags) -> Result<fio::NodeProxy, OpenError> {
    let (node, server_end) =
        fidl::endpoints::create_proxy::<fio::NodeMarker>().map_err(OpenError::CreateProxy)?;

    connect_in_namespace(path, flags, server_end.into_channel()).map_err(OpenError::Namespace)?;

    Ok(node)
}

/// Gracefully closes the node proxy from the remote end.
pub async fn close(node: fio::NodeProxy) -> Result<(), CloseError> {
    let result = node.close().await.map_err(CloseError::SendCloseRequest)?;
    result.map_err(|s| CloseError::CloseError(zx_status::Status::from_raw(s)))
}

/// Consume the first event from this NodeProxy's event stream, returning the proxy if it is
/// the expected type or an error otherwise.
pub(crate) async fn verify_node_describe_event(
    node: fio::NodeProxy,
) -> Result<fio::NodeProxy, OpenError> {
    let mut events = node.take_event_stream();
    match events
        .next()
        .await
        .ok_or(OpenError::OnOpenEventStreamClosed)?
        .map_err(OpenError::OnOpenDecode)?
    {
        fio::NodeEvent::OnOpen_ { s: status, info } => {
            let () = zx_status::Status::ok(status).map_err(OpenError::OpenError)?;
            info.ok_or(OpenError::MissingOnOpenInfo)?;
        }
        fio::NodeEvent::OnConnectionInfo { .. } => (),
    }

    Ok(node)
}

/// Consume the first event from this DirectoryProxy's event stream, returning the proxy if it is
/// the expected type or an error otherwise.
pub(crate) async fn verify_directory_describe_event(
    node: fio::DirectoryProxy,
) -> Result<fio::DirectoryProxy, OpenError> {
    let mut events = node.take_event_stream();
    match events
        .next()
        .await
        .ok_or(OpenError::OnOpenEventStreamClosed)?
        .map_err(OpenError::OnOpenDecode)?
    {
        fio::DirectoryEvent::OnOpen_ { s: status, info } => {
            let () = zx_status::Status::ok(status).map_err(OpenError::OpenError)?;
            let info = info.ok_or(OpenError::MissingOnOpenInfo)?;
            let () = Kind::expect_directory(*info).map_err(|actual| {
                OpenError::UnexpectedNodeKind { expected: Kind::Directory, actual }
            })?;
        }
        fio::DirectoryEvent::OnConnectionInfo { info } => {
            let representation = info.representation.ok_or(OpenError::MissingOnOpenInfo)?;
            let () = Kind::expect_directory2(&representation).map_err(|actual| {
                OpenError::UnexpectedNodeKind { expected: Kind::Directory, actual }
            })?;
        }
    }

    Ok(node)
}

/// Consume the first event from this FileProxy's event stream, returning the proxy if it is the
/// expected type or an error otherwise.
pub(crate) async fn verify_file_describe_event(
    node: fio::FileProxy,
) -> Result<fio::FileProxy, OpenError> {
    let mut events = node.take_event_stream();

    match events
        .next()
        .await
        .ok_or(OpenError::OnOpenEventStreamClosed)?
        .map_err(OpenError::OnOpenDecode)?
    {
        fio::FileEvent::OnOpen_ { s: status, info } => {
            let () = zx_status::Status::ok(status).map_err(OpenError::OpenError)?;
            let info = info.ok_or(OpenError::MissingOnOpenInfo)?;
            let () = Kind::expect_file(*info)
                .map_err(|actual| OpenError::UnexpectedNodeKind { expected: Kind::File, actual })?;
        }
        fio::FileEvent::OnConnectionInfo { info } => {
            let representation = info.representation.ok_or(OpenError::MissingOnOpenInfo)?;
            let () = Kind::expect_file2(&representation)
                .map_err(|actual| OpenError::UnexpectedNodeKind { expected: Kind::File, actual })?;
        }
    }

    Ok(node)
}

#[cfg(test)]
mod tests {
    use {super::*, crate::OpenFlags, assert_matches::assert_matches, fuchsia_async as fasync};

    // open_in_namespace

    #[fasync::run_singlethreaded(test)]
    async fn open_in_namespace_opens_real_node() {
        let file_node = open_in_namespace("/pkg/data/file", OpenFlags::RIGHT_READABLE).unwrap();
        let info = file_node.describe().await.unwrap();
        assert_matches!(Kind::expect_file(info), Ok(()));

        let dir_node = open_in_namespace("/pkg/data", OpenFlags::RIGHT_READABLE).unwrap();
        let info = dir_node.describe().await.unwrap();
        assert_eq!(Kind::expect_directory(info), Ok(()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_in_namespace_opens_fake_node_under_of_root_namespace_entry() {
        let notfound = open_in_namespace("/pkg/fake", OpenFlags::RIGHT_READABLE).unwrap();
        // The open error is not detected until the proxy is interacted with.
        assert_matches!(close(notfound).await, Err(_));
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_in_namespace_rejects_fake_root_namespace_entry() {
        assert_matches!(
            open_in_namespace("/fake", OpenFlags::RIGHT_READABLE),
            Err(OpenError::Namespace(zx_status::Status::NOT_FOUND))
        );
    }
}
