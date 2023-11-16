// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    device::{simple_device_ops, DeviceMode},
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_seekless,
        kobject::KObjectDeviceAttribute,
        Anon, FileHandle, FileObject, FileOps, FileWriteGuardRef, FsNodeInfo, NamespaceNode,
    },
    logging::{log, log_info},
    mm::{
        create_anonymous_mapping_vmo, DesiredAddress, MappingName, MappingOptions, ProtectionFlags,
    },
    task::{CurrentTask, Kernel},
    types::{
        device_type::DeviceType,
        errno::{error, Errno},
        file_mode::FileMode,
        open_flags::OpenFlags,
        user_address::UserAddress,
    },
};
use fuchsia_zircon::{
    cprng_draw, {self as zx},
};
use std::sync::Arc;

#[derive(Default)]
pub struct DevNull;

pub fn new_null_file(current_task: &CurrentTask, flags: OpenFlags) -> FileHandle {
    Anon::new_file_extended(
        current_task,
        Box::new(DevNull),
        flags,
        FsNodeInfo::new_factory(FileMode::from_bits(0o666), FsCred::root()),
    )
}

impl FileOps for DevNull {
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // Writes to /dev/null on Linux treat the input buffer in an unconventional way. The actual
        // data is not touched and if the input parameters are plausible the device claims to
        // successfully write up to MAX_RW_COUNT bytes.  If the input parameters are outside of the
        // user accessible address space, writes will return EFAULT.

        // For debugging log up to 4096 bytes from the input buffer. We don't care about errors when
        // trying to read data to log. The amount of data logged is chosen arbitrarily.
        let bytes_to_log = std::cmp::min(4096, data.available());
        let mut log_buffer = vec![0; bytes_to_log];
        let bytes_logged = match data.read(&mut log_buffer) {
            Ok(bytes) => {
                log_info!("write to devnull: {:?}", String::from_utf8_lossy(&log_buffer[0..bytes]));
                bytes
            }
            Err(_) => 0,
        };
        Ok(bytes_logged + data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn to_handle(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        Ok(None)
    }
}

#[derive(Default)]
struct DevZero;
impl FileOps for DevZero {
    fileops_impl_seekless!();

    fn mmap(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        mut options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        // All /dev/zero mappings behave as anonymous mappings.
        //
        // This means that we always create a new zero-filled VMO for this mmap request.
        // Memory is never shared between two mappings of /dev/zero, even if
        // `MappingOptions::SHARED` is set.
        //
        // Similar to anonymous mappings, if this process were to request a shared mapping
        // of /dev/zero and then fork, the child and the parent process would share the
        // VMO created here.
        let vmo = create_anonymous_mapping_vmo(length as u64)?;

        options |= MappingOptions::ANONYMOUS;

        current_task.mm.map_vmo(
            addr,
            vmo.clone(),
            vmo_offset,
            length,
            prot_flags,
            options,
            // We set the filename here, even though we are creating what is
            // functionally equivalent to an anonymous mapping. Doing so affects
            // the output of `/proc/self/maps` and identifies this mapping as
            // file-based.
            MappingName::File(filename),
            FileWriteGuardRef(None),
        )
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.zero()
    }
}

#[derive(Default)]
struct DevFull;
impl FileOps for DevFull {
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSPC)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            bytes.fill(0);
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
pub struct DevRandom;
impl FileOps for DevRandom {
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            cprng_draw(bytes);
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
struct DevKmsg;
impl FileOps for DevKmsg {
    fileops_impl_seekless!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        log!(
            level = info,
            tag = "kmsg",
            "{}",
            String::from_utf8_lossy(&bytes).trim_end_matches('\n')
        );
        Ok(bytes.len())
    }
}

pub fn mem_device_init(kernel: &Arc<Kernel>) {
    let mem_class = kernel.device_registry.add_class(b"mem", kernel.device_registry.virtual_bus());
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class.clone(),
            b"null",
            b"null",
            DeviceType::NULL,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevNull>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class.clone(),
            b"zero",
            b"zero",
            DeviceType::ZERO,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevZero>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class.clone(),
            b"full",
            b"full",
            DeviceType::FULL,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevFull>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class.clone(),
            b"random",
            b"random",
            DeviceType::RANDOM,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevRandom>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class.clone(),
            b"urandom",
            b"urandom",
            DeviceType::URANDOM,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevRandom>,
    );
    kernel.add_and_register_device(
        KObjectDeviceAttribute::new(
            mem_class,
            b"kmsg",
            b"kmsg",
            DeviceType::KMSG,
            DeviceMode::Char,
        ),
        simple_device_ops::<DevKmsg>,
    );
}
