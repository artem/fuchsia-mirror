// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use starnix_sync::{EventWaitGuard, InterruptibleEvent};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};

use crate::{
    fs::FdEvents,
    lock::Mutex,
    logging::*,
    signals::RunState,
    task::*,
    types::{Errno, *},
};

pub type SignalHandler = Box<dyn FnOnce(zx::Signals) + Send + Sync>;
pub type EventHandler = Box<dyn FnOnce(FdEvents) + Send + Sync>;

pub enum WaitCallback {
    SignalHandler(SignalHandler),
    EventHandler(EventHandler),
}

/// Return values for wait_async methods. Calling `cancel` will cancel any running wait.
pub struct WaitCanceler {
    canceler: Box<dyn Fn() + Send + Sync>,
}

impl WaitCanceler {
    pub fn new<F>(canceler: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        Self { canceler: Box::new(canceler) }
    }

    /// Cancel the pending wait. It is valid to call this function multiple times.
    pub fn cancel(&self) {
        (self.canceler)();
    }
}

/// Return values for wait_async methods that monitor the state of a handle. Calling `cancel` will
/// cancel any running wait.
pub struct HandleWaitCanceler {
    canceler: Box<dyn Fn(zx::HandleRef<'_>) + Send + Sync>,
}

impl HandleWaitCanceler {
    pub fn new<F>(canceler: F) -> Self
    where
        F: Fn(zx::HandleRef<'_>) + Send + Sync + 'static,
    {
        Self { canceler: Box::new(canceler) }
    }

    /// Cancel the pending wait. It is valid to call this function multiple times.
    pub fn cancel(&self, handle: zx::HandleRef<'_>) {
        (self.canceler)(handle);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct WaitKey {
    raw: u64,
}

/// The different type of event that can be waited on / triggered.
#[derive(Clone, Copy, Debug)]
enum WaitEvents {
    /// All event: a wait on `All` will be woken up by all event, and a trigger on `All` will wake
    /// every waiter.
    All,
    /// Wait on the set of FdEvents.
    Fd(FdEvents),
    /// Wait for the specified value.
    Value(u64),
}

impl WaitEvents {
    /// Returns whether a wait on `self` should be woken up by `other`.
    fn intercept(self: &WaitEvents, other: &WaitEvents) -> bool {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => true,
            (Self::Fd(m1), Self::Fd(m2)) => m1.bits() & m2.bits() != 0,
            (Self::Value(v1), Self::Value(v2)) => v1 == v2,
            _ => false,
        }
    }
}

impl WaitCallback {
    pub fn none() -> EventHandler {
        Box::new(|_| {})
    }
}

#[derive(Debug)]
struct UserEvent {
    key: WaitKey,
    events: WaitEvents,
}

/// Implementation of Waiter. We put the Waiter data in an Arc so that WaitQueue can tell when the
/// Waiter has been destroyed by keeping a Weak reference. But this is an implementation detail and
/// a Waiter should have a single owner. So the Arc is hidden inside Waiter.
struct PortWaiter {
    /// The underlying Zircon port that the thread sleeps in.
    port: zx::Port,
    callbacks: Mutex<HashMap<WaitKey, WaitCallback>>, // the key 0 is reserved for 'no handler'
    next_key: AtomicU64,
    ignore_signals: bool,

    /// Collection of wait queues this Waiter is waiting on, so that when the Waiter is Dropped it
    /// can remove itself from the queues.
    ///
    /// This lock is nested inside the WaitQueue.waiters lock.
    wait_queues: Mutex<HashMap<WaitKey, Weak<WaitQueueImpl>>>,
    /// A queue of events triggered within starnix.
    ///
    /// Events triggered by the Fuchsia platform (e.g. network sockets, filesystem)
    /// are only made available through non-user packets on the port.
    user_events: Mutex<Vec<UserEvent>>,
}

impl PortWaiter {
    /// Internal constructor.
    fn new(ignore_signals: bool) -> Arc<Self> {
        profile_duration!("NewPortWaiter");
        Arc::new(PortWaiter {
            port: zx::Port::create(),
            callbacks: Default::default(),
            next_key: AtomicU64::new(1),
            ignore_signals,
            wait_queues: Default::default(),
            user_events: Default::default(),
        })
    }

    /// Waits until the given deadline has passed or the waiter is woken up. See wait_until().
    fn wait_internal(&self, deadline: zx::Time) -> Result<(), Errno> {
        // This method can block arbitrarily long, possibly waiting for another process. The
        // current thread should not own any local ref that might delay the release of a resource
        // while doing so.
        debug_assert_no_local_temp_ref();

        match self.port.wait(deadline) {
            Ok(packet) => match packet.status() {
                zx::sys::ZX_OK => {
                    let contents = packet.contents();
                    match contents {
                        zx::PacketContents::SignalOne(sigpkt) => {
                            let key = WaitKey { raw: packet.key() };
                            if let Some(callback) = self.remove_callback(&key) {
                                match callback {
                                    WaitCallback::SignalHandler(handler) => {
                                        handler(sigpkt.observed())
                                    }
                                    WaitCallback::EventHandler(_) => {
                                        panic!("wrong type of handler called")
                                    }
                                }
                            }
                        }
                        zx::PacketContents::User(_) => {
                            // User packet w/ OK status is only used to wake up
                            // the waiter and have it process enqueued user events.

                            // `take` the queue of user events so that we do not
                            // contend with threads trying to add to the queue
                            // while handling what is currently available.
                            let user_events = std::mem::take(&mut *self.user_events.lock());
                            assert!(!user_events.is_empty(), "OK user packet should only be enqueued when there is a pending user event");

                            for UserEvent { key, events } in user_events {
                                let Some(callback) = self.remove_callback(&key) else {
                                    continue;
                                };

                                match callback {
                                    WaitCallback::EventHandler(handler) => {
                                        let fd_events = match events {
                                            // If the event is All, signal on all possible fd
                                            // events.
                                            WaitEvents::All => FdEvents::all(),
                                            WaitEvents::Fd(events) => events,
                                            _ => panic!("wrong type of handler called: {events:?}"),
                                        };
                                        handler(fd_events)
                                    }
                                    WaitCallback::SignalHandler(_) => {
                                        panic!("wrong type of handler called")
                                    }
                                }
                            }
                        }
                        _ => return error!(EBADMSG),
                    }
                    Ok(())
                }
                zx::sys::ZX_ERR_CANCELED => error!(EINTR),
                _ => {
                    debug_assert!(false, "Unexpected status in port wait {}", packet.status());
                    error!(EBADMSG)
                }
            },
            Err(zx::Status::TIMED_OUT) => error!(ETIMEDOUT),
            Err(errno) => Err(impossible_error(errno)),
        }
    }

    fn wait_until(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        deadline: zx::Time,
    ) -> Result<(), Errno> {
        profile_duration!("WaiterWaitUntil");
        let is_waiting = deadline.into_nanos() > 0;

        let callback = || {
            // We are susceptible to spurious wakeups because interrupt() posts a message to the port
            // queue. In addition to more subtle races, there could already be valid messages in the
            // port queue that will immediately wake us up, leaving the interrupt message in the queue
            // for subsequent waits (which by then may not have any signals pending) to read.
            //
            // It's impossible to non-racily guarantee that a signal is pending so there might always
            // be an EINTR result here with no signal. But any signal we get when !is_waiting we know is
            // leftover from before: the top of this function only sets ourself as the
            // current_task.signals.run_state when there's a nonzero timeout, and that waiter reference
            // is what is used to signal the interrupt().
            loop {
                let wait_result = self.wait_internal(deadline);
                if let Err(errno) = &wait_result {
                    if errno.code == EINTR && !is_waiting {
                        continue; // Spurious wakeup.
                    }
                }
                return wait_result;
            }
        };

        if is_waiting {
            current_task.run_in_state(RunState::Waiter(WaiterRef::from_port(self)), callback)
        } else {
            callback()
        }
    }

    fn next_key(&self) -> WaitKey {
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        // TODO - find a better reaction to wraparound
        assert!(key != 0, "bad key from u64 wraparound");
        WaitKey { raw: key }
    }

    fn register_callback(&self, callback: WaitCallback) -> WaitKey {
        let key = self.next_key();
        assert!(
            self.callbacks.lock().insert(key, callback).is_none(),
            "unexpected callback already present for key {key:?}"
        );
        key
    }

    fn remove_callback(&self, key: &WaitKey) -> Option<WaitCallback> {
        self.callbacks.lock().remove(&key)
    }

    fn wake_immediately(&self, events: FdEvents, handler: EventHandler) {
        let callback = WaitCallback::EventHandler(handler);
        let key = self.register_callback(callback);
        self.queue_events(&key, WaitEvents::Fd(events));
    }

    /// Establish an asynchronous wait for the signals on the given Zircon handle (not to be
    /// confused with POSIX signals), optionally running a FnOnce.
    ///
    /// Returns a `HandleWaitCanceler` that can be used to cancel the wait.
    fn wake_on_zircon_signals(
        self: &Arc<Self>,
        handle: &dyn zx::AsHandleRef,
        zx_signals: zx::Signals,
        handler: SignalHandler,
    ) -> Result<HandleWaitCanceler, zx::Status> {
        let callback = WaitCallback::SignalHandler(handler);
        let key = self.register_callback(callback);
        handle.wait_async_handle(
            &self.port,
            key.raw,
            zx_signals,
            zx::WaitAsyncOpts::EDGE_TRIGGERED,
        )?;
        let weak_self = Arc::downgrade(self);
        Ok(HandleWaitCanceler::new(move |handle_ref| {
            if let Some(waiter) = weak_self.upgrade() {
                let _ = waiter.port.cancel(&handle_ref, key.raw);
                waiter.remove_callback(&key);
            }
        }))
    }

    fn queue_events(&self, key: &WaitKey, events: WaitEvents) {
        let key = key.clone();

        // Putting these events into their own queue breaks any ordering
        // expectations on Linux by batching all starnix events with the first
        // starnix event even if other events occur on the Fuchsia platform (and
        // are enqueued to the `zx::Port`) between them. This ordering does not
        // seem to be load-bearing for applications running on starnix so we
        // take the divergence in ordering in favour of improved performance (by
        // minimizing syscalls) when operating on FDs backed by starnix.
        //
        // TODO(https://fxbug.dev/134622): If we can read a batch of packets
        // from the `zx::Port`, maybe we can keep the ordering?
        let enqueue_packet = {
            let mut user_events = self.user_events.lock();
            let was_empty = user_events.is_empty();
            user_events.push(UserEvent { key, events });
            // Only enqueue a user packet on the first enqueued user event.
            was_empty
        };

        if enqueue_packet {
            self.queue_user_packet_data(zx::sys::ZX_OK)
        }
    }

    /// Queue a packet to the underlying Zircon port, which will cause the
    /// waiter to wake up.
    fn queue_user_packet_data(&self, status: i32) {
        let packet = zx::Packet::from_user_packet(0, status, zx::UserPacket::default());
        self.port.queue(&packet).map_err(impossible_error).unwrap();
    }

    fn interrupt(&self) {
        if self.ignore_signals {
            return;
        }
        self.queue_user_packet_data(zx::sys::ZX_ERR_CANCELED);
    }
}

impl std::fmt::Debug for PortWaiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortWaiter").field("port", &self.port).finish_non_exhaustive()
    }
}

/// A type that can put a thread to sleep waiting for a condition.
#[derive(Debug)]
pub struct Waiter {
    inner: Arc<PortWaiter>,
}

impl Waiter {
    /// Create a new waiter.
    pub fn new() -> Self {
        Self { inner: PortWaiter::new(false) }
    }

    /// Create a new waiter that doesn't wake up when a signal is received.
    pub fn new_ignoring_signals() -> Self {
        Self { inner: PortWaiter::new(true) }
    }

    /// Create a weak reference to this waiter.
    fn weak(&self) -> WaiterRef {
        WaiterRef::from_port(&self.inner)
    }

    /// Wait until the waiter is woken up.
    ///
    /// If the wait is interrupted (see [`Waiter::interrupt`]), this function returns EINTR.
    pub fn wait(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.inner.wait_until(current_task, zx::Time::INFINITE)
    }

    /// Wait until the given deadline has passed or the waiter is woken up.
    ///
    /// If the wait deadline is nonzero and is interrupted (see [`Waiter::interrupt`]), this
    /// function returns EINTR. Callers must take special care not to lose any accumulated data or
    /// local state when EINTR is received as this is a normal and recoverable situation.
    ///
    /// Using a 0 deadline (no waiting, useful for draining pending events) will not wait and is
    /// guaranteed not to issue EINTR.
    ///
    /// It the timeout elapses with no events, this function returns ETIMEDOUT.
    ///
    /// Processes at most one event. If the caller is interested in draining the events, it should
    /// repeatedly call this function with a 0 deadline until it reports ETIMEDOUT. (This case is
    /// why a 0 deadline must not return EINTR, as previous calls to wait_until() may have
    /// accumulated state that would be lost when returning EINTR to userspace.)
    pub fn wait_until(&self, current_task: &CurrentTask, deadline: zx::Time) -> Result<(), Errno> {
        self.inner.wait_until(current_task, deadline)
    }

    fn create_wait_entry(&self, filter: WaitEvents) -> WaitEntry {
        WaitEntry { waiter: self.weak(), filter, key: self.inner.next_key() }
    }

    fn create_wait_entry_with_handler(
        &self,
        filter: WaitEvents,
        handler: EventHandler,
    ) -> WaitEntry {
        let key = self.inner.register_callback(WaitCallback::EventHandler(handler));
        WaitEntry { waiter: self.weak(), filter, key }
    }

    pub fn wake_immediately(&self, events: FdEvents, handler: EventHandler) {
        self.inner.wake_immediately(events, handler);
    }

    /// Establish an asynchronous wait for the signals on the given Zircon handle (not to be
    /// confused with POSIX signals), optionally running a FnOnce.
    ///
    /// Returns a `HandleWaitCanceler` that can be used to cancel the wait.
    pub fn wake_on_zircon_signals(
        &self,
        handle: &dyn zx::AsHandleRef,
        zx_signals: zx::Signals,
        handler: SignalHandler,
    ) -> Result<HandleWaitCanceler, zx::Status> {
        self.inner.wake_on_zircon_signals(handle, zx_signals, handler)
    }

    /// Return a WaitCanceler representing a wait that will never complete. Useful for stub
    /// implementations that should block forever even though a real implementation would wake up
    /// eventually.
    pub fn fake_wait(&self) -> WaitCanceler {
        WaitCanceler::new(move || {})
    }

    /// Interrupt the waiter to deliver a signal. The wait operation will return EINTR, and a
    /// typical caller should then unwind to the syscall dispatch loop to let the signal be
    /// processed. See wait_until() for more details.
    ///
    /// Ignored if the waiter was created with new_ignoring_signals().
    pub fn interrupt(&self) {
        self.inner.interrupt();
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        // Delete ourselves from each wait queue we know we're on to prevent Weak references to
        // ourself from sticking around forever.
        let wait_queues = std::mem::take(&mut *self.inner.wait_queues.lock()).into_values();
        for wait_queue in wait_queues {
            if let Some(wait_queue) = wait_queue.upgrade() {
                wait_queue.waiters.lock().retain(|entry| entry.waiter != *self)
            }
        }
    }
}

impl Default for Waiter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SimpleWaiter {
    event: Arc<InterruptibleEvent>,
    wait_queues: Vec<Weak<WaitQueueImpl>>,
}

impl SimpleWaiter {
    pub fn new(event: &Arc<InterruptibleEvent>) -> (SimpleWaiter, EventWaitGuard<'_>) {
        (SimpleWaiter { event: event.clone(), wait_queues: Default::default() }, event.begin_wait())
    }
}

impl Drop for SimpleWaiter {
    fn drop(&mut self) {
        for wait_queue in &self.wait_queues {
            if let Some(wait_queue) = wait_queue.upgrade() {
                wait_queue.waiters.lock().retain(|entry| entry.waiter != self.event)
            }
        }
    }
}

#[derive(Debug, Clone)]
enum WaiterKind {
    Port(Weak<PortWaiter>),
    Event(Weak<InterruptibleEvent>),
}

impl Default for WaiterKind {
    fn default() -> Self {
        WaiterKind::Port(Default::default())
    }
}

/// A weak reference to a Waiter. Intended for holding in wait queues or stashing elsewhere for
/// calling queue_events later.
#[derive(Debug, Default, Clone)]
pub struct WaiterRef(WaiterKind);

impl WaiterRef {
    fn from_port(waiter: &Arc<PortWaiter>) -> WaiterRef {
        WaiterRef(WaiterKind::Port(Arc::downgrade(waiter)))
    }

    fn from_event(event: &Arc<InterruptibleEvent>) -> WaiterRef {
        WaiterRef(WaiterKind::Event(Arc::downgrade(event)))
    }

    pub fn is_valid(&self) -> bool {
        match &self.0 {
            WaiterKind::Port(waiter) => waiter.strong_count() != 0,
            WaiterKind::Event(event) => event.strong_count() != 0,
        }
    }

    pub fn interrupt(&self) {
        match &self.0 {
            WaiterKind::Port(waiter) => {
                if let Some(waiter) = waiter.upgrade() {
                    waiter.interrupt();
                }
            }
            WaiterKind::Event(event) => {
                if let Some(event) = event.upgrade() {
                    event.interrupt();
                }
            }
        }
    }

    fn remove_callback(&self, key: &WaitKey) {
        match &self.0 {
            WaiterKind::Port(waiter) => {
                if let Some(waiter) = waiter.upgrade() {
                    waiter.remove_callback(key);
                }
            }
            _ => (),
        }
    }

    /// Called by the WaitQueue when this waiter is about to be removed from the queue.
    ///
    /// TODO(abarth): This function does not appear to be called when the WaitQueue is dropped,
    /// which appears to be a leak.
    fn will_remove_from_wait_queue(&self, key: &WaitKey) {
        match &self.0 {
            WaiterKind::Port(waiter) => {
                if let Some(waiter) = waiter.upgrade() {
                    waiter.wait_queues.lock().remove(key);
                }
            }
            _ => (),
        }
    }

    /// Notify the waiter that the `events` have occurred.
    ///
    /// If the client is using an `SimpleWaiter`, they will be notified but they will not learn
    /// which events occurred.
    fn notify(&self, key: &WaitKey, events: WaitEvents) -> bool {
        match &self.0 {
            WaiterKind::Port(waiter) => {
                if let Some(waiter) = waiter.upgrade() {
                    waiter.queue_events(key, events);
                    return true;
                }
            }
            WaiterKind::Event(event) => {
                if let Some(event) = event.upgrade() {
                    event.notify();
                    return true;
                }
            }
        }
        false
    }
}

impl PartialEq<Waiter> for WaiterRef {
    fn eq(&self, other: &Waiter) -> bool {
        match &self.0 {
            WaiterKind::Port(waiter) => waiter.as_ptr() == Arc::as_ptr(&other.inner),
            _ => false,
        }
    }
}

impl PartialEq<Arc<InterruptibleEvent>> for WaiterRef {
    fn eq(&self, other: &Arc<InterruptibleEvent>) -> bool {
        match &self.0 {
            WaiterKind::Event(event) => event.as_ptr() == Arc::as_ptr(other),
            _ => false,
        }
    }
}

impl PartialEq for WaiterRef {
    fn eq(&self, other: &WaiterRef) -> bool {
        match (&self.0, &other.0) {
            (WaiterKind::Port(lhs), WaiterKind::Port(rhs)) => Weak::ptr_eq(lhs, rhs),
            (WaiterKind::Event(lhs), WaiterKind::Event(rhs)) => Weak::ptr_eq(lhs, rhs),
            _ => false,
        }
    }
}

/// A list of waiters waiting for some event.
///
/// For events that are generated inside Starnix, we walk the wait queue
/// on the thread that triggered the event to notify the waiters that the event
/// has occurred. The waiters will then wake up on their own thread to handle
/// the event.
#[derive(Default, Debug)]
pub struct WaitQueue(Arc<WaitQueueImpl>);

#[derive(Default, Debug)]
struct WaitQueueImpl {
    /// The list of waiters.
    ///
    /// The WaiterImpl.wait_queues lock is nested inside this lock.
    waiters: Mutex<Vec<WaitEntry>>,
}

/// An entry in a WaitQueue.
#[derive(Debug)]
struct WaitEntry {
    /// The waiter that is waking for the FdEvent.
    waiter: WaiterRef,

    /// The events that the waiter is waiting for.
    filter: WaitEvents,

    /// key for cancelling and queueing events
    key: WaitKey,
}

impl WaitQueue {
    /// Establish a wait for the given entry.
    ///
    /// The waiter will be notified when an event matching the entry occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    fn wait_async_entry(&self, waiter: &Waiter, entry: WaitEntry) -> WaitCanceler {
        profile_duration!("WaitAsyncEntry");
        let key = entry.key;
        self.0.waiters.lock().push(entry);
        let weak_self = Arc::downgrade(&self.0);
        waiter.inner.wait_queues.lock().insert(key, weak_self.clone());
        let waiter = waiter.weak();
        WaitCanceler::new(move || {
            if let Some(wait_queue) = weak_self.upgrade() {
                waiter.remove_callback(&key);
                // TODO(steveaustin) Maybe make waiters a map to avoid linear search
                Self::filter_waiters(&mut wait_queue.waiters.lock(), |entry| {
                    if entry.key == key && entry.waiter == waiter {
                        Retention::Drop
                    } else {
                        Retention::Keep
                    }
                });
            }
        })
    }

    /// Establish a wait for the given value event.
    ///
    /// The waiter will be notified when an event with the same value occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async_value(&self, waiter: &Waiter, value: u64) -> WaitCanceler {
        self.wait_async_entry(waiter, waiter.create_wait_entry(WaitEvents::Value(value)))
    }

    /// Establish a wait for the given FdEvents.
    ///
    /// The waiter will be notified when an event matching the `events` occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async_fd_events(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        let entry = waiter.create_wait_entry_with_handler(WaitEvents::Fd(events), handler);
        self.wait_async_entry(waiter, entry)
    }

    /// Establish a wait for any event.
    ///
    /// The waiter will be notified when any event occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the [`Waiter::wait`] function on the waiter.
    ///
    /// Returns a `WaitCanceler` that can be used to cancel the wait.
    pub fn wait_async(&self, waiter: &Waiter) -> WaitCanceler {
        self.wait_async_entry(waiter, waiter.create_wait_entry(WaitEvents::All))
    }

    pub fn wait_async_simple(&self, waiter: &mut SimpleWaiter) {
        let entry = WaitEntry {
            waiter: WaiterRef::from_event(&waiter.event),
            filter: WaitEvents::All,
            key: Default::default(),
        };
        waiter.wait_queues.push(Arc::downgrade(&self.0));
        self.0.waiters.lock().push(entry);
    }

    fn notify_events_count(&self, events: WaitEvents, mut limit: usize) -> usize {
        profile_duration!("NotifyEventsCount");
        let mut woken = 0;
        Self::filter_waiters(&mut self.0.waiters.lock(), |entry| {
            if limit > 0 && entry.filter.intercept(&events) {
                if entry.waiter.notify(&entry.key, events) {
                    limit -= 1;
                    woken += 1;
                }
                return Retention::Drop;
            }
            Retention::Keep
        });
        woken
    }

    pub fn notify_fd_events(&self, events: FdEvents) {
        self.notify_events_count(WaitEvents::Fd(events), usize::MAX);
    }

    pub fn notify_value(&self, value: u64) {
        self.notify_events_count(WaitEvents::Value(value), usize::MAX);
    }

    pub fn notify_count(&self, limit: usize) {
        self.notify_events_count(WaitEvents::All, limit);
    }

    pub fn notify_all(&self) {
        self.notify_count(usize::MAX);
    }

    /// Returns whether there is no active waiters waiting on this `WaitQueue`.
    pub fn is_empty(&self) -> bool {
        let mut waiters = self.0.waiters.lock();
        Self::filter_waiters(&mut waiters, |entry| {
            if entry.waiter.is_valid() {
                Retention::Keep
            } else {
                Retention::Drop
            }
        });
        waiters.is_empty()
    }

    fn filter_waiters(
        waiters: &mut Vec<WaitEntry>,
        mut filter: impl FnMut(&WaitEntry) -> Retention,
    ) {
        waiters.retain(move |entry| match filter(entry) {
            Retention::Keep => true,
            Retention::Drop => {
                entry.waiter.will_remove_from_wait_queue(&entry.key);
                false
            }
        })
    }
}

enum Retention {
    Drop,
    Keep,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fs::{
            buffers::{VecInputBuffer, VecOutputBuffer},
            fuchsia::*,
            new_eventfd, EventFdType, FdEvents,
        },
        testing::*,
    };
    use std::sync::atomic::AtomicU64;

    static INIT_VAL: u64 = 0;
    static FINAL_VAL: u64 = 42;

    #[::fuchsia::test]
    async fn test_async_wait_exec() {
        static COUNTER: AtomicU64 = AtomicU64::new(INIT_VAL);
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);

        let (_kernel, current_task) = create_kernel_and_task();
        let (local_socket, remote_socket) = zx::Socket::create_stream();
        let pipe = create_fuchsia_pipe(&current_task, remote_socket, OpenFlags::RDWR).unwrap();

        const MEM_SIZE: usize = 1024;
        let mut output_buffer = VecOutputBuffer::new(MEM_SIZE);

        let test_string = "hello startnix".to_string();
        let report_packet: EventHandler = Box::new(|observed: FdEvents| {
            assert!(observed.contains(FdEvents::POLLIN));
            COUNTER.store(FINAL_VAL, Ordering::Relaxed);
        });
        let waiter = Waiter::new();
        pipe.wait_async(&current_task, &waiter, FdEvents::POLLIN, report_packet)
            .expect("wait_async");
        let test_string_clone = test_string.clone();

        let thread = std::thread::spawn(move || {
            let test_data = test_string_clone.as_bytes();
            let no_written = local_socket.write(test_data).unwrap();
            assert_eq!(0, WRITE_COUNT.fetch_add(no_written as u64, Ordering::Relaxed));
            assert_eq!(no_written, test_data.len());
        });

        // this code would block on failure
        assert_eq!(INIT_VAL, COUNTER.load(Ordering::Relaxed));
        waiter.wait(&current_task).unwrap();
        let _ = thread.join();
        assert_eq!(FINAL_VAL, COUNTER.load(Ordering::Relaxed));

        let read_size = pipe.read(&current_task, &mut output_buffer).unwrap();

        let no_written = WRITE_COUNT.load(Ordering::Relaxed);
        assert_eq!(no_written, read_size as u64);

        assert_eq!(output_buffer.data(), test_string.as_bytes());
    }

    #[::fuchsia::test]
    async fn test_async_wait_cancel() {
        for do_cancel in [true, false] {
            let (_kernel, current_task) = create_kernel_and_task();
            let event = new_eventfd(&current_task, 0, EventFdType::Counter, true);
            let waiter = Waiter::new();
            let callback_count = Arc::new(AtomicU64::new(0));
            let callback_count_clone = callback_count.clone();
            let handler = move |_observed: FdEvents| {
                callback_count_clone.fetch_add(1, Ordering::Relaxed);
            };
            let wait_canceler = event
                .wait_async(&current_task, &waiter, FdEvents::POLLIN, Box::new(handler))
                .expect("wait_async");
            if do_cancel {
                wait_canceler.cancel();
            }
            let add_val = 1u64;
            assert_eq!(
                event
                    .write(&current_task, &mut VecInputBuffer::new(&add_val.to_ne_bytes()))
                    .unwrap(),
                std::mem::size_of::<u64>()
            );

            let wait_result = waiter.wait_until(&current_task, zx::Time::ZERO);
            let final_count = callback_count.load(Ordering::Relaxed);
            if do_cancel {
                assert_eq!(wait_result, error!(ETIMEDOUT));
                assert_eq!(0, final_count);
            } else {
                assert_eq!(wait_result, Ok(()));
                assert_eq!(1, final_count);
            }
        }
    }

    #[::fuchsia::test]
    async fn single_waiter_multiple_waits_cancel_one_waiter_still_notified() {
        let (_kernel, current_task) = create_kernel_and_task();
        let wait_queue = WaitQueue::default();
        let waiter = Waiter::new();
        let wk1 = wait_queue.wait_async(&waiter);
        let _wk2 = wait_queue.wait_async(&waiter);
        wk1.cancel();
        wait_queue.notify_all();
        assert!(waiter.wait_until(&current_task, zx::Time::ZERO).is_ok());
    }

    #[::fuchsia::test]
    async fn multiple_waiters_cancel_one_other_still_notified() {
        let (_kernel, current_task) = create_kernel_and_task();
        let wait_queue = WaitQueue::default();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();
        let wk1 = wait_queue.wait_async(&waiter1);
        let _wk2 = wait_queue.wait_async(&waiter2);
        wk1.cancel();
        wait_queue.notify_all();
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_ok());
    }

    #[::fuchsia::test]
    async fn test_wait_queue() {
        let (_kernel, current_task) = create_kernel_and_task();
        let queue = WaitQueue::default();

        let waiter0 = Waiter::new();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        queue.wait_async(&waiter0);
        queue.wait_async(&waiter1);
        queue.wait_async(&waiter2);

        queue.notify_count(2);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_ok());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_err());

        queue.notify_all();
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_ok());

        queue.notify_count(3);
        assert!(waiter0.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(&current_task, zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(&current_task, zx::Time::ZERO).is_err());
    }
}
