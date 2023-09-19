// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    arch::vdso::{calculate_ticks_offset, get_sigreturn_offset, HAS_VDSO},
    mm::PAGE_SIZE,
    time::utc::utc_write_vvar_data_transform_to,
    types::{errno, from_status_like_fdio, uapi, Errno},
};
use fidl::AsHandleRef;
use fuchsia_zircon::{self as zx, ClockTransformation, HandleBased};
use once_cell::sync::Lazy;
use std::{
    mem::size_of,
    sync::{atomic::Ordering, Arc},
};

static VVAR_SIZE: Lazy<usize> = Lazy::new(|| *PAGE_SIZE as usize);

#[derive(Default)]
pub struct VvarInitialValues {
    pub raw_ticks_to_ticks_offset: i64,
    pub ticks_to_mono_numerator: u32,
    pub ticks_to_mono_denominator: u32,
}

#[derive(Default)]
pub struct MemoryMappedVvar {
    map_addr: usize,
}

impl MemoryMappedVvar {
    /// Maps the vvar vmo to a region of the Starnix kernel root VMAR and stores the address of
    /// the mapping in this object.
    /// Initialises the mapped region with data by writing an initial set of vvar data
    pub fn new(
        vmo: &zx::Vmo,
        vvar_initial_values: VvarInitialValues,
    ) -> Result<MemoryMappedVvar, zx::Status> {
        let vvar_data_size = size_of::<uapi::vvar_data>();
        // Check that the vvar_data struct isn't larger than the size of the memory mapped vvar
        debug_assert!(vvar_data_size <= *VVAR_SIZE);
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::ALLOW_FAULTS
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE
            | zx::VmarFlags::PERM_WRITE;
        let map_addr = fuchsia_runtime::vmar_root_self().map(0, &vmo, 0, *VVAR_SIZE, flags)?;
        let memory_mapped_vvar = MemoryMappedVvar { map_addr };
        let vvar_data = memory_mapped_vvar.get_pointer_to_memory_mapped_vvar();
        vvar_data
            .raw_ticks_to_ticks_offset
            .store(vvar_initial_values.raw_ticks_to_ticks_offset, Ordering::Release);
        vvar_data
            .ticks_to_mono_numerator
            .store(vvar_initial_values.ticks_to_mono_numerator, Ordering::Release);
        vvar_data
            .ticks_to_mono_denominator
            .store(vvar_initial_values.ticks_to_mono_denominator, Ordering::Release);

        vvar_data.seq_num.store(0, Ordering::Release);
        Ok(memory_mapped_vvar)
    }

    fn get_pointer_to_memory_mapped_vvar(&self) -> &uapi::vvar_data {
        let vvar_data = unsafe {
            // SAFETY: It is checked in the assertion in MemporyMappedVvar's constructor that the
            // size of the memory region map_addr points to is larger than the size of
            // uapi::vvar_data.
            &*(self.map_addr as *const uapi::vvar_data)
        };
        vvar_data
    }

    pub fn update_utc_data_transform(&self, new_transform: &ClockTransformation) {
        let vvar_data = self.get_pointer_to_memory_mapped_vvar();
        let old_transform = ClockTransformation {
            reference_offset: vvar_data.mono_to_utc_reference_offset.load(Ordering::Acquire),
            synthetic_offset: vvar_data.mono_to_utc_synthetic_offset.load(Ordering::Acquire),
            rate: zx::sys::zx_clock_rate_t {
                synthetic_ticks: vvar_data.mono_to_utc_synthetic_ticks.load(Ordering::Acquire),
                reference_ticks: vvar_data.mono_to_utc_reference_ticks.load(Ordering::Acquire),
            },
        };
        if old_transform != *new_transform {
            let seq_num = vvar_data.seq_num.fetch_add(1, Ordering::Acquire);
            // Verify that no other thread is currently trying to update vvar_data
            debug_assert!(seq_num & 1 == 0);
            vvar_data
                .mono_to_utc_reference_offset
                .store(new_transform.reference_offset, Ordering::Release);
            vvar_data
                .mono_to_utc_synthetic_offset
                .store(new_transform.synthetic_offset, Ordering::Release);
            vvar_data
                .mono_to_utc_reference_ticks
                .store(new_transform.rate.reference_ticks, Ordering::Release);
            vvar_data
                .mono_to_utc_synthetic_ticks
                .store(new_transform.rate.synthetic_ticks, Ordering::Release);
            let seq_num_after = vvar_data.seq_num.swap(seq_num + 2, Ordering::Release);
            // Verify that no other thread also tried to update vvar_data during this update
            debug_assert!(seq_num_after == seq_num + 1)
        }
    }
}

impl Drop for MemoryMappedVvar {
    fn drop(&mut self) {
        // SAFETY: We owned the mapping.
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(self.map_addr, *VVAR_SIZE)
                .expect("failed to unmap MemoryMappedVvar");
        }
    }
}

#[derive(Default)]
pub struct Vdso {
    pub vmo: Option<Arc<zx::Vmo>>,
    pub sigreturn_offset: Option<u64>,
    pub vvar_writeable: Option<Arc<MemoryMappedVvar>>,
    pub vvar_readonly: Option<Arc<zx::Vmo>>,
}

impl Vdso {
    pub fn new() -> Self {
        let vdso_vmo = load_vdso_from_file().expect("Couldn't read vDSO from disk");
        let sigreturn = match vdso_vmo.as_ref() {
            Some(vdso) => get_sigreturn_offset(vdso),
            None => Ok(None),
        }
        .expect("Couldn't find signal trampoline code in vDSO");

        let (vvar_vmo_writeable, vvar_vmo_readonly) = if HAS_VDSO {
            let (writeable_vvar, readonly_vvar) = create_vvar_and_handles();
            (Some(writeable_vvar), Some(readonly_vvar))
        } else {
            (None, None)
        };

        Self {
            vmo: vdso_vmo,
            sigreturn_offset: sigreturn,
            vvar_writeable: vvar_vmo_writeable,
            vvar_readonly: vvar_vmo_readonly,
        }
    }
}

fn create_vvar_and_handles() -> (Arc<MemoryMappedVvar>, Arc<zx::Vmo>) {
    // Creating a vvar vmo which has a handle which is writeable.
    let vvar_vmo_writeable =
        Arc::new(zx::Vmo::create(*VVAR_SIZE as u64).expect("Couldn't create vvar vvmo"));
    // Map the writeable vvar_vmo to a region of Starnix kernel VMAR and write initial vvar_data
    let vvar_initial_values = get_vvar_values();
    let vvar_memory_mapped = Arc::new(
        MemoryMappedVvar::new(&vvar_vmo_writeable, vvar_initial_values)
            .expect("couldn't map vvar vmo"),
    );
    // Write initial mono to utc transform to the vvar.
    utc_write_vvar_data_transform_to(&vvar_memory_mapped);
    let vvar_writeable_rights = vvar_vmo_writeable
        .basic_info()
        .expect("Couldn't get rights of writeable vvar handle")
        .rights;
    // Create a duplicate handle to this vvar vmo which doesn't have write permission
    // This handle is used to map vvar into linux userspace
    let vvar_readable_rights = vvar_writeable_rights.difference(zx::Rights::WRITE);
    let vvar_vmo_readonly = Arc::new(
        vvar_vmo_writeable
            .as_ref()
            .duplicate_handle(vvar_readable_rights)
            .expect("couldn't duplicate vvar handle"),
    );
    (vvar_memory_mapped, vvar_vmo_readonly)
}

fn sync_open_in_namespace(
    path: &str,
    flags: fidl_fuchsia_io::OpenFlags,
) -> Result<fidl_fuchsia_io::DirectorySynchronousProxy, Errno> {
    let (client, server) = fidl::Channel::create();
    let dir_proxy = fidl_fuchsia_io::DirectorySynchronousProxy::new(client);

    let namespace = fdio::Namespace::installed().map_err(|_| errno!(EINVAL))?;
    namespace.open(path, flags, server).map_err(|_| errno!(ENOENT))?;
    Ok(dir_proxy)
}

/// Reads the vDSO file and returns the backing VMO.
pub fn load_vdso_from_file() -> Result<Option<Arc<zx::Vmo>>, Errno> {
    if !HAS_VDSO {
        return Ok(None);
    }
    const VDSO_FILENAME: &str = "libvdso.so";
    const VDSO_LOCATION: &str = "/pkg/data";

    let dir_proxy = sync_open_in_namespace(VDSO_LOCATION, fuchsia_fs::OpenFlags::RIGHT_READABLE)?;
    let vdso_vmo = syncio::directory_open_vmo(
        &dir_proxy,
        VDSO_FILENAME,
        fidl_fuchsia_io::VmoFlags::READ,
        zx::Time::INFINITE,
    )
    .map_err(|status| from_status_like_fdio!(status))?;

    Ok(Some(Arc::new(vdso_vmo)))
}

pub fn get_vvar_values() -> VvarInitialValues {
    let clock = zx::Clock::create(zx::ClockOpts::MONOTONIC | zx::ClockOpts::AUTO_START, None)
        .expect("failed to create clock");
    let details = clock.get_details().expect("Failed to get clock details");
    let ticks_offset = calculate_ticks_offset();
    VvarInitialValues {
        raw_ticks_to_ticks_offset: ticks_offset,
        ticks_to_mono_numerator: details.ticks_to_synthetic.rate.synthetic_ticks,
        ticks_to_mono_denominator: details.ticks_to_synthetic.rate.reference_ticks,
    }
}
