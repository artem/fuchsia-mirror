// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{kobject::KObjectDeviceAttribute, magma::MagmaFile, DeviceMode},
    task::CurrentTask,
    vfs::{FileOps, FsNode},
};
use starnix_uapi::{device_type::DeviceType, errors::Errno, open_flags::OpenFlags};

fn create_magma_device(
    current_task: &CurrentTask,
    id: DeviceType,
    node: &FsNode,
    flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    MagmaFile::new_file(current_task, id, node, flags)
}

pub fn magma_device_init(current_task: &CurrentTask) {
    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let starnix_class = registry.add_class(b"starnix", registry.virtual_bus());

    let magma_type: DeviceType =
        registry.register_dyn_chrdev(create_magma_device).expect("magma device register failed.");

    registry.add_device(
        current_task,
        KObjectDeviceAttribute::new(
            starnix_class,
            b"magma0",
            b"magma0",
            magma_type,
            DeviceMode::Char,
        ),
    );
}
