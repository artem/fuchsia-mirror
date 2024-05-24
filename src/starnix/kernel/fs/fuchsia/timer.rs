// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    task::{
        CurrentTask, EventHandler, HandleWaitCanceler, HrTimer, SignalHandler, SignalHandlerInner,
        WaitCanceler, Waiter,
    },
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, Anon, FileHandle, FileObject, FileOps,
    },
};
use fuchsia_zircon::{self as zx, AsHandleRef, HandleRef};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, Mutex, WriteOps};
use starnix_uapi::{
    error,
    errors::Errno,
    from_status_like_fdio, itimerspec,
    open_flags::OpenFlags,
    time::{
        duration_from_timespec, itimerspec_from_deadline_interval, time_from_timespec,
        timespec_from_duration, timespec_is_zero,
    },
    vfs::FdEvents,
    TFD_TIMER_ABSTIME,
};
use std::sync::Arc;
use zerocopy::AsBytes;

pub trait TimerOps: Send + Sync + 'static {
    /// Starts the timer with the specified `deadline`.
    ///
    /// This method should start the timer and schedule it to trigger at the specified `deadline`.
    /// The timer should be cancelled if it is already running.
    fn start(&self, current_task: &CurrentTask, deadline: zx::Time) -> Result<(), Errno>;

    /// Stops the timer.
    ///
    /// This method should stop the timer and prevent it from triggering.
    fn stop(&self, current_task: &CurrentTask) -> Result<(), Errno>;

    /// Creates a `WaitCnaceler` that can be used to wait for the timer to be cancelled.
    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler;

    /// Returns a reference to the underlying Zircon handle.
    fn as_handle_ref(&self) -> HandleRef<'_>;
}

struct ZxTimer {
    timer: Arc<zx::Timer>,
}

impl ZxTimer {
    fn new() -> Self {
        Self { timer: Arc::new(zx::Timer::create()) }
    }
}

impl TimerOps for ZxTimer {
    fn start(&self, _currnet_task: &CurrentTask, deadline: zx::Time) -> Result<(), Errno> {
        self.timer
            .set(deadline, zx::Duration::default())
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(())
    }

    fn stop(&self, _current_task: &CurrentTask) -> Result<(), Errno> {
        self.timer.cancel().map_err(|status| from_status_like_fdio!(status))
    }

    fn wait_canceler(&self, canceler: HandleWaitCanceler) -> WaitCanceler {
        WaitCanceler::new_timer(Arc::downgrade(&self.timer), canceler)
    }

    fn as_handle_ref(&self) -> HandleRef<'_> {
        self.timer.as_handle_ref()
    }
}

/// Clock types supported by a `TimerFile`.
pub enum TimerFileClock {
    Monotonic,
    Realtime,
}

/// Wakeup types supported by a `TimerFile`.
pub enum TimerWakeup {
    /// A regular timer that does not wake the system if it is suspended.
    Regular,
    /// An alarm timer that will wake the system if it is suspended.
    Alarm,
}

/// A `TimerFile` represents a file created by `timerfd_create`.
///
/// Clients can read the number of times the timer has triggered from the file. The file supports
/// blocking reads, waiting for the timer to trigger.
pub struct TimerFile {
    /// The timer that is used to wait for blocking reads.
    timer: Arc<dyn TimerOps>,

    /// The type of clock this file was created with.
    clock: TimerFileClock,

    /// The deadline (`zx::Time`) for the next timer trigger, and the associated interval
    /// (`zx::Duration`).
    ///
    /// When the file is read, the deadline is recomputed based on the current time and the set
    /// interval. If the interval is 0, `self.timer` is cancelled after the file is read.
    deadline_interval: Mutex<(zx::Time, zx::Duration)>,
}

impl TimerFile {
    /// Creates a new anonymous `TimerFile` in `kernel`.
    ///
    /// Returns an error if the `zx::Timer` could not be created.
    pub fn new_file(
        current_task: &CurrentTask,
        wakeup_type: TimerWakeup,
        clock: TimerFileClock,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let timer: Arc<dyn TimerOps> = match wakeup_type {
            TimerWakeup::Regular => Arc::new(ZxTimer::new()),
            TimerWakeup::Alarm => Arc::new(HrTimer::new()),
        };

        Ok(Anon::new_file(
            current_task,
            Box::new(TimerFile {
                timer,
                clock,
                deadline_interval: Mutex::new((zx::Time::default(), zx::Duration::default())),
            }),
            flags,
        ))
    }

    /// Returns the current `itimerspec` for the file.
    ///
    /// The returned `itimerspec.it_value` contains the amount of time remaining until the
    /// next timer trigger.
    pub fn current_timer_spec(&self) -> itimerspec {
        let (deadline, interval) = *self.deadline_interval.lock();

        let now = zx::Time::get_monotonic();
        let remaining_time = if interval == zx::Duration::default() && deadline <= now {
            timespec_from_duration(zx::Duration::default())
        } else {
            timespec_from_duration(deadline - now)
        };

        itimerspec { it_interval: timespec_from_duration(interval), it_value: remaining_time }
    }

    /// Sets the `itimerspec` for the timer, which will either update the associated `zx::Timer`'s
    /// scheduled trigger or cancel the timer.
    ///
    /// Returns the previous `itimerspec` on success.
    pub fn set_timer_spec(
        &self,
        current_task: &CurrentTask,
        timer_spec: itimerspec,
        flags: u32,
    ) -> Result<itimerspec, Errno> {
        let mut deadline_interval = self.deadline_interval.lock();
        let (old_deadline, old_interval) = *deadline_interval;
        let old_itimerspec = itimerspec_from_deadline_interval(old_deadline, old_interval);

        if timespec_is_zero(timer_spec.it_value) {
            // Sayeth timerfd_settime(2):
            // Setting both fields of new_value.it_value to zero disarms the timer.
            self.timer.stop(current_task)?;
        } else {
            let now_monotonic = zx::Time::get_monotonic();
            let new_deadline = if flags & TFD_TIMER_ABSTIME != 0 {
                // If the time_spec represents an absolute time, then treat the
                // `it_value` as the deadline..
                match &self.clock {
                    TimerFileClock::Realtime => {
                        // Since Zircon does not have realtime timers, compute what the value would
                        // be in the monotonic clock assuming realtime progresses linearly.
                        track_stub!(
                            TODO("https://fxbug.dev/297433837"),
                            "realtime timer, TFD_TIMER_CANCEL_ON_SET"
                        );
                        crate::time::utc::estimate_monotonic_deadline_from_utc(time_from_timespec(
                            timer_spec.it_value,
                        )?)
                    }
                    TimerFileClock::Monotonic => time_from_timespec(timer_spec.it_value)?,
                }
            } else {
                // .. otherwise the deadline is computed relative to the current time. Without
                // realtime timers in Zircon, we assume realtime and monotonic time progresses the
                // same so relative values need no separate handling.
                let duration = duration_from_timespec(timer_spec.it_value)?;
                now_monotonic + duration
            };
            let new_interval = duration_from_timespec(timer_spec.it_interval)?;

            self.timer.start(current_task, new_deadline)?;
            *deadline_interval = (new_deadline, new_interval);
        }

        Ok(old_itimerspec)
    }

    /// Returns the `zx::Signals` to listen for given `events`. Used to wait on the `TimerOps`
    /// associated with a `TimerFile`.
    fn get_signals_from_events(events: FdEvents) -> zx::Signals {
        if events.contains(FdEvents::POLLIN) {
            zx::Signals::TIMER_SIGNALED
        } else {
            zx::Signals::NONE
        }
    }

    fn get_events_from_signals(signals: zx::Signals) -> FdEvents {
        let mut events = FdEvents::empty();

        if signals.contains(zx::Signals::TIMER_SIGNALED) {
            events |= FdEvents::POLLIN;
        }

        events
    }
}

impl FileOps for TimerFile {
    fileops_impl_nonseekable!();
    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        // The expected error seems to vary depending on the open flags..
        if file.flags().contains(OpenFlags::NONBLOCK) {
            error!(EINVAL)
        } else {
            error!(ESPIPE)
        }
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, || {
            let mut deadline_interval = self.deadline_interval.lock();
            let (deadline, interval) = *deadline_interval;

            if deadline == zx::Time::default() {
                // The timer has not been set.
                return error!(EAGAIN);
            }

            let now = zx::Time::get_monotonic();
            if deadline > now {
                // The next deadline has not yet passed.
                return error!(EAGAIN);
            }

            let count: i64 = if interval > zx::Duration::default() {
                let elapsed_nanos = (now - deadline).into_nanos();
                // The number of times the timer has triggered is written to `data`.
                let num_intervals = elapsed_nanos / interval.into_nanos() + 1;
                let new_deadline = deadline + interval * num_intervals;

                // The timer is set to clear the `ZX_TIMER_SIGNALED` signal until the next deadline
                // is reached.
                self.timer.start(current_task, new_deadline)?;

                // Update the stored deadline.
                *deadline_interval = (new_deadline, interval);

                num_intervals
            } else {
                // The timer is non-repeating, so cancel the timer to clear the `ZX_TIMER_SIGNALED`
                // signal.
                *deadline_interval = (zx::Time::default(), interval);
                self.timer.stop(current_task)?;
                1
            };

            data.write(count.as_bytes())
        })
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        event_handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let signal_handler = SignalHandler {
            inner: SignalHandlerInner::ZxHandle(TimerFile::get_events_from_signals),
            event_handler,
        };
        let canceler = waiter
            .wake_on_zircon_signals(
                &self.timer.as_handle_ref(),
                TimerFile::get_signals_from_events(events),
                signal_handler,
            )
            .unwrap(); // TODO return error
        let wait_canceler = self.timer.wait_canceler(canceler);
        Some(wait_canceler)
    }

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let observed =
            match self.timer.as_handle_ref().wait(zx::Signals::TIMER_SIGNALED, zx::Time::ZERO) {
                Err(zx::Status::TIMED_OUT) => zx::Signals::empty(),
                res => res.unwrap(),
            };
        Ok(TimerFile::get_events_from_signals(observed))
    }
}
