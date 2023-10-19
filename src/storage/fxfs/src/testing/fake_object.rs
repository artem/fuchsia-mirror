// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
        object_store::journal::JournalHandle,
    },
    anyhow::Error,
    async_trait::async_trait,
    std::{
        cmp::min,
        convert::TryInto,
        ops::Range,
        sync::{Arc, Mutex},
        vec::Vec,
    },
    storage_device::{
        buffer::{Buffer, BufferRef, MutableBufferRef},
        buffer_allocator::{BufferAllocator, BufferSource},
    },
};

pub struct FakeObject {
    buf: Mutex<Vec<u8>>,
}

impl FakeObject {
    pub fn new() -> Self {
        FakeObject { buf: Mutex::new(Vec::new()) }
    }

    fn read(&self, offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        let our_buf = self.buf.lock().unwrap();
        let to_do = min(buf.len(), our_buf.len() - offset as usize);
        buf.as_mut_slice()[0..to_do]
            .copy_from_slice(&our_buf[offset as usize..offset as usize + to_do]);
        Ok(to_do)
    }

    fn write_or_append(&self, offset: Option<u64>, buf: BufferRef<'_>) -> Result<u64, Error> {
        let mut our_buf = self.buf.lock().unwrap();
        let offset = offset.unwrap_or(our_buf.len() as u64);
        let required_len = offset as usize + buf.len();
        if our_buf.len() < required_len {
            our_buf.resize(required_len, 0);
        }
        our_buf[offset as usize..offset as usize + buf.len()].copy_from_slice(buf.as_slice());
        Ok(our_buf.len() as u64)
    }

    fn truncate(&self, size: u64) {
        self.buf.lock().unwrap().resize(size as usize, 0);
    }

    pub fn get_size(&self) -> u64 {
        self.buf.lock().unwrap().len() as u64
    }
}

pub struct FakeObjectHandle {
    object: Arc<FakeObject>,
    allocator: BufferAllocator,
}

impl FakeObjectHandle {
    pub fn new_with_block_size(object: Arc<FakeObject>, block_size: usize) -> Self {
        let allocator = BufferAllocator::new(block_size, BufferSource::new(32 * 1024 * 1024));
        Self { object, allocator }
    }
    pub fn new(object: Arc<FakeObject>) -> Self {
        Self::new_with_block_size(object, 512)
    }
}

#[async_trait]
impl ObjectHandle for FakeObjectHandle {
    fn object_id(&self) -> u64 {
        0
    }

    fn block_size(&self) -> u64 {
        self.allocator.block_size().try_into().unwrap()
    }

    fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.allocator.allocate_buffer(size)
    }
}

#[async_trait]
impl ReadObjectHandle for FakeObjectHandle {
    async fn read(&self, offset: u64, buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        self.object.read(offset, buf)
    }

    fn get_size(&self) -> u64 {
        self.object.get_size()
    }
}

#[async_trait]
impl WriteObjectHandle for FakeObjectHandle {
    async fn write_or_append(&self, offset: Option<u64>, buf: BufferRef<'_>) -> Result<u64, Error> {
        self.object.write_or_append(offset, buf)
    }

    async fn truncate(&self, size: u64) -> Result<(), Error> {
        self.object.truncate(size);
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

impl JournalHandle for FakeObjectHandle {
    fn start_offset(&self) -> Option<u64> {
        None
    }
    fn push_extent(&mut self, _device_range: Range<u64>) {
        // NOP
    }
    fn discard_extents(&mut self, _discard_offset: u64) {
        // NOP
    }
}
