// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        checksum::fletcher64,
        errors::FxfsError,
        log::*,
        lsm_tree::types::{Item, ItemRef, LayerIterator},
        object_handle::ObjectHandle,
        object_store::{
            allocator::Allocator,
            extent_record::{Checksums, ExtentKey, ExtentValue},
            object_manager::ObjectManager,
            object_record::{
                AttributeKey, EncryptionKeys, ExtendedAttributeValue, ObjectAttributes, ObjectItem,
                ObjectKey, ObjectKeyData, ObjectValue, Timestamp,
            },
            transaction::{
                lock_keys, AssocObj, AssociatedObject, LockKey, Mutation, ObjectStoreMutation,
                Options, ReadGuard, Transaction, TransactionHandler,
            },
            HandleOptions, HandleOwner, ObjectStore, TrimMode, TrimResult,
        },
        range::RangeExt,
        round::{round_down, round_up},
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio,
    futures::{
        stream::{FuturesOrdered, FuturesUnordered},
        try_join, TryStreamExt,
    },
    fxfs_crypto::{KeyPurpose, WrappedKeys, XtsCipherSet},
    std::{
        cmp::min,
        future::Future,
        ops::{Bound, Range},
        sync::{
            atomic::{self, AtomicBool},
            Arc,
        },
    },
    storage_device::buffer::{Buffer, BufferRef, MutableBufferRef},
};

/// Maximum size for an extended attribute name.
pub const MAX_XATTR_NAME_SIZE: usize = 255;
/// Maximum size an extended attribute can be before it's stored in an object attribute instead of
/// inside the record directly.
pub const MAX_INLINE_XATTR_SIZE: usize = 256;
/// Maximum size for an extended attribute value. NB: the maximum size for an extended attribute is
/// 64kB, which we rely on for correctness when deleting attributes, so ensure it's always
/// enforced.
pub const MAX_XATTR_VALUE_SIZE: usize = 64000;
/// The range of fxfs attribute ids which are reserved for extended attribute values. Whenever a
/// new attribute is needed, the first unused id will be chosen from this range. It's technically
/// safe to change these values, but it has potential consequences - they are only used during id
/// selection, so any existing extended attributes keep their ids, which means any past or present
/// selected range here could potentially have used attributes unless they are explicitly migrated,
/// which isn't currently done.
pub const EXTENDED_ATTRIBUTE_RANGE_START: u64 = 64;
pub const EXTENDED_ATTRIBUTE_RANGE_END: u64 = 512;

/// The mode of operation when setting extended attributes. This is the same as the fidl definition
/// but is replicated here so we don't have fuchsia.io structures in the api, so this can be used
/// on host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetExtendedAttributeMode {
    /// Create the extended attribute if it doesn't exist, replace the value if it does.
    Set,
    /// Create the extended attribute if it doesn't exist, fail if it does.
    Create,
    /// Replace the extended attribute value if it exists, fail if it doesn't.
    Replace,
}

impl From<fio::SetExtendedAttributeMode> for SetExtendedAttributeMode {
    fn from(other: fio::SetExtendedAttributeMode) -> SetExtendedAttributeMode {
        match other {
            fio::SetExtendedAttributeMode::Set => SetExtendedAttributeMode::Set,
            fio::SetExtendedAttributeMode::Create => SetExtendedAttributeMode::Create,
            fio::SetExtendedAttributeMode::Replace => SetExtendedAttributeMode::Replace,
        }
    }
}

enum Encryption {
    /// The object doesn't use encryption.
    None,

    /// The object has keys that are cached (which means unwrapping occurs on-demand) with
    /// KeyManager.
    CachedKeys,

    /// The object has permanent keys registered with KeyManager.
    PermanentKeys,
}

/// StoreObjectHandle is the lowest-level, untyped handle to an object with the id [`object_id`] in
/// a particular store, [`owner`]. It provides functionality shared across all objects, such as
/// reading and writing attributes and managing encryption keys.
///
/// Since it's untyped, it doesn't do any object kind validation, and is generally meant to
/// implement higher-level typed handles.
///
/// For file-like objects with a data attribute, DataObjectHandle implements traits and helpers for
/// doing more complex extent management and caches the content size.
///
/// For directory-like objects, Directory knows how to add and remove child objects and enumerate
/// its children.
pub struct StoreObjectHandle<S: HandleOwner> {
    owner: Arc<S>,
    object_id: u64,
    options: HandleOptions,
    trace: AtomicBool,
    encryption: Encryption,
}

impl<S: HandleOwner> ObjectHandle for StoreObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        info!(store_id = self.store().store_object_id, oid = self.object_id(), trace = v, "trace");
        self.trace.store(v, atomic::Ordering::Relaxed);
    }

    fn object_id(&self) -> u64 {
        return self.object_id;
    }

    fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.store().device.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.store().block_size()
    }
}

impl<S: HandleOwner> StoreObjectHandle<S> {
    /// Make a new StoreObjectHandle for the object with id [`object_id`] in store [`owner`].
    pub fn new(
        owner: Arc<S>,
        object_id: u64,
        permanent_keys: bool,
        options: HandleOptions,
        trace: bool,
    ) -> Self {
        let encryption = if permanent_keys {
            Encryption::PermanentKeys
        } else if owner.as_ref().as_ref().is_encrypted() {
            Encryption::CachedKeys
        } else {
            Encryption::None
        };
        Self { owner, object_id, encryption, options, trace: AtomicBool::new(trace) }
    }

    pub fn owner(&self) -> &Arc<S> {
        &self.owner
    }

    pub fn store(&self) -> &ObjectStore {
        self.owner.as_ref().as_ref()
    }

    pub fn trace(&self) -> bool {
        self.trace.load(atomic::Ordering::Relaxed)
    }

    pub fn is_encrypted(&self) -> bool {
        !matches!(self.encryption, Encryption::None)
    }

    /// Get the default set of transaction options for this object. This is mostly the overall
    /// default, modified by any [`HandleOptions`] held by this handle.
    pub fn default_transaction_options<'b>(&self) -> Options<'b> {
        Options { skip_journal_checks: self.options.skip_journal_checks, ..Default::default() }
    }

    pub async fn new_transaction_with_options<'b>(
        &self,
        attribute_id: u64,
        options: Options<'b>,
    ) -> Result<Transaction<'b>, Error> {
        Ok(self
            .store()
            .filesystem()
            .new_transaction(
                lock_keys![
                    LockKey::object_attribute(
                        self.store().store_object_id(),
                        self.object_id(),
                        attribute_id,
                    ),
                    LockKey::object(self.store().store_object_id(), self.object_id()),
                ],
                options,
            )
            .await?)
    }

    pub async fn new_transaction<'b>(&self, attribute_id: u64) -> Result<Transaction<'b>, Error> {
        self.new_transaction_with_options(attribute_id, self.default_transaction_options()).await
    }

    // If |transaction| has an impending mutation for the underlying object, returns that.
    // Otherwise, looks up the object from the tree.
    async fn txn_get_object_mutation(
        &self,
        transaction: &Transaction<'_>,
    ) -> Result<ObjectStoreMutation, Error> {
        self.store().txn_get_object_mutation(transaction, self.object_id()).await
    }

    // Returns the amount deallocated.
    async fn deallocate_old_extents(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        range: Range<u64>,
    ) -> Result<u64, Error> {
        let block_size = self.block_size();
        assert_eq!(range.start % block_size, 0);
        assert_eq!(range.end % block_size, 0);
        if range.start == range.end {
            return Ok(0);
        }
        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let key = ExtentKey { range };
        let lower_bound = ObjectKey::attribute(
            self.object_id(),
            attribute_id,
            AttributeKey::Extent(key.search_key()),
        );
        let mut merger = layer_set.merger();
        let mut iter = merger.seek(Bound::Included(&lower_bound)).await?;
        let allocator = self.store().allocator();
        let mut deallocated = 0;
        let trace = self.trace();
        while let Some(ItemRef {
            key:
                ObjectKey {
                    object_id,
                    data: ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                },
            value: ObjectValue::Extent(value),
            ..
        }) = iter.get()
        {
            if *object_id != self.object_id() || *attr_id != attribute_id {
                break;
            }
            if let ExtentValue::Some { device_offset, .. } = value {
                if let Some(overlap) = key.overlap(extent_key) {
                    let range = device_offset + overlap.start - extent_key.range.start
                        ..device_offset + overlap.end - extent_key.range.start;
                    ensure!(range.is_aligned(block_size), FxfsError::Inconsistent);
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?range,
                            len = range.end - range.start,
                            ?extent_key,
                            "D",
                        );
                    }
                    allocator
                        .deallocate(transaction, self.store().store_object_id(), range)
                        .await?;
                    deallocated += overlap.end - overlap.start;
                } else {
                    break;
                }
            }
            iter.advance().await?;
        }
        Ok(deallocated)
    }

    // Writes aligned data (that should already be encrypted) to the given offset and computes
    // checksums if requested.
    async fn write_aligned(
        &self,
        buf: BufferRef<'_>,
        device_offset: u64,
        compute_checksum: bool,
    ) -> Result<Checksums, Error> {
        if self.trace() {
            info!(
                store_id = self.store().store_object_id(),
                oid = self.object_id(),
                device_range = ?(device_offset..device_offset + buf.len() as u64),
                len = buf.len(),
                "W",
            );
        }
        let mut checksums = Vec::new();
        try_join!(self.store().device.write(device_offset, buf), async {
            if compute_checksum {
                let block_size = self.block_size();
                for chunk in buf.as_slice().chunks_exact(block_size as usize) {
                    checksums.push(fletcher64(chunk, 0));
                }
            }
            Ok(())
        })?;
        Ok(if compute_checksum { Checksums::Fletcher(checksums) } else { Checksums::None })
    }

    /// Flushes the underlying device.  This is expensive and should be used sparingly.
    pub async fn flush_device(&self) -> Result<(), Error> {
        self.store().device().flush().await
    }

    pub async fn write_timestamps<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        crtime: Option<Timestamp>,
        mtime: Option<Timestamp>,
    ) -> Result<(), Error> {
        if let (None, None) = (crtime.as_ref(), mtime.as_ref()) {
            return Ok(());
        }
        let mut mutation = self.txn_get_object_mutation(transaction).await?;
        if let ObjectValue::Object { ref mut attributes, .. } = mutation.item.value {
            if let Some(time) = crtime {
                attributes.creation_time = time;
            }
            if let Some(time) = mtime {
                attributes.modification_time = time;
            }
        } else {
            bail!(
                anyhow!(FxfsError::Inconsistent).context("write_timestamps: Expected object value")
            );
        };
        transaction.add(self.store().store_object_id(), Mutation::ObjectStore(mutation));
        Ok(())
    }

    pub async fn update_allocated_size(
        &self,
        transaction: &mut Transaction<'_>,
        allocated: u64,
        deallocated: u64,
    ) -> Result<(), Error> {
        if allocated == deallocated {
            return Ok(());
        }
        let mut mutation = self.txn_get_object_mutation(transaction).await?;
        if let ObjectValue::Object {
            attributes: ObjectAttributes { project_id, allocated_size, .. },
            ..
        } = &mut mutation.item.value
        {
            // The only way for these to fail are if the volume is inconsistent.
            *allocated_size = allocated_size
                .checked_add(allocated)
                .ok_or_else(|| anyhow!(FxfsError::Inconsistent).context("Allocated size overflow"))?
                .checked_sub(deallocated)
                .ok_or_else(|| {
                    anyhow!(FxfsError::Inconsistent).context("Allocated size underflow")
                })?;

            if *project_id != 0 {
                // The allocated and deallocated shouldn't exceed the max size of the file which is
                // bound within i64.
                let diff = i64::try_from(allocated).unwrap() - i64::try_from(deallocated).unwrap();
                transaction.add(
                    self.store().store_object_id(),
                    Mutation::merge_object(
                        ObjectKey::project_usage(
                            self.store().root_directory_object_id(),
                            *project_id,
                        ),
                        ObjectValue::BytesAndNodes { bytes: diff, nodes: 0 },
                    ),
                );
            }
        } else {
            // This can occur when the object mutation is created from an object in the tree which
            // was corrupt.
            bail!(anyhow!(FxfsError::Inconsistent).context("Unexpected object value"));
        }
        transaction.add(self.store().store_object_id, Mutation::ObjectStore(mutation));
        Ok(())
    }

    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        node_attributes: Option<&fio::MutableNodeAttributes>,
        change_time: Option<Timestamp>,
    ) -> Result<(), Error> {
        self.store()
            .update_attributes(transaction, self.object_id, node_attributes, change_time)
            .await
    }

    /// Zeroes the given range.  The range must be aligned.  Returns the amount of data deallocated.
    pub async fn zero(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        range: Range<u64>,
    ) -> Result<(), Error> {
        let deallocated =
            self.deallocate_old_extents(transaction, attribute_id, range.clone()).await?;
        if deallocated > 0 {
            self.update_allocated_size(transaction, 0, deallocated).await?;
            transaction.add(
                self.store().store_object_id,
                Mutation::merge_object(
                    ObjectKey::extent(self.object_id(), attribute_id, range),
                    ObjectValue::Extent(ExtentValue::deleted_extent()),
                ),
            );
        }
        Ok(())
    }

    // Returns a new aligned buffer (reading the head and tail blocks if necessary) with a copy of
    // the data from `buf`.
    pub async fn align_buffer(
        &self,
        attribute_id: u64,
        offset: u64,
        buf: BufferRef<'_>,
    ) -> Result<(std::ops::Range<u64>, Buffer<'_>), Error> {
        let block_size = self.block_size();
        let end = offset + buf.len() as u64;
        let aligned =
            round_down(offset, block_size)..round_up(end, block_size).ok_or(FxfsError::TooBig)?;

        let mut aligned_buf =
            self.store().device.allocate_buffer((aligned.end - aligned.start) as usize);

        // Deal with head alignment.
        if aligned.start < offset {
            let mut head_block = aligned_buf.subslice_mut(..block_size as usize);
            let read = self.read(attribute_id, aligned.start, head_block.reborrow()).await?;
            head_block.as_mut_slice()[read..].fill(0);
        }

        // Deal with tail alignment.
        if aligned.end > end {
            let end_block_offset = aligned.end - block_size;
            // There's no need to read the tail block if we read it as part of the head block.
            if offset <= end_block_offset {
                let mut tail_block =
                    aligned_buf.subslice_mut(aligned_buf.len() - block_size as usize..);
                let read = self.read(attribute_id, end_block_offset, tail_block.reborrow()).await?;
                tail_block.as_mut_slice()[read..].fill(0);
            }
        }

        aligned_buf.as_mut_slice()
            [(offset - aligned.start) as usize..(end - aligned.start) as usize]
            .copy_from_slice(buf.as_slice());

        Ok((aligned, aligned_buf))
    }

    /// Trim an attribute's extents, potentially adding a graveyard trim entry if more trimming is
    /// needed, so the transaction can be committed without worrying about leaking data.
    ///
    /// This doesn't update the size stored in the attribute value - the caller is responsible for
    /// doing that to keep the size up to date.
    pub async fn shrink<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attribute_id: u64,
        size: u64,
    ) -> Result<NeedsTrim, Error> {
        let store = self.store();
        let needs_trim = matches!(
            store
                .trim_some(transaction, self.object_id(), attribute_id, TrimMode::FromOffset(size))
                .await?,
            TrimResult::Incomplete
        );
        if needs_trim {
            // Add the object to the graveyard in case the following transactions don't get
            // replayed.
            let graveyard_id = store.graveyard_directory_object_id();
            match store
                .tree
                .find(&ObjectKey::graveyard_entry(graveyard_id, self.object_id()))
                .await?
            {
                Some(ObjectItem { value: ObjectValue::Some, .. })
                | Some(ObjectItem { value: ObjectValue::Trim, .. }) => {
                    // This object is already in the graveyard so we don't need to do anything.
                }
                _ => {
                    transaction.add(
                        store.store_object_id,
                        Mutation::replace_or_insert_object(
                            ObjectKey::graveyard_entry(graveyard_id, self.object_id()),
                            ObjectValue::Trim,
                        ),
                    );
                }
            }
        }
        Ok(NeedsTrim(needs_trim))
    }

    pub async fn read_and_decrypt(
        &self,
        device_offset: u64,
        file_offset: u64,
        mut buffer: MutableBufferRef<'_>,
        key_id: u64,
    ) -> Result<(), Error> {
        let store = self.store();
        store.device.read(device_offset, buffer.reborrow()).await?;
        if let Some(keys) = self.get_keys().await? {
            keys.decrypt(file_offset, key_id, buffer.as_mut_slice())?;
        }
        Ok(())
    }

    async fn get_keys(&self) -> Result<Option<Arc<XtsCipherSet>>, Error> {
        let store = self.store();
        Ok(match self.encryption {
            Encryption::None => None,
            Encryption::CachedKeys => Some(
                store
                    .key_manager
                    .get_or_insert(
                        self.object_id,
                        store.crypt().ok_or_else(|| anyhow!("No crypt!"))?,
                        store.get_keys(self.object_id),
                        false,
                    )
                    .await?,
            ),
            Encryption::PermanentKeys => {
                Some(store.key_manager.get(self.object_id).await?.unwrap())
            }
        })
    }

    async fn get_or_create_keys(
        &self,
        transaction: &mut Transaction<'_>,
    ) -> Result<Option<Arc<XtsCipherSet>>, Error> {
        let store = self.store();
        Ok(match self.encryption {
            Encryption::None => None,
            Encryption::CachedKeys => {
                // Fast path: try and get keys from the cache.
                if let Some(keys) = store.key_manager.get(self.object_id).await? {
                    return Ok(Some(keys));
                }

                // Next, see if the keys are already created.
                if let Some(item) = store.tree.find(&ObjectKey::keys(self.object_id)).await? {
                    if let ObjectValue::Keys(EncryptionKeys::AES256XTS(keys)) = item.value {
                        return Ok(Some(
                            store
                                .key_manager
                                .get_or_insert(
                                    self.object_id,
                                    store.crypt().ok_or_else(|| anyhow!("No crypt!"))?,
                                    async { Ok(keys) },
                                    false,
                                )
                                .await?,
                        ));
                    } else {
                        return Err(anyhow!(FxfsError::Inconsistent).context("get_or_create_keys"));
                    }
                }

                // Proceed to create the key.  The transaction holds the required locks.
                let (key, unwrapped_key) = store
                    .crypt()
                    .ok_or_else(|| anyhow!("No crypt!"))?
                    .create_key(self.object_id, KeyPurpose::Data)
                    .await?;

                // Arrange for the key to be added to the cache when (and if) the transaction
                // commits.
                struct UnwrappedKeys {
                    object_id: u64,
                    unwrapped_keys: Arc<XtsCipherSet>,
                }

                impl AssociatedObject for UnwrappedKeys {
                    fn will_apply_mutation(
                        &self,
                        _mutation: &Mutation,
                        object_id: u64,
                        manager: &ObjectManager,
                    ) {
                        manager.store(object_id).unwrap().key_manager.insert(
                            self.object_id,
                            self.unwrapped_keys.clone(),
                            /* permanent: */ false,
                        );
                    }
                }

                let unwrapped_keys = Arc::new(XtsCipherSet::new(&vec![(0, unwrapped_key)]));

                transaction.add_with_object(
                    store.store_object_id(),
                    Mutation::insert_object(
                        ObjectKey::keys(self.object_id),
                        ObjectValue::keys(EncryptionKeys::AES256XTS(WrappedKeys(vec![(0, key)]))),
                    ),
                    AssocObj::Owned(Box::new(UnwrappedKeys {
                        object_id: self.object_id,
                        unwrapped_keys: unwrapped_keys.clone(),
                    })),
                );

                Some(unwrapped_keys)
            }
            Encryption::PermanentKeys => {
                Some(store.key_manager.get(self.object_id).await?.unwrap())
            }
        })
    }

    pub async fn read(
        &self,
        attribute_id: u64,
        offset: u64,
        mut buf: MutableBufferRef<'_>,
    ) -> Result<usize, Error> {
        let fs = self.store().filesystem();
        let guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object_attribute(
                self.store().store_object_id(),
                self.object_id(),
                attribute_id,
            )])
            .await;

        let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
        let item = self.store().tree().find(&key).await?;
        let size = match item {
            Some(item) if item.key == key => match item.value {
                ObjectValue::Attribute { size } => size,
                // Attribute was deleted.
                ObjectValue::None => return Ok(0),
                _ => bail!(FxfsError::Inconsistent),
            },
            _ => return Ok(0),
        };
        if offset >= size {
            return Ok(0);
        }
        let length = min(buf.len() as u64, size - offset) as usize;
        buf = buf.subslice_mut(0..length);
        self.read_unchecked(attribute_id, offset, buf, &guard).await?;
        Ok(length)
    }

    /// Read `buf.len()` bytes from the attribute `attribute_id`, starting at `offset`, into `buf`.
    /// It's required that a read lock on this attribute id is taken before this is called.
    ///
    /// This function doesn't do any size checking - any portion of `buf` past the end of the file
    /// will be filled with zeros. The caller is responsible for enforcing the file size on reads.
    /// This is because, just looking at the extents, we can't tell the difference between the file
    /// actually ending and there just being a section at the end with no data (since attributes
    /// are sparse).
    pub(super) async fn read_unchecked(
        &self,
        attribute_id: u64,
        mut offset: u64,
        mut buf: MutableBufferRef<'_>,
        _guard: &ReadGuard<'_>,
    ) -> Result<(), Error> {
        if buf.len() == 0 {
            return Ok(());
        }
        // Whilst the read offset must be aligned to the filesystem block size, the buffer need only
        // be aligned to the device's block size.
        let block_size = self.block_size() as u64;
        let device_block_size = self.store().device.block_size() as u64;
        assert_eq!(offset % block_size, 0);
        assert_eq!(buf.range().start as u64 % device_block_size, 0);
        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .seek(Bound::Included(&ObjectKey::extent(
                self.object_id(),
                attribute_id,
                offset..offset + 1,
            )))
            .await?;
        let end_align = ((offset + buf.len() as u64) % block_size) as usize;
        let trace = self.trace();
        let reads = FuturesUnordered::new();
        while let Some(ItemRef {
            key:
                ObjectKey {
                    object_id,
                    data: ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                },
            value: ObjectValue::Extent(extent_value),
            ..
        }) = iter.get()
        {
            if *object_id != self.object_id() || *attr_id != attribute_id {
                break;
            }
            ensure!(
                extent_key.range.is_valid() && extent_key.range.is_aligned(block_size),
                FxfsError::Inconsistent
            );
            if extent_key.range.start > offset {
                // Zero everything up to the start of the extent.
                let to_zero = min(extent_key.range.start - offset, buf.len() as u64) as usize;
                for i in &mut buf.as_mut_slice()[..to_zero] {
                    *i = 0;
                }
                buf = buf.subslice_mut(to_zero..);
                if buf.is_empty() {
                    break;
                }
                offset += to_zero as u64;
            }

            if let ExtentValue::Some { device_offset, key_id, .. } = extent_value {
                let mut device_offset = device_offset + (offset - extent_key.range.start);

                let to_copy = min(buf.len() - end_align, (extent_key.range.end - offset) as usize);
                if to_copy > 0 {
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?(device_offset..device_offset + to_copy as u64),
                            "R",
                        );
                    }
                    let (head, tail) = buf.split_at_mut(to_copy);
                    reads.push(self.read_and_decrypt(device_offset, offset, head, *key_id));
                    buf = tail;
                    if buf.is_empty() {
                        break;
                    }
                    offset += to_copy as u64;
                    device_offset += to_copy as u64;
                }

                // Deal with end alignment by reading the existing contents into an alignment
                // buffer.
                if offset < extent_key.range.end && end_align > 0 {
                    let mut align_buf = self.store().device.allocate_buffer(block_size as usize);
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?(device_offset..device_offset + align_buf.len() as u64),
                            "RT",
                        );
                    }
                    self.read_and_decrypt(device_offset, offset, align_buf.as_mut(), *key_id)
                        .await?;
                    buf.as_mut_slice().copy_from_slice(&align_buf.as_slice()[..end_align]);
                    buf = buf.subslice_mut(0..0);
                    break;
                }
            } else if extent_key.range.end >= offset + buf.len() as u64 {
                // Deleted extent covers remainder, so we're done.
                break;
            }

            iter.advance().await?;
        }
        reads.try_collect().await?;
        buf.as_mut_slice().fill(0);
        Ok(())
    }

    /// Reads an entire attribute.
    pub async fn read_attr(&self, attribute_id: u64) -> Result<Option<Box<[u8]>>, Error> {
        let store = self.store();
        let tree = &store.tree;
        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
        let mut iter = merger.seek(Bound::Included(&key)).await?;
        let (mut buffer, size) = match iter.get() {
            Some(item) if item.key == &key => match item.value {
                ObjectValue::Attribute { size } => {
                    // TODO(fxbug.dev/122125): size > max buffer size
                    (
                        store
                            .device
                            .allocate_buffer(round_up(*size, self.block_size()).unwrap() as usize),
                        *size as usize,
                    )
                }
                // Attribute was deleted.
                ObjectValue::None => return Ok(None),
                _ => bail!(FxfsError::Inconsistent),
            },
            _ => return Ok(None),
        };
        let mut last_offset = 0;
        loop {
            iter.advance().await?;
            match iter.get() {
                Some(ItemRef {
                    key:
                        ObjectKey {
                            object_id,
                            data:
                                ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                        },
                    value: ObjectValue::Extent(extent_value),
                    ..
                }) if *object_id == self.object_id() && *attr_id == attribute_id => {
                    if let ExtentValue::Some { device_offset, key_id, .. } = extent_value {
                        let offset = extent_key.range.start as usize;
                        buffer.as_mut_slice()[last_offset..offset].fill(0);
                        let end = std::cmp::min(extent_key.range.end as usize, buffer.len());
                        self.read_and_decrypt(
                            *device_offset,
                            extent_key.range.start,
                            buffer.subslice_mut(offset..end as usize),
                            *key_id,
                        )
                        .await?;
                        last_offset = end;
                        if last_offset >= size {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        buffer.as_mut_slice()[std::cmp::min(last_offset, size)..].fill(0);
        Ok(Some(buffer.as_slice()[..size].into()))
    }

    /// Writes potentially unaligned data at `device_offset` and returns checksums if requested.
    /// The data will be encrypted if necessary.  `buf` is mutable as an optimization, since the
    /// write may require encryption, we can encrypt the buffer in-place rather than copying to
    /// another buffer if the write is already aligned.
    ///
    /// NOTE: This will not create keys if they are missing (it will fail with an error if that
    /// happens to be the case).
    pub async fn write_at(
        &self,
        attribute_id: u64,
        offset: u64,
        buf: MutableBufferRef<'_>,
        device_offset: u64,
        compute_checksum: bool,
    ) -> Result<Checksums, Error> {
        let mut transfer_buf;
        let block_size = self.block_size();
        let (range, mut transfer_buf_ref) =
            if offset % block_size == 0 && buf.len() as u64 % block_size == 0 {
                (offset..offset + buf.len() as u64, buf)
            } else {
                let (range, buf) = self.align_buffer(attribute_id, offset, buf.as_ref()).await?;
                transfer_buf = buf;
                (range, transfer_buf.as_mut())
            };

        if let Some(keys) = self.get_keys().await? {
            // TODO(https://fxbug.dev/92975): Support key_id != 0.
            keys.encrypt(range.start, 0, transfer_buf_ref.as_mut_slice())?;
        }

        self.write_aligned(
            transfer_buf_ref.as_ref(),
            device_offset - (offset - range.start),
            compute_checksum,
        )
        .await
    }

    // Writes to multiple ranges with data provided in `buf`.  The buffer can be modified in place
    // if encryption takes place.  The ranges must all be aligned and no change to content size is
    // applied; the caller is responsible for updating size if required.
    pub async fn multi_write<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attribute_id: u64,
        ranges: &[Range<u64>],
        mut buf: MutableBufferRef<'_>,
    ) -> Result<(), Error> {
        if buf.is_empty() {
            return Ok(());
        }
        let block_size = self.block_size();
        let store = self.store();
        let store_id = store.store_object_id();

        if let Some(keys) = self.get_or_create_keys(transaction).await? {
            let mut slice = buf.as_mut_slice();
            for r in ranges {
                let l = r.end - r.start;
                let (head, tail) = slice.split_at_mut(l as usize);
                // TODO(https://fxbug.dev/92975): Support key_id != 0.
                keys.encrypt(r.start, 0, head)?;
                slice = tail;
            }
        }

        let mut allocated = 0;
        let allocator = store.allocator();
        let trace = self.trace();
        let mut writes = FuturesOrdered::new();
        while !buf.is_empty() {
            let device_range = allocator
                .allocate(transaction, store_id, buf.len() as u64)
                .await
                .context("allocation failed")?;
            if trace {
                info!(
                    store_id,
                    oid = self.object_id(),
                    ?device_range,
                    len = device_range.end - device_range.start,
                    "A",
                );
            }
            let device_range_len = device_range.end - device_range.start;
            allocated += device_range_len;

            let (head, tail) = buf.split_at_mut(device_range_len as usize);
            buf = tail;

            writes.push(async move {
                let len = head.len() as u64;
                Result::<_, Error>::Ok((
                    device_range.start,
                    len,
                    self.write_aligned(head.as_ref(), device_range.start, true).await?,
                ))
            });
        }

        let (mutations, deallocated) = try_join!(
            async {
                let mut current_range = 0..0;
                let mut mutations = Vec::new();
                let mut ranges = ranges.iter();
                while let Some((mut device_offset, mut len, mut checksums)) =
                    writes.try_next().await?
                {
                    while len > 0 {
                        if current_range.end <= current_range.start {
                            current_range = ranges.next().unwrap().clone();
                        }
                        let l = std::cmp::min(len, current_range.end - current_range.start);
                        let tail = checksums.split_off((l / block_size) as usize);
                        mutations.push(Mutation::merge_object(
                            ObjectKey::extent(
                                self.object_id(),
                                attribute_id,
                                current_range.start..current_range.start + l,
                            ),
                            ObjectValue::Extent(ExtentValue::with_checksum(
                                device_offset,
                                checksums,
                            )),
                        ));
                        checksums = tail;
                        device_offset += l;
                        len -= l;
                        current_range.start += l;
                    }
                }
                Result::<_, Error>::Ok(mutations)
            },
            async {
                let mut deallocated = 0;
                for r in ranges {
                    deallocated +=
                        self.deallocate_old_extents(transaction, attribute_id, r.clone()).await?;
                }
                Result::<_, Error>::Ok(deallocated)
            }
        )?;
        for m in mutations {
            transaction.add(store_id, m);
        }
        self.update_allocated_size(transaction, allocated, deallocated).await
    }

    /// Writes an entire attribute. Returns whether or not the attribute needs to continue being
    /// trimmed - if the new data is shorter than the old data, this will trim any extents beyond
    /// the end of the new size, but if there were too many for a single transaction, a commit
    /// needs to be made before trimming again, so the responsibility is left to the caller so as
    /// to not accidentally split the transaction when it's not in a consistent state.
    pub async fn write_attr<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attribute_id: u64,
        data: &[u8],
    ) -> Result<NeedsTrim, Error> {
        let rounded_len = round_up(data.len() as u64, self.block_size()).unwrap();
        let store = self.store();
        let tree = store.tree();
        let should_trim = if let Some(item) = tree
            .find(&ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute))
            .await?
        {
            match item.value {
                ObjectValue::None => false,
                ObjectValue::Attribute { size } => (data.len() as u64) < size,
                _ => bail!(FxfsError::Inconsistent),
            }
        } else {
            false
        };
        let mut buffer = self.store().device.allocate_buffer(rounded_len as usize);
        let slice = buffer.as_mut_slice();
        slice[..data.len()].copy_from_slice(data);
        slice[data.len()..].fill(0);
        self.multi_write(transaction, attribute_id, &[0..rounded_len], buffer.as_mut()).await?;
        transaction.add(
            self.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute),
                ObjectValue::attribute(data.len() as u64),
            ),
        );
        if should_trim {
            self.shrink(transaction, attribute_id, data.len() as u64).await
        } else {
            Ok(NeedsTrim(false))
        }
    }

    pub async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Error> {
        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        // Seek to the first extended attribute key for this object.
        let mut iter = merger
            .seek(Bound::Included(&ObjectKey::extended_attribute(self.object_id(), Vec::new())))
            .await?;
        let mut out = Vec::new();
        while let Some(item) = iter.get() {
            // Skip deleted extended attributes.
            if item.value != &ObjectValue::None {
                match item.key {
                    ObjectKey { object_id, data: ObjectKeyData::ExtendedAttribute { name } } => {
                        if self.object_id() != *object_id {
                            bail!(anyhow!(FxfsError::Inconsistent)
                                .context("list_extended_attributes: wrong object id"))
                        }
                        out.push(name.clone());
                    }
                    // Once we hit something that isn't an extended attribute key, we've gotten to
                    // the end.
                    _ => break,
                }
            }
            iter.advance().await?;
        }
        Ok(out)
    }

    pub async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Error> {
        let item = self
            .store()
            .tree()
            .find(&ObjectKey::extended_attribute(self.object_id(), name))
            .await?
            .ok_or(FxfsError::NotFound)?;
        match item.value {
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(value)) => Ok(value),
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => {
                let data = self.read_attr(id).await?.ok_or(FxfsError::NotFound)?;
                Ok(data.into_vec())
            }
            // If an extended attribute has a value of None, it means it was deleted but hasn't
            // been cleaned up yet.
            ObjectValue::None => {
                bail!(FxfsError::NotFound)
            }
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_extended_attribute: Expected ExtendedAttribute value"))
            }
        }
    }

    pub async fn set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: SetExtendedAttributeMode,
    ) -> Result<(), Error> {
        ensure!(name.len() <= MAX_XATTR_NAME_SIZE, FxfsError::TooBig);
        ensure!(value.len() <= MAX_XATTR_VALUE_SIZE, FxfsError::TooBig);
        let store = self.store();
        let fs = store.filesystem();
        let tree = store.tree();
        let object_key = ObjectKey::extended_attribute(self.object_id(), name);

        // NB: We need to take this lock before we potentially look up the value to prevent racing
        // with another set.
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.object_id())];
        let mut transaction = fs.new_transaction(keys, Options::default()).await?;

        let existing_attribute_id = {
            let (found, existing_attribute_id) = match tree.find(&object_key).await? {
                None => (false, None),
                Some(Item { value, .. }) => (
                    true,
                    match value {
                        ObjectValue::None => None,
                        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(..)) => None,
                        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => {
                            Some(id)
                        }
                        _ => bail!(anyhow!(FxfsError::Inconsistent)
                            .context("expected extended attribute value")),
                    },
                ),
            };
            match mode {
                SetExtendedAttributeMode::Create if found => {
                    bail!(FxfsError::AlreadyExists)
                }
                SetExtendedAttributeMode::Replace if !found => {
                    bail!(FxfsError::NotFound)
                }
                _ => (),
            }
            existing_attribute_id
        };

        if let Some(attribute_id) = existing_attribute_id {
            // If we already have an attribute id allocated for this extended attribute, we always
            // use it, even if the value has shrunk enough to be stored inline. We don't need to
            // worry about trimming here for the same reason we don't need to worry about it when
            // we delete xattrs - they simply aren't large enough to ever need more than one
            // transaction.
            let _ = self.write_attr(&mut transaction, attribute_id, &value).await?;
        } else if value.len() <= MAX_INLINE_XATTR_SIZE {
            transaction.add(
                self.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    object_key,
                    ObjectValue::inline_extended_attribute(value),
                ),
            );
        } else {
            // If there isn't an existing attribute id and we are going to store the value in
            // an attribute, find the next empty attribute id in the range. We search for fxfs
            // attribute records specifically, instead of the extended attribute records, because
            // even if the extended attribute record is removed the attribute may not be fully
            // trimmed yet.
            let mut attribute_id = EXTENDED_ATTRIBUTE_RANGE_START;
            let layer_set = tree.layer_set();
            let mut merger = layer_set.merger();
            let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
            let mut iter = merger.seek(Bound::Included(&key)).await?;
            loop {
                match iter.get() {
                    // None means the key passed to seek wasn't found. That means the first
                    // attribute is available and we can just stop right away.
                    None => break,
                    Some(ItemRef {
                        key: ObjectKey { object_id, data: ObjectKeyData::Attribute(attr_id, _) },
                        value,
                        ..
                    }) if *object_id == self.object_id() => {
                        if matches!(value, ObjectValue::None) {
                            // This attribute was once used but is now deleted, so it's safe to use
                            // again.
                            break;
                        }
                        if attribute_id < *attr_id {
                            // We found a gap - use it.
                            break;
                        } else if attribute_id == *attr_id {
                            // This attribute id is in use, try the next one.
                            attribute_id += 1;
                            if attribute_id == EXTENDED_ATTRIBUTE_RANGE_END {
                                bail!(FxfsError::NoSpace);
                            }
                        }
                        // If we don't hit either of those cases, we are still moving through the
                        // extent keys for the current attribute, so just keep advancing until the
                        // attribute id changes.
                    }
                    // As we are working our way through the iterator, if we hit anything that
                    // doesn't have our object id or attribute key data, we've gone past the end of
                    // this section and can stop.
                    _ => break,
                }
                iter.advance().await?;
            }

            // We know this won't need trimming because it's a new attribute.
            let _ = self.write_attr(&mut transaction, attribute_id, &value).await?;
            transaction.add(
                self.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    object_key,
                    ObjectValue::extended_attribute(attribute_id),
                ),
            );
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        let store = self.store();
        let tree = store.tree();
        let object_key = ObjectKey::extended_attribute(self.object_id(), name);

        // NB: The API says we have to return an error if the attribute doesn't exist, so we have
        // to look it up first to make sure we have a record of it before we delete it. Make sure
        // we take a lock and make a transaction before we do so we don't race with other
        // operations.
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.object_id())];
        let mut transaction = store.filesystem().new_transaction(keys, Options::default()).await?;

        let attribute_to_delete =
            match tree.find(&object_key).await?.ok_or(FxfsError::NotFound)?.value {
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => Some(id),
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(..)) => None,
                ObjectValue::None => bail!(FxfsError::NotFound),
                _ => {
                    bail!(anyhow!(FxfsError::Inconsistent)
                        .context("remove_extended_attribute: Expected ExtendedAttribute value"))
                }
            };

        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(object_key, ObjectValue::None),
        );

        // If the attribute wasn't stored inline, we need to deallocate all the extents too. This
        // would normally need to interact with the graveyard for correctness - if there are too
        // many extents to delete to fit in a single transaction then we could potentially have
        // consistency issues. However, the maximum size of an extended attribute is small enough
        // that it will never come close to that limit even in the worst case, so we just delete
        // everything in one shot.
        if let Some(attribute_id) = attribute_to_delete {
            let trim_result = store
                .trim_some(
                    &mut transaction,
                    self.object_id(),
                    attribute_id,
                    TrimMode::FromOffset(0),
                )
                .await?;
            // In case you didn't read the comment above - this should not be used to delete
            // arbitrary attributes!
            assert_matches!(trim_result, TrimResult::Done(_));
            transaction.add(
                store.store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::attribute(self.object_id, attribute_id, AttributeKey::Attribute),
                    ObjectValue::None,
                ),
            );
        }

        transaction.commit().await?;
        Ok(())
    }

    /// Returns a future that will pre-fetches the keys so as to avoid paying the performance
    /// penalty later.
    pub fn pre_fetch_keys(&self) -> Option<impl Future<Output = ()>> {
        if let Encryption::CachedKeys = self.encryption {
            let owner = self.owner.clone();
            let object_id = self.object_id;
            Some(async move {
                let store = owner.as_ref().as_ref();
                if let Some(crypt) = store.crypt() {
                    let _: Result<_, _> = store
                        .key_manager
                        .get_or_insert(object_id, crypt, store.get_keys(object_id), false)
                        .await;
                }
            })
        } else {
            None
        }
    }
}

impl<S: HandleOwner> Drop for StoreObjectHandle<S> {
    fn drop(&mut self) {
        if self.is_encrypted() {
            self.store().key_manager.remove(self.object_id)
        }
    }
}

/// When truncating an object, sometimes it might not be possible to complete the transaction in a
/// single transaction, in which case the caller needs to finish trimming the object in subsequent
/// transactions (by calling ObjectStore::trim).
#[must_use]
pub struct NeedsTrim(pub bool);

#[cfg(test)]
mod tests {
    use {
        crate::{
            errors::FxfsError,
            filesystem::{FxFilesystem, OpenFxFilesystem},
            object_handle::ObjectHandle,
            object_store::{
                transaction::{lock_keys, Mutation, Options, TransactionHandler},
                AttributeKey, DataObjectHandle, Directory, HandleOptions, LockKey, ObjectKey,
                ObjectStore, ObjectValue, SetExtendedAttributeMode, StoreObjectHandle,
            },
        },
        fuchsia_async as fasync,
        futures::join,
        fxfs_insecure_crypto::InsecureCrypt,
        std::sync::Arc,
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;
    const TEST_OBJECT_NAME: &str = "foo";

    fn is_error(actual: anyhow::Error, expected: FxfsError) {
        assert_eq!(*actual.root_cause().downcast_ref::<FxfsError>().unwrap(), expected)
    }

    async fn test_filesystem() -> OpenFxFilesystem {
        let device = DeviceHolder::new(FakeDevice::new(16384, TEST_DEVICE_BLOCK_SIZE));
        FxFilesystem::new_empty(device).await.expect("new_empty failed")
    }

    async fn test_filesystem_and_empty_object() -> (OpenFxFilesystem, DataObjectHandle<ObjectStore>)
    {
        let fs = test_filesystem().await;
        let store = fs.root_store();

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

        let object = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            Some(&InsecureCrypt::new()),
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

        transaction.commit().await.expect("commit failed");

        (fs, object)
    }

    #[fuchsia::test(threads = 3)]
    async fn extended_attribute_double_remove() {
        // This test is intended to trip a potential race condition in remove. Removing an
        // attribute that doesn't exist is an error, so we need to check before we remove, but if
        // we aren't careful, two parallel removes might both succeed in the check and then both
        // remove the value.
        let (fs, object) = test_filesystem_and_empty_object().await;
        let basic = Arc::new(StoreObjectHandle::new(
            object.owner().clone(),
            object.object_id(),
            /* permanent_keys: */ false,
            HandleOptions::default(),
            false,
        ));
        let basic_a = basic.clone();
        let basic_b = basic.clone();

        basic
            .set_extended_attribute(
                b"security.selinux".to_vec(),
                b"bar".to_vec(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .expect("failed to set attribute");

        // Try to remove the attribute twice at the same time. One should succeed in the race and
        // return Ok, and the other should fail the race and return NOT_FOUND.
        let a_task = fasync::Task::spawn(async move {
            basic_a.remove_extended_attribute(b"security.selinux".to_vec()).await
        });
        let b_task = fasync::Task::spawn(async move {
            basic_b.remove_extended_attribute(b"security.selinux".to_vec()).await
        });
        match join!(a_task, b_task) {
            (Ok(()), Ok(())) => panic!("both remove calls succeeded"),
            (Err(_), Err(_)) => panic!("both remove calls failed"),

            (Ok(()), Err(e)) => is_error(e, FxfsError::NotFound),
            (Err(e), Ok(())) => is_error(e, FxfsError::NotFound),
        }

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 3)]
    async fn extended_attribute_double_create() {
        // This test is intended to trip a potential race in set when using the create flag,
        // similar to above. If the create mode is set, we need to check that the attribute isn't
        // already created, but if two parallel creates both succeed in that check, and we aren't
        // careful with locking, they will both succeed and one will overwrite the other.
        let (fs, object) = test_filesystem_and_empty_object().await;
        let basic = Arc::new(StoreObjectHandle::new(
            object.owner().clone(),
            object.object_id(),
            /* permanent_keys: */ false,
            HandleOptions::default(),
            false,
        ));
        let basic_a = basic.clone();
        let basic_b = basic.clone();

        // Try to set the attribute twice at the same time. One should succeed in the race and
        // return Ok, and the other should fail the race and return ALREADY_EXISTS.
        let a_task = fasync::Task::spawn(async move {
            basic_a
                .set_extended_attribute(
                    b"security.selinux".to_vec(),
                    b"one".to_vec(),
                    SetExtendedAttributeMode::Create,
                )
                .await
        });
        let b_task = fasync::Task::spawn(async move {
            basic_b
                .set_extended_attribute(
                    b"security.selinux".to_vec(),
                    b"two".to_vec(),
                    SetExtendedAttributeMode::Create,
                )
                .await
        });
        match join!(a_task, b_task) {
            (Ok(()), Ok(())) => panic!("both set calls succeeded"),
            (Err(_), Err(_)) => panic!("both set calls failed"),

            (Ok(()), Err(e)) => {
                assert_eq!(
                    basic
                        .get_extended_attribute(b"security.selinux".to_vec())
                        .await
                        .expect("failed to get xattr"),
                    b"one"
                );
                is_error(e, FxfsError::AlreadyExists);
            }
            (Err(e), Ok(())) => {
                assert_eq!(
                    basic
                        .get_extended_attribute(b"security.selinux".to_vec())
                        .await
                        .expect("failed to get xattr"),
                    b"two"
                );
                is_error(e, FxfsError::AlreadyExists);
            }
        }

        fs.close().await.expect("Close failed");
    }

    struct TestAttr {
        name: Vec<u8>,
        value: Vec<u8>,
    }

    impl TestAttr {
        fn new(name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
            Self { name: name.as_ref().to_vec(), value: value.as_ref().to_vec() }
        }
        fn name(&self) -> Vec<u8> {
            self.name.clone()
        }
        fn value(&self) -> Vec<u8> {
            self.value.clone()
        }
    }

    #[fuchsia::test]
    async fn extended_attributes() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let test_attr = TestAttr::new(b"security.selinux", b"foo");

        assert_eq!(handle.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            handle.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        handle
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(handle.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        assert_eq!(
            handle.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        handle.remove_extended_attribute(test_attr.name()).await.unwrap();
        assert_eq!(handle.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            handle.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        // Make sure we can handle the same attribute being set again.
        handle
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(handle.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        assert_eq!(
            handle.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        handle.remove_extended_attribute(test_attr.name()).await.unwrap();
        assert_eq!(handle.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            handle.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn large_extended_attribute() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let test_attr = TestAttr::new(b"security.selinux", vec![3u8; 300]);

        handle
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        // Probe the fxfs attributes to make sure it did the expected thing. This relies on inside
        // knowledge of how the attribute id is chosen.
        assert_eq!(
            handle
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_attr.value()
        );

        handle.remove_extended_attribute(test_attr.name()).await.unwrap();
        is_error(
            handle.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        // Make sure we can handle the same attribute being set again.
        handle
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );
        handle.remove_extended_attribute(test_attr.name()).await.unwrap();
        is_error(
            handle.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn multiple_extended_attributes() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let attrs = [
            TestAttr::new(b"security.selinux", b"foo"),
            TestAttr::new(b"large.attribute", vec![3u8; 300]),
            TestAttr::new(b"an.attribute", b"asdf"),
            TestAttr::new(b"user.big", vec![5u8; 288]),
            TestAttr::new(b"user.tiny", b"smol"),
            TestAttr::new(b"this string doesn't matter", b"the quick brown fox etc"),
            TestAttr::new(b"also big", vec![7u8; 500]),
            TestAttr::new(b"all.ones", vec![1u8; 11111]),
        ];

        for i in 0..attrs.len() {
            handle
                .set_extended_attribute(
                    attrs[i].name(),
                    attrs[i].value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap();
            assert_eq!(
                handle.get_extended_attribute(attrs[i].name()).await.unwrap(),
                attrs[i].value()
            );
        }

        for i in 0..attrs.len() {
            // Make sure expected attributes are still available.
            let mut found_attrs = handle.list_extended_attributes().await.unwrap();
            let mut expected_attrs: Vec<Vec<u8>> = attrs.iter().skip(i).map(|a| a.name()).collect();
            found_attrs.sort();
            expected_attrs.sort();
            assert_eq!(found_attrs, expected_attrs);
            for j in i..attrs.len() {
                assert_eq!(
                    handle.get_extended_attribute(attrs[j].name()).await.unwrap(),
                    attrs[j].value()
                );
            }

            handle.remove_extended_attribute(attrs[i].name()).await.expect("failed to remove");
            is_error(
                handle.get_extended_attribute(attrs[i].name()).await.unwrap_err(),
                FxfsError::NotFound,
            );
        }

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn multiple_extended_attributes_delete() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let store = object.owner().clone();
        let handle = object.handle();

        let attrs = [
            TestAttr::new(b"security.selinux", b"foo"),
            TestAttr::new(b"large.attribute", vec![3u8; 300]),
            TestAttr::new(b"an.attribute", b"asdf"),
            TestAttr::new(b"user.big", vec![5u8; 288]),
            TestAttr::new(b"user.tiny", b"smol"),
            TestAttr::new(b"this string doesn't matter", b"the quick brown fox etc"),
            TestAttr::new(b"also big", vec![7u8; 500]),
            TestAttr::new(b"all.ones", vec![1u8; 11111]),
        ];

        for i in 0..attrs.len() {
            handle
                .set_extended_attribute(
                    attrs[i].name(),
                    attrs[i].value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap();
            assert_eq!(
                handle.get_extended_attribute(attrs[i].name()).await.unwrap(),
                attrs[i].value()
            );
        }

        // Unlink the file
        let root_directory =
            Directory::open(object.owner(), object.store().root_directory_object_id())
                .await
                .expect("open failed");
        let mut transaction = fs
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
        crate::object_store::directory::replace_child(
            &mut transaction,
            None,
            (&root_directory, TEST_OBJECT_NAME),
        )
        .await
        .expect("replace_child failed");
        transaction.commit().await.unwrap();
        store.tombstone(object.object_id(), Options::default()).await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn extended_attribute_changing_sizes() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let test_name = b"security.selinux";
        let test_small_attr = TestAttr::new(test_name, b"smol");
        let test_large_attr = TestAttr::new(test_name, vec![3u8; 300]);

        handle
            .set_extended_attribute(
                test_small_attr.name(),
                test_small_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_small_attr.name()).await.unwrap(),
            test_small_attr.value()
        );

        // With a small attribute, we don't expect it to write to an fxfs attribute.
        assert!(handle.read_attr(64).await.expect("read_attr failed").is_none());

        crate::fsck::fsck(fs.clone()).await.unwrap();

        handle
            .set_extended_attribute(
                test_large_attr.name(),
                test_large_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_large_attr.name()).await.unwrap(),
            test_large_attr.value()
        );

        // Once the value is above the threshold, we expect it to get upgraded to an fxfs
        // attribute.
        assert_eq!(
            handle
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_large_attr.value()
        );

        crate::fsck::fsck(fs.clone()).await.unwrap();

        handle
            .set_extended_attribute(
                test_small_attr.name(),
                test_small_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_small_attr.name()).await.unwrap(),
            test_small_attr.value()
        );

        // Even though we are back under the threshold, we still expect it to be stored in an fxfs
        // attribute, because we don't downgrade to inline once we've allocated one.
        assert_eq!(
            handle
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_small_attr.value()
        );

        crate::fsck::fsck(fs.clone()).await.unwrap();

        handle.remove_extended_attribute(test_small_attr.name()).await.expect("failed to remove");

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn extended_attribute_max_size() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let test_attr = TestAttr::new(
            vec![3u8; super::MAX_XATTR_NAME_SIZE],
            vec![1u8; super::MAX_XATTR_VALUE_SIZE],
        );

        handle
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            handle.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );
        assert_eq!(handle.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        handle.remove_extended_attribute(test_attr.name()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn large_extended_attribute_max_number() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let max_xattrs =
            super::EXTENDED_ATTRIBUTE_RANGE_END - super::EXTENDED_ATTRIBUTE_RANGE_START;
        for i in 0..max_xattrs {
            let test_attr = TestAttr::new(format!("{}", i).as_bytes(), vec![0x3; 300]);
            handle
                .set_extended_attribute(
                    test_attr.name(),
                    test_attr.value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap_or_else(|_| panic!("failed to set xattr number {}", i));
        }

        // That should have taken up all the attributes we've allocated to extended attributes, so
        // this one should return ERR_NO_SPACE.
        match handle
            .set_extended_attribute(
                b"one.too.many".to_vec(),
                vec![0x3; 300],
                SetExtendedAttributeMode::Set,
            )
            .await
        {
            Ok(()) => panic!("set should not succeed"),
            Err(e) => is_error(e, FxfsError::NoSpace),
        }

        // But inline attributes don't need an attribute number, so it should work fine.
        handle
            .set_extended_attribute(
                b"this.is.okay".to_vec(),
                b"small value".to_vec(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        // And updating existing ones should be okay.
        handle
            .set_extended_attribute(b"11".to_vec(), vec![0x4; 300], SetExtendedAttributeMode::Set)
            .await
            .unwrap();
        handle
            .set_extended_attribute(
                b"12".to_vec(),
                vec![0x1; 300],
                SetExtendedAttributeMode::Replace,
            )
            .await
            .unwrap();

        // And we should be able to remove an attribute and set another one.
        handle.remove_extended_attribute(b"5".to_vec()).await.unwrap();
        handle
            .set_extended_attribute(
                b"new attr".to_vec(),
                vec![0x3; 300],
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn write_attr_trims_beyond_new_end() {
        // When writing, multi_write will deallocate old extents that overlap with the new data,
        // but it doesn't trim anything beyond that, since it doesn't know what the total size will
        // be. write_attr does know, because it writes the whole attribute at once, so we need to
        // make sure it cleans up properly.
        let (fs, object) = test_filesystem_and_empty_object().await;
        let handle = object.handle();

        let block_size = fs.block_size();
        let buf_size = block_size * 2;
        let attribute_id = 10;

        let mut transaction = handle.new_transaction(attribute_id).await.unwrap();
        let mut buffer = handle.allocate_buffer(buf_size as usize);
        buffer.as_mut_slice().fill(3);
        // Writing two separate ranges, even if they are contiguous, forces them to be separate
        // extent records.
        handle
            .multi_write(
                &mut transaction,
                attribute_id,
                &[0..block_size, block_size..block_size * 2],
                buffer.as_mut(),
            )
            .await
            .unwrap();
        transaction.add(
            handle.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(handle.object_id(), attribute_id, AttributeKey::Attribute),
                ObjectValue::attribute(block_size * 2),
            ),
        );
        transaction.commit().await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        let mut transaction = handle.new_transaction(attribute_id).await.unwrap();
        let needs_trim = handle
            .write_attr(&mut transaction, attribute_id, &vec![3u8; block_size as usize])
            .await
            .unwrap();
        assert!(!needs_trim.0);
        transaction.commit().await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }
}
