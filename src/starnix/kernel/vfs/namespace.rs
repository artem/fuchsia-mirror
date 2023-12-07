// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::BinderFs,
    fs::{
        devpts::dev_pts_fs, devtmpfs::dev_tmp_fs, ext4::ExtFilesystem, overlayfs::OverlayFs,
        proc::proc_fs, sysfs::sys_fs, tmpfs::TmpFs, tracefs::trace_fs,
    },
    mutable_state::{state_accessor, state_implementation},
    selinux::fs::selinux_fs,
    task::{CurrentTask, EventHandler, Kernel, Task, WaitCanceler, Waiter},
    time::utc,
    vfs::{
        bpf::BpfFs,
        buffers::InputBuffer,
        fileops_impl_dataless, fileops_impl_delegate_read_and_seek, fileops_impl_nonseekable,
        fs_node_impl_not_dir,
        fuse::new_fuse_fs,
        socket::{SocketAddress, SocketHandle, UnixSocket},
        DirEntry, DirEntryHandle, DynamicFile, DynamicFileBuf, DynamicFileSource, FdEvents,
        FileHandle, FileObject, FileOps, FileSystemHandle, FileSystemOptions, FsNode, FsNodeHandle,
        FsNodeOps, FsStr, FsString, PathBuilder, RenameFlags, SimpleFileNode, SymlinkTarget,
        UnlinkKind,
    },
};
use fidl_fuchsia_io as fio;
use ref_cast::RefCast;
use starnix_lock::{Mutex, RwLock};
use starnix_logging::log_warn;
use starnix_uapi::{
    arc_key::{ArcKey, PtrKey, WeakKey},
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    file_mode::{Access, FileMode},
    inotify_mask::InotifyMask,
    mount_flags::MountFlags,
    open_flags::OpenFlags,
    ownership::WeakRef,
    NAME_MAX,
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    sync::{Arc, Weak},
};

/// A mount namespace.
///
/// The namespace records at which entries filesystems are mounted.
#[derive(Debug)]
pub struct Namespace {
    root_mount: MountHandle,

    // Unique ID of this namespace.
    pub id: u64,
}

impl Namespace {
    pub fn new(fs: FileSystemHandle) -> Arc<Namespace> {
        let kernel = fs.kernel.upgrade().expect("can't create namespace without a kernel");
        Arc::new(Self {
            root_mount: Mount::new(WhatToMount::Fs(fs), MountFlags::empty()),
            id: kernel.get_next_namespace_id(),
        })
    }

    pub fn root(&self) -> NamespaceNode {
        self.root_mount.root()
    }

    pub fn clone_namespace(&self) -> Arc<Namespace> {
        let kernel =
            self.root_mount.fs.kernel.upgrade().expect("can't clone namespace without a kernel");
        Arc::new(Self {
            root_mount: self.root_mount.clone_mount_recursive(),
            id: kernel.get_next_namespace_id(),
        })
    }

    /// Assuming new_ns is a clone of the namespace that node is from, return the equivalent of
    /// node in new_ns. If this assumption is violated, returns None.
    pub fn translate_node(mut node: NamespaceNode, new_ns: &Namespace) -> Option<NamespaceNode> {
        // Collect the list of mountpoints that leads to this node's mount
        let mut mountpoints = vec![];
        let mut mount = node.mount;
        while let Some(mountpoint) = mount.as_ref().and_then(|m| m.mountpoint()) {
            mountpoints.push(mountpoint.entry);
            mount = mountpoint.mount;
        }

        // Follow the same path in the new namespace
        let mut mount = Arc::clone(&new_ns.root_mount);
        for mountpoint in mountpoints.iter().rev() {
            let next_mount = Arc::clone(mount.read().submounts.get(ArcKey::ref_cast(mountpoint))?);
            mount = next_mount;
        }
        node.mount = Some(mount).into();
        Some(node)
    }
}

impl FsNodeOps for Arc<Namespace> {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MountNamespaceFile(self.clone())))
    }
}

pub struct MountNamespaceFile(pub Arc<Namespace>);

impl FileOps for MountNamespaceFile {
    fileops_impl_nonseekable!();
    fileops_impl_dataless!();
}

/// An instance of a filesystem mounted in a namespace.
///
/// At a mount, path traversal switches from one filesystem to another.
/// The client sees a composed directory structure that glues together the
/// directories from the underlying FsNodes from those filesystems.
///
/// The mounts in a namespace form a mount tree, with `mountpoint` pointing to the parent and
/// `submounts` pointing to the children.
pub struct Mount {
    root: DirEntryHandle,
    flags: Mutex<MountFlags>,
    fs: FileSystemHandle,

    /// A unique identifier for this mount reported in /proc/pid/mountinfo.
    id: u64,

    // Lock ordering: mount -> submount
    state: RwLock<MountState>,
    // Mount used to contain a Weak<Namespace>. It no longer does because since the mount point
    // hash was moved from Namespace to Mount, nothing actually uses it. Now that
    // Namespace::clone_namespace() is implemented in terms of Mount::clone_mount_recursive, it
    // won't be trivial to add it back. I recommend turning the mountpoint field into an enum of
    // Mountpoint or Namespace, maybe called "parent", and then traverse up to the top of the tree
    // if you need to find a Mount's Namespace.
}
type MountHandle = Arc<Mount>;

/// Public representation of the mount options.
#[derive(Clone, Debug)]
pub struct MountInfo(Option<MountHandle>);

impl MountInfo {
    /// `MountInfo` for a element that is not tied to a given mount. Mount flags will be considered
    /// empty.
    pub fn detached() -> Self {
        None.into()
    }

    /// The mount flags of the represented mount.
    pub fn flags(&self) -> MountFlags {
        if let Some(mount) = &self.0 {
            mount.flags()
        } else {
            // Consider not mounted node have the NOATIME flags.
            MountFlags::NOATIME
        }
    }

    /// Checks whether this `MountInfo` represents a writable file system mounted.
    pub fn check_readonly_filesystem(&self) -> Result<(), Errno> {
        if self.flags().contains(MountFlags::RDONLY) {
            return error!(EROFS);
        }
        Ok(())
    }
}

impl Deref for MountInfo {
    type Target = Option<MountHandle>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for MountInfo {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::cmp::PartialEq for MountInfo {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref().map(Arc::as_ptr) == other.0.as_ref().map(Arc::as_ptr)
    }
}

impl std::cmp::Eq for MountInfo {}

impl Into<MountInfo> for Option<MountHandle> {
    fn into(self) -> MountInfo {
        MountInfo(self)
    }
}

#[derive(Default)]
pub struct MountState {
    /// The namespace node that this mount is mounted on. This is a tuple instead of a
    /// NamespaceNode because the Mount pointer has to be weak because this is the pointer to the
    /// parent mount, the parent has a pointer to the children too, and making both strong would be
    /// a cycle.
    mountpoint: Option<(Weak<Mount>, DirEntryHandle)>,

    // The keys of this map are always descendants of this mount's root.
    //
    // Each directory entry can only have one mount attached. Mount shadowing works by using the
    // root of the inner mount as a mountpoint. For example, if filesystem A is mounted at /foo,
    // mounting filesystem B on /foo will create the mount as a child of the A mount, attached to
    // A's root, instead of the root mount.
    submounts: HashMap<ArcKey<DirEntry>, MountHandle>,

    /// The membership of this mount in its peer group. Do not access directly. Instead use
    /// peer_group(), take_from_peer_group(), and set_peer_group().
    // TODO(tbodt): Refactor the links into, some kind of extra struct or something? This is hard
    // because setting this field requires the Arc<Mount>.
    peer_group_: Option<(Arc<PeerGroup>, PtrKey<Mount>)>,
    /// The membership of this mount in a PeerGroup's downstream. Do not access directly. Instead
    /// use upstream(), take_from_upstream(), and set_upstream().
    upstream_: Option<(Weak<PeerGroup>, PtrKey<Mount>)>,
}

/// A group of mounts. Setting MS_SHARED on a mount puts it in its own peer group. Any bind mounts
/// of a mount in the group are also added to the group. A mount created in any mount in a peer
/// group will be automatically propagated (recreated) in every other mount in the group.
#[derive(Default)]
struct PeerGroup {
    id: u64,
    state: RwLock<PeerGroupState>,
}
#[derive(Default)]
struct PeerGroupState {
    mounts: HashSet<WeakKey<Mount>>,
    downstream: HashSet<WeakKey<Mount>>,
}

pub enum WhatToMount {
    Fs(FileSystemHandle),
    Bind(NamespaceNode),
}

impl Mount {
    fn new(what: WhatToMount, flags: MountFlags) -> MountHandle {
        match what {
            WhatToMount::Fs(fs) => Self::new_with_root(fs.root().clone(), flags),
            WhatToMount::Bind(node) => {
                let mount = node.mount.as_ref().expect("can't bind mount from an anonymous node");
                mount.clone_mount(&node.entry, flags)
            }
        }
    }

    fn new_with_root(root: DirEntryHandle, flags: MountFlags) -> MountHandle {
        let known_flags = MountFlags::STORED_ON_MOUNT;
        assert!(
            !flags.intersects(!known_flags),
            "mount created with extra flags {:?}",
            flags - known_flags
        );
        let fs = root.node.fs();
        let kernel = fs.kernel.upgrade().expect("can't create mount without kernel");
        Arc::new(Self {
            id: kernel.get_next_mount_id(),
            flags: Mutex::new(flags),
            root,
            fs,
            state: Default::default(),
        })
    }

    /// A namespace node referring to the root of the mount.
    pub fn root(self: &MountHandle) -> NamespaceNode {
        NamespaceNode { mount: Some(Arc::clone(self)).into(), entry: Arc::clone(&self.root) }
    }

    /// The NamespaceNode on which this Mount is mounted.
    fn mountpoint(&self) -> Option<NamespaceNode> {
        let state = self.state.read();
        let (ref mount, ref entry) = state.mountpoint.as_ref()?;
        Some(NamespaceNode { mount: Some(mount.upgrade()?).into(), entry: entry.clone() })
    }

    /// Create the specified mount as a child. Also propagate it to the mount's peer group.
    fn create_submount(
        self: &MountHandle,
        dir: &DirEntryHandle,
        what: WhatToMount,
        flags: MountFlags,
    ) {
        // TODO(tbodt): Making a copy here is necessary for lock ordering, because the peer group
        // lock nests inside all mount locks (it would be impractical to reverse this because you
        // need to lock a mount to get its peer group.) But it opens the door to race conditions
        // where if a peer are concurrently being added, the mount might not get propagated to the
        // new peer. The only true solution to this is bigger locks, somehow using the same lock
        // for the peer group and all of the mounts in the group. Since peer groups are fluid and
        // can have mounts constantly joining and leaving and then joining other groups, the only
        // sensible locking option is to use a single global lock for all mounts and peer groups.
        // This is almost impossible to express in rust. Help.
        //
        // Update: Also necessary to make a copy to prevent excess replication, see the comment on
        // the following Mount::new call.
        let peers = {
            let state = self.state.read();
            state.peer_group().map(|g| g.copy_propagation_targets()).unwrap_or_default()
        };

        // Create the mount after copying the peer groups, because in the case of creating a bind
        // mount inside itself, the new mount would get added to our peer group during the
        // Mount::new call, but we don't want to replicate into it already. For an example see
        // MountTest.QuizBRecursion.
        let mount = Mount::new(what, flags);

        if self.read().is_shared() {
            mount.write().make_shared();
        }

        for peer in peers {
            if Arc::ptr_eq(self, &peer) {
                continue;
            }
            let clone = mount.clone_mount_recursive();
            peer.write().add_submount_internal(dir, clone);
        }

        self.write().add_submount_internal(dir, mount)
    }

    fn remove_submount(
        self: &MountHandle,
        mount_hash_key: &ArcKey<DirEntry>,
        mountpoint: &DirEntryHandle,
        propagate: bool,
    ) -> Result<Arc<Mount>, Errno> {
        if propagate {
            // create_submount explains why we need to make a copy of peers.
            let peers = {
                let state = self.state.read();
                state.peer_group().map(|g| g.copy_propagation_targets()).unwrap_or_default()
            };

            for peer in peers {
                if Arc::ptr_eq(self, &peer) {
                    continue;
                }
                peer.write().remove_submount_internal(mount_hash_key, mountpoint);
            }
        }

        self.write().remove_submount_internal(mount_hash_key, mountpoint).ok_or(errno!(EINVAL))
    }

    /// Create a new mount with the same filesystem, flags, and peer group. Used to implement bind
    /// mounts.
    fn clone_mount(
        self: &MountHandle,
        new_root: &DirEntryHandle,
        flags: MountFlags,
    ) -> MountHandle {
        assert!(new_root.is_descendant_of(&self.root));
        // According to mount(2) on bind mounts, all flags other than MS_REC are ignored when doing
        // a bind mount.
        let clone = Self::new_with_root(Arc::clone(new_root), self.flags());

        if flags.contains(MountFlags::REC) {
            // This is two steps because the alternative (locking clone.state while iterating over
            // self.state.submounts) trips tracing_mutex. The lock ordering is parent -> child, and
            // if the clone is eventually made a child of self, this looks like an ordering
            // violation. I'm not convinced it's a real issue, but I can't convince myself it's not
            // either.
            let mut submounts = vec![];
            for (dir, mount) in &self.state.read().submounts {
                submounts.push((dir.clone(), mount.clone_mount_recursive()));
            }
            let mut clone_state = clone.write();
            for (dir, submount) in submounts {
                clone_state.add_submount_internal(&dir, submount);
            }
        }

        // Put the clone in the same peer group
        let peer_group = self.state.read().peer_group().map(Arc::clone);
        if let Some(peer_group) = peer_group {
            clone.write().set_peer_group(peer_group);
        }

        clone
    }

    /// Do a clone of the full mount hierarchy below this mount. Used for creating mount
    /// namespaces and creating copies to use for propagation.
    fn clone_mount_recursive(self: &MountHandle) -> MountHandle {
        self.clone_mount(&self.root, MountFlags::REC)
    }

    pub fn change_propagation(self: &MountHandle, flag: MountFlags, recursive: bool) {
        let mut state = self.write();
        match flag {
            MountFlags::SHARED => state.make_shared(),
            MountFlags::PRIVATE => state.make_private(),
            MountFlags::DOWNSTREAM => state.make_downstream(),
            _ => {
                log_warn!("mount propagation {:?}", flag);
                return;
            }
        }

        if recursive {
            for mount in state.submounts.values() {
                mount.change_propagation(flag, recursive);
            }
        }
    }

    fn flags(self: &MountHandle) -> MountFlags {
        *self.flags.lock()
    }

    pub fn update_flags(self: &MountHandle, mut flags: MountFlags) {
        flags &= MountFlags::STORED_ON_MOUNT;
        let atime_flags = MountFlags::NOATIME
            | MountFlags::NODIRATIME
            | MountFlags::RELATIME
            | MountFlags::STRICTATIME;
        let mut stored_flags = self.flags.lock();
        if !flags.intersects(atime_flags) {
            // Since Linux 3.17, if none of MS_NOATIME, MS_NODIRATIME,
            // MS_RELATIME, or MS_STRICTATIME is specified in mountflags, then
            // the remount operation preserves the existing values of these
            // flags (rather than defaulting to MS_RELATIME).
            flags |= *stored_flags & atime_flags;
        }
        // The "effect [of MS_STRICTATIME] is to clear the MS_NOATIME and MS_RELATIME flags."
        flags &= !MountFlags::STRICTATIME;
        *stored_flags = flags;
    }

    state_accessor!(Mount, state);
}

impl MountState {
    /// Return this mount's current peer group.
    fn peer_group(&self) -> Option<&Arc<PeerGroup>> {
        let (ref group, _) = self.peer_group_.as_ref()?;
        Some(group)
    }

    /// Remove this mount from its peer group and return the peer group.
    fn take_from_peer_group(&mut self) -> Option<Arc<PeerGroup>> {
        let (old_group, old_mount) = self.peer_group_.take()?;
        old_group.remove(old_mount);
        if let Some(upstream) = self.take_from_upstream() {
            let next_mount =
                old_group.state.read().mounts.iter().next().map(|w| w.0.upgrade().unwrap());
            if let Some(next_mount) = next_mount {
                // TODO(fxbug.dev/114002): Fix the lock ordering here. We've locked next_mount
                // while self is locked, and since the propagation tree and mount tree are
                // separate, this could violate the mount -> submount order previously established.
                next_mount.write().set_upstream(upstream);
            }
        }
        Some(old_group)
    }

    fn upstream(&self) -> Option<Arc<PeerGroup>> {
        self.upstream_.as_ref().and_then(|g| g.0.upgrade())
    }

    fn take_from_upstream(&mut self) -> Option<Arc<PeerGroup>> {
        let (old_upstream, old_mount) = self.upstream_.take()?;
        // TODO(tbodt): Reason about whether the upgrade() could possibly return None, and what we
        // should actually do in that case.
        let old_upstream = old_upstream.upgrade()?;
        old_upstream.remove_downstream(old_mount);
        Some(old_upstream)
    }
}

#[apply(state_implementation!)]
impl MountState<Base = Mount> {
    /// Add a child mount *without propagating it to the peer group*. For internal use only.
    fn add_submount_internal(&mut self, dir: &DirEntryHandle, mount: MountHandle) {
        if !dir.is_descendant_of(&self.base.root) {
            return;
        }

        dir.register_mount();
        let old_mountpoint =
            mount.state.write().mountpoint.replace((Arc::downgrade(self.base), Arc::clone(dir)));
        assert!(old_mountpoint.is_none(), "add_submount can only take a newly created mount");
        // Mount shadowing is implemented by mounting onto the root of the first mount, not by
        // creating two mounts on the same mountpoint.
        let old_mount = self.submounts.insert(ArcKey(dir.clone()), Arc::clone(&mount));

        // In rare cases, mount propagation might result in a request to mount on a directory where
        // something is already mounted. MountTest.LotsOfShadowing will trigger this. Linux handles
        // this by inserting the new mount between the old mount and the current mount.
        if let Some(old_mount) = old_mount {
            // Previous state: self[dir] = old_mount
            // New state: self[dir] = new_mount, new_mount[new_mount.root] = old_mount
            // The new mount has already been inserted into self, now just update the old mount to
            // be a child of the new mount.
            old_mount.write().mountpoint = Some((Arc::downgrade(&mount), Arc::clone(dir)));
            mount.write().submounts.insert(ArcKey(Arc::clone(&mount.root)), old_mount);
        }
    }

    fn remove_submount_internal(
        &mut self,
        mount_hash_key: &ArcKey<DirEntry>,
        mountpoint: &DirEntryHandle,
    ) -> Option<Arc<Mount>> {
        let removed = self.submounts.remove(mount_hash_key);
        if removed.is_some() {
            mountpoint.unregister_mount();
        }
        removed
    }

    /// Set this mount's peer group.
    fn set_peer_group(&mut self, group: Arc<PeerGroup>) {
        self.take_from_peer_group();
        group.add(self.base);
        self.peer_group_ = Some((group, Arc::as_ptr(self.base).into()));
    }

    fn set_upstream(&mut self, group: Arc<PeerGroup>) {
        self.take_from_upstream();
        group.add_downstream(self.base);
        self.upstream_ = Some((Arc::downgrade(&group), Arc::as_ptr(self.base).into()));
    }

    /// Is the mount in a peer group? Corresponds to MS_SHARED.
    pub fn is_shared(&self) -> bool {
        self.peer_group().is_some()
    }

    /// Put the mount in a peer group. Implements MS_SHARED.
    pub fn make_shared(&mut self) {
        if self.is_shared() {
            return;
        }
        let kernel =
            self.base.fs.kernel.upgrade().expect("can't create new peer group without kernel");
        self.set_peer_group(PeerGroup::new(kernel.get_next_peer_group_id()));
    }

    /// Take the mount out of its peer group, also remove upstream if any. Implements MS_PRIVATE.
    pub fn make_private(&mut self) {
        self.take_from_peer_group();
        self.take_from_upstream();
    }

    /// Take the mount out of its peer group and make it downstream instead. Implements
    /// MountFlags::DOWNSTREAM (MS_SLAVE).
    pub fn make_downstream(&mut self) {
        if let Some(peer_group) = self.take_from_peer_group() {
            self.set_upstream(peer_group);
        }
    }
}

impl PeerGroup {
    fn new(id: u64) -> Arc<Self> {
        Arc::new(Self { id, state: Default::default() })
    }

    fn add(&self, mount: &Arc<Mount>) {
        self.state.write().mounts.insert(WeakKey::from(mount));
    }

    fn remove(&self, mount: PtrKey<Mount>) {
        self.state.write().mounts.remove(&mount);
    }

    fn add_downstream(&self, mount: &Arc<Mount>) {
        self.state.write().downstream.insert(WeakKey::from(mount));
    }

    fn remove_downstream(&self, mount: PtrKey<Mount>) {
        self.state.write().downstream.remove(&mount);
    }

    fn copy_propagation_targets(&self) -> Vec<MountHandle> {
        let mut buf = vec![];
        self.collect_propagation_targets(&mut buf);
        buf
    }

    fn collect_propagation_targets(&self, buf: &mut Vec<MountHandle>) {
        let downstream_mounts: Vec<_> = {
            let state = self.state.read();
            buf.extend(state.mounts.iter().filter_map(|m| m.0.upgrade()));
            state.downstream.iter().filter_map(|m| m.0.upgrade()).collect()
        };
        for mount in downstream_mounts {
            let peer_group = mount.read().peer_group().map(Arc::clone);
            match peer_group {
                Some(group) => group.collect_propagation_targets(buf),
                None => buf.push(mount),
            }
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        state.take_from_peer_group();
        state.take_from_upstream();
    }
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.read();
        f.debug_struct("Mount")
            .field("id", &(self as *const Mount))
            .field("root", &self.root)
            .field("mountpoint", &state.mountpoint)
            .field("submounts", &state.submounts)
            .finish()
    }
}

pub trait FileSystemCreator {
    fn kernel(&self) -> &Arc<Kernel>;

    fn create_filesystem(
        &self,
        fs_type: &FsStr,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno>;
}

impl Kernel {
    pub fn get_next_mount_id(&self) -> u64 {
        self.next_mount_id.next()
    }

    pub fn get_next_peer_group_id(&self) -> u64 {
        self.next_peer_group_id.next()
    }

    pub fn get_next_namespace_id(&self) -> u64 {
        self.next_namespace_id.next()
    }
}

impl FileSystemCreator for Arc<Kernel> {
    fn kernel(&self) -> &Arc<Kernel> {
        self
    }

    fn create_filesystem(
        &self,
        fs_type: &FsStr,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        Ok(match fs_type {
            b"binder" => BinderFs::new_fs(self, options)?,
            b"bpf" => BpfFs::new_fs(self, options)?,
            b"remotefs" => crate::execution::create_remotefs_filesystem(
                self,
                self.container_data_dir
                    .as_ref()
                    .ok_or_else(|| errno!(EPERM, "Missing container data directory"))?,
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
                options,
            )?,
            b"tmpfs" => {
                let fs = TmpFs::new_fs_with_options(self, options)?;
                if self.has_fake_selinux() {
                    let label = b"u:object_r:tmpfs:s0";
                    fs.selinux_context.set(label.to_vec()).unwrap();
                }
                fs
            }
            _ => {
                return error!(ENODEV, String::from_utf8_lossy(fs_type));
            }
        })
    }
}

impl FileSystemCreator for CurrentTask {
    fn kernel(&self) -> &Arc<Kernel> {
        (self as &Task).kernel()
    }

    fn create_filesystem(
        &self,
        fs_type: &FsStr,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = self.kernel();

        match fs_type {
            b"fuse" => new_fuse_fs(self, options),
            b"devpts" => Ok(dev_pts_fs(self, options).clone()),
            b"devtmpfs" => Ok(dev_tmp_fs(self).clone()),
            b"ext4" => ExtFilesystem::new_fs(kernel, self, options),
            b"overlay" => OverlayFs::new_fs(self, options),
            b"proc" => Ok(proc_fs(self, options).clone()),
            b"tracefs" => Ok(trace_fs(self, options).clone()),
            b"selinuxfs" => {
                if self.kernel().security_server.is_some() {
                    Ok(selinux_fs(self, options).clone())
                } else {
                    error!(ENODEV, String::from_utf8_lossy(fs_type))
                }
            }
            b"sysfs" => Ok(sys_fs(self, options).clone()),
            _ => kernel.create_filesystem(fs_type, options),
        }
    }
}

struct ProcMountsFileSource(WeakRef<Task>);

impl DynamicFileSource for ProcMountsFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // TODO(tbodt): We should figure out a way to have a real iterator instead of grabbing the
        // entire list in one go. Should we have a BTreeMap<u64, Weak<Mount>> in the Namespace?
        // Also has the benefit of correct (i.e. chronological) ordering. But then we have to do
        // extra work to maintain it.
        let task = Task::from_weak(&self.0)?;
        let root = task.fs().root();
        let ns = task.fs().namespace();
        for_each_mount(&ns.root_mount, &mut |mount| {
            let mountpoint = mount.mountpoint().unwrap_or_else(|| mount.root());
            if !mountpoint.is_descendant_of(&root) {
                return Ok(());
            }
            let fs_spec = String::from_utf8_lossy(mount.fs.options.source_for_display());
            let fs_file = String::from_utf8_lossy(&mountpoint.path(&task)).into_owned();
            let fs_vfstype = String::from_utf8_lossy(mount.fs.name());
            let fs_mntopts = mount.flags().to_string();
            writeln!(sink, "{fs_spec} {fs_file} {fs_vfstype} {fs_mntopts} 0 0")?;
            Ok(())
        })?;
        Ok(())
    }
}

pub struct ProcMountsFile {
    dynamic_file: DynamicFile<ProcMountsFileSource>,
}

impl ProcMountsFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self { dynamic_file: DynamicFile::new(ProcMountsFileSource(task.clone())) })
        })
    }
}

impl FileOps for ProcMountsFile {
    fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        // Polling this file gives notifications when any change to mounts occurs. This is not
        // implemented yet, but stubbed for Android init.
        Some(waiter.fake_wait())
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }
}

#[derive(Clone)]
pub struct ProcMountinfoFile(WeakRef<Task>);
impl ProcMountinfoFile {
    pub fn new_node(task: WeakRef<Task>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(task))
    }
}
impl DynamicFileSource for ProcMountinfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        // Returns path to the `dir` from the root of the file system.
        fn path_from_fs_root(dir: &DirEntryHandle) -> FsString {
            let mut path = PathBuilder::new();
            if dir.is_dead() {
                // Return `/foo/dir//deleted` if the dir was deleted.
                path.prepend_element(b"/deleted");
            }
            let mut current = dir.clone();
            while let Some(next) = current.parent() {
                path.prepend_element(&current.local_name());
                current = next
            }
            path.build()
        }

        // TODO(tbodt): We should figure out a way to have a real iterator instead of grabbing the
        // entire list in one go. Should we have a BTreeMap<u64, Weak<Mount>> in the Namespace?
        // Also has the benefit of correct (i.e. chronological) ordering. But then we have to do
        // extra work to maintain it.
        let task = Task::from_weak(&self.0)?;
        let root = task.fs().root();
        let ns = task.fs().namespace();
        for_each_mount(&ns.root_mount, &mut |mount| {
            let mountpoint = mount.mountpoint().unwrap_or_else(|| mount.root());
            if !mountpoint.is_descendant_of(&root) {
                return Ok(());
            }
            // Can't fail, mountpoint() and root() can't return a NamespaceNode with no mount
            let parent = mountpoint.mount.as_ref().unwrap();
            write!(
                sink,
                "{} {} {} {} {} {}",
                mount.id,
                parent.id,
                mount.root.node.fs().dev_id,
                String::from_utf8_lossy(&path_from_fs_root(&mount.root)),
                String::from_utf8_lossy(&mountpoint.path(&task)),
                mount.flags().to_string(),
            )?;
            if let Some(peer_group) = mount.read().peer_group() {
                write!(sink, " shared:{}", peer_group.id)?;
            }
            if let Some(upstream) = mount.read().upstream() {
                write!(sink, " master:{}", upstream.id)?;
            }
            writeln!(
                sink,
                " - {} {} {}",
                String::from_utf8_lossy(mount.fs.name()),
                String::from_utf8_lossy(mount.fs.options.source_for_display()),
                mount.fs.options.flags.to_string()
            )?;
            Ok(())
        })?;
        Ok(())
    }
}

fn for_each_mount<E>(
    mount: &MountHandle,
    callback: &mut impl FnMut(&MountHandle) -> Result<(), E>,
) -> Result<(), E> {
    callback(mount)?;
    // Collect list first to avoid self deadlock when ProcMountinfoFile::read_at tries to call
    // NamespaceNode::path()
    let submounts: Vec<_> = mount.read().submounts.values().map(Arc::clone).collect();
    for submount in submounts {
        for_each_mount(&submount, callback)?;
    }
    Ok(())
}

/// The `SymlinkMode` enum encodes how symlinks are followed during path traversal.
#[derive(Default, PartialEq, Eq, Copy, Clone, Debug)]
pub enum SymlinkMode {
    /// Follow a symlink at the end of a path resolution.
    #[default]
    Follow,

    /// Do not follow a symlink at the end of a path resolution.
    NoFollow,
}

/// The maximum number of symlink traversals that can be made during path resolution.
const MAX_SYMLINK_FOLLOWS: u8 = 40;

/// The context passed during namespace lookups.
///
/// Namespace lookups need to mutate a shared context in order to correctly
/// count the number of remaining symlink traversals.
pub struct LookupContext {
    /// The SymlinkMode for the lookup.
    ///
    /// As the lookup proceeds, the follow count is decremented each time the
    /// lookup traverses a symlink.
    pub symlink_mode: SymlinkMode,

    /// The number of symlinks remaining the follow.
    ///
    /// Each time path resolution calls readlink, this value is decremented.
    pub remaining_follows: u8,

    /// Whether the result of the lookup must be a directory.
    ///
    /// For example, if the path ends with a `/` or if userspace passes
    /// O_DIRECTORY. This flag can be set to true if the lookup encounters a
    /// symlink that ends with a `/`.
    pub must_be_directory: bool,
}

impl LookupContext {
    pub fn new(symlink_mode: SymlinkMode) -> LookupContext {
        LookupContext {
            symlink_mode,
            remaining_follows: MAX_SYMLINK_FOLLOWS,
            must_be_directory: false,
        }
    }

    pub fn with(&self, symlink_mode: SymlinkMode) -> LookupContext {
        LookupContext {
            symlink_mode,
            remaining_follows: self.remaining_follows,
            must_be_directory: self.must_be_directory,
        }
    }

    pub fn update_for_path(&mut self, path: &FsStr) {
        if path.last() == Some(&b'/') {
            // The last path element must resolve to a directory. This is because a trailing slash
            // was found in the path.
            self.must_be_directory = true;
            // If the last path element is a symlink, we should follow it.
            // See https://pubs.opengroup.org/onlinepubs/9699919799/xrat/V4_xbd_chap03.html#tag_21_03_00_75
            self.symlink_mode = SymlinkMode::Follow;
        }
    }
}

impl Default for LookupContext {
    fn default() -> Self {
        LookupContext::new(SymlinkMode::Follow)
    }
}

/// Whether the path is reachable from the given root.
pub enum PathWithReachability {
    /// The path is reachable from the given root.
    Reachable(FsString),

    /// The path is not reachable from the given root.
    Unreachable(FsString),
}

impl PathWithReachability {
    pub fn into_path(self) -> FsString {
        match self {
            PathWithReachability::Reachable(path) => path,
            PathWithReachability::Unreachable(path) => path,
        }
    }
}

/// A node in a mount namespace.
///
/// This tree is a composite of the mount tree and the FsNode tree.
///
/// These nodes are used when traversing paths in a namespace in order to
/// present the client the directory structure that includes the mounted
/// filesystems.
#[derive(Clone)]
pub struct NamespaceNode {
    /// The mount where this namespace node is mounted.
    ///
    /// A given FsNode can be mounted in multiple places in a namespace. This
    /// field distinguishes between them.
    pub mount: MountInfo,

    /// The FsNode that corresponds to this namespace entry.
    pub entry: DirEntryHandle,
}

impl NamespaceNode {
    /// Create a namespace node that is not mounted in a namespace.
    pub fn new_anonymous(dir_entry: DirEntryHandle) -> Self {
        Self { mount: None.into(), entry: dir_entry }
    }

    /// Create a namespace node that is not mounted in a namespace and that refers to a node that
    /// is not rooted in a hierarchy and has no name.
    pub fn new_anonymous_unrooted(node: FsNodeHandle) -> Self {
        Self::new_anonymous(DirEntry::new_unrooted(node))
    }

    /// Create a FileObject corresponding to this namespace node.
    ///
    /// This function is the primary way of instantiating FileObjects. Each
    /// FileObject records the NamespaceNode that created it in order to
    /// remember its path in the Namespace.
    pub fn open(
        &self,
        current_task: &CurrentTask,
        flags: OpenFlags,
        check_access: bool,
    ) -> Result<FileHandle, Errno> {
        FileObject::new(
            self.entry.node.open(current_task, &self.mount, flags, check_access)?,
            self.clone(),
            flags,
        )
    }

    /// Create or open a node in the file system.
    ///
    /// Works for any type of node other than a symlink.
    ///
    /// Will return an existing node unless `flags` contains `OpenFlags::EXCL`.
    pub fn open_create_node(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        flags: OpenFlags,
    ) -> Result<NamespaceNode, Errno> {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        let create_fn = |dir: &FsNodeHandle, mount: &MountInfo, name: &_| {
            dir.mknod(current_task, mount, name, mode, dev, owner)
        };
        let entry = if flags.contains(OpenFlags::EXCL) {
            self.entry.create_entry(current_task, &self.mount, name, create_fn)
        } else {
            self.entry.get_or_create_entry(current_task, &self.mount, name, create_fn)
        }?;
        Ok(self.with_new_entry(entry))
    }

    /// Create a node in the file system.
    ///
    /// Works for any type of node other than a symlink.
    ///
    /// Does not return an existing node.
    pub fn create_node(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
    ) -> Result<NamespaceNode, Errno> {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        let entry =
            self.entry.create_entry(current_task, &self.mount, name, |dir, mount, name| {
                dir.mknod(current_task, mount, name, mode, dev, owner)
            })?;
        Ok(self.with_new_entry(entry))
    }

    /// Create a symlink in the file system.
    ///
    /// To create another type of node, use `create_node`.
    pub fn create_symlink(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        let owner = current_task.as_fscred();
        let entry =
            self.entry.create_entry(current_task, &self.mount, name, |dir, mount, name| {
                dir.create_symlink(current_task, mount, name, target, owner)
            })?;
        Ok(self.with_new_entry(entry))
    }

    /// Creates an anonymous file.
    ///
    /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
    ///
    /// Used by O_TMPFILE.
    pub fn create_tmpfile(
        &self,
        current_task: &CurrentTask,
        mode: FileMode,
        flags: OpenFlags,
    ) -> Result<NamespaceNode, Errno> {
        let owner = current_task.as_fscred();
        let mode = current_task.fs().apply_umask(mode);
        Ok(self.with_new_entry(self.entry.create_tmpfile(
            current_task,
            &self.mount,
            mode,
            owner,
            flags,
        )?))
    }

    pub fn link(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<NamespaceNode, Errno> {
        let dir_entry =
            self.entry.create_entry(current_task, &self.mount, name, |dir, mount, name| {
                dir.link(current_task, mount, name, child)
            })?;
        Ok(self.with_new_entry(dir_entry))
    }

    pub fn bind_socket(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        socket: SocketHandle,
        socket_address: SocketAddress,
        mode: FileMode,
    ) -> Result<NamespaceNode, Errno> {
        let dir_entry =
            self.entry.create_entry(current_task, &self.mount, name, |dir, mount, name| {
                let node = dir.mknod(
                    current_task,
                    mount,
                    name,
                    mode,
                    DeviceType::NONE,
                    current_task.as_fscred(),
                )?;
                if let Some(unix_socket) = socket.downcast_socket::<UnixSocket>() {
                    unix_socket.bind_socket_to_node(&socket, socket_address, &node)?;
                } else {
                    return error!(ENOTSUP);
                }
                Ok(node)
            })?;
        Ok(self.with_new_entry(dir_entry))
    }

    pub fn unlink(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        kind: UnlinkKind,
        must_be_directory: bool,
    ) -> Result<(), Errno> {
        if DirEntry::is_reserved_name(name) {
            match kind {
                UnlinkKind::Directory => {
                    if name == b".." {
                        error!(ENOTEMPTY)
                    } else if self.parent().is_none() {
                        // The client is attempting to remove the root.
                        error!(EBUSY)
                    } else {
                        error!(EINVAL)
                    }
                }
                UnlinkKind::NonDirectory => error!(ENOTDIR),
            }
        } else {
            self.entry.unlink(current_task, &self.mount, name, kind, must_be_directory)
        }
    }

    /// Traverse down a parent-to-child link in the namespace.
    pub fn lookup_child(
        &self,
        current_task: &CurrentTask,
        context: &mut LookupContext,
        basename: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        if !self.entry.node.is_dir() {
            return error!(ENOTDIR);
        }

        if basename.len() > NAME_MAX as usize {
            return error!(ENAMETOOLONG);
        }

        let child = if basename == b"." || basename == b"" {
            self.clone()
        } else if basename == b".." {
            // Make sure this can't escape a chroot
            if *self == current_task.fs().root() {
                self.clone()
            } else {
                self.parent().unwrap_or_else(|| self.clone())
            }
        } else {
            let mut child = self.with_new_entry(self.entry.component_lookup(
                current_task,
                &self.mount,
                basename,
            )?);
            while child.entry.node.is_lnk() {
                match context.symlink_mode {
                    SymlinkMode::NoFollow => {
                        break;
                    }
                    SymlinkMode::Follow => {
                        if context.remaining_follows == 0 {
                            return error!(ELOOP);
                        }
                        context.remaining_follows -= 1;
                        child = match child.readlink(current_task)? {
                            SymlinkTarget::Path(link_target) => {
                                let link_directory = if link_target[0] == b'/' {
                                    current_task.fs().root()
                                } else {
                                    self.clone()
                                };
                                current_task.lookup_path(context, link_directory, &link_target)?
                            }
                            SymlinkTarget::Node(node) => node,
                        }
                    }
                };
            }

            child.enter_mount()
        };

        if context.must_be_directory && !child.entry.node.is_dir() {
            return error!(ENOTDIR);
        }

        Ok(child)
    }

    /// Traverse up a child-to-parent link in the namespace.
    ///
    /// This traversal matches the child-to-parent link in the underlying
    /// FsNode except at mountpoints, where the link switches from one
    /// filesystem to another.
    pub fn parent(&self) -> Option<NamespaceNode> {
        let mountpoint_or_self = self.escape_mount();
        Some(mountpoint_or_self.with_new_entry(mountpoint_or_self.entry.parent()?))
    }

    /// Returns the parent, but does not escape mounts i.e. returns None if this node
    /// is the root of a mount.
    pub fn parent_within_mount(&self) -> Option<DirEntryHandle> {
        if let Ok(_) = self.mount_if_root() {
            return None;
        }
        self.entry.parent()
    }

    /// Whether this namespace node is a descendant of the given node.
    ///
    /// Walks up the namespace node tree looking for ancestor. If ancestor is
    /// found, returns true. Otherwise, returns false.
    pub fn is_descendant_of(&self, ancestor: &NamespaceNode) -> bool {
        let ancestor = ancestor.escape_mount();
        let mut current = self.escape_mount();
        while current != ancestor {
            if let Some(parent) = current.parent() {
                current = parent.escape_mount();
            } else {
                return false;
            }
        }
        true
    }

    /// If this is a mount point, return the root of the mount. Otherwise return self.
    fn enter_mount(&self) -> NamespaceNode {
        // While the child is a mountpoint, replace child with the mount's root.
        fn enter_one_mount(node: &NamespaceNode) -> Option<NamespaceNode> {
            if let Some(mount) = node.mount.deref() {
                if let Some(mount) = mount.state.read().submounts.get(ArcKey::ref_cast(&node.entry))
                {
                    return Some(mount.root());
                }
            }
            None
        }
        let mut inner = self.clone();
        while let Some(inner_root) = enter_one_mount(&inner) {
            inner = inner_root;
        }
        inner
    }

    /// If this is the root of a mount, return the mount point. Otherwise return self.
    ///
    /// This is not exactly the same as parent(). If parent() is called on a root, it will escape
    /// the mount, but then return the parent of the mount point instead of the mount point.
    fn escape_mount(&self) -> NamespaceNode {
        let mut mountpoint_or_self = self.clone();
        while let Some(mountpoint) = mountpoint_or_self.mountpoint() {
            mountpoint_or_self = mountpoint;
        }
        mountpoint_or_self
    }

    /// If this node is the root of a mount, return it. Otherwise EINVAL.
    pub fn mount_if_root(&self) -> Result<&MountHandle, Errno> {
        if let Some(mount) = self.mount.deref() {
            if Arc::ptr_eq(&self.entry, &mount.root) {
                return Ok(mount);
            }
        }
        error!(EINVAL)
    }

    /// Returns the mountpoint at this location in the namespace.
    ///
    /// If this node is mounted in another node, this function returns the node
    /// at which this node is mounted. Otherwise, returns None.
    fn mountpoint(&self) -> Option<NamespaceNode> {
        self.mount_if_root().ok()?.mountpoint()
    }

    /// The path from the task's root to this node.
    pub fn path(&self, task: &Task) -> FsString {
        self.path_from_root(Some(&task.fs().root())).into_path()
    }

    /// The path from the root of the namespace to this node.
    pub fn path_escaping_chroot(&self) -> FsString {
        self.path_from_root(None).into_path()
    }

    /// Returns the path to this node, accounting for a custom root.
    /// A task may have a custom root set by `chroot`.
    pub fn path_from_root(&self, root: Option<&NamespaceNode>) -> PathWithReachability {
        if self.mount.is_none() {
            return PathWithReachability::Reachable(self.entry.local_name().to_vec());
        }

        let mut path = PathBuilder::new();
        let mut current = self.escape_mount();
        if let Some(root) = root {
            // The current node is expected to intersect with the custom root as we travel up the tree.
            let root = root.escape_mount();
            while current != root {
                if let Some(parent) = current.parent() {
                    path.prepend_element(&current.entry.local_name());
                    current = parent.escape_mount();
                } else {
                    // This node hasn't intersected with the custom root and has reached the namespace root.
                    return PathWithReachability::Unreachable(path.build());
                }
            }
        } else {
            // No custom root, so travel up the tree to the namespace root.
            while let Some(parent) = current.parent() {
                path.prepend_element(&current.entry.local_name());
                current = parent.escape_mount();
            }
        }

        PathWithReachability::Reachable(path.build())
    }

    pub fn mount(&self, what: WhatToMount, flags: MountFlags) -> Result<(), Errno> {
        let flags = flags & (MountFlags::STORED_ON_MOUNT | MountFlags::REC);
        let mountpoint = self.enter_mount();
        let mount = mountpoint.mount.as_ref().expect("a mountpoint must be part of a mount");
        mount.create_submount(&mountpoint.entry, what, flags);
        Ok(())
    }

    /// If this is the root of a filesystem, unmount. Otherwise return EINVAL.
    pub fn unmount(&self) -> Result<(), Errno> {
        // Drop submount outside of this state lock to ensure it is not done while holding a lock.
        let _submount = {
            let propagate = {
                if let Ok(mount) = self.mount_if_root() {
                    mount.read().is_shared()
                } else {
                    false
                }
            };
            let mountpoint = self.enter_mount().mountpoint().ok_or_else(|| errno!(EINVAL))?;
            let mount = mountpoint.mount.as_ref().expect("a mountpoint must be part of a mount");
            mount.remove_submount(mountpoint.mount_hash_key(), &mountpoint.entry, propagate)?;
        };
        Ok(())
    }

    pub fn rename(
        current_task: &CurrentTask,
        old_parent: &NamespaceNode,
        old_name: &FsStr,
        new_parent: &NamespaceNode,
        new_name: &FsStr,
        flags: RenameFlags,
    ) -> Result<(), Errno> {
        DirEntry::rename(
            current_task,
            &old_parent.entry,
            &old_parent.mount,
            old_name,
            &new_parent.entry,
            &new_parent.mount,
            new_name,
            flags,
        )
    }

    fn with_new_entry(&self, entry: DirEntryHandle) -> NamespaceNode {
        NamespaceNode { mount: self.mount.clone(), entry }
    }

    fn mount_hash_key(&self) -> &ArcKey<DirEntry> {
        ArcKey::ref_cast(&self.entry)
    }

    pub fn update_atime(&self) {
        // Do not update the atime of this node if it is mounted with the NOATIME flag.
        if !self.mount.flags().contains(MountFlags::NOATIME) {
            self.entry.node.update_info(|info| {
                let now = utc::utc_now();
                info.time_access = now;
            });
        }
    }

    pub fn readlink(&self, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        self.update_atime();
        self.entry.node.readlink(current_task)
    }

    pub fn notify(&self, event_mask: InotifyMask) {
        if self.mount.is_some() {
            self.entry.notify(event_mask);
        }
    }

    /// Check whether the node can be accessed in the current context with the specified access
    /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
    /// owner or is in the file's group.
    pub fn check_access(&self, current_task: &CurrentTask, access: Access) -> Result<(), Errno> {
        self.entry.node.check_access(current_task, &self.mount, access)
    }

    pub fn truncate(&self, current_task: &CurrentTask, length: u64) -> Result<(), Errno> {
        self.entry.node.truncate(current_task, &self.mount, length)
    }
}

impl fmt::Debug for NamespaceNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamespaceNode")
            .field("path", &String::from_utf8_lossy(&self.path_escaping_chroot()))
            .field("mount", &self.mount)
            .field("entry", &self.entry)
            .finish()
    }
}

// Eq/Hash impls intended for the MOUNT_POINTS hash
impl PartialEq for NamespaceNode {
    fn eq(&self, other: &Self) -> bool {
        self.mount.as_ref().map(Arc::as_ptr).eq(&other.mount.as_ref().map(Arc::as_ptr))
            && Arc::ptr_eq(&self.entry, &other.entry)
    }
}
impl Eq for NamespaceNode {}
impl Hash for NamespaceNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.mount.as_ref().map(Arc::as_ptr).hash(state);
        Arc::as_ptr(&self.entry).hash(state);
    }
}

#[cfg(test)]
mod test {
    use crate::{
        fs::tmpfs::TmpFs,
        testing::create_kernel_and_task,
        vfs::{LookupContext, Namespace, UnlinkKind, WhatToMount},
    };
    use starnix_uapi::{errno, mount_flags::MountFlags};
    use std::sync::Arc;

    #[::fuchsia::test]
    async fn test_namespace() -> anyhow::Result<()> {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node.create_dir(&current_task, b"dev").expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node =
            dev_root_node.create_dir(&current_task, b"pts").expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");

        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev");
        let mut context = LookupContext::default();
        let pts =
            dev.lookup_child(&current_task, &mut context, b"pts").expect("failed to lookup pts");
        let pts_parent =
            pts.parent().ok_or_else(|| errno!(ENOENT)).expect("failed to get parent of pts");
        assert!(Arc::ptr_eq(&pts_parent.entry, &dev.entry));

        let dev_parent =
            dev.parent().ok_or_else(|| errno!(ENOENT)).expect("failed to get parent of dev");
        assert!(Arc::ptr_eq(&dev_parent.entry, &ns.root().entry));
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_mount_does_not_upgrade() -> anyhow::Result<()> {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node.create_dir(&current_task, b"dev").expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node =
            dev_root_node.create_dir(&current_task, b"pts").expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");
        let mut context = LookupContext::default();
        let new_dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev again");
        assert!(!Arc::ptr_eq(&dev.entry, &new_dev.entry));
        assert_ne!(&dev, &new_dev);

        let mut context = LookupContext::default();
        let _new_pts = new_dev
            .lookup_child(&current_task, &mut context, b"pts")
            .expect("failed to lookup pts");
        let mut context = LookupContext::default();
        assert!(dev.lookup_child(&current_task, &mut context, b"pts").is_err());

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_path() -> anyhow::Result<()> {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let root_node = Arc::clone(root_fs.root());
        let _dev_node = root_node.create_dir(&current_task, b"dev").expect("failed to mkdir dev");
        let dev_fs = TmpFs::new_fs(&kernel);
        let dev_root_node = Arc::clone(dev_fs.root());
        let _dev_pts_node =
            dev_root_node.create_dir(&current_task, b"pts").expect("failed to mkdir pts");

        let ns = Namespace::new(root_fs);
        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev");
        dev.mount(WhatToMount::Fs(dev_fs), MountFlags::empty())
            .expect("failed to mount dev root node");

        let mut context = LookupContext::default();
        let dev = ns
            .root()
            .lookup_child(&current_task, &mut context, b"dev")
            .expect("failed to lookup dev");
        let mut context = LookupContext::default();
        let pts =
            dev.lookup_child(&current_task, &mut context, b"pts").expect("failed to lookup pts");

        assert_eq!(b"/".to_vec(), ns.root().path_escaping_chroot());
        assert_eq!(b"/dev".to_vec(), dev.path_escaping_chroot());
        assert_eq!(b"/dev/pts".to_vec(), pts.path_escaping_chroot());
        Ok(())
    }

    #[::fuchsia::test]
    async fn test_shadowing() -> anyhow::Result<()> {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let ns = Namespace::new(root_fs.clone());
        let _foo_node = root_fs.root().create_dir(&current_task, b"foo")?;
        let mut context = LookupContext::default();
        let foo_dir = ns.root().lookup_child(&current_task, &mut context, b"foo")?;

        let foofs1 = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs1.clone()), MountFlags::empty())?;
        let mut context = LookupContext::default();
        assert!(Arc::ptr_eq(
            &ns.root().lookup_child(&current_task, &mut context, b"foo")?.entry,
            foofs1.root()
        ));
        let foo_dir = ns.root().lookup_child(&current_task, &mut context, b"foo")?;

        let ns_clone = ns.clone_namespace();

        let foofs2 = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs2.clone()), MountFlags::empty())?;
        let mut context = LookupContext::default();
        assert!(Arc::ptr_eq(
            &ns.root().lookup_child(&current_task, &mut context, b"foo")?.entry,
            foofs2.root()
        ));

        assert!(Arc::ptr_eq(
            &ns_clone
                .root()
                .lookup_child(&current_task, &mut LookupContext::default(), b"foo")?
                .entry,
            foofs1.root()
        ));

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_unlink_mounted_directory() -> anyhow::Result<()> {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let ns1 = Namespace::new(root_fs.clone());
        let ns2 = Namespace::new(root_fs.clone());
        let _foo_node = root_fs.root().create_dir(&current_task, b"foo")?;
        let mut context = LookupContext::default();
        let foo_dir = ns1.root().lookup_child(&current_task, &mut context, b"foo")?;

        let foofs = TmpFs::new_fs(&kernel);
        foo_dir.mount(WhatToMount::Fs(foofs), MountFlags::empty())?;

        assert_eq!(
            errno!(EBUSY),
            ns2.root().unlink(&current_task, b"foo", UnlinkKind::Directory, false).unwrap_err()
        );

        Ok(())
    }
}
