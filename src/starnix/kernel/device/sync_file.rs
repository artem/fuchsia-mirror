// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::fuchsia::RemoteFileObject,
    mm::{MemoryAccessorExt, ProtectionFlags},
    task::{
        CurrentTask, EventHandler, ManyZxHandleSignalHandler, SignalHandler, SignalHandlerInner,
        WaitCanceler, Waiter,
    },
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, Anon, FdFlags, FdNumber, FileObject, FileOps,
    },
};
use fidl::HandleBased;
use fuchsia_zircon as zx;
use fuchsia_zircon::AsHandleRef;
use starnix_lifecycle::AtomicUsizeCounter;
use starnix_logging::{impossible_error, log_warn};
use starnix_sync::{FileOpsIoctl, Locked, ReadOps, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    c_char, error,
    errors::Errno,
    open_flags::OpenFlags,
    sync_fence_info, sync_file_info, sync_merge_data,
    user_address::{UserAddress, UserRef},
    vfs::FdEvents,
    SYNC_IOC_MAGIC,
};
use std::{collections::HashSet, sync::Arc};

// Implementation of the sync framework described at:
// https://source.android.com/docs/core/graphics/sync
//
// A sync point "is a single value or point on a sync_timeline. A point has three states: active,
// signaled, and error. Points start in the active state and transition to the signaled or error
// states."  A timestamp of the state transition is returned by the ioctl SYNC_IOC_FILE_INFO,
// so we use VMOs to implement the sync point.  The timestamp is stored in the first 8 bytes of
// the VMO and should be stored before the object signal state change.  This timestamp is always
// early; while in most cases only by a slight amount, the difference could be substantial if the
// signaling thread is de-scheduled in the middle of the two syscalls.
// TODO(b/305781995) - use events instead of VMOs.
//

const SYNC_IOC_MERGE: u8 = 3;
const SYNC_IOC_FILE_INFO: u8 = 4;

#[derive(Clone)]
pub enum Timeline {
    Magma,
    Hwc,
}

#[derive(PartialEq, Copy, Clone)]
// Error status (-1) is not currently used.
pub enum Status {
    Active = 0,
    Signaled = 1,
}

#[derive(Clone)]
pub struct SyncPoint {
    pub timeline: Timeline,
    pub handle: Arc<zx::Vmo>,
}

pub struct SyncFence {
    pub sync_points: Vec<SyncPoint>,
}

pub struct SyncFile {
    pub name: [u8; 32],
    pub fence: SyncFence,
}

struct FenceState {
    status: Status,
    timestamp_ns: u64,
}

impl SyncFile {
    const SIGNAL: zx::Signals = zx::Signals::USER_0;

    pub fn new(name: [u8; 32], fence: SyncFence) -> SyncFile {
        SyncFile { name, fence }
    }

    fn get_fence_state(&self) -> Vec<FenceState> {
        let mut state: Vec<FenceState> = vec![];

        for sync_point in &self.fence.sync_points {
            if sync_point.handle.wait_handle(zx::Signals::USER_0, zx::Time::ZERO)
                == Err(zx::Status::TIMED_OUT)
            {
                state.push(FenceState { status: Status::Active, timestamp_ns: 0 });
            } else {
                // Object is signaled so it should be safe to read the timestamp now.
                let mut vmo_bytes = vec![0; 8];
                let result = sync_point.handle.read(&mut vmo_bytes, /*offset=*/ 0);
                assert!(result.is_ok());

                let timestamp_ns = u64::from_le_bytes(vmo_bytes[0..8].try_into().unwrap());
                state.push(FenceState { status: Status::Signaled, timestamp_ns });
            }
        }
        state
    }
}

impl FileOps for SyncFile {
    fileops_impl_nonseekable!();

    fn to_handle(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        assert!(self.fence.sync_points.len() == 1);
        let vmo_dupe = self.fence.sync_points[0]
            .handle
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(impossible_error)?;
        Ok(Some(vmo_dupe.into()))
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, FileOpsIoctl>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        let ioctl_type = (request >> 8) as u8;
        let ioctl_number = request as u8;

        if ioctl_type != SYNC_IOC_MAGIC {
            log_warn!("Unexpected type {:?}", ioctl_type);
            return error!(EINVAL);
        }

        match ioctl_number {
            SYNC_IOC_MERGE => {
                let user_ref = UserRef::new(user_addr);
                let mut merge_data: sync_merge_data = current_task.read_object(user_ref)?;
                let file2 = current_task.files.get(FdNumber::from_raw(merge_data.fd2))?;

                let mut fence = SyncFence { sync_points: vec![] };
                let mut set = HashSet::<zx::Koid>::new();

                for sync_point in &self.fence.sync_points {
                    let koid = sync_point.handle.get_koid().unwrap();
                    if set.insert(koid) {
                        fence.sync_points.push(sync_point.clone());
                    }
                }

                if let Some(file2) = file2.downcast_file::<SyncFile>() {
                    for sync_point in &file2.fence.sync_points {
                        let koid = sync_point.handle.get_koid().unwrap();
                        if set.insert(koid) {
                            fence.sync_points.push(sync_point.clone());
                        }
                    }
                } else if let Some(file2) = file2.downcast_file::<RemoteFileObject>() {
                    let vmo = file2.get_vmo(file, current_task, None, ProtectionFlags::READ)?;
                    let koid = vmo.get_koid().unwrap();
                    if set.insert(koid) {
                        fence.sync_points.push(SyncPoint { timeline: Timeline::Hwc, handle: vmo });
                    }
                } else {
                    return error!(EINVAL);
                }

                // TODO(https://fxbug.dev/42078834): remove sync points that are already signaled?
                let name = merge_data.name.map(|x| x as u8);
                let file = Anon::new_file(
                    current_task,
                    Box::new(SyncFile::new(name, fence)),
                    OpenFlags::RDWR,
                );

                let fd = current_task.add_file(file, FdFlags::empty())?;
                merge_data.fence = fd.raw();

                current_task.write_object(user_ref, &merge_data)?;
                Ok(SUCCESS)
            }
            SYNC_IOC_FILE_INFO => {
                let user_ref = UserRef::new(user_addr);
                let mut info: sync_file_info = current_task.read_object(user_ref)?;

                for i in 0..self.name.len() {
                    info.name[i] = self.name[i] as c_char;
                }
                info.status = 0;

                if info.num_fences == 0 {
                    info.num_fences = self.fence.sync_points.len() as u32;
                } else if info.num_fences > self.fence.sync_points.len() as u32 {
                    return error!(EINVAL);
                } else {
                    let fence_state = self.get_fence_state();
                    let mut user_addr = info.sync_fence_info;

                    let mut sync_file_status = 1;
                    for (i, state) in fence_state.iter().enumerate() {
                        if state.status == Status::Active {
                            sync_file_status = 0;
                        }
                        if i < info.num_fences as usize {
                            // Note: obj_name not supported.
                            let mut fence_info = sync_fence_info {
                                status: state.status as i32,
                                timestamp_ns: state.timestamp_ns,
                                ..sync_fence_info::default()
                            };
                            let driver_name = match self.fence.sync_points[i].timeline {
                                Timeline::Magma => b"Magma\0",
                                Timeline::Hwc => b"Hwc\0\0\0",
                            };
                            assert!(driver_name.len() <= fence_info.driver_name.len());
                            for i in 0..driver_name.len() {
                                fence_info.driver_name[i] = driver_name[i] as c_char;
                            }

                            let fence_user_ref = UserRef::new(UserAddress::from(user_addr));
                            user_addr += std::mem::size_of::<sync_fence_info>() as u64;

                            current_task.write_object(fence_user_ref, &fence_info)?;
                        }
                    }

                    info.status = sync_file_status;
                }

                current_task.write_object(user_ref, &info)?;
                Ok(SUCCESS)
            }
            _ => {
                error!(EINVAL)
            }
        }
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        event_handler: EventHandler,
    ) -> Option<WaitCanceler> {
        if !events.contains(FdEvents::POLLIN) {
            return None;
        }

        let count = Arc::<AtomicUsizeCounter>::new(0.into());

        let mut canceler = WaitCanceler::new_noop();

        for sync_point in &self.fence.sync_points {
            let signal_handler = SignalHandler {
                inner: SignalHandlerInner::ManyZxHandle(ManyZxHandleSignalHandler {
                    count: self.fence.sync_points.len(),
                    counter: count.clone(),
                    expected_signals: Self::SIGNAL,
                    events: FdEvents::POLLIN,
                }),
                event_handler: event_handler.clone(),
            };

            let canceler_result = waiter.wake_on_zircon_signals(
                sync_point.handle.as_ref(),
                Self::SIGNAL,
                signal_handler,
            );
            let canceler_result = match canceler_result {
                Ok(o) => o,
                Err(e) => {
                    log_warn!("Error returned from wake_on_zircon_signals: {:?}", e);
                    return None;
                }
            };

            // The wakeup is edge triggered, so handles that were already signaled will never get
            // a callback. Normally the "already signaled" case is handled by a call to
            // query_events() after this query_async() returns; however that works only if all
            // handles are signaled.  Here we perform the counting, and cancel waits, for any
            // handles currently signaled.
            if sync_point.handle.wait_handle(Self::SIGNAL, zx::Time::ZERO)
                == Err(zx::Status::TIMED_OUT)
            {
                canceler = WaitCanceler::merge_unbounded(
                    canceler,
                    WaitCanceler::new_vmo(Arc::downgrade(&sync_point.handle), canceler_result),
                );
            } else {
                canceler_result.cancel(sync_point.handle.as_handle_ref());

                count.next();
            }
        }

        Some(canceler)
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let fence_state = self.get_fence_state();

        for state in fence_state.iter() {
            if state.status == Status::Active {
                return Ok(FdEvents::empty());
            }
        }

        Ok(FdEvents::POLLIN)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, ReadOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENODEV)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENODEV)
    }
}
