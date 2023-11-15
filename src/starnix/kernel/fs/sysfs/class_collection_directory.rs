// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    auth::FsCred,
    fs::{
        fs_node_impl_dir_readonly,
        kobject::{KObject, KObjectHandle},
        sysfs::SysFsOps,
        DirectoryEntryType, FileOps, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
        VecDirectory, VecDirectoryEntry,
    },
    task::CurrentTask,
    types::errno::{error, Errno},
    types::{mode, OpenFlags},
};

use std::sync::Weak;

pub struct ClassCollectionDirectory {
    kobject: Weak<KObject>,
}

impl ClassCollectionDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { kobject }
    }
}

impl SysFsOps for ClassCollectionDirectory {
    fn kobject(&self) -> KObjectHandle {
        self.kobject.upgrade().expect("Weak references to kobject must always be valid")
    }
}

impl FsNodeOps for ClassCollectionDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(
            self.kobject()
                .get_children_names()
                .into_iter()
                .map(|name| VecDirectoryEntry {
                    entry_type: DirectoryEntryType::LNK,
                    name,
                    inode: None,
                })
                .collect(),
        ))
    }

    fn lookup(
        &self,
        node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.kobject().get_child(name) {
            Some(child_kobject) => Ok(node.fs().create_node(
                child_kobject.ops(),
                FsNodeInfo::new_factory(mode!(IFDIR, 0o777), FsCred::root()),
            )),
            None => error!(ENOENT),
        }
    }
}
