// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::file::GrallocFile;
use crate::{
    device::{kobject::KObjectDeviceAttribute, DeviceMode, DeviceOps},
    task::CurrentTask,
    vfs::{FileOps, FsNode},
};
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_starnix_gralloc as fgralloc;
use starnix_sync::Mutex;
use starnix_uapi::{device_type::DeviceType, errors::Errno, open_flags::OpenFlags};
use std::sync::Arc;

#[derive(Clone)]
struct GrallocDevice {
    mode_setter: Arc<Mutex<fgralloc::VulkanModeSetterSynchronousProxy>>,
}

impl GrallocDevice {
    fn new(mode_setter: Arc<Mutex<fgralloc::VulkanModeSetterSynchronousProxy>>) -> GrallocDevice {
        GrallocDevice { mode_setter }
    }
}

impl DeviceOps for GrallocDevice {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        GrallocFile::new_file(self.mode_setter.clone())
    }
}

pub fn gralloc_device_init(current_task: &CurrentTask) {
    let mode_setter = current_task.kernel().connect_to_named_protocol_at_container_svc::<fgralloc::VulkanModeSetterMarker>(fgralloc::VulkanModeSetterMarker::PROTOCOL_NAME).expect("gralloc feature requires fuchsia.starnix.gralloc.VulkanModeSetter protocol in container /svc dir, and a corresponding server").into_sync_proxy();
    let mode_setter = Arc::new(Mutex::new(mode_setter));

    let kernel = current_task.kernel();
    let registry = &kernel.device_registry;

    let starnix_class = registry.add_class(b"starnix", registry.virtual_bus());

    let gralloc_type: DeviceType = registry
        .register_dyn_chrdev(GrallocDevice::new(mode_setter))
        .expect("gralloc device register failed.");

    registry.add_device(
        current_task,
        KObjectDeviceAttribute::new(
            None,
            starnix_class,
            b"virtgralloc0",
            b"virtgralloc0",
            gralloc_type,
            DeviceMode::Char,
        ),
    );
}
