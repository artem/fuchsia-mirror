// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        object_handle::{ObjectHandle, ReadObjectHandle},
    },
    anyhow::{anyhow, ensure, Error},
    event_listener::{Event, EventListener},
    std::{
        ops::Deref,
        sync::{Arc, Mutex},
        vec::Vec,
    },
    storage_device::buffer::BufferFuture,
};

pub const CHUNK_SIZE: usize = 128 * 1024;

fn block_aligned_size(source: &impl ReadObjectHandle) -> usize {
    let block_size = source.block_size() as usize;
    let source_size = source.get_size() as usize;
    source_size.checked_next_multiple_of(block_size).unwrap()
}

/// A reference to a chunk of data which is currently in the cache.  The data will be held in the
/// cache as long as a reference to this chunk exists.
#[repr(transparent)]
#[derive(Clone)]
pub struct CachedChunk(Arc<Box<[u8]>>);

impl std::fmt::Debug for CachedChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("CachedChunk").field("len", &self.len()).finish()
    }
}

impl Deref for CachedChunk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &**self.0
    }
}

#[derive(Default, Debug)]
enum Chunk {
    #[default]
    Missing,
    Pending,
    Present(CachedChunk),
    // The chunk has been marked for eviction, and on the next pass it will be deallocated, unless
    // it is used before then (which returns it to Present).
    Expired(Box<[u8]>),
}

impl Chunk {
    // If the chunk is Present and has no remaining references, move it to Expired.
    // If the chunk is Expired, move it to Missing, and return its data (so the caller can
    // deallocate).
    fn maybe_purge(&mut self) -> Option<Box<[u8]>> {
        let this = std::mem::take(self);
        match this {
            Chunk::Expired(data) => Some(data),
            Chunk::Present(chunk) => {
                match Arc::try_unwrap(chunk.0) {
                    Ok(data) => *self = Chunk::Expired(data),
                    Err(chunk) => *self = Chunk::Present(CachedChunk(chunk)),
                }
                None
            }
            _ => {
                *self = this;
                None
            }
        }
    }
}

/// A wrapper handle around a `ReadObjectHandle` which provides a memory cache for its contents.
/// Contents are fetched as needed and can be evicted (for example due to memory pressure).
pub struct CachingObjectHandle<S> {
    source: S,

    // Data is stored in separately accessible chunks.  These can be independently loaded and
    // evicted.  The size of this array never changes.
    chunks: Mutex<Vec<Chunk>>,

    // Reads that are waiting on another read to load a chunk will wait on this event. There's only
    // 1 event for all chunks so a read may receive a notification for a different chunk than the
    // one it's waiting for and need to re-wait. It's rare for multiple chunks to be loading at once
    // and even rarer for a read to be waiting on a chunk to be loaded while another chunk is also
    // loading.
    event: Event,
}

// SAFETY: Only `buffer` isn't Sync. Access to `buffer` is synchronized with `chunk_states`.
unsafe impl<S> Sync for CachingObjectHandle<S> {}

impl<S: ReadObjectHandle> CachingObjectHandle<S> {
    pub fn new(source: S) -> Self {
        let block_size = source.block_size() as usize;
        assert!(CHUNK_SIZE % block_size == 0);
        let aligned_size = block_aligned_size(&source);
        let chunk_count = aligned_size.div_ceil(CHUNK_SIZE);

        let mut chunks = Vec::<Chunk>::new();
        chunks.resize_with(chunk_count, Default::default);
        Self { source, chunks: Mutex::new(chunks), event: Event::new() }
    }

    /// Returns a reference to the chunk (up to `CHUNK_SIZE` bytes) containing `offset`.  If the
    /// data is already cached, this does not require reading from `source`.
    /// `offset` must be less than the size of `source`.
    pub async fn read(&self, offset: usize) -> Result<CachedChunk, Error> {
        ensure!(offset < self.source.get_size() as usize, FxfsError::OutOfRange);
        let chunk_num = offset / CHUNK_SIZE;

        enum Action {
            Wait(EventListener),
            Load,
        }
        loop {
            let action = {
                let mut chunks = self.chunks.lock().unwrap();
                match std::mem::take(&mut chunks[chunk_num]) {
                    Chunk::Missing => {
                        chunks[chunk_num] = Chunk::Pending;
                        Action::Load
                    }
                    Chunk::Pending => {
                        chunks[chunk_num] = Chunk::Pending;
                        Action::Wait(self.event.listen())
                    }
                    Chunk::Present(cached_chunk) => {
                        chunks[chunk_num] = Chunk::Present(cached_chunk.clone());
                        return Ok(cached_chunk);
                    }
                    Chunk::Expired(data) => {
                        let cached_chunk = CachedChunk(Arc::new(data));
                        chunks[chunk_num] = Chunk::Present(cached_chunk.clone());
                        return Ok(cached_chunk);
                    }
                }
            };
            match action {
                Action::Wait(listener) => {
                    listener.await;
                }
                Action::Load => {
                    // If this future is dropped or reading fails then put the chunk back into the
                    // `Missing` state.
                    let drop_guard = scopeguard::guard((), |_| {
                        {
                            let mut chunks = self.chunks.lock().unwrap();
                            debug_assert!(matches!(chunks[chunk_num], Chunk::Pending));
                            chunks[chunk_num] = Chunk::Missing;
                        }
                        self.event.notify(usize::MAX);
                    });

                    let read_start = chunk_num * CHUNK_SIZE;
                    let len =
                        std::cmp::min(read_start + CHUNK_SIZE, self.source.get_size() as usize)
                            - read_start;
                    let aligned_len =
                        std::cmp::min(read_start + CHUNK_SIZE, block_aligned_size(&self.source))
                            - read_start;
                    let mut read_buf = self.source.allocate_buffer(aligned_len).await;
                    let amount_read =
                        self.source.read(read_start as u64, read_buf.as_mut()).await?;
                    ensure!(amount_read >= len, anyhow!(FxfsError::Internal).context("Short read"));

                    tracing::debug!("COH {}: Read {len}@{read_start}", self.source.object_id());

                    let data = Vec::from(&read_buf.as_slice()[..len]).into_boxed_slice();
                    let cached_chunk = CachedChunk(Arc::new(data));

                    {
                        let mut chunks = self.chunks.lock().unwrap();
                        debug_assert!(matches!(chunks[chunk_num], Chunk::Pending));
                        chunks[chunk_num] = Chunk::Present(cached_chunk.clone());
                    }
                    self.event.notify(usize::MAX);

                    scopeguard::ScopeGuard::into_inner(drop_guard);
                    return Ok(cached_chunk);
                }
            }
        }
    }

    /// Purges unused extents, freeing unused memory.  This follows a second-chance algorithm:
    /// unused extents will be marked for purging the first time this is called, and if they are not
    /// used again by the next time this is called, they will be deallocated.
    /// This is intended to be run regularly, e.g. on a timer.
    pub fn purge(&self) {
        // Deallocate out of the lock scope, so we don't hold up readers.
        let mut to_deallocate = vec![];
        let mut chunks = self.chunks.lock().unwrap();
        for chunk in chunks.iter_mut() {
            if let Some(data) = chunk.maybe_purge() {
                to_deallocate.push(data);
            }
        }
        tracing::debug!(
            "COH {}: Purging {} cached chunks ({} bytes)",
            self.source.object_id(),
            to_deallocate.len(),
            to_deallocate.len() * CHUNK_SIZE
        );
    }
}

impl<S: ReadObjectHandle> ObjectHandle for CachingObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        self.source.set_trace(v);
    }

    fn object_id(&self) -> u64 {
        self.source.object_id()
    }

    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.source.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.source.block_size()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{CachingObjectHandle, CHUNK_SIZE},
        crate::object_handle::{ObjectHandle, ReadObjectHandle},
        anyhow::{anyhow, ensure, Error},
        async_trait::async_trait,
        event_listener::Event,
        std::sync::{
            atomic::{AtomicBool, AtomicU8, Ordering},
            Arc,
        },
        storage_device::{
            buffer::{BufferFuture, MutableBufferRef},
            fake_device::FakeDevice,
            Device,
        },
    };

    // Fills a buffer with a pattern seeded by counter.
    fn fill_buf(buf: &mut [u8], counter: u8) {
        for (i, chunk) in buf.chunks_exact_mut(2).enumerate() {
            chunk[0] = counter;
            chunk[1] = i as u8;
        }
    }

    // Returns a buffer filled with fill_buf.
    fn make_buf(counter: u8, size: usize) -> Vec<u8> {
        let mut buf = vec![0; size];
        fill_buf(&mut buf, counter);
        buf
    }

    struct FakeSource {
        device: Arc<dyn Device>,
        size: usize,
        started: AtomicBool,
        allow_reads: AtomicBool,
        wake: Event,
        counter: AtomicU8,
    }

    impl FakeSource {
        // `device` is only used to provide allocate_buffer; reads don't go to the device.
        fn new(device: Arc<dyn Device>, size: usize) -> Self {
            FakeSource {
                started: AtomicBool::new(false),
                allow_reads: AtomicBool::new(true),
                size,
                wake: Event::new(),
                device,
                counter: AtomicU8::new(1),
            }
        }

        fn start(&self) {
            self.started.store(true, Ordering::SeqCst);
            self.wake.notify(usize::MAX);
        }

        // Toggle whether reads from source are allowed.  If an uncached read occurs while this was
        // set to false, the test panics.
        fn allow_reads(&self, allow: bool) {
            self.allow_reads.store(allow, Ordering::SeqCst);
        }

        async fn wait_for_start(&self) {
            while !self.started.load(Ordering::SeqCst) {
                let listener = self.wake.listen();
                if self.started.load(Ordering::SeqCst) {
                    break;
                }
                listener.await;
            }
        }
    }

    #[async_trait]
    impl ReadObjectHandle for FakeSource {
        async fn read(&self, _offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
            ensure!(self.allow_reads.load(Ordering::SeqCst), anyhow!("Received unexpected read"));
            let counter = self.counter.fetch_add(1, Ordering::Relaxed);
            self.wait_for_start().await;
            fill_buf(buf.as_mut_slice(), counter);
            Ok(buf.len())
        }

        fn get_size(&self) -> u64 {
            self.size as u64
        }
    }

    impl ObjectHandle for FakeSource {
        fn object_id(&self) -> u64 {
            unreachable!();
        }

        fn block_size(&self) -> u64 {
            self.device.block_size().into()
        }

        fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
            self.device.allocate_buffer(size)
        }
    }

    #[fuchsia::test]
    async fn test_read_with_missing_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let chunk = caching_object_handle.read(0).await.unwrap();
        assert_eq!(&*chunk, make_buf(1, 4096));
    }

    #[fuchsia::test]
    async fn test_read_with_present_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let expected = make_buf(1, 4096);
        let chunk = caching_object_handle.read(0).await.unwrap();
        assert_eq!(&*chunk, expected);

        // The chunk was already populated so this read receives the same value as the above read.
        let chunk = caching_object_handle.read(0).await.unwrap();
        assert_eq!(&*chunk, expected);
    }

    #[fuchsia::test]
    async fn test_read_with_pending_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 8192);
        let caching_object_handle = CachingObjectHandle::new(source);

        // The two reads should target the same chunk.
        let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0));
        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(4096));

        // The first future will transition the chunk from `Missing` to `Pending` and then wait
        // on the source.
        assert!(futures::poll!(&mut read_fut1).is_pending());
        // The second future will wait on the event.
        assert!(futures::poll!(&mut read_fut2).is_pending());
        caching_object_handle.source.start();
        // Even though the source is ready the second future can't make progress.
        assert!(futures::poll!(&mut read_fut2).is_pending());
        // The first future reads from the source, transition the chunk from `Pending` to
        // `Present`, and then notifies the event.
        let expected = make_buf(1, 8192);
        assert_eq!(&*read_fut1.await.unwrap(), expected);
        // The event has been notified and the second future can now complete.
        assert_eq!(&*read_fut2.await.unwrap(), expected);
    }

    #[fuchsia::test]
    async fn test_read_with_notification_for_other_chunk() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, CHUNK_SIZE + 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0));
        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(CHUNK_SIZE));
        let mut read_fut3 = std::pin::pin!(caching_object_handle.read(0));

        // The first and second futures will transition their chunks from `Missing` to `Pending`
        // and then wait on the source.
        assert!(futures::poll!(&mut read_fut1).is_pending());
        assert!(futures::poll!(&mut read_fut2).is_pending());
        // The third future will wait on the event.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        caching_object_handle.source.start();
        // Even though the source is ready the third future can't make progress.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        // The second future will read from the source and notify the event.
        assert_eq!(&*read_fut2.await.unwrap(), make_buf(2, 4096));
        // The event was notified but the first chunk is still `Pending` so the third future
        // resumes waiting.
        assert!(futures::poll!(&mut read_fut3).is_pending());
        // The first future will read from the source, transition the first chunk to `Present`,
        // and notify the event.
        let expected = make_buf(1, CHUNK_SIZE);
        assert_eq!(&*read_fut1.await.unwrap(), expected);
        // The first chunk is now present so the third future can complete.
        assert_eq!(&*read_fut3.await.unwrap(), expected);
    }

    #[fuchsia::test]
    async fn test_read_with_dropped_future() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 4096);
        let caching_object_handle = CachingObjectHandle::new(source);

        let mut read_fut2 = std::pin::pin!(caching_object_handle.read(0));
        {
            let mut read_fut1 = std::pin::pin!(caching_object_handle.read(0));

            // The first future will transition the chunk from `Missing` to `Pending` and then
            // wait on the source.
            assert!(futures::poll!(&mut read_fut1).is_pending());
            // The second future will wait on the event.
            assert!(futures::poll!(&mut read_fut2).is_pending());
            caching_object_handle.source.start();
            // Even though the source is ready the second future can't make progress.
            assert!(futures::poll!(&mut read_fut2).is_pending());
        }
        // The first future was dropped which transitioned the chunk from `Pending` to `Missing`
        // and notified the event. When the second future is polled it transitions the chunk
        // from `Missing` back to `Pending`, reads from the source, and then transitions the
        // chunk to `Present`.
        assert_eq!(&*read_fut2.await.unwrap(), make_buf(2, 4096));
    }

    #[fuchsia::test]
    async fn test_read_past_end_of_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = FakeSource::new(device, 300);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        caching_object_handle.read(500).await.expect_err("Read should fail");
    }

    #[fuchsia::test]
    async fn test_read_to_end_of_source() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        const SOURCE_SIZE: usize = 300;
        let source = FakeSource::new(device, SOURCE_SIZE);
        source.start();
        let caching_object_handle = CachingObjectHandle::new(source);

        let chunk = caching_object_handle.read(0).await.unwrap();
        assert_eq!(&*chunk, make_buf(1, SOURCE_SIZE));
    }

    #[fuchsia::test]
    async fn test_chunk_purging() {
        let device = Arc::new(FakeDevice::new(1024, 512));
        let source = Arc::new(FakeSource::new(device, CHUNK_SIZE + 4096));
        source.start();
        let caching_object_handle =
            CachingObjectHandle::new(source.clone() as Arc<dyn ReadObjectHandle>);

        let _chunk1 = caching_object_handle.read(0).await.unwrap();
        // Immediately drop the second chunk.
        caching_object_handle.read(CHUNK_SIZE).await.unwrap();

        source.allow_reads(false);

        // The first purge should not evict the second chunk yet, and the read should save the chunk
        // from being evicted by the next purge too.
        caching_object_handle.purge();
        caching_object_handle.read(0).await.unwrap();
        caching_object_handle.read(CHUNK_SIZE).await.unwrap();

        caching_object_handle.purge();
        caching_object_handle.read(0).await.unwrap();
        caching_object_handle.read(CHUNK_SIZE).await.unwrap();

        // Purging twice should result in evicting the second chunk.
        caching_object_handle.purge();
        caching_object_handle.purge();
        caching_object_handle.read(0).await.unwrap();
        caching_object_handle.read(CHUNK_SIZE).await.expect_err("Chunk was not purged");
    }
}
