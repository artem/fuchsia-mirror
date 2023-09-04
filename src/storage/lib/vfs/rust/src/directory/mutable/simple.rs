// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This is an implementation of a mutable "simple" pseudo directories.  Use [`simple()`] to
//! construct actual instances.  See [`Simple`] for details.

#[cfg(test)]
mod tests;

use crate::{
    directory::{
        entry::DirectoryEntry,
        mutable::{
            connection,
            entry_constructor::{EntryConstructor, NewEntryType},
        },
        simple,
    },
    path::Path,
};

use {fidl_fuchsia_io as fio, fuchsia_zircon::Status, std::sync::Arc};

pub type Connection = connection::MutableConnection;
pub type Simple = simple::Simple<Connection>;

/// Creates a mutable empty "simple" directory.  This directory holds a "static" set of entries,
/// allowing the server to add or remove entries via the
/// [`crate::directory::helper::DirectlyMutable::add_entry()`] and
/// [`crate::directory::helper::DirectlyMutable::remove_entry()`] methods.  These directories
/// content can be modified by the client.  It uses [`Connection`] type for the connection objects.
pub fn simple() -> Arc<Simple> {
    Simple::new(fio::INO_UNKNOWN)
}

/// Creates an [`EntryConstructor`] that will insert empty mutable directories when asked to create
/// a directory and when asked to create a file will delegate to the `file_constructor` function.
///
/// As the [`EntryConstructor`]s are expected to be inserted into the
/// [`crate::execution_scope::ExecutionScope`] via [`Arc`]s, it returns an `Arc` right away.
pub fn tree_constructor<FileConstructor>(
    file_constructor: FileConstructor,
) -> Arc<dyn EntryConstructor + Send + Sync>
where
    FileConstructor: Fn(Arc<dyn DirectoryEntry>, &str) -> Result<Arc<dyn DirectoryEntry>, Status>
        + Send
        + Sync
        + 'static,
{
    Arc::new(MutableTreeConstructor { file_constructor })
}

struct MutableTreeConstructor<FileConstructor>
where
    FileConstructor: Fn(Arc<dyn DirectoryEntry>, &str) -> Result<Arc<dyn DirectoryEntry>, Status>
        + Send
        + Sync
        + 'static,
{
    file_constructor: FileConstructor,
}

impl<FileConstructor> EntryConstructor for MutableTreeConstructor<FileConstructor>
where
    FileConstructor: Fn(Arc<dyn DirectoryEntry>, &str) -> Result<Arc<dyn DirectoryEntry>, Status>
        + Send
        + Sync
        + 'static,
{
    fn create_entry(
        self: Arc<Self>,
        parent: Arc<dyn DirectoryEntry>,
        what: NewEntryType,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, Status> {
        if !path.is_empty() {
            return Err(Status::NOT_FOUND);
        }

        let entry = match what {
            NewEntryType::Directory => simple() as Arc<dyn DirectoryEntry>,
            NewEntryType::File => (self.file_constructor)(parent, name)?,
        };

        Ok(entry)
    }
}
