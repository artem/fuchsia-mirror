// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains the [`BlobDirectory`] node type used to represent a directory of immutable
//! content-addressable blobs.

use {
    crate::{
        component::map_to_raw_status,
        fuchsia::{
            directory::FxDirectory,
            fxblob::{blob::FxBlob, writer::FxDeliveryBlob},
            node::{FxNode, GetResult, OpenedNode},
            volume::{FxVolume, RootDir},
        },
    },
    anyhow::{anyhow, bail, ensure, Error},
    async_trait::async_trait,
    fidl::endpoints::{create_proxy, ClientEnd, Proxy as _, ServerEnd},
    fidl_fuchsia_fxfs::{
        BlobCreatorRequest, BlobCreatorRequestStream, BlobReaderRequest, BlobReaderRequestStream,
        BlobWriterMarker, CreateBlobError,
    },
    fidl_fuchsia_io::{
        self as fio, FilesystemInfo, MutableNodeAttributes, NodeAttributeFlags, NodeAttributes,
        NodeMarker, WatchMask,
    },
    fuchsia_async as fasync,
    fuchsia_hash::Hash,
    fuchsia_merkle::{MerkleTree, MerkleTreeBuilder},
    fuchsia_zircon::Status,
    futures::{FutureExt, TryStreamExt},
    fxfs::{
        errors::FxfsError,
        log::*,
        object_handle::ReadObjectHandle,
        object_store::{
            self,
            transaction::{lock_keys, LockKey, Options},
            HandleOptions, ObjectDescriptor, ObjectStore, BLOB_MERKLE_ATTRIBUTE_ID,
        },
        serialized_types::BlobMetadata,
    },
    std::{str::FromStr, sync::Arc},
    vfs::{
        directory::{
            dirents_sink::{self, Sink},
            entry::{DirectoryEntry, EntryInfo, OpenRequest},
            entry_container::{Directory as VfsDirectory, DirectoryWatcher, MutableDirectory},
            mutable::connection::MutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        path::Path,
        ToObjectRequest,
    },
};

/// A flat directory containing content-addressable blobs (names are their hashes).
/// It is not possible to create sub-directories.
/// It is not possible to write to an existing blob.
/// It is not possible to open or read a blob until it is written and verified.
pub struct BlobDirectory {
    directory: Arc<FxDirectory>,
}

/// Instead of constantly switching back and forth between strings and hashes. Do it once and then
/// just pass around a reference to that.
pub(crate) struct Identifier {
    pub string: String,
    pub hash: Hash,
}

impl Identifier {
    pub fn from_str(string: &str) -> Result<Self, Error> {
        Ok(Self {
            string: string.to_owned(),
            hash: Hash::from_str(string).map_err(|_| FxfsError::InvalidArgs)?,
        })
    }

    pub fn from_hash(hash: Hash) -> Self {
        Self { string: hash.to_string(), hash }
    }
}

#[async_trait]
impl RootDir for BlobDirectory {
    fn as_directory_entry(self: Arc<Self>) -> Arc<dyn DirectoryEntry> {
        self
    }

    fn as_directory(self: Arc<Self>) -> Arc<dyn VfsDirectory> {
        self
    }

    fn as_node(self: Arc<Self>) -> Arc<dyn FxNode> {
        self as Arc<dyn FxNode>
    }

    fn on_open(self: Arc<Self>) {
        fasync::Task::spawn(async move {
            if let Err(e) = self.prefetch_blobs().await {
                warn!("Failed to prefetch blobs: {:?}", e);
            }
        })
        .detach();
    }

    async fn handle_blob_creator_requests(self: Arc<Self>, mut requests: BlobCreatorRequestStream) {
        while let Ok(Some(request)) = requests.try_next().await {
            match request {
                BlobCreatorRequest::Create { responder, hash, .. } => {
                    responder.send(self.create_blob(Hash::from(hash)).await).unwrap_or_else(
                        |error| {
                            tracing::error!(?error, "failed to send Create response");
                        },
                    );
                }
            }
        }
    }

    async fn handle_blob_reader_requests(self: Arc<Self>, mut requests: BlobReaderRequestStream) {
        while let Ok(Some(request)) = requests.try_next().await {
            match request {
                BlobReaderRequest::GetVmo { blob_hash, responder } => {
                    responder
                        .send(
                            self.clone()
                                .get_blob_vmo(blob_hash.into())
                                .await
                                .map_err(map_to_raw_status),
                        )
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to send GetVmo response");
                        });
                }
            };
        }
    }
}

impl BlobDirectory {
    fn new(directory: FxDirectory) -> Self {
        fuchsia_merkle::crypto_library_init();
        Self { directory: Arc::new(directory) }
    }

    pub fn directory(&self) -> &Arc<FxDirectory> {
        &self.directory
    }

    pub fn volume(&self) -> &Arc<FxVolume> {
        self.directory.volume()
    }

    fn store(&self) -> &ObjectStore {
        self.directory.store()
    }

    async fn prefetch_blobs(self: &Arc<Self>) -> Result<(), Error> {
        let store = self.store();
        let fs = store.filesystem();

        let dirents = {
            let _guard = fs
                .lock_manager()
                .read_lock(lock_keys![LockKey::object(
                    store.store_object_id(),
                    self.directory.object_id()
                )])
                .await;
            let mut dirents = vec![];
            let layer_set = store.tree().layer_set();
            let mut merger = layer_set.merger();
            let mut iter = self.directory.directory().iter(&mut merger).await?;
            let mut num = 0;
            let limit = self.directory.directory().owner().dirent_cache().limit();
            while let Some((name, object_id, _)) = iter.get() {
                dirents.push((Identifier::from_str(name)?, object_id));
                iter.advance().await?;
                num += 1;
                if num >= limit {
                    break;
                }
            }
            dirents
        };

        for (identifier, object_id) in dirents {
            if let Ok(node) = self.get_or_load_node(object_id, &identifier).await {
                self.directory.directory().owner().dirent_cache().insert(
                    self.directory.object_id(),
                    identifier.string,
                    node,
                );
            }
        }
        Ok(())
    }

    pub async fn open_blob(self: &Arc<Self>, hash: Hash) -> Result<Arc<FxBlob>, Error> {
        self.lookup(fio::OpenFlags::RIGHT_READABLE, Identifier::from_hash(hash))
            .await?
            .take()
            .into_any()
            .downcast::<FxBlob>()
            .map_err(|_| FxfsError::Inconsistent.into())
    }

    pub(crate) async fn lookup(
        self: &Arc<Self>,
        flags: fio::OpenFlags,
        id: Identifier,
    ) -> Result<OpenedNode<dyn FxNode>, Error> {
        let store = self.store();
        let fs = store.filesystem();

        // TODO(https://fxbug.dev/42073113): Create the transaction here if we might need to create the object
        // so that we have a lock in place.
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.directory.object_id())];

        // A lock needs to be held over searching the directory and incrementing the open count.
        let guard = fs.lock_manager().read_lock(keys.clone()).await;

        let child_node = match self
            .directory
            .directory()
            .owner()
            .dirent_cache()
            .lookup(&(self.directory.object_id(), &id.string))
        {
            Some(node) => Some(node),
            None => {
                if let Some((object_id, _)) = self.directory.directory().lookup(&id.string).await? {
                    let node = self.get_or_load_node(object_id, &id).await?;
                    self.directory.directory().owner().dirent_cache().insert(
                        self.directory.object_id(),
                        id.string.clone(),
                        node.clone(),
                    );
                    Some(node)
                } else {
                    None
                }
            }
        };
        match child_node {
            Some(node) => {
                ensure!(
                    !flags.contains(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT),
                    FxfsError::AlreadyExists
                );
                ensure!(!flags.contains(fio::OpenFlags::RIGHT_WRITABLE), FxfsError::AccessDenied);
                match node.object_descriptor() {
                    ObjectDescriptor::File => {
                        ensure!(!flags.contains(fio::OpenFlags::DIRECTORY), FxfsError::NotDir)
                    }
                    _ => bail!(FxfsError::Inconsistent),
                }
                // TODO(https://fxbug.dev/42073113): Test that we can't open a blob while still writing it.
                Ok(OpenedNode::new(node))
            }
            None => {
                std::mem::drop(guard);

                ensure!(flags.contains(fio::OpenFlags::CREATE), FxfsError::NotFound);
                ensure!(flags.contains(fio::OpenFlags::RIGHT_WRITABLE), FxfsError::AccessDenied);
                let mut transaction = fs.clone().new_transaction(keys, Options::default()).await?;

                let handle = ObjectStore::create_object(
                    self.volume(),
                    &mut transaction,
                    // Checksums are redundant for blobs, which are already content-verified.
                    HandleOptions { skip_checksums: true, ..Default::default() },
                    None,
                    None,
                )
                .await?;

                let node = OpenedNode::new(
                    FxDeliveryBlob::new(self.clone(), id.hash, handle) as Arc<dyn FxNode>
                );

                // Add the object to the graveyard so that it's cleaned up if we crash.
                store.add_to_graveyard(&mut transaction, node.object_id());

                // Note that we don't bother notifying watchers yet.  Nothing else should be able to
                // see this object yet.
                transaction.commit().await?;

                Ok(node)
            }
        }
    }

    // Attempts to get a node from the node cache. If the node wasn't present in the cache, loads
    // the object from the object store, installing the returned node into the cache and returns the
    // newly created FxNode backed by the loaded object.
    async fn get_or_load_node(
        self: &Arc<Self>,
        object_id: u64,
        id: &Identifier,
    ) -> Result<Arc<dyn FxNode>, Error> {
        let volume = self.volume();
        match volume.cache().get_or_reserve(object_id).await {
            GetResult::Node(node) => {
                // Protecting against the scenario where a directory entry points to another node
                // which has already been loaded and verified with the correct hash. We need to
                // verify that the hash for the blob that is cached here matches the requested hash.
                let blob = node.into_any().downcast::<FxBlob>().map_err(|_| {
                    anyhow!(FxfsError::Inconsistent).context("Loaded non-blob from cache")
                })?;
                ensure!(
                    blob.root() == id.hash,
                    anyhow!(FxfsError::Inconsistent)
                        .context("Loaded blob by node that did not match the given hash")
                );
                Ok(blob as Arc<dyn FxNode>)
            }
            GetResult::Placeholder(placeholder) => {
                let object =
                    ObjectStore::open_object(volume, object_id, HandleOptions::default(), None)
                        .await?;
                let (tree, metadata) = match object.read_attr(BLOB_MERKLE_ATTRIBUTE_ID).await? {
                    None => {
                        // If the file is uncompressed and is small enough, it may not have any
                        // metadata stored on disk.
                        (
                            MerkleTree::from_levels(vec![vec![id.hash]]),
                            BlobMetadata {
                                hashes: vec![],
                                chunk_size: 0,
                                compressed_offsets: vec![],
                                uncompressed_size: object.get_size(),
                            },
                        )
                    }
                    Some(data) => {
                        let mut metadata: BlobMetadata = bincode::deserialize_from(&*data)?;
                        let tree = if metadata.hashes.is_empty() {
                            MerkleTree::from_levels(vec![vec![id.hash]])
                        } else {
                            let mut builder = MerkleTreeBuilder::new();
                            for hash in std::mem::take(&mut metadata.hashes) {
                                builder.push_data_hash(hash.into());
                            }
                            let tree = builder.finish();
                            ensure!(tree.root() == id.hash, FxfsError::Inconsistent);
                            tree
                        };
                        (tree, metadata)
                    }
                };

                let node = FxBlob::new(
                    object,
                    tree,
                    metadata.chunk_size,
                    metadata.compressed_offsets,
                    metadata.uncompressed_size,
                ) as Arc<dyn FxNode>;
                placeholder.commit(&node);
                Ok(node)
            }
        }
    }

    async fn create_blob(
        self: &Arc<Self>,
        hash: Hash,
    ) -> Result<ClientEnd<BlobWriterMarker>, CreateBlobError> {
        let flags = fio::OpenFlags::CREATE
            | fio::OpenFlags::CREATE_IF_ABSENT
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_READABLE;
        let node = match self.lookup(flags, Identifier::from_hash(hash)).await {
            Ok(node) => node,
            Err(e) => {
                if FxfsError::AlreadyExists.matches(&e) {
                    return Err(CreateBlobError::AlreadyExists);
                } else {
                    tracing::error!("lookup failed: {:?}", e);
                    return Err(CreateBlobError::Internal);
                }
            }
        };
        let blob = node.downcast::<FxDeliveryBlob>().unwrap_or_else(|_| unreachable!());

        let (client, server_end) = create_proxy::<BlobWriterMarker>().map_err(|e| {
            tracing::error!("create_proxy failed for the BlobWriter protocol: {:?}", e);
            CreateBlobError::Internal
        })?;
        let client_channel = client.into_channel().map_err(|_| {
            tracing::error!("failed to create client channel");
            CreateBlobError::Internal
        })?;
        let client_end = ClientEnd::new(client_channel.into());
        self.volume().scope().spawn(async move {
            if let Err(e) = blob.as_ref().handle_requests(server_end).await {
                tracing::error!("Failed to handle blob writer requests: {}", e);
            }
        });
        return Ok(client_end);
    }
}

impl FxNode for BlobDirectory {
    fn object_id(&self) -> u64 {
        self.directory.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        self.directory.parent()
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {
        // This directory can't be renamed.
        unreachable!();
    }

    fn open_count_add_one(&self) {}
    fn open_count_sub_one(self: Arc<Self>) {}

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::Directory
    }
}

#[async_trait]
impl MutableDirectory for BlobDirectory {
    async fn unlink(self: Arc<Self>, name: &str, must_be_directory: bool) -> Result<(), Status> {
        if must_be_directory {
            return Err(Status::INVALID_ARGS);
        }
        self.directory.clone().unlink(name, must_be_directory).await
    }

    async fn set_attrs(
        &self,
        flags: NodeAttributeFlags,
        attrs: NodeAttributes,
    ) -> Result<(), Status> {
        self.directory.set_attrs(flags, attrs).await
    }

    async fn update_attributes(&self, attributes: MutableNodeAttributes) -> Result<(), Status> {
        self.directory.update_attributes(attributes).await
    }

    async fn sync(&self) -> Result<(), Status> {
        self.directory.sync().await
    }

    async fn rename(
        self: Arc<Self>,
        _src_dir: Arc<dyn vfs::directory::entry_container::MutableDirectory + 'static>,
        _src_name: Path,
        _dst_name: Path,
    ) -> Result<(), Status> {
        // Files in a blob directory can't be renamed.
        Err(Status::NOT_SUPPORTED)
    }
}

/// Implementation of VFS pseudo-directory for blobs. Forks a task per connection.
impl DirectoryEntry for BlobDirectory {
    fn entry_info(&self) -> EntryInfo {
        self.directory.entry_info()
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_dir(self)
    }
}

#[async_trait]
impl vfs::node::Node for BlobDirectory {
    async fn get_attrs(&self) -> Result<NodeAttributes, Status> {
        self.directory.get_attrs().await
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        self.directory.get_attributes(requested_attributes).await
    }

    fn query_filesystem(&self) -> Result<FilesystemInfo, Status> {
        self.directory.query_filesystem()
    }
}

/// Implements VFS entry container trait for directories, allowing manipulation of their contents.
#[async_trait]
impl VfsDirectory for BlobDirectory {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<NodeMarker>,
    ) {
        flags.to_object_request(server_end).spawn(&scope.clone(), move |object_request| {
            async move {
                if path.is_empty() {
                    object_request.create_connection(
                        scope,
                        OpenedNode::new(self.clone())
                            .downcast::<BlobDirectory>()
                            .unwrap_or_else(|_| unreachable!())
                            .take(),
                        flags,
                        MutableConnection::create,
                    )
                } else {
                    tracing::error!(
                        "Tried to open a blob via open(). Use the BlobCreator or BlobReader
                            instead."
                    );
                    Err(Status::NOT_SUPPORTED)
                }
            }
            .boxed()
        });
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<dyn Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
        self.directory.read_dirents(pos, sink).await
    }

    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        self.directory.clone().register_watcher(scope, mask, watcher)
    }

    fn unregister_watcher(self: Arc<Self>, key: usize) {
        self.directory.clone().unregister_watcher(key)
    }
}

impl From<object_store::Directory<FxVolume>> for BlobDirectory {
    fn from(dir: object_store::Directory<FxVolume>) -> Self {
        Self::new(dir.into())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::fuchsia::fxblob::testing::{new_blob_fixture, open_blob_fixture, BlobFixture},
        assert_matches::assert_matches,
        blob_writer::BlobWriter,
        delivery_blob::{delivery_blob_path, CompressionMode, Type1Blob},
        fidl_fuchsia_fxfs::BlobReaderMarker,
        fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _},
        fuchsia_component::client::connect_to_protocol_at_dir_svc,
        fuchsia_fs::directory::{
            readdir_inclusive, DirEntry, DirentKind, WatchEvent, WatchMessage, Watcher,
        },
        fuchsia_zircon::DurationNum as _,
        futures::StreamExt as _,
        std::path::PathBuf,
    };

    #[fasync::run(10, test)]
    async fn test_unlink() {
        let fixture = new_blob_fixture().await;

        let data = [1; 1000];

        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        assert_eq!(fixture.read_blob(hash).await, data);

        fixture
            .root()
            .unlink(&format!("{}", hash), &fio::UnlinkOptions::default())
            .await
            .expect("FIDL failed")
            .expect("unlink failed");

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_readdir() {
        let fixture = new_blob_fixture().await;

        let data = [0xab; 2];
        let hash;
        {
            let mut builder = MerkleTreeBuilder::new();
            builder.write(&data);
            hash = builder.finish().root();
            let compressed_data: Vec<u8> = Type1Blob::generate(&data, CompressionMode::Always);

            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true)
                .expect("failed to serve blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
                .await
                .expect("failed to create BlobWriter");
            blob_writer.write(&compressed_data[..1]).await.unwrap();

            // Before the blob is finished writing, it shouldn't appear in the directory.
            assert_eq!(
                readdir_inclusive(fixture.root()).await.ok(),
                Some(vec![DirEntry { name: ".".to_string(), kind: DirentKind::Directory }])
            );

            blob_writer.write(&compressed_data[1..]).await.unwrap();
        }

        assert_eq!(
            readdir_inclusive(fixture.root()).await.ok(),
            Some(vec![
                DirEntry { name: ".".to_string(), kind: DirentKind::Directory },
                DirEntry { name: format! {"{}", hash}, kind: DirentKind::File },
            ])
        );

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_watchers() {
        let fixture = new_blob_fixture().await;

        let mut watcher = Watcher::new(fixture.root()).await.unwrap();
        assert_eq!(
            watcher.next().await,
            Some(Ok(WatchMessage { event: WatchEvent::EXISTING, filename: PathBuf::from(".") }))
        );
        assert_matches!(
            watcher.next().await,
            Some(Ok(WatchMessage { event: WatchEvent::IDLE, .. }))
        );

        let data = vec![vec![0xab; 2], vec![0xcd; 65_536]];
        let mut hashes = vec![];
        let mut filenames = vec![];
        for datum in data {
            let mut builder = MerkleTreeBuilder::new();
            builder.write(&datum);
            let hash = builder.finish().root();
            let filename = PathBuf::from(format!("{}", hash));
            hashes.push(hash.clone());
            filenames.push(filename.clone());

            let compressed_data: Vec<u8> = Type1Blob::generate(&datum, CompressionMode::Always);
            let (blob_volume_outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create dir proxy to succeed");

            fixture
                .volumes_directory()
                .serve_volume(fixture.volume(), server_end, true)
                .expect("failed to serve blob volume");
            let blob_proxy =
                connect_to_protocol_at_dir_svc::<fidl_fuchsia_fxfs::BlobCreatorMarker>(
                    &blob_volume_outgoing_dir,
                )
                .expect("failed to connect to the Blob service");

            let blob_writer_client_end = blob_proxy
                .create(&hash.into(), false)
                .await
                .expect("transport error on create")
                .expect("failed to create blob");

            let writer = blob_writer_client_end.into_proxy().unwrap();
            let mut blob_writer = BlobWriter::create(writer, compressed_data.len() as u64)
                .await
                .expect("failed to create BlobWriter");
            blob_writer.write(&compressed_data[..compressed_data.len() - 1]).await.unwrap();

            // Before the blob is finished writing, we shouldn't see any watch events for it.
            assert_matches!(
                watcher.next().on_timeout(500.millis().after_now(), || None).await,
                None
            );

            blob_writer.write(&compressed_data[compressed_data.len() - 1..]).await.unwrap();

            assert_eq!(
                watcher.next().await,
                Some(Ok(WatchMessage { event: WatchEvent::ADD_FILE, filename }))
            );
        }

        for (hash, filename) in hashes.iter().zip(filenames) {
            fixture
                .root()
                .unlink(&format!("{}", hash), &fio::UnlinkOptions::default())
                .await
                .expect("FIDL call failed")
                .expect("unlink failed");
            assert_eq!(
                watcher.next().await,
                Some(Ok(WatchMessage { event: WatchEvent::REMOVE_FILE, filename }))
            );
        }

        std::mem::drop(watcher);
        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_rename_fails() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        let (status, token) = fixture.root().get_token().await.expect("FIDL failed");
        Status::ok(status).unwrap();
        fixture
            .root()
            .rename(&format!("{}", delivery_blob_path(hash)), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed")
            .expect_err("rename should fail");

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_link_fails() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;

        let (status, token) = fixture.root().get_token().await.expect("FIDL failed");
        Status::ok(status).unwrap();
        let status = fixture
            .root()
            .link(&format!("{}", hash), token.unwrap().into(), "foo")
            .await
            .expect("FIDL failed");
        assert_eq!(Status::from_raw(status), Status::NOT_SUPPORTED);

        fixture.close().await;
    }

    #[fasync::run(10, test)]
    async fn test_verify_cached_hash_node() {
        let fixture = new_blob_fixture().await;

        let data = vec![];
        let hash = fixture.write_blob(&data, CompressionMode::Never).await;
        let evil_hash =
            Hash::from_str("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();

        // Create a malicious link to the existing blob. This shouldn't be possible without special
        // access either via internal apis or modifying the disk image.
        {
            let root = fixture
                .volume()
                .root()
                .clone()
                .as_node()
                .into_any()
                .downcast::<BlobDirectory>()
                .unwrap()
                .directory()
                .clone();
            root.clone()
                .link(evil_hash.to_string(), root, &hash.to_string())
                .await
                .expect("Linking file");
        }
        let device = fixture.close().await;

        let fixture = open_blob_fixture(device).await;
        {
            // Hold open a ref to keep it in the node cache.
            let _vmo = fixture.get_blob_vmo(hash).await;

            // Open the malicious link
            let blob_reader =
                connect_to_protocol_at_dir_svc::<BlobReaderMarker>(fixture.volume_out_dir())
                    .expect("failed to connect to the BlobReader service");
            blob_reader
                .get_vmo(&evil_hash.into())
                .await
                .expect("transport error on BlobReader.GetVmo")
                .expect_err("Hashes should mismatch");
        }
        fixture.close().await;
    }
}
