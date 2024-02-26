// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Starts the 'aggressive' stressor.
//! This variant performs open-read-close, open-append-close, and unlink-open-append-close
//! operations. Note that 'truncate' is not exercised.
//! This version also silently ignores errors such as out of space.

use {
    fidl_fuchsia_io as fio,
    fuchsia_async::Time,
    rand::{distributions::WeightedIndex, prelude::*},
    std::{
        fs::File,
        io::ErrorKind,
        os::unix::fs::FileExt,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex,
        },
    },
};

struct Inner {
    /// Used to round-robin across NUM_FILES in order.
    file_counter: usize,
    /// Maps from file_counter to file index. This allows us to shuffle the tail
    /// as we round-robin without touching the files that are in the dirent cache.
    file_map: [u64; NUM_FILES],
}

impl Inner {
    /// Returns the next file to use, avoiding the most recent DIRENT_CACHE_LIMIT
    /// files returned but shuffling the remainder.
    pub fn next_file_num(&mut self) -> u64 {
        let mut rng = rand::thread_rng();
        self.file_map.swap(
            self.file_counter,
            (self.file_counter + rng.gen_range(0..(NUM_FILES - DIRENT_CACHE_LIMIT))) % NUM_FILES,
        );
        let ret = self.file_map[self.file_counter];
        self.file_counter = (self.file_counter + 1) % NUM_FILES;
        ret
    }
}

/// Designed to 'exercise' the filesystem and thrash the dirent cache.
/// This cyclically works its way through 10000 files (the dirent cache holds 8000).
/// On each iteration it will either read, write or rewrite a file.
/// Unaligned random reads and append-only writes up to 128kB each in length.
/// Write errors causes the file to be truncated to zero bytes.
pub struct Stressor {
    /// Inner state tracking which files can be accessed next.
    inner: Mutex<Inner>,
    /// Tracks the number of bytes we have allocated.
    bytes_stored: AtomicU64,
    /// The target number of bytes we want the filesystem to consume in steady-state.
    target_bytes: u64,
    op_stats: Mutex<[u64; NUM_OPS]>,
    /// A handle to the directory used to query filesystem info periodically.
    dir: fio::DirectorySynchronousProxy,
}

#[derive(Eq, PartialEq)]
struct FileState {
    path: String,
}

const READ: usize = 0;
const WRITE: usize = 1;
const NUM_OPS: usize = 2;

/// The number of files to cycle through.
/// This must be larger than DIRENT_CACHE_LIMIT to induce cache thrashing.
const NUM_FILES: usize = 10000;

/// See src/storage/fxfs/platform/src/fuchsia/volume.rs.
const DIRENT_CACHE_LIMIT: usize = 8000;

// We favour writes because half of those writes will end up being truncates.
// The expected long-term mix will therefore be around 1:1 reads/writes.
const WEIGHTS: [f64; NUM_OPS] = [/* READ: */ 1.0, /* WRITE: */ 2.0];

impl Stressor {
    /// Creates a new aggressive stressor that will try to fill the disk until
    /// `target_free_bytes` of free space remains.
    pub fn new(target_free_bytes: u64) -> Arc<Self> {
        let dir = fio::DirectorySynchronousProxy::new(
            fuchsia_async::Channel::from_channel(
                fdio::transfer_fd(std::fs::File::open("/data").unwrap()).unwrap().into(),
            )
            .into(),
        );
        let info = dir.query_filesystem(Time::INFINITE.into()).unwrap().1.unwrap();

        tracing::info!(
            "Aggressive stressor mode targetting {} free bytes on a {} byte volume.",
            target_free_bytes,
            info.total_bytes
        );

        Arc::new(Stressor {
            inner: Mutex::new(Inner {
                file_counter: 0,
                file_map: std::array::from_fn(|i| i as u64),
            }),
            bytes_stored: AtomicU64::new(info.used_bytes),
            target_bytes: info.total_bytes - target_free_bytes,
            op_stats: Default::default(),
            dir,
        })
    }

    /// Starts the stressor. Loops forever.
    pub fn run(self: &Arc<Self>, num_threads: usize) {
        for _ in 0..num_threads {
            let this = self.clone();
            std::thread::spawn(move || this.worker());
        }
        // Sleep forever, periodically updating bytes stored.
        // This update is racy but close enough for our purposes.
        loop {
            let info = self.dir.query_filesystem(Time::INFINITE.into()).unwrap().1.unwrap();
            std::thread::sleep(std::time::Duration::from_secs(10));
            tracing::info!(
                "bytes_stored: {}, target_bytes {}, counts: {:?}, info: {info:?}",
                self.bytes_stored.load(Ordering::Relaxed),
                self.target_bytes,
                self.op_stats.lock().unwrap()
            );
            self.bytes_stored.store(info.used_bytes, Ordering::SeqCst);
        }
    }

    /// Worker thread function.
    fn worker(&self) {
        let mut rng = rand::thread_rng();
        let mut buf = Vec::new();
        loop {
            let op = WeightedIndex::new(WEIGHTS).unwrap().sample(&mut rng);
            let file_num = self.inner.lock().unwrap().next_file_num();
            let path = format!("/data/{}", file_num);
            match op {
                READ => match File::options().read(true).write(false).open(&path) {
                    Ok(f) => {
                        let file_len = f.metadata().unwrap().len();
                        let read_len = rng.gen_range(0..128 * 1024u64);
                        let end = file_len.saturating_sub(read_len);
                        let offset = rng.gen_range(0..end + 1);
                        buf.resize(read_len as usize, 0);
                        f.read_at(&mut buf, offset).unwrap();
                    }
                    Err(e) => match e.kind() {
                        ErrorKind::NotFound => {}
                        error => {
                            tracing::warn!("Got an open error on READ: {error:?}");
                        }
                    },
                },
                WRITE => {
                    match File::options().create(true).read(true).write(true).open(&path) {
                        Ok(f) => {
                            let file_len = f.metadata().unwrap().len();
                            if self.bytes_stored.load(Ordering::Relaxed) >= self.target_bytes {
                                self.bytes_stored.fetch_sub(file_len, Ordering::Relaxed);
                                let _ = f.set_len(0).unwrap();
                            } else {
                                buf.resize(rng.gen_range(0..128 * 1024), 1);
                                match f.write_at(&buf, file_len) {
                                    Ok(bytes) => {
                                        self.bytes_stored
                                            .fetch_add(bytes as u64, Ordering::Relaxed);
                                    }
                                    Err(_) => {
                                        // When a write fails due to space, we truncate.
                                        // In this way we fill up the disk then start fragmenting it.
                                        // Until #![feature(io_error_more)] is stable, we have
                                        // a catch-all here.
                                        let _ = f.set_len(0).unwrap();
                                        // Metadata (layer files and such) can take up megabytes of
                                        // space that is not tracked in `self.bytes_stored`.
                                        // If we hit this code path then we've blown through
                                        // available space but not hit our 'bytes_stored' limit.
                                        //
                                        // Writes are relatively small compared to the potential
                                        // layer file size so even though it's a bit racy, we will
                                        // accept the potential inaccuracy of concurrent writes
                                        // and adjust bytes_stored here based on the filesystem's
                                        // internal tally.
                                        let info = self
                                            .dir
                                            .query_filesystem(Time::INFINITE.into())
                                            .unwrap()
                                            .1
                                            .unwrap();
                                        tracing::info!("Correcting bytes_stored. {info:?}");
                                        self.bytes_stored.store(info.used_bytes, Ordering::SeqCst);
                                    }
                                };
                            }
                        }
                        // Unfortunately ErrorKind::StorageFull is unstable so
                        // we rely on a catch-all here and assume write errors are
                        // due to disk being full. This happens often so we don't log it.
                        Err(_) => {}
                    }
                }
                _ => unreachable!(),
            }
            self.op_stats.lock().unwrap()[op] += 1;
        }
    }
}
