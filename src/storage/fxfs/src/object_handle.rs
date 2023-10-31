// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_store::{PosixAttributes, Timestamp},
    anyhow::Error,
    async_trait::async_trait,
    storage_device::buffer::{Buffer, BufferRef, MutableBufferRef},
};

// Some places use Default and assume that zero is an invalid object ID, so this cannot be changed
// easily.
pub const INVALID_OBJECT_ID: u64 = 0;

/// A handle for a generic object.  For objects with a data payload, use the ReadObjectHandle or
/// WriteObjectHandle traits.
pub trait ObjectHandle: Send + Sync + 'static {
    /// Returns the object identifier for this object which will be unique for the store that the
    /// object is contained in, but not necessarily unique within the entire system.
    fn object_id(&self) -> u64;

    /// Returns the filesystem block size, which should be at least as big as the device block size,
    /// but not necessarily the same.
    fn block_size(&self) -> u64;

    /// Allocates a buffer for doing I/O (read and write) for the object.
    fn allocate_buffer(&self, size: usize) -> Buffer<'_>;

    /// Sets tracing for this object.
    fn set_trace(&self, _v: bool) {}
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectProperties {
    /// The number of references to this object.
    pub refs: u64,
    /// The number of bytes allocated to all extents across all attributes for this object.
    pub allocated_size: u64,
    /// The logical content size for the default data attribute of this object, i.e. the size of a
    /// file.  (Objects with no data attribute have size 0.)
    pub data_attribute_size: u64,
    /// The timestamp at which the object was created (i.e. crtime).
    pub creation_time: Timestamp,
    /// The timestamp at which the objects's data was last modified (i.e. mtime).
    pub modification_time: Timestamp,
    /// The timestamp at which the object was last read (i.e. atime).
    pub access_time: Timestamp,
    /// The timestamp at which the object's status was last modified (i.e. ctime).
    pub change_time: Timestamp,
    /// The number of sub-directories.
    pub sub_dirs: u64,
    // The POSIX attributes: mode, uid, gid, rdev
    pub posix_attributes: Option<PosixAttributes>,
}

#[async_trait]
pub trait ReadObjectHandle: ObjectHandle {
    /// Fills |buf| with up to |buf.len()| bytes read from |offset| on the underlying device.
    /// |offset| and |buf| must both be block-aligned.
    async fn read(&self, offset: u64, buf: MutableBufferRef<'_>) -> Result<usize, Error>;

    /// Returns the size of the object.
    fn get_size(&self) -> u64;
}

#[async_trait]
pub trait WriteObjectHandle: ObjectHandle {
    /// Writes |buf.len())| bytes at |offset| (or the end of the file), returning the object size
    /// after writing.
    /// The writes may be cached, in which case a later call to |flush| is necessary to persist the
    /// writes.
    async fn write_or_append(&self, offset: Option<u64>, buf: BufferRef<'_>) -> Result<u64, Error>;

    /// Truncates the object to |size| bytes.
    /// The truncate may be cached, in which case a later call to |flush| is necessary to persist
    /// the truncate.
    async fn truncate(&self, size: u64) -> Result<(), Error>;

    /// Flushes all pending data and metadata updates for the object.
    async fn flush(&self) -> Result<(), Error>;
}

#[async_trait]
/// This trait is an asynchronous streaming writer.
pub trait WriteBytes {
    fn handle(&self) -> &dyn WriteObjectHandle;

    /// Buffers writes to be written to the underlying handle. This may flush bytes immediately
    /// or when buffers are full.
    async fn write_bytes(&mut self, buf: &[u8]) -> Result<(), Error>;

    /// Called to flush to the handle.  Named to avoid confluct with the flush method above.
    async fn complete(&mut self) -> Result<(), Error>;

    /// Moves the offset forward by `amount`, which will result in zeroes in the output stream, even
    /// if no other data is appended to it.
    async fn skip(&mut self, amount: u64) -> Result<(), Error>;
}
