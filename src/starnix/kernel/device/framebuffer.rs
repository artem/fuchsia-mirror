// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::framebuffer_server::{init_viewport_scene, start_presentation_loop, FramebufferServer};
use crate::{
    device::{kobject::DeviceMetadata, DeviceMode, DeviceOps},
    fs::sysfs::DeviceDirectory,
    mm::MemoryAccessorExt,
    task::{CurrentTask, Kernel},
    vfs::{fileops_impl_vmo, FileObject, FileOps, FsNode},
};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_display_singleton as fuidisplay;
use fidl_fuchsia_ui_views as fuiviews;
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use starnix_logging::{impossible_error, log_info, log_warn};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, RwLock, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    fb_bitfield, fb_fix_screeninfo, fb_var_screeninfo,
    open_flags::OpenFlags,
    user_address::{UserAddress, UserRef},
    FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO, FBIOPUT_VSCREENINFO, FB_TYPE_PACKED_PIXELS,
    FB_VISUAL_TRUECOLOR,
};
use std::sync::Arc;
use zerocopy::AsBytes;

#[derive(Default, Debug)]
pub struct AspectRatio {
    pub width: u32,
    pub height: u32,
}

pub struct Framebuffer {
    vmo: Arc<zx::Vmo>,
    vmo_len: u32,
    pub info: RwLock<fb_var_screeninfo>,
    server: Option<Arc<FramebufferServer>>,
    pub view_identity: Mutex<Option<fuiviews::ViewIdentityOnCreation>>,
    pub view_bound_protocols: Mutex<Option<fuicomposition::ViewBoundProtocols>>,
}

impl Framebuffer {
    /// Creates a new `Framebuffer` fit to the screen, while maintaining the provided aspect ratio.
    ///
    /// If the `aspect_ratio` is `None`, the framebuffer will be scaled to the display.
    pub fn new(aspect_ratio: Option<&AspectRatio>) -> Result<Arc<Self>, Errno> {
        let mut info = fb_var_screeninfo::default();

        let display_size =
            Self::get_display_size().unwrap_or(fmath::SizeU { width: 700, height: 1200 });

        // If the container has a specific aspect ratio set, use that to fit the framebuffer
        // inside of the display.
        let (feature_width, feature_height) = aspect_ratio
            .map(|ar| (ar.width, ar.height))
            .unwrap_or((display_size.width, display_size.height));

        // Scale to framebuffer to fit the display, while maintaining the expected aspect ratio.
        let ratio =
            std::cmp::min(display_size.width / feature_width, display_size.height / feature_height);
        let (width, height) = (feature_width * ratio, feature_height * ratio);

        info.xres = width;
        info.yres = height;
        info.xres_virtual = info.xres;
        info.yres_virtual = info.yres;
        info.bits_per_pixel = 32;
        info.red = fb_bitfield { offset: 0, length: 8, msb_right: 0 };
        info.green = fb_bitfield { offset: 8, length: 8, msb_right: 0 };
        info.blue = fb_bitfield { offset: 16, length: 8, msb_right: 0 };
        info.transp = fb_bitfield { offset: 24, length: 8, msb_right: 0 };

        if let Ok(server) = FramebufferServer::new(width, height) {
            let server = Arc::new(server);
            let vmo = Arc::new(server.get_vmo()?);
            let vmo_len = vmo.info().map_err(|_| errno!(EINVAL))?.size_bytes as u32;
            // Fill the buffer with white pixels as a placeholder.
            if let Err(err) = vmo.write(&vec![0xff; vmo_len as usize], 0) {
                log_warn!("could not write initial framebuffer: {:?}", err);
            }

            Ok(Arc::new(Self {
                vmo,
                vmo_len,
                server: Some(server),
                info: RwLock::new(info),
                view_identity: Default::default(),
                view_bound_protocols: Default::default(),
            }))
        } else {
            let vmo_len = info.xres * info.yres * (info.bits_per_pixel / 8);
            let vmo = Arc::new(zx::Vmo::create(vmo_len as u64).map_err(|s| match s {
                zx::Status::NO_MEMORY => errno!(ENOMEM),
                _ => impossible_error(s),
            })?);
            Ok(Arc::new(Self {
                vmo,
                vmo_len,
                server: None,
                info: RwLock::new(info),
                view_identity: Default::default(),
                view_bound_protocols: Default::default(),
            }))
        }
    }

    /// Starts presenting a view based on this framebuffer.
    ///
    /// # Parameters
    /// * `incoming_dir`: the incoming service directory under which the
    ///   `fuchsia.element.GraphicalPresenter` protocol can be retrieved.
    pub fn start_server(
        &self,
        kernel: &Arc<Kernel>,
        incoming_dir: Option<fio::DirectoryProxy>,
    ) -> Result<(), anyhow::Error> {
        if let Some(server) = &self.server {
            let view_bound_protocols = self.view_bound_protocols.lock().take().unwrap();
            let view_identity = self.view_identity.lock().take().unwrap();
            log_info!("Presenting view using GraphicalPresenter");
            start_presentation_loop(
                kernel,
                server.clone(),
                view_bound_protocols,
                view_identity,
                incoming_dir,
            );
        }

        Ok(())
    }

    /// Starts presenting a child view instead of the framebuffer.
    ///
    /// # Parameters
    /// * `viewport_token`: handles to the child view
    pub fn present_view(&self, viewport_token: fuiviews::ViewportCreationToken) {
        if let Some(server) = &self.server {
            init_viewport_scene(server.clone(), viewport_token);
        }
    }

    fn get_display_size() -> Result<fmath::SizeU, Errno> {
        let singleton_display_info =
            connect_to_protocol_sync::<fuidisplay::InfoMarker>().map_err(|_| errno!(ENOENT))?;
        let metrics =
            singleton_display_info.get_metrics(zx::Time::INFINITE).map_err(|_| errno!(EINVAL))?;
        let extent_in_px =
            metrics.extent_in_px.ok_or("Failed to get extent_in_px").map_err(|_| errno!(EINVAL))?;
        Ok(extent_in_px)
    }
}

impl DeviceOps for Arc<Framebuffer> {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        dev: DeviceType,
        node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if dev.minor() != 0 {
            return error!(ENODEV);
        }
        node.update_info(|info| {
            info.size = self.vmo_len as usize;
            info.blocks = self.vmo.get_size().map_err(impossible_error)? as usize / info.blksize;
            Ok(())
        })?;
        Ok(Box::new(Arc::clone(self)))
    }
}

impl FileOps for Framebuffer {
    fileops_impl_vmo!(self, &self.vmo);

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            FBIOGET_FSCREENINFO => {
                let info = self.info.read();
                let finfo = fb_fix_screeninfo {
                    id: zerocopy::FromBytes::read_from(&b"Starnix\0\0\0\0\0\0\0\0\0"[..]).unwrap(),
                    smem_start: 0,
                    smem_len: self.vmo_len,
                    type_: FB_TYPE_PACKED_PIXELS,
                    visual: FB_VISUAL_TRUECOLOR,
                    line_length: info.bits_per_pixel / 8 * info.xres,
                    ..fb_fix_screeninfo::default()
                };
                current_task.write_object(UserRef::new(user_addr), &finfo)?;
                Ok(SUCCESS)
            }

            FBIOGET_VSCREENINFO => {
                let info = self.info.read();
                current_task.write_object(UserRef::new(user_addr), &*info)?;
                Ok(SUCCESS)
            }

            FBIOPUT_VSCREENINFO => {
                let new_info: fb_var_screeninfo =
                    current_task.read_object(UserRef::new(user_addr))?;
                let old_info = self.info.read();
                // We don't yet support actually changing anything
                if new_info.as_bytes() != old_info.as_bytes() {
                    return error!(EINVAL);
                }
                Ok(SUCCESS)
            }

            _ => {
                error!(EINVAL)
            }
        }
    }
}

pub fn fb_device_init<L>(locked: &mut Locked<'_, L>, system_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let graphics_class = registry.get_or_create_class("graphics".into(), registry.virtual_bus());
    registry.add_and_register_device(
        locked,
        system_task,
        "fb0".into(),
        DeviceMetadata::new("fb0".into(), DeviceType::FB0, DeviceMode::Char),
        graphics_class,
        DeviceDirectory::new,
        kernel.framebuffer.clone(),
    );
}
