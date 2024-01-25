// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

//! Typesafe wrappers around the /blob filesystem.

use {
    fidl::endpoints::{Proxy as _, ServerEnd},
    fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg,
    fuchsia_hash::{Hash, ParseHashError},
    fuchsia_zircon::{self as zx, AsHandleRef as _, Status},
    futures::{stream, StreamExt as _},
    std::{collections::HashSet, sync::Arc},
    thiserror::Error,
    tracing::{error, info, warn},
    vfs::{
        common::send_on_open_with_error, directory::entry::DirectoryEntry,
        execution_scope::ExecutionScope, path::Path,
    },
};

pub mod mock;
pub use mock::Mock;

/// Blobfs client errors.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum BlobfsError {
    #[error("while opening blobfs dir")]
    OpenDir(#[from] fuchsia_fs::node::OpenError),

    #[error("while cloning the blobfs dir")]
    CloneDir(#[from] fuchsia_fs::node::CloneError),

    #[error("while listing blobfs dir")]
    ReadDir(#[source] fuchsia_fs::directory::EnumerateError),

    #[error("while deleting blob")]
    Unlink(#[source] Status),

    #[error("while sync'ing")]
    Sync(#[source] Status),

    #[error("while parsing blob merkle hash")]
    ParseHash(#[from] ParseHashError),

    #[error("FIDL error")]
    Fidl(#[from] fidl::Error),

    #[error("while connecting to fuchsia.fxfs/BlobCreator")]
    ConnectToBlobCreator(#[source] anyhow::Error),

    #[error("while connecting to fuchsia.fxfs/BlobReader")]
    ConnectToBlobReader(#[source] anyhow::Error),

    #[error("while connecting to fuchsia.kernel/VmexResource")]
    ConnectToVmexResource(#[source] anyhow::Error),

    #[error("while setting the VmexResource")]
    InitVmexResource(#[source] anyhow::Error),
}

/// An error encountered while creating a blob
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum CreateError {
    #[error("the blob already exists or is being concurrently written")]
    AlreadyExists,

    #[error("while creating the blob")]
    Io(#[source] fuchsia_fs::node::OpenError),

    #[error("while converting the proxy into a client end")]
    ConvertToClientEnd,

    #[error("FIDL error")]
    Fidl(#[from] fidl::Error),

    #[error("while calling fuchsia.fxfs/BlobCreator.Create: {0:?}")]
    BlobCreator(ffxfs::CreateBlobError),

    #[error("unsupported blob type {0:?}")]
    UnsupportedBlobType(fpkg::BlobType),
}

impl From<ffxfs::CreateBlobError> for CreateError {
    fn from(e: ffxfs::CreateBlobError) -> Self {
        match e {
            ffxfs::CreateBlobError::AlreadyExists => CreateError::AlreadyExists,
            e @ ffxfs::CreateBlobError::Internal => CreateError::BlobCreator(e),
        }
    }
}

/// A builder for [`Client`]
#[derive(Default)]
pub struct ClientBuilder {
    use_reader: Reader,
    use_creator: bool,
    readable: bool,
    writable: bool,
    executable: bool,
}

#[derive(Default)]
enum Reader {
    #[default]
    DontUse,
    Use {
        use_vmex: bool,
    },
}

impl ClientBuilder {
    /// Opens the /blob directory in the component's namespace with readable, writable, and/or
    /// executable flags. Connects to the fuchsia.fxfs.BlobCreator and BlobReader if requested.
    /// Connects to and initializes the VmexResource if `use_vmex` is set. Returns a `Client`.
    pub async fn build(self) -> Result<Client, BlobfsError> {
        let mut flags = fio::OpenFlags::empty();
        if self.readable {
            flags |= fio::OpenFlags::RIGHT_READABLE
        }
        if self.writable {
            flags |= fio::OpenFlags::RIGHT_WRITABLE
        }
        if self.executable {
            flags |= fio::OpenFlags::RIGHT_EXECUTABLE
        }
        let dir = fuchsia_fs::directory::open_in_namespace("/blob", flags)?;
        let reader = match self.use_reader {
            Reader::DontUse => None,
            Reader::Use { use_vmex } => {
                if use_vmex {
                    if let Ok(client) = fuchsia_component::client::connect_to_protocol::<
                        fidl_fuchsia_kernel::VmexResourceMarker,
                    >() {
                        if let Ok(vmex) = client.get().await {
                            info!("Got vmex resource");
                            vmo_blob::init_vmex_resource(vmex)
                                .map_err(BlobfsError::InitVmexResource)?;
                        }
                    }
                }
                Some(
                    fuchsia_component::client::connect_to_protocol::<ffxfs::BlobReaderMarker>()
                        .map_err(BlobfsError::ConnectToBlobReader)?,
                )
            }
        };

        let creator = if self.use_creator {
            Some(
                fuchsia_component::client::connect_to_protocol::<ffxfs::BlobCreatorMarker>()
                    .map_err(BlobfsError::ConnectToBlobCreator)?,
            )
        } else {
            None
        };

        Ok(Client { dir, creator, reader })
    }

    /// `Client` will connect to and use fuchsia.fxfs/BlobReader for reads. Sets the VmexResource
    /// for `Client`. The VmexResource is used by `get_backing_memory` to mark blobs as executable.
    pub fn use_reader(self) -> Self {
        Self { use_reader: Reader::Use { use_vmex: true }, ..self }
    }

    /// `Client` will connect to and use fuchsia.fxfs/BlobReader for reads. Does not set the
    /// VmexResource.
    pub fn use_reader_no_vmex(self) -> Self {
        Self { use_reader: Reader::Use { use_vmex: false }, ..self }
    }

    /// If set, `Client` will connect to and use fuchsia.fxfs/BlobCreator for writes.
    pub fn use_creator(self) -> Self {
        Self { use_creator: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_READABLE.
    pub fn readable(self) -> Self {
        Self { readable: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_WRITABLE. WRITABLE is needed so that `delete_blob` can call
    /// fuchsia.io/Directory.Unlink.
    pub fn writable(self) -> Self {
        Self { writable: true, ..self }
    }

    /// If set, `Client` will connect to /blob in the current component's namespace with
    /// OPEN_RIGHT_EXECUTABLE.
    pub fn executable(self) -> Self {
        Self { executable: true, ..self }
    }
}

impl Client {
    /// Create an empty `ClientBuilder`
    pub fn builder() -> ClientBuilder {
        Default::default()
    }
}
/// Blobfs client
#[derive(Debug, Clone)]
pub struct Client {
    dir: fio::DirectoryProxy,
    creator: Option<ffxfs::BlobCreatorProxy>,
    reader: Option<ffxfs::BlobReaderProxy>,
}

impl Client {
    /// Returns a client connected to the given blob directory, BlobCreatorProxy, and
    /// BlobReaderProxy. If `vmex` is passed in, sets the VmexResource, which is used to mark blobs
    /// as executable. If `creator` or `reader` is not supplied, writes or reads respectively will
    /// be performed through the blob directory.
    pub fn new(
        dir: fio::DirectoryProxy,
        creator: Option<ffxfs::BlobCreatorProxy>,
        reader: Option<ffxfs::BlobReaderProxy>,
        vmex: Option<zx::Resource>,
    ) -> Result<Self, anyhow::Error> {
        if let Some(vmex) = vmex {
            vmo_blob::init_vmex_resource(vmex)?;
        }
        Ok(Self { dir, creator, reader })
    }

    /// Creates a new client backed by the returned request stream. This constructor should not be
    /// used outside of tests.
    ///
    /// # Panics
    ///
    /// Panics on error
    pub fn new_test() -> (Self, fio::DirectoryRequestStream) {
        let (dir, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();

        (Self { dir, creator: None, reader: None }, stream)
    }

    /// Creates a new client backed by the returned mock. This constructor should not be used
    /// outside of tests.
    ///
    /// # Panics
    ///
    /// Panics on error
    pub fn new_mock() -> (Self, mock::Mock) {
        let (dir, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();

        (Self { dir, creator: None, reader: None }, mock::Mock { stream })
    }

    /// Open a blob for read. `scope` will only be used if the client was configured to use
    /// fuchsia.fxfs.BlobReader.
    pub fn open_blob_for_read(
        &self,
        blob: &Hash,
        flags: fio::OpenFlags,
        scope: ExecutionScope,
        server_end: ServerEnd<fio::NodeMarker>,
    ) -> Result<(), fidl::Error> {
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);
        if flags.contains(fio::OpenFlags::RIGHT_WRITABLE) {
            send_on_open_with_error(describe, server_end, zx::Status::ACCESS_DENIED);
            return Ok(());
        }
        if let Some(reader) = &self.reader {
            let hash = blob.clone();
            let scope_clone = scope.clone();
            let reader = reader.clone();

            scope.spawn(async move {
                let vmo = match reader.get_vmo(&hash.into()).await {
                    Err(e) => {
                        error!("Transport error on get_vmo: {:?}", e);
                        return send_on_open_with_error(describe, server_end, zx::Status::INTERNAL);
                    }
                    Ok(Err(s)) => {
                        warn!(
                            error = ?zx::Status::from_raw(s),
                            blob = %hash,
                            "Failed to get vmo from reader"
                        );
                        return send_on_open_with_error(
                            describe,
                            server_end,
                            zx::Status::from_raw(s),
                        );
                    }
                    Ok(Ok(vmo)) => vmo,
                };
                let blob = Arc::new(vmo_blob::VmoBlob::new(vmo));
                let () = blob.open(scope_clone, flags, Path::dot(), server_end);
            });
        } else {
            return self.dir.open(flags, fio::ModeType::empty(), &blob.to_string(), server_end);
        }
        Ok(())
    }

    /// Returns the list of known blobs in blobfs.
    pub async fn list_known_blobs(&self) -> Result<HashSet<Hash>, BlobfsError> {
        // fuchsia.io.Directory.ReadDirents uses a per-connection index into the array of
        // directory entries. To prevent contention over this index by concurrent calls (either
        // from concurrent calls to list_known_blobs on this object, or on clones of this object,
        // or other clones of the DirectoryProxy this object was made from), create a new
        // connection which will have its own index.
        let private_connection = fuchsia_fs::directory::clone_no_describe(&self.dir, None)?;
        fuchsia_fs::directory::readdir(&private_connection)
            .await
            .map_err(BlobfsError::ReadDir)?
            .into_iter()
            .filter(|entry| entry.kind == fuchsia_fs::directory::DirentKind::File)
            .map(|entry| entry.name.parse().map_err(BlobfsError::ParseHash))
            .collect()
    }

    /// Delete the blob with the given merkle hash.
    pub async fn delete_blob(&self, blob: &Hash) -> Result<(), BlobfsError> {
        self.dir
            .unlink(&blob.to_string(), &fio::UnlinkOptions::default())
            .await?
            .map_err(|s| BlobfsError::Unlink(Status::from_raw(s)))
    }

    /// Open a new blob for write.
    pub async fn open_blob_for_write(
        &self,
        blob: &Hash,
        blob_type: fpkg::BlobType,
    ) -> Result<fpkg::BlobWriter, CreateError> {
        Ok(if let Some(blob_creator) = &self.creator {
            // FxBlob only supports Delivery blobs.
            if blob_type != fpkg::BlobType::Delivery {
                return Err(CreateError::UnsupportedBlobType(blob_type));
            }
            fpkg::BlobWriter::Writer(blob_creator.create(blob, false).await??)
        } else {
            fpkg::BlobWriter::File(
                self.open_blob_proxy_from_dir_for_write(blob, blob_type)
                    .await?
                    .into_channel()
                    .map_err(|_: fio::FileProxy| CreateError::ConvertToClientEnd)?
                    .into_zx_channel()
                    .into(),
            )
        })
    }

    /// Open a new blob for write, unconditionally using the blob directory.
    async fn open_blob_proxy_from_dir_for_write(
        &self,
        blob: &Hash,
        blob_type: fpkg::BlobType,
    ) -> Result<fio::FileProxy, CreateError> {
        let flags = fio::OpenFlags::CREATE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_READABLE;

        let path = match blob_type {
            fpkg::BlobType::Uncompressed => blob.to_string(),
            fpkg::BlobType::Delivery => delivery_blob::delivery_blob_path(blob),
        };
        fuchsia_fs::directory::open_file(&self.dir, &path, flags).await.map_err(|e| match e {
            fuchsia_fs::node::OpenError::OpenError(Status::ACCESS_DENIED) => {
                CreateError::AlreadyExists
            }
            other => CreateError::Io(other),
        })
    }

    /// Returns whether blobfs has a blob with the given hash.
    pub async fn has_blob(&self, blob: &Hash) -> bool {
        if let Some(reader) = &self.reader {
            // TODO(https://fxbug.dev/295552228): Use faster API for determining blob presence.
            matches!(reader.get_vmo(blob).await, Ok(Ok(_)))
        } else {
            let file = match fuchsia_fs::directory::open_file_no_describe(
                &self.dir,
                &blob.to_string(),
                fio::OpenFlags::DESCRIBE | fio::OpenFlags::RIGHT_READABLE,
            ) {
                Ok(file) => file,
                Err(_) => return false,
            };

            let mut events = file.take_event_stream();

            let event = match events.next().await {
                None => return false,
                Some(event) => match event {
                    Err(_) => return false,
                    Ok(event) => match event {
                        fio::FileEvent::OnOpen_ { s, info } => {
                            if Status::from_raw(s) != Status::OK {
                                return false;
                            }

                            match info {
                                Some(info) => match *info {
                                    fio::NodeInfoDeprecated::File(fio::FileObject {
                                        event: Some(event),
                                        stream: _, // TODO(https://fxbug.dev/293606235): Use stream
                                    }) => event,
                                    _ => return false,
                                },
                                _ => return false,
                            }
                        }
                        fio::FileEvent::OnRepresentation { payload } => match payload {
                            fio::Representation::File(fio::FileInfo {
                                observer: Some(event),
                                ..
                            }) => event,
                            _ => return false,
                        },
                    },
                },
            };

            // Check that the USER_0 signal has been asserted on the file's event to make sure we
            // return false on the edge case of the blob is current being written.
            match event.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST) {
                Ok(_) => true,
                Err(status) => {
                    if status != Status::TIMED_OUT {
                        warn!("blobfs: unknown error asserting blob existence: {}", status);
                    }
                    false
                }
            }
        }
    }

    /// Determines which of candidate blobs exist and are readable in blobfs, returning the
    /// set difference of candidates and readable.
    /// If provided, `all_known` should be a superset of all readable blobs in blobfs, i.e.
    /// if a blob is readable it must be in `all_known`, but non-readable blobs may also be
    /// included.
    /// `all_known` is used to skip the expensive per-blob readable check for blobs that we are
    /// sure are missing.
    pub async fn filter_to_missing_blobs(
        &self,
        candidates: &HashSet<Hash>,
        all_known: Option<&HashSet<Hash>>,
    ) -> HashSet<Hash> {
        // This heuristic was taken from pkgfs. We are not sure how useful it is or why it was
        // added, however it is kept in out of an abundance of caution. We *suspect* the heuristic
        // is a performance optimization. Without the heuristic, we would always have to open every
        // candidate blob and see if it's readable, which may be expensive if there are many blobs.
        //
        // Note that if there are less than 20 blobs, we don't use the heuristic. This is because we
        // assume there is a certain threshold of number of blobs in a package where it is faster to
        // first do a readdir on blobfs to help rule out some blobs without having to open them. We
        // assume this threshold is 20. The optimal threshold is likely different between pkg-cache
        // and pkgfs, and, especially since this checks multiple blobs concurrently, we may not be
        // getting any benefits from the heuristic anymore.
        //
        // If you wish to remove this heuristic or change the threshold, consider doing a trace on
        // packages with varying numbers of blobs present/missing.
        // TODO(https://fxbug.dev/42157763) re-evaluate filter_to_missing_blobs heuristic.
        let all_known_storage;
        let all_known = if let Some(all_known) = all_known {
            Some(all_known)
        } else {
            if candidates.len() > 20 {
                if let Some(all_known) = self.list_known_blobs().await.ok() {
                    all_known_storage = all_known;
                    Some(&all_known_storage)
                } else {
                    None
                }
            } else {
                None
            }
        };

        stream::iter(candidates.clone())
            .map(move |blob| {
                async move {
                    // We still need to check `has_blob()` even if the blob is in `all_known`,
                    // because it might not have been fully written yet.
                    if all_known.map(|blobs| blobs.contains(&blob)) == Some(false)
                        || !self.has_blob(&blob).await
                    {
                        Some(blob)
                    } else {
                        None
                    }
                }
            })
            .buffer_unordered(50)
            .filter_map(|blob| async move { blob })
            .collect()
            .await
    }

    /// Call fuchsia.io/Node.Sync on the blobfs directory.
    pub async fn sync(&self) -> Result<(), BlobfsError> {
        self.dir.sync().await?.map_err(zx::Status::from_raw).map_err(BlobfsError::Sync)
    }
}

#[cfg(test)]
impl Client {
    /// Constructs a new [`Client`] connected to the provided [`BlobfsRamdisk`]. Tests in this
    /// crate should use this constructor rather than [`BlobfsRamdisk::client`], which returns
    /// the non-cfg(test) build of this crate's [`blobfs::Client`]. While tests could use the
    /// [`blobfs::Client`] returned by [`BlobfsRamdisk::client`], it will be a different type than
    /// [`super::Client`], and the tests could not access its private members or any cfg(test)
    /// specific functionality.
    ///
    /// # Panics
    ///
    /// Panics on error.
    pub fn for_ramdisk(blobfs: &blobfs_ramdisk::BlobfsRamdisk) -> Self {
        Self::new(
            blobfs.root_dir_proxy().unwrap(),
            blobfs.blob_creator_proxy().unwrap(),
            blobfs.blob_reader_proxy().unwrap(),
            None,
        )
        .unwrap()
    }
}

#[cfg(test)]
#[allow(clippy::bool_assert_comparison)]
mod tests {
    use {
        super::*, assert_matches::assert_matches, blobfs_ramdisk::BlobfsRamdisk,
        fuchsia_async as fasync, fuchsia_merkle::MerkleTree, futures::stream::TryStreamExt,
        maplit::hashset, std::io::Write as _,
    };

    #[fasync::run_singlethreaded(test)]
    async fn list_known_blobs_empty() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert_eq!(client.list_known_blobs().await.unwrap(), HashSet::new());
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn list_known_blobs() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let expected = blobfs.list_blobs().unwrap().into_iter().collect();
        assert_eq!(client.list_known_blobs().await.unwrap(), expected);
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_blob_and_then_list() {
        let blobfs = BlobfsRamdisk::builder()
            .with_blob(&b"blob 1"[..])
            .with_blob(&b"blob 2"[..])
            .start()
            .await
            .unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let merkle = MerkleTree::from_reader(&b"blob 1"[..]).unwrap().root();
        assert_matches!(client.delete_blob(&merkle).await, Ok(()));

        let expected = hashset! {MerkleTree::from_reader(&b"blob 2"[..]).unwrap().root()};
        assert_eq!(client.list_known_blobs().await.unwrap(), expected);
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_non_existing_blob() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);
        let blob_merkle = Hash::from([1; 32]);

        assert_matches!(
            client.delete_blob(&blob_merkle).await,
            Err(BlobfsError::Unlink(Status::NOT_FOUND))
        );
        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn delete_blob_mock() {
        let (client, mut stream) = Client::new_test();
        let blob_merkle = Hash::from([1; 32]);
        fasync::Task::spawn(async move {
            match stream.try_next().await.unwrap().unwrap() {
                fio::DirectoryRequest::Unlink { name, responder, .. } => {
                    assert_eq!(name, blob_merkle.to_string());
                    responder.send(Ok(())).unwrap();
                }
                other => panic!("unexpected request: {other:?}"),
            }
        })
        .detach();

        assert_matches!(client.delete_blob(&blob_merkle).await, Ok(()));
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob() {
        let blobfs = BlobfsRamdisk::builder().with_blob(&b"blob 1"[..]).start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert_eq!(
            client.has_blob(&MerkleTree::from_reader(&b"blob 1"[..]).unwrap().root()).await,
            true
        );
        assert_eq!(client.has_blob(&Hash::from([1; 32])).await, false);

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob_fxblob() {
        let blobfs =
            BlobfsRamdisk::builder().fxblob().with_blob(&b"blob 1"[..]).start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        assert!(client.has_blob(&MerkleTree::from_reader(&b"blob 1"[..]).unwrap().root()).await);
        assert!(!client.has_blob(&Hash::from([1; 32])).await);

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn has_blob_return_false_if_blob_is_partially_written() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let blob = [3; 1024];
        let hash = MerkleTree::from_reader(&blob[..]).unwrap().root();

        let mut file = blobfs.root_dir().unwrap().write_file(hash.to_string(), 0o777).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.set_len(blob.len() as u64).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.write_all(&blob[..512]).unwrap();
        assert_eq!(client.has_blob(&hash).await, false);
        file.write_all(&blob[512..]).unwrap();
        assert_eq!(client.has_blob(&hash).await, true);

        blobfs.stop().await.unwrap();
    }

    async fn resize(blob: &fio::FileProxy, size: usize) {
        let () = blob.resize(size as u64).await.unwrap().map_err(Status::from_raw).unwrap();
    }

    async fn write(blob: &fio::FileProxy, bytes: &[u8]) {
        assert_eq!(
            blob.write(bytes).await.unwrap().map_err(Status::from_raw).unwrap(),
            bytes.len() as u64
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn write_uncompressed_blob() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let content = [3; 1024];
        let hash = MerkleTree::from_reader(&content[..]).unwrap().root();

        let proxy = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Uncompressed)
            .await
            .unwrap();

        let () = resize(&proxy, content.len()).await;
        let () = write(&proxy, &content).await;

        assert!(client.has_blob(&hash).await);

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn write_delivery_blob() {
        let blobfs = BlobfsRamdisk::start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let content = [3; 1024];
        let hash = MerkleTree::from_reader(&content[..]).unwrap().root();
        let delivery_content =
            delivery_blob::Type1Blob::generate(&content, delivery_blob::CompressionMode::Always);

        let proxy = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Delivery)
            .await
            .unwrap();

        let () = resize(&proxy, delivery_content.len()).await;
        let () = write(&proxy, &delivery_content).await;

        assert!(client.has_blob(&hash).await);

        blobfs.stop().await.unwrap();
    }

    /// Wrapper for a blob and its hash. This lets the tests retain ownership of the Blob,
    /// which is important because it ensures blobfs will not close partially written blobs for the
    /// duration of the test.
    struct TestBlob {
        _blob: fio::FileProxy,
        hash: Hash,
    }

    async fn open_blob_only(client: &Client, blob: &[u8; 1024]) -> TestBlob {
        let hash = MerkleTree::from_reader(&blob[..]).unwrap().root();
        let _blob = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Uncompressed)
            .await
            .unwrap();
        TestBlob { _blob, hash }
    }

    async fn open_and_truncate_blob(client: &Client, content: &[u8; 1024]) -> TestBlob {
        let hash = MerkleTree::from_reader(&content[..]).unwrap().root();
        let _blob = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Uncompressed)
            .await
            .unwrap();
        let () = resize(&_blob, content.len()).await;
        TestBlob { _blob, hash }
    }

    async fn partially_write_blob(client: &Client, content: &[u8; 1024]) -> TestBlob {
        let hash = MerkleTree::from_reader(&content[..]).unwrap().root();
        let _blob = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Uncompressed)
            .await
            .unwrap();
        let () = resize(&_blob, content.len()).await;
        let () = write(&_blob, &content[..512]).await;
        TestBlob { _blob, hash }
    }

    async fn fully_write_blob(client: &Client, content: &[u8]) -> TestBlob {
        let hash = MerkleTree::from_reader(content).unwrap().root();
        let _blob = client
            .open_blob_proxy_from_dir_for_write(&hash, fpkg::BlobType::Uncompressed)
            .await
            .unwrap();
        let () = resize(&_blob, content.len()).await;
        let () = write(&_blob, content).await;
        TestBlob { _blob, hash }
    }

    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs_without_heuristic() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let missing_hash0 = Hash::from([0; 32]);
        let missing_hash1 = Hash::from([1; 32]);

        let present_blob0 = fully_write_blob(&client, &[2; 1024]).await;
        let present_blob1 = fully_write_blob(&client, &[3; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(
                    // Pass in <= 20 candidates so the heuristic is not used.
                    &hashset! { missing_hash0, missing_hash1,
                        present_blob0.hash, present_blob1.hash
                    },
                    None
                )
                .await,
            hashset! { missing_hash0, missing_hash1 }
        );

        blobfs.stop().await.unwrap();
    }

    /// Similar to the above test, except also test that partially written blobs count as missing.
    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs_without_heuristic_and_with_partially_written_blobs() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        // Some blobs are created (but not yet truncated).
        let missing_blob0 = open_blob_only(&client, &[0; 1024]).await;

        // Some are truncated but not written.
        let missing_blob1 = open_and_truncate_blob(&client, &[1; 1024]).await;

        // Some are partially written.
        let missing_blob2 = partially_write_blob(&client, &[2; 1024]).await;

        // Some are fully written.
        let present_blob = fully_write_blob(&client, &[3; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(
                    &hashset! {
                        missing_blob0.hash,
                        missing_blob1.hash,
                        missing_blob2.hash,
                        present_blob.hash
                    },
                    None
                )
                .await,
            // All partially written blobs should count as missing.
            hashset! { missing_blob0.hash, missing_blob1.hash, missing_blob2.hash }
        );

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs_with_heuristic() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        let missing_hash0 = Hash::from([0; 32]);
        let missing_hash1 = Hash::from([1; 32]);
        let missing_hash2 = Hash::from([2; 32]);
        let missing_hash3 = Hash::from([3; 32]);
        let missing_hash4 = Hash::from([4; 32]);
        let missing_hash5 = Hash::from([5; 32]);
        let missing_hash6 = Hash::from([6; 32]);
        let missing_hash7 = Hash::from([7; 32]);
        let missing_hash8 = Hash::from([8; 32]);
        let missing_hash9 = Hash::from([9; 32]);
        let missing_hash10 = Hash::from([10; 32]);

        let present_blob0 = fully_write_blob(&client, &[20; 1024]).await;
        let present_blob1 = fully_write_blob(&client, &[21; 1024]).await;
        let present_blob2 = fully_write_blob(&client, &[22; 1024]).await;
        let present_blob3 = fully_write_blob(&client, &[23; 1024]).await;
        let present_blob4 = fully_write_blob(&client, &[24; 1024]).await;
        let present_blob5 = fully_write_blob(&client, &[25; 1024]).await;
        let present_blob6 = fully_write_blob(&client, &[26; 1024]).await;
        let present_blob7 = fully_write_blob(&client, &[27; 1024]).await;
        let present_blob8 = fully_write_blob(&client, &[28; 1024]).await;
        let present_blob9 = fully_write_blob(&client, &[29; 1024]).await;
        let present_blob10 = fully_write_blob(&client, &[30; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(
                    // Pass in over 20 candidates to trigger the heuristic.
                    &hashset! { missing_hash0, missing_hash1, missing_hash2, missing_hash3,
                        missing_hash4, missing_hash5, missing_hash6, missing_hash7, missing_hash8,
                        missing_hash9, missing_hash10, present_blob0.hash, present_blob1.hash,
                        present_blob2.hash, present_blob3.hash, present_blob4.hash,
                        present_blob5.hash, present_blob6.hash, present_blob7.hash,
                        present_blob8.hash, present_blob9.hash, present_blob10.hash
                    },
                    None
                )
                .await,
            hashset! { missing_hash0, missing_hash1, missing_hash2, missing_hash3,
                missing_hash4, missing_hash5, missing_hash6, missing_hash7, missing_hash8,
                missing_hash9, missing_hash10
            }
        );

        blobfs.stop().await.unwrap();
    }

    /// Similar to the above test, except also test that partially written blobs count as missing.
    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_blobs_with_heuristic_and_with_partially_written_blobs() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        // Some blobs are created (but not yet truncated).
        let missing_blob0 = open_blob_only(&client, &[0; 1024]).await;
        let missing_blob1 = open_blob_only(&client, &[1; 1024]).await;
        let missing_blob2 = open_blob_only(&client, &[2; 1024]).await;

        // Some are truncated but not written.
        let missing_blob3 = open_and_truncate_blob(&client, &[3; 1024]).await;
        let missing_blob4 = open_and_truncate_blob(&client, &[4; 1024]).await;
        let missing_blob5 = open_and_truncate_blob(&client, &[5; 1024]).await;

        // Some are partially written.
        let missing_blob6 = partially_write_blob(&client, &[6; 1024]).await;
        let missing_blob7 = partially_write_blob(&client, &[7; 1024]).await;
        let missing_blob8 = partially_write_blob(&client, &[8; 1024]).await;

        // Some aren't even open.
        let missing_hash9 = Hash::from([9; 32]);
        let missing_hash10 = Hash::from([10; 32]);

        let present_blob0 = fully_write_blob(&client, &[20; 1024]).await;
        let present_blob1 = fully_write_blob(&client, &[21; 1024]).await;
        let present_blob2 = fully_write_blob(&client, &[22; 1024]).await;
        let present_blob3 = fully_write_blob(&client, &[23; 1024]).await;
        let present_blob4 = fully_write_blob(&client, &[24; 1024]).await;
        let present_blob5 = fully_write_blob(&client, &[25; 1024]).await;
        let present_blob6 = fully_write_blob(&client, &[26; 1024]).await;
        let present_blob7 = fully_write_blob(&client, &[27; 1024]).await;
        let present_blob8 = fully_write_blob(&client, &[28; 1024]).await;
        let present_blob9 = fully_write_blob(&client, &[29; 1024]).await;
        let present_blob10 = fully_write_blob(&client, &[30; 1024]).await;

        assert_eq!(
            client
                .filter_to_missing_blobs(
                    &hashset! { missing_blob0.hash, missing_blob1.hash, missing_blob2.hash,
                        missing_blob3.hash, missing_blob4.hash, missing_blob5.hash,
                        missing_blob6.hash, missing_blob7.hash, missing_blob8.hash,
                        missing_hash9, missing_hash10, present_blob0.hash,
                        present_blob1.hash, present_blob2.hash, present_blob3.hash,
                        present_blob4.hash, present_blob5.hash, present_blob6.hash,
                        present_blob7.hash, present_blob8.hash, present_blob9.hash,
                        present_blob10.hash
                    },
                    None
                )
                .await,
            // All partially written blobs should count as missing.
            hashset! { missing_blob0.hash, missing_blob1.hash, missing_blob2.hash,
                missing_blob3.hash, missing_blob4.hash, missing_blob5.hash, missing_blob6.hash,
                missing_blob7.hash, missing_blob8.hash, missing_hash9, missing_hash10
            }
        );

        blobfs.stop().await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn sync() {
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let (client, mut stream) = Client::new_test();
        fasync::Task::spawn(async move {
            match stream.try_next().await.unwrap().unwrap() {
                fio::DirectoryRequest::Sync { responder } => {
                    counter_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    responder.send(Ok(())).unwrap();
                }
                other => panic!("unexpected request: {other:?}"),
            }
        })
        .detach();

        assert_matches!(client.sync().await, Ok(()));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_blob_for_write_uses_fxblob_if_configured() {
        let (blob_creator, mut blob_creator_stream) =
            fidl::endpoints::create_proxy_and_stream::<ffxfs::BlobCreatorMarker>().unwrap();
        let (blob_reader, _) = fidl::endpoints::create_proxy::<ffxfs::BlobReaderMarker>().unwrap();
        let client = Client::new(
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap().0,
            Some(blob_creator),
            Some(blob_reader),
            None,
        )
        .unwrap();

        fuchsia_async::Task::spawn(async move {
            match blob_creator_stream.next().await.unwrap().unwrap() {
                ffxfs::BlobCreatorRequest::Create { hash, allow_existing, responder } => {
                    assert_eq!(hash, [0; 32]);
                    assert!(!allow_existing);
                    let () = responder.send(Ok(fidl::endpoints::create_endpoints().0)).unwrap();
                }
            }
        })
        .detach();

        assert_matches!(
            client.open_blob_for_write(&[0; 32].into(), fpkg::BlobType::Delivery).await,
            Ok(fpkg::BlobWriter::Writer(_))
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn open_blob_for_write_fxblob_maps_already_exists() {
        let (blob_creator, mut blob_creator_stream) =
            fidl::endpoints::create_proxy_and_stream::<ffxfs::BlobCreatorMarker>().unwrap();
        let (blob_reader, _) = fidl::endpoints::create_proxy::<ffxfs::BlobReaderMarker>().unwrap();

        let client = Client::new(
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap().0,
            Some(blob_creator),
            Some(blob_reader),
            None,
        )
        .unwrap();

        fuchsia_async::Task::spawn(async move {
            match blob_creator_stream.next().await.unwrap().unwrap() {
                ffxfs::BlobCreatorRequest::Create { hash, allow_existing, responder } => {
                    assert_eq!(hash, [0; 32]);
                    assert!(!allow_existing);
                    let () = responder.send(Err(ffxfs::CreateBlobError::AlreadyExists)).unwrap();
                }
            }
        })
        .detach();

        assert_matches!(
            client.open_blob_for_write(&[0; 32].into(), fpkg::BlobType::Delivery).await,
            Err(CreateError::AlreadyExists)
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn concurrent_list_known_blobs_all_return_full_contents() {
        use futures::StreamExt;
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);

        // ReadDirents returns an 8,192 byte buffer, and each entry is 74 bytes [0] (including 64
        // bytes of filename), so use more than 110 entries to guarantee that listing all contents
        // requires multiple ReadDirents calls. This isn't necessary to cause conflict, because
        // each successful listing requires a call to Rewind as well, but it does make conflict
        // more likely.
        // [0] https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.io/directory.fidl;l=261;drc=9e84e19d3f42240c46d2b0c3c132c2f0b5a3343f
        for i in 0..256u16 {
            let _: TestBlob = fully_write_blob(&client, i.to_le_bytes().as_slice()).await;
        }

        let () = futures::stream::iter(0..100)
            .for_each_concurrent(None, |_| async {
                assert_eq!(client.list_known_blobs().await.unwrap().len(), 256);
            })
            .await;
    }

    #[fasync::run_singlethreaded(test)]
    async fn filter_to_missing_uses_provided_all_known() {
        let blobfs = BlobfsRamdisk::builder().start().await.unwrap();
        let client = Client::for_ramdisk(&blobfs);
        let present_blob = fully_write_blob(&client, &[0; 1024]).await;

        // Even though actually present, the written blob will be reported as missing because the
        // provided `all_known` is empty.
        assert_eq!(
            client
                .filter_to_missing_blobs(&HashSet::from([present_blob.hash]), Some(&HashSet::new()))
                .await,
            HashSet::from([present_blob.hash])
        );
    }
}
