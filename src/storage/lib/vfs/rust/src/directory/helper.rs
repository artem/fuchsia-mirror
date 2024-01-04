// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        directory::{
            entry::DirectoryEntry,
            entry_container::{Directory, MutableDirectory},
        },
        name::Name,
        path::Path,
    },
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::sync::Arc,
    thiserror::Error,
};

/// An entry with the same name already exists in the directory.
#[derive(Error, Debug)]
#[error("An entry with the same name already exists in the directory")]
pub struct AlreadyExists;

impl Into<Status> for AlreadyExists {
    fn into(self) -> Status {
        Status::ALREADY_EXISTS
    }
}

/// The entry identified by `name` is not a directory.
#[derive(Error, Debug)]
#[error("The specified entry is not a directory")]
pub struct NotDirectory;

impl Into<Status> for NotDirectory {
    fn into(self) -> Status {
        Status::NOT_DIR
    }
}

/// `DirectlyMutable` is a superset of `MutableDirectory` which also allows server-side management
/// of directory entries (via `add_entry` and `remove_entry`). It also provides `rename_from` and
/// `rename_to` to support global ordering in rename operations. You may wish to use
/// `filesystem::simple::SimpleFilesystem` to provide the filesystem type for this DirectlyMutable.
pub trait DirectlyMutable: Directory + Send + Sync {
    /// Adds a child entry to this directory.
    ///
    /// Possible errors are:
    ///   * `ZX_ERR_INVALID_ARGS` or `ZX_ERR_BAD_PATH` if `name` is not a valid [`Name`].
    ///   * `ZX_ERR_ALREADY_EXISTS` if an entry with the same name is already present in the
    ///     directory.
    fn add_entry<NameT>(&self, name: NameT, entry: Arc<dyn DirectoryEntry>) -> Result<(), Status>
    where
        NameT: Into<String>,
        Self: Sized,
    {
        self.add_entry_may_overwrite(name, entry, false)
    }

    /// Adds a child entry to this directory. If `overwrite` is true, this function may overwrite
    /// an existing entry.
    ///
    /// Possible errors are:
    ///   * `ZX_ERR_INVALID_ARGS` or `ZX_ERR_BAD_PATH` if `name` is not a valid [`Name`].
    ///   * `ZX_ERR_ALREADY_EXISTS` if an entry with the same name is already present in the
    ///     directory and `overwrite` is false.
    fn add_entry_may_overwrite<NameT>(
        &self,
        name: NameT,
        entry: Arc<dyn DirectoryEntry>,
        overwrite: bool,
    ) -> Result<(), Status>
    where
        NameT: Into<String>,
        Self: Sized,
    {
        let name: String = name.into();
        let name: Name = name.try_into()?;
        self.add_entry_impl(name, entry, overwrite)
            .map_err(|_: AlreadyExists| Status::ALREADY_EXISTS)
    }

    /// Adds a child entry to this directory.
    fn add_entry_impl(
        &self,
        name: Name,
        entry: Arc<dyn DirectoryEntry>,
        overwrite: bool,
    ) -> Result<(), AlreadyExists>;

    /// Removes a child entry from this directory.  In case an entry with the matching name was
    /// found, the entry will be returned to the caller.  If `must_be_directory` is true, an error
    /// is returned if the entry is not a directory.
    ///
    /// Possible errors are:
    ///   * `ZX_ERR_INVALID_ARGS` or `ZX_ERR_BAD_PATH` if `name` is not a valid [`Name`].
    ///   * `ZX_ERR_NOT_DIR` if the entry identified by `name` is not a directory and
    ///     `must_be_directory` is true.
    fn remove_entry<NameT>(
        &self,
        name: NameT,
        must_be_directory: bool,
    ) -> Result<Option<Arc<dyn DirectoryEntry>>, Status>
    where
        NameT: Into<String>,
        Self: Sized,
    {
        let name: String = name.into();
        let name: Name = name.try_into()?;
        let entry = self
            .remove_entry_impl(name, must_be_directory)
            .map_err(|_: NotDirectory| Status::NOT_DIR)?;
        Ok(entry)
    }

    /// Removes a child entry from this directory.  In case an entry with the matching name was
    /// found, the entry will be returned to the caller.
    fn remove_entry_impl(
        &self,
        name: Name,
        must_be_directory: bool,
    ) -> Result<Option<Arc<dyn DirectoryEntry>>, NotDirectory>;

    /// Renaming needs to be atomic, even across two distinct directories.  So we need a special
    /// API to handle that.
    ///
    /// As two distinct directories may mean two mutexes to lock, using it correctly is non-trivial.
    /// In order to avoid a deadlock, we need to decide on a global ordering for the locks.
    /// `rename_from` and [`Self::rename_to`] are used depending on the relative order of the two
    /// directories involved in the operation.
    ///
    /// Implementations are expected to lock this directory, check that the entry exists and pass a
    /// reference to the entry to the `to` callback.  Only if the `to` callback succeed, should the
    /// entry be removed from the current directory.  This will guarantee atomic rename from the
    /// standpoint of the client.
    ///
    /// # Safety
    ///
    /// This should only be called by the rename implementation below, which will establish the
    /// global order and will call proper method.
    ///
    /// See https://fxbug.dev/99061: unsafe is not for visibility reduction.
    fn rename_from(
        &self,
        src: String,
        to: Box<dyn FnOnce(Arc<dyn DirectoryEntry>) -> Result<(), Status>>,
    ) -> Result<(), Status>;

    /// Renaming needs to be atomic, even across two distinct directories.  So we need a special
    /// API to handle that.
    ///
    /// See [`Self::rename_from`] comment for an explanation.
    ///
    /// Implementations are expected to lock this directory, check if they can accept an entry named
    /// `dst` (in case there might be any restrictions), then call the `from` callback to obtain a
    /// new entry which must be added into the current directory with no errors.
    ///
    /// # Safety
    ///
    /// This should only be called by the rename implementation below, which will establish the
    /// global order and will call proper method.
    ///
    /// See https://fxbug.dev/99061: unsafe is not for visibility reduction.
    fn rename_to(
        &self,
        dst: String,
        from: Box<dyn FnOnce() -> Result<Arc<dyn DirectoryEntry>, Status>>,
    ) -> Result<(), Status>;

    /// In case an entry is renamed within the same directory only one lock needs to be obtained.
    /// This is a companion method to the [`Self::rename_from`]/[`Self::rename_to`] pair.  The
    /// rename implementation below will use this method to avoid locking the same directory mutex
    /// twice.
    ///
    /// It should only be used by the rename implementation below.
    fn rename_within(&self, src: String, dst: String) -> Result<(), Status>;
}

#[async_trait]
impl<T: DirectlyMutable> MutableDirectory for T {
    async fn unlink(self: Arc<Self>, name: &str, must_be_directory: bool) -> Result<(), Status> {
        match self.remove_entry(name, must_be_directory) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(Status::NOT_FOUND),
            Err(e) => Err(e),
        }
    }

    // TODO(https://fxbug.dev/72801)
    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    // TODO(https://fxbug.dev/72801)
    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn sync(&self) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn rename(
        self: Arc<Self>,
        src_dir: Arc<dyn MutableDirectory>,
        src_name: Path,
        dst_name: Path,
    ) -> Result<(), Status> {
        let src_parent = src_dir.into_any().downcast::<T>().map_err(|_| Status::INVALID_ARGS)?;

        // We need to lock directories using the same global order, otherwise we risk a deadlock. We
        // will use directory objects memory location to establish global order for the locks.  It
        // introduces additional complexity, but, hopefully, avoids this subtle deadlocking issue.
        //
        // We will lock first object with the smaller memory address.
        let src_order = src_parent.as_ref() as *const dyn DirectlyMutable as *const usize as usize;
        let dst_order = self.as_ref() as *const dyn DirectlyMutable as *const usize as usize;

        if src_order < dst_order {
            // We must ensure that we have checked the global order for the locks for
            // `src_parent` and `self` and we are calling `rename_from` as `src_parent` has a
            // smaller memory address than the `self`.
            src_parent.clone().rename_from(
                src_name.into_string(),
                Box::new(move |entry| {
                    self.add_entry_may_overwrite(dst_name.into_string(), entry, true)
                }),
            )
        } else if src_order == dst_order {
            src_parent.rename_within(src_name.into_string(), dst_name.into_string())
        } else {
            // We must ensure that we have checked the global order for the locks for
            // `src_parent` and `self` and we are calling `rename_to` as `self` has a
            // smaller memory address than the `src_parent`.
            self.rename_to(
                dst_name.into_string(),
                Box::new(move || match src_parent.remove_entry(src_name.into_string(), false)? {
                    None => Err(Status::NOT_FOUND),
                    Some(entry) => Ok(entry),
                }),
            )
        }
    }
}
