// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lock::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard},
    task::{CurrentTask, WaitQueue, Waiter},
    types::Errno,
};
use std::ops::{Deref, DerefMut};

/// The guard associated with `InterruptibleMutex`.
pub type InterruptibleMutexGuard<'a, T> = InterruptibleGuard<'a, Mutex<T>, MutexGuard<'a, T>>;
/// The read guard associated with `InterruptibleRwLock`.
pub type InterruptibleRwLockReadGuard<'a, T> =
    InterruptibleGuard<'a, RwLock<T>, RwLockReadGuard<'a, T>>;
/// The write guard associated with `InterruptibleRwLock`.
pub type InterruptibleRwLockWriteGuard<'a, T> =
    InterruptibleGuard<'a, RwLock<T>, RwLockWriteGuard<'a, T>>;

/// Version of `Mutex` that can be interrupted when the starnix task is interrupted.
#[derive(Debug)]
pub struct InterruptibleMutex<T> {
    lock: InterruptibleBaseLock<Mutex<T>>,
}

impl<T> InterruptibleMutex<T> {
    fn new(t: T) -> Self {
        Self { lock: InterruptibleBaseLock::new(Mutex::new(t)) }
    }
    #[allow(dead_code)]
    pub fn lock<'a>(
        &'a self,
        current_task: &'a CurrentTask,
    ) -> Result<InterruptibleMutexGuard<'a, T>, Errno> {
        self.lock.lock(current_task, false, |l| l.try_lock())
    }
}

impl<T: Default> Default for InterruptibleMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[derive(Debug)]
pub struct InterruptibleRwLock<T> {
    lock: InterruptibleBaseLock<RwLock<T>>,
}

/// Version of `RwLock` that can be interrupted when the starnix task is interrupted.
impl<T> InterruptibleRwLock<T> {
    fn new(t: T) -> Self {
        Self { lock: InterruptibleBaseLock::new(RwLock::new(t)) }
    }
    pub fn read<'a>(
        &'a self,
        current_task: &'a CurrentTask,
    ) -> Result<InterruptibleRwLockReadGuard<'a, T>, Errno> {
        self.lock.lock(current_task, true, |l| l.try_read())
    }
    pub fn write<'a>(
        &'a self,
        current_task: &'a CurrentTask,
    ) -> Result<InterruptibleRwLockWriteGuard<'a, T>, Errno> {
        self.lock.lock(current_task, true, |l| l.try_write())
    }
    /// Debug method that will unconditionally obtain a read guard from the underlying lock. Used
    /// to ensure lock ordering with tracing_mutex.
    #[cfg(any(test, debug_assertions))]
    pub fn raw_read(&self) -> RwLockReadGuard<'_, T> {
        self.lock.lock.read()
    }
}

impl<T: Default> Default for InterruptibleRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[derive(Debug)]
struct InterruptibleBaseLock<L> {
    waiters: WaitQueue,
    lock: L,
}

impl<L> InterruptibleBaseLock<L> {
    fn new(lock: L) -> Self {
        Self { waiters: Default::default(), lock }
    }
    pub fn lock<'a, G, F>(
        &'a self,
        current_task: &'a CurrentTask,
        notify_all: bool,
        acquire_guard: F,
    ) -> Result<InterruptibleGuard<'a, L, G>, Errno>
    where
        F: Fn(&'a L) -> Option<G>,
    {
        // Try once to lock without creating a waiter.
        if let Some(guard) = acquire_guard(&self.lock) {
            return Ok(InterruptibleGuard {
                guard,
                _notifier: LockNotifier { lock: self, notify_all },
            });
        }
        // The lock is contended, creating a waiter.
        let waiter = Waiter::new();
        loop {
            self.waiters.wait_async(&waiter);
            if let Some(guard) = acquire_guard(&self.lock) {
                return Ok(InterruptibleGuard {
                    guard,
                    _notifier: LockNotifier { lock: self, notify_all },
                });
            }
            if let Err(e) = waiter.wait(current_task) {
                // If the wait is interrupted, notify the queue before quitting in case the
                // interruption happened concurrently to a wake signal for this thread.
                self.notify(notify_all);
                return Err(e);
            }
        }
    }

    fn notify(&self, notify_all: bool) {
        if notify_all {
            self.waiters.notify_all();
        } else {
            self.waiters.notify_count(1);
        }
    }
}

/// Guard like structure that notify its lock when dropped.
#[derive(Debug)]
pub struct LockNotifier<'a, L> {
    lock: &'a InterruptibleBaseLock<L>,
    notify_all: bool,
}

impl<'a, L> Drop for LockNotifier<'a, L> {
    fn drop(&mut self) {
        self.lock.notify(self.notify_all);
    }
}

#[derive(Debug)]
pub struct InterruptibleGuard<'a, L, G> {
    /// The guard that this object wraps.
    guard: G,
    /// Guard like member that will notify the lock when the guard is dropped. Need to be after
    /// `guard` to ensure the notification happens after the guard is released.
    _notifier: LockNotifier<'a, L>,
}

impl<'a, L, G: Deref<Target = T>, T> Deref for InterruptibleGuard<'a, L, G> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<'a, L, G: DerefMut<Target = T>, T> DerefMut for InterruptibleGuard<'a, L, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.deref_mut()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        testing::{create_kernel_and_task, create_task},
        types::{errno, pid_t},
    };
    use std::{
        sync::{mpsc::sync_channel, Arc},
        thread,
    };

    #[::fuchsia::test]
    async fn test_lock() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleMutex::new(0));
        let mut guard = value.lock(&task).expect("lock");
        let t = std::thread::spawn({
            let value = value.clone();
            move || {
                let task = create_task(&kernel, "second task");
                *value.lock(&task).expect("lock") = 42;
            }
        });
        thread::sleep(std::time::Duration::from_millis(50));
        *guard = 3;
        std::mem::drop(guard);
        t.join().expect("join");
        assert_eq!(*value.lock(&task).expect("lock"), 42);
    }

    #[::fuchsia::test]
    async fn test_rwlock_write_write() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleRwLock::new(0));
        let mut guard = value.write(&task).expect("write");
        let t = std::thread::spawn({
            let value = value.clone();
            move || {
                let task = create_task(&kernel, "second task");
                *value.write(&task).expect("lock") = 42;
            }
        });
        thread::sleep(std::time::Duration::from_millis(50));
        *guard = 3;
        std::mem::drop(guard);
        t.join().expect("join");
        assert_eq!(*value.read(&task).expect("lock"), 42);
    }

    #[::fuchsia::test]
    async fn test_rwlock_read_write() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleRwLock::new(0));
        let guard = value.read(&task).expect("read");
        let t = std::thread::spawn({
            let value = value.clone();
            move || {
                let task = create_task(&kernel, "second task");
                *value.write(&task).expect("lock") = 42;
            }
        });
        thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(*guard, 0);
        std::mem::drop(guard);
        t.join().expect("join");
        assert_eq!(*value.read(&task).expect("lock"), 42);
    }

    #[::fuchsia::test]
    async fn test_rwlock_read_read() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleRwLock::new(0));
        let guard = value.read(&task).expect("read");
        let t = std::thread::spawn({
            let value = value.clone();
            move || {
                let task = create_task(&kernel, "second task");
                assert_eq!(*value.read(&task).expect("lock"), 0);
            }
        });
        t.join().expect("join");
        assert_eq!(*guard, 0);
    }

    #[::fuchsia::test]
    async fn test_mutex_high_concurrency() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleMutex::new(0));
        let threads = (0..50)
            .map(|i| {
                let kernel = kernel.clone();
                let value = value.clone();
                std::thread::spawn(move || {
                    let task = create_task(&kernel, "concurrent_task");
                    if i % 2 == 0 {
                        let v = *value.lock(&task).expect("lock");
                        assert!(v >= 0);
                        assert!(v <= 25);
                    } else {
                        *value.lock(&task).expect("lock") += 1;
                    }
                })
            })
            .collect::<Vec<_>>();
        for t in threads {
            t.join().expect("join");
        }
        assert_eq!(*value.lock(&task).expect("lock"), 25);
    }

    #[::fuchsia::test]
    async fn test_rwlock_high_concurrency() {
        let (kernel, task) = create_kernel_and_task();
        let value = Arc::new(InterruptibleRwLock::new(0));
        let threads = (0..50)
            .map(|i| {
                let kernel = kernel.clone();
                let value = value.clone();
                std::thread::spawn(move || {
                    let task = create_task(&kernel, "concurrent_task");
                    if i % 2 == 0 {
                        let v = *value.read(&task).expect("lock");
                        assert!(v >= 0);
                        assert!(v <= 25);
                    } else {
                        *value.write(&task).expect("lock") += 1;
                    }
                })
            })
            .collect::<Vec<_>>();
        for t in threads {
            t.join().expect("join");
        }
        assert_eq!(*value.read(&task).expect("lock"), 25);
    }

    #[::fuchsia::test]
    async fn test_interrupt() {
        let (kernel, task) = create_kernel_and_task();
        let (sender, receiver) = sync_channel::<pid_t>(1);
        let value = Arc::new(InterruptibleMutex::new(0));
        let guard = value.lock(&task).expect("lock");
        let t = std::thread::spawn({
            let kernel = kernel.clone();
            let value = value.clone();
            move || {
                let task = create_task(&kernel, "second task");
                sender.send(task.get_tid()).expect("send");
                assert_eq!(value.lock(&task).expect_err("lock"), errno!(EINTR));
            }
        });
        let tid = receiver.recv().expect("recv");
        let other_task_weak = kernel.pids.read().get_task(tid);
        let other_task = other_task_weak.upgrade().expect("task");
        loop {
            let other_task_waiting = other_task.read().signals.run_state.is_blocked();
            if other_task_waiting {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        other_task.interrupt();
        // Drop other_task to let the thread release it.
        std::mem::drop(other_task);
        t.join().expect("join");
        std::mem::drop(guard);
        assert_eq!(*value.lock(&task).expect("lock"), 0);
    }
}
