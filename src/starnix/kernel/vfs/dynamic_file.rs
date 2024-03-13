// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::CurrentTask,
    vfs::{
        buffers::{InputBuffer, OutputBuffer, VecOutputBuffer},
        default_seek, fileops_impl_delegate_read_and_seek, FileObject, FileOps, FsNodeOps,
        SeekTarget, SimpleFileNode,
    },
};
use starnix_sync::{FileOpsCore, Locked, Mutex, WriteOps};
use starnix_uapi::{errno, error, errors::Errno, off_t};
use std::collections::VecDeque;

pub trait SequenceFileSource: Send + Sync + 'static {
    type Cursor: Default + Send;
    fn next(
        &self,
        cursor: Self::Cursor,
        sink: &mut DynamicFileBuf,
    ) -> Result<Option<Self::Cursor>, Errno>;
}

pub trait DynamicFileSource: Send + Sync + 'static {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno>;
}

impl<T> SequenceFileSource for T
where
    T: DynamicFileSource,
{
    type Cursor = ();
    fn next(&self, _cursor: (), sink: &mut DynamicFileBuf) -> Result<Option<()>, Errno> {
        self.generate(sink).map(|_| None)
    }
}

/// `DynamicFile` implements `FileOps` for files whose contents are generated by the kernel
/// dynamically either from a sequence (see `SequenceFileSource`) or as a single blob of data
/// (see `DynamicFileSource`). The file may be updated dynamically as it's normally expected
/// for files in `/proc`, e.g. when seeking back from the current position.
///
/// The following example shows how `DynamicFile` can be used with a `DynamicFileSource`:
/// ```
/// #[derive(Clone)]
/// pub struct SimpleFile(u32);
/// impl SimpleFile {
///     pub fn new_node(param: u32) -> impl FsNodeOps {
///         DynamicFile::new_node(Self(param))
///     }
/// }
/// impl DynamicFileSource for SimpleFile {
///     fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
///         writeln!(sink, "param: {}", self.0)?
///         Ok(())
///     }
/// }
/// ```
///
/// `SequenceFileSource` should be used to generate file contents from a sequence of objects.
/// `SequenceFileSource::next()` takes the cursor for the current position, outputs the next
/// chunk of data in the sequence, and returns the the advanced cursor value. At the start of
/// iteration, the cursor is `Default::default()`. The end of the sequence is indicated by
/// returning `None`.
///
/// The next example generates the contents from a sequence of integer values:
/// ```
/// [#derive(Clone)]
/// struct IntegersFile;
/// impl FileOps for IntegersFile {
///     type Cursor = usize;
///     fn next(&self, cursor: usize, sink: &mut DynamicFileBuf) -> Result<Option<usize>, Errno> {
///         // The cursor starts at i32::default(), which is 0.
///         writeln!(sink, "{}", cursor)?;
///         if cursor > 1000 {
///             // End of the sequence.
///             return Ok(None);
///         }
///         Ok(Some(cursor + 1))
///     }
/// }
/// ```
///
/// Writable files should wrap `DynamicFile` and delegate it `read()` and `seek()` using the
/// `fileops_impl_delegate_read_and_seek!` macro, as shown in the example below:
/// ```
/// struct WritableProcFileSource {
///   data: usize,
/// }
/// impl DynamicFileSource for WritableProcFileSource {
///     fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
///         writeln!("{}", self.data);
///         Ok(())
///     }
/// }
/// pub struct WritableProcFile {
///   dynamic_file: DynamicFile<WritableProcFileSource>,
///   ...
/// }
/// impl WritableProcFile {
///     fn new() -> Self {
///         Self { dynamic_file: DynamicFile::new(WritableProcFileSource { data: 42 }) }
///     }
/// }
/// impl FileOps for WritableProcFile {
///     fileops_impl_delegate_read_and_seek!(self, self.dynamic_file);
///
///     fn write(
///         &self,
///         _file: &FileObject,
///         _current_task: &CurrentTask,
///         _offset: usize,
///         data: &mut dyn InputBuffer,
///     ) -> Result<usize, Errno> {
///         ... Process write() ...
///     }
/// }
/// ```
///
pub struct DynamicFile<Source: SequenceFileSource> {
    state: Mutex<DynamicFileState<Source>>,
}

impl<Source: SequenceFileSource> DynamicFile<Source> {
    pub fn new(source: Source) -> Self {
        DynamicFile { state: Mutex::new(DynamicFileState::new(source)) }
    }
}

impl<Source: SequenceFileSource + Clone> DynamicFile<Source> {
    pub fn new_node(source: Source) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(DynamicFile::new(source.clone())))
    }
}

impl<Source: SequenceFileSource> DynamicFile<Source> {
    fn read_internal(&self, offset: usize, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        self.state.lock().read(offset, data)
    }
}

impl<Source: SequenceFileSource> FileOps for DynamicFile<Source> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.read_internal(offset, data)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        let new_offset = default_seek(current_offset, target, |_| error!(EINVAL))?;

        // Call `read(0)` to ensure the data is generated now instead of later (except, when
        // seeking to the start of the file).
        if new_offset > 0 {
            let mut dummy_buf = VecOutputBuffer::new(0);
            self.read_internal(new_offset as usize, &mut dummy_buf)?;
        }

        Ok(new_offset)
    }
}

/// Internal state of a `DynamicFile`.
struct DynamicFileState<Source: SequenceFileSource> {
    /// The `Source` that's used to generate content of the file.
    source: Source,

    /// The current position in the sequence. This is an opaque object. Stepping the iterator
    /// replaces it with the next value in the sequence.
    cursor: Option<Source::Cursor>,

    /// Buffer for upcoming data in the sequence. Read calls will expand this buffer until it is
    /// big enough and then copy out data from it.
    buf: DynamicFileBuf,

    /// The current seek offset in the file. The first byte in the buffer is at this offset in the
    /// file.
    ///
    /// If a read has an offset greater than this, bytes will be generated from the iterator
    /// and skipped. If a read has an offset less than this, all state is reset and iteration
    /// starts from the beginning until it reaches the requested offset.
    byte_offset: usize,
}

impl<Source: SequenceFileSource> DynamicFileState<Source> {
    fn new(source: Source) -> Self {
        Self {
            source,
            cursor: Some(Source::Cursor::default()),
            buf: DynamicFileBuf::default(),
            byte_offset: 0,
        }
    }
}

impl<Source: SequenceFileSource> DynamicFileState<Source> {
    fn reset(&mut self) {
        self.cursor = Some(Source::Cursor::default());
        self.buf = DynamicFileBuf::default();
        self.byte_offset = 0;
    }

    fn read(&mut self, offset: usize, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        if offset != self.byte_offset {
            self.reset();
        }
        let read_size = data.available();

        // 1. Grow the buffer until either EOF or it's at least as big as the read request
        while self.byte_offset + self.buf.0.len() < offset + read_size {
            let cursor = if let Some(cursor) = std::mem::take(&mut self.cursor) {
                cursor
            } else {
                break;
            };
            let mut buf = std::mem::take(&mut self.buf);
            self.cursor = self.source.next(cursor, &mut buf).map_err(|e| {
                // Reset everything on failure
                self.reset();
                e
            })?;
            self.buf = buf;

            // If the seek pointer is ahead of our current byte offset, we will generate data that
            // needs to be thrown away. Calculation for that is here.
            let to_drain = std::cmp::min(offset - self.byte_offset, self.buf.0.len());
            self.buf.0.drain(..to_drain);
            self.byte_offset += to_drain;
        }

        // 2. Copy out as much of the data as possible. `write()` may need to be called twice
        // because `VecDeque` keeps the data in a ring buffer.
        let (slice1, slice2) = self.buf.0.as_slices();
        let mut written = data.write(slice1)?;
        if written == slice1.len() && !slice2.is_empty() {
            written += data.write(slice2)?;
        }

        // 3. Move the current position and drop the consumed data.
        self.buf.0.drain(..written);
        self.byte_offset += written;
        Ok(written)
    }
}

#[derive(Default)]
pub struct DynamicFileBuf(VecDeque<u8>);
impl DynamicFileBuf {
    pub fn write(&mut self, data: &[u8]) {
        self.0.extend(data.iter().copied());
    }
    pub fn write_iter<I>(&mut self, data: I)
    where
        I: IntoIterator<Item = u8>,
    {
        self.0.extend(data);
    }
    pub fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> Result<usize, Errno> {
        let start_size = self.0.len();
        std::io::Write::write_fmt(&mut self.0, args).map_err(|_| errno!(EINVAL))?;
        let end_size = self.0.len();
        Ok(end_size - start_size)
    }
}

/// A file whose contents are fixed even if writes occur.
pub struct ConstFile(DynamicFile<ConstFileSource>);

struct ConstFileSource {
    data: Vec<u8>,
}

impl DynamicFileSource for ConstFileSource {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        sink.write(&self.data);
        Ok(())
    }
}

impl ConstFile {
    /// Create a file with the given contents.
    pub fn new_node(data: Vec<u8>) -> impl FsNodeOps {
        SimpleFileNode::new(move || {
            Ok(Self(DynamicFile::new(ConstFileSource { data: data.clone() })))
        })
    }
}

impl FileOps for ConstFile {
    fileops_impl_delegate_read_and_seek!(self, self.0);

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        testing::{create_kernel_task_and_unlocked, AutoReleasableTask},
        vfs::{
            Anon, DynamicFile, DynamicFileBuf, DynamicFileSource, FileHandle, SeekTarget,
            SequenceFileSource, VecOutputBuffer,
        },
    };
    use starnix_sync::{Locked, Mutex, Unlocked};
    use starnix_uapi::{errors::Errno, open_flags::OpenFlags};
    use std::sync::Arc;

    struct Counter {
        value: Mutex<u8>,
    }

    struct TestSequenceFileSource;

    impl SequenceFileSource for TestSequenceFileSource {
        type Cursor = u8;
        fn next(&self, i: u8, sink: &mut DynamicFileBuf) -> Result<Option<u8>, Errno> {
            sink.write(&[i]);
            Ok(if i == u8::MAX { None } else { Some(i + 1) })
        }
    }

    fn create_test_file<'l, T: SequenceFileSource>(
        source: T,
    ) -> (AutoReleasableTask, FileHandle, Locked<'l, Unlocked>) {
        let (_kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let file =
            Anon::new_file(&current_task, Box::new(DynamicFile::new(source)), OpenFlags::RDONLY);
        (current_task, file, locked)
    }

    #[::fuchsia::test]
    async fn test_sequence() -> Result<(), Errno> {
        let (current_task, file, mut locked) = create_test_file(TestSequenceFileSource {});

        let read_at = |locked: &mut Locked<'_, Unlocked>,
                       offset: usize,
                       length: usize|
         -> Result<Vec<u8>, Errno> {
            let mut buffer = VecOutputBuffer::new(length);
            file.read_at(locked, &current_task, offset, &mut buffer)?;
            Ok(buffer.data().to_vec())
        };

        assert_eq!(read_at(&mut locked, 0, 2)?, &[0, 1]);
        assert_eq!(read_at(&mut locked, 2, 2)?, &[2, 3]);
        assert_eq!(read_at(&mut locked, 4, 4)?, &[4, 5, 6, 7]);
        assert_eq!(read_at(&mut locked, 0, 2)?, &[0, 1]);
        assert_eq!(read_at(&mut locked, 4, 2)?, &[4, 5]);
        Ok(())
    }

    struct TestFileSource {
        counter: Arc<Counter>,
    }

    impl DynamicFileSource for TestFileSource {
        fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
            let mut counter = self.counter.value.lock();
            let base = *counter;
            // Write 10 bytes where v[i] = base + i.
            let data = (0..10).map(|i| base + i).collect::<Vec<u8>>();
            sink.write(&data);
            *counter += 1;
            Ok(())
        }
    }

    #[::fuchsia::test]
    async fn test_read() -> Result<(), Errno> {
        let counter = Arc::new(Counter { value: Mutex::new(0) });
        let (current_task, file, mut locked) = create_test_file(TestFileSource { counter });
        let read_at = |locked: &mut Locked<'_, Unlocked>,
                       offset: usize,
                       length: usize|
         -> Result<Vec<u8>, Errno> {
            let mut buffer = VecOutputBuffer::new(length);
            let bytes_read = file.read_at(locked, &current_task, offset, &mut buffer)?;
            Ok(buffer.data()[0..bytes_read].to_vec())
        };

        // Verify that we can read all data to the end.
        assert_eq!(read_at(&mut locked, 0, 20)?, (0..10).collect::<Vec<u8>>());

        // Read from the beginning. Content should be refreshed.
        assert_eq!(read_at(&mut locked, 0, 2)?, [1, 2]);

        // Continue reading. Content should not be updated.
        assert_eq!(read_at(&mut locked, 2, 2)?, [3, 4]);

        // Try reading from a new position. Content should be updated.
        assert_eq!(read_at(&mut locked, 5, 2)?, [7, 8]);

        Ok(())
    }

    #[::fuchsia::test]
    async fn test_read_and_seek() -> Result<(), Errno> {
        let counter = Arc::new(Counter { value: Mutex::new(0) });
        let (current_task, file, mut locked) =
            create_test_file(TestFileSource { counter: counter.clone() });
        let read = |locked: &mut Locked<'_, Unlocked>, length: usize| -> Result<Vec<u8>, Errno> {
            let mut buffer = VecOutputBuffer::new(length);
            let bytes_read = file.read(locked, &current_task, &mut buffer)?;
            Ok(buffer.data()[0..bytes_read].to_vec())
        };

        // Call `read()` to read the content all the way to the end. Content should not update
        assert_eq!(read(&mut locked, 1)?, [0]);
        assert_eq!(read(&mut locked, 2)?, [1, 2]);
        assert_eq!(read(&mut locked, 20)?, (3..10).collect::<Vec<u8>>());

        // Seek to the start of the file. Content should be updated on the following read.
        file.seek(&current_task, SeekTarget::Set(0))?;
        assert_eq!(*counter.value.lock(), 1);
        assert_eq!(read(&mut locked, 2)?, [1, 2]);
        assert_eq!(*counter.value.lock(), 2);

        // Seeking to `pos > 0` should update the content.
        file.seek(&current_task, SeekTarget::Set(1))?;
        assert_eq!(*counter.value.lock(), 3);

        Ok(())
    }
}
