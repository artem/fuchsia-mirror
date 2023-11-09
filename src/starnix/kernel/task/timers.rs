// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_lock::Mutex;
use std::collections::HashMap;

use crate::{
    signals::{SignalEvent, SignalEventNotify, SignalEventValue},
    task::{
        interval_timer::{IntervalTimer, IntervalTimerHandle},
        CurrentTask,
    },
    time::utc,
    types::{
        Errno, __kernel_timer_t, duration_from_timespec, error, itimerspec, time_from_timespec,
        uapi, SIGALRM, TIMER_ABSTIME,
    },
};

// Table for POSIX timers from timer_create() that deliver timers via signals (not new-style
// timerfd's).
//
// This is currently unimplemented.
#[derive(Debug, Default)]
pub struct TimerTable {
    state: Mutex<TimerTableMutableState>,
}

#[derive(Debug, Default)]
struct TimerTableMutableState {
    /// The `TimerId` at which allocation should begin searching for an unused ID.
    next_timer_id: TimerId,
    timers: HashMap<TimerId, IntervalTimerHandle>,
}

pub type TimerId = __kernel_timer_t;
pub type ClockId = uapi::__kernel_clockid_t;

impl TimerTable {
    pub fn new() -> TimerTable {
        Default::default()
    }

    /// Creates a new per-process interval timer.
    ///
    /// The new timer is initially disarmed.
    pub fn create(
        &self,
        clock_id: ClockId,
        signal_event: Option<SignalEvent>,
    ) -> Result<TimerId, Errno> {
        let mut state = self.state.lock();

        // Find a vacant timer id.
        let end = state.next_timer_id;
        let timer_id = loop {
            let timer_id = state.next_timer_id;
            state.next_timer_id += 1;

            if state.next_timer_id == TimerId::MAX {
                state.next_timer_id = 0;
            } else if state.next_timer_id == end {
                // After searching the entire timer map, there is no vacant timer id.
                // Fails the call and implies the program could try it again later.
                return error!(EAGAIN);
            }

            if !state.timers.contains_key(&timer_id) {
                break timer_id;
            }
        };

        state.timers.insert(
            timer_id,
            IntervalTimer::new(
                timer_id,
                clock_id,
                signal_event.unwrap_or(SignalEvent::new(
                    SignalEventValue(timer_id as u64),
                    SIGALRM,
                    SignalEventNotify::Signal,
                )),
            ),
        );

        Ok(timer_id)
    }

    /// Disarms and deletes a timer.
    pub fn delete(&self, id: TimerId) -> Result<(), Errno> {
        let mut state = self.state.lock();
        match state.timers.remove_entry(&id) {
            Some(entry) => {
                entry.1.disarm();
                Ok(())
            }
            None => error!(EINVAL),
        }
    }

    /// Fetches the time remaining until the next expiration of a timer, along with the interval
    /// setting of the timer.
    pub fn get_time(&self, id: TimerId) -> Result<itimerspec, Errno> {
        Ok(self.get_timer(id)?.time_remaining().into())
    }

    /// Returns the overrun count for the last timer expiration.
    pub fn get_overrun(&self, id: TimerId) -> Result<i32, Errno> {
        Ok(self.get_timer(id)?.overrun_last())
    }

    /// Arms (start) or disarms (stop) the timer identifierd by `id`. The `new_value` arg is a
    /// pointer to an `itimerspec` structure that specifies the new initial value and the new
    /// interval for the timer.
    pub fn set_time(
        &self,
        current_task: &CurrentTask,
        id: TimerId,
        flags: i32,
        new_value: itimerspec,
    ) -> Result<itimerspec, Errno> {
        let itimer = self.get_timer(id)?;
        let old_value: itimerspec = itimer.time_remaining().into();
        if new_value.it_value.tv_sec != 0 || new_value.it_value.tv_nsec != 0 {
            let target_time = if flags == TIMER_ABSTIME as i32 {
                time_from_timespec(new_value.it_value)?
            } else {
                utc::utc_now() + duration_from_timespec(new_value.it_value)?
            };
            itimer.arm(
                std::sync::Arc::downgrade(&current_task.thread_group),
                &current_task.kernel().kthreads.ehandle,
                target_time,
                duration_from_timespec(new_value.it_interval)?,
            );
        } else {
            itimer.disarm();
        }

        Ok(old_value)
    }

    pub fn get_timer(&self, id: TimerId) -> Result<IntervalTimerHandle, Errno> {
        match self.state.lock().timers.get(&id) {
            Some(itimer) => Ok(itimer.clone()),
            None => error!(EINVAL),
        }
    }
}
