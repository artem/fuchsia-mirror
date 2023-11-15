// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    fs::{
        default_seek, emit_dotdot, fileops_impl_directory, fileops_impl_seekable, fs_args,
        CacheMode, DirEntry, DirEntryHandle, DirectoryEntryType, DirentSink, FallocMode,
        FileHandle, FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
        FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString,
        InputBuffer, MountInfo, OutputBuffer, RenameFlags, SeekTarget, SymlinkTarget, UnlinkKind,
        ValueOrSize, VecInputBuffer, VecOutputBuffer, XattrOp,
    },
    logging::{log_error, log_warn, not_implemented},
    task::CurrentTask,
    types::errno::{errno, error, Errno, EEXIST, ENOENT},
    types::{device_type::DeviceType, ino_t, off_t, statfs, FileMode, OpenFlags},
};
use once_cell::sync::OnceCell;
use rand::Rng;
use starnix_lock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

// Name and value for the xattr used to mark opaque directories in the upper FS.
// See https://docs.kernel.org/filesystems/overlayfs.html#whiteouts-and-opaque-directories
const OPAQUE_DIR_XATTR: &FsStr = b"trusted.overlay.opaque";
const OPAQUE_DIR_XATTR_VALUE: &FsStr = b"y";

#[derive(Clone)]
struct DirEntryInfo {
    name: FsString,
    inode_num: ino_t,
    entry_type: DirectoryEntryType,
}

type DirEntries = Vec<DirEntryInfo>;

#[derive(Default)]
struct DirentSinkAdapter {
    items: Vec<DirEntryInfo>,
    offset: off_t,
}

impl DirentSink for DirentSinkAdapter {
    fn add(
        &mut self,
        inode_num: ino_t,
        offset: off_t,
        entry_type: DirectoryEntryType,
        name: &FsStr,
    ) -> Result<(), Errno> {
        if !DirEntry::is_reserved_name(name) {
            self.items.push(DirEntryInfo { name: name.to_vec(), inode_num, entry_type });
        }
        self.offset = offset;
        Ok(())
    }

    fn offset(&self) -> off_t {
        self.offset
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum UpperCopyMode {
    MetadataOnly,
    CopyAll,
}

/// An `DirEntry` associated with the mount options. This is required because OverlayFs mostly
/// works at the `DirEntry` level (mounts on the lower, upper and work directories are ignored),
/// but operation must still depend on mount options.
#[derive(Clone)]
struct ActiveEntry {
    entry: DirEntryHandle,
    mount: MountInfo,
}

impl ActiveEntry {
    fn mapper<'a>(entry: &'a ActiveEntry) -> impl Fn(DirEntryHandle) -> ActiveEntry + 'a {
        |dir_entry| ActiveEntry { entry: dir_entry, mount: entry.mount.clone() }
    }

    fn entry(&self) -> &DirEntryHandle {
        &self.entry
    }

    fn mount(&self) -> &MountInfo {
        &self.mount
    }

    fn component_lookup(&self, current_task: &CurrentTask, name: &FsStr) -> Result<Self, Errno> {
        self.entry()
            .component_lookup(current_task, self.mount(), name)
            .map(ActiveEntry::mapper(self))
    }

    fn create_entry(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        create_node_fn: impl FnOnce(&FsNodeHandle, &MountInfo, &FsStr) -> Result<FsNodeHandle, Errno>,
    ) -> Result<Self, Errno> {
        self.entry()
            .create_entry(current_task, self.mount(), name, create_node_fn)
            .map(ActiveEntry::mapper(self))
    }

    /// Sets an xattr to mark the directory referenced by `entry` as opaque. Directories that are
    /// marked as opaque in the upper FS are not merged with the corresponding directories in the
    /// lower FS.
    fn set_opaque_xattr(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.entry().node.set_xattr(
            current_task,
            self.mount(),
            OPAQUE_DIR_XATTR,
            OPAQUE_DIR_XATTR_VALUE,
            XattrOp::Set,
        )
    }

    /// Checks if the `entry` is marked as opaque.
    fn is_opaque_node(&self, current_task: &CurrentTask) -> bool {
        match self.entry().node.get_xattr(
            current_task,
            self.mount(),
            OPAQUE_DIR_XATTR,
            OPAQUE_DIR_XATTR_VALUE.len(),
        ) {
            Ok(ValueOrSize::Value(v)) if v == OPAQUE_DIR_XATTR_VALUE => true,
            _ => false,
        }
    }

    /// Creates a "whiteout" entry in the directory called `name`. Whiteouts are created by
    /// overlayfs to denote files and directories that were removed and should not be listed in the
    /// directory. This is necessary because we cannot remove entries from the lower FS.
    fn create_whiteout(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<ActiveEntry, Errno> {
        self.create_entry(current_task, name, |dir, mount, name| {
            dir.mknod(current_task, mount, name, FileMode::IFCHR, DeviceType::NONE, FsCred::root())
        })
    }

    /// Returns `true` if this is a "whiteout".
    fn is_whiteout(&self) -> bool {
        let info = self.entry().node.info();
        info.mode.is_chr() && info.rdev == DeviceType::NONE
    }

    /// Checks whether the child of this entry represented by `info` is a "whiteout".
    ///
    /// Only looks up the corresponding `DirEntry` when necessary.
    fn is_whiteout_child(
        &self,
        current_task: &CurrentTask,
        info: &DirEntryInfo,
    ) -> Result<bool, Errno> {
        // We need to lookup the node only if the file is a char device.
        if info.entry_type != DirectoryEntryType::CHR {
            return Ok(false);
        }
        let entry = self.component_lookup(current_task, &info.name)?;
        Ok(entry.is_whiteout())
    }

    fn read_dir_entries(&self, current_task: &CurrentTask) -> Result<Vec<DirEntryInfo>, Errno> {
        let mut sink = DirentSinkAdapter::default();
        self.entry()
            .open_anonymous(current_task, OpenFlags::DIRECTORY)?
            .readdir(current_task, &mut sink)?;
        Ok(sink.items)
    }
}

struct OverlayNode {
    fs: Arc<OverlayFs>,

    // Corresponding `DirEntries` in the lower and the upper filesystems. At least one must be
    // set. Note that we don't care about `NamespaceNode`: overlayfs overlays filesystems
    // (i.e. not namespace subtrees). These directories may not be mounted anywhere.
    // `upper` may be created dynamically whenever write access is required.
    upper: OnceCell<ActiveEntry>,
    lower: Option<ActiveEntry>,

    // `prepare_to_unlink()` may mark `upper` as opaque. In that case we want to skip merging
    // with `lower` in `readdir()`.
    upper_is_opaque: OnceCell<()>,

    parent: Option<Arc<OverlayNode>>,
}

impl OverlayNode {
    fn new(
        fs: Arc<OverlayFs>,
        lower: Option<ActiveEntry>,
        upper: Option<ActiveEntry>,
        parent: Option<Arc<OverlayNode>>,
    ) -> Arc<Self> {
        assert!(upper.is_some() || parent.is_some());

        let upper = match upper {
            Some(entry) => OnceCell::with_value(entry),
            None => OnceCell::new(),
        };

        let node = OverlayNode { fs, upper, lower, upper_is_opaque: OnceCell::new(), parent };

        Arc::new(node)
    }

    fn from_fs_node(node: &FsNodeHandle) -> Result<&Arc<Self>, Errno> {
        node.downcast_ops::<Arc<Self>>().ok_or_else(|| errno!(EIO))
    }

    fn main_entry(&self) -> &ActiveEntry {
        self.upper
            .get()
            .or_else(|| self.lower.as_ref())
            .expect("Expected either upper or lower node")
    }

    fn init_fs_node_for_child(
        self: &Arc<OverlayNode>,
        node: &FsNode,
        lower: Option<ActiveEntry>,
        upper: Option<ActiveEntry>,
    ) -> Arc<FsNode> {
        let entry = upper.as_ref().or(lower.as_ref()).expect("expect either lower or upper node");
        let info = entry.entry().node.info().clone();

        // Parent may be needed to initialize `upper`. We don't need to pass it if we have `upper`.
        let parent = if upper.is_some() { None } else { Some(self.clone()) };

        let overlay_node = OverlayNode::new(self.fs.clone(), lower, upper, parent);
        FsNode::new_uncached(overlay_node, &node.fs(), info.ino, info)
    }

    /// If the file is currently in the lower FS, then promote it to the upper FS. No-op if the
    /// file is already in the upper FS.
    fn ensure_upper(&self, current_task: &CurrentTask) -> Result<&ActiveEntry, Errno> {
        self.ensure_upper_maybe_copy(current_task, UpperCopyMode::CopyAll)
    }

    /// Same as `ensure_upper()`, but allows to skip copying of the file content.
    fn ensure_upper_maybe_copy(
        &self,
        current_task: &CurrentTask,
        copy_mode: UpperCopyMode,
    ) -> Result<&ActiveEntry, Errno> {
        self.upper.get_or_try_init(|| {
            let lower = self.lower.as_ref().expect("lower is expected when upper is missing");
            let parent = self.parent.as_ref().expect("Parent is expected when upper is missing");
            let parent_upper = parent.ensure_upper(current_task)?;
            let name = lower.entry().local_name();
            let info = {
                let info = lower.entry().node.info();
                info.clone()
            };
            let cred = info.cred();

            if info.mode.is_lnk() {
                let link_target = lower.entry().node.readlink(current_task)?;
                let link_path = match &link_target {
                    SymlinkTarget::Node(_) => return error!(EIO),
                    SymlinkTarget::Path(path) => path,
                };
                parent_upper.create_entry(current_task, &name, |dir, mount, name| {
                    dir.create_symlink(current_task, mount, name, link_path, info.cred())
                })
            } else if info.mode.is_reg() && copy_mode == UpperCopyMode::CopyAll {
                // Regular files need to be copied from lower FS to upper FS.
                self.fs.create_upper_entry(
                    current_task,
                    parent_upper,
                    &name,
                    |dir, name| {
                        dir.create_entry(current_task, name, |dir_node, mount, name| {
                            dir_node.mknod(
                                current_task,
                                mount,
                                name,
                                info.mode,
                                DeviceType::NONE,
                                cred,
                            )
                        })
                    },
                    |entry| copy_file_content(current_task, lower, &entry),
                )
            } else {
                // TODO(sergeyu): create_node() checks access, but we don't need that here.
                parent_upper.create_entry(current_task, &name, |dir, mount, name| {
                    dir.mknod(current_task, mount, name, info.mode, info.rdev, cred)
                })
            }

            // TODO(sergeyu): Copy xattrs to the new node.
        })
    }

    /// Check that an item isn't present in the lower FS.
    fn lower_entry_exists(&self, current_task: &CurrentTask, name: &FsStr) -> Result<bool, Errno> {
        match &self.lower {
            Some(lower) => match lower.component_lookup(current_task, name) {
                Ok(entry) => Ok(!entry.is_whiteout()),
                Err(err) if err.code == ENOENT => Ok(false),
                Err(err) => Err(err),
            },
            None => Ok(false),
        }
    }

    /// Helper used to create a new entry in the directory. It first checks that the target node
    /// doesn't exist. Then `do_create` is called to create the new node in the work dir, which
    /// is then moved to the target dir in the upper file system.
    ///
    /// It's assumed that the calling `DirEntry` has the current directory locked, so it is not
    /// supposed to change while this method is executed. Note that OveralayFS doesn't handle
    /// the case when the underlying file systems are changed directly, but that restriction
    /// is not enforced.
    fn create_entry<F>(
        self: &Arc<OverlayNode>,
        current_task: &CurrentTask,
        name: &FsStr,
        do_create: F,
    ) -> Result<ActiveEntry, Errno>
    where
        F: Fn(&ActiveEntry, &FsStr) -> Result<ActiveEntry, Errno>,
    {
        let upper = self.ensure_upper(current_task)?;

        match upper.component_lookup(current_task, name) {
            Ok(existing) => {
                // If there is an entry in the upper dir, then it must be a whiteout.
                if !existing.is_whiteout() {
                    return error!(EEXIST);
                }
            }

            Err(e) if e.code == ENOENT => {
                // If we don't have the entry in the upper fs, then check lower.
                if self.lower_entry_exists(current_task, name)? {
                    return error!(EEXIST);
                }
            }
            Err(e) => return Err(e),
        };

        self.fs.create_upper_entry(current_task, upper, name, do_create, |_entry| Ok(()))
    }

    /// An overlay directory may appear empty when the corresponding upper dir isn't empty:
    /// it may contain a number of whiteout entries. In that case the whiteouts need to be
    /// unlinked before the upper directory can be unlinked as well.
    /// `prepare_to_unlink()` checks that the directory doesn't contain anything other
    /// than whiteouts and if that is the case then it unlinks all of them.
    fn prepare_to_unlink(self: &Arc<OverlayNode>, current_task: &CurrentTask) -> Result<(), Errno> {
        if self.main_entry().entry().node.is_dir() {
            let mut lower_entries = BTreeSet::new();
            if let Some(dir) = &self.lower {
                for item in dir.read_dir_entries(current_task)?.drain(..) {
                    if !dir.is_whiteout_child(current_task, &item)? {
                        lower_entries.insert(item.name);
                    }
                }
            }

            if let Some(dir) = self.upper.get() {
                let mut to_remove = Vec::<FsString>::new();
                for item in dir.read_dir_entries(current_task)?.drain(..) {
                    if !dir.is_whiteout_child(current_task, &item)? {
                        return error!(ENOTEMPTY);
                    }
                    lower_entries.remove(&item.name);
                    to_remove.push(item.name);
                }

                if !lower_entries.is_empty() {
                    return error!(ENOTEMPTY);
                }

                // Mark the directory as opaque. Children can be removed after this.
                dir.set_opaque_xattr(current_task)?;
                let _ = self.upper_is_opaque.set(());

                // Finally, remove the children.
                for name in to_remove.iter() {
                    dir.entry().unlink(
                        current_task,
                        dir.mount(),
                        name,
                        UnlinkKind::NonDirectory,
                        false,
                    )?;
                }
            }
        }

        Ok(())
    }
}

impl FsNodeOps for Arc<OverlayNode> {
    fn create_file_ops(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if flags.can_write() {
            // Only upper FS can be writable.
            let copy_mode = if flags.contains(OpenFlags::TRUNC) {
                UpperCopyMode::MetadataOnly
            } else {
                UpperCopyMode::CopyAll
            };
            self.ensure_upper_maybe_copy(current_task, copy_mode)?;
        }

        let ops: Box<dyn FileOps> = if node.is_dir() {
            Box::new(OverlayDirectory { node: self.clone(), dir_entries: Default::default() })
        } else {
            let state = match (self.upper.get(), &self.lower) {
                (Some(upper), _) => {
                    OverlayFileState::Upper(upper.entry().open_anonymous(current_task, flags)?)
                }
                (None, Some(lower)) => {
                    OverlayFileState::Lower(lower.entry().open_anonymous(current_task, flags)?)
                }
                _ => panic!("Expected either upper or lower node"),
            };

            Box::new(OverlayFile { node: self.clone(), flags, state: RwLock::new(state) })
        };

        Ok(ops)
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let resolve_child = |dir_opt: Option<&ActiveEntry>| {
            // TODO(sergeyu): lookup() checks access, but we don't need that here.
            dir_opt
                .as_ref()
                .map(|dir| match dir.component_lookup(current_task, name) {
                    Ok(entry) => Some(Ok(entry)),
                    Err(e) if e.code == ENOENT => None,
                    Err(e) => Some(Err(e)),
                })
                .flatten()
                .transpose()
        };

        let upper: Option<ActiveEntry> = resolve_child(self.upper.get())?;

        let (upper_is_dir, upper_is_opaque) = match &upper {
            Some(upper) if upper.is_whiteout() => return error!(ENOENT),
            Some(upper) => {
                let is_dir = upper.entry().node.is_dir();
                let is_opaque = !is_dir || upper.is_opaque_node(current_task);
                (is_dir, is_opaque)
            }
            None => (false, false),
        };

        let parent_upper_is_opaque = self.upper_is_opaque.get().is_some();

        // We don't need to resolve the lower node if we have an opaque node in the upper dir.
        let lookup_lower = !parent_upper_is_opaque && !upper_is_opaque;
        let lower: Option<ActiveEntry> = if lookup_lower {
            match resolve_child(self.lower.as_ref())? {
                // If the upper node is a directory and the lower isn't then ignore the lower node.
                Some(lower) if upper_is_dir && !lower.entry().node.is_dir() => None,
                Some(lower) if lower.is_whiteout() => None,
                result => result,
            }
        } else {
            None
        };

        if upper.is_none() && lower.is_none() {
            return error!(ENOENT);
        }

        Ok(self.init_fs_node_for_child(node, lower, upper))
    }

    fn mknod(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node = self.create_entry(current_task, name, |dir, temp_name| {
            dir.create_entry(current_task, temp_name, |dir_node, mount, name| {
                dir_node.mknod(current_task, mount, name, mode, dev, owner.clone())
            })
        })?;
        Ok(self.init_fs_node_for_child(node, None, Some(new_upper_node)))
    }

    fn mkdir(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node = self.create_entry(current_task, name, |dir, temp_name| {
            let entry = dir.create_entry(current_task, temp_name, |dir_node, mount, name| {
                dir_node.mknod(current_task, mount, name, mode, DeviceType::NONE, owner.clone())
            })?;

            // Set opaque attribute to ensure the new directory is not merged with lower.
            entry.set_opaque_xattr(current_task)?;

            Ok(entry)
        })?;

        Ok(self.init_fs_node_for_child(node, None, Some(new_upper_node)))
    }

    fn create_symlink(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let new_upper_node = self.create_entry(current_task, name, |dir, temp_name| {
            dir.create_entry(current_task, temp_name, |dir_node, mount, name| {
                dir_node.create_symlink(current_task, mount, name, target, owner.clone())
            })
        })?;
        Ok(self.init_fs_node_for_child(node, None, Some(new_upper_node)))
    }

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        self.main_entry().entry().node.readlink(current_task)
    }

    fn link(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let child_overlay = OverlayNode::from_fs_node(child)?;
        let upper_child = child_overlay.ensure_upper(current_task)?;
        self.create_entry(current_task, name, |dir, temp_name| {
            dir.create_entry(current_task, temp_name, |dir_node, mount, name| {
                dir_node.link(current_task, mount, name, &upper_child.entry().node)
            })
        })?;
        Ok(())
    }

    fn unlink(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let upper = self.ensure_upper(current_task)?;
        let child_overlay = OverlayNode::from_fs_node(child)?;
        child_overlay.prepare_to_unlink(current_task)?;

        let need_whiteout = self.lower_entry_exists(current_task, name)?;
        if need_whiteout {
            self.fs.create_upper_entry(
                current_task,
                &upper,
                &name,
                |work, name| work.create_whiteout(current_task, name),
                |_entry| Ok(()),
            )?;
        } else if let Some(child_upper) = child_overlay.upper.get() {
            let kind = if child_upper.entry().node.is_dir() {
                UnlinkKind::Directory
            } else {
                UnlinkKind::NonDirectory
            };
            upper.entry().unlink(current_task, upper.mount(), name, kind, false)?
        }

        Ok(())
    }

    fn refresh_info<'a>(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let mut lock = info.write();
        *lock = self.main_entry().entry().node.refresh_info(current_task)?.clone();
        Ok(RwLockWriteGuard::downgrade(lock))
    }

    fn truncate(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        let upper = self.ensure_upper(current_task)?;
        upper.entry().node.truncate(current_task, upper.mount(), length)
    }

    fn allocate(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno> {
        self.ensure_upper(current_task)?.entry().node.fallocate(current_task, mode, offset, length)
    }
}
struct OverlayDirectory {
    node: Arc<OverlayNode>,
    dir_entries: RwLock<DirEntries>,
}

impl OverlayDirectory {
    fn refresh_dir_entries(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        let mut entries = DirEntries::new();

        let upper_is_opaque = self.node.upper_is_opaque.get().is_some();
        let merge_with_lower = self.node.lower.is_some() && !upper_is_opaque;

        // First enumerate entries in the upper dir. Then enumerate the lower dir and add only
        // items that are not present in the upper.
        let mut upper_set = BTreeSet::new();
        if let Some(dir) = self.node.upper.get() {
            for item in dir.read_dir_entries(current_task)?.drain(..) {
                // Fill `upper_set` only if we will need it later.
                if merge_with_lower {
                    upper_set.insert(item.name.clone());
                }
                if !dir.is_whiteout_child(current_task, &item)? {
                    entries.push(item);
                }
            }
        }

        if merge_with_lower {
            if let Some(dir) = &self.node.lower {
                for item in dir.read_dir_entries(current_task)?.drain(..) {
                    if !upper_set.contains(&item.name)
                        && !dir.is_whiteout_child(current_task, &item)?
                    {
                        entries.push(item);
                    }
                }
            }
        }

        *self.dir_entries.write() = entries;

        Ok(())
    }
}

impl FileOps for OverlayDirectory {
    fileops_impl_directory!();

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        default_seek(current_offset, target, |_| error!(EINVAL))
    }

    fn readdir(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        if sink.offset() == 0 {
            self.refresh_dir_entries(current_task)?;
        }

        emit_dotdot(file, sink)?;

        for item in self.dir_entries.read().iter().skip(sink.offset() as usize - 2) {
            sink.add(item.inode_num, sink.offset() + 1, item.entry_type, &item.name)?;
        }

        Ok(())
    }
}

enum OverlayFileState {
    Lower(FileHandle),
    Upper(FileHandle),
}

impl OverlayFileState {
    fn file(&self) -> &FileHandle {
        match self {
            Self::Lower(f) | Self::Upper(f) => f,
        }
    }
}

struct OverlayFile {
    node: Arc<OverlayNode>,
    flags: OpenFlags,
    state: RwLock<OverlayFileState>,
}

impl FileOps for OverlayFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let mut state = self.state.read();

        // Check if the file was promoted to the upper FS. In that case we need to reopen it from
        // there.
        if let Some(upper) = self.node.upper.get() {
            if matches!(*state, OverlayFileState::Lower(_)) {
                std::mem::drop(state);

                {
                    let mut write_state = self.state.write();
                    *write_state = OverlayFileState::Upper(
                        upper.entry().open_anonymous(current_task, self.flags)?,
                    );
                }
                state = self.state.read();
            }
        }

        state.file().read_at(current_task, offset, data)
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let state = self.state.read();
        match &*state {
            OverlayFileState::Upper(f) => f.write_at(current_task, offset, data),

            // `write()` should be called only for files that were opened for write, and that
            // required the file to be promoted to the upper FS.
            OverlayFileState::Lower(_) => panic!("write() called for a lower FS file."),
        }
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: crate::mm::ProtectionFlags,
    ) -> Result<Arc<fuchsia_zircon::Vmo>, Errno> {
        // Not that the VMO returned here will not updated if the file is promoted to upper FS
        // later. This is consistent with OveralyFS behavior on Linux, see
        // https://docs.kernel.org/filesystems/overlayfs.html#non-standard-behavior .
        self.state.read().file().get_vmo(current_task, length, prot)
    }
}
pub struct OverlayFs {
    // Keep references to the underlying file systems to ensure they outlive `overlayfs` since
    // they may be unmounted before overlayfs.
    #[allow(unused)]
    lower_fs: FileSystemHandle,
    upper_fs: FileSystemHandle,

    work: ActiveEntry,
}

impl OverlayFs {
    pub fn new_fs(
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let mount_options = fs_args::generic_parse_mount_options(&options.params);
        const REDIRECT_DIR: &FsStr = b"redirect_dir";
        match mount_options.get(&*REDIRECT_DIR) {
            None | Some(&b"off") => (),
            Some(_) => {
                not_implemented!("redirect_dir is not implemented in overlayfs yet.");
                return error!(ENOTSUP);
            }
        }

        let lower = resolve_dir_param(current_task, &mount_options, b"lowerdir")?;
        let upper = resolve_dir_param(current_task, &mount_options, b"upperdir")?;
        let work = resolve_dir_param(current_task, &mount_options, b"workdir")?;

        let lower_fs = lower.entry().node.fs().clone();
        let upper_fs = upper.entry().node.fs().clone();

        if !Arc::ptr_eq(&upper_fs, &work.entry().node.fs()) {
            log_error!("overlayfs: upperdir and workdir must be on the same FS");
            return error!(EINVAL);
        }

        let overlay_fs = Arc::new(OverlayFs { lower_fs, upper_fs, work });
        let root_node = OverlayNode::new(overlay_fs.clone(), Some(lower), Some(upper), None);
        let fs = FileSystem::new(current_task.kernel(), CacheMode::Uncached, overlay_fs, options);
        fs.set_root(root_node);
        Ok(fs)
    }

    // Helper used to create new entry called `name` in `target_dir` in the upper FS.
    // 1. Calls `try_create` to create a new entry in `work`. It is called repeateadly with a
    //    new name until it returns any result other than `EEXIST`.
    // 2. `do_init` is called to initilize the contents and the attributes of the new entry, etc.
    // 3. The new entry is moved to `target_dir`. If there is an existing entry called `name` in
    //    `target_dir` then it's replaced with the new entry.
    // The temp file is cleared from the work dir if either of the last two steps fails.
    fn create_upper_entry<FCreate, FInit>(
        &self,
        current_task: &CurrentTask,
        target_dir: &ActiveEntry,
        name: &FsStr,
        try_create: FCreate,
        do_init: FInit,
    ) -> Result<ActiveEntry, Errno>
    where
        FCreate: Fn(&ActiveEntry, &FsStr) -> Result<ActiveEntry, Errno>,
        FInit: FnOnce(&ActiveEntry) -> Result<(), Errno>,
    {
        let mut rng = rand::thread_rng();
        let (temp_name, entry) = loop {
            let x: u64 = rng.gen();
            let temp_name = format!("tmp{:x}", x).into_bytes();
            match try_create(&self.work, &temp_name) {
                Err(err) if err.code == EEXIST => continue,
                Err(err) => return Err(err),
                Ok(entry) => break (temp_name, entry),
            }
        };

        do_init(&entry)
            .and_then(|()| {
                DirEntry::rename(
                    current_task,
                    self.work.entry(),
                    self.work.mount(),
                    &temp_name,
                    target_dir.entry(),
                    target_dir.mount(),
                    name,
                    RenameFlags::REPLACE_ANY,
                )
            })
            .map_err(|e| {
                // Remove the temp entry in case of a failure.
                self.work
                    .entry()
                    .unlink(
                        current_task,
                        self.work.mount(),
                        &temp_name,
                        UnlinkKind::NonDirectory,
                        false,
                    )
                    .unwrap_or_else(|e| {
                        log_error!("Failed to cleanup work dir after an error: {}", e)
                    });
                e
            })?;

        Ok(entry)
    }
}

impl FileSystemOps for Arc<OverlayFs> {
    fn statfs(&self, _fs: &FileSystem, current_task: &CurrentTask) -> Result<statfs, Errno> {
        self.upper_fs.statfs(current_task)
    }

    fn name(&self) -> &'static FsStr {
        b"overlay"
    }

    fn rename(
        &self,
        _fs: &FileSystem,
        current_task: &CurrentTask,
        old_parent: &FsNodeHandle,
        old_name: &FsStr,
        new_parent: &FsNodeHandle,
        new_name: &FsStr,
        renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        let renamed = OverlayNode::from_fs_node(renamed)?;
        if renamed.main_entry().entry().node.is_dir() {
            // Return EXDEV for directory renames. Potentially they may be handled with the
            // `redirect_dir` feature, but it's not implemented here yet.
            // See https://docs.kernel.org/filesystems/overlayfs.html#renaming-directories
            return error!(EXDEV);
        }
        renamed.ensure_upper(current_task)?;

        let old_parent_overlay = OverlayNode::from_fs_node(old_parent)?;
        let old_parent_upper = old_parent_overlay.ensure_upper(current_task)?;

        let new_parent_overlay = OverlayNode::from_fs_node(new_parent)?;
        let new_parent_upper = new_parent_overlay.ensure_upper(current_task)?;

        let need_whiteout = old_parent_overlay.lower_entry_exists(current_task, old_name)?;

        DirEntry::rename(
            current_task,
            old_parent_upper.entry(),
            old_parent_upper.mount(),
            old_name,
            new_parent_upper.entry(),
            new_parent_upper.mount(),
            new_name,
            RenameFlags::REPLACE_ANY,
        )?;

        // If the old node existed in lower FS, then override it in the upper FS with a whiteout.
        if need_whiteout {
            match old_parent_upper.create_whiteout(current_task, old_name) {
                Err(e) => {
                    log_warn!(
                        "overlayfs: failed to create whiteout for {}: {}",
                        String::from_utf8_lossy(old_name),
                        e
                    )
                }
                Ok(_) => (),
            }
        }

        Ok(())
    }

    fn unmount(&self) {}
}

/// Helper used to resolve directories passed in mount options. The directory is resolved in the
/// namespace of the calling process, but only `DirEntry` is returned (detached from the
/// namespace). The corresponding file systems may be unmounted before overlayfs that uses them.
fn resolve_dir_param(
    current_task: &CurrentTask,
    mount_options: &HashMap<&FsStr, &FsStr>,
    name: &FsStr,
) -> Result<ActiveEntry, Errno> {
    let path = mount_options.get(name).ok_or_else(|| {
        log_error!("overlayfs: {} was not specified", String::from_utf8_lossy(name));
        errno!(EINVAL)
    })?;

    current_task
        .open_file(path, OpenFlags::RDONLY | OpenFlags::DIRECTORY)
        .map(|f| ActiveEntry { entry: f.name.entry.clone(), mount: f.name.mount.clone() })
        .map_err(|e| {
            log_error!("overlayfs: Failed to lookup {}: {}", String::from_utf8_lossy(path), e);
            e
        })
}

/// Copies file content from one file to another.
fn copy_file_content(
    current_task: &CurrentTask,
    from: &ActiveEntry,
    to: &ActiveEntry,
) -> Result<(), Errno> {
    let from_file = from.entry().open_anonymous(current_task, OpenFlags::RDONLY)?;
    let to_file = to.entry().open_anonymous(current_task, OpenFlags::WRONLY)?;

    const BUFFER_SIZE: usize = 4096;

    loop {
        // TODO(sergeyu): Reuse buffer between iterations.

        let mut output_buffer = VecOutputBuffer::new(BUFFER_SIZE);
        let bytes_read = from_file.read(current_task, &mut output_buffer)?;
        if bytes_read == 0 {
            break;
        }

        let buffer: Vec<u8> = output_buffer.into();
        let mut input_buffer = VecInputBuffer::from(buffer);
        while input_buffer.available() > 0 {
            to_file.write(current_task, &mut input_buffer)?;
        }
    }

    to_file.data_sync(current_task)?;

    Ok(())
}
