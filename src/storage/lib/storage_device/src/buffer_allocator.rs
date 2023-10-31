// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::buffer::{round_down, round_up, Buffer},
    std::collections::BTreeMap,
    std::ops::Range,
    std::sync::Mutex,
    std::vec::Vec,
};

#[cfg(target_os = "fuchsia")]
mod buffer_source {
    use {
        fuchsia_runtime::vmar_root_self,
        fuchsia_zircon::{self as zx, AsHandleRef},
        std::{ffi::CString, ops::Range},
    };

    /// A buffer source backed by a VMO.
    #[derive(Debug)]
    pub struct BufferSource {
        base: *mut u8,
        size: usize,
        vmo: zx::Vmo,
    }

    // SAFETY: This is required for the *mut u8 which is just the base address of the VMO mapping
    // and doesn't stop us making BufferSource Send and Sync.
    unsafe impl Send for BufferSource {}
    unsafe impl Sync for BufferSource {}

    impl BufferSource {
        pub fn new(size: usize) -> Self {
            let vmo = zx::Vmo::create(size as u64).unwrap();
            let cname = CString::new("transfer-buf").unwrap();
            vmo.set_name(&cname).unwrap();
            let flags = zx::VmarFlags::PERM_READ
                | zx::VmarFlags::PERM_WRITE
                | zx::VmarFlags::MAP_RANGE
                | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
            let base = vmar_root_self().map(0, &vmo, 0, size, flags).unwrap() as *mut u8;
            Self { base, size, vmo }
        }

        pub fn size(&self) -> usize {
            self.size
        }

        pub fn vmo(&self) -> &zx::Vmo {
            &self.vmo
        }

        #[allow(clippy::mut_from_ref)]
        pub(super) unsafe fn sub_slice(&self, range: &Range<usize>) -> &mut [u8] {
            assert!(range.start < self.size && range.end <= self.size);
            std::slice::from_raw_parts_mut(self.base.add(range.start), range.end - range.start)
        }

        /// Commits the range in memory to avoid future page faults.
        pub fn commit_range(&self, range: Range<usize>) -> Result<(), zx::Status> {
            self.vmo.op_range(zx::VmoOp::COMMIT, range.start as u64, range.len() as u64)
        }
    }

    impl Drop for BufferSource {
        fn drop(&mut self) {
            // SAFETY: This balances the `map` in `new` above.
            unsafe {
                let _ = vmar_root_self().unmap(self.base as usize, self.size);
            }
        }
    }
}

#[cfg(not(target_os = "fuchsia"))]
mod buffer_source {
    use std::{cell::UnsafeCell, ops::Range, pin::Pin};

    /// A basic heap-backed buffer source.
    #[derive(Debug)]
    pub struct BufferSource {
        // We use an UnsafeCell here because we need interior mutability of the buffer (to hand out
        // mutable slices to it in |buffer()|), but don't want to pay the cost of wrapping the
        // buffer in a Mutex. We must guarantee that the Buffer objects we hand out don't overlap,
        // but that is already a requirement for correctness.
        data: UnsafeCell<Pin<Vec<u8>>>,
    }

    // Safe because none of the fields in BufferSource are modified, except the contents of |data|,
    // but that is managed by the BufferAllocator.
    unsafe impl Sync for BufferSource {}

    impl BufferSource {
        pub fn new(size: usize) -> Self {
            Self { data: UnsafeCell::new(Pin::new(vec![0 as u8; size])) }
        }

        pub fn size(&self) -> usize {
            // Safe because the reference goes out of scope as soon as we use it.
            unsafe { (&*self.data.get()).len() }
        }

        #[allow(clippy::mut_from_ref)]
        pub(super) unsafe fn sub_slice(&self, range: &Range<usize>) -> &mut [u8] {
            assert!(range.start < self.size() && range.end <= self.size());
            &mut (&mut *self.data.get())[range.start..range.end]
        }
    }
}

pub use buffer_source::BufferSource;

// Stores a list of offsets into a BufferSource. The size of the free ranges is determined by which
// FreeList we are looking at.
// FreeLists are sorted.
type FreeList = Vec<usize>;

#[derive(Debug)]
struct Inner {
    // The index corresponds to the order of free memory blocks in the free list.
    free_lists: Vec<FreeList>,
    // Maps offsets to allocated length (the actual length, not the size requested by the client).
    allocation_map: BTreeMap<usize, usize>,
}

/// BufferAllocator creates Buffer objects to be used for block device I/O requests.
///
/// This is implemented through a simple buddy allocation scheme.
#[derive(Debug)]
pub struct BufferAllocator {
    block_size: usize,
    source: BufferSource,
    inner: Mutex<Inner>,
}

// Returns the smallest order which is at least |size| bytes.
fn order(size: usize, block_size: usize) -> usize {
    if size <= block_size {
        return 0;
    }
    let nblocks = round_up(size, block_size) / block_size;
    nblocks.next_power_of_two().trailing_zeros() as usize
}

// Returns the largest order which is no more than |size| bytes.
fn order_fit(size: usize, block_size: usize) -> usize {
    assert!(size >= block_size);
    let nblocks = round_up(size, block_size) / block_size;
    if nblocks.is_power_of_two() {
        nblocks.trailing_zeros() as usize
    } else {
        nblocks.next_power_of_two().trailing_zeros() as usize - 1
    }
}

fn size_for_order(order: usize, block_size: usize) -> usize {
    block_size * (1 << (order as u32))
}

fn initial_free_lists(size: usize, block_size: usize) -> Vec<FreeList> {
    let size = round_down(size, block_size);
    assert!(block_size <= size);
    assert!(block_size.is_power_of_two());
    let max_order = order_fit(size, block_size);
    let mut free_lists = Vec::new();
    for _ in 0..max_order + 1 {
        free_lists.push(FreeList::new())
    }
    let mut offset = 0;
    while offset < size {
        let order = order_fit(size - offset, block_size);
        let size = size_for_order(order, block_size);
        free_lists[order].push(offset);
        offset += size;
    }
    free_lists
}

impl BufferAllocator {
    pub fn new(block_size: usize, source: BufferSource) -> Self {
        let free_lists = initial_free_lists(source.size(), block_size);
        Self {
            block_size,
            source,
            inner: Mutex::new(Inner { free_lists, allocation_map: BTreeMap::new() }),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn buffer_source(&self) -> &BufferSource {
        &self.source
    }

    /// Takes the buffer source from the allocator and consumes the allocator.
    pub fn take_buffer_source(self) -> BufferSource {
        self.source
    }

    /// Allocates a Buffer with capacity for |size| bytes. Panics if the allocation cannot be
    /// satisfied.
    ///
    /// The allocated buffer will be block-aligned and the padding up to block alignment can also
    /// be used by the buffer.
    ///
    /// Allocation is O(lg(N) + M), where N = size and M = number of allocations.
    pub fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        // TODO(jfsulliv): Wait until a buffer is free, rather than asserting.
        self.try_allocate_buffer(size)
            .unwrap_or_else(|| panic!("Unable to allocate {} bytes", size))
    }

    /// Like |allocate_buffer|, but returns None if the allocation cannot be satisfied.
    pub fn try_allocate_buffer(&self, size: usize) -> Option<Buffer<'_>> {
        if size > self.source.size() {
            panic!("Allocation of {} bytes would exceed limit {}", size, self.source.size());
        }

        let mut inner = self.inner.lock().unwrap();
        let requested_order = order(size, self.block_size());
        assert!(requested_order < inner.free_lists.len());
        // Pick the smallest possible order with a free entry.
        let mut order = {
            let mut idx = requested_order;
            loop {
                if idx >= inner.free_lists.len() {
                    return None;
                }
                if !inner.free_lists[idx].is_empty() {
                    break idx;
                }
                idx += 1;
            }
        };

        // Split the free region until it's the right size.
        let offset = inner.free_lists[order].pop().unwrap();
        while order > requested_order {
            order -= 1;
            assert!(inner.free_lists[order].is_empty());
            inner.free_lists[order].push(offset + self.size_for_order(order));
        }

        inner.allocation_map.insert(offset, self.size_for_order(order));
        let range = offset..offset + size;
        tracing::debug!(?range, bytes_used = self.size_for_order(order), "Allocated");

        // Safety is ensured by the allocator not double-allocating any regions.
        Some(Buffer::new(unsafe { self.source.sub_slice(&range) }, range, &self))
    }

    /// Deallocation is O(lg(N) + M), where N = size and M = number of allocations.
    #[doc(hidden)]
    pub(super) fn free_buffer(&self, range: Range<usize>) {
        let mut inner = self.inner.lock().unwrap();
        let mut offset = range.start;
        let size = inner
            .allocation_map
            .remove(&offset)
            .unwrap_or_else(|| panic!("No allocation record found for {:?}", range));
        assert!(range.end - range.start <= size);
        tracing::debug!(?range, bytes_used = size, "Freeing");

        // Merge as many free slots as we can.
        let mut order = order(size, self.block_size());
        while order < inner.free_lists.len() - 1 {
            let buddy = self.find_buddy(offset, order);
            let idx = if let Ok(idx) = inner.free_lists[order].binary_search(&buddy) {
                idx
            } else {
                break;
            };
            inner.free_lists[order].remove(idx);
            offset = std::cmp::min(offset, buddy);
            order += 1;
        }

        let idx = match inner.free_lists[order].binary_search(&offset) {
            Ok(_) => panic!("Unexpectedly found {} in free list {}", offset, order),
            Err(idx) => idx,
        };

        inner.free_lists[order].insert(idx, offset);
    }

    fn size_for_order(&self, order: usize) -> usize {
        size_for_order(order, self.block_size)
    }

    fn find_buddy(&self, offset: usize, order: usize) -> usize {
        offset ^ self.size_for_order(order)
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            buffer::Buffer,
            buffer_allocator::{order, BufferAllocator, BufferSource},
        },
        fuchsia_async as fasync,
        futures::future::join_all,
        rand::{prelude::SliceRandom, thread_rng, Rng},
        std::sync::Arc,
    };

    #[fuchsia::test]
    async fn test_odd_sized_buffer_source() {
        let source = BufferSource::new(123);
        let allocator = BufferAllocator::new(2, source);

        // 123 == 64 + 32 + 16 + 8 + 2 + 1. (The last byte is unusable.)
        let sizes = vec![64, 32, 16, 8, 2];
        let bufs: Vec<Buffer<'_>> =
            sizes.iter().map(|size| allocator.allocate_buffer(*size)).collect();
        for (expected_size, buf) in sizes.iter().zip(bufs.iter()) {
            assert_eq!(*expected_size, buf.len());
        }
        assert!(allocator.try_allocate_buffer(2).is_none());
    }

    #[fuchsia::test]
    async fn test_allocate_buffer_read_write() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        let mut buf = allocator.allocate_buffer(8192);
        buf.as_mut_slice().fill(0xaa as u8);
        let mut vec = vec![0 as u8; 8192];
        vec.copy_from_slice(buf.as_slice());
        assert_eq!(vec, vec![0xaa as u8; 8192]);
    }

    #[fuchsia::test]
    async fn test_allocate_buffer_consecutive_calls_do_not_overlap() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        let buf1 = allocator.allocate_buffer(8192);
        let buf2 = allocator.allocate_buffer(8192);
        assert!(buf1.range().end <= buf2.range().start || buf2.range().end <= buf1.range().start);
    }

    #[fuchsia::test]
    async fn test_allocate_many_buffers() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        for _ in 0..10 {
            let _ = allocator.allocate_buffer(8192);
        }
    }

    #[fuchsia::test]
    async fn test_allocate_small_buffers_dont_overlap() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        let buf1 = allocator.allocate_buffer(1);
        let buf2 = allocator.allocate_buffer(1);
        assert!(buf1.range().end <= buf2.range().start || buf2.range().end <= buf1.range().start);
    }

    #[fuchsia::test]
    async fn test_allocate_large_buffer() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        let mut buf = allocator.allocate_buffer(1024 * 1024);
        assert_eq!(buf.len(), 1024 * 1024);
        buf.as_mut_slice().fill(0xaa as u8);
        let mut vec = vec![0 as u8; 1024 * 1024];
        vec.copy_from_slice(buf.as_slice());
        assert_eq!(vec, vec![0xaa as u8; 1024 * 1024]);
    }

    #[fuchsia::test]
    async fn test_allocate_large_buffer_after_smaller_buffers() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        {
            let mut buffers = vec![];
            while let Some(buffer) = allocator.try_allocate_buffer(8192) {
                buffers.push(buffer);
            }
        }
        let buf = allocator.allocate_buffer(1024 * 1024);
        assert_eq!(buf.len(), 1024 * 1024);
    }

    #[fuchsia::test]
    async fn test_allocate_at_limits() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(8192, source);

        let mut buffers = vec![];
        while let Some(buffer) = allocator.try_allocate_buffer(8192) {
            buffers.push(buffer);
        }
        // Deallocate a single buffer, and reallocate a single one back.
        buffers.pop();
        let buf = allocator.allocate_buffer(8192);
        assert_eq!(buf.len(), 8192);
    }

    #[fuchsia::test(threads = 10)]
    async fn test_random_allocs_deallocs() {
        let source = BufferSource::new(16 * 1024 * 1024);
        let bs = 512;
        let allocator = Arc::new(BufferAllocator::new(bs, source));

        join_all((0..10).map(|_| {
            let allocator = allocator.clone();
            fasync::Task::spawn(async move {
                let mut rng = thread_rng();
                enum Op {
                    Alloc,
                    Dealloc,
                }
                let ops = vec![Op::Alloc, Op::Dealloc];
                let mut buffers = vec![];
                for _ in 0..1000 {
                    match ops.choose(&mut rng).unwrap() {
                        Op::Alloc => {
                            // Rather than a uniform distribution 1..64K, first pick an order and
                            // then pick a size within that. For example, we might pick order 3,
                            // which would give us 8 * 512..16 * 512 as our possible range.
                            // This way we don't bias towards larger allocations too much.
                            let order: usize = rng.gen_range(order(1, bs)..order(65536 + 1, bs));
                            let size: usize = rng.gen_range(
                                bs * 2_usize.pow(order as u32)..bs * 2_usize.pow(order as u32 + 1),
                            );
                            if let Some(mut buf) = allocator.try_allocate_buffer(size) {
                                let val = rng.gen::<u8>();
                                buf.as_mut_slice().fill(val);
                                for v in buf.as_slice() {
                                    assert_eq!(v, &val);
                                }
                                buffers.push(buf);
                            }
                        }
                        Op::Dealloc if !buffers.is_empty() => {
                            let idx = rng.gen_range(0..buffers.len());
                            buffers.remove(idx);
                        }
                        _ => {}
                    };
                }
            })
        }))
        .await;
    }

    #[fuchsia::test]
    async fn test_buffer_refs() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(512, source);

        // Allocate one buffer first so that |buf| is not starting at offset 0. This helps catch
        // bugs.
        let _buf = allocator.allocate_buffer(512);
        let mut buf = allocator.allocate_buffer(4096);
        let base = buf.range().start;
        {
            let mut bref = buf.subslice_mut(1000..2000);
            assert_eq!(bref.len(), 1000);
            assert_eq!(bref.range(), base + 1000..base + 2000);
            bref.as_mut_slice().fill(0xbb);
            {
                let mut bref2 = bref.reborrow().subslice_mut(0..100);
                assert_eq!(bref2.len(), 100);
                assert_eq!(bref2.range(), base + 1000..base + 1100);
                bref2.as_mut_slice().fill(0xaa);
            }
            {
                let mut bref2 = bref.reborrow().subslice_mut(900..1000);
                assert_eq!(bref2.len(), 100);
                assert_eq!(bref2.range(), base + 1900..base + 2000);
                bref2.as_mut_slice().fill(0xcc);
            }
            assert_eq!(bref.as_slice()[..100], vec![0xaa; 100]);
            assert_eq!(bref.as_slice()[100..900], vec![0xbb; 800]);

            let bref = bref.subslice_mut(900..);
            assert_eq!(bref.len(), 100);
            assert_eq!(bref.as_slice(), vec![0xcc; 100]);
        }
        {
            let bref = buf.as_ref();
            assert_eq!(bref.len(), 4096);
            assert_eq!(bref.range(), base..base + 4096);
            assert_eq!(bref.as_slice()[0..1000], vec![0x00; 1000]);
            {
                let bref2 = bref.subslice(1000..2000);
                assert_eq!(bref2.len(), 1000);
                assert_eq!(bref2.range(), base + 1000..base + 2000);
                assert_eq!(bref2.as_slice()[..100], vec![0xaa; 100]);
                assert_eq!(bref2.as_slice()[100..900], vec![0xbb; 800]);
                assert_eq!(bref2.as_slice()[900..1000], vec![0xcc; 100]);
            }

            let bref = bref.subslice(2048..);
            assert_eq!(bref.len(), 2048);
            assert_eq!(bref.as_slice(), vec![0x00; 2048]);
        }
    }

    #[fuchsia::test]
    async fn test_buffer_split() {
        let source = BufferSource::new(1024 * 1024);
        let allocator = BufferAllocator::new(512, source);

        // Allocate one buffer first so that |buf| is not starting at offset 0. This helps catch
        // bugs.
        let _buf = allocator.allocate_buffer(512);
        let mut buf = allocator.allocate_buffer(4096);
        let base = buf.range().start;
        {
            let bref = buf.as_mut();
            let (mut s1, mut s2) = bref.split_at_mut(2048);
            assert_eq!(s1.len(), 2048);
            assert_eq!(s1.range(), base..base + 2048);
            s1.as_mut_slice().fill(0xaa);
            assert_eq!(s2.len(), 2048);
            assert_eq!(s2.range(), base + 2048..base + 4096);
            s2.as_mut_slice().fill(0xbb);
        }
        {
            let bref = buf.as_ref();
            let (s1, s2) = bref.split_at(1);
            let (s2, s3) = s2.split_at(2047);
            let (s3, s4) = s3.split_at(0);
            assert_eq!(s1.len(), 1);
            assert_eq!(s1.range(), base..base + 1);
            assert_eq!(s2.len(), 2047);
            assert_eq!(s2.range(), base + 1..base + 2048);
            assert_eq!(s3.len(), 0);
            assert_eq!(s3.range(), base + 2048..base + 2048);
            assert_eq!(s4.len(), 2048);
            assert_eq!(s4.range(), base + 2048..base + 4096);
            assert_eq!(s1.as_slice(), vec![0xaa; 1]);
            assert_eq!(s2.as_slice(), vec![0xaa; 2047]);
            assert_eq!(s3.as_slice(), vec![]);
            assert_eq!(s4.as_slice(), vec![0xbb; 2048]);
        }
    }
}
