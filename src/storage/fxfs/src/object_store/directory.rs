// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        lsm_tree::{
            merge::{Merger, MergerIterator},
            types::{ItemRef, LayerIterator},
        },
        object_handle::{ObjectHandle, ObjectProperties, INVALID_OBJECT_ID},
        object_store::{
            object_record::{
                ChildValue, ObjectAttributes, ObjectDescriptor, ObjectItem, ObjectKey,
                ObjectKeyData, ObjectKind, ObjectValue, PosixAttributes, Timestamp,
            },
            transaction::{LockKey, LockKeys, Mutation, Options, Transaction},
            DataObjectHandle, HandleOptions, HandleOwner, ObjectStore, SetExtendedAttributeMode,
            StoreObjectHandle,
        },
    },
    anyhow::{anyhow, bail, ensure, Error},
    fidl_fuchsia_io as fio,
    std::{
        fmt,
        ops::Bound,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    },
};

/// This contains the transaction with the appropriate locks to replace src with dst, and also the
/// ID and type of the src and dst.
pub struct ReplaceContext<'a> {
    pub transaction: Transaction<'a>,
    pub src_id_and_descriptor: Option<(u64, ObjectDescriptor)>,
    pub dst_id_and_descriptor: Option<(u64, ObjectDescriptor)>,
}

/// A directory stores name to child object mappings.
pub struct Directory<S: HandleOwner> {
    handle: StoreObjectHandle<S>,
    is_deleted: AtomicBool,
}

#[fxfs_trace::trace]
impl<S: HandleOwner> Directory<S> {
    fn new(owner: Arc<S>, object_id: u64) -> Self {
        Directory {
            handle: StoreObjectHandle::new(
                owner,
                object_id,
                /* permanent_keys: */ false,
                HandleOptions::default(),
                /* trace: */ false,
            ),
            is_deleted: AtomicBool::new(false),
        }
    }

    pub fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    pub fn owner(&self) -> &Arc<S> {
        self.handle.owner()
    }

    pub fn store(&self) -> &ObjectStore {
        self.handle.store()
    }

    pub fn is_deleted(&self) -> bool {
        self.is_deleted.load(Ordering::Relaxed)
    }

    pub fn set_deleted(&self) {
        self.is_deleted.store(true, Ordering::Relaxed);
    }

    pub async fn create(
        transaction: &mut Transaction<'_>,
        owner: &Arc<S>,
        create_attributes: Option<&fio::MutableNodeAttributes>,
    ) -> Result<Directory<S>, Error> {
        let store = owner.as_ref().as_ref();
        let object_id = store.get_next_object_id(transaction).await?;
        let now = Timestamp::now();
        let creation_time = create_attributes
            .and_then(|a| a.creation_time)
            .map(Timestamp::from_nanos)
            .unwrap_or_else(|| now.clone());
        let modification_time = create_attributes
            .and_then(|a| a.modification_time)
            .map(Timestamp::from_nanos)
            .unwrap_or_else(|| now.clone());
        let access_time = create_attributes
            .and_then(|a| a.access_time)
            .map(Timestamp::from_nanos)
            .unwrap_or_else(|| now.clone());
        let change_time = now;
        let posix_attributes = create_attributes.and_then(|a| {
            (a.mode.is_some() || a.uid.is_some() || a.gid.is_some() || a.rdev.is_some()).then_some(
                PosixAttributes {
                    mode: a.mode.unwrap_or_default(),
                    uid: a.uid.unwrap_or_default(),
                    gid: a.gid.unwrap_or_default(),
                    rdev: a.rdev.unwrap_or_default(),
                },
            )
        });
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::object(object_id),
                ObjectValue::Object {
                    kind: ObjectKind::Directory { sub_dirs: 0 },
                    attributes: ObjectAttributes {
                        creation_time,
                        modification_time,
                        project_id: 0,
                        posix_attributes,
                        allocated_size: 0,
                        access_time,
                        change_time,
                    },
                },
            ),
        );
        Ok(Directory::new(owner.clone(), object_id))
    }

    #[trace]
    pub async fn open(owner: &Arc<S>, object_id: u64) -> Result<Directory<S>, Error> {
        let store = owner.as_ref().as_ref();
        match store.tree.find(&ObjectKey::object(object_id)).await?.ok_or(FxfsError::NotFound)? {
            ObjectItem {
                value: ObjectValue::Object { kind: ObjectKind::Directory { .. }, .. },
                ..
            } => Ok(Directory::new(owner.clone(), object_id)),
            ObjectItem { value: ObjectValue::None, .. } => bail!(FxfsError::NotFound),
            _ => bail!(FxfsError::NotDir),
        }
    }

    /// Opens a directory. The caller is responsible for ensuring that the object exists and is a
    /// directory.
    pub fn open_unchecked(owner: Arc<S>, object_id: u64) -> Self {
        Self::new(owner, object_id)
    }

    /// Acquires the transaction with the appropriate locks to replace src.0/src.1 with |dst|.
    /// src can be None in the case of unlinking |dst| from |self|.
    /// Returns the transaction, as well as the ID and type of the child and the src. If the child
    /// doesn't exist, then a transaction is returned with a lock only on the parent and None for
    /// the target info so that the transaction can be executed with the confidence that the target
    /// doesn't exist. If the src doesn't exist (in the case of unlinking), None is return for the
    /// source info.
    ///
    /// We need to lock |self|, but also the child if it exists. When it is a directory the lock
    /// prevents entries being added at the same time. When it is a file needs to be able to
    /// decrement the reference count.
    /// If src exists, we also need to lock |src.0| and |src.1|. This is to update their timestamps.
    pub async fn acquire_context_for_replace(
        &self,
        src: Option<(&Directory<S>, &str)>,
        dst: &str,
        borrow_metadata_space: bool,
    ) -> Result<ReplaceContext<'_>, Error> {
        // Since we don't know the child object ID until we've looked up the child, we need to loop
        // until we have acquired a lock on a child whose ID is the same as it was in the last
        // iteration. This also applies for src object ID if |src| is passed in.
        //
        // Note that the returned transaction may lock more objects than is necessary (for example,
        // if the child "foo" was first a directory, then was renamed to "bar" and a file "foo" was
        // created, we might acquire a lock on both the parent and "bar").
        //
        // We can look into not having this loop by adding support to try to add locks in the
        // transaction. If it fails, we can drop all the locks and start a new transaction.
        let store = self.store();
        let mut child_object_id = INVALID_OBJECT_ID;
        let mut src_object_id = src.map(|_| INVALID_OBJECT_ID);
        let mut lock_keys = LockKeys::with_capacity(4);
        lock_keys.push(LockKey::object(store.store_object_id(), self.object_id()));
        loop {
            lock_keys.truncate(1);
            if let Some(src) = src {
                lock_keys.push(LockKey::object(store.store_object_id(), src.0.object_id()));
                if let Some(src_object_id) = src_object_id {
                    if src_object_id != INVALID_OBJECT_ID {
                        lock_keys.push(LockKey::object(store.store_object_id(), src_object_id));
                    }
                }
            }
            if child_object_id != INVALID_OBJECT_ID {
                lock_keys.push(LockKey::object(store.store_object_id(), child_object_id));
            };
            let fs = store.filesystem().clone();
            let transaction = fs
                .new_transaction(
                    lock_keys.clone(),
                    Options { borrow_metadata_space, ..Default::default() },
                )
                .await?;

            let mut have_required_locks = true;
            let mut src_id_and_descriptor = None;
            if let Some((src_dir, src_name)) = src {
                match src_dir.lookup(src_name).await? {
                    Some((object_id, object_descriptor)) => match object_descriptor {
                        ObjectDescriptor::File
                        | ObjectDescriptor::Directory
                        | ObjectDescriptor::Symlink => {
                            if src_object_id != Some(object_id) {
                                have_required_locks = false;
                                src_object_id = Some(object_id);
                            }
                            src_id_and_descriptor = Some((object_id, object_descriptor));
                        }
                        _ => bail!(FxfsError::Inconsistent),
                    },
                    None => {
                        // Can't find src.0/src.1
                        bail!(FxfsError::NotFound)
                    }
                }
            };
            let dst_id_and_descriptor = match self.lookup(dst).await? {
                Some((object_id, object_descriptor)) => match object_descriptor {
                    ObjectDescriptor::File
                    | ObjectDescriptor::Directory
                    | ObjectDescriptor::Symlink => {
                        if child_object_id != object_id {
                            have_required_locks = false;
                            child_object_id = object_id
                        }
                        Some((object_id, object_descriptor))
                    }
                    _ => bail!(FxfsError::Inconsistent),
                },
                None => {
                    if child_object_id != INVALID_OBJECT_ID {
                        have_required_locks = false;
                        child_object_id = INVALID_OBJECT_ID;
                    }
                    None
                }
            };
            if have_required_locks {
                return Ok(ReplaceContext {
                    transaction,
                    src_id_and_descriptor,
                    dst_id_and_descriptor,
                });
            }
        }
    }

    async fn has_children(&self) -> Result<bool, Error> {
        if self.is_deleted() {
            return Ok(false);
        }
        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        Ok(self.iter(&mut merger).await?.get().is_some())
    }

    /// Returns the object ID and descriptor for the given child, or None if not found.
    #[trace]
    pub async fn lookup(&self, name: &str) -> Result<Option<(u64, ObjectDescriptor)>, Error> {
        if self.is_deleted() {
            return Ok(None);
        }
        match self.store().tree().find(&ObjectKey::child(self.object_id(), name)).await? {
            None | Some(ObjectItem { value: ObjectValue::None, .. }) => Ok(None),
            Some(ObjectItem {
                value: ObjectValue::Child(ChildValue { object_id, object_descriptor }),
                ..
            }) => Ok(Some((object_id, object_descriptor))),
            Some(item) => Err(anyhow!(FxfsError::Inconsistent)
                .context(format!("Unexpected item in lookup: {:?}", item))),
        }
    }

    pub async fn create_child_dir(
        &self,
        transaction: &mut Transaction<'_>,
        name: &str,
        create_attributes: Option<&fio::MutableNodeAttributes>,
    ) -> Result<Directory<S>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = Directory::create(transaction, self.owner(), create_attributes).await?;
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), name),
                ObjectValue::child(handle.object_id(), ObjectDescriptor::Directory),
            ),
        );
        let now = Timestamp::now();
        self.update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(now.as_nanos()),
                ..Default::default()
            }),
            1,
            Some(now),
        )
        .await?;
        self.copy_project_id_to_object_in_txn(transaction, handle.object_id())?;
        Ok(handle)
    }

    pub async fn add_child_file<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
        handle: &DataObjectHandle<S>,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), name),
                ObjectValue::child(handle.object_id(), ObjectDescriptor::File),
            ),
        );
        let now = Timestamp::now();
        self.update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(now.as_nanos()),
                ..Default::default()
            }),
            0,
            Some(now),
        )
        .await
    }

    // This applies the project id of this directory (if nonzero) to an object. The method assumes
    // both this and child objects are already present in the mutations of the provided
    // transactions and that the child is of of zero size. This is meant for use inside
    // `create_child_file()` and `create_child_dir()` only, where such assumptions are safe.
    fn copy_project_id_to_object_in_txn<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        object_id: u64,
    ) -> Result<(), Error> {
        let store_id = self.store().store_object_id();
        // This mutation must already be in here as we've just modified the mtime.
        let ObjectValue::Object { attributes: ObjectAttributes { project_id, .. }, .. } =
            transaction
                .get_object_mutation(store_id, ObjectKey::object(self.object_id()))
                .unwrap()
                .item
                .value
        else {
            return Err(anyhow!(FxfsError::Inconsistent));
        };
        if project_id > 0 {
            // This mutation must be present as well since we've just created the object. So this
            // replaces it.
            let mut mutation = transaction
                .get_object_mutation(store_id, ObjectKey::object(object_id))
                .unwrap()
                .clone();
            if let ObjectValue::Object {
                attributes: ObjectAttributes { project_id: child_project_id, .. },
                ..
            } = &mut mutation.item.value
            {
                *child_project_id = project_id;
            } else {
                return Err(anyhow!(FxfsError::Inconsistent));
            }
            transaction.add(store_id, Mutation::ObjectStore(mutation));
            transaction.add(
                store_id,
                Mutation::merge_object(
                    ObjectKey::project_usage(self.store().root_directory_object_id(), project_id),
                    ObjectValue::BytesAndNodes { bytes: 0, nodes: 1 },
                ),
            );
        }
        Ok(())
    }

    pub async fn create_child_file<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
        create_attributes: Option<&fio::MutableNodeAttributes>,
    ) -> Result<DataObjectHandle<S>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = ObjectStore::create_object(
            self.owner(),
            transaction,
            HandleOptions::default(),
            None,
            create_attributes,
        )
        .await?;
        self.add_child_file(transaction, name, &handle).await?;
        self.copy_project_id_to_object_in_txn(transaction, handle.object_id())?;
        Ok(handle)
    }

    pub async fn create_symlink(
        &self,
        transaction: &mut Transaction<'_>,
        link: &[u8],
        name: &str,
    ) -> Result<u64, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        // Limit the length of link that might be too big to put in the tree.
        // https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/limits.h.html.
        // See _POSIX_SYMLINK_MAX.
        ensure!(link.len() <= 256, FxfsError::BadPath);
        let symlink_id = self.store().get_next_object_id(transaction).await?;
        transaction.add(
            self.store().store_object_id(),
            Mutation::insert_object(
                ObjectKey::object(symlink_id),
                ObjectValue::symlink(link, Timestamp::now(), Timestamp::now(), 0),
            ),
        );
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), name),
                ObjectValue::child(symlink_id, ObjectDescriptor::Symlink),
            ),
        );
        self.update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(Timestamp::now().as_nanos()),
                ..Default::default()
            }),
            0,
            None,
        )
        .await?;
        Ok(symlink_id)
    }

    pub async fn add_child_volume(
        &self,
        transaction: &mut Transaction<'_>,
        volume_name: &str,
        store_object_id: u64,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), volume_name),
                ObjectValue::child(store_object_id, ObjectDescriptor::Volume),
            ),
        );
        let now = Timestamp::now();
        self.update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(now.as_nanos()),
                ..Default::default()
            }),
            0,
            Some(now),
        )
        .await
    }

    pub async fn delete_child_volume<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        volume_name: &str,
        store_object_id: u64,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), volume_name),
                ObjectValue::None,
            ),
        );
        // We note in the journal that we've deleted the volume. ObjectManager applies this
        // mutation by forgetting the store. We do it this way to ensure that the store is removed
        // during replay where there may be mutations to the store prior to its deletion. Without
        // this, we will try (and fail) to open the store after replay.
        transaction.add(store_object_id, Mutation::DeleteVolume);
        Ok(())
    }

    /// Inserts a child into the directory.
    ///
    /// Requires transaction locks on |self|.
    pub async fn insert_child<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
        object_id: u64,
        descriptor: ObjectDescriptor,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let sub_dirs_delta = if descriptor == ObjectDescriptor::Directory { 1 } else { 0 };
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id(), name),
                ObjectValue::child(object_id, descriptor),
            ),
        );
        let now = Timestamp::now();
        self.update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(now.as_nanos()),
                ..Default::default()
            }),
            sub_dirs_delta,
            Some(now),
        )
        .await
    }

    /// Updates attributes for the directory.
    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        node_attributes: Option<&fio::MutableNodeAttributes>,
        sub_dirs_delta: i64,
        change_time: Option<Timestamp>,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        if sub_dirs_delta != 0 {
            let mut mutation =
                self.store().txn_get_object_mutation(&transaction, self.object_id()).await?;
            if let ObjectValue::Object { kind: ObjectKind::Directory { sub_dirs }, .. } =
                &mut mutation.item.value
            {
                *sub_dirs = sub_dirs.saturating_add_signed(sub_dirs_delta);
            } else {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("Directory.update_attributes: expected directory object"));
            };
            transaction.add(self.store().store_object_id(), Mutation::ObjectStore(mutation));
        }

        // Delegate to the StoreObjectHandle update_attributes for the rest of the updates.
        if node_attributes.is_some() || change_time.is_some() {
            self.handle.update_attributes(transaction, node_attributes, change_time).await?;
        }
        Ok(())
    }

    pub async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        let item = self
            .store()
            .tree()
            .find(&ObjectKey::object(self.object_id()))
            .await?
            .ok_or(FxfsError::NotFound)?;
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::Directory { sub_dirs },
                attributes:
                    ObjectAttributes {
                        creation_time,
                        modification_time,
                        posix_attributes,
                        access_time,
                        change_time,
                        ..
                    },
            } => Ok(ObjectProperties {
                refs: 1,
                allocated_size: 0,
                data_attribute_size: 0,
                creation_time,
                modification_time,
                access_time,
                change_time,
                sub_dirs,
                posix_attributes,
            }),
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_properties: Expected object value"))
            }
        }
    }

    pub async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        self.handle.list_extended_attributes().await
    }

    pub async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        self.handle.get_extended_attribute(name).await
    }

    pub async fn set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: SetExtendedAttributeMode,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        self.handle.set_extended_attribute(name, value, mode).await
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        self.handle.remove_extended_attribute(name).await
    }

    /// Returns an iterator that will return directory entries skipping deleted ones.  Example
    /// usage:
    ///
    ///   let layer_set = dir.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = dir.iter(&mut merger).await?;
    ///
    pub async fn iter<'a, 'b>(
        &self,
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
    ) -> Result<DirectoryIterator<'a, 'b>, Error> {
        self.iter_from(merger, "").await
    }

    /// Like "iter", but seeks from a specific filename (inclusive).  Example usage:
    ///
    ///   let layer_set = dir.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = dir.iter_from(&mut merger, "foo").await?;
    ///
    pub async fn iter_from<'a, 'b>(
        &self,
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
        from: &str,
    ) -> Result<DirectoryIterator<'a, 'b>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let mut iter =
            merger.seek(Bound::Included(&ObjectKey::child(self.object_id(), from))).await?;
        // Skip deleted entries.
        loop {
            match iter.get() {
                Some(ItemRef {
                    key: ObjectKey { object_id, .. },
                    value: ObjectValue::None,
                    ..
                }) if *object_id == self.object_id() => {}
                _ => break,
            }
            iter.advance().await?;
        }
        Ok(DirectoryIterator { object_id: self.object_id(), iter })
    }
}

impl<S: HandleOwner> fmt::Debug for Directory<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory")
            .field("store_id", &self.store().store_object_id())
            .field("object_id", &self.object_id())
            .finish()
    }
}

pub struct DirectoryIterator<'a, 'b> {
    object_id: u64,
    iter: MergerIterator<'a, 'b, ObjectKey, ObjectValue>,
}

impl DirectoryIterator<'_, '_> {
    pub fn get(&self) -> Option<(&str, u64, &ObjectDescriptor)> {
        match self.iter.get() {
            Some(ItemRef {
                key: ObjectKey { object_id: oid, data: ObjectKeyData::Child { name } },
                value: ObjectValue::Child(ChildValue { object_id, object_descriptor }),
                ..
            }) if *oid == self.object_id => Some((name, *object_id, object_descriptor)),
            _ => None,
        }
    }

    pub async fn advance(&mut self) -> Result<(), Error> {
        loop {
            self.iter.advance().await?;
            // Skip deleted entries.
            match self.iter.get() {
                Some(ItemRef {
                    key: ObjectKey { object_id, .. },
                    value: ObjectValue::None,
                    ..
                }) if *object_id == self.object_id => {}
                _ => return Ok(()),
            }
        }
    }
}

/// Return type for |replace_child| describing the object which was replaced. The u64 fields are all
/// object_ids.
#[derive(Debug)]
pub enum ReplacedChild {
    None,
    // "Object" can be a file or symbolic link, but not a directory.
    Object(u64),
    ObjectWithRemainingLinks(u64),
    Directory(u64),
}

/// Moves src.0/src.1 to dst.0/dst.1.
///
/// If |dst.0| already has a child |dst.1|, it is removed from dst.0.  For files, if this was their
/// last reference, the file is moved to the graveyard.  For directories, the removed directory will
/// be deleted permanently (and must be empty).
///
/// If |src| is None, this is effectively the same as unlink(dst.0/dst.1).
pub async fn replace_child<'a, S: HandleOwner>(
    transaction: &mut Transaction<'a>,
    src: Option<(&'a Directory<S>, &str)>,
    dst: (&'a Directory<S>, &str),
) -> Result<ReplacedChild, Error> {
    let mut sub_dirs_delta: i64 = 0;
    let now = Timestamp::now();

    let src = if let Some((src_dir, src_name)) = src {
        let store_id = dst.0.store().store_object_id();
        assert_eq!(store_id, src_dir.store().store_object_id());
        transaction.add(
            store_id,
            Mutation::replace_or_insert_object(
                ObjectKey::child(src_dir.object_id(), src_name),
                ObjectValue::None,
            ),
        );
        let (id, descriptor) = src_dir.lookup(src_name).await?.ok_or(FxfsError::NotFound)?;
        src_dir.store().update_attributes(transaction, id, None, Some(now)).await?;
        if src_dir.object_id() != dst.0.object_id() {
            sub_dirs_delta = if descriptor == ObjectDescriptor::Directory { 1 } else { 0 };
            src_dir
                .update_attributes(
                    transaction,
                    Some(&fio::MutableNodeAttributes {
                        modification_time: Some(now.as_nanos()),
                        ..Default::default()
                    }),
                    -sub_dirs_delta,
                    Some(now),
                )
                .await?;
        }
        Some((id, descriptor))
    } else {
        None
    };
    replace_child_with_object(transaction, src, dst, sub_dirs_delta, now).await
}

/// Replaces dst.0/dst.1 with the given object, or unlinks if `src` is None.
///
/// If |dst.0| already has a child |dst.1|, it is removed from dst.0.  For files, if this was their
/// last reference, the file is moved to the graveyard.  For directories, the removed directory will
/// be deleted permanently (and must be empty).
///
/// `sub_dirs_delta` can be used if `src` is a directory and happened to already be a child of
/// `dst`.
pub async fn replace_child_with_object<'a, S: HandleOwner>(
    transaction: &mut Transaction<'a>,
    src: Option<(u64, ObjectDescriptor)>,
    dst: (&'a Directory<S>, &str),
    mut sub_dirs_delta: i64,
    timestamp: Timestamp,
) -> Result<ReplacedChild, Error> {
    let deleted_id_and_descriptor = dst.0.lookup(dst.1).await?;
    let store_id = dst.0.store().store_object_id();
    let result = match deleted_id_and_descriptor {
        Some((old_id, ObjectDescriptor::File | ObjectDescriptor::Symlink)) => {
            let was_last_ref = dst.0.store().adjust_refs(transaction, old_id, -1).await?;
            dst.0.store().update_attributes(transaction, old_id, None, Some(timestamp)).await?;
            if was_last_ref {
                ReplacedChild::Object(old_id)
            } else {
                ReplacedChild::ObjectWithRemainingLinks(old_id)
            }
        }
        Some((old_id, ObjectDescriptor::Directory)) => {
            let dir = Directory::open(&dst.0.owner(), old_id).await?;
            if dir.has_children().await? {
                bail!(FxfsError::NotEmpty);
            }
            dst.0.store().add_to_graveyard(transaction, old_id);
            dst.0.store().filesystem().graveyard().queue_tombstone(store_id, old_id);
            sub_dirs_delta -= 1;
            ReplacedChild::Directory(old_id)
        }
        Some((_, ObjectDescriptor::Volume)) => {
            bail!(anyhow!(FxfsError::Inconsistent).context("Unexpected volume child"))
        }
        None => {
            if src.is_none() {
                // Neither src nor dst exist
                bail!(FxfsError::NotFound);
            }
            ReplacedChild::None
        }
    };
    let new_value = match src {
        Some((id, descriptor)) => ObjectValue::child(id, descriptor),
        None => ObjectValue::None,
    };
    transaction.add(
        store_id,
        Mutation::replace_or_insert_object(ObjectKey::child(dst.0.object_id(), dst.1), new_value),
    );
    dst.0
        .update_attributes(
            transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(timestamp.as_nanos()),
                ..Default::default()
            }),
            sub_dirs_delta,
            Some(timestamp),
        )
        .await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use crate::object_store::StoreObjectHandle;

    use {
        crate::{
            errors::FxfsError,
            filesystem::{FxFilesystem, JournalingObject, SyncOptions},
            object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
            object_store::{
                directory::{replace_child, Directory, ReplacedChild},
                object_record::Timestamp,
                transaction::{lock_keys, Options},
                volume::root_volume,
                HandleOptions, LockKey, ObjectDescriptor, ObjectStore, SetExtendedAttributeMode,
            },
        },
        assert_matches::assert_matches,
        fidl_fuchsia_io as fio,
        fxfs_insecure_crypto::InsecureCrypt,
        std::sync::Arc,
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    #[fuchsia::test]
    async fn test_create_directory() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let object_id = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            let dir = Directory::create(&mut transaction, &fs.root_store(), None)
                .await
                .expect("create failed");

            let child_dir = dir
                .create_child_dir(&mut transaction, "foo", None)
                .await
                .expect("create_child_dir failed");
            let _child_dir_file = child_dir
                .create_child_file(&mut transaction, "bar", None)
                .await
                .expect("create_child_file failed");
            let _child_file = dir
                .create_child_file(&mut transaction, "baz", None)
                .await
                .expect("create_child_file failed");
            dir.add_child_volume(&mut transaction, "corge", 100)
                .await
                .expect("add_child_volume failed");
            transaction.commit().await.expect("commit failed");
            fs.sync(SyncOptions::default()).await.expect("sync failed");
            dir.object_id()
        };
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        {
            let dir = Directory::open(&fs.root_store(), object_id).await.expect("open failed");
            let (object_id, object_descriptor) =
                dir.lookup("foo").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::Directory);
            let child_dir =
                Directory::open(&fs.root_store(), object_id).await.expect("open failed");
            let (object_id, object_descriptor) =
                child_dir.lookup("bar").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::File);
            let _child_dir_file = ObjectStore::open_object(
                &fs.root_store(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open object failed");
            let (object_id, object_descriptor) =
                dir.lookup("baz").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::File);
            let _child_file = ObjectStore::open_object(
                &fs.root_store(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open object failed");
            let (object_id, object_descriptor) =
                dir.lookup("corge").await.expect("lookup failed").expect("not found");
            assert_eq!(object_id, 100);
            if let ObjectDescriptor::Volume = object_descriptor {
            } else {
                panic!("wrong ObjectDescriptor");
            }

            assert_eq!(dir.lookup("qux").await.expect("lookup failed"), None);
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child = dir
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child_with_children_fails() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child;
        let bar;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child = dir
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        bar = child
            .create_child_file(&mut transaction, "bar", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect_err("replace_child succeeded")
                .downcast::<FxfsError>()
                .expect("wrong error"),
            FxfsError::NotEmpty
        );
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), bar.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&child, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(..)
        );
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Directory(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_and_reinsert_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child = dir
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(..)
        );
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        dir.create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        dir.lookup("foo").await.expect("lookup failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child_persists() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let object_id = {
            let dir;
            let child;
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            dir = Directory::create(&mut transaction, &fs.root_store(), None)
                .await
                .expect("create failed");

            child = dir
                .create_child_file(&mut transaction, "foo", None)
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            dir.lookup("foo").await.expect("lookup failed");

            transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![
                        LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                        LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                    ],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            assert_matches!(
                replace_child(&mut transaction, None, (&dir, "foo"))
                    .await
                    .expect("replace_child failed"),
                ReplacedChild::Object(..)
            );
            transaction.commit().await.expect("commit failed");

            fs.sync(SyncOptions::default()).await.expect("sync failed");
            dir.object_id()
        };

        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        let dir = Directory::open(&fs.root_store(), object_id).await.expect("open failed");
        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", None)
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", None)
            .await
            .expect("create_child_dir failed");
        let file = child_dir1
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), file.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir1.lookup("foo").await.expect("lookup failed"), None);
        child_dir2.lookup("bar").await.expect("lookup failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_overwrites_dst() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", None)
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", None)
            .await
            .expect("create_child_dir failed");
        let foo = child_dir1
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        let bar = child_dir2
            .create_child_file(&mut transaction, "bar", None)
            .await
            .expect("create_child_file failed");
        let foo_oid = foo.object_id();
        let bar_oid = bar.object_id();
        transaction.commit().await.expect("commit failed");

        {
            let mut buf = foo.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize).await;
            buf.as_mut_slice().fill(0xaa);
            foo.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            buf.as_mut_slice().fill(0xbb);
            bar.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        }
        std::mem::drop(bar);
        std::mem::drop(foo);

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), foo_oid),
                    LockKey::object(fs.root_store().store_object_id(), bar_oid),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir1.lookup("foo").await.expect("lookup failed"), None);

        // Check the contents to ensure that the file was replaced.
        let (oid, object_descriptor) =
            child_dir2.lookup("bar").await.expect("lookup failed").expect("not found");
        assert_eq!(object_descriptor, ObjectDescriptor::File);
        let bar =
            ObjectStore::open_object(&child_dir2.owner(), oid, HandleOptions::default(), None)
                .await
                .expect("Open failed");
        let mut buf = bar.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize).await;
        bar.read(0, buf.as_mut()).await.expect("read failed");
        assert_eq!(buf.as_slice(), vec![0xaa; TEST_DEVICE_BLOCK_SIZE as usize]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_fails_if_would_overwrite_nonempty_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", None)
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", None)
            .await
            .expect("create_child_dir failed");
        let foo = child_dir1
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        let nested_child = child_dir2
            .create_child_dir(&mut transaction, "bar", None)
            .await
            .expect("create_child_file failed");
        nested_child
            .create_child_file(&mut transaction, "baz", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), foo.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), nested_child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect_err("replace_child succeeded")
                .downcast::<FxfsError>()
                .expect("wrong error"),
            FxfsError::NotEmpty
        );
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_within_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");
        let foo = dir
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), foo.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&dir, "foo")), (&dir, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        dir.lookup("bar").await.expect("lookup new name failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_iterate() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");
        let _cat = dir
            .create_child_file(&mut transaction, "cat", None)
            .await
            .expect("create_child_file failed");
        let _ball = dir
            .create_child_file(&mut transaction, "ball", None)
            .await
            .expect("create_child_file failed");
        let apple = dir
            .create_child_file(&mut transaction, "apple", None)
            .await
            .expect("create_child_file failed");
        let _dog = dir
            .create_child_file(&mut transaction, "dog", None)
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), apple.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "apple")).await.expect("replace_child failed");
        transaction.commit().await.expect("commit failed");
        let layer_set = dir.store().tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter = dir.iter(&mut merger).await.expect("iter failed");
        let mut entries = Vec::new();
        while let Some((name, _, _)) = iter.get() {
            entries.push(name.to_string());
            iter.advance().await.expect("advance failed");
        }
        assert_eq!(&entries, &["ball", "cat", "dog"]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_sub_dir_count() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");
        child_dir = dir
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");
        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        // Moving within the same directory should not change the sub_dir count.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&dir, "foo")), (&dir, "bar"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);
        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);

        // Moving between two different directories should update source and destination.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    fs.root_store().store_object_id(),
                    child_dir.object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let second_child = child_dir
            .create_child_dir(&mut transaction, "baz", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), second_child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&child_dir, "baz")), (&dir, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 2);
        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);

        // Moving over a directory.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), second_child.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&dir, "bar")), (&dir, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        // Unlinking a directory.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "foo")).await.expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_deleted_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), None)
            .await
            .expect("create failed");
        let child = dir
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        dir.create_child_dir(&mut transaction, "bar", None).await.expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        // Flush the tree so that we end up with records in different layers.
        dir.store().flush().await.expect("flush failed");

        // Unlink the child directory.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "foo")).await.expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        // Finding the child should fail now.
        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);

        // But finding "bar" should succeed.
        assert!(dir.lookup("bar").await.expect("lookup failed").is_some());

        // If we mark dir as deleted, any further operations should fail.
        dir.set_deleted();

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        assert_eq!(dir.lookup("bar").await.expect("lookup failed"), None);
        assert!(!dir.has_children().await.expect("has_children failed"));

        transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let assert_access_denied = |result| {
            if let Err(e) = result {
                assert!(FxfsError::Deleted.matches(&e));
            } else {
                panic!();
            }
        };
        assert_access_denied(dir.create_child_dir(&mut transaction, "baz", None).await.map(|_| {}));
        assert_access_denied(
            dir.create_child_file(&mut transaction, "baz", None).await.map(|_| {}),
        );
        assert_access_denied(dir.add_child_volume(&mut transaction, "baz", 1).await);
        assert_access_denied(
            dir.insert_child(&mut transaction, "baz", 1, ObjectDescriptor::File).await,
        );
        assert_access_denied(
            dir.update_attributes(
                &mut transaction,
                Some(&fio::MutableNodeAttributes {
                    creation_time: Some(Timestamp::zero().as_nanos()),
                    ..Default::default()
                }),
                0,
                None,
            )
            .await,
        );
        let layer_set = dir.store().tree().layer_set();
        let mut merger = layer_set.merger();
        assert_access_denied(dir.iter(&mut merger).await.map(|_| {}));
    }

    #[fuchsia::test]
    async fn test_create_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let (dir_id, symlink_id) = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            let dir = Directory::create(&mut transaction, &fs.root_store(), None)
                .await
                .expect("create failed");

            let symlink_id = dir
                .create_symlink(&mut transaction, b"link", "foo")
                .await
                .expect("create_symlink failed");
            transaction.commit().await.expect("commit failed");

            fs.sync(SyncOptions::default()).await.expect("sync failed");
            (dir.object_id(), symlink_id)
        };
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        {
            let dir = Directory::open(&fs.root_store(), dir_id).await.expect("open failed");
            assert_eq!(
                dir.lookup("foo").await.expect("lookup failed").expect("not found"),
                (symlink_id, ObjectDescriptor::Symlink)
            );
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let dir = Directory::create(&mut transaction, &store, None).await.expect("create failed");

        let symlink_id = dir
            .create_symlink(&mut transaction, b"link", "foo")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");

        let link = store.read_symlink(symlink_id).await.expect("read_symlink failed");
        assert_eq!(&link, b"link");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_unlink_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        dir = Directory::create(&mut transaction, &store, None).await.expect("create failed");

        let symlink_id = dir
            .create_symlink(&mut transaction, b"link", "foo")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), dir.object_id()),
                    LockKey::object(store.store_object_id(), symlink_id),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(_)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_get_properties() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let create_attributes = fio::MutableNodeAttributes::default();
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        transaction.commit().await.expect("commit failed");

        // Check attributes of `dir`
        let mut properties = dir.get_properties().await.expect("get_properties failed");
        let dir_creation_time = properties.creation_time;
        assert_eq!(dir_creation_time, properties.modification_time);
        assert_eq!(properties.sub_dirs, 0);
        assert!(properties.posix_attributes.is_none());

        // Create child directory
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let child_dir = dir
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        // Check attributes of `dir` after adding child directory
        properties = dir.get_properties().await.expect("get_properties failed");
        // The modification time property should have updated
        assert_eq!(dir_creation_time, properties.creation_time);
        assert!(dir_creation_time < properties.modification_time);
        assert_eq!(properties.sub_dirs, 1);
        assert!(properties.posix_attributes.is_none());

        // Check attributes of `child_dir`
        properties = child_dir.get_properties().await.expect("get_properties failed");
        assert_eq!(properties.creation_time, properties.modification_time);
        assert_eq!(properties.sub_dirs, 0);
        assert!(properties.posix_attributes.is_none());

        // Create child file with MutableAttributes
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    fs.root_store().store_object_id(),
                    child_dir.object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let child_dir_file = child_dir
            .create_child_file(
                &mut transaction,
                "bar",
                Some(&fio::MutableNodeAttributes { gid: Some(1), ..Default::default() }),
            )
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        // The modification time property of `child_dir` should have updated
        properties = child_dir.get_properties().await.expect("get_properties failed");
        assert!(properties.creation_time < properties.modification_time);
        assert!(properties.posix_attributes.is_none());

        // Check attributes of `child_dir_file`
        properties = child_dir_file.get_properties().await.expect("get_properties failed");
        assert_eq!(properties.creation_time, properties.modification_time);
        assert_eq!(properties.sub_dirs, 0);
        assert!(properties.posix_attributes.is_some());
        assert_eq!(properties.posix_attributes.unwrap().gid, 1);
        // The other POSIX attributes should be set to default values
        assert_eq!(properties.posix_attributes.unwrap().uid, 0);
        assert_eq!(properties.posix_attributes.unwrap().mode, 0);
        assert_eq!(properties.posix_attributes.unwrap().rdev, 0);
    }

    #[fuchsia::test]
    async fn test_update_create_attributes() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let create_attributes = fio::MutableNodeAttributes::default();
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        transaction.commit().await.expect("commit failed");
        let mut properties = dir.get_properties().await.expect("get_properties failed");
        assert_eq!(properties.sub_dirs, 0);
        assert!(properties.posix_attributes.is_none());
        let creation_time = properties.creation_time;
        let modification_time = properties.modification_time;
        assert_eq!(creation_time, modification_time);

        // First update: test that
        // 1. updating attributes with a POSIX attribute will assign some PosixAttributes to the
        //    Object associated with `dir`,
        // 2. creation/modification time are only updated if specified in the update,
        // 3. any changes will not overwrite other attributes.
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let now = Timestamp::now();
        dir.update_attributes(
            &mut transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(now.as_nanos()),
                uid: Some(1),
                gid: Some(2),
                ..Default::default()
            }),
            0,
            None,
        )
        .await
        .expect("update_attributes failed");
        transaction.commit().await.expect("commit failed");
        properties = dir.get_properties().await.expect("get_properties failed");
        // Check that the properties reflect the updates
        assert_eq!(properties.modification_time, now);
        assert!(properties.posix_attributes.is_some());
        assert_eq!(properties.posix_attributes.unwrap().uid, 1);
        assert_eq!(properties.posix_attributes.unwrap().gid, 2);
        // The other POSIX attributes should be set to default values
        assert_eq!(properties.posix_attributes.unwrap().mode, 0);
        assert_eq!(properties.posix_attributes.unwrap().rdev, 0);
        // The remaining properties should not have changed
        assert_eq!(properties.sub_dirs, 0);
        assert_eq!(properties.creation_time, creation_time);

        // Second update: test that we can update attributes and that any changes will not overwrite
        // other attributes
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        dir.update_attributes(
            &mut transaction,
            Some(&fio::MutableNodeAttributes {
                creation_time: Some(now.as_nanos()),
                uid: Some(3),
                rdev: Some(10),
                ..Default::default()
            }),
            0,
            None,
        )
        .await
        .expect("update_attributes failed");
        transaction.commit().await.expect("commit failed");
        properties = dir.get_properties().await.expect("get_properties failed");
        assert_eq!(properties.creation_time, now);
        assert!(properties.posix_attributes.is_some());
        assert_eq!(properties.posix_attributes.unwrap().uid, 3);
        assert_eq!(properties.posix_attributes.unwrap().rdev, 10);
        // The other properties should not have changed
        assert_eq!(properties.sub_dirs, 0);
        assert_eq!(properties.modification_time, now);
        assert_eq!(properties.posix_attributes.unwrap().gid, 2);
        assert_eq!(properties.posix_attributes.unwrap().mode, 0);
    }

    #[fuchsia::test]
    async fn write_to_directory_attribute_creates_keys() {
        let device = DeviceHolder::new(FakeDevice::new(16384, 512));
        let filesystem = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let crypt = Arc::new(InsecureCrypt::new());

        {
            let root_volume = root_volume(filesystem.clone()).await.expect("root_volume failed");
            let store = root_volume
                .new_volume("vol", Some(crypt.clone()))
                .await
                .expect("new_volume failed");
            let mut transaction = filesystem
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        store.store_object_id(),
                        store.root_directory_object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new transaction failed");
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");
            let directory = root_directory
                .create_child_dir(&mut transaction, "foo", None)
                .await
                .expect("create_child_dir failed");
            transaction.commit().await.expect("commit failed");

            let mut transaction = filesystem
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(store.store_object_id(), directory.object_id())],
                    Options::default(),
                )
                .await
                .expect("new transaction failed");
            let _ = directory
                .handle
                .write_attr(&mut transaction, 1, b"bar")
                .await
                .expect("write_attr failed");
            transaction.commit().await.expect("commit failed");
        }

        filesystem.close().await.expect("Close failed");
        let device = filesystem.take_device().await;
        device.reopen(false);
        let filesystem = FxFilesystem::open(device).await.expect("open failed");

        {
            let root_volume = root_volume(filesystem.clone()).await.expect("root_volume failed");
            let volume = root_volume.volume("vol", Some(crypt)).await.expect("volume failed");
            let root_directory = Directory::open(&volume, volume.root_directory_object_id())
                .await
                .expect("open failed");
            let directory = Directory::open(
                &volume,
                root_directory.lookup("foo").await.expect("lookup failed").expect("not found").0,
            )
            .await
            .expect("open failed");
            let mut buffer = directory.handle.allocate_buffer(10).await;
            assert_eq!(directory.handle.read(1, 0, buffer.as_mut()).await.expect("read failed"), 3);
            assert_eq!(&buffer.as_slice()[..3], b"bar");
        }

        filesystem.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn directory_with_extended_attributes() {
        let device = DeviceHolder::new(FakeDevice::new(16384, 512));
        let filesystem = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let crypt = Arc::new(InsecureCrypt::new());

        let root_volume = root_volume(filesystem.clone()).await.expect("root_volume failed");
        let store =
            root_volume.new_volume("vol", Some(crypt.clone())).await.expect("new_volume failed");
        let directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let test_small_name = b"security.selinux".to_vec();
        let test_small_value = b"foo".to_vec();
        let test_large_name = b"large.attribute".to_vec();
        let test_large_value = vec![1u8; 500];

        directory
            .set_extended_attribute(
                test_small_name.clone(),
                test_small_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            directory.get_extended_attribute(test_small_name.clone()).await.unwrap(),
            test_small_value
        );

        directory
            .set_extended_attribute(
                test_large_name.clone(),
                test_large_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            directory.get_extended_attribute(test_large_name.clone()).await.unwrap(),
            test_large_value
        );

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        directory.remove_extended_attribute(test_small_name.clone()).await.unwrap();
        directory.remove_extended_attribute(test_large_name.clone()).await.unwrap();

        filesystem.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn remove_directory_with_extended_attributes() {
        let device = DeviceHolder::new(FakeDevice::new(16384, 512));
        let filesystem = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let crypt = Arc::new(InsecureCrypt::new());

        let root_volume = root_volume(filesystem.clone()).await.expect("root_volume failed");
        let store =
            root_volume.new_volume("vol", Some(crypt.clone())).await.expect("new_volume failed");
        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    store.store_object_id(),
                    store.root_directory_object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new transaction failed");
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let directory = root_directory
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        let test_small_name = b"security.selinux".to_vec();
        let test_small_value = b"foo".to_vec();
        let test_large_name = b"large.attribute".to_vec();
        let test_large_value = vec![1u8; 500];

        directory
            .set_extended_attribute(
                test_small_name.clone(),
                test_small_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        directory
            .set_extended_attribute(
                test_large_name.clone(),
                test_large_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), root_directory.object_id()),
                    LockKey::object(store.store_object_id(), directory.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&root_directory, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.unwrap();

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        filesystem.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn remove_symlink_with_extended_attributes() {
        let device = DeviceHolder::new(FakeDevice::new(16384, 512));
        let filesystem = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let crypt = Arc::new(InsecureCrypt::new());

        let root_volume = root_volume(filesystem.clone()).await.expect("root_volume failed");
        let store =
            root_volume.new_volume("vol", Some(crypt.clone())).await.expect("new_volume failed");
        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    store.store_object_id(),
                    store.root_directory_object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new transaction failed");
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let symlink_id = root_directory
            .create_symlink(&mut transaction, b"somewhere/else", "foo")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");

        let symlink = StoreObjectHandle::new(
            store.clone(),
            symlink_id,
            false,
            HandleOptions::default(),
            false,
        );

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        let test_small_name = b"security.selinux".to_vec();
        let test_small_value = b"foo".to_vec();
        let test_large_name = b"large.attribute".to_vec();
        let test_large_value = vec![1u8; 500];

        symlink
            .set_extended_attribute(
                test_small_name.clone(),
                test_small_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        symlink
            .set_extended_attribute(
                test_large_name.clone(),
                test_large_value.clone(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), root_directory.object_id()),
                    LockKey::object(store.store_object_id(), symlink.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&root_directory, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.unwrap();

        crate::fsck::fsck(filesystem.clone()).await.unwrap();
        crate::fsck::fsck_volume(filesystem.as_ref(), store.store_object_id(), Some(crypt.clone()))
            .await
            .unwrap();

        filesystem.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn test_update_timestamps() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let create_attributes = fio::MutableNodeAttributes::default();
        // Expect that atime, ctime, mtime (and creation time) to be the same when we create a
        // directory
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        transaction.commit().await.expect("commit failed");
        let mut properties = dir.get_properties().await.expect("get_properties failed");
        let starting_time = properties.creation_time;
        assert_eq!(properties.creation_time, starting_time);
        assert_eq!(properties.modification_time, starting_time);
        assert_eq!(properties.change_time, starting_time);
        assert_eq!(properties.access_time, starting_time);

        // Test that we can update the timestamps
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let update1_time = Timestamp::now();
        dir.update_attributes(
            &mut transaction,
            Some(&fio::MutableNodeAttributes {
                modification_time: Some(update1_time.as_nanos()),
                ..Default::default()
            }),
            0,
            Some(update1_time),
        )
        .await
        .expect("update_attributes failed");
        transaction.commit().await.expect("commit failed");
        properties = dir.get_properties().await.expect("get_properties failed");
        assert_eq!(properties.modification_time, update1_time);
        assert_eq!(properties.access_time, starting_time);
        assert_eq!(properties.creation_time, starting_time);
        assert_eq!(properties.change_time, update1_time);
    }

    #[fuchsia::test]
    async fn test_move_dir_timestamps() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child1;
        let child2;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let create_attributes = fio::MutableNodeAttributes::default();
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        child1 = dir
            .create_child_dir(&mut transaction, "child1", None)
            .await
            .expect("create_child_dir failed");
        child2 = dir
            .create_child_dir(&mut transaction, "child2", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");
        let dir_properties = dir.get_properties().await.expect("get_properties failed");
        let child2_properties = child2.get_properties().await.expect("get_properties failed");

        // Move dir/child2 to dir/child1/child2
        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child2.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&dir, "child2")), (&child1, "child2"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");
        // Both mtime and ctime for dir should be updated
        let new_dir_properties = dir.get_properties().await.expect("get_properties failed");
        let time_of_replacement = new_dir_properties.change_time;
        assert!(new_dir_properties.change_time > dir_properties.change_time);
        assert_eq!(new_dir_properties.modification_time, time_of_replacement);
        // Both mtime and ctime for child1 should be updated
        let new_child1_properties = child1.get_properties().await.expect("get_properties failed");
        assert_eq!(new_child1_properties.modification_time, time_of_replacement);
        assert_eq!(new_child1_properties.change_time, time_of_replacement);
        // Only ctime for child2 should be updated
        let moved_child2_properties = child2.get_properties().await.expect("get_properties failed");
        assert_eq!(moved_child2_properties.change_time, time_of_replacement);
        assert_eq!(moved_child2_properties.creation_time, child2_properties.creation_time);
        assert_eq!(moved_child2_properties.access_time, child2_properties.access_time);
        assert_eq!(moved_child2_properties.modification_time, child2_properties.modification_time);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_unlink_timestamps() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let foo;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let create_attributes = fio::MutableNodeAttributes::default();
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        foo = dir
            .create_child_file(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");

        transaction.commit().await.expect("commit failed");
        let dir_properties = dir.get_properties().await.expect("get_properties failed");
        let foo_properties = foo.get_properties().await.expect("get_properties failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), foo.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Object(_)
        );
        transaction.commit().await.expect("commit failed");
        // Both mtime and ctime for dir should be updated
        let new_dir_properties = dir.get_properties().await.expect("get_properties failed");
        let time_of_replacement = new_dir_properties.change_time;
        assert!(new_dir_properties.change_time > dir_properties.change_time);
        assert_eq!(new_dir_properties.modification_time, time_of_replacement);
        // Only ctime for foo should be updated
        let moved_foo_properties = foo.get_properties().await.expect("get_properties failed");
        assert_eq!(moved_foo_properties.change_time, time_of_replacement);
        assert_eq!(moved_foo_properties.creation_time, foo_properties.creation_time);
        assert_eq!(moved_foo_properties.access_time, foo_properties.access_time);
        assert_eq!(moved_foo_properties.modification_time, foo_properties.modification_time);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_dir_timestamps() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let foo;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let create_attributes = fio::MutableNodeAttributes::default();
        dir = Directory::create(&mut transaction, &fs.root_store(), Some(&create_attributes))
            .await
            .expect("create failed");
        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", None)
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", None)
            .await
            .expect("create_child_dir failed");
        foo = child_dir1
            .create_child_dir(&mut transaction, "foo", None)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");
        let dir_props = dir.get_properties().await.expect("get_properties failed");
        let foo_props = foo.get_properties().await.expect("get_properties failed");

        transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), foo.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&dir, "dir2"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Directory(_)
        );
        transaction.commit().await.expect("commit failed");
        // Both mtime and ctime for dir should be updated
        let new_dir_props = dir.get_properties().await.expect("get_properties failed");
        let time_of_replacement = new_dir_props.change_time;
        assert!(new_dir_props.change_time > dir_props.change_time);
        assert_eq!(new_dir_props.modification_time, time_of_replacement);
        // Both mtime and ctime for dir1 should be updated
        let new_dir1_props = child_dir1.get_properties().await.expect("get_properties failed");
        let time_of_replacement = new_dir1_props.change_time;
        assert_eq!(new_dir1_props.change_time, time_of_replacement);
        assert_eq!(new_dir1_props.modification_time, time_of_replacement);
        // Only ctime for foo should be updated
        let moved_foo_props = foo.get_properties().await.expect("get_properties failed");
        assert_eq!(moved_foo_props.change_time, time_of_replacement);
        assert_eq!(moved_foo_props.creation_time, foo_props.creation_time);
        assert_eq!(moved_foo_props.access_time, foo_props.access_time);
        assert_eq!(moved_foo_props.modification_time, foo_props.modification_time);
        fs.close().await.expect("Close failed");
    }
}
