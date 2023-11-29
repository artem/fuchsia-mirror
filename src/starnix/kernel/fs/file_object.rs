// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        file_server::serve_file,
        fsverity::{
            FsVerityState, {self},
        },
        DirentSink, FallocMode, FdEvents, FdTableId, FileReleaser, FileSystemHandle,
        FileWriteGuard, FileWriteGuardMode, FileWriteGuardRef, FsNodeHandle, InotifyMask,
        NamespaceNode, RecordLockCommand, RecordLockOwner,
    },
    logging::{impossible_error, not_implemented},
    mm::{
        vmo::round_up_to_system_page_size, DesiredAddress, MappingName, MappingOptions,
        MemoryAccessorExt, ProtectionFlags,
    },
    task::{CurrentTask, EventHandler, Task, WaitCallback, WaitCanceler, Waiter},
};
use fidl::HandleBased;
use fuchsia_zircon as zx;
use starnix_lock::Mutex;
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    as_any::AsAny,
    errno, error,
    errors::{Errno, EAGAIN, ETIMEDOUT},
    fsxattr, off_t,
    open_flags::OpenFlags,
    ownership::Releasable,
    pid_t,
    resource_limits::Resource,
    seal_flags::SealFlags,
    uapi,
    user_address::UserAddress,
    FIONBIO, FIONREAD, FS_IOC_ENABLE_VERITY, FS_IOC_FSGETXATTR, FS_IOC_FSSETXATTR, FS_IOC_GETFLAGS,
    FS_IOC_MEASURE_VERITY, FS_IOC_READ_VERITY_METADATA, FS_IOC_SETFLAGS, FS_VERITY_FL, SEEK_CUR,
    SEEK_DATA, SEEK_END, SEEK_HOLE, SEEK_SET, TCGETS,
};
use std::{
    fmt,
    sync::{Arc, Weak},
};

pub const MAX_LFS_FILESIZE: usize = 0x7fff_ffff_ffff_ffff;

pub enum SeekTarget {
    /// Seek to the given offset relative to the start of the file.
    Set(off_t),
    /// Seek to the given offset relative to the current position.
    Cur(off_t),
    /// Seek to the given offset relative to the end of the file.
    End(off_t),
    /// Seek for the first data after the given offset,
    Data(off_t),
    /// Seek for the first hole after the given offset,
    Hole(off_t),
}

impl SeekTarget {
    pub fn from_raw(whence: u32, offset: off_t) -> Result<SeekTarget, Errno> {
        match whence {
            SEEK_SET => Ok(SeekTarget::Set(offset)),
            SEEK_CUR => Ok(SeekTarget::Cur(offset)),
            SEEK_END => Ok(SeekTarget::End(offset)),
            SEEK_DATA => Ok(SeekTarget::Data(offset)),
            SEEK_HOLE => Ok(SeekTarget::Hole(offset)),
            _ => error!(EINVAL),
        }
    }

    pub fn whence(&self) -> u32 {
        match self {
            Self::Set(_) => SEEK_SET,
            Self::Cur(_) => SEEK_CUR,
            Self::End(_) => SEEK_END,
            Self::Data(_) => SEEK_DATA,
            Self::Hole(_) => SEEK_HOLE,
        }
    }

    pub fn offset(&self) -> off_t {
        match self {
            Self::Set(off)
            | Self::Cur(off)
            | Self::End(off)
            | Self::Data(off)
            | Self::Hole(off) => *off,
        }
    }
}

/// This function adds `POLLRDNORM` and `POLLWRNORM` to the FdEvents
/// return from the FileOps because these FdEvents are equivalent to
/// `POLLIN` and `POLLOUT`, respectively, in the Linux UAPI.
///
/// See https://linux.die.net/man/2/poll
fn add_equivalent_fd_events(mut events: FdEvents) -> FdEvents {
    if events.contains(FdEvents::POLLIN) {
        events |= FdEvents::POLLRDNORM;
    }
    if events.contains(FdEvents::POLLOUT) {
        events |= FdEvents::POLLWRNORM;
    }
    events
}

/// Corresponds to struct file_operations in Linux, plus any filesystem-specific data.
pub trait FileOps: Send + Sync + AsAny + 'static {
    /// Called when the FileObject is closed.
    fn close(&self, _file: &FileObject, _current_task: &CurrentTask) {}

    /// Called every time close() is called on this file, even if the file is not ready to be
    /// released.
    fn flush(&self, _file: &FileObject, _current_task: &CurrentTask) {}

    /// Returns whether the file has meaningful seek offsets. Returning `false` is only
    /// optimization and will makes `FileObject` never hold the offset lock when calling `read` and
    /// `write`.
    fn has_persistent_offsets(&self) -> bool {
        self.is_seekable()
    }

    /// Returns whether the file is seekable.
    fn is_seekable(&self) -> bool;

    /// Read from the file at an offset. If the file does not have persistent offsets (either
    /// directly, or because it is not seekable), offset will be 0 and can be ignored.
    /// Returns the number of bytes read.
    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>;
    /// Write to the file with an offset. If the file does not have persistent offsets (either
    /// directly, or because it is not seekable), offset will be 0 and can be ignored.
    /// Returns the number of bytes written.
    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>;

    /// Adjust the `current_offset` if the file is seekable.
    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno>;

    /// Syncs cached state associated with the file descriptor to persistent storage.
    ///
    /// The method blocks until the synchronization is complete.
    fn sync(&self, _file: &FileObject, _current_task: &CurrentTask) -> Result<(), Errno> {
        Ok(())
    }

    /// Syncs cached data, and only enough metadata to retrieve said data, to persistent storage.
    ///
    /// The method blocks until the synchronization is complete.
    fn data_sync(&self, file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        self.sync(file, current_task)
    }

    /// Returns a VMO representing this file. At least the requested protection flags must
    /// be set on the VMO. Reading or writing the VMO must read or write the file. If this is not
    /// possible given the requested protection, an error must be returned.
    /// The `length` is a hint for the desired size of the VMO. The returned VMO may be larger or
    /// smaller than the requested length.
    /// This method is typically called by [`Self::mmap`].
    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(ENODEV)
    }

    /// Responds to an mmap call. The default implementation calls [`Self::get_vmo`] to get a VMO
    /// and then maps it with [`crate::mm::MemoryManager::map`].
    /// Only implement this trait method if your file needs to control mapping, or record where
    /// a VMO gets mapped.
    fn mmap(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        profile_duration!("FileOpsDefaultMmap");
        trace_duration!(trace_category_starnix_mm!(), "FileOpsDefaultMmap");
        let min_vmo_size = (vmo_offset as usize)
            .checked_add(round_up_to_system_page_size(length)?)
            .ok_or(errno!(EINVAL))?;
        let mut vmo = if options.contains(MappingOptions::SHARED) {
            profile_duration!("GetSharedVmo");
            trace_duration!(trace_category_starnix_mm!(), "GetSharedVmo");
            self.get_vmo(file, current_task, Some(min_vmo_size), prot_flags)?
        } else {
            profile_duration!("GetPrivateVmo");
            trace_duration!(trace_category_starnix_mm!(), "GetPrivateVmo");
            // TODO(tbodt): Use PRIVATE_CLONE to have the filesystem server do the clone for us.
            let base_prot_flags = (prot_flags | ProtectionFlags::READ) - ProtectionFlags::WRITE;
            let vmo = self.get_vmo(file, current_task, Some(min_vmo_size), base_prot_flags)?;
            let mut clone_flags = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
            if !prot_flags.contains(ProtectionFlags::WRITE) {
                clone_flags |= zx::VmoChildOptions::NO_WRITE;
            }
            trace_duration!(trace_category_starnix_mm!(), "CreatePrivateChildVmo");
            Arc::new(
                vmo.create_child(clone_flags, 0, vmo.get_size().map_err(impossible_error)?)
                    .map_err(impossible_error)?,
            )
        };

        // Write guard is necessary only for shared mappings. Note that this doesn't depend on
        // `prot_flags` since these can be changed later with `mprotect()`.
        let file_write_guard = if options.contains(MappingOptions::SHARED) && file.can_write() {
            profile_duration!("AcquireFileWriteGuard");
            let node = &file.name.entry.node;
            let mut state = node.write_guard_state.lock();

            // `F_SEAL_FUTURE_WRITE` should allow `mmap(PROT_READ)`, but block
            // `mprotect(PROT_WRITE)`. This is different from `F_SEAL_WRITE`, which blocks
            // `mmap(PROT_READ)`. To handle this case correctly remove `WRITE` right from the
            // VMO handle to ensure `mprotect(PROT_WRITE)` fails.
            let seals = state.get_seals().unwrap_or(SealFlags::empty());
            if seals.contains(SealFlags::FUTURE_WRITE)
                && !seals.contains(SealFlags::WRITE)
                && !prot_flags.contains(ProtectionFlags::WRITE)
            {
                let mut new_rights = zx::Rights::VMO_DEFAULT - zx::Rights::WRITE;
                if prot_flags.contains(ProtectionFlags::EXEC) {
                    new_rights |= zx::Rights::EXECUTE;
                }
                vmo = Arc::new(vmo.duplicate_handle(new_rights).map_err(impossible_error)?);

                FileWriteGuardRef(None)
            } else {
                state.create_write_guard(node.clone(), FileWriteGuardMode::WriteMapping)?.into_ref()
            }
        } else {
            FileWriteGuardRef(None)
        };

        current_task.mm.map_vmo(
            addr,
            vmo,
            vmo_offset,
            length,
            prot_flags,
            options,
            MappingName::File(filename),
            file_write_guard,
        )
    }

    /// Respond to a `getdents` or `getdents64` calls.
    ///
    /// The `file.offset` lock will be held while entering this method. The implementation must look
    /// at `sink.offset()` to read the current offset into the file.
    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        error!(ENOTDIR)
    }

    /// Establish a one-shot, edge-triggered, asynchronous wait for the given FdEvents for the
    /// given file and task. Returns `None` if this file does not support blocking waits.
    ///
    /// Active events are not considered. This is similar to the semantics of the
    /// ZX_WAIT_ASYNC_EDGE flag on zx_wait_async. To avoid missing events, the caller must call
    /// query_events after calling this.
    ///
    /// If your file does not support blocking waits, leave this as the default implementation.
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

    /// The events currently active on this file.
    ///
    /// If this function returns `POLLIN` or `POLLOUT`, then FileObject will
    /// add `POLLRDNORM` and `POLLWRNORM`, respective, which are equivalent in
    /// the Linux UAPI.
    ///
    /// See https://linux.die.net/man/2/poll
    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::POLLIN | FdEvents::POLLOUT)
    }

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(file, current_task, request, arg)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        default_fcntl(cmd)
    }

    /// Return a handle that allows access to this file descritor through the zxio protocols.
    ///
    /// If None is returned, the file will act as if it was a fd to `/dev/null`.
    fn to_handle(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        serve_file(current_task, file).map(|c| Some(c.into_handle()))
    }

    /// Returns the associated pid_t.
    ///
    /// Used by pidfd and `/proc/<pid>`. Unlikely to be used by other files.
    fn as_pid(&self, _file: &FileObject) -> Result<pid_t, Errno> {
        error!(EBADF)
    }
}

pub fn default_eof_offset(file: &FileObject, current_task: &CurrentTask) -> Result<off_t, Errno> {
    Ok(file.node().stat(current_task)?.st_size as off_t)
}

/// Implement the seek method for a file. The computation from the end of the file must be provided
/// through a callback.
///
/// Errors if the calculated offset is invalid.
///
/// - `current_offset`: The current position
/// - `target`: The location to seek to.
/// - `compute_end`: Compute the new offset from the end. Return an error if the operation is not
///    supported.
pub fn default_seek<F>(
    current_offset: off_t,
    target: SeekTarget,
    compute_end: F,
) -> Result<off_t, Errno>
where
    F: FnOnce(off_t) -> Result<off_t, Errno>,
{
    let new_offset = match target {
        SeekTarget::Set(offset) => Some(offset),
        SeekTarget::Cur(offset) => current_offset.checked_add(offset),
        SeekTarget::End(offset) => Some(compute_end(offset)?),
        SeekTarget::Data(offset) => {
            let eof = compute_end(0).unwrap_or(off_t::MAX);
            if offset >= eof {
                return error!(ENXIO);
            }
            Some(offset)
        }
        SeekTarget::Hole(offset) => {
            let eof = compute_end(0)?;
            if offset >= eof {
                return error!(ENXIO);
            }
            Some(eof)
        }
    }
    .ok_or_else(|| errno!(EINVAL))?;

    if new_offset < 0 {
        return error!(EINVAL);
    }

    Ok(new_offset)
}

/// Implement the seek method for a file without an upper bound on the resulting offset.
///
/// This is useful for files without a defined size.
///
/// Errors if the calculated offset is invalid.
///
/// - `current_offset`: The current position
/// - `target`: The location to seek to.
pub fn unbounded_seek(current_offset: off_t, target: SeekTarget) -> Result<off_t, Errno> {
    default_seek(current_offset, target, |_| Ok(MAX_LFS_FILESIZE as off_t))
}

macro_rules! fileops_impl_delegate_read_and_seek {
    ($self:ident, $delegate:expr) => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            offset: usize,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            $delegate.read(file, current_task, offset, data)
        }

        fn seek(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            current_offset: starnix_uapi::off_t,
            target: crate::fs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            $delegate.seek(file, current_task, current_offset, target)
        }
    };
}

/// Implements [`FileOps::seek`] in a way that makes sense for seekable files.
macro_rules! fileops_impl_seekable {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn seek(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            current_offset: starnix_uapi::off_t,
            target: crate::fs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            crate::fs::default_seek(current_offset, target, |offset| {
                let eof_offset = crate::fs::default_eof_offset(file, current_task)?;
                offset.checked_add(eof_offset).ok_or_else(|| starnix_uapi::errno!(EINVAL))
            })
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for non-seekable files.
macro_rules! fileops_impl_nonseekable {
    () => {
        fn is_seekable(&self) -> bool {
            false
        }

        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _current_offset: starnix_uapi::off_t,
            _target: crate::fs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ESPIPE)
        }
    };
}

/// Implements [`FileOps::seek`] methods in a way that makes sense for files that ignore
/// seeking operations and always read/write at offset 0.
macro_rules! fileops_impl_seekless {
    () => {
        fn has_persistent_offsets(&self) -> bool {
            false
        }

        fn is_seekable(&self) -> bool {
            true
        }

        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _current_offset: starnix_uapi::off_t,
            _target: crate::fs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            Ok(0)
        }
    };
}

macro_rules! fileops_impl_dataless {
    () => {
        fn write(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, Errno> {
            starnix_uapi::error!(EINVAL)
        }

        fn read(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, Errno> {
            starnix_uapi::error!(EINVAL)
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for directories. You must implement
/// [`FileOps::seek`] and [`FileOps::readdir`].
macro_rules! fileops_impl_directory {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EISDIR)
        }

        fn write(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EISDIR)
        }
    };
}

// Public re-export of macros allows them to be used like regular rust items.

pub(crate) use fileops_impl_dataless;
pub(crate) use fileops_impl_delegate_read_and_seek;
pub(crate) use fileops_impl_directory;
pub(crate) use fileops_impl_nonseekable;
pub(crate) use fileops_impl_seekable;
pub(crate) use fileops_impl_seekless;

pub fn default_ioctl(
    file: &FileObject,
    current_task: &CurrentTask,
    request: u32,
    arg: SyscallArg,
) -> Result<SyscallResult, Errno> {
    match request {
        TCGETS => error!(ENOTTY),
        FIONBIO => {
            file.update_file_flags(OpenFlags::NONBLOCK, OpenFlags::NONBLOCK);
            Ok(SUCCESS)
        }
        FIONREAD => {
            not_implemented!("FIONREAD");
            if !file.name.entry.node.is_reg() {
                return error!(ENOTTY);
            }

            let size =
                file.name.entry.node.refresh_info(current_task).map_err(|_| errno!(EINVAL))?.size;
            let offset = usize::try_from(*file.offset.lock()).map_err(|_| errno!(EINVAL))?;
            let remaining =
                if size < offset { 0 } else { i32::try_from(size - offset).unwrap_or(i32::MAX) };
            current_task.write_object(arg.into(), &remaining)?;
            Ok(SUCCESS)
        }
        FS_IOC_FSGETXATTR => {
            not_implemented!("FS_IOC_FSGETXATTR");
            let arg = UserAddress::from(arg).into();
            current_task.write_object(arg, &fsxattr::default())?;
            Ok(SUCCESS)
        }
        FS_IOC_FSSETXATTR => {
            not_implemented!("FS_IOC_FSSETXATTR");
            let arg = UserAddress::from(arg).into();
            let _: fsxattr = current_task.read_object(arg)?;
            Ok(SUCCESS)
        }
        FS_IOC_GETFLAGS => {
            not_implemented!("FS_IOC_GETFLAGS");
            let arg = UserAddress::from(arg).into();
            let mut flags: u32 = 0;
            if matches!(*file.node().fsverity.lock(), FsVerityState::FsVerity { .. }) {
                flags |= FS_VERITY_FL;
            }
            current_task.write_object(arg, &flags)?;
            Ok(SUCCESS)
        }
        FS_IOC_SETFLAGS => {
            not_implemented!("FS_IOC_SETFLAGS");
            let arg = UserAddress::from(arg).into();
            let _: u32 = current_task.read_object(arg)?;
            Ok(SUCCESS)
        }
        FS_IOC_ENABLE_VERITY => {
            Ok(fsverity::ioctl::enable(current_task, UserAddress::from(arg).into(), file)?)
        }
        FS_IOC_MEASURE_VERITY => {
            Ok(fsverity::ioctl::measure(current_task, UserAddress::from(arg).into(), file)?)
        }
        FS_IOC_READ_VERITY_METADATA => {
            Ok(fsverity::ioctl::read_metadata(current_task, UserAddress::from(arg).into(), file)?)
        }
        _ => {
            not_implemented!("ioctl: request=0x{:x}", request);
            error!(ENOTTY)
        }
    }
}

pub fn default_fcntl(cmd: u32) -> Result<SyscallResult, Errno> {
    not_implemented!("fcntl: command=0x{:x}", cmd);
    error!(EINVAL)
}

pub struct OPathOps {}

impl OPathOps {
    pub fn new() -> OPathOps {
        OPathOps {}
    }
}

impl FileOps for OPathOps {
    fn has_persistent_offsets(&self) -> bool {
        false
    }
    fn is_seekable(&self) -> bool {
        true
    }
    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _target: SeekTarget,
    ) -> Result<off_t, Errno> {
        error!(EBADF)
    }
    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(EBADF)
    }
    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        error!(EBADF)
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _request: u32,
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        error!(EBADF)
    }
}

pub struct ProxyFileOps(pub FileHandle);

macro_rules! delegate {
    {
        $delegate_to:expr;
        $(
            fn $name:ident(&$self:ident, $file:ident: &FileObject $(, $arg_name:ident: $arg_type:ty)*$(,)?) $(-> $ret:ty)?;
        )*
    } => {
        $(
            fn $name(&$self, _file: &FileObject $(, $arg_name: $arg_type)*) $(-> $ret)? {
                $delegate_to.ops().$name(&$delegate_to $(, $arg_name)*)
            }
        )*
    }
}

impl FileOps for ProxyFileOps {
    delegate! {
        self.0;
        fn close(&self, file: &FileObject, current_task: &CurrentTask);
        fn flush(&self, file: &FileObject, current_task: &CurrentTask);
        fn read(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: usize,
            data: &mut dyn OutputBuffer,
        ) -> Result<usize, Errno>;
        fn write(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: usize,
            data: &mut dyn InputBuffer,
        ) -> Result<usize, Errno>;
        fn seek(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: off_t,
            target: SeekTarget,
        ) -> Result<off_t, Errno>;
        fn get_vmo(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _length: Option<usize>,
            _prot: ProtectionFlags,
        ) -> Result<Arc<zx::Vmo>, Errno>;
        fn mmap(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            addr: DesiredAddress,
            vmo_offset: u64,
            length: usize,
            prot_flags: ProtectionFlags,
            options: MappingOptions,
            filename: NamespaceNode,
        ) -> Result<UserAddress, Errno>;
        fn readdir(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _sink: &mut dyn DirentSink,
        ) -> Result<(), Errno>;
        fn wait_async(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _waiter: &Waiter,
            _events: FdEvents,
            _handler: EventHandler,
        ) -> Option<WaitCanceler>;
        fn ioctl(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            request: u32,
            _arg: SyscallArg,
        ) -> Result<SyscallResult, Errno>;
        fn fcntl(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            cmd: u32,
            arg: u64,
        ) -> Result<SyscallResult, Errno>;
    }
    // These don't take &FileObject making it too hard to handle them properly in the macro
    fn query_events(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        self.0.ops().query_events(file, current_task)
    }
    fn has_persistent_offsets(&self) -> bool {
        self.0.ops().has_persistent_offsets()
    }
    fn is_seekable(&self) -> bool {
        self.0.ops().is_seekable()
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub enum FileAsyncOwner {
    #[default]
    Unowned,
    Thread(pid_t),
    Process(pid_t),
    ProcessGroup(pid_t),
}

impl FileAsyncOwner {
    pub fn validate(self, current_task: &CurrentTask) -> Result<(), Errno> {
        match self {
            FileAsyncOwner::Unowned => (),
            FileAsyncOwner::Thread(id) | FileAsyncOwner::Process(id) => {
                Task::from_weak(&current_task.get_task(id))?;
            }
            FileAsyncOwner::ProcessGroup(pgid) => {
                current_task
                    .kernel()
                    .pids
                    .read()
                    .get_process_group(pgid)
                    .ok_or_else(|| errno!(ESRCH))?;
            }
        }
        Ok(())
    }
}

fn check_offset(current_task: &CurrentTask, offset: usize) -> Result<(), Errno> {
    if offset >= MAX_LFS_FILESIZE
        || offset >= current_task.thread_group.get_rlimit(Resource::FSIZE) as usize
    {
        error!(EINVAL)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileObjectId(u64);

/// A session with a file object.
///
/// Each time a client calls open(), we create a new FileObject from the
/// underlying FsNode that receives the open(). This object contains the state
/// that is specific to this sessions whereas the underlying FsNode contains
/// the state that is shared between all the sessions.
pub struct FileObject {
    /// Weak reference to the `FileHandle` of this `FileObject`. This allows to retrieve the
    /// `FileHandle` from a `FileObject`.
    pub weak_handle: WeakFileHandle,

    /// A unique identifier for this file object.
    pub id: FileObjectId,

    ops: Box<dyn FileOps>,

    /// The NamespaceNode associated with this FileObject.
    ///
    /// Represents the name the process used to open this file.
    pub name: NamespaceNode,

    pub fs: FileSystemHandle,

    pub offset: Mutex<off_t>,

    flags: Mutex<OpenFlags>,

    async_owner: Mutex<FileAsyncOwner>,

    _file_write_guard: Option<FileWriteGuard>,
}

pub type FileHandle = Arc<FileReleaser>;
pub type WeakFileHandle = Weak<FileReleaser>;

impl FileObject {
    /// Create a FileObject that is not mounted in a namespace.
    ///
    /// In particular, this will create a new unrooted entries. This should not be used on
    /// file system with persistent entries, as the created entry will be out of sync with the one
    /// from the file system.
    ///
    /// The returned FileObject does not have a name.
    pub fn new_anonymous(
        ops: Box<dyn FileOps>,
        node: FsNodeHandle,
        flags: OpenFlags,
    ) -> FileHandle {
        assert!(!node.fs().has_permanent_entries());
        Self::new(ops, NamespaceNode::new_anonymous_unrooted(node), flags)
            .expect("Failed to create anonymous FileObject")
    }

    /// Create a FileObject with an associated NamespaceNode.
    ///
    /// This function is not typically called directly. Instead, consider
    /// calling NamespaceNode::open.
    pub fn new(
        ops: Box<dyn FileOps>,
        name: NamespaceNode,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let file_write_guard = if flags.can_write() {
            Some(name.entry.node.create_write_guard(FileWriteGuardMode::WriteFile)?)
        } else {
            None
        };
        let fs = name.entry.node.fs();
        let kernel = fs.kernel.upgrade().ok_or_else(|| errno!(ENOENT))?;
        let id = FileObjectId(kernel.next_file_object_id.next());
        let file = FileHandle::new_cyclic(|weak_handle| {
            Self {
                weak_handle: weak_handle.clone(),
                id,
                name,
                fs,
                ops,
                offset: Mutex::new(0),
                flags: Mutex::new(flags - OpenFlags::CREAT),
                async_owner: Default::default(),
                _file_write_guard: file_write_guard,
            }
            .into()
        });
        file.notify(InotifyMask::OPEN);
        Ok(file)
    }

    /// The FsNode from which this FileObject was created.
    pub fn node(&self) -> &FsNodeHandle {
        &self.name.entry.node
    }

    pub fn can_read(&self) -> bool {
        // TODO: Consider caching the access mode outside of this lock
        // because it cannot change.
        self.flags.lock().can_read()
    }

    pub fn can_write(&self) -> bool {
        // TODO: Consider caching the access mode outside of this lock
        // because it cannot change.
        self.flags.lock().can_write()
    }

    fn ops(&self) -> &dyn FileOps {
        self.ops.as_ref()
    }

    /// Returns the `FileObject`'s `FileOps` as a `&T`, or `None` if the downcast fails.
    ///
    /// This is useful for syscalls that only operate on a certain type of file.
    pub fn downcast_file<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        self.ops().as_any().downcast_ref::<T>()
    }

    pub fn is_non_blocking(&self) -> bool {
        self.flags().contains(OpenFlags::NONBLOCK)
    }

    pub fn blocking_op<T, Op>(
        &self,
        current_task: &CurrentTask,
        events: FdEvents,
        deadline: Option<zx::Time>,
        mut op: Op,
    ) -> Result<T, Errno>
    where
        Op: FnMut() -> Result<T, Errno>,
    {
        // Run the operation a first time without registering a waiter in case no wait is needed.
        match op() {
            Err(errno) if errno == EAGAIN && !self.flags().contains(OpenFlags::NONBLOCK) => {}
            result => return result,
        }

        let waiter = Waiter::new();
        loop {
            // Register the waiter before running the operation to prevent a race.
            self.wait_async(current_task, &waiter, events, WaitCallback::none());
            match op() {
                Err(e) if e == EAGAIN => {}
                result => return result,
            }
            waiter
                .wait_until(current_task, deadline.unwrap_or(zx::Time::INFINITE))
                .map_err(|e| if e == ETIMEDOUT { errno!(EAGAIN) } else { e })?;
        }
    }

    pub fn is_seekable(&self) -> bool {
        self.ops().is_seekable()
    }

    pub fn has_persistent_offsets(&self) -> bool {
        self.ops().has_persistent_offsets()
    }

    /// Common implementation for `read` and `read_at`.
    fn read_internal<R>(&self, read: R) -> Result<usize, Errno>
    where
        R: FnOnce() -> Result<usize, Errno>,
    {
        if !self.can_read() {
            return error!(EBADF);
        }
        let bytes_read = read()?;

        // TODO(steveaustin) - omit updating time_access to allow info to be immutable
        // and thus allow simultaneous reads.
        self.update_atime();
        if bytes_read > 0 {
            self.notify(InotifyMask::ACCESS);
        }

        Ok(bytes_read)
    }

    pub fn read(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.read_internal(|| {
            if !self.ops().has_persistent_offsets() {
                return self.ops.read(self, current_task, 0, data);
            }

            let mut offset = self.offset.lock();
            let read = self.ops.read(self, current_task, *offset as usize, data)?;
            *offset += read as off_t;
            Ok(read)
        })
    }

    pub fn read_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        self.read_raw(current_task, offset, data)
    }

    /// Delegate the read operation to FileOps after executing the common permission check. This
    /// calls does not handle any operation related to file offsets.
    pub fn read_raw(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if offset >= MAX_LFS_FILESIZE {
            return error!(EINVAL);
        }
        self.read_internal(|| self.ops.read(self, current_task, offset, data))
    }

    /// Common checks before calling ops().write.
    fn write_common(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // We need to cap the size of `data` to prevent us from growing the file too large,
        // according to <https://man7.org/linux/man-pages/man2/write.2.html>:
        //
        //   The number of bytes written may be less than count if, for example, there is
        //   insufficient space on the underlying physical medium, or the RLIMIT_FSIZE resource
        //   limit is encountered (see setrlimit(2)),
        //
        // However, at the moment, we just check the `offset`.
        check_offset(current_task, offset)?;
        self.ops().write(self, current_task, offset, data)
    }

    /// Common wrapper work for `write` and `write_at`.
    fn write_fn<W>(&self, current_task: &CurrentTask, write: W) -> Result<usize, Errno>
    where
        W: FnOnce() -> Result<usize, Errno>,
    {
        if !self.can_write() {
            return error!(EBADF);
        }
        self.node().clear_suid_and_sgid_bits(current_task)?;
        let bytes_written = write()?;
        self.node().update_ctime_mtime();

        if bytes_written > 0 {
            self.notify(InotifyMask::MODIFY);
        }

        Ok(bytes_written)
    }

    pub fn write(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.write_fn(current_task, || {
            if !self.ops().has_persistent_offsets() {
                return self.write_common(current_task, 0, data);
            }

            let mut offset = self.offset.lock();
            let bytes_written = if self.flags().contains(OpenFlags::APPEND) {
                let _guard = self.node().append_lock.write(current_task)?;
                *offset = self.ops().seek(self, current_task, *offset, SeekTarget::End(0))?;
                self.write_common(current_task, *offset as usize, data)
            } else {
                let _guard = self.node().append_lock.read(current_task)?;
                self.write_common(current_task, *offset as usize, data)
            }?;
            *offset += bytes_written as off_t;
            Ok(bytes_written)
        })
    }

    pub fn write_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        self.write_raw(current_task, offset, data)
    }

    /// Delegate the write operation to FileOps after executing the common permission check. This
    /// calls does not handle any operation related to file offsets.
    pub fn write_raw(
        &self,
        current_task: &CurrentTask,
        mut offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.write_fn(current_task, || {
            let _guard = self.node().append_lock.read(current_task)?;

            // According to LTP test pwrite04:
            //
            //   POSIX requires that opening a file with the O_APPEND flag should have no effect on the
            //   location at which pwrite() writes data. However, on Linux, if a file is opened with
            //   O_APPEND, pwrite() appends data to the end of the file, regardless of the value of offset.
            if self.flags().contains(OpenFlags::APPEND) && self.ops().is_seekable() {
                check_offset(current_task, offset)?;
                offset = default_eof_offset(self, current_task)? as usize;
            }

            self.write_common(current_task, offset, data)
        })
    }

    pub fn seek(&self, current_task: &CurrentTask, target: SeekTarget) -> Result<off_t, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }

        if !self.ops().has_persistent_offsets() {
            return self.ops().seek(self, current_task, 0, target);
        }

        let mut offset_guard = self.offset.lock();
        let new_offset = self.ops().seek(self, current_task, *offset_guard, target)?;
        *offset_guard = new_offset;
        Ok(new_offset)
    }

    pub fn sync(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.ops().sync(self, current_task)
    }

    pub fn data_sync(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.ops().data_sync(self, current_task)
    }

    pub fn get_vmo(
        &self,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        if prot.contains(ProtectionFlags::READ) && !self.can_read() {
            return error!(EACCES);
        }
        if prot.contains(ProtectionFlags::WRITE) && !self.can_write() {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().get_vmo(self, current_task, length, prot)
    }

    pub fn mmap(
        &self,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        if prot_flags.intersects(ProtectionFlags::READ | ProtectionFlags::WRITE) && !self.can_read()
        {
            return error!(EACCES);
        }
        if prot_flags.contains(ProtectionFlags::WRITE)
            && !self.can_write()
            && options.contains(MappingOptions::SHARED)
        {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().mmap(self, current_task, addr, vmo_offset, length, prot_flags, options, filename)
    }

    pub fn readdir(
        &self,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        if self.name.entry.is_dead() {
            return error!(ENOENT);
        }

        self.ops().readdir(self, current_task, sink)?;
        self.update_atime();
        Ok(())
    }

    pub fn ioctl(
        &self,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.ops().ioctl(self, current_task, request, arg)
    }

    pub fn fcntl(
        &self,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        self.ops().fcntl(self, current_task, cmd, arg)
    }

    pub fn ftruncate(&self, current_task: &CurrentTask, length: u64) -> Result<(), Errno> {
        // The file must be opened with write permissions. Otherwise
        // truncating it is forbidden.
        if !self.can_write() {
            return error!(EINVAL);
        }
        self.node().ftruncate(current_task, length)
    }

    pub fn fallocate(
        &self,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno> {
        // If the file is a pipe or FIFO, ESPIPE is returned.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if self.node().is_fifo() {
            return error!(ESPIPE);
        }

        // Must be a regular file or directory.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if !self.node().is_dir() && !self.node().is_reg() {
            return error!(ENODEV);
        }

        // The file must be opened with write permissions. Otherwise operation is forbidden.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if !self.can_write() {
            return error!(EBADF);
        }

        self.node().fallocate(current_task, mode, offset, length)
    }

    pub fn to_handle(&self, current_task: &CurrentTask) -> Result<Option<zx::Handle>, Errno> {
        self.ops().to_handle(self, current_task)
    }

    pub fn as_pid(&self) -> Result<pid_t, Errno> {
        self.ops().as_pid(self)
    }

    pub fn update_file_flags(&self, value: OpenFlags, mask: OpenFlags) {
        let mask_bits = mask.bits();
        let mut flags = self.flags.lock();
        let bits = (flags.bits() & !mask_bits) | (value.bits() & mask_bits);
        *flags = OpenFlags::from_bits_truncate(bits);
    }

    pub fn flags(&self) -> OpenFlags {
        *self.flags.lock()
    }

    /// Get the async owner of this file.
    ///
    /// See fcntl(F_GETOWN)
    pub fn get_async_owner(&self) -> FileAsyncOwner {
        *self.async_owner.lock()
    }

    /// Set the async owner of this file.
    ///
    /// See fcntl(F_SETOWN)
    pub fn set_async_owner(&self, owner: FileAsyncOwner) {
        *self.async_owner.lock() = owner;
    }

    /// Wait on the specified events and call the EventHandler when ready
    pub fn wait_async(
        &self,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        mut handler: EventHandler,
    ) -> Option<WaitCanceler> {
        handler.add_mapping(add_equivalent_fd_events);
        self.ops().wait_async(self, current_task, waiter, events, handler)
    }

    /// The events currently active on this file.
    pub fn query_events(&self, current_task: &CurrentTask) -> Result<FdEvents, Errno> {
        self.ops().query_events(self, current_task).map(add_equivalent_fd_events)
    }

    pub fn record_lock(
        &self,
        current_task: &CurrentTask,
        cmd: RecordLockCommand,
        flock: uapi::flock,
    ) -> Result<Option<uapi::flock>, Errno> {
        self.node().record_lock(current_task, self, cmd, flock)
    }

    pub fn flush(&self, current_task: &CurrentTask, id: FdTableId) {
        self.name.entry.node.record_lock_release(RecordLockOwner::FdTable(id));
        self.ops().flush(self, current_task)
    }

    // Notifies watchers on the current node and its parent about an event.
    pub fn notify(&self, event_mask: InotifyMask) {
        self.name.notify(event_mask)
    }

    fn update_atime(&self) {
        if !self.flags().contains(OpenFlags::NOATIME) {
            self.name.update_atime();
        }
    }
}

impl Releasable for FileObject {
    type Context<'a> = &'a CurrentTask;

    fn release(self, current_task: Self::Context<'_>) {
        self.ops().close(&self, current_task);
        self.name.entry.node.on_file_closed(&self);
        let event =
            if self.can_write() { InotifyMask::CLOSE_WRITE } else { InotifyMask::CLOSE_NOWRITE };
        self.notify(event);
    }
}

impl fmt::Debug for FileObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileObject")
            .field("name", &self.name)
            .field("fs", &String::from_utf8_lossy(self.fs.name()))
            .field("offset", &self.offset)
            .field("flags", &self.flags)
            .field("ops_ty", &self.ops().type_name())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        fs::{
            buffers::{VecInputBuffer, VecOutputBuffer},
            tmpfs::TmpFs,
            MountInfo,
        },
        testing::*,
    };
    use starnix_uapi::{
        auth::FsCred, device_type::DeviceType, file_mode::FileMode, open_flags::OpenFlags,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use zerocopy::{AsBytes, FromBytes, LE, U64};

    #[::fuchsia::test]
    async fn test_append_truncate_race() {
        let (kernel, current_task) = create_kernel_and_task();
        let root_fs = TmpFs::new_fs(&kernel);
        let mount = MountInfo::detached();
        let root_node = Arc::clone(root_fs.root());
        let file = root_node
            .create_entry(&current_task, &mount, b"test", |dir, mount, name| {
                dir.mknod(
                    &current_task,
                    mount,
                    name,
                    FileMode::IFREG | FileMode::ALLOW_ALL,
                    DeviceType::NONE,
                    FsCred::root(),
                )
            })
            .expect("create_node failed");
        let file_handle = file
            .open_anonymous(&current_task, OpenFlags::APPEND | OpenFlags::RDWR)
            .expect("open failed");
        let done = Arc::new(AtomicBool::new(false));

        let fh = file_handle.clone();
        let done_clone = done.clone();
        let write_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |_, current_task| {
                for i in 0..2000 {
                    fh.write(current_task, &mut VecInputBuffer::new(U64::<LE>::new(i).as_bytes()))
                        .expect("write failed");
                }
                done_clone.store(true, Ordering::SeqCst);
            });

        let fh = file_handle.clone();
        let done_clone = done.clone();
        let truncate_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |_, current_task| {
                while !done_clone.load(Ordering::SeqCst) {
                    fh.ftruncate(current_task, 0).expect("truncate failed");
                }
            });

        // If we read from the file, we should always find an increasing sequence. If there are
        // races, then we might unexpectedly see zeroes.
        while !done.load(Ordering::SeqCst) {
            let mut buffer = VecOutputBuffer::new(4096);
            let amount = file_handle.read_at(&current_task, 0, &mut buffer).expect("read failed");
            let mut last = None;
            let buffer = &Vec::from(buffer)[..amount];
            for i in buffer.chunks_exact(8).map(|chunk| U64::<LE>::read_from(chunk).unwrap()) {
                if let Some(last) = last {
                    assert!(i.get() > last, "buffer: {:?}", buffer);
                }
                last = Some(i.get());
            }
        }

        write_thread.await.expect("join");
        truncate_thread.await.expect("join");
    }
}
