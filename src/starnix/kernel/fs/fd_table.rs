// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use starnix_lock::Mutex;
use std::ops::DerefMut;

use crate::{
    fs::{FdNumber, FileHandle, RecordLockOwner},
    task::{CurrentTask, Task},
    types::{
        errno, error, Errno, OpenFlags, OwnedRef, Releasable, ReleasableByRef, Resource, FD_CLOEXEC,
    },
};

bitflags! {
    pub struct FdFlags: u32 {
        const CLOEXEC = FD_CLOEXEC;
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct FdTableId(usize);

impl FdTableId {
    fn new(id: *const Vec<Option<FdTableEntry>>) -> Self {
        Self(id as usize)
    }

    pub fn raw(&self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct FdTableEntry {
    pub file: FileHandle,

    // Identifier of the FdTable containing this entry.
    fd_table_id: FdTableId,

    // Rather than using a separate "flags" field, we could maintain this data
    // as a bitfield over the file descriptors because there is only one flag
    // currently (CLOEXEC) and file descriptor numbers tend to cluster near 0.
    flags: FdFlags,
}

impl Drop for FdTableEntry {
    fn drop(&mut self) {
        self.file.name.entry.node.record_lock_release(RecordLockOwner::FdTable(self.fd_table_id));
        self.file.flush();
    }
}

impl FdTableEntry {
    fn new(file: FileHandle, fd_table_id: FdTableId, flags: FdFlags) -> FdTableEntry {
        FdTableEntry { file, fd_table_id, flags }
    }
}

/// Having the map a separate data structure allows us to memoize next_fd, which is the
/// lowest numbered file descriptor not in use.
#[derive(Clone, Debug)]
struct FdTableStore {
    entries: Vec<Option<FdTableEntry>>,
    next_fd: FdNumber,
}

impl Default for FdTableStore {
    fn default() -> Self {
        FdTableStore { entries: Default::default(), next_fd: FdNumber::from_raw(0) }
    }
}

impl FdTableStore {
    fn insert_entry(
        &mut self,
        fd: FdNumber,
        rlimit: u64,
        entry: FdTableEntry,
    ) -> Result<Option<FdTableEntry>, Errno> {
        let raw_fd = fd.raw();
        if raw_fd < 0 {
            return error!(EBADF);
        }
        if raw_fd as u64 >= rlimit {
            return error!(EMFILE);
        }
        if raw_fd == self.next_fd.raw() {
            self.next_fd = self.calculate_lowest_available_fd(&FdNumber::from_raw(raw_fd + 1));
        }
        let raw_fd = raw_fd as usize;
        if raw_fd >= self.entries.len() {
            self.entries.resize(raw_fd + 1, None);
        }
        let mut entry = Some(entry);
        std::mem::swap(&mut entry, &mut self.entries[raw_fd]);
        Ok(entry)
    }

    fn remove_entry(&mut self, fd: &FdNumber) -> Option<FdTableEntry> {
        let raw_fd = fd.raw() as usize;
        if raw_fd >= self.entries.len() {
            return None;
        }
        let mut removed = None;
        std::mem::swap(&mut removed, &mut self.entries[raw_fd]);
        if removed.is_some() && raw_fd < self.next_fd.raw() as usize {
            self.next_fd = *fd;
        }
        removed
    }

    fn get(&self, fd: FdNumber) -> Option<&FdTableEntry> {
        match self.entries.get(fd.raw() as usize) {
            Some(Some(entry)) => Some(entry),
            _ => None,
        }
    }

    fn get_mut(&mut self, fd: FdNumber) -> Option<&mut FdTableEntry> {
        match self.entries.get_mut(fd.raw() as usize) {
            Some(Some(entry)) => Some(entry),
            _ => None,
        }
    }

    // Returns the (possibly memoized) lowest available FD >= minfd in this map.
    fn get_lowest_available_fd(&self, minfd: FdNumber) -> FdNumber {
        if minfd.raw() > self.next_fd.raw() {
            return self.calculate_lowest_available_fd(&minfd);
        }
        self.next_fd
    }

    // Recalculates the lowest available FD >= minfd based on the contents of the map.
    fn calculate_lowest_available_fd(&self, minfd: &FdNumber) -> FdNumber {
        let mut fd = *minfd;
        while self.get(fd).is_some() {
            fd = FdNumber::from_raw(fd.raw() + 1);
        }
        fd
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&FdNumber, &mut FdTableEntry) -> bool,
    {
        for (index, maybe_entry) in self.entries.iter_mut().enumerate() {
            let fd = FdNumber::from_raw(index as i32);
            if let Some(entry) = maybe_entry {
                if !f(&fd, entry) {
                    *maybe_entry = None;
                }
            }
        }
        self.next_fd = self.calculate_lowest_available_fd(&FdNumber::from_raw(0));
    }
}

#[derive(Debug, Default)]
struct FdTableInner {
    store: Mutex<FdTableStore>,
}

impl FdTableInner {
    fn id(&self) -> FdTableId {
        FdTableId::new(&self.store.lock().entries as *const Vec<Option<FdTableEntry>>)
    }

    fn unshare(&self) -> OwnedRef<FdTableInner> {
        let inner = {
            let new_store = self.store.lock().clone();
            FdTableInner { store: Mutex::new(new_store) }
        };
        let id = inner.id();
        for maybe_entry in inner.store.lock().entries.iter_mut() {
            if let Some(entry) = maybe_entry {
                entry.fd_table_id = id;
            }
        }
        OwnedRef::new(inner)
    }
}

impl Releasable for FdTableInner {
    type Context<'a> = &'a CurrentTask;

    fn release(self, _current_task: &CurrentTask) {
        // Nothing to do until FdMap is Releasable. This will drop this object.
    }
}

#[derive(Debug, Default)]
pub struct FdTable {
    inner: Mutex<OwnedRef<FdTableInner>>,
}

pub enum TargetFdNumber {
    /// The duplicated FdNumber will be the smallest available FdNumber.
    Default,

    /// The duplicated FdNumber should be this specific FdNumber.
    Specific(FdNumber),

    /// The duplicated FdNumber should be greater than this FdNumber.
    Minimum(FdNumber),
}

impl FdTable {
    pub fn id(&self) -> FdTableId {
        self.inner.lock().id()
    }

    pub fn fork(&self) -> FdTable {
        let inner = Mutex::new(self.inner.lock().unshare());
        FdTable { inner }
    }

    pub fn unshare(&self, current_task: &CurrentTask) {
        let old_inner = {
            let mut innner = self.inner.lock();
            let new_inner = innner.unshare();
            std::mem::replace(innner.deref_mut(), new_inner)
        };
        old_inner.release(current_task);
    }

    pub fn exec(&self) {
        self.retain(|_fd, flags| !flags.contains(FdFlags::CLOEXEC));
    }

    pub fn insert(&self, task: &Task, fd: FdNumber, file: FileHandle) -> Result<(), Errno> {
        self.insert_with_flags(task, fd, file, FdFlags::empty())
    }

    pub fn insert_with_flags(
        &self,
        task: &Task,
        fd: FdNumber,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<(), Errno> {
        profile_duration!("InsertFd");
        let removed_entry;
        {
            let rlimit = task.thread_group.get_rlimit(Resource::NOFILE);
            let id = self.id();
            let inner = self.inner.lock();
            let mut state = inner.store.lock();
            removed_entry = state.insert_entry(fd, rlimit, FdTableEntry::new(file, id, flags))?;
        }
        // Drop the removed entry after we drop the table lock.
        std::mem::drop(removed_entry);
        Ok(())
    }

    pub fn add_with_flags(
        &self,
        task: &Task,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        profile_duration!("AddFd");
        let fd;
        let removed_entry;
        {
            let rlimit = task.thread_group.get_rlimit(Resource::NOFILE);
            let id = self.id();
            let inner = self.inner.lock();
            let mut state = inner.store.lock();
            fd = state.next_fd;
            removed_entry = state.insert_entry(fd, rlimit, FdTableEntry::new(file, id, flags))?;
        }
        // Drop the removed entry after we drop the table lock.
        std::mem::drop(removed_entry);
        Ok(fd)
    }

    // Duplicates a file handle.
    // If target is  TargetFdNumber::Minimum, a new FdNumber is allocated. Returns the new FdNumber.
    pub fn duplicate(
        &self,
        task: &Task,
        oldfd: FdNumber,
        target: TargetFdNumber,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        profile_duration!("DuplicateFd");
        // Drop the removed entry only after releasing the writer lock in case
        // the close() function on the FileOps calls back into the FdTable.
        let _removed_entry;
        let result = {
            let rlimit = task.thread_group.get_rlimit(Resource::NOFILE);
            let id = self.id();
            let inner = self.inner.lock();
            let mut state = inner.store.lock();
            let file =
                state.get(oldfd).map(|entry| entry.file.clone()).ok_or_else(|| errno!(EBADF))?;

            let fd = match target {
                TargetFdNumber::Specific(fd) => {
                    // We need to check the rlimit before we remove the entry from state
                    // because we cannot error out after removing the entry.
                    if fd.raw() as u64 >= rlimit {
                        // ltp_dup201 shows that we're supposed to return EBADF in this
                        // situtation, instead of EMFILE, which is what we normally return
                        // when we're past the rlimit.
                        return error!(EBADF);
                    }
                    _removed_entry = state.remove_entry(&fd);
                    fd
                }
                TargetFdNumber::Minimum(fd) => state.get_lowest_available_fd(fd),
                TargetFdNumber::Default => state.get_lowest_available_fd(FdNumber::from_raw(0)),
            };
            let existing_entry =
                state.insert_entry(fd, rlimit, FdTableEntry::new(file, id, flags))?;
            assert!(existing_entry.is_none());
            Ok(fd)
        };
        result
    }

    pub fn get(&self, fd: FdNumber) -> Result<FileHandle, Errno> {
        self.get_with_flags(fd).map(|(file, _flags)| file)
    }

    pub fn get_with_flags(&self, fd: FdNumber) -> Result<(FileHandle, FdFlags), Errno> {
        profile_duration!("GetFdWithFlags");
        let inner = self.inner.lock();
        let state = inner.store.lock();
        state.get(fd).map(|entry| (entry.file.clone(), entry.flags)).ok_or_else(|| errno!(EBADF))
    }

    pub fn get_unless_opath(&self, fd: FdNumber) -> Result<FileHandle, Errno> {
        let file = self.get(fd)?;
        if file.flags().contains(OpenFlags::PATH) {
            return error!(EBADF);
        }
        Ok(file)
    }

    pub fn close(&self, fd: FdNumber) -> Result<(), Errno> {
        profile_duration!("CloseFile");
        // Drop the file object only after releasing the writer lock in case
        // the close() function on the FileOps calls back into the FdTable.
        let removed = {
            let inner = self.inner.lock();
            let mut state = inner.store.lock();
            state.remove_entry(&fd)
        };
        if removed.is_some() {
            Ok(())
        } else {
            Err(errno!(EBADF))
        }
    }

    pub fn get_fd_flags(&self, fd: FdNumber) -> Result<FdFlags, Errno> {
        self.get_with_flags(fd).map(|(_file, flags)| flags)
    }

    pub fn set_fd_flags(&self, fd: FdNumber, flags: FdFlags) -> Result<(), Errno> {
        profile_duration!("SetFdFlags");
        self.inner
            .lock()
            .store
            .lock()
            .get_mut(fd)
            .map(|entry| {
                entry.flags = flags;
            })
            .ok_or_else(|| errno!(EBADF))
    }

    pub fn retain<F>(&self, f: F)
    where
        F: Fn(FdNumber, &mut FdFlags) -> bool,
    {
        profile_duration!("RetainFds");
        let mut doomed = vec![];
        self.inner.lock().store.lock().retain(|fd, entry| {
            if f(*fd, &mut entry.flags) {
                true
            } else {
                doomed.push(entry.file.clone());
                false
            }
        });
        // Avoid dropping the files until after we drop the table locks.
        std::mem::drop(doomed);
    }

    /// Returns a vector of all current file descriptors in the table.
    pub fn get_all_fds(&self) -> Vec<FdNumber> {
        self.inner
            .lock()
            .store
            .lock()
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, maybe_entry)| {
                maybe_entry.as_ref().map(|_| FdNumber::from_raw(index as i32))
            })
            .collect()
    }
}

impl ReleasableByRef for FdTable {
    type Context<'a> = &'a CurrentTask;
    /// Drop the fd table, closing any files opened exclusively by this table.
    fn release(&self, current_task: &CurrentTask) {
        let inner = OwnedRef::take(&mut self.inner.lock());
        inner.release(current_task);
    }
}

impl Clone for FdTable {
    fn clone(&self) -> Self {
        FdTable { inner: Mutex::new(self.inner.lock().clone()) }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Arc;

    use crate::{fs::fuchsia::SyslogFile, task::*, testing::*};

    fn add(
        current_task: &CurrentTask,
        files: &FdTable,
        file: FileHandle,
    ) -> Result<FdNumber, Errno> {
        files.add_with_flags(current_task, file, FdFlags::empty())
    }

    #[::fuchsia::test]
    async fn test_fd_table_install() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd0.raw(), 0);
        let fd1 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd1.raw(), 1);

        assert!(Arc::ptr_eq(&files.get(fd0).unwrap(), &file));
        assert!(Arc::ptr_eq(&files.get(fd1).unwrap(), &file));
        assert_eq!(files.get(FdNumber::from_raw(fd1.raw() + 1)).map(|_| ()), error!(EBADF));

        files.release(&current_task);
    }

    #[::fuchsia::test]
    async fn test_fd_table_fork() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file).unwrap();
        let fd2 = FdNumber::from_raw(2);

        let forked = files.fork();

        assert_eq!(Arc::as_ptr(&files.get(fd0).unwrap()), Arc::as_ptr(&forked.get(fd0).unwrap()));
        assert_eq!(Arc::as_ptr(&files.get(fd1).unwrap()), Arc::as_ptr(&forked.get(fd1).unwrap()));
        assert!(files.get(fd2).is_err());
        assert!(forked.get(fd2).is_err());

        files.set_fd_flags(fd0, FdFlags::CLOEXEC).unwrap();
        assert_eq!(FdFlags::CLOEXEC, files.get_fd_flags(fd0).unwrap());
        assert_ne!(FdFlags::CLOEXEC, forked.get_fd_flags(fd0).unwrap());

        forked.release(&current_task);
        files.release(&current_task);
    }

    #[::fuchsia::test]
    async fn test_fd_table_exec() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file).unwrap();

        files.set_fd_flags(fd0, FdFlags::CLOEXEC).unwrap();

        assert!(files.get(fd0).is_ok());
        assert!(files.get(fd1).is_ok());

        files.exec();

        assert!(files.get(fd0).is_err());
        assert!(files.get(fd1).is_ok());

        files.release(&current_task);
    }

    #[::fuchsia::test]
    async fn test_fd_table_pack_values() {
        let (_kernel, current_task) = create_kernel_and_task();
        let files = FdTable::default();
        let file = SyslogFile::new_file(&current_task);

        // Add two FDs.
        let fd0 = add(&current_task, &files, file.clone()).unwrap();
        let fd1 = add(&current_task, &files, file.clone()).unwrap();
        assert_eq!(fd0.raw(), 0);
        assert_eq!(fd1.raw(), 1);

        // Close FD 0
        assert!(files.close(fd0).is_ok());
        assert!(files.close(fd0).is_err());
        // Now it's gone.
        assert!(files.get(fd0).is_err());

        // The next FD we insert fills in the hole we created.
        let another_fd = add(&current_task, &files, file).unwrap();
        assert_eq!(another_fd.raw(), 0);

        files.release(&current_task);
    }
}
