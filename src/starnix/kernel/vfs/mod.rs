// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod anon_node;
mod dir_entry;
mod dirent_sink;
mod dynamic_file;
mod epoll;
mod fd_events;
mod fd_number;
mod fd_table;
mod file_object;
mod file_system;
mod file_write_guard;
mod fs_context;
mod fs_node;
mod namespace;
mod record_locks;
mod simple_file;
mod splice;
mod static_directory;
mod symlink_node;
mod vec_directory;
mod vmo_file;
mod wd_number;
mod xattr;

pub mod bpf;
pub mod buffers;
pub mod directory_file;
pub mod eventfd;
pub mod file_server;
pub mod fs_args;
pub mod fsverity;
pub mod fuse;
pub mod inotify;
pub mod path;
pub mod pidfd;
pub mod pipe;
pub mod rw_queue;
pub mod socket;
pub mod syscalls;

pub use anon_node::*;
pub use buffers::*;
pub use dir_entry::*;
pub use directory_file::*;
pub use dirent_sink::*;
pub use dynamic_file::*;
pub use epoll::*;
pub use fd_events::FdEvents;
pub use fd_number::*;
pub use fd_table::*;
pub use file_object::*;
pub use file_system::*;
pub use file_write_guard::*;
pub use fs_context::*;
pub use fs_node::*;
pub use namespace::*;
pub use path::*;
pub use record_locks::*;
pub use simple_file::*;
pub use static_directory::*;
pub use symlink_node::*;
pub use vec_directory::*;
pub use vmo_file::*;
pub use wd_number::*;
pub use xattr::*;

use crate::task::CurrentTask;
use lifecycle::{ObjectReleaser, ReleaserAction};
use starnix_uapi::ownership::{Releasable, ReleaseGuard};
use std::{cell::RefCell, ops::DerefMut, sync::Arc};

pub enum FileObjectReleaserAction {}
impl ReleaserAction<FileObject> for FileObjectReleaserAction {
    fn release(file_object: ReleaseGuard<FileObject>) {
        RELEASERS.with(|cell| {
            cell.borrow_mut().closed_files.push(file_object);
        });
    }
}
pub type FileReleaser = ObjectReleaser<FileObject, FileObjectReleaserAction>;

pub enum FsNodeReleaserAction {}
impl ReleaserAction<FsNode> for FsNodeReleaserAction {
    fn release(fs_node: ReleaseGuard<FsNode>) {
        RELEASERS.with(|cell| {
            cell.borrow_mut().dropped_fs_nodes.push(fs_node);
        });
    }
}
pub type FsNodeReleaser = ObjectReleaser<FsNode, FsNodeReleaserAction>;

thread_local! {
    /// Container of all `FileObject` that are not used anymore, but have not been closed yet.
    static RELEASERS: RefCell<LocalReleasers> = RefCell::new(LocalReleasers::default());
}

#[derive(Debug, Default)]
struct LocalReleasers {
    dropped_fs_nodes: Vec<ReleaseGuard<FsNode>>,
    closed_files: Vec<ReleaseGuard<FileObject>>,
    flushed_files: Vec<(FileHandle, FdTableId)>,
}

impl LocalReleasers {
    fn is_empty(&self) -> bool {
        self.dropped_fs_nodes.is_empty()
            && self.closed_files.is_empty()
            && self.flushed_files.is_empty()
    }
}

impl Releasable for LocalReleasers {
    type Context<'a> = &'a CurrentTask;

    fn release(self, context: Self::Context<'_>) {
        for fs_node in self.dropped_fs_nodes {
            fs_node.release(context);
        }
        for file in self.closed_files {
            file.release(context);
        }
        for (file, id) in self.flushed_files {
            file.flush(context, id);
        }
    }
}

/// Service to handle delayed releases.
///
/// Delayed releases are cleanup code that is run at specific point where the lock level is
/// known. The starnix kernel must ensure that delayed releases are run regularly.
#[derive(Debug, Default)]
pub struct DelayedReleaser {}

impl DelayedReleaser {
    pub fn flush_file(&self, file: &FileHandle, id: FdTableId) {
        RELEASERS.with(|cell| {
            cell.borrow_mut().flushed_files.push((Arc::clone(file), id));
        });
    }

    /// Run all current delayed releases for the current thread.
    pub fn apply(&self, current_task: &CurrentTask) {
        loop {
            let releasers = RELEASERS.with(|cell| std::mem::take(cell.borrow_mut().deref_mut()));
            if releasers.is_empty() {
                return;
            }
            releasers.release(current_task);
        }
    }
}
