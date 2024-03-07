// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::{
    ffi::{
        create_connection, create_image, device_import, device_release, execute_command,
        execute_immediate_commands, export_buffer, flush, get_buffer_handle, import_semaphore2,
        query, read_notification_channel, release_connection,
    },
    image_file::ImageFile,
    image_file::ImageInfo,
    magma::read_control_and_response,
    magma::read_magma_command_and_type,
    magma::StarnixPollItem,
};
use fuchsia_zircon as zx;
use fuchsia_zircon::HandleBased;
use magma::{
    magma_buffer_clean_cache, magma_buffer_get_cache_policy, magma_buffer_get_info,
    magma_buffer_id_t, magma_buffer_info_t, magma_buffer_set_cache_policy, magma_buffer_set_name,
    magma_buffer_t, magma_cache_operation_t, magma_cache_policy_t, magma_connection_create_buffer,
    magma_connection_create_context, magma_connection_get_error,
    magma_connection_get_notification_channel_handle, magma_connection_import_buffer,
    magma_connection_map_buffer, magma_connection_perform_buffer_op, magma_connection_release,
    magma_connection_release_buffer, magma_connection_release_context,
    magma_connection_release_semaphore, magma_connection_t, magma_connection_unmap_buffer,
    magma_device_release, magma_device_t, magma_initialize_logging, magma_poll, magma_poll_item,
    magma_poll_item_t, magma_semaphore_export, magma_semaphore_id_t, magma_semaphore_reset,
    magma_semaphore_signal, magma_semaphore_t, virtio_magma_buffer_clean_cache_ctrl_t,
    virtio_magma_buffer_clean_cache_resp_t, virtio_magma_buffer_export_ctrl_t,
    virtio_magma_buffer_export_resp_t, virtio_magma_buffer_get_cache_policy_ctrl_t,
    virtio_magma_buffer_get_cache_policy_resp_t, virtio_magma_buffer_get_handle_ctrl_t,
    virtio_magma_buffer_get_handle_resp_t, virtio_magma_buffer_get_info_ctrl_t,
    virtio_magma_buffer_get_info_resp_t, virtio_magma_buffer_set_cache_policy_ctrl_t,
    virtio_magma_buffer_set_cache_policy_resp_t, virtio_magma_buffer_set_name_ctrl_t,
    virtio_magma_buffer_set_name_resp_t, virtio_magma_connection_create_buffer_ctrl_t,
    virtio_magma_connection_create_buffer_resp_t, virtio_magma_connection_create_context_ctrl_t,
    virtio_magma_connection_create_context_resp_t, virtio_magma_connection_create_semaphore_ctrl_t,
    virtio_magma_connection_create_semaphore_resp_t,
    virtio_magma_connection_execute_command_ctrl_t, virtio_magma_connection_execute_command_resp_t,
    virtio_magma_connection_execute_immediate_commands_ctrl_t,
    virtio_magma_connection_execute_immediate_commands_resp_t,
    virtio_magma_connection_flush_ctrl_t, virtio_magma_connection_flush_resp_t,
    virtio_magma_connection_get_error_ctrl_t, virtio_magma_connection_get_error_resp_t,
    virtio_magma_connection_get_notification_channel_handle_ctrl_t,
    virtio_magma_connection_get_notification_channel_handle_resp_t,
    virtio_magma_connection_import_buffer_ctrl_t, virtio_magma_connection_import_buffer_resp_t,
    virtio_magma_connection_import_semaphore2_ctrl_t,
    virtio_magma_connection_import_semaphore2_resp_t, virtio_magma_connection_map_buffer_ctrl_t,
    virtio_magma_connection_map_buffer_resp_t, virtio_magma_connection_perform_buffer_op_ctrl_t,
    virtio_magma_connection_perform_buffer_op_resp_t,
    virtio_magma_connection_read_notification_channel_ctrl_t,
    virtio_magma_connection_read_notification_channel_resp_t,
    virtio_magma_connection_release_buffer_ctrl_t, virtio_magma_connection_release_buffer_resp_t,
    virtio_magma_connection_release_context_ctrl_t, virtio_magma_connection_release_context_resp_t,
    virtio_magma_connection_release_ctrl_t, virtio_magma_connection_release_resp_t,
    virtio_magma_connection_release_semaphore_ctrl_t,
    virtio_magma_connection_release_semaphore_resp_t, virtio_magma_connection_unmap_buffer_ctrl_t,
    virtio_magma_connection_unmap_buffer_resp_t,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_CLEAN_CACHE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_EXPORT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_CACHE_POLICY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_HANDLE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_INFO,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_CACHE_POLICY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_NAME,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_CONTEXT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_SEMAPHORE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_COMMAND,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_FLUSH,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_ERROR,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_SEMAPHORE2,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_MAP_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_PERFORM_BUFFER_OP,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_READ_NOTIFICATION_CHANNEL,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_CONTEXT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_SEMAPHORE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_UNMAP_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_CREATE_CONNECTION,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_IMPORT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_QUERY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_RELEASE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_POLL,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_EXPORT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_RESET,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_SIGNAL,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_CREATE_IMAGE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_GET_IMAGE_INFO,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_CLEAN_CACHE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_CACHE_POLICY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_INFO,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_CACHE_POLICY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_NAME,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_CONTEXT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_SEMAPHORE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_COMMAND,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_ERROR,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_SEMAPHORE2,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_MAP_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_PERFORM_BUFFER_OP,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_CONTEXT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_SEMAPHORE,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_UNMAP_BUFFER,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_QUERY,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_POLL,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_EXPORT,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_RESET,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_SIGNAL,
    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_VIRT_CONNECTION_GET_IMAGE_INFO,
    virtio_magma_device_create_connection_ctrl, virtio_magma_device_create_connection_resp_t,
    virtio_magma_device_query_ctrl_t, virtio_magma_device_query_resp_t,
    virtio_magma_device_release_ctrl_t, virtio_magma_device_release_resp_t,
    virtio_magma_poll_ctrl_t, virtio_magma_poll_resp_t, virtio_magma_semaphore_export_ctrl_t,
    virtio_magma_semaphore_export_resp_t, virtio_magma_semaphore_reset_ctrl_t,
    virtio_magma_semaphore_reset_resp_t, virtio_magma_semaphore_signal_ctrl_t,
    virtio_magma_semaphore_signal_resp_t, virtio_magma_virt_connection_create_image_ctrl_t,
    virtio_magma_virt_connection_create_image_resp_t,
    virtio_magma_virt_connection_get_image_info_ctrl_t,
    virtio_magma_virt_connection_get_image_info_resp_t, virtmagma_buffer_set_name_wrapper,
    MAGMA_CACHE_POLICY_CACHED, MAGMA_IMPORT_SEMAPHORE_ONE_SHOT, MAGMA_POLL_CONDITION_SIGNALED,
    MAGMA_POLL_TYPE_SEMAPHORE, MAGMA_STATUS_INVALID_ARGS, MAGMA_STATUS_MEMORY_ERROR,
    MAGMA_STATUS_OK, MAGMA_STATUS_TIMED_OUT,
};
use starnix_core::{
    device::sync_file::{SyncFence, SyncFile, SyncPoint, Timeline},
    fileops_impl_nonseekable,
    fs::fuchsia::RemoteFileObject,
    mm::{MemoryAccessorExt, ProtectionFlags},
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        Anon, FdFlags, FdNumber, FileObject, FileOps, FsNode, VmoFileObject,
    },
};
use starnix_lifecycle::AtomicU64Counter;
use starnix_logging::{impossible_error, log_error, log_warn, set_zx_name, track_stub};
use starnix_sync::{FileOpsIoctl, Locked, Mutex, ReadOps, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    open_flags::OpenFlags,
    user_address::{UserAddress, UserRef},
    user_buffer::UserBuffer,
};
use std::{
    collections::HashMap,
    sync::{Arc, Once},
};

#[derive(Clone)]
pub enum BufferInfo {
    Default,
    Image(ImageInfo),
}

/// A `MagmaConnection` is an RAII wrapper around a `magma_connection_t`.
pub struct MagmaConnection {
    pub handle: magma_connection_t,
}

impl Drop for MagmaConnection {
    fn drop(&mut self) {
        unsafe { magma_connection_release(self.handle) }
    }
}

/// A `MagmaDevice` is an RAII wrapper around a `magma_device_t`.
pub struct MagmaDevice {
    pub handle: magma_device_t,
}

impl Drop for MagmaDevice {
    /// SAFETY: Makes an FFI call to release a handle that was imported using `magma_device_import`.
    fn drop(&mut self) {
        unsafe { magma_device_release(self.handle) }
    }
}

/// A `MagmaBuffer` is an RAII wrapper around a `magma_buffer_t`.
pub struct MagmaBuffer {
    // This reference is needed to release the buffer.
    pub connection: Arc<MagmaConnection>,
    pub handle: magma_buffer_t,
}

impl Drop for MagmaBuffer {
    /// SAFETY: Makes an FFI call to release a a `magma_buffer_t` handle. `connection.handle` must
    /// be valid because `connection` is refcounted.
    fn drop(&mut self) {
        unsafe { magma_connection_release_buffer(self.connection.handle, self.handle) }
    }
}

/// A `MagmaSemaphore` is an RAII wrapper around one or more `magma_semaphore_t`.  Multiple are
/// supported because a sync file may be imported.
pub struct MagmaSemaphore {
    // This reference is needed to release the semaphores.
    pub connection: Arc<MagmaConnection>,
    pub handles: Vec<magma_semaphore_t>,
    pub ids: Vec<magma_semaphore_id_t>,
}

impl Drop for MagmaSemaphore {
    /// SAFETY: Makes an FFI call to release the `magma_semaphore_t` handles. `connection.handle` must
    /// be valid because `connection` is refcounted.
    fn drop(&mut self) {
        for handle in &self.handles {
            unsafe { magma_connection_release_semaphore(self.connection.handle, *handle) }
        }
    }
}

/// A `BufferMap` stores all the magma buffers for a given connection.
type BufferMap = HashMap<magma_buffer_t, BufferInfo>;

/// A `ConnectionMap` stores the `ConnectionInfo`s associated with each magma connection.
pub type ConnectionMap = HashMap<magma_connection_t, ConnectionInfo>;

pub struct ConnectionInfo {
    pub connection: Arc<MagmaConnection>,
    pub buffer_map: BufferMap,
}

impl ConnectionInfo {
    pub fn new(connection: Arc<MagmaConnection>) -> Self {
        Self { connection, buffer_map: HashMap::new() }
    }
}

pub type DeviceMap = HashMap<magma_device_t, MagmaDevice>;

pub struct MagmaFile {
    devices: Arc<Mutex<DeviceMap>>,
    connections: Arc<Mutex<ConnectionMap>>,
    buffers: Arc<Mutex<HashMap<magma_buffer_t, Arc<MagmaBuffer>>>>,
    semaphores: Arc<Mutex<HashMap<magma_semaphore_t, Arc<MagmaSemaphore>>>>,
    semaphore_id_generator: AtomicU64Counter,
}

impl MagmaFile {
    pub fn init() {
        // Enable the magma client library to emit logs for debug and error cases.
        let (server_end, client_end) = zx::Channel::create();

        let result = fuchsia_component::client::connect_channel_to_protocol::<
            fidl_fuchsia_logger::LogSinkMarker,
        >(server_end);

        if result.is_ok() {
            unsafe {
                magma_initialize_logging(client_end.into_raw());
            }
        }
    }

    pub fn new_file(
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            Self::init();
        });

        Ok(Box::new(Self {
            devices: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            buffers: Arc::new(Mutex::new(HashMap::new())),
            semaphores: Arc::new(Mutex::new(HashMap::new())),
            semaphore_id_generator: AtomicU64Counter::new(1),
        }))
    }

    /// Returns a duplicate of the VMO associated with the file at `fd`, as well as a `BufferInfo`
    /// of the correct type for that file.
    ///
    /// Returns an error if the file does not contain a buffer.
    fn get_vmo_and_magma_buffer(
        current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(zx::Vmo, BufferInfo), Errno> {
        let file = current_task.files.get(fd)?;
        if let Some(file) = file.downcast_file::<ImageFile>() {
            let buffer = BufferInfo::Image(file.info.clone());
            Ok((
                file.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?,
                buffer,
            ))
        } else if let Some(file) = file.downcast_file::<VmoFileObject>() {
            let buffer = BufferInfo::Default;
            Ok((
                file.vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?,
                buffer,
            ))
        } else if file.downcast_file::<RemoteFileObject>().is_some() {
            // TODO: Currently this does not preserve BufferInfo::Image fields across allocation via
            // HIDL/AIDL gralloc followed by import here. If that turns out to be needed, we can add
            // to system the ability to get ImageFormatConstraints from a sysmem VMO, which could be
            // used instead of ImageFile. Or if not needed, maybe we can remove ImageFile without
            // any replacement.
            //
            // TODO: Consider if we can have binder related code in starnix use VmoFileObject for
            // any FD wrapping a VMO, or if that's not workable, we may want to have magma related
            // code use RemoteFileObject.
            let buffer = BufferInfo::Default;
            // Map any failure to EINVAL; any failure here is most likely to be an FD that isn't
            // a gralloc buffer.
            let vmo = file
                .get_vmo(current_task, None, ProtectionFlags::READ | ProtectionFlags::WRITE)
                .map_err(|_| errno!(EINVAL))?;
            Ok((vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).map_err(impossible_error)?, buffer))
        } else {
            error!(EINVAL)
        }
    }

    /// Adds a `BufferInfo` for the given `magma_buffer_t`, associated with the specified
    /// connection.
    fn add_buffer_info(
        &self,
        connection: Arc<MagmaConnection>,
        buffer: magma_buffer_t,
        buffer_info: BufferInfo,
    ) {
        let connection_handle = connection.handle;
        let arc_buffer = Arc::new(MagmaBuffer { connection, handle: buffer });
        self.connections
            .lock()
            .get_mut(&connection_handle)
            .map(|info| info.buffer_map.insert(buffer, buffer_info));
        self.buffers.lock().insert(buffer, arc_buffer);
    }

    /// Returns a `BufferInfo` for the given `magma_buffer_t`, if one exists for the given
    /// `connection`. Otherwise returns `None`.
    fn get_buffer_info(
        &self,
        connection: magma_connection_t,
        buffer: magma_buffer_t,
    ) -> Option<BufferInfo> {
        match self.connections.lock().get(&connection) {
            Some(buffers) => buffers.buffer_map.get(&buffer).cloned(),
            _ => None,
        }
    }

    fn get_connection(
        &self,
        connection: magma_connection_t,
    ) -> Result<Arc<MagmaConnection>, Errno> {
        Ok(self
            .connections
            .lock()
            .get(&connection)
            .ok_or_else(|| errno!(EINVAL))?
            .connection
            .clone())
    }

    fn get_buffer(&self, buffer: magma_buffer_t) -> Result<Arc<MagmaBuffer>, Errno> {
        Ok(self.buffers.lock().get(&buffer).ok_or_else(|| errno!(EINVAL))?.clone())
    }

    fn get_semaphore(&self, semaphore: magma_semaphore_t) -> Result<Arc<MagmaSemaphore>, i32> {
        Ok(self.semaphores.lock().get(&semaphore).ok_or_else(|| MAGMA_STATUS_INVALID_ARGS)?.clone())
    }

    fn import_semaphore2(
        &self,
        current_task: &CurrentTask,
        control: &virtio_magma_connection_import_semaphore2_ctrl_t,
        response: &mut virtio_magma_connection_import_semaphore2_resp_t,
    ) {
        let mut status: i32 = MAGMA_STATUS_OK;

        let fd = FdNumber::from_raw(control.semaphore_handle as i32);
        let mut result_semaphore_id = 0;

        if let (Ok(connection), Ok(file)) =
            (self.get_connection(control.connection), current_task.files.get(fd))
        {
            let mut handles: Vec<magma_semaphore_t> = vec![];
            let mut ids: Vec<magma_semaphore_id_t> = vec![];

            if let Some(sync_file) = file.downcast_file::<SyncFile>() {
                for sync_point in &sync_file.fence.sync_points {
                    if let Ok(vmo) = sync_point.handle.duplicate_handle(zx::Rights::SAME_RIGHTS) {
                        if control.flags & MAGMA_IMPORT_SEMAPHORE_ONE_SHOT == 0 {
                            // For most non-test cases, one shot should be specified.
                            log_warn!(
                                "Importing magma semaphore without MAGMA_IMPORT_SEMAPHORE_ONE_SHOT"
                            );
                        }
                        let semaphore;
                        let semaphore_id;
                        (status, semaphore, semaphore_id) =
                            import_semaphore2(&connection, vmo, control.flags);
                        if status != MAGMA_STATUS_OK {
                            break;
                        }
                        handles.push(semaphore as magma_semaphore_t);
                        ids.push(semaphore_id as magma_semaphore_id_t);
                    } else {
                        status = MAGMA_STATUS_MEMORY_ERROR;
                        break;
                    }
                }
            } else {
                status = MAGMA_STATUS_INVALID_ARGS;
            }

            if status == MAGMA_STATUS_OK {
                result_semaphore_id = self.semaphore_id_generator.next();

                self.semaphores.lock().insert(
                    result_semaphore_id,
                    Arc::new(MagmaSemaphore { connection, handles, ids }),
                );
            }
        } else {
            status = MAGMA_STATUS_INVALID_ARGS;
        }

        // Import is expected to close the file that was imported.
        let _ = current_task.files.close(fd);

        response.result_return = status as u64;
        response.semaphore_out = result_semaphore_id;
        response.id_out = result_semaphore_id;
        response.hdr.type_ =
            virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_SEMAPHORE2 as u32;
    }
}

impl FileOps for MagmaFile {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, FileOpsIoctl>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        let (command, command_type) = read_magma_command_and_type(current_task, user_addr)?;
        let response_address = UserAddress::from(command.response_address);

        match command_type {
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_IMPORT => {
                let (control, mut response) = read_control_and_response(current_task, &command)?;
                let device = device_import(control, &mut response)?;
                (*self.devices.lock()).insert(device.handle, device);

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_CREATE_CONNECTION => {
                let (control, mut response): (
                    virtio_magma_device_create_connection_ctrl,
                    virtio_magma_device_create_connection_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                create_connection(control, &mut response, &mut self.connections.lock());
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE => {
                let (control, mut response): (
                    virtio_magma_connection_release_ctrl_t,
                    virtio_magma_connection_release_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                release_connection(control, &mut response, &mut self.connections.lock());

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_RELEASE => {
                let (control, mut response): (
                    virtio_magma_device_release_ctrl_t,
                    virtio_magma_device_release_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                device_release(control, &mut response, &mut self.devices.lock());

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_CREATE_IMAGE => {
                let (control, mut response): (
                    virtio_magma_virt_connection_create_image_ctrl_t,
                    virtio_magma_virt_connection_create_image_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let connection = self.get_connection(control.connection)?;

                if let Ok(buffer) = create_image(current_task, control, &mut response, &connection)
                {
                    self.add_buffer_info(connection, response.image_out, buffer);
                }

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_VIRT_CONNECTION_GET_IMAGE_INFO => {
                let (control, mut response): (
                    virtio_magma_virt_connection_get_image_info_ctrl_t,
                    virtio_magma_virt_connection_get_image_info_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                let image_info_address_ref =
                    UserRef::new(UserAddress::from(control.image_info_out));
                let image_info_ptr = current_task.read_object(image_info_address_ref)?;

                match self.get_buffer_info(
                    control.connection as magma_connection_t,
                    control.image as magma_buffer_t,
                ) {
                    Some(BufferInfo::Image(image_info)) => {
                        let image_info_ref = UserRef::new(image_info_ptr);
                        current_task.write_object(image_info_ref, &image_info.info)?;
                        response.result_return = MAGMA_STATUS_OK as u64;
                    }
                    _ => {
                        log_error!("No image info was found for buffer: {:?}", { control.image });
                        response.result_return = MAGMA_STATUS_INVALID_ARGS as u64;
                    }
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_VIRT_CONNECTION_GET_IMAGE_INFO as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_FLUSH => {
                let (control, mut response): (
                    virtio_magma_connection_flush_ctrl_t,
                    virtio_magma_connection_flush_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                flush(control, &mut response, &connection);

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_READ_NOTIFICATION_CHANNEL => {
                let (control, mut response): (
                    virtio_magma_connection_read_notification_channel_ctrl_t,
                    virtio_magma_connection_read_notification_channel_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                read_notification_channel(current_task, control, &mut response, &connection)?;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_HANDLE => {
                let (control, mut response): (
                    virtio_magma_buffer_get_handle_ctrl_t,
                    virtio_magma_buffer_get_handle_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                get_buffer_handle(current_task, control, &mut response, &buffer)?;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_release_buffer_ctrl_t,
                    virtio_magma_connection_release_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                if let Some(buffers) = self.connections.lock().get_mut(&{ control.connection }) {
                    match buffers.buffer_map.remove(&{ control.buffer }) {
                        Some(_) => (),
                        _ => {
                            log_error!("Calling magma_release_buffer with an invalid buffer.");
                        }
                    };
                }
                self.buffers.lock().remove(&{ control.buffer });

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_BUFFER as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_EXPORT => {
                let (control, mut response): (
                    virtio_magma_buffer_export_ctrl_t,
                    virtio_magma_buffer_export_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                export_buffer(
                    current_task,
                    control,
                    &mut response,
                    &buffer,
                    &self.connections.lock(),
                )?;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_import_buffer_ctrl_t,
                    virtio_magma_connection_import_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                let buffer_fd = FdNumber::from_raw(control.buffer_handle as i32);
                let (vmo, buffer) = MagmaFile::get_vmo_and_magma_buffer(current_task, buffer_fd)?;

                let mut buffer_out = magma_buffer_t::default();
                let mut size_out = 0u64;
                let mut id_out = magma_buffer_id_t::default();
                response.result_return = unsafe {
                    magma_connection_import_buffer(
                        connection.handle,
                        vmo.into_raw(),
                        &mut size_out,
                        &mut buffer_out,
                        &mut id_out,
                    ) as u64
                };

                // Store the information for the newly imported buffer.
                self.add_buffer_info(connection, buffer_out, buffer);
                // Import is expected to close the file that was imported.
                let _ = current_task.files.close(buffer_fd);

                response.buffer_out = buffer_out;
                response.size_out = size_out;
                response.id_out = id_out;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_IMPORT_BUFFER as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE => {
                let (control, mut response): (
                    virtio_magma_connection_get_notification_channel_handle_ctrl_t,
                    virtio_magma_connection_get_notification_channel_handle_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                response.result_return =
                    unsafe { magma_connection_get_notification_channel_handle(connection.handle) };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_NOTIFICATION_CHANNEL_HANDLE as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_CONTEXT => {
                let (control, mut response): (
                    virtio_magma_connection_create_context_ctrl_t,
                    virtio_magma_connection_create_context_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                let mut context_id_out = 0;
                response.result_return = unsafe {
                    magma_connection_create_context(connection.handle, &mut context_id_out) as u64
                };
                response.context_id_out = context_id_out as u64;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_CONTEXT as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_CONTEXT => {
                let (control, mut response): (
                    virtio_magma_connection_release_context_ctrl_t,
                    virtio_magma_connection_release_context_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                unsafe {
                    magma_connection_release_context(connection.handle, control.context_id);
                }

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_CONTEXT as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_create_buffer_ctrl_t,
                    virtio_magma_connection_create_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                let mut size_out = 0;
                let mut buffer_out = 0;
                let mut id_out = 0;
                response.result_return = unsafe {
                    magma_connection_create_buffer(
                        connection.handle,
                        control.size,
                        &mut size_out,
                        &mut buffer_out,
                        &mut id_out,
                    ) as u64
                };
                response.size_out = size_out;
                response.buffer_out = buffer_out;
                response.id_out = id_out;
                self.add_buffer_info(connection, buffer_out, BufferInfo::Default);

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_BUFFER as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_CREATE_SEMAPHORE => {
                let (control, mut response): (
                    virtio_magma_connection_create_semaphore_ctrl_t,
                    virtio_magma_connection_create_semaphore_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;
                let status: i32;
                let mut result_semaphore_id = 0;

                // Use VMO semaphores for compatibility with sync files, which need timestamps.
                if let Ok(vmo) = zx::Vmo::create(/*size=*/ 1) {
                    let flags: u64 = 0;
                    set_zx_name(&vmo, b"magma semaphore");
                    let semaphore;
                    let semaphore_id;
                    (status, semaphore, semaphore_id) = import_semaphore2(&connection, vmo, flags);
                    if status == MAGMA_STATUS_OK {
                        result_semaphore_id = self.semaphore_id_generator.next();

                        self.semaphores.lock().insert(
                            result_semaphore_id,
                            Arc::new(MagmaSemaphore {
                                connection,
                                handles: vec![semaphore; 1],
                                ids: vec![semaphore_id; 1],
                            }),
                        );
                    }
                } else {
                    status = MAGMA_STATUS_MEMORY_ERROR;
                }
                response.result_return = status as u64;
                response.semaphore_out = result_semaphore_id;
                response.id_out = result_semaphore_id;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_CREATE_SEMAPHORE as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_GET_ERROR => {
                let (control, mut response): (
                    virtio_magma_connection_get_error_ctrl_t,
                    virtio_magma_connection_get_error_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                response.result_return =
                    unsafe { magma_connection_get_error(connection.handle) as u64 };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_GET_ERROR as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_IMPORT_SEMAPHORE2 => {
                let (control, mut response): (
                    virtio_magma_connection_import_semaphore2_ctrl_t,
                    virtio_magma_connection_import_semaphore2_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                self.import_semaphore2(current_task, &control, &mut response);

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_RELEASE_SEMAPHORE => {
                let (control, mut response): (
                    virtio_magma_connection_release_semaphore_ctrl_t,
                    virtio_magma_connection_release_semaphore_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                self.semaphores.lock().remove(&{ control.semaphore });

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_RELEASE_SEMAPHORE as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_EXPORT => {
                let (control, mut response): (
                    virtio_magma_semaphore_export_ctrl_t,
                    virtio_magma_semaphore_export_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let mut status: i32 = MAGMA_STATUS_OK;

                let mut sync_points: Vec<SyncPoint> = vec![];
                let mut sync_file_fd: i32 = -1;

                match self.get_semaphore(control.semaphore) {
                    Ok(semaphore) => {
                        for handle in &semaphore.handles {
                            let mut vmo_handle = 0;
                            status = unsafe { magma_semaphore_export(*handle, &mut vmo_handle) };
                            if status != MAGMA_STATUS_OK {
                                break;
                            }

                            let vmo = unsafe { zx::Vmo::from(zx::Handle::from_raw(vmo_handle)) };

                            sync_points.push(SyncPoint {
                                timeline: Timeline::Magma,
                                handle: Arc::new(vmo),
                            });
                        }
                    }
                    Err(s) => status = s,
                }

                if status == MAGMA_STATUS_OK {
                    let sync_file_name: &[u8; 32] =
                        b"magma semaphore\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
                    let sync_file = SyncFile::new(*sync_file_name, SyncFence { sync_points });

                    let file = Anon::new_file(current_task, Box::new(sync_file), OpenFlags::RDWR);

                    let fd = current_task.add_file(file, FdFlags::empty())?;
                    sync_file_fd = fd.raw();
                }

                response.result_return = status as u64;
                response.semaphore_handle_out = sync_file_fd as u64;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_EXPORT as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_RESET => {
                let (control, mut response): (
                    virtio_magma_semaphore_reset_ctrl_t,
                    virtio_magma_semaphore_reset_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                if let Ok(semaphore) = self.get_semaphore(control.semaphore) {
                    for handle in &semaphore.handles {
                        unsafe {
                            magma_semaphore_reset(*handle);
                        }
                    }
                }
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_RESET as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_SEMAPHORE_SIGNAL => {
                let (control, mut response): (
                    virtio_magma_semaphore_signal_ctrl_t,
                    virtio_magma_semaphore_signal_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                if let Ok(semaphore) = self.get_semaphore(control.semaphore) {
                    for handle in &semaphore.handles {
                        unsafe {
                            magma_semaphore_signal(*handle);
                        }
                    }
                }
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_SEMAPHORE_SIGNAL as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_MAP_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_map_buffer_ctrl_t,
                    virtio_magma_connection_map_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;
                let buffer = self.get_buffer(control.buffer)?;

                response.result_return = unsafe {
                    magma_connection_map_buffer(
                        connection.handle,
                        control.hw_va,
                        buffer.handle,
                        control.offset,
                        control.length,
                        control.map_flags,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_MAP_BUFFER as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_POLL => {
                let (control, mut response): (virtio_magma_poll_ctrl_t, virtio_magma_poll_resp_t) =
                    read_control_and_response(current_task, &command)?;

                let num_items = control.count as usize / std::mem::size_of::<StarnixPollItem>();
                let items_ref = UserRef::<StarnixPollItem>::new(UserAddress::from(control.items));
                // Read the poll items as `StarnixPollItem`, since they contain a union. Also note
                // that the minimum length of the vector is 1, to always have a valid reference for
                // `magma_poll`.
                let starnix_items =
                    current_task.read_objects_to_vec(items_ref, std::cmp::max(num_items, 1))?;
                // Then convert each item "manually" into `magma_poll_item_t`.
                let mut magma_items: Vec<magma_poll_item_t> =
                    starnix_items.iter().map(|item| item.as_poll_item()).collect();

                // Expand semaphores.
                let mut status: i32 = MAGMA_STATUS_OK;
                let mut child_semaphore_items: Vec<magma_poll_item_t> = vec![];

                for i in 0..magma_items.len() {
                    magma_items[i].result = 0;

                    if magma_items[i].type_ == MAGMA_POLL_TYPE_SEMAPHORE
                        && magma_items[i].condition == MAGMA_POLL_CONDITION_SIGNALED
                    {
                        match self.get_semaphore(starnix_items[i].semaphore_or_handle) {
                            Ok(semaphore) => {
                                magma_items[i].condition = 0; // magma_poll must ignore this item

                                // Store the number of expanded semaphores to be signaled
                                let handles_ref = &semaphore.handles;
                                let child_count = handles_ref.len() as u32;
                                magma_items[i].unused = child_count;

                                for handle in handles_ref {
                                    child_semaphore_items.push(
                                        StarnixPollItem {
                                            semaphore_or_handle: *handle,
                                            type_: MAGMA_POLL_TYPE_SEMAPHORE,
                                            condition: MAGMA_POLL_CONDITION_SIGNALED,
                                            result: 0,
                                            // Points back to the parent item
                                            unused: i as u32,
                                        }
                                        .as_poll_item(),
                                    );
                                }
                            }
                            Err(s) => status = s,
                        }
                    }
                }

                if status == MAGMA_STATUS_OK {
                    magma_items.append(&mut child_semaphore_items);

                    let abs_timeout_ns = if control.timeout_ns == std::u64::MAX {
                        0
                    } else {
                        zx::Time::get_monotonic().into_nanos() as u64 + control.timeout_ns
                    };

                    'outer: while status == MAGMA_STATUS_OK {
                        // Iterate from the end to process child semaphores first
                        for i in (0..magma_items.len()).rev() {
                            if i < num_items && magma_items[i].result > 0 {
                                // A handle or parent semaphore is signaled, we're done
                                break 'outer;
                            } else if magma_items[i].result > 0 {
                                // A child semaphore is signaled
                                assert_eq!(magma_items[i].condition, MAGMA_POLL_CONDITION_SIGNALED);
                                let parent_index = magma_items[i].unused as usize;
                                assert_ne!(magma_items[parent_index].unused, 0);
                                magma_items[parent_index].unused -= 1;
                                if magma_items[parent_index].unused == 0 {
                                    // All children signaled, signal the parent
                                    magma_items[parent_index].result = magma_items[i].result;
                                }
                                // Don't poll on this child again
                                magma_items[i].condition = 0;
                            }
                        }

                        let current_time_ns = zx::Time::get_monotonic().into_nanos() as u64;
                        let rel_timeout_ns = if abs_timeout_ns == 0 {
                            std::u64::MAX
                        } else if abs_timeout_ns > current_time_ns {
                            abs_timeout_ns - current_time_ns
                        } else {
                            0
                        };

                        // Force EINTR every second to allow signals to interrupt the wait.
                        // TODO(https://fxbug.dev/42080364): Only interrupt the wait when needed.
                        let capped_rel_timeout_ns = std::cmp::min(rel_timeout_ns, 1_000_000_000);

                        status = unsafe {
                            magma_poll(
                                &mut magma_items[0] as *mut magma_poll_item,
                                magma_items.len() as u32,
                                capped_rel_timeout_ns,
                            )
                        };
                        let current_time = zx::Time::get_monotonic().into_nanos();

                        // Check if the wait timed out before the user-requested timeout.
                        if status == MAGMA_STATUS_TIMED_OUT
                            && (control.timeout_ns == std::u64::MAX
                                || (current_time as u64) < abs_timeout_ns)
                        {
                            if control.timeout_ns != std::u64::MAX {
                                // Update relative deadline.
                                let mut control = control;
                                control.timeout_ns = abs_timeout_ns - (current_time as u64);
                                let request_address = UserAddress::from(command.request_address);
                                let _ = current_task
                                    .write_object(UserRef::new(request_address), &control);
                            }
                            return Err(errno!(EINTR));
                        }
                    }
                }

                // Walk child items to restore modified parent items
                for i in num_items..magma_items.len() {
                    assert_eq!(magma_items[i].type_, MAGMA_POLL_TYPE_SEMAPHORE);
                    let parent_index = magma_items[i].unused as usize;
                    magma_items[parent_index].condition = MAGMA_POLL_CONDITION_SIGNALED;
                    magma_items[parent_index].unused = 0;
                }

                // Remove any child semaphores
                magma_items.truncate(num_items);

                // Convert the poll items back to a serializable version after the `magma_poll`
                // call.
                let starnix_items: Vec<StarnixPollItem> =
                    magma_items.iter().map(StarnixPollItem::new).collect();
                current_task.write_objects(items_ref, &starnix_items)?;

                response.result_return = status as u64;
                response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_POLL as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_COMMAND => {
                let (control, mut response): (
                    virtio_magma_connection_execute_command_ctrl_t,
                    virtio_magma_connection_execute_command_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                let status = execute_command(current_task, control, &connection, |semaphore_id| {
                    self.get_semaphore(semaphore_id)
                })?;

                response.result_return = status as u64;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_COMMAND as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS => {
                let (control, mut response): (
                    virtio_magma_connection_execute_immediate_commands_ctrl_t,
                    virtio_magma_connection_execute_immediate_commands_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;

                let status = execute_immediate_commands(
                    current_task,
                    control,
                    &connection,
                    |semaphore_id| self.get_semaphore(semaphore_id),
                )?;

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_EXECUTE_IMMEDIATE_COMMANDS
                        as u32;
                response.result_return = status as u64;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_DEVICE_QUERY => {
                let (control, mut response): (
                    virtio_magma_device_query_ctrl_t,
                    virtio_magma_device_query_resp_t,
                ) = read_control_and_response(current_task, &command)?;

                query(current_task, control, &mut response)?;

                response.hdr.type_ = virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_DEVICE_QUERY as u32;
                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_UNMAP_BUFFER => {
                let (control, mut response): (
                    virtio_magma_connection_unmap_buffer_ctrl_t,
                    virtio_magma_connection_unmap_buffer_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;
                let buffer = self.get_buffer(control.buffer)?;

                unsafe {
                    magma_connection_unmap_buffer(connection.handle, control.hw_va, buffer.handle)
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_UNMAP_BUFFER as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_CONNECTION_PERFORM_BUFFER_OP => {
                let (control, mut response): (
                    virtio_magma_connection_perform_buffer_op_ctrl_t,
                    virtio_magma_connection_perform_buffer_op_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let connection = self.get_connection(control.connection)?;
                let buffer = self.get_buffer(control.buffer)?;

                response.result_return = unsafe {
                    magma_connection_perform_buffer_op(
                        connection.handle,
                        buffer.handle,
                        control.options,
                        control.start_offset,
                        control.length,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_CONNECTION_PERFORM_BUFFER_OP as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_INFO => {
                let (control, mut response): (
                    virtio_magma_buffer_get_info_ctrl_t,
                    virtio_magma_buffer_get_info_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                let mut buffer_info = magma_buffer_info_t { committed_byte_count: 0, size: 0 };

                let status = unsafe { magma_buffer_get_info(buffer.handle, &mut buffer_info) };

                if status == MAGMA_STATUS_OK {
                    current_task.write_object(
                        UserRef::<magma_buffer_info_t>::new(UserAddress::from(control.info_out)),
                        &buffer_info,
                    )?;
                }

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_INFO as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_CACHE_POLICY => {
                let (control, mut response): (
                    virtio_magma_buffer_set_cache_policy_ctrl_t,
                    virtio_magma_buffer_set_cache_policy_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                response.result_return = unsafe {
                    magma_buffer_set_cache_policy(
                        buffer.handle,
                        control.policy as magma_cache_policy_t,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_CACHE_POLICY as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_GET_CACHE_POLICY => {
                let (control, mut response): (
                    virtio_magma_buffer_get_cache_policy_ctrl_t,
                    virtio_magma_buffer_get_cache_policy_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                let mut policy: magma_cache_policy_t = MAGMA_CACHE_POLICY_CACHED;

                let status = unsafe { magma_buffer_get_cache_policy(buffer.handle, &mut policy) };

                if status == MAGMA_STATUS_OK {
                    response.cache_policy_out = policy as u64;
                }
                response.result_return = status as u64;
                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_GET_CACHE_POLICY as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_CLEAN_CACHE => {
                let (control, mut response): (
                    virtio_magma_buffer_clean_cache_ctrl_t,
                    virtio_magma_buffer_clean_cache_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                response.result_return = unsafe {
                    magma_buffer_clean_cache(
                        buffer.handle,
                        control.offset,
                        control.size,
                        control.operation as magma_cache_operation_t,
                    ) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_CLEAN_CACHE as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            virtio_magma_ctrl_type_VIRTIO_MAGMA_CMD_BUFFER_SET_NAME => {
                let (control, mut response): (
                    virtio_magma_buffer_set_name_ctrl_t,
                    virtio_magma_buffer_set_name_resp_t,
                ) = read_control_and_response(current_task, &command)?;
                let buffer = self.get_buffer(control.buffer)?;

                let wrapper_ref = UserRef::<virtmagma_buffer_set_name_wrapper>::new(
                    UserAddress::from(control.name),
                );
                let wrapper = current_task.read_object(wrapper_ref)?;

                let name = current_task.read_buffer(&UserBuffer {
                    address: UserAddress::from(wrapper.name_address),
                    length: wrapper.name_size as usize, // name_size includes null terminate byte
                })?;

                response.result_return = unsafe {
                    let name_ptr = &name[0] as *const u8 as *const std::os::raw::c_char;
                    magma_buffer_set_name(buffer.handle, name_ptr) as u64
                };

                response.hdr.type_ =
                    virtio_magma_ctrl_type_VIRTIO_MAGMA_RESP_BUFFER_SET_NAME as u32;

                current_task.write_object(UserRef::new(response_address), &response)
            }
            t => {
                track_stub!(TODO("https://fxbug.dev/322874166"), "virtio magma ioctl", t);
                error!(ENOSYS)
            }
        }?;

        Ok(SUCCESS)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, ReadOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EINVAL)
    }
}
