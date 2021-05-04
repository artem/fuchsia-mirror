// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        device::{
            buffer::{Buffer, BufferRef, MutableBufferRef},
            buffer_allocator::{BufferAllocator, BufferSource},
            Device,
        },
        errors::FxfsError,
    },
    anyhow::{bail, ensure, Error},
    async_trait::async_trait,
    fuchsia_runtime::vmar_root_self,
    fuchsia_zircon::{self as zx, AsHandleRef},
    remote_block_device::{BlockClient, BufferSlice, MutableBufferSlice, VmoId},
    std::{any::Any, cell::UnsafeCell, ffi::CString, ops::Range},
};

#[derive(Debug)]
struct VmoBufferSource {
    vmoid: VmoId,
    // This needs to be 'static because the BufferSource trait requires 'static.
    slice: UnsafeCell<&'static mut [u8]>,
    vmo: zx::Vmo,
}

// Safe because none of the fields in VmoBufferSource are modified, except the contents of |slice|,
// but that is managed by the BufferAllocator.
unsafe impl Sync for VmoBufferSource {}

impl VmoBufferSource {
    fn new(remote: &dyn BlockClient, size: usize) -> Self {
        let vmo = zx::Vmo::create(size as u64).unwrap();
        let cname = CString::new("transfer-buf").unwrap();
        vmo.set_name(&cname).unwrap();
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::PERM_WRITE
            | zx::VmarFlags::MAP_RANGE
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
        let addr = vmar_root_self().map(0, &vmo, 0, size, flags).unwrap();
        let slice =
            unsafe { UnsafeCell::new(std::slice::from_raw_parts_mut(addr as *mut u8, size)) };
        let vmoid = remote.attach_vmo(&vmo).unwrap();
        Self { vmoid, slice, vmo }
    }

    fn vmoid(&self) -> &VmoId {
        &self.vmoid
    }

    fn take_vmoid(&self) -> VmoId {
        self.vmoid.take()
    }
}

impl BufferSource for VmoBufferSource {
    fn size(&self) -> usize {
        // Safe because the reference goes out of scope as soon as we use it.
        unsafe { (&*self.slice.get()).len() }
    }

    unsafe fn sub_slice(&self, range: &Range<usize>) -> &mut [u8] {
        assert!(range.start < self.size() && range.end <= self.size());
        std::slice::from_raw_parts_mut(
            ((&mut *self.slice.get()).as_mut_ptr() as usize + range.start) as *mut u8,
            range.end - range.start,
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// BlockDevice is an implementation of Device backed by a real block device behind a FIFO.
pub struct BlockDevice {
    allocator: BufferAllocator,
    remote: Box<dyn BlockClient>,
    read_only: bool,
}

const TRANSFER_VMO_SIZE: usize = 128 * 1024 * 1024;

impl BlockDevice {
    /// Creates a new BlockDevice over |remote|.
    pub fn new(remote: Box<dyn BlockClient>, read_only: bool) -> Self {
        // TODO(jfsulliv): Should we align buffers to the system page size as well? This will be
        // necessary for splicing pages out of the transfer buffer, but that only improves
        // performance for large data transfers, and we might simply attach separate VMOs to the
        // block device in those cases.
        let allocator = BufferAllocator::new(
            remote.block_size() as usize,
            Box::new(VmoBufferSource::new(remote.as_ref(), TRANSFER_VMO_SIZE)),
        );
        Self { allocator, remote, read_only }
    }

    fn buffer_source(&self) -> &VmoBufferSource {
        self.allocator.buffer_source().as_any().downcast_ref::<VmoBufferSource>().unwrap()
    }
}

#[async_trait]
impl Device for BlockDevice {
    fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.allocator.allocate_buffer(size)
    }

    fn block_size(&self) -> u32 {
        self.remote.block_size()
    }

    fn block_count(&self) -> u64 {
        self.remote.block_count()
    }

    async fn read(&self, offset: u64, buffer: MutableBufferRef<'_>) -> Result<(), Error> {
        if buffer.len() == 0 {
            return Ok(());
        }
        let vmoid = self.buffer_source().vmoid();
        ensure!(vmoid.is_valid(), "Device is closed");
        assert_eq!(offset % self.block_size() as u64, 0);
        assert_eq!(buffer.range().start % self.block_size() as usize, 0);
        assert_eq!((offset + buffer.len() as u64) % self.block_size() as u64, 0);
        self.remote
            .read_at(
                MutableBufferSlice::new_with_vmo_id(
                    vmoid,
                    buffer.range().start as u64,
                    buffer.len() as u64,
                ),
                offset,
            )
            .await
    }

    async fn write(&self, offset: u64, buffer: BufferRef<'_>) -> Result<(), Error> {
        if self.read_only {
            bail!(FxfsError::ReadOnlyFilesystem);
        }
        if buffer.len() == 0 {
            return Ok(());
        }
        let vmoid = self.buffer_source().vmoid();
        ensure!(vmoid.is_valid(), "Device is closed");
        assert_eq!(offset % self.block_size() as u64, 0);
        assert_eq!(buffer.range().start % self.block_size() as usize, 0);
        assert_eq!((offset + buffer.len() as u64) % self.block_size() as u64, 0);
        self.remote
            .write_at(
                BufferSlice::new_with_vmo_id(
                    vmoid,
                    buffer.range().start as u64,
                    buffer.len() as u64,
                ),
                offset,
            )
            .await
    }

    async fn close(&self) -> Result<(), Error> {
        self.remote.detach_vmo(self.buffer_source().take_vmoid()).await
    }

    async fn flush(&self) -> Result<(), Error> {
        self.remote.flush().await
    }
}

impl Drop for BlockDevice {
    fn drop(&mut self) {
        // We can't detach the VmoId because we're not async here, but we are tearing down the
        // connection to the block device so we don't really need to.
        self.buffer_source().take_vmoid().into_id();
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            device::{block_device::BlockDevice, Device},
            errors::FxfsError,
        },
        fuchsia_async as fasync,
        remote_block_device::testing::FakeBlockClient,
    };

    #[fasync::run_singlethreaded(test)]
    async fn test_lifecycle() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), false);

        {
            let _buf = device.allocate_buffer(8192);
        }

        device.close().await.expect("Close failed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_read_write_buffer() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), false);

        {
            let mut buf1 = device.allocate_buffer(8192);
            let mut buf2 = device.allocate_buffer(1024);
            buf1.as_mut_slice().fill(0xaa as u8);
            buf2.as_mut_slice().fill(0xbb as u8);
            device.write(65536, buf1.as_ref()).await.expect("Write failed");
            device.write(65536 + 8192, buf2.as_ref()).await.expect("Write failed");
        }
        {
            let mut buf = device.allocate_buffer(8192 + 1024);
            device.read(65536, buf.as_mut()).await.expect("Read failed");
            assert_eq!(buf.as_slice()[..8192], vec![0xaa as u8; 8192]);
            assert_eq!(buf.as_slice()[8192..], vec![0xbb as u8; 1024]);
        }

        device.close().await.expect("Close failed");
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_read_only() {
        let device = BlockDevice::new(Box::new(FakeBlockClient::new(1024, 1024)), true);
        let mut buf1 = device.allocate_buffer(8192);
        buf1.as_mut_slice().fill(0xaa as u8);
        assert!(FxfsError::ReadOnlyFilesystem
            .matches(&device.write(65536, buf1.as_ref()).await.expect_err("Write succeeded")));
    }
}
