// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{vmo::round_up_to_increment, PAGE_SIZE},
    mutable_state::Guard,
    task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter},
    vfs::{
        buffers::{Buffer, InputBuffer, InputBufferExt as _, OutputBuffer, OutputBufferCallback},
        default_eof_offset, default_fcntl, default_ioctl, default_seek, fileops_impl_nonseekable,
        fs_args, fs_node_impl_dir_readonly, CacheConfig, CacheMode, DirEntry, DirectoryEntryType,
        DirentSink, DynamicFile, DynamicFileBuf, DynamicFileSource, FallocMode, FdNumber,
        FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions,
        FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString, PeekBufferSegmentsCallback,
        SeekTarget, SimpleFileNode, StaticDirectoryBuilder, SymlinkTarget, ValueOrSize,
        VecDirectory, VecDirectoryEntry, XattrOp,
    },
};
use bstr::B;
use fuchsia_zircon as zx;
use starnix_lifecycle::AtomicU64Counter;
use starnix_logging::{log_error, log_trace, log_warn, track_stub};
use starnix_sync::{
    AtomicTime, DeviceOpen, FileOpsCore, FsNodeAllocate, Locked, Mutex, MutexGuard, RwLock,
    RwLockReadGuard, RwLockWriteGuard, Unlocked, WriteOps,
};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_uapi::{
    auth::FsCred,
    device_type::DeviceType,
    errno, errno_from_code, error,
    errors::{Errno, EINTR, EINVAL, ENOSYS},
    file_mode::{Access, FileMode},
    mode, off_t,
    open_flags::OpenFlags,
    statfs,
    time::{duration_from_timespec, time_from_timespec},
    uapi,
    vfs::{default_statfs, FdEvents},
    FUSE_SUPER_MAGIC,
};
use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
use zerocopy::{AsBytes, FromBytes, NoCell};

const CONFIGURATION_AVAILABLE_EVENT: u64 = u64::MAX;

#[derive(Debug)]
pub struct DevFuse {
    connection: Arc<FuseConnection>,
}

pub fn open_fuse_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    let fusectl_fs = fusectl_fs(current_task);
    let connection = fusectl_fs.new_connection(current_task);
    Ok(Box::new(DevFuse { connection }))
}

fn attr_valid_to_duration(attr_valid: u64, attr_valid_nsec: u32) -> Result<zx::Duration, Errno> {
    duration_from_timespec(uapi::timespec {
        tv_sec: i64::try_from(attr_valid).unwrap_or(i64::MAX),
        tv_nsec: attr_valid_nsec.into(),
    })
}

impl FileOps for DevFuse {
    fileops_impl_nonseekable!();

    fn close(&self, _file: &FileObject, _current_task: &CurrentTask) {
        self.connection.lock().disconnect();
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN, None, || self.connection.lock().read(data))
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        self.connection.lock().write(data)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.connection.lock().wait_async(waiter, events, handler)
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.connection.lock().query_events())
    }
}

pub fn new_fuse_fs(
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let mut mount_options = fs_args::generic_parse_mount_options(options.params.as_ref())?;
    let fd = fs_args::parse::<FdNumber>(
        mount_options.remove(B("fd")).ok_or_else(|| errno!(EINVAL))?.as_ref(),
    )?;
    let default_permissions = mount_options.remove(B("default_permissions")).is_some().into();
    let connection = current_task
        .files
        .get(fd)?
        .downcast_file::<DevFuse>()
        .ok_or_else(|| errno!(EINVAL))?
        .connection
        .clone();

    let fs = FileSystem::new(
        current_task.kernel(),
        CacheMode::Cached(CacheConfig::default()),
        FuseFs { connection: connection.clone(), default_permissions },
        options,
    );
    let fuse_node = FuseNode::new(connection.clone(), uapi::FUSE_ROOT_ID as u64);
    fuse_node.state.lock().nlookup += 1;

    let mut root_node = FsNode::new_root(fuse_node.clone());
    root_node.node_id = uapi::FUSE_ROOT_ID as u64;
    fs.set_root_node(root_node);
    {
        let mut state = connection.lock();
        state.connect();
        state.execute_operation(
            current_task,
            &fuse_node,
            FuseOperation::Init { fs: Arc::downgrade(&fs) },
        )?;
    }
    Ok(fs)
}

fn fusectl_fs(current_task: &CurrentTask) -> &Arc<FuseCtlFs> {
    current_task.kernel().fusectl_fs.get_or_init(|| Default::default())
}

pub fn new_fusectl_fs(
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let fs = FileSystem::new(
        current_task.kernel(),
        CacheMode::Uncached,
        Arc::clone(fusectl_fs(current_task)),
        options,
    );
    let root_node = FsNode::new_root_with_properties(FuseCtlConnectionsDirectory {}, |info| {
        info.chmod(mode!(IFDIR, 0o755));
    });
    fs.set_root_node(root_node);
    Ok(fs)
}

#[derive(Debug)]
struct FuseFs {
    connection: Arc<FuseConnection>,
    default_permissions: AtomicBool,
}

impl FuseFs {
    fn from_fs(fs: &FileSystem) -> Result<&FuseFs, Errno> {
        fs.downcast_ops::<FuseFs>().ok_or_else(|| errno!(ENOENT))
    }
}

impl FileSystemOps for FuseFs {
    fn rename(
        &self,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn generate_node_ids(&self) -> bool {
        true
    }

    fn statfs(&self, fs: &FileSystem, current_task: &CurrentTask) -> Result<statfs, Errno> {
        let node = if let Ok(node) = FuseNode::from_node(&fs.root().node) {
            node
        } else {
            log_error!("Unexpected file type");
            return error!(EINVAL);
        };
        let response =
            self.connection.lock().execute_operation(current_task, node, FuseOperation::Statfs)?;
        let statfs_out = if let FuseResponse::Statfs(statfs_out) = response {
            statfs_out
        } else {
            return error!(EINVAL);
        };
        Ok(statfs {
            f_type: FUSE_SUPER_MAGIC as i64,
            f_blocks: statfs_out.st.blocks.try_into().map_err(|_| errno!(EINVAL))?,
            f_bfree: statfs_out.st.bfree.try_into().map_err(|_| errno!(EINVAL))?,
            f_bavail: statfs_out.st.bavail.try_into().map_err(|_| errno!(EINVAL))?,
            f_files: statfs_out.st.files.try_into().map_err(|_| errno!(EINVAL))?,
            f_ffree: statfs_out.st.ffree.try_into().map_err(|_| errno!(EINVAL))?,
            f_bsize: statfs_out.st.bsize.try_into().map_err(|_| errno!(EINVAL))?,
            f_namelen: statfs_out.st.namelen.try_into().map_err(|_| errno!(EINVAL))?,
            f_frsize: statfs_out.st.frsize.try_into().map_err(|_| errno!(EINVAL))?,
            ..statfs::default()
        })
    }
    fn name(&self) -> &'static FsStr {
        "fuse".into()
    }
    fn unmount(&self) {
        self.connection.lock().disconnect();
    }
}

#[derive(Debug, Default)]
pub struct FuseCtlFs {
    connections: Mutex<Vec<Weak<FuseConnection>>>,
    next_identifier: AtomicU64Counter,
}

impl FuseCtlFs {
    fn new_connection(&self, current_task: &CurrentTask) -> Arc<FuseConnection> {
        let connection = Arc::new(FuseConnection {
            id: self.next_identifier.next(),
            creds: current_task.as_fscred(),
            state: Default::default(),
        });
        self.connections.lock().push(Arc::downgrade(&connection));
        connection
    }

    fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(Arc<FuseConnection>),
    {
        self.connections.lock().retain(|connection| {
            if let Some(connection) = connection.upgrade() {
                f(connection);
                true
            } else {
                false
            }
        });
    }
}

impl FileSystemOps for Arc<FuseCtlFs> {
    fn rename(
        &self,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        // Magic number has been extracted from the stat utility.
        const FUSE_CTL_MAGIC: u32 = 0x65735543;
        Ok(default_statfs(FUSE_CTL_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        "fusectl".into()
    }
}

#[derive(Debug)]
struct FuseCtlConnectionsDirectory;

impl FsNodeOps for FuseCtlConnectionsDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let fs = fusectl_fs(current_task);
        let mut entries = vec![];
        fs.for_each(|connection| {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::DIR,
                name: connection.id.to_string().into(),
                inode: None,
            });
        });
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let name = std::str::from_utf8(name).map_err(|_| errno!(ENOENT))?;
        let id = name.parse::<u64>().map_err(|_| errno!(ENOENT))?;
        let fs = fusectl_fs(current_task);
        let mut connection = None;
        fs.for_each(|c| {
            if c.id == id {
                connection = Some(c);
            }
        });
        let Some(connection) = connection else {
            return error!(ENOENT);
        };
        let fs = node.fs();
        let mut dir = StaticDirectoryBuilder::new(&fs);
        dir.set_mode(mode!(IFDIR, 0o500));
        dir.dir_creds(connection.creds);
        dir.node(
            "abort",
            fs.create_node(
                current_task,
                AbortFile::new_node(connection.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o200), connection.creds),
            ),
        );
        dir.node(
            "waiting",
            fs.create_node(
                current_task,
                WaitingFile::new_node(connection.clone()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o400), connection.creds),
            ),
        );

        Ok(dir.build(current_task))
    }
}

#[derive(Debug)]
struct AbortFile {
    connection: Arc<FuseConnection>,
}

impl AbortFile {
    fn new_node(connection: Arc<FuseConnection>) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(Self { connection: connection.clone() }))
    }
}

impl FileOps for AbortFile {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let drained = data.drain();
        if drained > 0 {
            self.connection.lock().disconnect();
        }
        Ok(drained)
    }
}

#[derive(Clone, Debug)]
struct WaitingFile {
    connection: Arc<FuseConnection>,
}

impl WaitingFile {
    fn new_node(connection: Arc<FuseConnection>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { connection })
    }
}

impl DynamicFileSource for WaitingFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let value = {
            let state = self.connection.state.lock();
            state.operations.len() + state.message_queue.len()
        };
        let value = format!("{value}\n");
        sink.write(value.as_bytes());
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FuseNodeMutableState {
    nlookup: u64,
}

#[derive(Debug)]
struct FuseNode {
    connection: Arc<FuseConnection>,
    nodeid: u64,
    attributes_valid_until: AtomicTime,
    state: Mutex<FuseNodeMutableState>,
}

impl FuseNode {
    fn new(connection: Arc<FuseConnection>, nodeid: u64) -> Arc<Self> {
        Arc::new(Self {
            connection,
            nodeid,
            attributes_valid_until: zx::Time::INFINITE_PAST.into(),
            state: Default::default(),
        })
    }

    fn from_node(node: &FsNode) -> Result<&Arc<FuseNode>, Errno> {
        node.downcast_ops::<Arc<FuseNode>>().ok_or_else(|| errno!(ENOENT))
    }

    fn refresh_expired_node_attributes<'a>(
        &self,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        // Relaxed because the attributes valid until atomic is not used to synchronize
        // anything. Its final access is protected by the info lock anyways.
        const VALID_UNTIL_LOAD_ORDERING: Ordering = Ordering::Relaxed;

        let now = zx::Time::get_monotonic();
        if self.attributes_valid_until.load(VALID_UNTIL_LOAD_ORDERING) >= now {
            let info = info.read();

            // Check the valid_until again after taking the info lock to make sure
            // that the attributes are still valid. We do this because after we
            // checked the first time and now, the node's attributes could have been
            // invalidated or expired.
            //
            // But why not only check if the attributes are valid after taking the
            // lock? Because when we will impact FUSE implementations that don't
            // support caching, or caching with small valid durations, by always
            // taking a read lock which we then drop to acquire a write lock. We
            // slightly pessimize the "happy" path with an extra atomic load so
            // that we don't overly pessimize the uncached/"slower" path.
            if self.attributes_valid_until.load(VALID_UNTIL_LOAD_ORDERING) >= now {
                return Ok(info);
            }
        }

        // Force a refresh of our cached attributes.
        self.fetch_and_refresh_info_impl(current_task, info)
    }

    fn fetch_and_refresh_info_impl<'a>(
        &self,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let response =
            self.connection.lock().execute_operation(current_task, self, FuseOperation::GetAttr)?;
        let uapi::fuse_attr_out { attr_valid, attr_valid_nsec, attr, .. } =
            if let FuseResponse::Attr(attr) = response {
                attr
            } else {
                return error!(EINVAL);
            };
        let mut info = info.write();
        FuseNode::set_node_info(
            &mut info,
            attr,
            attr_valid_to_duration(attr_valid, attr_valid_nsec)?,
            &self.attributes_valid_until,
        )?;
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn set_node_info(
        info: &mut FsNodeInfo,
        attributes: uapi::fuse_attr,
        attr_valid_duration: zx::Duration,
        node_attributes_valid_until: &AtomicTime,
    ) -> Result<(), Errno> {
        info.ino = attributes.ino as uapi::ino_t;
        info.mode = FileMode::from_bits(attributes.mode);
        info.size = attributes.size.try_into().map_err(|_| errno!(EINVAL))?;
        info.blocks = attributes.blocks.try_into().map_err(|_| errno!(EINVAL))?;
        info.blksize = attributes.blksize.try_into().map_err(|_| errno!(EINVAL))?;
        info.uid = attributes.uid;
        info.gid = attributes.gid;
        info.link_count = attributes.nlink.try_into().map_err(|_| errno!(EINVAL))?;
        info.time_status_change = time_from_timespec(uapi::timespec {
            tv_sec: attributes.ctime as i64,
            tv_nsec: attributes.ctimensec as i64,
        })?;
        info.time_access = time_from_timespec(uapi::timespec {
            tv_sec: attributes.atime as i64,
            tv_nsec: attributes.atimensec as i64,
        })?;
        info.time_modify = time_from_timespec(uapi::timespec {
            tv_sec: attributes.mtime as i64,
            tv_nsec: attributes.mtimensec as i64,
        })?;
        info.rdev = DeviceType::from_bits(attributes.rdev as u64);

        node_attributes_valid_until.store(zx::Time::after(attr_valid_duration), Ordering::Relaxed);
        Ok(())
    }

    /// Build a FsNodeHandle from a FuseResponse that is expected to be a FuseResponse::Entry.
    fn fs_node_from_entry(
        &self,
        current_task: &CurrentTask,
        node: &FsNode,
        name: &FsStr,
        response: FuseResponse,
    ) -> Result<FsNodeHandle, Errno> {
        let entry = if let FuseResponse::Entry(entry) = response {
            entry
        } else {
            return error!(EINVAL);
        };
        if entry.nodeid == 0 {
            return error!(ENOENT);
        }
        let node = node.fs().get_or_create_node(current_task, Some(entry.nodeid), |id| {
            let fuse_node = FuseNode::new(self.connection.clone(), entry.nodeid);
            let mut info = FsNodeInfo::default();
            FuseNode::set_node_info(
                &mut info,
                entry.attr,
                attr_valid_to_duration(entry.attr_valid, entry.attr_valid_nsec)?,
                &fuse_node.attributes_valid_until,
            )?;
            Ok(FsNode::new_uncached(current_task, fuse_node, &node.fs(), id, info))
        })?;
        // . and .. do not get their lookup count increased.
        if !DirEntry::is_reserved_name(name) {
            let fuse_node = FuseNode::from_node(&node)?;
            fuse_node.state.lock().nlookup += 1;
        }
        Ok(node)
    }
}

struct FuseFileObject {
    connection: Arc<FuseConnection>,
    /// The response to the open calls from the userspace process.
    open_out: uapi::fuse_open_out,
}

impl FuseFileObject {
    /// Returns the `FuseNode` associated with the opened file.
    fn get_fuse_node<'a>(&self, file: &'a FileObject) -> Result<&'a Arc<FuseNode>, Errno> {
        FuseNode::from_node(file.node())
    }
}

impl FileOps for FuseFileObject {
    fn close(&self, file: &FileObject, current_task: &CurrentTask) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        let mode = file.node().info().mode;
        if let Err(e) = self.connection.lock().execute_operation(
            current_task,
            node,
            FuseOperation::Release { flags: file.flags(), mode, open_out: self.open_out },
        ) {
            log_error!("Error when relasing fh: {e:?}");
        }
    }

    fn flush(&self, file: &FileObject, current_task: &CurrentTask) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        if let Err(e) = self.connection.lock().execute_operation(
            current_task,
            node,
            FuseOperation::Flush(self.open_out),
        ) {
            log_error!("Error when flushing fh: {e:?}");
        }
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let response = self.connection.lock().execute_operation(
            current_task,
            node,
            FuseOperation::Read(uapi::fuse_read_in {
                fh: self.open_out.fh,
                offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                size: data.available().try_into().unwrap_or(u32::MAX),
                read_flags: 0,
                lock_owner: 0,
                flags: 0,
                padding: 0,
            }),
        )?;
        let read_out = if let FuseResponse::Read(read_out) = response {
            read_out
        } else {
            return error!(EINVAL);
        };
        data.write(&read_out)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let content = data.peek_all()?;
        let response = self.connection.lock().execute_operation(
            current_task,
            node,
            FuseOperation::Write {
                write_in: uapi::fuse_write_in {
                    fh: self.open_out.fh,
                    offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                    size: content.len().try_into().map_err(|_| errno!(EINVAL))?,
                    write_flags: 0,
                    lock_owner: 0,
                    flags: 0,
                    padding: 0,
                },
                content,
            },
        )?;
        let write_out = if let FuseResponse::Write(write_out) = response {
            write_out
        } else {
            return error!(EINVAL);
        };

        let written = write_out.size as usize;

        data.advance(written)?;
        Ok(written)
    }

    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        // Only delegate SEEK_DATA and SEEK_HOLE to the userspace process.
        if matches!(target, SeekTarget::Data(_) | SeekTarget::Hole(_)) {
            let node = self.get_fuse_node(file)?;
            let response = self.connection.lock().execute_operation(
                current_task,
                node,
                FuseOperation::Seek(uapi::fuse_lseek_in {
                    fh: self.open_out.fh,
                    offset: target.offset().try_into().map_err(|_| errno!(EINVAL))?,
                    whence: target.whence(),
                    padding: 0,
                }),
            );
            match response {
                Ok(response) => {
                    let seek_out = if let FuseResponse::Seek(seek_out) = response {
                        seek_out
                    } else {
                        return error!(EINVAL);
                    };
                    return seek_out.offset.try_into().map_err(|_| errno!(EINVAL));
                }
                // If errno is ENOSYS, the userspace process doesn't support the seek operation and
                // the default seek must be used.
                Err(errno) if errno == ENOSYS => {}
                Err(errno) => return Err(errno),
            };
        }

        default_seek(current_offset, target, |offset| {
            let eof_offset = default_eof_offset(file, current_task)?;
            offset.checked_add(eof_offset).ok_or_else(|| errno!(EINVAL))
        })
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        None
    }

    fn query_events(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let node = self.get_fuse_node(file)?;
        let response = self.connection.lock().execute_operation(
            current_task,
            node,
            FuseOperation::Poll(uapi::fuse_poll_in {
                fh: self.open_out.fh,
                kh: 0,
                flags: 0,
                events: FdEvents::all().bits(),
            }),
        )?;
        let poll_out = if let FuseResponse::Poll(poll_out) = response {
            poll_out
        } else {
            return error!(EINVAL);
        };
        FdEvents::from_bits(poll_out.revents).ok_or_else(|| errno!(EINVAL))
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        let mut state = self.connection.lock();
        let configuration = state.get_configuration(current_task)?;
        let use_readdirplus = {
            if configuration.flags.contains(FuseInitFlags::DO_READDIRPLUS) {
                if configuration.flags.contains(FuseInitFlags::READDIRPLUS_AUTO) {
                    sink.offset() == 0
                } else {
                    true
                }
            } else {
                false
            }
        };
        // Request a number of bytes related to the user capacity. If none is given, default to a
        // single page of data.
        let user_capacity = if let Some(base_user_capacity) = sink.user_capacity() {
            if use_readdirplus {
                // Add some amount of capacity for the entries.
                base_user_capacity * 3 / 2
            } else {
                base_user_capacity
            }
        } else {
            *PAGE_SIZE as usize
        };
        let node = self.get_fuse_node(file)?;
        let response = state.execute_operation(
            current_task,
            node,
            FuseOperation::Readdir {
                read_in: uapi::fuse_read_in {
                    fh: self.open_out.fh,
                    offset: sink.offset().try_into().map_err(|_| errno!(EINVAL))?,
                    size: user_capacity.try_into().map_err(|_| errno!(EINVAL))?,
                    read_flags: 0,
                    lock_owner: 0,
                    flags: 0,
                    padding: 0,
                },
                use_readdirplus,
            },
        )?;
        std::mem::drop(state);
        let dirents = if let FuseResponse::Readdir(dirents) = response {
            dirents
        } else {
            return error!(EINVAL);
        };
        let mut sink_result = Ok(());
        for (dirent, name, entry) in dirents {
            if let Some(entry) = entry {
                // nodeid == 0 means the server doesn't want to send entry info.
                if entry.nodeid != 0 {
                    if let Err(e) = node.fs_node_from_entry(
                        current_task,
                        file.node(),
                        name.as_ref(),
                        FuseResponse::Entry(entry),
                    ) {
                        log_error!("Unable to prefill entry: {e:?}");
                    }
                }
            }
            if sink_result.is_ok() {
                sink_result = sink.add(
                    dirent.ino,
                    dirent.off.try_into().map_err(|_| errno!(EINVAL))?,
                    DirectoryEntryType::from_bits(
                        dirent.type_.try_into().map_err(|_| errno!(EINVAL))?,
                    ),
                    name.as_ref(),
                );
            }
        }
        sink_result
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        track_stub!(TODO("https://fxbug.dev/322875259"), "fuse ioctl");
        default_ioctl(file, current_task, request, arg)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        track_stub!(TODO("https://fxbug.dev/322875764"), "fuse fcntl");
        default_fcntl(cmd)
    }
}

// `FuseFs.default_permissions` is not used to synchronize anything.
const DEFAULT_PERMISSIONS_ATOMIC_ORDERING: Ordering = Ordering::Relaxed;

impl FsNodeOps for Arc<FuseNode> {
    fn check_access(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        access: Access,
        info: &RwLock<FsNodeInfo>,
    ) -> Result<(), Errno> {
        if FuseFs::from_fs(&node.fs())?
            .default_permissions
            .load(DEFAULT_PERMISSIONS_ATOMIC_ORDERING)
        {
            let info = self.refresh_expired_node_attributes(current_task, info)?;
            return node.default_check_access_impl(current_task, access, info);
        }

        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Access { mask: (access & Access::ACCESS_MASK).bits() },
        )?;
        if let FuseResponse::Access(result) = response {
            result
        } else {
            error!(EINVAL)
        }
    }

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // The node already exists. The creation has been handled before calling this method.
        let flags = flags & !(OpenFlags::CREAT | OpenFlags::EXCL);
        let mode = node.info().mode;
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Open { flags, mode },
        )?;
        let open_out = if let FuseResponse::Open(open_out) = response {
            open_out
        } else {
            return error!(EINVAL);
        };
        Ok(Box::new(FuseFileObject { connection: self.connection.clone(), open_out }))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Lookup { name: name.to_owned() },
        )?;
        self.fs_node_from_entry(current_task, node, name, response)
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Mknod {
                mknod_in: uapi::fuse_mknod_in {
                    mode: mode.bits(),
                    rdev: dev.bits() as u32,
                    umask: current_task.fs().umask().bits(),
                    padding: 0,
                },
                name: name.to_owned(),
            },
        )?;
        self.fs_node_from_entry(current_task, node, name, response)
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Mkdir {
                mkdir_in: uapi::fuse_mkdir_in {
                    mode: mode.bits(),
                    umask: current_task.fs().umask().bits(),
                },
                name: name.to_owned(),
            },
        )?;
        self.fs_node_from_entry(current_task, node, name, response)
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Symlink { target: target.to_owned(), name: name.to_owned() },
        )?;
        self.fs_node_from_entry(current_task, node, name, response)
    }

    fn readlink(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::Readlink,
        )?;
        let read_out = if let FuseResponse::Read(read_out) = response {
            read_out
        } else {
            return error!(EINVAL);
        };
        Ok(SymlinkTarget::Path(read_out.into()))
    }

    fn link(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let child_node = FuseNode::from_node(child)?;
        self.connection
            .lock()
            .execute_operation(
                current_task,
                self,
                FuseOperation::Link {
                    link_in: uapi::fuse_link_in { oldnodeid: child_node.nodeid },
                    name: name.to_owned(),
                },
            )
            .map(|_| ())
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        self.connection
            .lock()
            .execute_operation(current_task, self, FuseOperation::Unlink { name: name.to_owned() })
            .map(|_| ())
    }

    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        node.update_info(|info| {
            // Truncate is implemented by updating the attributes of the file.
            let attributes = uapi::fuse_setattr_in {
                size: length,
                valid: uapi::FATTR_SIZE,
                ..Default::default()
            };

            let response = self.connection.lock().execute_operation(
                current_task,
                self,
                FuseOperation::SetAttr(attributes),
            )?;
            let uapi::fuse_attr_out { attr_valid, attr_valid_nsec, attr, .. } =
                if let FuseResponse::Attr(attr) = response {
                    attr
                } else {
                    return error!(EINVAL);
                };
            FuseNode::set_node_info(
                info,
                attr,
                attr_valid_to_duration(attr_valid, attr_valid_nsec)?,
                &self.attributes_valid_until,
            )?;
            Ok(())
        })
    }

    fn allocate(
        &self,
        _locked: &mut Locked<'_, FsNodeAllocate>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _mode: FallocMode,
        _offset: u64,
        _length: u64,
    ) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/322875414"), "FsNodeOps::allocate");
        error!(ENOTSUP)
    }

    fn fetch_and_refresh_info<'a>(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        self.fetch_and_refresh_info_impl(current_task, info)
    }

    fn get_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::GetXAttr {
                getxattr_in: uapi::fuse_getxattr_in {
                    size: max_size.try_into().map_err(|_| errno!(EINVAL))?,
                    padding: 0,
                },
                name: name.to_owned(),
            },
        )?;
        if let FuseResponse::GetXAttr(result) = response {
            Ok(result)
        } else {
            error!(EINVAL)
        }
    }

    fn set_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno> {
        let mut state = self.connection.lock();
        let configuration = state.get_configuration(current_task)?;
        state.execute_operation(
            current_task,
            self,
            FuseOperation::SetXAttr {
                setxattr_in: uapi::fuse_setxattr_in {
                    size: value.len().try_into().map_err(|_| errno!(EINVAL))?,
                    flags: op.into_flags(),
                    setxattr_flags: 0,
                    padding: 0,
                },
                is_ext: configuration.flags.contains(FuseInitFlags::SETXATTR_EXT),
                name: name.to_owned(),
                value: value.to_owned(),
            },
        )?;
        Ok(())
    }

    fn remove_xattr(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<(), Errno> {
        self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::RemoveXAttr { name: name.to_owned() },
        )?;
        Ok(())
    }

    fn list_xattrs(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        let response = self.connection.lock().execute_operation(
            current_task,
            self,
            FuseOperation::ListXAttr(uapi::fuse_getxattr_in {
                size: max_size.try_into().map_err(|_| errno!(EINVAL))?,
                padding: 0,
            }),
        )?;
        if let FuseResponse::GetXAttr(result) = response {
            Ok(result.map(|s| {
                let mut result = s.split(|c| *c == 0).map(FsString::from).collect::<Vec<_>>();
                // The returned string ends with a '\0', so the split ends with an empty value that
                // needs to be removed.
                result.pop();
                result
            }))
        } else {
            error!(EINVAL)
        }
    }

    fn forget(&self, _node: &FsNode, current_task: &CurrentTask) -> Result<(), Errno> {
        let nlookup = self.state.lock().nlookup;
        let mut state = self.connection.lock();
        if !state.is_connected() {
            return Ok(());
        }
        if nlookup > 0 {
            state.execute_operation(
                current_task,
                self,
                FuseOperation::Forget(uapi::fuse_forget_in { nlookup }),
            )?;
        };
        Ok(())
    }
}

/// The state of the connection to the /dev/fuse file descriptor.
#[derive(Debug, Default)]
enum FuseConnectionState {
    #[default]
    /// The /dev/fuse device has been opened, but the filesystem has not been mounted yet.
    Waiting,
    /// The file system is mounted.
    Connected,
    /// The file system has been unmounted.
    Disconnected,
}

#[derive(Debug)]
struct FuseConnection {
    /// Connection identifier for fusectl.
    id: u64,

    /// Credentials of the task that opened the connection.
    creds: FsCred,

    /// Mutable state of the connection.
    state: Mutex<FuseMutableState>,
}

type FuseMutableStateGuard<'a> = Guard<'a, FuseConnection, MutexGuard<'a, FuseMutableState>>;

impl FuseConnection {
    fn lock(&self) -> FuseMutableStateGuard<'_> {
        FuseMutableStateGuard::new(self, self.state.lock())
    }
}

#[derive(Clone, Copy, Debug)]
struct FuseConfiguration {
    flags: FuseInitFlags,
}

impl TryFrom<uapi::fuse_init_out> for FuseConfiguration {
    type Error = Errno;
    fn try_from(init_out: uapi::fuse_init_out) -> Result<Self, Errno> {
        let unknown_flags = init_out.flags & !FuseInitFlags::all().bits();
        if unknown_flags != 0 {
            track_stub!(
                TODO("https://fxbug.dev/322875725"),
                "FUSE unknown init flags",
                unknown_flags
            );
            log_warn!("FUSE daemon requested unknown flags in init: {unknown_flags}");
        }
        let flags = FuseInitFlags::from_bits_truncate(init_out.flags);
        Ok(Self { flags })
    }
}

/// A per connection state for operations that can be shortcircuited.
///
/// For a number of Fuse operation, Fuse protocol specifies that if they fail in a specific way,
/// they should not be sent to the server again and must be handled in a predefined way. This
/// map keep track of these operations for a given connection. If this map contains a result for a
/// given opcode, any further attempt to send this opcode to userspace will be answered with the
/// content of the map.
type OperationsState = HashMap<uapi::fuse_opcode, Result<FuseResponse, Errno>>;

#[derive(Debug, Default)]
struct FuseMutableState {
    /// The state of the mount.
    state: FuseConnectionState,

    /// Last unique id used to identify messages between the kernel and user space.
    last_unique_id: u64,

    /// The configuration, negotiated with the client.
    configuration: Option<FuseConfiguration>,

    /// In progress operations.
    operations: HashMap<u64, RunningOperation>,

    /// Enqueued messages. These messages have not yet been sent to userspace. There should be
    /// multiple queues, but for now, push every messages to the same queue.
    /// New messages are added at the end of the queues. Read consume from the front of the queue.
    message_queue: VecDeque<FuseKernelMessage>,

    /// Queue to notify of new messages.
    waiters: WaitQueue,

    /// The state of the different operations, to allow short-circuiting the userspace process.
    operations_state: OperationsState,
}

impl<'a> FuseMutableStateGuard<'a> {
    fn wait_for_configuration<T>(
        &mut self,
        current_task: &CurrentTask,
        f: impl Fn(&FuseConfiguration) -> T,
    ) -> Result<T, Errno> {
        if let Some(configuration) = self.configuration.as_ref() {
            return Ok(f(configuration));
        }
        loop {
            if !self.is_connected() {
                return error!(ECONNABORTED);
            }
            let waiter = Waiter::new();
            self.waiters.wait_async_value(&waiter, CONFIGURATION_AVAILABLE_EVENT);
            if let Some(configuration) = self.configuration.as_ref() {
                return Ok(f(configuration));
            }
            Self::unlocked(self, || waiter.wait(current_task))?;
        }
    }

    fn get_configuration(
        &mut self,
        current_task: &CurrentTask,
    ) -> Result<FuseConfiguration, Errno> {
        self.wait_for_configuration(current_task, Clone::clone)
    }

    fn wait_for_configuration_ready(&mut self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.wait_for_configuration(current_task, |_| ())
    }

    /// Execute the given operation on the `node`. If the operation is not asynchronous, this
    /// method will wait on the userspace process for a response. If the operation is interrupted,
    /// an interrupt will be sent to the userspace process and the operation will then block until
    /// the initial operation has a response. This block can only be interrupted by the filesystem
    /// being unmounted.
    fn execute_operation(
        &mut self,
        current_task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
    ) -> Result<FuseResponse, Errno> {
        // Block until we have a valid configuration to make sure that the FUSE
        // implementation has initialized, indicated by its response to the
        // `FUSE_INIT` request. Obviously, we skip this check for the `FUSE_INIT`
        // request itself.
        if !matches!(operation, FuseOperation::Init { .. }) {
            self.wait_for_configuration_ready(current_task)?;
        }

        if let Some(result) = self.operations_state.get(&operation.opcode()) {
            return result.clone();
        }
        if !operation.has_response() {
            self.queue_operation(current_task, node, operation, None)?;
            return Ok(FuseResponse::None);
        }
        let waiter = Waiter::new();
        let is_async = operation.is_async();
        let unique_id = self.queue_operation(current_task, node, operation, Some(&waiter))?;
        if is_async {
            return Ok(FuseResponse::None);
        }
        let mut first_loop = true;
        loop {
            if let Some(response) = self.get_response(unique_id) {
                return response;
            }
            match Self::unlocked(self, || waiter.wait(current_task)) {
                Ok(()) => {}
                Err(e) if e == EINTR => {
                    // If interrupted by another process, send an interrupt command to the server
                    // the first time, then wait unconditionally.
                    if first_loop {
                        self.interrupt(current_task, node, unique_id)?;
                        first_loop = false;
                    }
                }
                Err(e) => {
                    log_error!("Unexpected error: {e:?}");
                    return Err(e);
                }
            }
        }
    }
}

impl FuseMutableState {
    fn wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn is_connected(&self) -> bool {
        matches!(self.state, FuseConnectionState::Connected)
    }

    fn set_configuration(&mut self, configuration: FuseConfiguration) {
        debug_assert!(self.configuration.is_none());
        log_trace!("Fuse configuration: {configuration:?}");
        self.configuration = Some(configuration);
        self.waiters.notify_value(CONFIGURATION_AVAILABLE_EVENT);
    }

    fn connect(&mut self) {
        debug_assert!(matches!(self.state, FuseConnectionState::Waiting));
        self.state = FuseConnectionState::Connected;
    }

    /// Disconnect the mount. Happens on unmount. Every filesystem operation will fail with
    /// ECONNABORTED, and every read/write on the /dev/fuse fd will fail with ENODEV.
    fn disconnect(&mut self) {
        if matches!(self.state, FuseConnectionState::Disconnected) {
            return;
        }
        self.state = FuseConnectionState::Disconnected;
        self.message_queue.clear();
        for operation in &mut self.operations {
            operation.1.response = Some(error!(ECONNABORTED));
        }
        self.waiters.notify_all();
    }

    /// Queue the given operation on the internal queue for the userspace daemon to read. If
    /// `waiter` is not None, register `waiter` to be notified when userspace responds to the
    /// operation. This should only be used if the operation expects a response.
    fn queue_operation(
        &mut self,
        current_task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
        waiter: Option<&Waiter>,
    ) -> Result<u64, Errno> {
        debug_assert!(waiter.is_some() == operation.has_response(), "{operation:?}");
        if !self.is_connected() {
            return error!(ECONNABORTED);
        }
        self.last_unique_id += 1;
        let message = FuseKernelMessage::new(self.last_unique_id, current_task, node, operation)?;
        if let Some(waiter) = waiter {
            self.waiters.wait_async_value(waiter, self.last_unique_id);
        }
        if message.operation.has_response() {
            self.operations.insert(self.last_unique_id, message.operation.as_running().into());
        }
        self.message_queue.push_back(message);
        self.waiters.notify_fd_events(FdEvents::POLLIN);
        Ok(self.last_unique_id)
    }

    /// Interrupt the operation with the given unique_id.
    ///
    /// If the operation is still enqueued, this will immediately dequeue the operation and return
    /// with an EINTR error.
    ///
    /// If not, it will send an interrupt operation.
    fn interrupt(
        &mut self,
        current_task: &CurrentTask,
        node: &FuseNode,
        unique_id: u64,
    ) -> Result<(), Errno> {
        debug_assert!(self.operations.contains_key(&unique_id));

        let mut in_queue = false;
        self.message_queue.retain(|m| {
            if m.header.unique == unique_id {
                self.operations.remove(&unique_id);
                in_queue = true;
                false
            } else {
                true
            }
        });
        if in_queue {
            // Nothing to do, the operation has been cancelled before being sent.
            return error!(EINTR);
        }
        self.queue_operation(current_task, node, FuseOperation::Interrupt { unique_id }, None)
            .map(|_| ())
    }

    /// Returns the response for the operation with the given identifier. Returns None if the
    /// operation is still in flight.
    fn get_response(&mut self, unique_id: u64) -> Option<Result<FuseResponse, Errno>> {
        match self.operations.entry(unique_id) {
            Entry::Vacant(_) => Some(error!(EINVAL)),
            Entry::Occupied(mut entry) => {
                let result = entry.get_mut().response.take();
                if result.is_some() {
                    entry.remove();
                }
                result
            }
        }
    }

    fn query_events(&self) -> FdEvents {
        let mut events = FdEvents::POLLOUT;
        if !self.is_connected() || !self.message_queue.is_empty() {
            events |= FdEvents::POLLIN
        };
        if !self.is_connected() {
            events |= FdEvents::POLLERR;
        }
        events
    }

    fn read(&mut self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        match self.state {
            FuseConnectionState::Waiting => return error!(EPERM),
            FuseConnectionState::Disconnected => return error!(ENODEV),
            _ => {}
        }
        if let Some(message) = self.message_queue.pop_front() {
            message.serialize(data)
        } else {
            error!(EAGAIN)
        }
    }

    fn write(&mut self, data: &mut dyn InputBuffer) -> Result<usize, Errno> {
        match self.state {
            FuseConnectionState::Waiting => return error!(EPERM),
            FuseConnectionState::Disconnected => return error!(ENODEV),
            _ => {}
        }
        let header: uapi::fuse_out_header = data.read_to_object()?;
        let payload_size = std::cmp::min(
            (header.len as usize).saturating_sub(std::mem::size_of::<uapi::fuse_out_header>()),
            data.available(),
        );
        self.waiters.notify_value(header.unique);
        let mut running_operation = match self.operations.entry(header.unique) {
            Entry::Occupied(e) => e,
            Entry::Vacant(_) => return error!(EINVAL),
        };
        let operation = &running_operation.get().operation;
        let is_async = operation.is_async();
        if header.error < 0 {
            log_trace!("Fuse: {operation:?} -> {header:?}");
            let code = i16::try_from(-header.error).unwrap_or(EINVAL.error_code() as i16);
            let errno = errno_from_code!(code);
            let response = operation.handle_error(&mut self.operations_state, errno);
            if is_async {
                running_operation.remove();
            } else {
                running_operation.get_mut().response = Some(response);
            }
        } else {
            let buffer = data.read_to_vec_limited(payload_size)?;
            if buffer.len() != payload_size {
                return error!(EINVAL);
            }
            let response = operation.parse_response(buffer)?;
            log_trace!("Fuse: {operation:?} -> {response:?}");
            if is_async {
                let operation = running_operation.remove();
                self.handle_async(operation, response)?;
            } else {
                running_operation.get_mut().response = Some(Ok(response));
            }
        }
        Ok(data.bytes_read())
    }

    fn handle_async(
        &mut self,
        operation: RunningOperation,
        response: FuseResponse,
    ) -> Result<(), Errno> {
        match (operation.operation, response) {
            (RunningOperationKind::Init { fs }, FuseResponse::Init(init_out)) => {
                let configuration = FuseConfiguration::try_from(init_out)?;
                if configuration.flags.contains(FuseInitFlags::POSIX_ACL) {
                    // Per libfuse's documentation on `FUSE_CAP_POSIX_ACL`,
                    // the POSIX_ACL flag implicitly enables the
                    // `default_permissions` mount option.
                    if let Some(fs) = fs.upgrade() {
                        FuseFs::from_fs(&fs)
                            .expect("should only perform FUSE ops with FUSE fs")
                            .default_permissions
                            .store(true, DEFAULT_PERMISSIONS_ATOMIC_ORDERING)
                    } else {
                        log_warn!("failed to upgrade FuseFs when handling FUSE_INIT response");
                        return error!(ENOTCONN);
                    }
                }
                self.set_configuration(configuration);
                Ok(())
            }
            operation => {
                // Init is the only async operation.
                panic!("Incompatible operation={operation:?}");
            }
        }
    }
}

/// An operation that is either queued to be send to userspace, or already sent to userspace and
/// waiting for a response.
#[derive(Debug)]
struct RunningOperation {
    operation: RunningOperationKind,
    response: Option<Result<FuseResponse, Errno>>,
}

impl From<RunningOperationKind> for RunningOperation {
    fn from(operation: RunningOperationKind) -> Self {
        Self { operation, response: None }
    }
}

#[derive(Debug)]
struct FuseKernelMessage {
    header: uapi::fuse_in_header,
    operation: FuseOperation,
}

impl FuseKernelMessage {
    fn new(
        unique: u64,
        current_task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
    ) -> Result<Self, Errno> {
        let creds = current_task.creds();
        Ok(Self {
            header: uapi::fuse_in_header {
                len: u32::try_from(std::mem::size_of::<uapi::fuse_in_header>() + operation.len())
                    .map_err(|_| errno!(EINVAL))?,
                opcode: operation.opcode(),
                unique,
                nodeid: node.nodeid,
                uid: creds.uid,
                gid: creds.gid,
                pid: current_task.get_tid() as u32,
                total_extlen: 0,
                padding: 0,
            },
            operation,
        })
    }

    fn serialize(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        let size = data.write(self.header.as_bytes())?;
        Ok(size + self.operation.serialize(data)?)
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct FuseInitFlags : u32 {
        const BIG_WRITES = uapi::FUSE_BIG_WRITES;
        const DONT_MASK = uapi::FUSE_DONT_MASK;
        const SPLICE_WRITE = uapi::FUSE_SPLICE_WRITE;
        const SPLICE_MOVE = uapi::FUSE_SPLICE_MOVE;
        const SPLICE_READ = uapi::FUSE_SPLICE_READ;
        const DO_READDIRPLUS = uapi::FUSE_DO_READDIRPLUS;
        const READDIRPLUS_AUTO = uapi::FUSE_READDIRPLUS_AUTO;
        const SETXATTR_EXT = uapi::FUSE_SETXATTR_EXT;
        const POSIX_ACL = uapi::FUSE_POSIX_ACL;
    }
}

#[derive(Clone, Debug)]
enum RunningOperationKind {
    Access,
    Flush,
    Forget,
    GetAttr,
    Init {
        /// The FUSE fs that triggered this operation.
        ///
        /// `Weak` because the `FuseFs` holds an `Arc<FuseConnection>` which
        /// may hold this operation.
        fs: Weak<FileSystem>,
    },
    Interrupt,
    GetXAttr {
        size: u32,
    },
    ListXAttr {
        size: u32,
    },
    Lookup,
    Mkdir,
    Mknod,
    Link,
    Open {
        dir: bool,
    },
    Poll,
    Read,
    Readdir {
        use_readdirplus: bool,
    },
    Readlink,
    Release {
        dir: bool,
    },
    RemoveXAttr,
    Seek,
    SetAttr,
    SetXAttr,
    Statfs,
    Symlink,
    Unlink,
    Write,
}

impl RunningOperationKind {
    fn is_async(&self) -> bool {
        matches!(self, Self::Init { .. })
    }

    fn opcode(&self) -> u32 {
        match self {
            Self::Access => uapi::fuse_opcode_FUSE_ACCESS,
            Self::Flush => uapi::fuse_opcode_FUSE_FLUSH,
            Self::Forget => uapi::fuse_opcode_FUSE_FORGET,
            Self::GetAttr => uapi::fuse_opcode_FUSE_GETATTR,
            Self::GetXAttr { .. } => uapi::fuse_opcode_FUSE_GETXATTR,
            Self::Init { .. } => uapi::fuse_opcode_FUSE_INIT,
            Self::Interrupt => uapi::fuse_opcode_FUSE_INTERRUPT,
            Self::ListXAttr { .. } => uapi::fuse_opcode_FUSE_LISTXATTR,
            Self::Lookup => uapi::fuse_opcode_FUSE_LOOKUP,
            Self::Mkdir => uapi::fuse_opcode_FUSE_MKDIR,
            Self::Mknod => uapi::fuse_opcode_FUSE_MKNOD,
            Self::Link => uapi::fuse_opcode_FUSE_LINK,
            Self::Open { dir } => {
                if *dir {
                    uapi::fuse_opcode_FUSE_OPENDIR
                } else {
                    uapi::fuse_opcode_FUSE_OPEN
                }
            }
            Self::Poll => uapi::fuse_opcode_FUSE_POLL,
            Self::Read => uapi::fuse_opcode_FUSE_READ,
            Self::Readdir { use_readdirplus } => {
                if *use_readdirplus {
                    uapi::fuse_opcode_FUSE_READDIRPLUS
                } else {
                    uapi::fuse_opcode_FUSE_READDIR
                }
            }
            Self::Readlink => uapi::fuse_opcode_FUSE_READLINK,
            Self::Release { dir } => {
                if *dir {
                    uapi::fuse_opcode_FUSE_RELEASEDIR
                } else {
                    uapi::fuse_opcode_FUSE_RELEASE
                }
            }
            Self::RemoveXAttr => uapi::fuse_opcode_FUSE_REMOVEXATTR,
            Self::Seek => uapi::fuse_opcode_FUSE_LSEEK,
            Self::SetAttr => uapi::fuse_opcode_FUSE_SETATTR,
            Self::SetXAttr => uapi::fuse_opcode_FUSE_SETXATTR,
            Self::Statfs => uapi::fuse_opcode_FUSE_STATFS,
            Self::Symlink => uapi::fuse_opcode_FUSE_SYMLINK,
            Self::Unlink => uapi::fuse_opcode_FUSE_UNLINK,
            Self::Write => uapi::fuse_opcode_FUSE_WRITE,
        }
    }

    fn to_response<T: FromBytes + AsBytes + NoCell>(buffer: &[u8]) -> T {
        let mut result = T::new_zeroed();
        let length_to_copy = std::cmp::min(buffer.len(), std::mem::size_of::<T>());
        result.as_bytes_mut()[..length_to_copy].copy_from_slice(&buffer[..length_to_copy]);
        result
    }

    fn parse_response(&self, buffer: Vec<u8>) -> Result<FuseResponse, Errno> {
        match self {
            Self::Access => Ok(FuseResponse::Access(Ok(()))),
            Self::GetAttr | Self::SetAttr => {
                Ok(FuseResponse::Attr(Self::to_response::<uapi::fuse_attr_out>(&buffer)))
            }
            Self::GetXAttr { size } | Self::ListXAttr { size } => {
                if *size == 0 {
                    if buffer.len() < std::mem::size_of::<uapi::fuse_getxattr_out>() {
                        return error!(EINVAL);
                    }
                    let getxattr_out = Self::to_response::<uapi::fuse_getxattr_out>(&buffer);
                    Ok(FuseResponse::GetXAttr(ValueOrSize::Size(getxattr_out.size as usize)))
                } else {
                    Ok(FuseResponse::GetXAttr(FsString::new(buffer).into()))
                }
            }
            Self::Init { .. } => {
                Ok(FuseResponse::Init(Self::to_response::<uapi::fuse_init_out>(&buffer)))
            }
            Self::Lookup | Self::Mkdir | Self::Mknod | Self::Link | Self::Symlink => {
                Ok(FuseResponse::Entry(Self::to_response::<uapi::fuse_entry_out>(&buffer)))
            }
            Self::Open { .. } => {
                Ok(FuseResponse::Open(Self::to_response::<uapi::fuse_open_out>(&buffer)))
            }
            Self::Poll => Ok(FuseResponse::Poll(Self::to_response::<uapi::fuse_poll_out>(&buffer))),
            Self::Read | Self::Readlink => Ok(FuseResponse::Read(buffer)),
            Self::Readdir { use_readdirplus, .. } => {
                let mut result = vec![];
                let mut slice = &buffer[..];
                while !slice.is_empty() {
                    // If using READDIRPLUS, the data starts with the entry.
                    let entry = if *use_readdirplus {
                        if slice.len() < std::mem::size_of::<uapi::fuse_entry_out>() {
                            return error!(EINVAL);
                        }
                        let entry = Self::to_response::<uapi::fuse_entry_out>(slice);
                        slice = &slice[std::mem::size_of::<uapi::fuse_entry_out>()..];
                        Some(entry)
                    } else {
                        None
                    };
                    // The next item is the dirent.
                    if slice.len() < std::mem::size_of::<uapi::fuse_dirent>() {
                        return error!(EINVAL);
                    }
                    let dirent = Self::to_response::<uapi::fuse_dirent>(slice);
                    // And it ends with the name.
                    slice = &slice[std::mem::size_of::<uapi::fuse_dirent>()..];
                    let namelen = dirent.namelen as usize;
                    if slice.len() < namelen {
                        return error!(EINVAL);
                    }
                    let name = FsString::from(&slice[..namelen]);
                    result.push((dirent, name, entry));
                    let skipped = round_up_to_increment(namelen, 8)?;
                    if slice.len() < skipped {
                        return error!(EINVAL);
                    }
                    slice = &slice[skipped..];
                }
                Ok(FuseResponse::Readdir(result))
            }
            Self::Flush
            | Self::Release { .. }
            | Self::RemoveXAttr
            | Self::SetXAttr
            | Self::Unlink => Ok(FuseResponse::None),
            Self::Statfs => {
                Ok(FuseResponse::Statfs(Self::to_response::<uapi::fuse_statfs_out>(&buffer)))
            }
            Self::Seek => {
                Ok(FuseResponse::Seek(Self::to_response::<uapi::fuse_lseek_out>(&buffer)))
            }
            Self::Write => {
                Ok(FuseResponse::Write(Self::to_response::<uapi::fuse_write_out>(&buffer)))
            }
            Self::Interrupt | Self::Forget => {
                panic!("Response for operation without one");
            }
        }
    }

    /// Handles an error from the userspace daemon.
    ///
    /// Given the `errno` returned by the userspace daemon, returns the response the caller should
    /// see. This can also update the `OperationState` to allow shortcircuit on future requests.
    fn handle_error(
        &self,
        state: &mut OperationsState,
        errno: Errno,
    ) -> Result<FuseResponse, Errno> {
        match self {
            Self::Access if errno == ENOSYS => {
                // Per libfuse, ENOSYS is interpreted as a "permanent success"
                // so we don't need to do anything further, including performing
                // the default/standard file permission checks like we do
                // when the `default_permissions` mount option is set.
                const UNIMPLEMENTED_ACCESS_RESPONSE: Result<FuseResponse, Errno> =
                    Ok(FuseResponse::Access(Ok(())));
                state.insert(self.opcode(), UNIMPLEMENTED_ACCESS_RESPONSE);
                UNIMPLEMENTED_ACCESS_RESPONSE
            }
            Self::Flush if errno == ENOSYS => {
                state.insert(self.opcode(), Ok(FuseResponse::None));
                Ok(FuseResponse::None)
            }
            Self::Seek if errno == ENOSYS => {
                state.insert(self.opcode(), Err(errno.clone()));
                Err(errno)
            }
            Self::Poll if errno == ENOSYS => {
                let response = FuseResponse::Poll(uapi::fuse_poll_out {
                    revents: (FdEvents::POLLIN | FdEvents::POLLOUT).bits(),
                    padding: 0,
                });
                state.insert(self.opcode(), Ok(response.clone()));
                Ok(response)
            }
            _ => Err(errno),
        }
    }
}

#[derive(Debug)]
enum FuseOperation {
    Access {
        mask: u32,
    },
    Flush(uapi::fuse_open_out),
    Forget(uapi::fuse_forget_in),
    GetAttr,
    Init {
        /// The FUSE fs that triggered this operation.
        ///
        /// `Weak` because the `FuseFs` holds an `Arc<FuseConnection>` which
        /// may hold this operation.
        fs: Weak<FileSystem>,
    },
    Interrupt {
        /// Identifier of the operation to interrupt
        unique_id: u64,
    },
    GetXAttr {
        getxattr_in: uapi::fuse_getxattr_in,
        /// Name of the attribute
        name: FsString,
    },
    ListXAttr(uapi::fuse_getxattr_in),
    Lookup {
        /// Name of the entry to lookup
        name: FsString,
    },
    Mkdir {
        mkdir_in: uapi::fuse_mkdir_in,
        /// Name of the entry to create
        name: FsString,
    },
    Mknod {
        mknod_in: uapi::fuse_mknod_in,
        /// Name of the node to create
        name: FsString,
    },
    Link {
        link_in: uapi::fuse_link_in,
        /// Name of the link to create
        name: FsString,
    },
    Open {
        flags: OpenFlags,
        mode: FileMode,
    },
    Poll(uapi::fuse_poll_in),
    Read(uapi::fuse_read_in),
    Readdir {
        read_in: uapi::fuse_read_in,
        /// Whether to use the READDIRPLUS api
        use_readdirplus: bool,
    },
    Readlink,
    Release {
        flags: OpenFlags,
        mode: FileMode,
        open_out: uapi::fuse_open_out,
    },
    RemoveXAttr {
        /// Name of the attribute
        name: FsString,
    },
    Seek(uapi::fuse_lseek_in),
    SetAttr(uapi::fuse_setattr_in),
    SetXAttr {
        setxattr_in: uapi::fuse_setxattr_in,
        /// Indicates if userspace supports the, most-recent/extended variant of
        /// `fuse_setxattr_in`.
        is_ext: bool,
        /// Name of the attribute
        name: FsString,
        /// Value of the attribute
        value: FsString,
    },
    Statfs,
    Symlink {
        /// Target of the link
        target: FsString,
        /// Name of the link
        name: FsString,
    },
    Unlink {
        /// Name of the file to unlink
        name: FsString,
    },
    Write {
        write_in: uapi::fuse_write_in,
        // Content to write
        content: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
enum FuseResponse {
    Access(Result<(), Errno>),
    Attr(uapi::fuse_attr_out),
    Entry(uapi::fuse_entry_out),
    GetXAttr(ValueOrSize<FsString>),
    Init(uapi::fuse_init_out),
    Open(uapi::fuse_open_out),
    Poll(uapi::fuse_poll_out),
    Read(
        // Content read
        Vec<u8>,
    ),
    Seek(uapi::fuse_lseek_out),
    Readdir(Vec<(uapi::fuse_dirent, FsString, Option<uapi::fuse_entry_out>)>),
    Statfs(uapi::fuse_statfs_out),
    Write(uapi::fuse_write_out),
    None,
}

impl FuseOperation {
    fn serialize(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        match self {
            Self::Access { mask } => {
                let message = uapi::fuse_access_in { mask: *mask, padding: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Flush(open_in) => {
                let message =
                    uapi::fuse_flush_in { fh: open_in.fh, unused: 0, padding: 0, lock_owner: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Forget(forget_in) => data.write_all(forget_in.as_bytes()),
            Self::GetAttr | Self::Readlink | Self::Statfs => Ok(0),
            Self::GetXAttr { getxattr_in, name } => {
                let mut len = data.write_all(getxattr_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Init { .. } => {
                let message = uapi::fuse_init_in {
                    major: uapi::FUSE_KERNEL_VERSION,
                    minor: uapi::FUSE_KERNEL_MINOR_VERSION,
                    flags: FuseInitFlags::all().bits(),
                    ..Default::default()
                };
                data.write_all(message.as_bytes())
            }
            Self::Interrupt { unique_id } => {
                let message = uapi::fuse_interrupt_in { unique: *unique_id };
                data.write_all(message.as_bytes())
            }
            Self::ListXAttr(getxattr_in) => data.write_all(getxattr_in.as_bytes()),
            Self::Lookup { name } => Self::write_null_terminated(data, name),
            Self::Open { flags, .. } => {
                let message = uapi::fuse_open_in { flags: flags.bits(), open_flags: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Poll(poll_in) => data.write_all(poll_in.as_bytes()),
            Self::Mkdir { mkdir_in, name } => {
                let mut len = data.write_all(mkdir_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Mknod { mknod_in, name } => {
                let mut len = data.write_all(mknod_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Link { link_in, name } => {
                let mut len = data.write_all(link_in.as_bytes())?;
                len += Self::write_null_terminated(data, name)?;
                Ok(len)
            }
            Self::Read(read_in) | Self::Readdir { read_in, .. } => {
                data.write_all(read_in.as_bytes())
            }
            Self::Release { open_out, .. } => {
                let message = uapi::fuse_release_in {
                    fh: open_out.fh,
                    flags: 0,
                    release_flags: 0,
                    lock_owner: 0,
                };
                data.write_all(message.as_bytes())
            }
            Self::RemoveXAttr { name } => Self::write_null_terminated(data, name),
            Self::Seek(seek_in) => data.write_all(seek_in.as_bytes()),
            Self::SetAttr(setattr_in) => data.write_all(setattr_in.as_bytes()),
            Self::SetXAttr { setxattr_in, is_ext, name, value } => {
                let header =
                    if *is_ext { setxattr_in.as_bytes() } else { &setxattr_in.as_bytes()[..8] };
                let mut len = data.write_all(header)?;
                len += Self::write_null_terminated(data, name)?;
                len += data.write_all(value.as_bytes())?;
                Ok(len)
            }
            Self::Symlink { target, name } => {
                let mut len = Self::write_null_terminated(data, name)?;
                len += Self::write_null_terminated(data, target)?;
                Ok(len)
            }
            Self::Unlink { name } => Self::write_null_terminated(data, name),
            Self::Write { write_in, content } => {
                let mut len = data.write_all(write_in.as_bytes())?;
                len += data.write_all(content)?;
                Ok(len)
            }
        }
    }

    fn write_null_terminated(
        data: &mut dyn OutputBuffer,
        content: &Vec<u8>,
    ) -> Result<usize, Errno> {
        let mut len = data.write_all(content.as_bytes())?;
        len += data.write_all(&[0])?;
        Ok(len)
    }

    fn opcode(&self) -> u32 {
        match self {
            Self::Access { .. } => uapi::fuse_opcode_FUSE_ACCESS,
            Self::Flush(_) => uapi::fuse_opcode_FUSE_FLUSH,
            Self::Forget(_) => uapi::fuse_opcode_FUSE_FORGET,
            Self::GetAttr => uapi::fuse_opcode_FUSE_GETATTR,
            Self::GetXAttr { .. } => uapi::fuse_opcode_FUSE_GETXATTR,
            Self::Init { .. } => uapi::fuse_opcode_FUSE_INIT,
            Self::Interrupt { .. } => uapi::fuse_opcode_FUSE_INTERRUPT,
            Self::ListXAttr(_) => uapi::fuse_opcode_FUSE_LISTXATTR,
            Self::Lookup { .. } => uapi::fuse_opcode_FUSE_LOOKUP,
            Self::Mkdir { .. } => uapi::fuse_opcode_FUSE_MKDIR,
            Self::Mknod { .. } => uapi::fuse_opcode_FUSE_MKNOD,
            Self::Link { .. } => uapi::fuse_opcode_FUSE_LINK,
            Self::Open { flags, mode } => {
                if mode.is_dir() || flags.contains(OpenFlags::DIRECTORY) {
                    uapi::fuse_opcode_FUSE_OPENDIR
                } else {
                    uapi::fuse_opcode_FUSE_OPEN
                }
            }
            Self::Poll(_) => uapi::fuse_opcode_FUSE_POLL,
            Self::Read(_) => uapi::fuse_opcode_FUSE_READ,
            Self::Readdir { use_readdirplus, .. } => {
                if *use_readdirplus {
                    uapi::fuse_opcode_FUSE_READDIRPLUS
                } else {
                    uapi::fuse_opcode_FUSE_READDIR
                }
            }
            Self::Readlink => uapi::fuse_opcode_FUSE_READLINK,
            Self::Release { flags, mode, .. } => {
                if mode.is_dir() || flags.contains(OpenFlags::DIRECTORY) {
                    uapi::fuse_opcode_FUSE_RELEASEDIR
                } else {
                    uapi::fuse_opcode_FUSE_RELEASE
                }
            }
            Self::RemoveXAttr { .. } => uapi::fuse_opcode_FUSE_REMOVEXATTR,
            Self::Seek(_) => uapi::fuse_opcode_FUSE_LSEEK,
            Self::SetAttr(_) => uapi::fuse_opcode_FUSE_SETATTR,
            Self::SetXAttr { .. } => uapi::fuse_opcode_FUSE_SETXATTR,
            Self::Statfs => uapi::fuse_opcode_FUSE_STATFS,
            Self::Symlink { .. } => uapi::fuse_opcode_FUSE_SYMLINK,
            Self::Unlink { .. } => uapi::fuse_opcode_FUSE_UNLINK,
            Self::Write { .. } => uapi::fuse_opcode_FUSE_WRITE,
        }
    }

    fn as_running(&self) -> RunningOperationKind {
        match self {
            Self::Access { .. } => RunningOperationKind::Access,
            Self::Flush(_) => RunningOperationKind::Flush,
            Self::Forget(_) => RunningOperationKind::Forget,
            Self::GetAttr => RunningOperationKind::GetAttr,
            Self::GetXAttr { getxattr_in, .. } => {
                RunningOperationKind::GetXAttr { size: getxattr_in.size }
            }
            Self::Init { fs } => RunningOperationKind::Init { fs: fs.clone() },
            Self::Interrupt { .. } => RunningOperationKind::Interrupt,
            Self::ListXAttr(getxattr_in) => {
                RunningOperationKind::ListXAttr { size: getxattr_in.size }
            }
            Self::Lookup { .. } => RunningOperationKind::Lookup,
            Self::Mkdir { .. } => RunningOperationKind::Mkdir,
            Self::Mknod { .. } => RunningOperationKind::Mknod,
            Self::Link { .. } => RunningOperationKind::Link,
            Self::Open { flags, mode } => RunningOperationKind::Open {
                dir: mode.is_dir() || flags.contains(OpenFlags::DIRECTORY),
            },
            Self::Poll(_) => RunningOperationKind::Poll,
            Self::Read(_) => RunningOperationKind::Read,
            Self::Readdir { use_readdirplus, .. } => {
                RunningOperationKind::Readdir { use_readdirplus: *use_readdirplus }
            }
            Self::Readlink => RunningOperationKind::Readlink,
            Self::Release { flags, mode, .. } => RunningOperationKind::Release {
                dir: mode.is_dir() || flags.contains(OpenFlags::DIRECTORY),
            },
            Self::RemoveXAttr { .. } => RunningOperationKind::RemoveXAttr,
            Self::Seek(_) => RunningOperationKind::Seek,
            Self::SetAttr(_) => RunningOperationKind::SetAttr,
            Self::SetXAttr { .. } => RunningOperationKind::SetXAttr,
            Self::Statfs => RunningOperationKind::Statfs,
            Self::Symlink { .. } => RunningOperationKind::Symlink,
            Self::Unlink { .. } => RunningOperationKind::Unlink,
            Self::Write { .. } => RunningOperationKind::Write,
        }
    }

    fn len(&self) -> usize {
        #[derive(Debug, Default)]
        struct CountingOutputBuffer {
            written: usize,
        }

        impl Buffer for CountingOutputBuffer {
            fn segments_count(&self) -> Result<usize, Errno> {
                panic!("Should not be called");
            }

            fn peek_each_segment(
                &mut self,
                _callback: &mut PeekBufferSegmentsCallback<'_>,
            ) -> Result<(), Errno> {
                panic!("Should not be called");
            }
        }

        impl OutputBuffer for CountingOutputBuffer {
            fn available(&self) -> usize {
                usize::MAX
            }

            fn bytes_written(&self) -> usize {
                self.written
            }

            fn zero(&mut self) -> Result<usize, Errno> {
                panic!("Should not be called");
            }

            fn write_each(
                &mut self,
                _callback: &mut OutputBufferCallback<'_>,
            ) -> Result<usize, Errno> {
                panic!("Should not be called.");
            }

            fn write_all(&mut self, buffer: &[u8]) -> Result<usize, Errno> {
                self.written += buffer.len();
                Ok(buffer.len())
            }

            unsafe fn advance(&mut self, _length: usize) -> Result<(), Errno> {
                panic!("Should not be called.");
            }
        }

        let mut counting_output_buffer = CountingOutputBuffer::default();
        self.serialize(&mut counting_output_buffer).expect("Serialization should not fail");
        counting_output_buffer.written
    }

    fn has_response(&self) -> bool {
        !matches!(self, Self::Interrupt { .. } | Self::Forget(_))
    }

    fn is_async(&self) -> bool {
        matches!(self, Self::Init { .. })
    }
}
