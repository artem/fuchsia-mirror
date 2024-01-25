// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        log::*,
        lsm_tree::types::{ItemRef, LayerIterator},
        object_handle::{
            ObjectHandle, ObjectProperties, ReadObjectHandle, WriteBytes, WriteObjectHandle,
        },
        object_store::{
            extent_record::{Checksums, ExtentKey, ExtentValue},
            object_manager::ObjectManager,
            object_record::{
                AttributeKey, ObjectAttributes, ObjectItem, ObjectKey, ObjectKeyData, ObjectKind,
                ObjectValue, Timestamp,
            },
            store_object_handle::NeedsTrim,
            transaction::{
                self, lock_keys, AssocObj, AssociatedObject, LockKey, Mutation,
                ObjectStoreMutation, Options, Transaction,
            },
            FsverityMetadata, HandleOptions, HandleOwner, ObjectStore, RootDigest,
            StoreObjectHandle, TrimMode, TrimResult, DEFAULT_DATA_ATTRIBUTE_ID,
            FSVERITY_MERKLE_ATTRIBUTE_ID, TRANSACTION_MUTATION_THRESHOLD,
        },
        range::RangeExt,
        round::{round_down, round_up},
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fsverity_merkle::{FsVerityHasher, FsVerityHasherOptions, MerkleTreeBuilder},
    futures::{stream::FuturesUnordered, TryStreamExt},
    mundane::hash::{Digest, Hasher, Sha256, Sha512},
    std::{
        cmp::min,
        ops::{Bound, DerefMut, Range},
        sync::{
            atomic::{self, AtomicU64, Ordering},
            Arc, Mutex,
        },
    },
    storage_device::buffer::{Buffer, BufferFuture, BufferRef, MutableBufferRef},
};

/// DataObjectHandle is a typed handle for file-like objects that store data in the default data
/// attribute. In addition to traditional files, this means things like the journal, superblocks,
/// and layer files.
///
/// It caches the content size of the data attribute it was configured for, and has helpers for
/// complex extent manipulation, as well as implementations of ReadObjectHandle and
/// WriteObjectHandle.
pub struct DataObjectHandle<S: HandleOwner> {
    handle: StoreObjectHandle<S>,
    attribute_id: u64,
    content_size: AtomicU64,
    fsverity_state: Mutex<FsverityState>,
}

pub enum FsverityState {
    None,
    Pending(FsverityStateInner),
    Some(FsverityStateInner),
}

pub struct FsverityStateInner {
    descriptor: FsverityMetadata,
    // TODO(b/309656632): This should store the entire merkle tree and not just the leaf nodes.
    // Potentially store a pager-backed vmo instead of passing around a boxed array.
    merkle_tree: Box<[u8]>,
}

impl FsverityStateInner {
    pub fn new(descriptor: FsverityMetadata, merkle_tree: Box<[u8]>) -> Self {
        FsverityStateInner { descriptor, merkle_tree }
    }
}

impl<S: HandleOwner> DataObjectHandle<S> {
    pub fn new(
        owner: Arc<S>,
        object_id: u64,
        permanent_keys: bool,
        attribute_id: u64,
        size: u64,
        fsverity_state: FsverityState,
        options: HandleOptions,
        trace: bool,
    ) -> Self {
        Self {
            handle: StoreObjectHandle::new(owner, object_id, permanent_keys, options, trace),
            attribute_id,
            content_size: AtomicU64::new(size),
            fsverity_state: Mutex::new(fsverity_state),
        }
    }

    pub fn owner(&self) -> &Arc<S> {
        self.handle.owner()
    }

    pub fn attribute_id(&self) -> u64 {
        self.attribute_id
    }

    pub fn verified_file(&self) -> bool {
        matches!(*self.fsverity_state.lock().unwrap(), FsverityState::Some(_))
    }

    pub fn store(&self) -> &ObjectStore {
        self.handle.store()
    }

    pub fn trace(&self) -> bool {
        self.handle.trace()
    }

    pub fn handle(&self) -> &StoreObjectHandle<S> {
        &self.handle
    }

    /// Sets `self.fsverity_state` to Pending. Must be called before `finalize_fsverity_state()`.
    /// Asserts that the prior state of `self.fsverity_state` was `FsverityState::None`.
    pub fn set_fsverity_state_pending(&self, descriptor: FsverityMetadata, merkle_tree: Box<[u8]>) {
        let mut fsverity_guard = self.fsverity_state.lock().unwrap();
        assert!(matches!(*fsverity_guard, FsverityState::None));
        *fsverity_guard = FsverityState::Pending(FsverityStateInner { descriptor, merkle_tree });
    }

    /// Sets `self.fsverity_state` to Some. Panics if the prior state of `self.fsverity_state` was
    /// not `FsverityState::Pending(_)`.
    pub fn finalize_fsverity_state(&self) {
        let mut fsverity_state_guard = self.fsverity_state.lock().unwrap();
        let mut_fsverity_state = fsverity_state_guard.deref_mut();
        let fsverity_state = std::mem::replace(mut_fsverity_state, FsverityState::None);
        match fsverity_state {
            FsverityState::None => panic!("Cannot go from FsverityState::None to Some"),
            FsverityState::Pending(inner) => *mut_fsverity_state = FsverityState::Some(inner),
            FsverityState::Some(_) => panic!("Fsverity state was already set to Some"),
        }
    }

    /// Verifies contents of `buffer` against the corresponding hashes in the stored merkle tree.
    /// `offset` is the logical offset in the file that `buffer` starts at. `offset` must be
    /// block-aligned. Fails on non fsverity-enabled files.
    async fn verify_data(&self, mut offset: usize, buffer: &[u8]) -> Result<(), Error> {
        let block_size = self.block_size() as usize;
        assert!(offset % block_size == 0);
        match &*self.fsverity_state.lock().unwrap() {
            FsverityState::None => {
                Err(anyhow!("Tried to verify read on a non verity-enabled file"))
            }
            FsverityState::Pending(_) => {
                Err(anyhow!("Mutation for fsverity metadata has not yet been applied"))
            }
            FsverityState::Some(metadata) => {
                let (hasher, digest_size) = match metadata.descriptor.root_digest {
                    RootDigest::Sha256(_) => {
                        let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(
                            metadata.descriptor.salt.clone(),
                            block_size,
                        ));
                        (hasher, <Sha256 as Hasher>::Digest::DIGEST_LEN)
                    }
                    RootDigest::Sha512(_) => {
                        let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(
                            metadata.descriptor.salt.clone(),
                            block_size,
                        ));
                        (hasher, <Sha512 as Hasher>::Digest::DIGEST_LEN)
                    }
                };
                let leaf_nodes: Vec<&[u8]> = metadata.merkle_tree.chunks(digest_size).collect();
                fxfs_trace::duration!("fsverity-verify", "len" => buffer.len());
                // TODO(b/318880297): Consider parallelizing computation.
                for b in buffer.chunks(block_size) {
                    ensure!(
                        hasher.hash_block(b) == leaf_nodes[offset / block_size],
                        anyhow!(FxfsError::Inconsistent).context("Hash mismatch")
                    );
                    offset += block_size;
                }
                Ok(())
            }
        }
    }

    /// Extend the file with the given extent.  The only use case for this right now is for files
    /// that must exist at certain offsets on the device, such as super-blocks.
    pub async fn extend<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        device_range: Range<u64>,
    ) -> Result<(), Error> {
        let old_end =
            round_up(self.txn_get_size(transaction), self.block_size()).ok_or(FxfsError::TooBig)?;
        let new_size = old_end + device_range.end - device_range.start;
        self.store()
            .allocator()
            .mark_allocated(transaction, self.store().store_object_id(), device_range.clone())
            .await?;
        transaction.add_with_object(
            self.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    self.object_id(),
                    self.attribute_id(),
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(new_size),
            ),
            AssocObj::Borrowed(self),
        );
        transaction.add(
            self.store().store_object_id,
            Mutation::merge_object(
                ObjectKey::extent(self.object_id(), self.attribute_id(), old_end..new_size),
                ObjectValue::Extent(ExtentValue::new(device_range.start)),
            ),
        );
        self.update_allocated_size(transaction, device_range.end - device_range.start, 0).await
    }

    // Returns a new aligned buffer (reading the head and tail blocks if necessary) with a copy of
    // the data from `buf`.
    async fn align_buffer(
        &self,
        offset: u64,
        buf: BufferRef<'_>,
    ) -> Result<(std::ops::Range<u64>, Buffer<'_>), Error> {
        self.handle.align_buffer(self.attribute_id(), offset, buf).await
    }

    // Writes potentially unaligned data at `device_offset` and returns checksums if requested. The
    // data will be encrypted if necessary.
    // `buf` is mutable as an optimization, since the write may require encryption, we can encrypt
    // the buffer in-place rather than copying to another buffer if the write is already aligned.
    async fn write_at(
        &self,
        offset: u64,
        buf: MutableBufferRef<'_>,
        device_offset: u64,
        compute_checksum: bool,
    ) -> Result<Checksums, Error> {
        self.handle
            .write_at(self.attribute_id(), offset, buf, device_offset, compute_checksum)
            .await
    }

    /// Zeroes the given range.  The range must be aligned.  Returns the amount of data deallocated.
    pub async fn zero(
        &self,
        transaction: &mut Transaction<'_>,
        range: Range<u64>,
    ) -> Result<(), Error> {
        self.handle.zero(transaction, self.attribute_id(), range).await
    }

    /// The cached value for `self.fsverity_state` is set either in `open_object` or on
    /// `enable_verity`. If set, translates `self.fsverity_state.descriptor` into an
    /// fio::VerificationOptions instance and a root hash. Otherwise, returns None.
    pub async fn get_descriptor(
        &self,
    ) -> Result<Option<(fio::VerificationOptions, Vec<u8>)>, Error> {
        match &*self.fsverity_state.lock().unwrap() {
            FsverityState::None => Ok(None),
            FsverityState::Pending(_) => {
                Err(anyhow!("Mutation for fsverity metadata has not yet been applied"))
            }
            FsverityState::Some(metadata) => {
                let (options, root_hash) = match &metadata.descriptor.root_digest {
                    RootDigest::Sha256(root_hash) => {
                        let mut root_vec = root_hash.to_vec();
                        // Need to zero out the rest of the vector so that there's no garbage.
                        root_vec.extend_from_slice(&[0; 32]);
                        (
                            fio::VerificationOptions {
                                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                                salt: Some(metadata.descriptor.salt.clone()),
                                ..Default::default()
                            },
                            root_vec,
                        )
                    }
                    RootDigest::Sha512(root_hash) => (
                        fio::VerificationOptions {
                            hash_algorithm: Some(fio::HashAlgorithm::Sha512),
                            salt: Some(metadata.descriptor.salt.clone()),
                            ..Default::default()
                        },
                        root_hash.clone(),
                    ),
                };
                Ok(Some((options, root_hash)))
            }
        }
    }

    /// Reads the data attribute and computes a merkle tree from the data. The values of the
    /// parameters required to build the merkle tree are supplied by `descriptor` (i.e. salt,
    /// hash_algorithm, etc.) Writes the leaf nodes of the merkle tree to an attribute with id
    /// `FSVERITY_MERKLE_ATTRIBUTE_ID`. Updates the root_hash of the `descriptor` according to the
    /// computed merkle tree and then replaces the ObjectValue of the data attribute with
    /// ObjectValue::VerifiedAttribute, which stores the `descriptor` inline.
    /// TODO(b/314195208): Add locking to ensure file is modified while we are computing hashes.
    pub async fn enable_verity(&self, options: fio::VerificationOptions) -> Result<(), Error> {
        let hash_alg =
            options.hash_algorithm.ok_or_else(|| anyhow!("No hash algorithm provided"))?;
        let salt = options.salt.ok_or_else(|| anyhow!("No salt provided"))?;
        let mut transaction = self.new_transaction().await?;
        let (root_digest, merkle_tree) = match hash_alg {
            fio::HashAlgorithm::Sha256 => {
                let hasher = FsVerityHasher::Sha256(FsVerityHasherOptions::new(
                    salt.clone(),
                    self.block_size() as usize,
                ));
                let mut builder = MerkleTreeBuilder::new(hasher);
                let mut offset = 0;
                let size = self.get_size();
                // TODO(b/314836822): Consider reading more than 4k bytes into memory at a time
                // for a potential performance boost.
                let mut buf = self.allocate_buffer(self.block_size() as usize).await;
                while offset < size {
                    // TODO(b/314842875): Consider optimizations for sparse files.
                    let read = self.read(offset, buf.as_mut()).await? as u64;
                    assert!(offset + read <= size);
                    builder.write(&buf.as_slice()[0..read as usize]);
                    offset += read;
                }
                let tree = builder.finish();
                let merkle_leaf_nodes: Vec<u8> =
                    tree.as_ref()[0].iter().flat_map(|x| x.clone()).collect();
                // TODO(b/314194485): Eventually want streaming writes.
                // The merkle tree attribute should not require trimming because it should not
                // exist.
                if self
                    .handle
                    .write_attr(&mut transaction, FSVERITY_MERKLE_ATTRIBUTE_ID, &merkle_leaf_nodes)
                    .await?
                    .0
                {
                    return Err(anyhow!(FxfsError::AlreadyExists));
                }
                let root: [u8; 32] = tree.root().try_into().unwrap();
                (RootDigest::Sha256(root), merkle_leaf_nodes)
            }
            fio::HashAlgorithm::Sha512 => {
                let hasher = FsVerityHasher::Sha512(FsVerityHasherOptions::new(
                    salt.clone(),
                    self.block_size() as usize,
                ));
                let mut builder = MerkleTreeBuilder::new(hasher);
                let mut offset = 0;
                let size = self.get_size();
                // TODO(b/314836822): Consider reading more than 4k bytes into memory at a time
                // for a potential performance boost.
                let mut buf = self.allocate_buffer(self.block_size() as usize).await;
                while offset < size {
                    // TODO(b/314842875): Consider optimizations for sparse files.
                    let read = self.read(offset, buf.as_mut()).await? as u64;
                    assert!(offset + read <= size);
                    builder.write(&buf.as_slice()[0..read as usize]);
                    offset += read;
                }
                let tree = builder.finish();
                let merkle_leaf_nodes: Vec<u8> =
                    tree.as_ref()[0].iter().flat_map(|x| x.clone()).collect();
                // TODO(b/314194485): Eventually want streaming writes.
                // The merkle tree attribute should not require trimming because it should not
                // exist.
                if self
                    .handle
                    .write_attr(&mut transaction, FSVERITY_MERKLE_ATTRIBUTE_ID, &merkle_leaf_nodes)
                    .await?
                    .0
                {
                    return Err(anyhow!(FxfsError::AlreadyExists));
                }
                (RootDigest::Sha512(tree.root().to_vec()), merkle_leaf_nodes)
            }
            _ => {
                bail!(anyhow!(FxfsError::NotSupported)
                    .context(format!("hash algorithm not supported")));
            }
        };
        let descriptor = FsverityMetadata { root_digest, salt };
        self.set_fsverity_state_pending(descriptor.clone(), merkle_tree.into());
        transaction.add_with_object(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    self.object_id(),
                    DEFAULT_DATA_ATTRIBUTE_ID,
                    AttributeKey::Attribute,
                ),
                ObjectValue::verified_attribute(self.get_size(), descriptor),
            ),
            AssocObj::Borrowed(self),
        );
        transaction.commit().await?;

        Ok(())
    }

    /// Return information on a contiguous set of extents that has the same allocation status,
    /// starting from `start_offset`. The information returned is if this set of extents are marked
    /// allocated/not allocated and also the size of this set (in bytes). This is used when
    /// querying slices for volumes.
    /// This function expects `start_offset` to be aligned to block size
    pub async fn is_allocated(&self, start_offset: u64) -> Result<(bool, u64), Error> {
        let block_size = self.block_size();
        assert_eq!(start_offset % block_size, 0);

        if start_offset > self.get_size() {
            bail!(FxfsError::OutOfRange)
        }

        if start_offset == self.get_size() {
            return Ok((false, 0));
        }

        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let offset_key = ObjectKey::attribute(
            self.object_id(),
            self.attribute_id(),
            AttributeKey::Extent(ExtentKey::search_key_from_offset(start_offset)),
        );
        let mut merger = layer_set.merger();
        let mut iter = merger.seek(Bound::Included(&offset_key)).await?;

        let mut allocated = None;
        let mut end = start_offset;

        loop {
            // Iterate through the extents, each time setting `end` as the end of the previous
            // extent
            match iter.get() {
                Some(ItemRef {
                    key:
                        ObjectKey {
                            object_id,
                            data:
                                ObjectKeyData::Attribute(attribute_id, AttributeKey::Extent(extent_key)),
                        },
                    value: ObjectValue::Extent(extent_value),
                    ..
                }) => {
                    // Equivalent of getting no extents back
                    if *object_id != self.object_id() || *attribute_id != self.attribute_id() {
                        if allocated == Some(false) || allocated.is_none() {
                            end = self.get_size();
                            allocated = Some(false);
                        }
                        break;
                    }
                    ensure!(extent_key.range.is_aligned(block_size), FxfsError::Inconsistent);
                    if extent_key.range.start > end {
                        // If a previous extent has already been visited and we are tracking an
                        // allocated set, we are only interested in an extent where the range of the
                        // current extent follows immediately after the previous one.
                        if allocated == Some(true) {
                            break;
                        } else {
                            // The gap between the previous `end` and this extent is not allocated
                            end = extent_key.range.start;
                            allocated = Some(false);
                            // Continue this iteration, except now the `end` is set to the end of
                            // the "previous" extent which is this gap between the start_offset
                            // and the current extent
                        }
                    }

                    // We can assume that from here, the `end` points to the end of a previous
                    // extent.
                    match extent_value {
                        // The current extent has been allocated
                        ExtentValue::Some { .. } => {
                            // Stop searching if previous extent was marked deleted
                            if allocated == Some(false) {
                                break;
                            }
                            allocated = Some(true);
                        }
                        // This extent has been marked deleted
                        ExtentValue::None => {
                            // Stop searching if previous extent was marked allocated
                            if allocated == Some(true) {
                                break;
                            }
                            allocated = Some(false);
                        }
                    }
                    end = extent_key.range.end;
                }
                // This occurs when there are no extents left
                None => {
                    if allocated == Some(false) || allocated.is_none() {
                        end = self.get_size();
                        allocated = Some(false);
                    }
                    // Otherwise, we were monitoring extents that were allocated, so just exit.
                    break;
                }
                // Non-extent records (Object, Child, GraveyardEntry) are ignored.
                Some(_) => {}
            }
            iter.advance().await?;
        }

        Ok((allocated.unwrap(), end - start_offset))
    }

    pub async fn txn_write<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        offset: u64,
        buf: BufferRef<'_>,
    ) -> Result<(), Error> {
        if buf.is_empty() {
            return Ok(());
        }
        let (aligned, mut transfer_buf) = self.align_buffer(offset, buf).await?;
        self.multi_write(transaction, self.attribute_id(), &[aligned], transfer_buf.as_mut())
            .await?;
        if offset + buf.len() as u64 > self.txn_get_size(transaction) {
            transaction.add_with_object(
                self.store().store_object_id,
                Mutation::replace_or_insert_object(
                    ObjectKey::attribute(
                        self.object_id(),
                        self.attribute_id(),
                        AttributeKey::Attribute,
                    ),
                    ObjectValue::attribute(offset + buf.len() as u64),
                ),
                AssocObj::Borrowed(self),
            );
        }
        Ok(())
    }

    // Writes to multiple ranges with data provided in `buf`.  The buffer can be modified in place
    // if encryption takes place.  The ranges must all be aligned and no change to content size is
    // applied; the caller is responsible for updating size if required.
    pub async fn multi_write<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attribute_id: u64,
        ranges: &[Range<u64>],
        buf: MutableBufferRef<'_>,
    ) -> Result<(), Error> {
        self.handle.multi_write(transaction, attribute_id, ranges, buf).await
    }

    // If allow_allocations is false, then all the extents for the range must have been
    // preallocated using preallocate_range or from existing writes.
    //
    // `buf` is mutable as an optimization, since the write may require encryption, we can
    // encrypt the buffer in-place rather than copying to another buffer if the write is
    // already aligned.
    //
    // Note: in the event of power failure during an overwrite() call, it is possible that
    // old data (which hasn't been overwritten with new bytes yet) may be exposed to the user.
    // Since the old data should be encrypted, it is probably safe to expose, although not ideal.
    pub async fn overwrite(
        &self,
        mut offset: u64,
        mut buf: MutableBufferRef<'_>,
        allow_allocations: bool,
    ) -> Result<(), Error> {
        assert_eq!((buf.len() as u32) % self.store().device.block_size(), 0);
        let end = offset + buf.len() as u64;

        // The transaction only ends up being used if allow_allocations is true
        let mut transaction =
            if allow_allocations { Some(self.new_transaction().await?) } else { None };

        // We build up a list of writes to perform later
        let writes = FuturesUnordered::new();

        // We create a new scope here, so that the merger iterator will get dropped before we try to
        // commit our transaction. Otherwise the transaction commit would block.
        {
            let store = self.store();
            let store_object_id = store.store_object_id;
            let allocator = store.allocator();
            let tree = &store.tree;
            let layer_set = tree.layer_set();
            let mut merger = layer_set.merger();
            let mut iter = merger
                .seek(Bound::Included(&ObjectKey::attribute(
                    self.object_id(),
                    self.attribute_id(),
                    AttributeKey::Extent(ExtentKey::search_key_from_offset(offset)),
                )))
                .await?;
            let block_size = self.block_size();

            loop {
                let (device_offset, bytes_to_write, should_advance) = match iter.get() {
                    Some(ItemRef {
                        key:
                            ObjectKey {
                                object_id,
                                data:
                                    ObjectKeyData::Attribute(
                                        attribute_id,
                                        AttributeKey::Extent(ExtentKey { range }),
                                    ),
                            },
                        value,
                        ..
                    }) if *object_id == self.object_id()
                        && *attribute_id == self.attribute_id()
                        && range.start <= offset =>
                    {
                        match value {
                            ObjectValue::Extent(ExtentValue::Some {
                                device_offset,
                                checksums,
                                ..
                            }) => {
                                match checksums {
                                    Checksums::None => {
                                        ensure!(
                                            range.is_aligned(block_size)
                                                && device_offset % block_size == 0,
                                            FxfsError::Inconsistent
                                        );
                                        let offset_within_extent = offset - range.start;
                                        let remaining_length_of_extent = (range
                                            .end
                                            .checked_sub(offset)
                                            .ok_or(FxfsError::Inconsistent)?)
                                            as usize;
                                        // Yields (device_offset, bytes_to_write, should_advance)
                                        (
                                            device_offset + offset_within_extent,
                                            min(buf.len(), remaining_length_of_extent),
                                            true,
                                        )
                                    }
                                    _ => {
                                        // TODO(https://fxbug.dev/42066056): Maybe we should create
                                        // a new extent without checksums?
                                        bail!(
                                            "extent from ({},{}) which overlaps offset \
                                                {} has checksums, overwrite is not supported",
                                            range.start,
                                            range.end,
                                            offset
                                        )
                                    }
                                }
                            }
                            _ => {
                                bail!(
                                    "overwrite failed: extent overlapping offset {} has \
                                      unexpected ObjectValue",
                                    offset
                                )
                            }
                        }
                    }
                    maybe_item_ref => {
                        if let Some(transaction) = transaction.as_mut() {
                            assert_eq!(allow_allocations, true);
                            assert_eq!(offset % self.block_size(), 0);

                            // We are going to make a new extent, but let's check if there is an
                            // extent after us. If there is an extent after us, then we don't want
                            // our new extent to bump into it...
                            let mut bytes_to_allocate =
                                round_up(buf.len() as u64, self.block_size())
                                    .ok_or(FxfsError::TooBig)?;
                            if let Some(ItemRef {
                                key:
                                    ObjectKey {
                                        object_id,
                                        data:
                                            ObjectKeyData::Attribute(
                                                attribute_id,
                                                AttributeKey::Extent(ExtentKey { range }),
                                            ),
                                    },
                                ..
                            }) = maybe_item_ref
                            {
                                if *object_id == self.object_id()
                                    && *attribute_id == self.attribute_id()
                                    && offset < range.start
                                {
                                    let bytes_until_next_extent = range.start - offset;
                                    bytes_to_allocate =
                                        min(bytes_to_allocate, bytes_until_next_extent);
                                }
                            }

                            let device_range = allocator
                                .allocate(transaction, store_object_id, bytes_to_allocate)
                                .await?;
                            let device_range_len = device_range.end - device_range.start;
                            transaction.add(
                                store_object_id,
                                Mutation::insert_object(
                                    ObjectKey::extent(
                                        self.object_id(),
                                        self.attribute_id(),
                                        offset..offset + device_range_len,
                                    ),
                                    ObjectValue::Extent(ExtentValue::new(device_range.start)),
                                ),
                            );

                            self.update_allocated_size(transaction, device_range_len, 0).await?;

                            // Yields (device_offset, bytes_to_write, should_advance)
                            (device_range.start, min(buf.len(), device_range_len as usize), false)
                        } else {
                            bail!(
                                "no extent overlapping offset {}, \
                                and new allocations are not allowed",
                                offset
                            )
                        }
                    }
                };
                let (current_buf, remaining_buf) = buf.split_at_mut(bytes_to_write);
                writes.push(self.write_at(offset, current_buf, device_offset, false));
                if remaining_buf.len() == 0 {
                    break;
                } else {
                    buf = remaining_buf;
                    offset += bytes_to_write as u64;
                    if should_advance {
                        iter.advance().await?;
                    }
                }
            }
        }

        self.store().logical_write_ops.fetch_add(1, Ordering::Relaxed);
        // The checksums are being ignored here, but we don't need to know them
        writes.try_collect::<Vec<Checksums>>().await?;

        if let Some(mut transaction) = transaction {
            assert_eq!(allow_allocations, true);
            if !transaction.is_empty() {
                if end > self.get_size() {
                    self.grow(&mut transaction, self.get_size(), end).await?;
                }
                transaction.commit().await?;
            }
        }

        Ok(())
    }

    // Within a transaction, the size of the object might have changed, so get the size from there
    // if it exists, otherwise, fall back on the cached size.
    fn txn_get_size(&self, transaction: &Transaction<'_>) -> u64 {
        transaction
            .get_object_mutation(
                self.store().store_object_id,
                ObjectKey::attribute(
                    self.object_id(),
                    self.attribute_id(),
                    AttributeKey::Attribute,
                ),
            )
            .and_then(|m| {
                if let ObjectItem { value: ObjectValue::Attribute { size }, .. } = m.item {
                    Some(size)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.get_size())
    }

    async fn update_allocated_size(
        &self,
        transaction: &mut Transaction<'_>,
        allocated: u64,
        deallocated: u64,
    ) -> Result<(), Error> {
        self.handle.update_allocated_size(transaction, allocated, deallocated).await
    }

    pub async fn shrink<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        size: u64,
    ) -> Result<NeedsTrim, Error> {
        let needs_trim = self.handle.shrink(transaction, self.attribute_id(), size).await?;
        transaction.add_with_object(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    self.object_id(),
                    self.attribute_id(),
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(size),
            ),
            AssocObj::Borrowed(self),
        );
        Ok(needs_trim)
    }

    pub async fn grow<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        old_size: u64,
        size: u64,
    ) -> Result<(), Error> {
        // Before growing the file, we must make sure that a previous trim has completed.
        let store = self.store();
        while matches!(
            store
                .trim_some(
                    transaction,
                    self.object_id(),
                    self.attribute_id(),
                    TrimMode::FromOffset(old_size)
                )
                .await?,
            TrimResult::Incomplete
        ) {
            transaction.commit_and_continue().await?;
        }
        // We might need to zero out the tail of the old last block.
        let block_size = self.block_size();
        if old_size % block_size != 0 {
            let layer_set = store.tree.layer_set();
            let mut merger = layer_set.merger();
            let aligned_old_size = round_down(old_size, block_size);
            let iter = merger
                .seek(Bound::Included(&ObjectKey::extent(
                    self.object_id(),
                    self.attribute_id(),
                    aligned_old_size..aligned_old_size + 1,
                )))
                .await?;
            if let Some(ItemRef {
                key:
                    ObjectKey {
                        object_id,
                        data:
                            ObjectKeyData::Attribute(attribute_id, AttributeKey::Extent(extent_key)),
                    },
                value: ObjectValue::Extent(ExtentValue::Some { device_offset, key_id, .. }),
                ..
            }) = iter.get()
            {
                if *object_id == self.object_id() && *attribute_id == self.attribute_id() {
                    let device_offset = device_offset
                        .checked_add(aligned_old_size - extent_key.range.start)
                        .ok_or(FxfsError::Inconsistent)?;
                    ensure!(device_offset % block_size == 0, FxfsError::Inconsistent);
                    let mut buf = self.allocate_buffer(block_size as usize).await;
                    self.read_and_decrypt(device_offset, aligned_old_size, buf.as_mut(), *key_id)
                        .await?;
                    buf.as_mut_slice()[(old_size % block_size) as usize..].fill(0);
                    self.multi_write(
                        transaction,
                        *attribute_id,
                        &[aligned_old_size..aligned_old_size + block_size],
                        buf.as_mut(),
                    )
                    .await?;
                }
            }
        }
        transaction.add_with_object(
            store.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    self.object_id(),
                    self.attribute_id(),
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(size),
            ),
            AssocObj::Borrowed(self),
        );
        Ok(())
    }

    /// Attempts to pre-allocate a `file_range` of bytes for this object.
    /// Returns a set of device ranges (i.e. potentially multiple extents).
    ///
    /// It may not be possible to preallocate the entire requested range in one request
    /// due to limitations on transaction size. In such cases, we will preallocate as much as
    /// we can up to some (arbitrary, internal) limit on transaction size.
    ///
    /// `file_range.start` is modified to point at the end of the logical range
    /// that was preallocated such that repeated calls to `preallocate_range` with new
    /// transactions can be used to preallocate ranges of any size.
    ///
    /// Requested range must be a multiple of block size.
    pub async fn preallocate_range<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        file_range: &mut Range<u64>,
    ) -> Result<Vec<Range<u64>>, Error> {
        let block_size = self.block_size();
        assert!(file_range.is_aligned(block_size));
        assert!(!self.handle.is_encrypted());
        let mut ranges = Vec::new();
        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .seek(Bound::Included(&ObjectKey::attribute(
                self.object_id(),
                self.attribute_id(),
                AttributeKey::Extent(ExtentKey::search_key_from_offset(file_range.start)),
            )))
            .await?;
        let mut allocated = 0;
        'outer: while file_range.start < file_range.end {
            let allocate_end = loop {
                match iter.get() {
                    // Case for allocated extents for the same object that overlap with file_range.
                    Some(ItemRef {
                        key:
                            ObjectKey {
                                object_id,
                                data:
                                    ObjectKeyData::Attribute(
                                        attribute_id,
                                        AttributeKey::Extent(ExtentKey { range }),
                                    ),
                            },
                        value: ObjectValue::Extent(ExtentValue::Some { device_offset, .. }),
                        ..
                    }) if *object_id == self.object_id()
                        && *attribute_id == self.attribute_id()
                        && range.start < file_range.end =>
                    {
                        ensure!(
                            range.is_valid()
                                && range.is_aligned(block_size)
                                && device_offset % block_size == 0,
                            FxfsError::Inconsistent
                        );
                        // If the start of the requested file_range overlaps with an existing extent...
                        if range.start <= file_range.start {
                            // Record the existing extent and move on.
                            let device_range = device_offset
                                .checked_add(file_range.start - range.start)
                                .ok_or(FxfsError::Inconsistent)?
                                ..device_offset
                                    .checked_add(min(range.end, file_range.end) - range.start)
                                    .ok_or(FxfsError::Inconsistent)?;
                            file_range.start += device_range.end - device_range.start;
                            ranges.push(device_range);
                            if file_range.start >= file_range.end {
                                break 'outer;
                            }
                            iter.advance().await?;
                            continue;
                        } else {
                            // There's nothing allocated between file_range.start and the beginning
                            // of this extent.
                            break range.start;
                        }
                    }
                    // Case for deleted extents eclipsed by file_range.
                    Some(ItemRef {
                        key:
                            ObjectKey {
                                object_id,
                                data:
                                    ObjectKeyData::Attribute(
                                        attribute_id,
                                        AttributeKey::Extent(ExtentKey { range }),
                                    ),
                            },
                        value: ObjectValue::Extent(ExtentValue::None),
                        ..
                    }) if *object_id == self.object_id()
                        && *attribute_id == self.attribute_id()
                        && range.end < file_range.end =>
                    {
                        iter.advance().await?;
                    }
                    _ => {
                        // We can just preallocate the rest.
                        break file_range.end;
                    }
                }
            };
            let device_range = self
                .store()
                .allocator()
                .allocate(
                    transaction,
                    self.store().store_object_id(),
                    allocate_end - file_range.start,
                )
                .await
                .context("Allocation failed")?;
            allocated += device_range.end - device_range.start;
            let this_file_range =
                file_range.start..file_range.start + device_range.end - device_range.start;
            file_range.start = this_file_range.end;
            transaction.add(
                self.store().store_object_id,
                Mutation::merge_object(
                    ObjectKey::extent(self.object_id(), self.attribute_id(), this_file_range),
                    ObjectValue::Extent(ExtentValue::new(device_range.start)),
                ),
            );
            ranges.push(device_range);
            // If we didn't allocate all that we requested, we'll loop around and try again.
            // ... unless we have filled the transaction. The caller should check file_range.
            if transaction.mutations().len() > TRANSACTION_MUTATION_THRESHOLD {
                break;
            }
        }
        // Update the file size if it changed.
        if file_range.start > round_up(self.txn_get_size(transaction), block_size).unwrap() {
            transaction.add_with_object(
                self.store().store_object_id,
                Mutation::replace_or_insert_object(
                    ObjectKey::attribute(
                        self.object_id(),
                        self.attribute_id(),
                        AttributeKey::Attribute,
                    ),
                    ObjectValue::attribute(file_range.start),
                ),
                AssocObj::Borrowed(self),
            );
        }
        self.update_allocated_size(transaction, allocated, 0).await?;
        Ok(ranges)
    }

    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        node_attributes: Option<&fio::MutableNodeAttributes>,
        change_time: Option<Timestamp>,
    ) -> Result<(), Error> {
        self.handle.update_attributes(transaction, node_attributes, change_time).await
    }

    /// Get the default set of transaction options for this object. This is mostly the overall
    /// default, modified by any [`HandleOptions`] held by this handle.
    pub fn default_transaction_options<'b>(&self) -> Options<'b> {
        self.handle.default_transaction_options()
    }

    pub async fn new_transaction<'b>(&self) -> Result<Transaction<'b>, Error> {
        self.new_transaction_with_options(self.default_transaction_options()).await
    }

    pub async fn new_transaction_with_options<'b>(
        &self,
        options: Options<'b>,
    ) -> Result<Transaction<'b>, Error> {
        self.handle.new_transaction_with_options(self.attribute_id(), options).await
    }

    /// Flushes the underlying device.  This is expensive and should be used sparingly.
    pub async fn flush_device(&self) -> Result<(), Error> {
        self.handle.flush_device().await
    }

    /// Reads an entire attribute.
    pub async fn read_attr(&self, attribute_id: u64) -> Result<Option<Box<[u8]>>, Error> {
        self.handle.read_attr(attribute_id).await
    }

    /// Writes an entire attribute.
    pub async fn write_attr(&self, attribute_id: u64, data: &[u8]) -> Result<(), Error> {
        // Must be different attribute otherwise cached size gets out of date.
        assert_ne!(attribute_id, self.attribute_id());
        let store = self.store();
        let mut transaction = self.new_transaction().await?;
        if self.handle.write_attr(&mut transaction, attribute_id, data).await?.0 {
            transaction.commit_and_continue().await?;
            while matches!(
                store
                    .trim_some(
                        &mut transaction,
                        self.object_id(),
                        attribute_id,
                        TrimMode::FromOffset(data.len() as u64),
                    )
                    .await?,
                TrimResult::Incomplete
            ) {
                transaction.commit_and_continue().await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn read_and_decrypt(
        &self,
        device_offset: u64,
        file_offset: u64,
        buffer: MutableBufferRef<'_>,
        key_id: u64,
    ) -> Result<(), Error> {
        self.handle.read_and_decrypt(device_offset, file_offset, buffer, key_id).await
    }

    /// Truncates a file to a given size (growing/shrinking as required).
    ///
    /// Nb: Most code will want to call truncate() instead. This method is used
    /// to update the super block -- a case where we must borrow metadata space.
    pub async fn truncate_with_options(
        &self,
        options: Options<'_>,
        size: u64,
    ) -> Result<(), Error> {
        let mut transaction = self.new_transaction_with_options(options).await?;
        let old_size = self.get_size();
        if size == old_size {
            return Ok(());
        }
        if size < old_size {
            if self.shrink(&mut transaction, size).await?.0 {
                // The file needs to be trimmed.
                transaction.commit_and_continue().await?;
                let store = self.store();
                while matches!(
                    store
                        .trim_some(
                            &mut transaction,
                            self.object_id(),
                            self.attribute_id(),
                            TrimMode::FromOffset(size)
                        )
                        .await?,
                    TrimResult::Incomplete
                ) {
                    if let Err(error) = transaction.commit_and_continue().await {
                        warn!(?error, "Failed to trim after truncate");
                        return Ok(());
                    }
                }
                if let Err(error) = transaction.commit().await {
                    warn!(?error, "Failed to trim after truncate");
                }
                return Ok(());
            }
        } else {
            self.grow(&mut transaction, old_size, size).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        // We don't take a read guard here since the object properties are contained in a single
        // object, which cannot be inconsistent with itself. The LSM tree does not return
        // intermediate states for a single object.
        let item = self
            .store()
            .tree
            .find(&ObjectKey::object(self.object_id()))
            .await?
            .expect("Unable to find object record");
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::File { refs, .. },
                attributes:
                    ObjectAttributes {
                        creation_time,
                        modification_time,
                        posix_attributes,
                        allocated_size,
                        access_time,
                        change_time,
                        ..
                    },
            } => Ok(ObjectProperties {
                refs,
                allocated_size,
                data_attribute_size: self.get_size(),
                creation_time,
                modification_time,
                access_time,
                change_time,
                sub_dirs: 0,
                posix_attributes,
            }),
            _ => bail!(FxfsError::NotFile),
        }
    }

    // Returns the contents of this object. This object must be < |limit| bytes in size.
    pub async fn contents(&self, limit: usize) -> Result<Box<[u8]>, Error> {
        let size = self.get_size();
        if size > limit as u64 {
            bail!("Object too big ({} > {})", size, limit);
        }
        let mut buf = self.allocate_buffer(size as usize).await;
        self.read(0u64, buf.as_mut()).await?;
        Ok(buf.as_slice().into())
    }
}

impl<S: HandleOwner> AssociatedObject for DataObjectHandle<S> {
    fn will_apply_mutation(&self, mutation: &Mutation, _object_id: u64, _manager: &ObjectManager) {
        match mutation {
            Mutation::ObjectStore(ObjectStoreMutation {
                item: ObjectItem { value: ObjectValue::Attribute { size }, .. },
                ..
            }) => self.content_size.store(*size, atomic::Ordering::Relaxed),
            Mutation::ObjectStore(ObjectStoreMutation {
                item: ObjectItem { value: ObjectValue::VerifiedAttribute { .. }, .. },
                ..
            }) => self.finalize_fsverity_state(),
            _ => {}
        }
    }
}

impl<S: HandleOwner> ObjectHandle for DataObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        self.handle.set_trace(v)
    }

    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.handle.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.handle.block_size()
    }
}

#[async_trait]
impl<S: HandleOwner> ReadObjectHandle for DataObjectHandle<S> {
    async fn read(&self, offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        let fs = self.store().filesystem();
        let guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object_attribute(
                self.store().store_object_id,
                self.object_id(),
                self.attribute_id(),
            )])
            .await;

        let size = self.get_size();
        if offset >= size {
            return Ok(0);
        }
        let length = min(buf.len() as u64, size - offset) as usize;
        buf = buf.subslice_mut(0..length);
        self.handle.read_unchecked(self.attribute_id(), offset, buf.reborrow(), &guard).await?;
        if self.verified_file() {
            self.verify_data(offset as usize, buf.as_slice()).await?;
        }
        Ok(length)
    }

    fn get_size(&self) -> u64 {
        self.content_size.load(atomic::Ordering::Relaxed)
    }
}

impl<S: HandleOwner> WriteObjectHandle for DataObjectHandle<S> {
    async fn write_or_append(&self, offset: Option<u64>, buf: BufferRef<'_>) -> Result<u64, Error> {
        let offset = offset.unwrap_or(self.get_size());
        let mut transaction = self.new_transaction().await?;
        self.txn_write(&mut transaction, offset, buf).await?;
        let new_size = self.txn_get_size(&transaction);
        transaction.commit().await?;
        Ok(new_size)
    }

    async fn truncate(&self, size: u64) -> Result<(), Error> {
        self.truncate_with_options(self.default_transaction_options(), size).await
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// Like object_handle::Writer, but allows custom transaction options to be set, and makes every
/// write go directly to the handle in a transaction.
pub struct DirectWriter<'a, S: HandleOwner> {
    handle: &'a DataObjectHandle<S>,
    options: transaction::Options<'a>,
    buffer: Buffer<'a>,
    offset: u64,
    buf_offset: usize,
}

const BUFFER_SIZE: usize = 1_048_576;

impl<S: HandleOwner> Drop for DirectWriter<'_, S> {
    fn drop(&mut self) {
        if self.buf_offset != 0 {
            warn!("DirectWriter: dropping data, did you forget to call complete?");
        }
    }
}

impl<'a, S: HandleOwner> DirectWriter<'a, S> {
    pub async fn new(
        handle: &'a DataObjectHandle<S>,
        options: transaction::Options<'a>,
    ) -> DirectWriter<'a, S> {
        Self {
            handle,
            options,
            buffer: handle.allocate_buffer(BUFFER_SIZE).await,
            offset: 0,
            buf_offset: 0,
        }
    }

    async fn flush(&mut self) -> Result<(), Error> {
        let mut transaction = self.handle.new_transaction_with_options(self.options).await?;
        self.handle
            .txn_write(&mut transaction, self.offset, self.buffer.subslice(..self.buf_offset))
            .await?;
        transaction.commit().await?;
        self.offset += self.buf_offset as u64;
        self.buf_offset = 0;
        Ok(())
    }
}

impl<'a, S: HandleOwner> WriteBytes for DirectWriter<'a, S> {
    fn block_size(&self) -> u64 {
        self.handle.block_size()
    }

    async fn write_bytes(&mut self, mut buf: &[u8]) -> Result<(), Error> {
        while buf.len() > 0 {
            let to_do = std::cmp::min(buf.len(), BUFFER_SIZE - self.buf_offset);
            self.buffer
                .subslice_mut(self.buf_offset..self.buf_offset + to_do)
                .as_mut_slice()
                .copy_from_slice(&buf[..to_do]);
            self.buf_offset += to_do;
            if self.buf_offset == BUFFER_SIZE {
                self.flush().await?;
            }
            buf = &buf[to_do..];
        }
        Ok(())
    }

    async fn complete(&mut self) -> Result<(), Error> {
        self.flush().await?;
        Ok(())
    }

    async fn skip(&mut self, amount: u64) -> Result<(), Error> {
        if (BUFFER_SIZE - self.buf_offset) as u64 > amount {
            self.buffer
                .subslice_mut(self.buf_offset..self.buf_offset + amount as usize)
                .as_mut_slice()
                .fill(0);
            self.buf_offset += amount as usize;
        } else {
            self.flush().await?;
            self.offset += amount;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            errors::FxfsError,
            filesystem::{
                FxFilesystem, FxFilesystemBuilder, JournalingObject, OpenFxFilesystem, SyncOptions,
            },
            fsck::{fsck_volume_with_options, fsck_with_options, FsckOptions},
            object_handle::{ObjectHandle, ObjectProperties, ReadObjectHandle, WriteObjectHandle},
            object_store::{
                directory::replace_child,
                object_record::{ObjectKey, ObjectValue, Timestamp},
                transaction::{lock_keys, Mutation, Options},
                volume::root_volume,
                DataObjectHandle, Directory, HandleOptions, LockKey, ObjectStore, PosixAttributes,
                TRANSACTION_MUTATION_THRESHOLD,
            },
            round::{round_down, round_up},
        },
        assert_matches::assert_matches,
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        futures::{
            channel::oneshot::channel,
            stream::{FuturesUnordered, StreamExt},
            FutureExt,
        },
        fxfs_crypto::Crypt,
        fxfs_insecure_crypto::InsecureCrypt,
        rand::Rng,
        std::{
            ops::Range,
            sync::{Arc, Mutex},
            time::Duration,
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    // Some tests (the preallocate_range ones) currently assume that the data only occupies a single
    // device block.
    const TEST_DATA_OFFSET: u64 = 5000;
    const TEST_DATA: &[u8] = b"hello";
    const TEST_OBJECT_SIZE: u64 = 5678;
    const TEST_OBJECT_ALLOCATED_SIZE: u64 = 4096;
    const TEST_OBJECT_NAME: &str = "foo";

    async fn test_filesystem() -> OpenFxFilesystem {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        FxFilesystem::new_empty(device).await.expect("new_empty failed")
    }

    async fn test_filesystem_and_object_with_key(
        crypt: Option<&dyn Crypt>,
        write_object_test_data: bool,
    ) -> (OpenFxFilesystem, DataObjectHandle<ObjectStore>) {
        let fs = test_filesystem().await;
        let store = fs.root_store();
        let object;

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    store.store_object_id(),
                    store.root_directory_object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        object = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            crypt,
            None,
        )
        .await
        .expect("create_object failed");

        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        root_directory
            .add_child_file(&mut transaction, TEST_OBJECT_NAME, &object)
            .await
            .expect("add_child_file failed");

        if write_object_test_data {
            let align = TEST_DATA_OFFSET as usize % TEST_DEVICE_BLOCK_SIZE as usize;
            let mut buf = object.allocate_buffer(align + TEST_DATA.len()).await;
            buf.as_mut_slice()[align..].copy_from_slice(TEST_DATA);
            object
                .txn_write(&mut transaction, TEST_DATA_OFFSET, buf.subslice(align..))
                .await
                .expect("write failed");
        }
        transaction.commit().await.expect("commit failed");
        object.truncate(TEST_OBJECT_SIZE).await.expect("truncate failed");
        (fs, object)
    }

    async fn test_filesystem_and_object() -> (OpenFxFilesystem, DataObjectHandle<ObjectStore>) {
        test_filesystem_and_object_with_key(Some(&InsecureCrypt::new()), true).await
    }

    async fn test_filesystem_and_empty_object() -> (OpenFxFilesystem, DataObjectHandle<ObjectStore>)
    {
        test_filesystem_and_object_with_key(Some(&InsecureCrypt::new()), false).await
    }

    #[fuchsia::test]
    async fn test_zero_buf_len_read() {
        let (fs, object) = test_filesystem_and_object().await;
        let mut buf = object.allocate_buffer(0).await;
        assert_eq!(object.read(0u64, buf.as_mut()).await.expect("read failed"), 0);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_beyond_eof_read() {
        let (fs, object) = test_filesystem_and_object().await;
        let offset = TEST_OBJECT_SIZE as usize - 2;
        let align = offset % fs.block_size() as usize;
        let len: usize = 2;
        let mut buf = object.allocate_buffer(align + len + 1).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(
            object.read((offset - align) as u64, buf.as_mut()).await.expect("read failed"),
            align + len
        );
        assert_eq!(&buf.as_slice()[align..align + len], &vec![0u8; len]);
        assert_eq!(&buf.as_slice()[align + len..], &vec![123u8; buf.len() - align - len]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_beyond_eof_read_from() {
        let (fs, object) = test_filesystem_and_object().await;
        let offset = TEST_OBJECT_SIZE as usize - 2;
        let align = offset % fs.block_size() as usize;
        let len: usize = 2;
        let mut buf = object.allocate_buffer(align + len + 1).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(
            object
                .handle()
                .read(0, (offset - align) as u64, buf.as_mut())
                .await
                .expect("read failed"),
            align + len
        );
        assert_eq!(&buf.as_slice()[align..align + len], &vec![0u8; len]);
        assert_eq!(&buf.as_slice()[align + len..], &vec![123u8; buf.len() - align - len]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_beyond_eof_read_unchecked() {
        let (fs, object) = test_filesystem_and_object().await;
        let offset = TEST_OBJECT_SIZE as usize - 2;
        let align = offset % fs.block_size() as usize;
        let len: usize = 2;
        let mut buf = object.allocate_buffer(align + len + 1).await;
        buf.as_mut_slice().fill(123u8);
        let guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object_attribute(
                object.store().store_object_id,
                object.object_id(),
                0,
            )])
            .await;
        object
            .handle()
            .read_unchecked(0, (offset - align) as u64, buf.as_mut(), &guard)
            .await
            .expect("read failed");
        assert_eq!(&buf.as_slice()[align..], &vec![0u8; len + 1]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_sparse() {
        let (fs, object) = test_filesystem_and_object().await;
        // Deliberately read not right to eof.
        let len = TEST_OBJECT_SIZE as usize - 1;
        let mut buf = object.allocate_buffer(len).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), len);
        let mut expected = vec![0; len];
        let offset = TEST_DATA_OFFSET as usize;
        expected[offset..offset + TEST_DATA.len()].copy_from_slice(TEST_DATA);
        assert_eq!(buf.as_slice()[..len], expected[..]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_after_writes_interspersed_with_flush() {
        let (fs, object) = test_filesystem_and_object().await;

        object.owner().flush().await.expect("flush failed");

        // Write more test data to the first block fo the file.
        let mut buf = object.allocate_buffer(TEST_DATA.len()).await;
        buf.as_mut_slice().copy_from_slice(TEST_DATA);
        object.write_or_append(Some(0u64), buf.as_ref()).await.expect("write failed");

        let len = TEST_OBJECT_SIZE as usize - 1;
        let mut buf = object.allocate_buffer(len).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), len);

        let mut expected = vec![0u8; len];
        let offset = TEST_DATA_OFFSET as usize;
        expected[offset..offset + TEST_DATA.len()].copy_from_slice(TEST_DATA);
        expected[..TEST_DATA.len()].copy_from_slice(TEST_DATA);
        assert_eq!(buf.as_slice(), &expected);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_after_truncate_and_extend() {
        let (fs, object) = test_filesystem_and_object().await;

        // Arrange for there to be <extent><deleted-extent><extent>.
        let mut buf = object.allocate_buffer(TEST_DATA.len()).await;
        buf.as_mut_slice().copy_from_slice(TEST_DATA);
        // This adds an extent at 0..512.
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        // This deletes 512..1024.
        object.truncate(3).await.expect("truncate failed");
        let data = b"foo";
        let offset = 1500u64;
        let align = (offset % fs.block_size() as u64) as usize;
        let mut buf = object.allocate_buffer(align + data.len()).await;
        buf.as_mut_slice()[align..].copy_from_slice(data);
        // This adds 1024..1536.
        object.write_or_append(Some(1500), buf.subslice(align..)).await.expect("write failed");

        const LEN1: usize = 1503;
        let mut buf = object.allocate_buffer(LEN1).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), LEN1);
        let mut expected = [0; LEN1];
        expected[..3].copy_from_slice(&TEST_DATA[..3]);
        expected[1500..].copy_from_slice(b"foo");
        assert_eq!(buf.as_slice(), &expected);

        // Also test a read that ends midway through the deleted extent.
        const LEN2: usize = 601;
        let mut buf = object.allocate_buffer(LEN2).await;
        buf.as_mut_slice().fill(123u8);
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), LEN2);
        assert_eq!(buf.as_slice(), &expected[..LEN2]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_whole_blocks_with_multiple_objects() {
        let (fs, object) = test_filesystem_and_object().await;
        let block_size = object.block_size() as usize;
        let mut buffer = object.allocate_buffer(block_size).await;
        buffer.as_mut_slice().fill(0xaf);
        object.write_or_append(Some(0), buffer.as_ref()).await.expect("write failed");

        let store = object.owner();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let object2 = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");
        let mut ef_buffer = object.allocate_buffer(block_size).await;
        ef_buffer.as_mut_slice().fill(0xef);
        object2.write_or_append(Some(0), ef_buffer.as_ref()).await.expect("write failed");

        let mut buffer = object.allocate_buffer(block_size).await;
        buffer.as_mut_slice().fill(0xaf);
        object
            .write_or_append(Some(block_size as u64), buffer.as_ref())
            .await
            .expect("write failed");
        object.truncate(3 * block_size as u64).await.expect("truncate failed");
        object2
            .write_or_append(Some(block_size as u64), ef_buffer.as_ref())
            .await
            .expect("write failed");

        let mut buffer = object.allocate_buffer(4 * block_size).await;
        buffer.as_mut_slice().fill(123);
        assert_eq!(object.read(0, buffer.as_mut()).await.expect("read failed"), 3 * block_size);
        assert_eq!(&buffer.as_slice()[..2 * block_size], &vec![0xaf; 2 * block_size]);
        assert_eq!(&buffer.as_slice()[2 * block_size..3 * block_size], &vec![0; block_size]);
        assert_eq!(object2.read(0, buffer.as_mut()).await.expect("read failed"), 2 * block_size);
        assert_eq!(&buffer.as_slice()[..2 * block_size], &vec![0xef; 2 * block_size]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_alignment() {
        let (fs, object) = test_filesystem_and_object().await;

        struct AlignTest {
            fill: u8,
            object: DataObjectHandle<ObjectStore>,
            mirror: Vec<u8>,
        }

        impl AlignTest {
            async fn new(object: DataObjectHandle<ObjectStore>) -> Self {
                let mirror = {
                    let mut buf = object.allocate_buffer(object.get_size() as usize).await;
                    assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), buf.len());
                    buf.as_slice().to_vec()
                };
                Self { fill: 0, object, mirror }
            }

            // Fills |range| of self.object with a byte value (self.fill) and mirrors the same
            // operation to an in-memory copy of the object.
            // Each subsequent call bumps the value of fill.
            // It is expected that the object and its mirror maintain identical content.
            async fn test(&mut self, range: Range<u64>) {
                let mut buf = self.object.allocate_buffer((range.end - range.start) as usize).await;
                self.fill += 1;
                buf.as_mut_slice().fill(self.fill);
                self.object
                    .write_or_append(Some(range.start), buf.as_ref())
                    .await
                    .expect("write_or_append failed");
                if range.end > self.mirror.len() as u64 {
                    self.mirror.resize(range.end as usize, 0);
                }
                self.mirror[range.start as usize..range.end as usize].fill(self.fill);
                let mut buf = self.object.allocate_buffer(self.mirror.len() + 1).await;
                assert_eq!(
                    self.object.read(0, buf.as_mut()).await.expect("read failed"),
                    self.mirror.len()
                );
                assert_eq!(&buf.as_slice()[..self.mirror.len()], self.mirror.as_slice());
            }
        }

        let block_size = object.block_size() as u64;
        let mut align = AlignTest::new(object).await;

        // Fill the object to start with (with 1).
        align.test(0..2 * block_size + 1).await;

        // Unaligned head (fills with 2, overwrites that with 3).
        align.test(1..block_size).await;
        align.test(1..2 * block_size).await;

        // Unaligned tail (fills with 4 and 5).
        align.test(0..block_size - 1).await;
        align.test(0..2 * block_size - 1).await;

        // Both unaligned (fills with 6 and 7).
        align.test(1..block_size - 1).await;
        align.test(1..2 * block_size - 1).await;

        fs.close().await.expect("Close failed");
    }

    async fn test_preallocate_common(fs: &FxFilesystem, object: DataObjectHandle<ObjectStore>) {
        let allocator = fs.allocator();
        let allocated_before = allocator.get_allocated_bytes();
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .preallocate_range(&mut transaction, &mut (0..fs.block_size() as u64))
            .await
            .expect("preallocate_range failed");
        transaction.commit().await.expect("commit failed");
        assert!(object.get_size() < 1048576);
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .preallocate_range(&mut transaction, &mut (0..1048576))
            .await
            .expect("preallocate_range failed");
        transaction.commit().await.expect("commit failed");
        assert_eq!(object.get_size(), 1048576);
        // Check that it didn't reallocate the space for the existing extent
        let allocated_after = allocator.get_allocated_bytes();
        assert_eq!(allocated_after - allocated_before, 1048576 - fs.block_size() as u64);

        let mut buf = object
            .allocate_buffer(round_up(TEST_DATA_OFFSET, fs.block_size()).unwrap() as usize)
            .await;
        buf.as_mut_slice().fill(47);
        object
            .write_or_append(Some(0), buf.subslice(..TEST_DATA_OFFSET as usize))
            .await
            .expect("write failed");
        buf.as_mut_slice().fill(95);
        let offset = round_up(TEST_OBJECT_SIZE, fs.block_size()).unwrap();
        object.overwrite(offset, buf.as_mut(), false).await.expect("write failed");

        // Make sure there were no more allocations.
        assert_eq!(allocator.get_allocated_bytes(), allocated_after);

        // Read back the data and make sure it is what we expect.
        let mut buf = object.allocate_buffer(104876).await;
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), buf.len());
        assert_eq!(&buf.as_slice()[..TEST_DATA_OFFSET as usize], &[47; TEST_DATA_OFFSET as usize]);
        assert_eq!(
            &buf.as_slice()[TEST_DATA_OFFSET as usize..TEST_DATA_OFFSET as usize + TEST_DATA.len()],
            TEST_DATA
        );
        assert_eq!(&buf.as_slice()[offset as usize..offset as usize + 2048], &[95; 2048]);
    }

    #[fuchsia::test]
    async fn test_preallocate_range() {
        let (fs, object) = test_filesystem_and_object_with_key(None, true).await;
        test_preallocate_common(&fs, object).await;
        fs.close().await.expect("Close failed");
    }

    // This is identical to the previous test except that we flush so that extents end up in
    // different layers.
    #[fuchsia::test]
    async fn test_preallocate_succeeds_when_extents_are_in_different_layers() {
        let (fs, object) = test_filesystem_and_object_with_key(None, true).await;
        object.owner().flush().await.expect("flush failed");
        test_preallocate_common(&fs, object).await;
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_already_preallocated() {
        let (fs, object) = test_filesystem_and_object_with_key(None, true).await;
        let allocator = fs.allocator();
        let allocated_before = allocator.get_allocated_bytes();
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        let offset = TEST_DATA_OFFSET - TEST_DATA_OFFSET % fs.block_size() as u64;
        object
            .preallocate_range(&mut transaction, &mut (offset..offset + fs.block_size() as u64))
            .await
            .expect("preallocate_range failed");
        transaction.commit().await.expect("commit failed");
        // Check that it didn't reallocate any new space.
        assert_eq!(allocator.get_allocated_bytes(), allocated_before);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_overwrite_when_preallocated_at_start_of_file() {
        // The standard test data we put in the test object would cause an extent with checksums
        // to be created, which overwrite() doesn't support. So we create an empty object instead.
        let (fs, object) = test_filesystem_and_empty_object().await;

        let object = ObjectStore::open_object(
            object.owner(),
            object.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open_object failed");

        assert_eq!(fs.block_size(), 4096);

        let mut write_buf = object.allocate_buffer(4096).await;
        write_buf.as_mut_slice().fill(95);

        // First try to overwrite without allowing allocations
        // We expect this to fail, since nothing is allocated yet
        object.overwrite(0, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");

        // Now preallocate some space (exactly one block)
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .preallocate_range(&mut transaction, &mut (0..4096 as u64))
            .await
            .expect("preallocate_range failed");
        transaction.commit().await.expect("commit failed");

        // Now try the same overwrite command as before, it should work this time,
        // even with allocations disabled...
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(0, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[0; 4096]);
        }
        object.overwrite(0, write_buf.as_mut(), false).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(0, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        // Now try to overwrite at offset 4096. We expect this to fail, since we only preallocated
        // one block earlier at offset 0
        object.overwrite(4096, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");

        // We can't assert anything about the existing bytes, because they haven't been allocated
        // yet and they could contain any values
        object.overwrite(4096, write_buf.as_mut(), true).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(4096, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        // Check that the overwrites haven't messed up the filesystem state
        let fsck_options = FsckOptions {
            fail_on_warning: true,
            no_lock: true,
            on_error: Box::new(|err| println!("fsck error: {:?}", err)),
            ..Default::default()
        };
        fsck_with_options(fs.clone(), &fsck_options).await.expect("fsck failed");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_overwrite_large_buffer_and_file_with_many_holes() {
        // The standard test data we put in the test object would cause an extent with checksums
        // to be created, which overwrite() doesn't support. So we create an empty object instead.
        let (fs, object) = test_filesystem_and_empty_object().await;

        let object = ObjectStore::open_object(
            object.owner(),
            object.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open_object failed");

        assert_eq!(fs.block_size(), 4096);
        assert_eq!(object.get_size(), TEST_OBJECT_SIZE);

        // Let's create some non-holes
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .preallocate_range(&mut transaction, &mut (4096..8192 as u64))
            .await
            .expect("preallocate_range failed");
        object
            .preallocate_range(&mut transaction, &mut (16384..32768 as u64))
            .await
            .expect("preallocate_range failed");
        object
            .preallocate_range(&mut transaction, &mut (65536..131072 as u64))
            .await
            .expect("preallocate_range failed");
        object
            .preallocate_range(&mut transaction, &mut (262144..524288 as u64))
            .await
            .expect("preallocate_range failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(object.get_size(), 524288);

        let mut write_buf = object.allocate_buffer(4096).await;
        write_buf.as_mut_slice().fill(95);

        // We shouldn't be able to overwrite in the holes if new allocations aren't enabled
        object.overwrite(0, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");
        object.overwrite(8192, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");
        object.overwrite(32768, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");
        object.overwrite(131072, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");

        // But we should be able to overwrite in the prealloc'd areas without needing allocations
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(4096, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[0; 4096]);
        }
        object.overwrite(4096, write_buf.as_mut(), false).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(4096, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(16384, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[0; 4096]);
        }
        object.overwrite(16384, write_buf.as_mut(), false).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(16384, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(65536, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[0; 4096]);
        }
        object.overwrite(65536, write_buf.as_mut(), false).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(65536, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(262144, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[0; 4096]);
        }
        object.overwrite(262144, write_buf.as_mut(), false).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(262144, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        // Now let's try to do a huge overwrite, that spans over many holes and non-holes
        let mut huge_write_buf = object.allocate_buffer(524288).await;
        huge_write_buf.as_mut_slice().fill(96);

        // With allocations disabled, the big overwrite should fail...
        object.overwrite(0, huge_write_buf.as_mut(), false).await.expect_err("overwrite succeeded");
        // ... but it should work when allocations are enabled
        object.overwrite(0, huge_write_buf.as_mut(), true).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(524288).await;
            object.read(0, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[96; 524288]);
        }

        // Check that the overwrites haven't messed up the filesystem state
        let fsck_options = FsckOptions {
            fail_on_warning: true,
            no_lock: true,
            on_error: Box::new(|err| println!("fsck error: {:?}", err)),
            ..Default::default()
        };
        fsck_with_options(fs.clone(), &fsck_options).await.expect("fsck failed");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_overwrite_when_unallocated_at_start_of_file() {
        // The standard test data we put in the test object would cause an extent with checksums
        // to be created, which overwrite() doesn't support. So we create an empty object instead.
        let (fs, object) = test_filesystem_and_empty_object().await;

        let object = ObjectStore::open_object(
            object.owner(),
            object.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open_object failed");

        assert_eq!(fs.block_size(), 4096);

        let mut write_buf = object.allocate_buffer(4096).await;
        write_buf.as_mut_slice().fill(95);

        // First try to overwrite without allowing allocations
        // We expect this to fail, since nothing is allocated yet
        object.overwrite(0, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");

        // Now try the same overwrite command as before, but allow allocations
        object.overwrite(0, write_buf.as_mut(), true).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(0, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        // Now try to overwrite at the next block. This should fail if allocations are disabled
        object.overwrite(4096, write_buf.as_mut(), false).await.expect_err("overwrite succeeded");

        // ... but it should work if allocations are enabled
        object.overwrite(4096, write_buf.as_mut(), true).await.expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(4096, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        // Check that the overwrites haven't messed up the filesystem state
        let fsck_options = FsckOptions {
            fail_on_warning: true,
            no_lock: true,
            on_error: Box::new(|err| println!("fsck error: {:?}", err)),
            ..Default::default()
        };
        fsck_with_options(fs.clone(), &fsck_options).await.expect("fsck failed");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_overwrite_can_extend_a_file() {
        // The standard test data we put in the test object would cause an extent with checksums
        // to be created, which overwrite() doesn't support. So we create an empty object instead.
        let (fs, object) = test_filesystem_and_empty_object().await;

        let object = ObjectStore::open_object(
            object.owner(),
            object.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open_object failed");

        assert_eq!(fs.block_size(), 4096);
        assert_eq!(object.get_size(), TEST_OBJECT_SIZE);

        let mut write_buf = object.allocate_buffer(4096).await;
        write_buf.as_mut_slice().fill(95);

        // Let's try to fill up the last block, and increase the file size in doing so
        let last_block_offset = round_down(TEST_OBJECT_SIZE, 4096 as u32);

        // Expected to fail with allocations disabled
        object
            .overwrite(last_block_offset, write_buf.as_mut(), false)
            .await
            .expect_err("overwrite succeeded");
        // ... but expected to succeed with allocations enabled
        object
            .overwrite(last_block_offset, write_buf.as_mut(), true)
            .await
            .expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(last_block_offset, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        assert_eq!(object.get_size(), 8192);

        // Let's try to write at the next block, too
        let next_block_offset = round_up(TEST_OBJECT_SIZE, 4096 as u32).unwrap();

        // Expected to fail with allocations disabled
        object
            .overwrite(next_block_offset, write_buf.as_mut(), false)
            .await
            .expect_err("overwrite succeeded");
        // ... but expected to succeed with allocations enabled
        object
            .overwrite(next_block_offset, write_buf.as_mut(), true)
            .await
            .expect("overwrite failed");
        {
            let mut read_buf = object.allocate_buffer(4096).await;
            object.read(next_block_offset, read_buf.as_mut()).await.expect("read failed");
            assert_eq!(&read_buf.as_slice(), &[95; 4096]);
        }

        assert_eq!(object.get_size(), 12288);

        // Check that the overwrites haven't messed up the filesystem state
        let fsck_options = FsckOptions {
            fail_on_warning: true,
            no_lock: true,
            on_error: Box::new(|err| println!("fsck error: {:?}", err)),
            ..Default::default()
        };
        fsck_with_options(fs.clone(), &fsck_options).await.expect("fsck failed");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_enable_verity() {
        let fs: OpenFxFilesystem = test_filesystem().await;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );

        transaction.commit().await.unwrap();

        object
            .enable_verity(fio::VerificationOptions {
                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                salt: Some(vec![]),
                ..Default::default()
            })
            .await
            .expect("set verified file metadata failed");

        let handle =
            ObjectStore::open_object(&store, object.object_id(), HandleOptions::default(), None)
                .await
                .expect("open_object failed");

        assert!(handle.verified_file());

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_verify_data_corrupt_file() {
        let fs: OpenFxFilesystem = test_filesystem().await;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );

        transaction.commit().await.unwrap();

        let mut buf = object.allocate_buffer(5 * fs.block_size() as usize).await;
        buf.as_mut_slice().fill(123);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        object
            .enable_verity(fio::VerificationOptions {
                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                salt: Some(vec![]),
                ..Default::default()
            })
            .await
            .expect("set verified file metadata failed");

        // Change file contents and ensure verification fails
        buf.as_mut_slice().fill(234);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        object.read(0, buf.as_mut()).await.expect_err("verification during read should fail");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_extend() {
        let fs = test_filesystem().await;
        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        handle = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        handle
            .extend(&mut transaction, 0..5 * fs.block_size() as u64)
            .await
            .expect("extend failed");
        transaction.commit().await.expect("commit failed");
        let mut buf = handle.allocate_buffer(5 * fs.block_size() as usize).await;
        buf.as_mut_slice().fill(123);
        handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        buf.as_mut_slice().fill(67);
        handle.read(0, buf.as_mut()).await.expect("read failed");
        assert_eq!(buf.as_slice(), &vec![123; 5 * fs.block_size() as usize]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_truncate_deallocates_old_extents() {
        let (fs, object) = test_filesystem_and_object().await;
        let mut buf = object.allocate_buffer(5 * fs.block_size() as usize).await;
        buf.as_mut_slice().fill(0xaa);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        let allocator = fs.allocator();
        let allocated_before = allocator.get_allocated_bytes();
        object.truncate(fs.block_size() as u64).await.expect("truncate failed");
        let allocated_after = allocator.get_allocated_bytes();
        assert!(
            allocated_after < allocated_before,
            "before = {} after = {}",
            allocated_before,
            allocated_after
        );
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_truncate_zeroes_tail_block() {
        let (fs, object) = test_filesystem_and_object().await;

        WriteObjectHandle::truncate(&object, TEST_DATA_OFFSET + 3).await.expect("truncate failed");
        WriteObjectHandle::truncate(&object, TEST_DATA_OFFSET + TEST_DATA.len() as u64)
            .await
            .expect("truncate failed");

        let mut buf = object.allocate_buffer(fs.block_size() as usize).await;
        let offset = (TEST_DATA_OFFSET % fs.block_size()) as usize;
        object.read(TEST_DATA_OFFSET - offset as u64, buf.as_mut()).await.expect("read failed");

        let mut expected = TEST_DATA.to_vec();
        expected[3..].fill(0);
        assert_eq!(&buf.as_slice()[offset..offset + expected.len()], &expected);
    }

    #[fuchsia::test]
    async fn test_trim() {
        // Format a new filesystem.
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let block_size = fs.block_size();
        root_volume(fs.clone())
            .await
            .expect("root_volume failed")
            .new_volume("test", None)
            .await
            .expect("volume failed");
        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        // To test trim, we open the filesystem and set up a post commit hook that runs after every
        // transaction.  When the hook triggers, we can fsck the volume, take a snapshot of the
        // device and check that it gets replayed correctly on the snapshot.  We can check that the
        // graveyard trims the file as expected.
        #[derive(Default)]
        struct Context {
            store: Option<Arc<ObjectStore>>,
            object_id: Option<u64>,
        }
        let shared_context = Arc::new(Mutex::new(Context::default()));

        let object_size = (TRANSACTION_MUTATION_THRESHOLD as u64 + 10) * 2 * block_size;

        // Wait for an object to get tombstoned by the graveyard.
        async fn expect_tombstoned(store: &Arc<ObjectStore>, object_id: u64) {
            loop {
                if let Err(e) =
                    ObjectStore::open_object(store, object_id, HandleOptions::default(), None).await
                {
                    assert!(FxfsError::NotFound.matches(&e));
                    break;
                }
                // The graveyard should eventually tombstone the object.
                fasync::Timer::new(std::time::Duration::from_millis(100)).await;
            }
        }

        // Checks to see if the object needs to be trimmed.
        async fn needs_trim(store: &Arc<ObjectStore>) -> Option<DataObjectHandle<ObjectStore>> {
            let root_directory = Directory::open(store, store.root_directory_object_id())
                .await
                .expect("open failed");
            let oid = root_directory.lookup("foo").await.expect("lookup failed");
            if let Some((oid, _)) = oid {
                let object = ObjectStore::open_object(store, oid, HandleOptions::default(), None)
                    .await
                    .expect("open_object failed");
                let props = object.get_properties().await.expect("get_properties failed");
                if props.allocated_size > 0 && props.data_attribute_size == 0 {
                    Some(object)
                } else {
                    None
                }
            } else {
                None
            }
        }

        let shared_context_clone = shared_context.clone();
        let post_commit = move || {
            let store = shared_context_clone.lock().unwrap().store.as_ref().cloned().unwrap();
            let shared_context = shared_context_clone.clone();
            async move {
                // First run fsck on the current filesystem.
                let options = FsckOptions {
                    fail_on_warning: true,
                    no_lock: true,
                    on_error: Box::new(|err| println!("fsck error: {:?}", err)),
                    ..Default::default()
                };
                let fs = store.filesystem();

                fsck_with_options(fs.clone(), &options).await.expect("fsck_with_options failed");
                fsck_volume_with_options(fs.as_ref(), &options, store.store_object_id(), None)
                    .await
                    .expect("fsck_volume_with_options failed");

                // Now check that we can replay this correctly.
                fs.sync(SyncOptions { flush_device: true, ..Default::default() })
                    .await
                    .expect("sync failed");
                let device = fs.device().snapshot().expect("snapshot failed");

                let object_id = shared_context.lock().unwrap().object_id.clone();

                let fs2 = FxFilesystemBuilder::new()
                    .skip_initial_reap(object_id.is_none())
                    .open(device)
                    .await
                    .expect("open failed");

                // If the "foo" file exists check that allocated size matches content size.
                let root_vol = root_volume(fs2.clone()).await.expect("root_volume failed");
                let store = root_vol.volume("test", None).await.expect("volume failed");

                if let Some(oid) = object_id {
                    // For the second pass, the object should get tombstoned.
                    expect_tombstoned(&store, oid).await;
                } else if let Some(object) = needs_trim(&store).await {
                    // Extend the file and make sure that it is correctly trimmed.
                    object.truncate(object_size).await.expect("truncate failed");
                    let mut buf = object.allocate_buffer(block_size as usize).await;
                    object
                        .read(object_size - block_size * 2, buf.as_mut())
                        .await
                        .expect("read failed");
                    assert_eq!(buf.as_slice(), &vec![0; block_size as usize]);

                    // Remount, this time with the graveyard performing an initial reap and the
                    // object should get trimmed.
                    let fs = FxFilesystem::open(fs.device().snapshot().expect("snapshot failed"))
                        .await
                        .expect("open failed");
                    let root_vol = root_volume(fs.clone()).await.expect("root_volume failed");
                    let store = root_vol.volume("test", None).await.expect("volume failed");
                    while needs_trim(&store).await.is_some() {
                        // The object has been truncated, but still has some data allocated to
                        // it.  The graveyard should trim the object eventually.
                        fasync::Timer::new(std::time::Duration::from_millis(100)).await;
                    }

                    // Run fsck.
                    fsck_with_options(fs.clone(), &options)
                        .await
                        .expect("fsck_with_options failed");
                    fsck_volume_with_options(fs.as_ref(), &options, store.store_object_id(), None)
                        .await
                        .expect("fsck_volume_with_options failed");
                    fs.close().await.expect("close failed");
                }

                // Run fsck on fs2.
                fsck_with_options(fs2.clone(), &options).await.expect("fsck_with_options failed");
                fsck_volume_with_options(fs2.as_ref(), &options, store.store_object_id(), None)
                    .await
                    .expect("fsck_volume_with_options failed");
                fs2.close().await.expect("close failed");
            }
            .boxed()
        };

        let fs = FxFilesystemBuilder::new()
            .post_commit_hook(post_commit)
            .open(device)
            .await
            .expect("open failed");

        let root_vol = root_volume(fs.clone()).await.expect("root_volume failed");
        let store = root_vol.volume("test", None).await.expect("volume failed");

        shared_context.lock().unwrap().store = Some(store.clone());

        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let object;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    store.store_object_id(),
                    store.root_directory_object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        object = root_directory
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_object failed");
        transaction.commit().await.expect("commit failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), object.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        // Two passes: first with a regular object, and then with that object moved into the
        // graveyard.
        let mut pass = 0;
        loop {
            // Create enough extents in it such that when we truncate the object it will require
            // more than one transaction.
            let mut buf = object.allocate_buffer(5).await;
            buf.as_mut_slice().fill(1);
            // Write every other block.
            for offset in (0..object_size).into_iter().step_by(2 * block_size as usize) {
                object
                    .txn_write(&mut transaction, offset, buf.as_ref())
                    .await
                    .expect("write failed");
            }
            transaction.commit().await.expect("commit failed");

            // This should take up more than one transaction.
            WriteObjectHandle::truncate(&object, 0).await.expect("truncate failed");

            if pass == 1 {
                break;
            }

            // Store the object ID so that we can make sure the object is always tombstoned
            // after remount (see above).
            shared_context.lock().unwrap().object_id = Some(object.object_id());

            transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![
                        LockKey::object(store.store_object_id(), store.root_directory_object_id()),
                        LockKey::object(store.store_object_id(), object.object_id()),
                    ],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");

            // Move the object into the graveyard.
            replace_child(&mut transaction, None, (&root_directory, "foo"))
                .await
                .expect("replace_child failed");
            store.add_to_graveyard(&mut transaction, object.object_id());

            pass += 1;
        }

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_adjust_refs() {
        let (fs, object) = test_filesystem_and_object().await;
        let store = object.owner();
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), object.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            store
                .adjust_refs(&mut transaction, object.object_id(), 1)
                .await
                .expect("adjust_refs failed"),
            false
        );
        transaction.commit().await.expect("commit failed");

        let allocator = fs.allocator();
        let allocated_before = allocator.get_allocated_bytes();
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), object.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            store
                .adjust_refs(&mut transaction, object.object_id(), -2)
                .await
                .expect("adjust_refs failed"),
            true
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(allocator.get_allocated_bytes(), allocated_before);

        store
            .tombstone(
                object.object_id(),
                Options { borrow_metadata_space: true, ..Default::default() },
            )
            .await
            .expect("purge failed");

        assert_eq!(allocated_before - allocator.get_allocated_bytes(), fs.block_size() as u64);

        // We need to remove the directory entry, too, otherwise fsck will complain
        {
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        store.store_object_id(),
                        store.root_directory_object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");
            transaction.add(
                store.store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::child(root_directory.object_id(), TEST_OBJECT_NAME),
                    ObjectValue::None,
                ),
            );
            transaction.commit().await.expect("commit failed");
        }

        fsck_with_options(
            fs.clone(),
            &FsckOptions {
                fail_on_warning: true,
                on_error: Box::new(|err| println!("fsck error: {:?}", err)),
                ..Default::default()
            },
        )
        .await
        .expect("fsck_with_options failed");

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_locks() {
        let (fs, object) = test_filesystem_and_object().await;
        let (send1, recv1) = channel();
        let (send2, recv2) = channel();
        let (send3, recv3) = channel();
        let done = Mutex::new(false);
        let mut futures = FuturesUnordered::new();
        futures.push(
            async {
                let mut t = object.new_transaction().await.expect("new_transaction failed");
                send1.send(()).unwrap(); // Tell the next future to continue.
                send3.send(()).unwrap(); // Tell the last future to continue.
                recv2.await.unwrap();
                let mut buf = object.allocate_buffer(5).await;
                buf.as_mut_slice().copy_from_slice(b"hello");
                object.txn_write(&mut t, 0, buf.as_ref()).await.expect("write failed");
                // This is a halting problem so all we can do is sleep.
                fasync::Timer::new(Duration::from_millis(100)).await;
                assert!(!*done.lock().unwrap());
                t.commit().await.expect("commit failed");
            }
            .boxed(),
        );
        futures.push(
            async {
                recv1.await.unwrap();
                // Reads should not block.
                let offset = TEST_DATA_OFFSET as usize;
                let align = offset % fs.block_size() as usize;
                let len = TEST_DATA.len();
                let mut buf = object.allocate_buffer(align + len).await;
                assert_eq!(
                    object.read((offset - align) as u64, buf.as_mut()).await.expect("read failed"),
                    align + TEST_DATA.len()
                );
                assert_eq!(&buf.as_slice()[align..], TEST_DATA);
                // Tell the first future to continue.
                send2.send(()).unwrap();
            }
            .boxed(),
        );
        futures.push(
            async {
                // This should block until the first future has completed.
                recv3.await.unwrap();
                let _t = object.new_transaction().await.expect("new_transaction failed");
                let mut buf = object.allocate_buffer(5).await;
                assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), 5);
                assert_eq!(buf.as_slice(), b"hello");
            }
            .boxed(),
        );
        while let Some(()) = futures.next().await {}
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_racy_reads() {
        let fs = test_filesystem().await;
        let object;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );
        transaction.commit().await.expect("commit failed");
        for _ in 0..100 {
            let cloned_object = object.clone();
            let writer = fasync::Task::spawn(async move {
                let mut buf = cloned_object.allocate_buffer(10).await;
                buf.as_mut_slice().fill(123);
                cloned_object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            });
            let cloned_object = object.clone();
            let reader = fasync::Task::spawn(async move {
                let wait_time = rand::thread_rng().gen_range(0..5);
                fasync::Timer::new(Duration::from_millis(wait_time)).await;
                let mut buf = cloned_object.allocate_buffer(10).await;
                buf.as_mut_slice().fill(23);
                let amount = cloned_object.read(0, buf.as_mut()).await.expect("write failed");
                // If we succeed in reading data, it must include the write; i.e. if we see the size
                // change, we should see the data too.  For this to succeed it requires locking on
                // the read size to ensure that when we read the size, we get the extents changed in
                // that same transaction.
                if amount != 0 {
                    assert_eq!(amount, 10);
                    assert_eq!(buf.as_slice(), &[123; 10]);
                }
            });
            writer.await;
            reader.await;
            object.truncate(0).await.expect("truncate failed");
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_allocated_size() {
        let (fs, object) = test_filesystem_and_object_with_key(None, true).await;

        let before = object.get_properties().await.expect("get_properties failed").allocated_size;
        let mut buf = object.allocate_buffer(5).await;
        buf.as_mut_slice().copy_from_slice(b"hello");
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        let after = object.get_properties().await.expect("get_properties failed").allocated_size;
        assert_eq!(after, before + fs.block_size() as u64);

        // Do the same write again and there should be no change.
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        assert_eq!(
            object.get_properties().await.expect("get_properties failed").allocated_size,
            after
        );

        // extend...
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        let offset = 1000 * fs.block_size() as u64;
        let before = after;
        object
            .extend(&mut transaction, offset..offset + fs.block_size() as u64)
            .await
            .expect("extend failed");
        transaction.commit().await.expect("commit failed");
        let after = object.get_properties().await.expect("get_properties failed").allocated_size;
        assert_eq!(after, before + fs.block_size() as u64);

        // truncate...
        let before = after;
        let size = object.get_size();
        object.truncate(size - fs.block_size() as u64).await.expect("extend failed");
        let after = object.get_properties().await.expect("get_properties failed").allocated_size;
        assert_eq!(after, before - fs.block_size() as u64);

        // preallocate_range...
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        let before = after;
        let mut file_range = offset..offset + fs.block_size() as u64;
        object.preallocate_range(&mut transaction, &mut file_range).await.expect("extend failed");
        transaction.commit().await.expect("commit failed");
        let after = object.get_properties().await.expect("get_properties failed").allocated_size;
        assert_eq!(after, before + fs.block_size() as u64);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_zero() {
        let (fs, object) = test_filesystem_and_object().await;
        let expected_size = object.get_size();
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object.zero(&mut transaction, 0..fs.block_size() as u64 * 10).await.expect("zero failed");
        transaction.commit().await.expect("commit failed");
        assert_eq!(object.get_size(), expected_size);
        let mut buf = object.allocate_buffer(fs.block_size() as usize * 10).await;
        assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed") as u64, expected_size);
        assert_eq!(
            &buf.as_slice()[0..expected_size as usize],
            vec![0u8; expected_size as usize].as_slice()
        );
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_properties() {
        let (fs, object) = test_filesystem_and_object().await;
        const CRTIME: Timestamp = Timestamp::from_nanos(1234);
        const MTIME: Timestamp = Timestamp::from_nanos(5678);
        const CTIME: Timestamp = Timestamp::from_nanos(8765);

        // ObjectProperties can be updated through `update_attributes`.
        // `get_properties` should reflect the latest changes.
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .update_attributes(
                &mut transaction,
                Some(&fio::MutableNodeAttributes {
                    creation_time: Some(CRTIME.as_nanos()),
                    modification_time: Some(MTIME.as_nanos()),
                    mode: Some(111),
                    gid: Some(222),
                    ..Default::default()
                }),
                None,
            )
            .await
            .expect("update_attributes failed");
        const MTIME_NEW: Timestamp = Timestamp::from_nanos(12345678);
        object
            .update_attributes(
                &mut transaction,
                Some(&fio::MutableNodeAttributes {
                    modification_time: Some(MTIME_NEW.as_nanos()),
                    gid: Some(333),
                    rdev: Some(444),
                    ..Default::default()
                }),
                Some(CTIME),
            )
            .await
            .expect("update_timestamps failed");
        transaction.commit().await.expect("commit failed");

        let properties = object.get_properties().await.expect("get_properties failed");
        assert_matches!(
            properties,
            ObjectProperties {
                refs: 1u64,
                allocated_size: TEST_OBJECT_ALLOCATED_SIZE,
                data_attribute_size: TEST_OBJECT_SIZE,
                creation_time: CRTIME,
                modification_time: MTIME_NEW,
                posix_attributes: Some(PosixAttributes { mode: 111, gid: 333, rdev: 444, .. }),
                change_time: CTIME,
                ..
            }
        );
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_is_allocated() {
        let (fs, object) = test_filesystem_and_object().await;

        // `test_filesystem_and_object()` wrote the buffer `TEST_DATA` to the device at offset
        // `TEST_DATA_OFFSET` where the length and offset are aligned to the block size.
        let aligned_offset = round_down(TEST_DATA_OFFSET, fs.block_size());
        let aligned_length = round_up(TEST_DATA.len() as u64, fs.block_size()).unwrap();

        // Check for the case where where we have the following extent layout
        //       [ unallocated ][ `TEST_DATA` ]
        // The extents before `aligned_offset` should not be allocated
        let (allocated, count) = object.is_allocated(0).await.expect("is_allocated failed");
        assert_eq!(count, aligned_offset);
        assert_eq!(allocated, false);

        let (allocated, count) =
            object.is_allocated(aligned_offset).await.expect("is_allocated failed");
        assert_eq!(count, aligned_length);
        assert_eq!(allocated, true);

        // Check for the case where where we query out of range
        let end = aligned_offset + aligned_length;
        object
            .is_allocated(end)
            .await
            .expect_err("is_allocated should have returned ERR_OUT_OF_RANGE");

        // Check for the case where where we start querying for allocation starting from
        // an allocated range to the end of the device
        let size = 50 * fs.block_size() as u64;
        object.truncate(size).await.expect("extend failed");

        let (allocated, count) = object.is_allocated(end).await.expect("is_allocated failed");
        assert_eq!(count, size - end);
        assert_eq!(allocated, false);

        // Check for the case where where we have the following extent layout
        //      [ unallocated ][ `buf` ][ `buf` ]
        let buf_length = 5 * fs.block_size();
        let mut buf = object.allocate_buffer(buf_length as usize).await;
        buf.as_mut_slice().fill(123);
        let new_offset = end + 20 * fs.block_size() as u64;
        object.write_or_append(Some(new_offset), buf.as_ref()).await.expect("write failed");
        object
            .write_or_append(Some(new_offset + buf_length), buf.as_ref())
            .await
            .expect("write failed");

        let (allocated, count) = object.is_allocated(end).await.expect("is_allocated failed");
        assert_eq!(count, new_offset - end);
        assert_eq!(allocated, false);

        let (allocated, count) =
            object.is_allocated(new_offset).await.expect("is_allocated failed");
        assert_eq!(count, 2 * buf_length);
        assert_eq!(allocated, true);

        // Check the case where we query from the middle of an extent
        let (allocated, count) = object
            .is_allocated(new_offset + 4 * fs.block_size())
            .await
            .expect("is_allocated failed");
        assert_eq!(count, 2 * buf_length - 4 * fs.block_size());
        assert_eq!(allocated, true);

        // Now, write buffer to a location already written to.
        // Check for the case when we the following extent layout
        //      [ unallocated ][ `other_buf` ][ (part of) `buf` ][ `buf` ]
        let other_buf_length = 3 * fs.block_size();
        let mut other_buf = object.allocate_buffer(other_buf_length as usize).await;
        other_buf.as_mut_slice().fill(231);
        object.write_or_append(Some(new_offset), other_buf.as_ref()).await.expect("write failed");

        // We still expect that `is_allocated(..)` will return that  there are 2*`buf_length bytes`
        // allocated from `new_offset`
        let (allocated, count) =
            object.is_allocated(new_offset).await.expect("is_allocated failed");
        assert_eq!(count, 2 * buf_length);
        assert_eq!(allocated, true);

        // Check for the case when we the following extent layout
        //   [ unallocated ][ deleted ][ unallocated ][ deleted ][ allocated ]
        // Mark TEST_DATA as deleted
        let mut transaction = object.new_transaction().await.expect("new_transaction failed");
        object
            .zero(&mut transaction, aligned_offset..aligned_offset + aligned_length)
            .await
            .expect("zero failed");
        // Mark `other_buf` as deleted
        object
            .zero(&mut transaction, new_offset..new_offset + buf_length)
            .await
            .expect("zero failed");
        transaction.commit().await.expect("commit transaction failed");

        let (allocated, count) = object.is_allocated(0).await.expect("is_allocated failed");
        assert_eq!(count, new_offset + buf_length);
        assert_eq!(allocated, false);

        let (allocated, count) =
            object.is_allocated(new_offset + buf_length).await.expect("is_allocated failed");
        assert_eq!(count, buf_length);
        assert_eq!(allocated, true);

        let new_end = new_offset + buf_length + count;

        // Check for the case where there are objects with different keys.
        // Case that we're checking for:
        //      [ unallocated ][ extent (object with different key) ][ unallocated ]
        let store = object.owner();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let object2 = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");

        object2
            .write_or_append(Some(new_end + fs.block_size()), buf.as_ref())
            .await
            .expect("write failed");

        // Expecting that the extent with a different key is treated like unallocated extent
        let (allocated, count) = object.is_allocated(new_end).await.expect("is_allocated failed");
        assert_eq!(count, size - new_end);
        assert_eq!(allocated, false);

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_read_write_attr() {
        let (_fs, object) = test_filesystem_and_object().await;
        let data = [0xffu8; 16_384];
        object.write_attr(20, &data).await.expect("write_attr failed");
        let rdata =
            object.read_attr(20).await.expect("read_attr failed").expect("no attribute data found");
        assert_eq!(&data[..], &rdata[..]);

        assert_eq!(object.read_attr(21).await.expect("read_attr failed"), None);
    }
}
