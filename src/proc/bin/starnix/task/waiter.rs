// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error;
use crate::fs::FdEvents;
use crate::logging::*;
use crate::types::Errno;
use crate::types::*;
use fuchsia_zircon as zx;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub type SignalHandler = Box<dyn FnOnce(zx::Signals) + Send + Sync>;
pub type EventHandler = Box<dyn FnOnce(FdEvents) + Send + Sync>;

pub enum WaitCallback {
    SignalHandler(SignalHandler),
    EventHandler(EventHandler),
}

pub struct WaitKey {
    key: u64,
}

impl WaitKey {
    /// an empty key means no associated handler
    pub fn empty() -> WaitKey {
        WaitKey { key: 0 }
    }
}

impl WaitCallback {
    pub fn none() -> EventHandler {
        Box::new(|_| {})
    }
}
/// A type that can put a thread to sleep waiting for a condition.
pub struct Waiter {
    /// The underlying Zircon port that the thread sleeps in.
    port: zx::Port,
    key_map: Mutex<HashMap<u64, WaitCallback>>, // the key 0 is reserved for 'no handler'
    next_key: AtomicU64,
}

impl Waiter {
    /// Create a new waiter object.
    pub fn new() -> Arc<Waiter> {
        Arc::new(Waiter {
            port: zx::Port::create().map_err(impossible_error).unwrap(),
            key_map: Mutex::new(HashMap::new()),
            next_key: AtomicU64::new(1),
        })
    }

    /// Wait until the waiter is woken up.
    ///
    /// If the wait is interrupted (see interrupt), this function returns
    /// EINTR.
    pub fn wait(&self) -> Result<(), Errno> {
        self.wait_until(zx::Time::INFINITE)
    }

    /// Wait until the given deadline has passed or the waiter is woken up.
    ///
    /// If the wait is interrupted (seee interrupt), this function returns
    /// EINTR.
    pub fn wait_until(&self, deadline: zx::Time) -> Result<(), Errno> {
        match self.port.wait(deadline) {
            Ok(packet) => match packet.status() {
                zx::sys::ZX_OK => {
                    let contents = packet.contents();
                    match contents {
                        zx::PacketContents::SignalOne(sigpkt) => {
                            let key = packet.key();
                            if let Some(callback) = self.key_map.lock().remove(&key) {
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
                        zx::PacketContents::User(usrpkt) => {
                            let observed = usrpkt.as_u8_array();
                            let mut mask_bytes = [0u8; 4];
                            mask_bytes[..4].copy_from_slice(&observed[..4]);
                            let events = FdEvents::from(u32::from_ne_bytes(mask_bytes));
                            let key = packet.key();
                            if let Some(callback) = self.key_map.lock().remove(&key) {
                                match callback {
                                    WaitCallback::EventHandler(handler) => {
                                        assert!(events != FdEvents::empty());
                                        handler(events)
                                    }
                                    WaitCallback::SignalHandler(_) => {
                                        panic!("wrong type of handler called")
                                    }
                                }
                            }
                        }
                        _ => {
                            return error!(EBADMSG);
                        }
                    }
                    Ok(())
                }
                // TODO make a match arm for this and return EBADMSG by default
                _ => {
                    return error!(EINTR);
                }
            },
            Err(zx::Status::TIMED_OUT) => error!(ETIMEDOUT),
            Err(errno) => Err(impossible_error(errno)),
        }
    }

    fn register_callback(&self, callback: WaitCallback) -> u64 {
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        // TODO - find a better reaction to wraparound
        assert!(key != 0, "bad key from u64 wraparound");
        assert!(
            self.key_map.lock().insert(key, callback).is_none(),
            "unexpected callback already present for key {}",
            key
        );
        key
    }

    /// Establish an asynchronous wait for the signals on the given handle,
    /// optionally running a FnOnce.
    pub fn wake_on_signals(
        &self,
        handle: &dyn zx::AsHandleRef,
        signals: zx::Signals,
        handler: SignalHandler,
    ) -> Result<(), zx::Status> {
        let callback = WaitCallback::SignalHandler(handler);
        let key = self.register_callback(callback);
        handle.wait_async_handle(&self.port, key, signals, zx::WaitAsyncOpts::empty())
    }

    pub fn wake_on_events(&self, handler: EventHandler) -> WaitKey {
        let callback = WaitCallback::EventHandler(handler);
        let key = self.register_callback(callback);
        WaitKey { key }
    }

    pub fn queue_events(&self, key: &WaitKey, event_mask: u32) -> Result<(), Errno> {
        let mut packet_data = [0u8; 32];
        packet_data[..4].copy_from_slice(&event_mask.to_ne_bytes()[..4]);

        self.queue_user_packet_data(key, zx::sys::ZX_OK, packet_data)?;
        Ok(())
    }

    /// Wake up the waiter.
    ///
    /// This function is called before the waiter goes to sleep, the waiter
    /// will wake up immediately upon attempting to go to sleep.
    pub fn wake(&self) -> Result<(), Errno> {
        self.queue_user_packet(zx::sys::ZX_OK)?;
        Ok(())
    }

    /// Interrupt the waiter.
    ///
    /// Used to break the waiter out of its sleep, for example to deliver an
    /// async signal. The wait operation will return EINTR, and unwind until
    /// the thread can process the async signal.
    #[allow(dead_code)]
    pub fn interrupt(&self) -> Result<(), Errno> {
        self.queue_user_packet(zx::sys::ZX_ERR_CANCELED)?;
        Ok(())
    }

    /// Queue a packet to the underlying Zircon port, which will cause the
    /// waiter to wake up.
    fn queue_user_packet(&self, status: i32) -> Result<(), Errno> {
        let key = WaitKey::empty();
        self.queue_user_packet_data(&key, status, [0u8; 32])?;
        Ok(())
    }

    fn queue_user_packet_data(
        &self,
        key: &WaitKey,
        status: i32,
        packet_data: [u8; 32],
    ) -> Result<(), Errno> {
        let user_packet = zx::UserPacket::from_u8_array(packet_data);
        let packet = zx::Packet::from_user_packet(key.key, status, user_packet);
        //self.port.queue(&packet).map_err(impossible_error)?;
        self.port.queue(&packet).map_err(|_| EFAULT)?;
        Ok(())
    }
}

/// A list of waiters waiting for some event.
///
/// For events that are generated inside Starnix, we walk the observer list
/// on the thread that triggered the event to notify the waiters that the event
/// has occurred. The waiters will then wake up on their own thread to handle
/// the event.
#[derive(Default)]
pub struct ObserverList {
    /// The list of observers.
    observers: Vec<Observer>,
}

/// An entry in an ObserverList.
struct Observer {
    /// The waiter that is waking for the FdEvent.
    waiter: Arc<Waiter>,

    /// The bitmask that the waiter is waiting for.
    events: u32,

    /// Whether the observer wishes to remain in the ObserverList after one of
    /// the events that the waiter is waiting for occurs.
    persistent: bool,

    /// key for cancelling and queueing events
    key: WaitKey,
}

// TODO this code should deal with FdEvents, not u32
impl ObserverList {
    /// Establish a wait for the given events.
    ///
    /// The waiter will be notified when an event matching the events mask
    /// occurs.
    ///
    /// This function does not actually block the waiter. To block the waiter,
    /// call the "wait" function on the waiter.
    pub fn wait_async(&mut self, waiter: &Arc<Waiter>, events: u32, handler: EventHandler) {
        let key = waiter.wake_on_events(handler);
        self.observers.push(Observer {
            waiter: Arc::clone(waiter),
            events: events,
            persistent: false,
            key,
        });
    }

    /// Notify any observers that the given events have occurred.
    ///
    /// Walks the observer list and wakes each observer that is waiting on an
    /// event that matches the given mask. Persistent observers remain in the
    /// list. Non-persistent observers are removed.
    ///
    /// The waiters will wake up on their own threads to handle these events.
    /// They are not called synchronously by this function.
    pub fn notify(&mut self, events: u32, mut limit: usize) {
        self.observers = std::mem::take(&mut self.observers)
            .into_iter()
            .filter(|observer| {
                if limit > 0 && (observer.events & events) != 0 {
                    observer.waiter.queue_events(&observer.key, events).unwrap();
                    limit -= 1;
                    return observer.persistent;
                }
                return true;
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::fuchsia::*;
    use crate::fs::pipe::new_pipe;
    use crate::fs::FdEvents;
    use crate::syscalls::SyscallContext;
    use crate::types::UserBuffer;
    use fuchsia_async as fasync;

    use crate::testing::*;

    static INIT_VAL: u64 = 0;
    static FINAL_VAL: u64 = 42;

    #[fasync::run_singlethreaded(test)]
    async fn test_async_wait_exec() {
        static COUNTER: AtomicU64 = AtomicU64::new(INIT_VAL);
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);

        let (kernel, task_owner) = create_kernel_and_task();
        let task = &task_owner.task;
        let ctx = SyscallContext::new(&task_owner.task);
        let (local_socket, remote_socket) = zx::Socket::create(zx::SocketOpts::STREAM).unwrap();
        let pipe = create_fuchsia_pipe(&kernel, remote_socket).unwrap();

        const MEM_SIZE: usize = 1024;
        let proc_mem = map_memory(&ctx, UserAddress::default(), MEM_SIZE as u64);
        let proc_read_buf = [UserBuffer { address: proc_mem, length: MEM_SIZE }];

        let test_string = "hello startnix".to_string();
        let report_packet: EventHandler = Box::new(|observed: FdEvents| {
            assert!(FdEvents::POLLIN & observed);
            COUNTER.store(FINAL_VAL, Ordering::Relaxed);
        });
        let waiter = Waiter::new();
        pipe.wait_async(&waiter, FdEvents::POLLIN, report_packet);
        let test_string_clone = test_string.clone();

        let thread = std::thread::spawn(move || {
            let test_data = test_string_clone.as_bytes();
            let no_written = local_socket.write(&test_data).unwrap();
            assert_eq!(0, WRITE_COUNT.fetch_add(no_written as u64, Ordering::Relaxed));
            assert_eq!(no_written, test_data.len());
        });

        // this code would block on failure
        assert_eq!(INIT_VAL, COUNTER.load(Ordering::Relaxed));
        waiter.wait().unwrap();
        let _ = thread.join();
        assert_eq!(FINAL_VAL, COUNTER.load(Ordering::Relaxed));

        let read_size = pipe.read(&task, &proc_read_buf).unwrap();
        let mut read_mem = [0u8; MEM_SIZE];
        task.mm.read_all(&proc_read_buf, &mut read_mem).unwrap();

        let no_written = WRITE_COUNT.load(Ordering::Relaxed);
        assert_eq!(no_written, read_size as u64);

        let read_mem_valid = &read_mem[0..read_size];
        assert_eq!(*&read_mem_valid, test_string.as_bytes());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_async_wait_fdevent() {
        static COUNTER: AtomicU64 = AtomicU64::new(INIT_VAL);
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);

        let (kernel, task_owner) = create_kernel_and_task();
        let task = &task_owner.task;
        let ctx = SyscallContext::new(&task_owner.task);
        let (pipe_out, pipe_in) = new_pipe(&kernel).unwrap();

        let test_string = "hello startnix".to_string();
        let test_bytes = test_string.as_bytes();
        let test_len = test_bytes.len();
        let read_mem = map_memory(&ctx, UserAddress::default(), test_len as u64);
        let read_buf = [UserBuffer { address: read_mem, length: test_len }];
        let write_mem = map_memory(&ctx, UserAddress::default(), test_len as u64);
        let write_buf = [UserBuffer { address: write_mem, length: test_len }];

        task.mm.write_memory(write_mem, test_bytes).unwrap();

        let waiter = Waiter::new();
        let watched_events = FdEvents::POLLIN;
        let report_packet = move |observed: FdEvents| {
            assert!(observed & watched_events);
            COUNTER.store(FINAL_VAL, Ordering::Relaxed);
        };
        pipe_out.wait_async(&waiter, watched_events, Box::new(report_packet));

        let task_clone = task.clone();
        let thread = std::thread::spawn(move || {
            let no_written = pipe_in.write(&task_clone, &write_buf).unwrap();
            assert_eq!(no_written, test_len);
            WRITE_COUNT.fetch_add(no_written as u64, Ordering::Relaxed);
        });

        // this code would block on failure
        assert_eq!(INIT_VAL, COUNTER.load(Ordering::Relaxed));
        waiter.wait().unwrap();
        let _ = thread.join();
        assert_eq!(FINAL_VAL, COUNTER.load(Ordering::Relaxed));

        let no_read = pipe_out.read(task, &read_buf).unwrap();
        assert_eq!(no_read as u64, WRITE_COUNT.load(Ordering::Relaxed));
        assert_eq!(no_read, test_len);
        let mut read_data = vec![0u8, test_len as u8];
        task.mm.read_memory(read_buf[0].address, &mut read_data).unwrap();
        for (test_out, test_ref) in read_data.iter().zip(test_bytes) {
            assert_eq!(*test_out, *test_ref);
        }
    }

    #[test]
    fn test_observer_list() {
        let mut list = ObserverList::default();

        let waiter0 = Waiter::new();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        list.wait_async(&waiter0, 1, WaitCallback::none());
        list.wait_async(&waiter1, 1, WaitCallback::none());
        list.wait_async(&waiter2, 1, WaitCallback::none());

        list.notify(1, 2);
        assert!(waiter0.wait_until(zx::Time::ZERO).is_ok());
        assert!(waiter1.wait_until(zx::Time::ZERO).is_ok());
        assert!(waiter2.wait_until(zx::Time::ZERO).is_err());

        list.notify(1, usize::MAX);
        assert!(waiter0.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(zx::Time::ZERO).is_ok());

        list.notify(1, 3);
        assert!(waiter0.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(zx::Time::ZERO).is_err());
    }

    #[test]
    fn test_observer_list_mask() {
        let mut list = ObserverList::default();

        let waiter0 = Waiter::new();
        let waiter1 = Waiter::new();
        let waiter2 = Waiter::new();

        list.wait_async(&waiter0, 0x13, WaitCallback::none());
        list.wait_async(&waiter1, 0x11, WaitCallback::none());
        list.wait_async(&waiter2, 0x12, WaitCallback::none());

        list.notify(0x2, 2);
        assert!(waiter0.wait_until(zx::Time::ZERO).is_ok());
        assert!(waiter1.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter2.wait_until(zx::Time::ZERO).is_ok());

        list.notify(0x1, usize::MAX);
        assert!(waiter0.wait_until(zx::Time::ZERO).is_err());
        assert!(waiter1.wait_until(zx::Time::ZERO).is_ok());
        assert!(waiter2.wait_until(zx::Time::ZERO).is_err());
    }
}
