// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::{CurrentTask, Kernel},
    vfs::{
        fileops_impl_directory, fs_node_impl_dir_readonly, unbounded_seek, CacheMode,
        DirectoryEntryType, DirentSink, FileHandle, FileObject, FileOps, FileSystem,
        FileSystemHandle, FileSystemOps, FsNode, FsNodeHandle, FsNodeOps, FsStr, FsString,
        MountInfo, SeekTarget,
    },
};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::{errno, errors::Errno, ino_t, off_t, open_flags::OpenFlags, statfs};
use std::{collections::BTreeMap, sync::Arc};

/// A filesystem that will delegate most operation to a base one, but have a number of top level
/// directory that points to other filesystems.
pub struct LayeredFs {
    base_fs: FileSystemHandle,
    mappings: BTreeMap<FsString, FileSystemHandle>,
}

impl LayeredFs {
    /// Build a new filesystem.
    ///
    /// `base_fs`: The base file system that this file system will delegate to.
    /// `mappings`: The map of top level directory to filesystems that will be layered on top of
    /// `base_fs`.
    pub fn new_fs(
        kernel: &Arc<Kernel>,
        base_fs: FileSystemHandle,
        mappings: BTreeMap<FsString, FileSystemHandle>,
    ) -> FileSystemHandle {
        let options = base_fs.options.clone();
        let layered_fs = Arc::new(LayeredFs { base_fs, mappings });
        let fs = FileSystem::new(kernel, CacheMode::Uncached, layered_fs.clone(), options);
        fs.set_root_node(FsNode::new_root(layered_fs));
        fs
    }
}

pub struct LayeredFsRootNodeOps {
    fs: Arc<LayeredFs>,
    root_file: FileHandle,
}

impl FileSystemOps for Arc<LayeredFs> {
    fn statfs(&self, _fs: &FileSystem, current_task: &CurrentTask) -> Result<statfs, Errno> {
        self.base_fs.statfs(current_task)
    }
    fn name(&self) -> &'static FsStr {
        self.base_fs.name()
    }
}

impl FsNodeOps for Arc<LayeredFs> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(LayeredFsRootNodeOps {
            fs: self.clone(),
            root_file: self.base_fs.root().open_anonymous(locked, current_task, flags)?,
        }))
    }

    fn lookup(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if let Some(fs) = self.mappings.get(name) {
            Ok(fs.root().node.clone())
        } else {
            self.base_fs.root().node.lookup(current_task, &MountInfo::detached(), name)
        }
    }
}

impl FileOps for LayeredFsRootNodeOps {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        let mut new_offset = unbounded_seek(current_offset, target)?;
        if new_offset >= self.fs.mappings.len() as off_t {
            new_offset = self
                .root_file
                .seek(current_task, SeekTarget::Set(new_offset - self.fs.mappings.len() as off_t))?
                .checked_add(self.fs.mappings.len() as off_t)
                .ok_or_else(|| errno!(EINVAL))?;
        }
        Ok(new_offset)
    }

    fn readdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        for (key, fs) in self.fs.mappings.iter().skip(sink.offset() as usize) {
            sink.add(
                fs.root().node.info().ino,
                sink.offset() + 1,
                DirectoryEntryType::DIR,
                key.as_ref(),
            )?;
        }

        struct DirentSinkWrapper<'a> {
            sink: &'a mut dyn DirentSink,
            mappings: &'a BTreeMap<FsString, FileSystemHandle>,
            offset: &'a mut off_t,
        }

        impl<'a> DirentSink for DirentSinkWrapper<'a> {
            fn add(
                &mut self,
                inode_num: ino_t,
                offset: off_t,
                entry_type: DirectoryEntryType,
                name: &FsStr,
            ) -> Result<(), Errno> {
                if !self.mappings.contains_key(name) {
                    self.sink.add(
                        inode_num,
                        offset + (self.mappings.len() as off_t),
                        entry_type,
                        name,
                    )?;
                }
                *self.offset = offset;
                Ok(())
            }
            fn offset(&self) -> off_t {
                *self.offset
            }
        }

        let mut root_file_offset = self.root_file.offset.lock();
        let mut wrapper =
            DirentSinkWrapper { sink, mappings: &self.fs.mappings, offset: &mut root_file_offset };

        self.root_file.readdir(locked, current_task, &mut wrapper)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{fs::tmpfs::TmpFs, testing::*};
    use starnix_sync::Unlocked;

    fn get_root_entry_names(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        fs: &FileSystem,
    ) -> Vec<Vec<u8>> {
        struct DirentNameCapturer {
            pub names: Vec<Vec<u8>>,
            offset: off_t,
        }
        impl DirentSink for DirentNameCapturer {
            fn add(
                &mut self,
                _inode_num: ino_t,
                offset: off_t,
                _entry_type: DirectoryEntryType,
                name: &FsStr,
            ) -> Result<(), Errno> {
                self.names.push(name.to_vec());
                self.offset = offset;
                Ok(())
            }
            fn offset(&self) -> off_t {
                self.offset
            }
        }
        let mut sink = DirentNameCapturer { names: vec![], offset: 0 };
        fs.root()
            .open_anonymous(locked, current_task, OpenFlags::RDONLY)
            .expect("open")
            .readdir(locked, current_task, &mut sink)
            .expect("readdir");
        std::mem::take(&mut sink.names)
    }

    #[::fuchsia::test]
    async fn test_remove_duplicates() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let base = TmpFs::new_fs(&kernel);
        base.root().create_dir(&mut locked, &current_task, "d1".into()).expect("create_dir");
        base.root().create_dir(&mut locked, &current_task, "d2".into()).expect("create_dir");
        let base_entries = get_root_entry_names(&mut locked, &current_task, &base);
        assert_eq!(base_entries.len(), 4);
        assert!(base_entries.contains(&b".".to_vec()));
        assert!(base_entries.contains(&b"..".to_vec()));
        assert!(base_entries.contains(&b"d1".to_vec()));
        assert!(base_entries.contains(&b"d2".to_vec()));

        let layered_fs = LayeredFs::new_fs(
            &kernel,
            base,
            BTreeMap::from([
                ("d1".into(), TmpFs::new_fs(&kernel)),
                ("d3".into(), TmpFs::new_fs(&kernel)),
            ]),
        );
        let layered_fs_entries = get_root_entry_names(&mut locked, &current_task, &layered_fs);
        assert_eq!(layered_fs_entries.len(), 5);
        assert!(layered_fs_entries.contains(&b".".to_vec()));
        assert!(layered_fs_entries.contains(&b"..".to_vec()));
        assert!(layered_fs_entries.contains(&b"d1".to_vec()));
        assert!(layered_fs_entries.contains(&b"d2".to_vec()));
        assert!(layered_fs_entries.contains(&b"d3".to_vec()));
    }
}
