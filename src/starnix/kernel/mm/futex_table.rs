// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use futures::channel::oneshot;
use starnix_lock::Mutex;
use starnix_sync::InterruptibleEvent;
use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    hash::Hash,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Weak,
    },
};

use crate::{
    mm::{ProtectionFlags, PAGE_SIZE},
    task::{CurrentTask, Task},
};
use starnix_logging::{impossible_error, log_error};
use starnix_uapi::{
    errno, error, errors::Errno, user_address::UserAddress, FUTEX_BITSET_MATCH_ANY, FUTEX_TID_MASK,
    FUTEX_WAITERS,
};

/// A table of futexes.
///
/// Each 32-bit aligned address in an address space can potentially have an associated futex that
/// userspace can wait upon. This table is a sparse representation that has an actual WaitQueue
/// only for those addresses that have ever actually had a futex operation performed on them.
pub struct FutexTable<Key: FutexKey> {
    /// The futexes associated with each address in each VMO.
    ///
    /// This HashMap is populated on-demand when futexes are used.
    state: Mutex<FutexTableState<Key>>,
}

impl<Key: FutexKey> Default for FutexTable<Key> {
    fn default() -> Self {
        Self { state: Mutex::new(FutexTableState::default()) }
    }
}

impl<Key: FutexKey> FutexTable<Key> {
    /// Wait on the futex at the given address.
    ///
    /// See FUTEX_WAIT.
    pub fn wait(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        value: u32,
        mask: u32,
        deadline: zx::Time,
    ) -> Result<(), Errno> {
        let mut state = self.state.lock();
        // As the state is locked, no wake can happen before the waiter is registered.
        // If the addr is remapped, we will read stale data, but we will not miss a futex wake.
        let (FutexOperand { vmo, offset }, key) =
            Key::get_operand_and_key(current_task, addr, ProtectionFlags::READ)?;
        Self::check_futex_value(&vmo, offset, value)?;

        let event = InterruptibleEvent::new();
        let guard = event.begin_wait();
        state.get_waiters_or_default(key).add(FutexWaiter {
            mask,
            notifiable: FutexNotifiable::new_internal(Arc::downgrade(&event)),
        });
        std::mem::drop(state);

        current_task.block_until(guard, deadline)
    }

    /// Wake the given number of waiters on futex at the given address. Returns the number of
    /// waiters actually woken.
    ///
    /// See FUTEX_WAKE.
    pub fn wake(
        &self,
        task: &Task,
        addr: UserAddress,
        count: usize,
        mask: u32,
    ) -> Result<usize, Errno> {
        let key = Key::get_key(task, addr)?;
        Ok(self.state.lock().wake(key, count, mask))
    }

    /// Requeue the waiters to another address.
    ///
    /// See FUTEX_REQUEUE
    pub fn requeue(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        count: usize,
        new_addr: UserAddress,
    ) -> Result<usize, Errno> {
        let key = Key::get_key(current_task, addr)?;
        let new_key = Key::get_key(current_task, new_addr)?;
        let mut state = self.state.lock();
        let mut old_waiters = if let Some(old_waiters) = state.waiters.remove(&key) {
            old_waiters
        } else {
            return Ok(0);
        };
        let woken = old_waiters.notify(FUTEX_BITSET_MATCH_ANY, count);
        if !old_waiters.is_empty() {
            state.get_waiters_or_default(new_key).transfer(old_waiters);
        }
        Ok(woken)
    }

    /// Lock the futex at the given address.
    ///
    /// See FUTEX_LOCK_PI.
    pub fn lock_pi(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        deadline: zx::Time,
    ) -> Result<(), Errno> {
        let mut state = self.state.lock();
        // As the state is locked, no unlock can happen before the waiter is registered.
        // If the addr is remapped, we will read stale data, but we will not miss a futex unlock.
        let (operand, key) = Key::get_operand_and_key(
            current_task,
            addr,
            ProtectionFlags::READ | ProtectionFlags::WRITE,
        )?;

        let tid = current_task.get_tid() as u32;

        {
            // Unfortunately, we need these operations to actually be atomic, which means we need
            // to create a mapping for this memory in the kernel address space. Churning these
            // mappings is painfully slow. We should be able to optimize this operation with
            // co-resident mappings.
            let mapping = FutexOperandMapping::painfully_slow_create(&operand)?;
            let value = mapping.value();

            let mut current_value = value.load(Ordering::Relaxed);
            loop {
                if current_value & FUTEX_TID_MASK == tid {
                    // From <https://man7.org/linux/man-pages/man2/futex.2.html>:
                    //
                    //   EDEADLK
                    //          (FUTEX_LOCK_PI, FUTEX_LOCK_PI2, FUTEX_TRYLOCK_PI,
                    //          FUTEX_CMP_REQUEUE_PI) The futex word at uaddr is already
                    //          locked by the caller.
                    return error!(EDEADLK);
                }

                if current_value == 0 {
                    match value.compare_exchange_weak(
                        current_value,
                        tid,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(observed_state) => {
                            current_value = observed_state;
                            continue;
                        }
                    }
                }

                let target_value = current_value | FUTEX_WAITERS;
                if let Err(observed_state) = value.compare_exchange(
                    current_value,
                    target_value,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    current_value = observed_state;
                    continue;
                }
                break;
            }

            // We will drop the mapping when we leave this scope.
        }

        let event = InterruptibleEvent::new();
        let guard = event.begin_wait();
        let notifiable = FutexNotifiable::new_internal(Arc::downgrade(&event));
        state.get_rt_mutex_waiters_or_default(key).push_back(RtMutexWaiter { tid, notifiable });
        std::mem::drop(state);

        current_task.block_until(guard, deadline)
    }

    /// Unlock the futex at the given address.
    ///
    /// See FUTEX_UNLOCK_PI.
    pub fn unlock_pi(&self, current_task: &CurrentTask, addr: UserAddress) -> Result<(), Errno> {
        let mut state = self.state.lock();
        let tid = current_task.get_tid() as u32;

        let (operand, key) = Key::get_operand_and_key(
            current_task,
            addr,
            ProtectionFlags::READ | ProtectionFlags::WRITE,
        )?;

        {
            // Unfortunately, we need these operations to actually be atomic, which means we need
            // to create a mapping for this memory in the kernel address space. Churning these
            // mappings is painfully slow. We should be able to optimize this operation with
            // co-resident mappings.
            let mapping = FutexOperandMapping::painfully_slow_create(&operand)?;
            let value = mapping.value();

            let current_value = value.load(Ordering::Relaxed);
            if current_value & FUTEX_TID_MASK != tid {
                // From <https://man7.org/linux/man-pages/man2/futex.2.html>:
                //
                //   EPERM  (FUTEX_UNLOCK_PI) The caller does not own the lock
                //          represented by the futex word.
                return error!(EPERM);
            }

            loop {
                let maybe_waiter = state.pop_rt_mutex_waiter(key.clone());
                let target_value = if let Some(waiter) = &maybe_waiter { waiter.tid } else { 0 };

                if value
                    .compare_exchange(
                        current_value,
                        target_value,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    // From <https://man7.org/linux/man-pages/man2/futex.2.html>:
                    //
                    //   EINVAL (FUTEX_LOCK_PI, FUTEX_LOCK_PI2, FUTEX_TRYLOCK_PI,
                    //       FUTEX_UNLOCK_PI) The kernel detected an inconsistency
                    //       between the user-space state at uaddr and the kernel
                    //       state.  This indicates either state corruption or that the
                    //       kernel found a waiter on uaddr which is waiting via
                    //       FUTEX_WAIT or FUTEX_WAIT_BITSET.
                    return error!(EINVAL);
                }

                let Some(mut waiter) = maybe_waiter else {
                    // We can stop trying to notify a thread if there is are no more waiters.
                    break;
                };

                if waiter.notifiable.notify() {
                    break;
                }

                // If we couldn't notify the waiter, then we need to pull the next thread off the
                // waiter list.
            }

            // We will drop the mapping when we leave this scope.
        }

        Ok(())
    }

    fn read_futex_value(vmo: &zx::Vmo, offset: u64) -> Result<u32, Errno> {
        let mut buf = [0u8; 4];
        vmo.read(&mut buf, offset).map_err(|_| errno!(EINVAL))?;
        Ok(u32::from_ne_bytes(buf))
    }

    fn check_futex_value(vmo: &zx::Vmo, offset: u64, value: u32) -> Result<(), Errno> {
        // TODO: This read should be atomic.
        if Self::read_futex_value(vmo, offset)? != value {
            return error!(EAGAIN);
        }
        Ok(())
    }
}

impl FutexTable<SharedFutexKey> {
    /// Wait on the futex at the given offset in the vmo.
    ///
    /// See FUTEX_WAIT.
    pub fn external_wait(
        &self,
        vmo: zx::Vmo,
        offset: u64,
        value: u32,
        mask: u32,
    ) -> Result<oneshot::Receiver<()>, Errno> {
        let key = SharedFutexKey::new(&vmo, offset)?;
        let mut state = self.state.lock();
        // As the state is locked, no wake can happen before the waiter is registered.
        Self::check_futex_value(&vmo, offset, value)?;

        let (sender, receiver) = oneshot::channel::<()>();
        state
            .get_waiters_or_default(key)
            .add(FutexWaiter { mask, notifiable: FutexNotifiable::new_external(sender) });
        Ok(receiver)
    }

    /// Wake the given number of waiters on futex at the given offset in the vmo. Returns the
    /// number of waiters actually woken.
    ///
    /// See FUTEX_WAKE.
    pub fn external_wake(
        &self,
        vmo: zx::Vmo,
        offset: u64,
        count: usize,
        mask: u32,
    ) -> Result<usize, Errno> {
        Ok(self.state.lock().wake(SharedFutexKey::new(&vmo, offset)?, count, mask))
    }
}

pub struct FutexOperand {
    vmo: Arc<zx::Vmo>,
    offset: u64,
}

struct FutexOperandMapping {
    kernel_address: *const u8,
    length: usize,
    value_offset: usize,
}

impl FutexOperandMapping {
    /// Create a mapping for this futex operand in kernel memory.
    ///
    /// This operation is painfully slow because churning the kernel mappings requires coordination
    /// across all the page tables for all the userspace processes.
    ///
    /// TODO(b/276973344): Once we have coresident mappings, we should investigate using the
    /// restricted mapping of this memory instead of creating a kernel mapping.
    fn painfully_slow_create(operand: &FutexOperand) -> Result<Self, Errno> {
        let page_size: u64 = *PAGE_SIZE;
        let value_offset = operand.offset % page_size;
        let mapping_start = operand.offset - value_offset;

        let value_offset = value_offset as usize;
        let length = page_size as usize;

        // This assert always passes because `value_offset` is four-byte aligned according to
        // the checks in `{Private,Shared}FutexKey::get_key`.
        assert!(value_offset + std::mem::size_of::<AtomicU32>() <= length);

        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();
        let kernel_address = kernel_root_vmar
            .map(
                0,
                &operand.vmo,
                mapping_start,
                length,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .map_err(|_| errno!(EFAULT))?;
        Ok(Self { kernel_address: kernel_address as *const u8, length, value_offset })
    }

    fn value(&self) -> &AtomicU32 {
        let value_ptr = (self.kernel_address.wrapping_add(self.value_offset)) as *const AtomicU32;
        // SAFETY: This object ensures that the kernel_address remains valid for its own lifetime.
        // We also know that `value_ptr` is properly aligned because of the checks in
        // `{Private,Shared}FutexKey::get_key`.
        unsafe { &*value_ptr }
    }
}

impl Drop for FutexOperandMapping {
    fn drop(&mut self) {
        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();

        // SAFETY: This object hands out references to the mapped memory, but the borrow checker
        // ensures correct lifetimes.
        match unsafe { kernel_root_vmar.unmap(self.kernel_address as usize, self.length) } {
            Ok(()) => {}
            Err(status) => {
                log_error!("failed to unmap futex value from kernel: {:?}", status);
            }
        }
    }
}

pub trait FutexKey: Sized + Ord + Hash + Clone {
    fn get_key(task: &Task, addr: UserAddress) -> Result<Self, Errno>;

    fn get_operand_and_key(
        task: &Task,
        addr: UserAddress,
        perms: ProtectionFlags,
    ) -> Result<(FutexOperand, Self), Errno>;
}

#[derive(Debug, Clone, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct PrivateFutexKey {
    addr: UserAddress,
}

impl FutexKey for PrivateFutexKey {
    fn get_key(_task: &Task, addr: UserAddress) -> Result<Self, Errno> {
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
        Ok(PrivateFutexKey { addr })
    }

    fn get_operand_and_key(
        task: &Task,
        addr: UserAddress,
        perms: ProtectionFlags,
    ) -> Result<(FutexOperand, Self), Errno> {
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
        let (vmo, offset) = task.mm().get_mapping_vmo(addr, perms)?;
        let key = PrivateFutexKey { addr };
        Ok((FutexOperand { vmo, offset }, key))
    }
}

#[derive(Debug, Clone, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct SharedFutexKey {
    // No chance of collisions since koids are never reused:
    // https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts#kernel_object_ids
    koid: zx::Koid,
    offset: u64,
}

impl FutexKey for SharedFutexKey {
    fn get_key(task: &Task, addr: UserAddress) -> Result<Self, Errno> {
        Self::get_operand_and_key(task, addr, ProtectionFlags::READ).map(|(_, key)| key)
    }

    fn get_operand_and_key(
        task: &Task,
        addr: UserAddress,
        perms: ProtectionFlags,
    ) -> Result<(FutexOperand, Self), Errno> {
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
        let (vmo, offset) = task.mm().get_mapping_vmo(addr, perms)?;
        let key = SharedFutexKey::new(&vmo, offset)?;
        Ok((FutexOperand { vmo, offset }, key))
    }
}

impl SharedFutexKey {
    fn new(vmo: &zx::Vmo, offset: u64) -> Result<Self, Errno> {
        let koid = vmo.info().map_err(impossible_error)?.koid;
        Ok(Self { koid, offset })
    }
}

struct FutexTableState<Key: FutexKey> {
    // TODO(tbodt): Delete the wait queue from the hashmap when it becomes empty. Not doing
    // this is a memory leak.
    waiters: HashMap<Key, FutexWaiters>,
    rt_mutex_waiters: HashMap<Key, VecDeque<RtMutexWaiter>>,
}

impl<Key: FutexKey> Default for FutexTableState<Key> {
    fn default() -> Self {
        Self { waiters: Default::default(), rt_mutex_waiters: Default::default() }
    }
}

impl<Key: FutexKey> FutexTableState<Key> {
    /// Returns the FutexWaiters for a given address, creating an empty one if none is registered.
    fn get_waiters_or_default(&mut self, key: Key) -> &mut FutexWaiters {
        self.waiters.entry(key).or_default()
    }

    fn wake(&mut self, key: Key, count: usize, mask: u32) -> usize {
        let entry = self.waiters.entry(key);
        match entry {
            Entry::Vacant(_) => 0,
            Entry::Occupied(mut entry) => {
                let count = entry.get_mut().notify(mask, count);
                if entry.get().is_empty() {
                    entry.remove();
                }
                count
            }
        }
    }

    /// Returns the RT-Mutex waiters queue for a given address, creating an empty queue if none is
    /// registered.
    fn get_rt_mutex_waiters_or_default(&mut self, key: Key) -> &mut VecDeque<RtMutexWaiter> {
        self.rt_mutex_waiters.entry(key).or_default()
    }

    /// Pop the next RT-Mutex for the given address.
    fn pop_rt_mutex_waiter(&mut self, key: Key) -> Option<RtMutexWaiter> {
        let entry = self.rt_mutex_waiters.entry(key);
        match entry {
            Entry::Vacant(_) => None,
            Entry::Occupied(mut entry) => {
                if let Some(mut waiter) = entry.get_mut().pop_front() {
                    if entry.get().is_empty() {
                        entry.remove();
                    } else {
                        waiter.tid |= FUTEX_WAITERS;
                    }
                    Some(waiter)
                } else {
                    None
                }
            }
        }
    }
}

/// Abstraction over a process waiting on a Futex that can be notified.
enum FutexNotifiable {
    /// An internal process waiting on a Futex.
    Internal(Weak<InterruptibleEvent>),
    /// An external process waiting on a Futex.
    // The sender needs to be an option so that one can send the notification while only holding a
    // mut reference on the ExternalWaiter.
    External(Option<oneshot::Sender<()>>),
}

impl FutexNotifiable {
    fn new_internal(event: Weak<InterruptibleEvent>) -> Self {
        Self::Internal(event)
    }

    fn new_external(sender: oneshot::Sender<()>) -> Self {
        Self::External(Some(sender))
    }

    /// Tries to notify the process. Returns `true` is the process have been notified. Returns
    /// `false` otherwise. This means the process is stale and will never be available again.
    fn notify(&mut self) -> bool {
        match self {
            Self::Internal(event) => {
                if let Some(event) = event.upgrade() {
                    event.notify();
                    true
                } else {
                    false
                }
            }
            Self::External(ref mut sender) => {
                if let Some(sender) = sender.take() {
                    sender.send(()).is_ok()
                } else {
                    false
                }
            }
        }
    }
}

struct FutexWaiter {
    mask: u32,
    notifiable: FutexNotifiable,
}

#[derive(Default)]
struct FutexWaiters(VecDeque<FutexWaiter>);

impl FutexWaiters {
    fn add(&mut self, waiter: FutexWaiter) {
        self.0.push_back(waiter);
    }

    fn notify(&mut self, mask: u32, count: usize) -> usize {
        let mut woken = 0;
        self.0.retain_mut(|waiter| {
            if woken == count || waiter.mask & mask == 0 {
                return true;
            }
            // The send will fail if the receiver is gone, which means nothing was actualling
            // waiting on the futex.
            if waiter.notifiable.notify() {
                woken += 1;
            }
            false
        });
        woken
    }

    fn transfer(&mut self, mut other: Self) {
        self.0.append(&mut other.0);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

struct RtMutexWaiter {
    /// The tid, possibly with the FUTEX_WAITERS bit set.
    tid: u32,

    notifiable: FutexNotifiable,
}
