// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module holding different kinds of pseudo directories and their building blocks.

use {
    crate::{
        common::io2_conversions, directory::entry_container::Directory,
        execution_scope::ExecutionScope, path::Path,
    },
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl_fuchsia_io as fio,
    std::sync::Arc,
};

#[macro_use]
pub mod test_utils;

pub mod common;

pub mod immutable;
pub mod mutable;

mod connection;
pub mod dirents_sink;
pub mod entry;
pub mod entry_container;
pub mod helper;
pub mod read_dirents;
pub mod simple;
pub mod traversal_position;
pub mod watchers;

/// A directory can be open either as a directory or a node.
#[derive(Clone)]
pub struct DirectoryOptions {
    pub(crate) rights: fio::Operations,
}

impl DirectoryOptions {
    pub(crate) fn to_io1(&self) -> fio::OpenFlags {
        io2_conversions::io2_to_io1(self.rights)
    }

    /// Creates a copy of these options with new [rights].
    pub fn new(rights: fio::Operations) -> DirectoryOptions {
        Self { rights: rights }
    }
}

impl Default for DirectoryOptions {
    fn default() -> Self {
        DirectoryOptions { rights: fio::R_STAR_DIR }
    }
}

/// Serves [dir] and returns a `DirectoryProxy` to it.
pub fn spawn_directory<D: Directory + ?Sized>(dir: Arc<D>) -> fio::DirectoryProxy {
    spawn_directory_with_options(dir, DirectoryOptions::default())
}

/// Serves [dir] with the given [options] and returns a `DirectoryProxy` to it.
pub fn spawn_directory_with_options<D: Directory + ?Sized>(
    dir: Arc<D>,
    options: DirectoryOptions,
) -> fio::DirectoryProxy {
    let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
    let scope = ExecutionScope::new();
    let rights = options.to_io1();
    dir.open(scope, rights, Path::dot(), ServerEnd::new(server_end.into_channel()));
    client_end.into_proxy().unwrap()
}
