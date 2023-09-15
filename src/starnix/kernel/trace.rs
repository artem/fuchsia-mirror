// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::CStr;

#[cfg(not(feature = "disable_tracing"))]
use fuchsia_trace::{category_enabled, cstr};
#[cfg(not(feature = "disable_tracing"))]
use fuchsia_zircon::{self as zx, Task as _};

// The trace category used for starnix-related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix, "starnix");

// The trace category used for memory manager related traces.
fuchsia_trace::string_name_macro!(trace_category_starnix_mm, "starnix:mm");

// The trace category used to enable task runtime args on trace durations.
fuchsia_trace::string_name_macro!(trace_category_starnix_task_runtime, "starnix:task_runtime");

// The name used to track the duration in Starnix while executing a task.
fuchsia_trace::string_name_macro!(trace_name_run_task, "RunTask");

// The trace category used for atrace events generated within starnix.
fuchsia_trace::string_name_macro!(trace_category_atrace, "starnix:atrace");

// The name used to identify blob records from the container's Perfetto daemon.
fuchsia_trace::string_name_macro!(trace_name_perfetto_blob, "starnix_perfetto");

// The name used to track the duration in Starnix while executing a syscall.
fuchsia_trace::string_name_macro!(trace_name_normal_mode, "NormalMode");

// The name used to track the duration in user space between syscalls.
fuchsia_trace::string_name_macro!(trace_name_restricted_mode, "RestrictedMode");

// The name used to track the duration of a syscall.
fuchsia_trace::string_name_macro!(trace_name_execute_syscall, "ExecuteSyscall");

// The name used to track the duration of creating a container.
fuchsia_trace::string_name_macro!(trace_name_create_container, "CreateContainer");

// The name used to track the start time of the starnix kernel.
fuchsia_trace::string_name_macro!(trace_name_start_kernel, "StartKernel");

// The name used to track when a thread was kicked.
fuchsia_trace::string_name_macro!(trace_name_restricted_kick, "RestrictedKick");

// The name used to track the duration for inline exception handling.
fuchsia_trace::string_name_macro!(trace_name_handle_exception, "HandleException");

// The names used to track durations for restricted state I/O.
fuchsia_trace::string_name_macro!(trace_name_read_restricted_state, "ReadRestrictedState");
fuchsia_trace::string_name_macro!(trace_name_write_restricted_state, "WriteRestrictedState");

// The name used to track the duration of checking whether the task loop should exit.
fuchsia_trace::string_name_macro!(trace_name_check_task_exit, "CheckTaskExit");

// The argument used to track the name of a syscall.
fuchsia_trace::string_name_macro!(trace_arg_name, "name");

// The argument used to track the CPU time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_cpu_time, "cpu_time");

// The argument used to track the queue time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_queue_time, "queue_time");

// The argument used to track the page fault time of a thread at various points during a syscall.
fuchsia_trace::string_name_macro!(trace_arg_page_fault_time, "page_fault_time");

// The argument used to track the lock contention time of a thread at various points during a
// syscall.
fuchsia_trace::string_name_macro!(trace_arg_lock_contention_time, "lock_contention_time");

macro_rules! ignore_unused_variables_if_disable_tracing {
    ($($val:expr),*) => {
        $(
            #[cfg(feature = "disable_tracing")]
            { let _ = &$val; }
        )*
    };
}

macro_rules! trace_instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::instant!($category, $name, $scope $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

// The `trace_duration` macro defines a `_scope` instead of executing a statement because the
// lifetime of the `_scope` variable corresponds to the duration.
macro_rules! trace_duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        let _args = [$(fuchsia_trace::ArgValue::of($key, $val)),*];
        #[cfg(not(feature = "disable_tracing"))]
        let _scope = fuchsia_trace::duration(fuchsia_trace::cstr!($category), fuchsia_trace::cstr!($name), &_args);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    }
}

macro_rules! trace_duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::duration_begin!($category, $name $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::duration_end!($category, $name $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

/// Zircon scheduler stats don't understand restricted vs. normal modes, so to compute the relative
/// time between them we need to measure ourselves when switching into and out of restricted mode.
pub(crate) struct TaskInfoDurationScope {
    category: &'static CStr,
    name: &'static CStr,
    start_ticks: i64,
    start_runtime: Option<zx::TaskRuntimeInfo>,
}

impl TaskInfoDurationScope {
    pub fn start(category: &'static CStr, name: &'static CStr) -> Self {
        let start_ticks = zx::ticks_get();
        let start_runtime = if category_enabled(cstr!(trace_category_starnix_task_runtime!())) {
            let start_runtime = fuchsia_runtime::thread_self().get_runtime_info().unwrap();
            Some(start_runtime)
        } else {
            None
        };

        Self { category, name, start_runtime, start_ticks }
    }

    pub fn finish(self) {
        if let Some(start_runtime) = self.start_runtime {
            let zx::TaskRuntimeInfo { cpu_time, queue_time, page_fault_time, lock_contention_time } =
                fuchsia_runtime::thread_self().get_runtime_info().unwrap() - start_runtime;
            fuchsia_trace::complete_duration(
                self.category,
                self.name,
                self.start_ticks,
                &[
                    fuchsia_trace::ArgValue::of(trace_arg_cpu_time!(), cpu_time),
                    fuchsia_trace::ArgValue::of(trace_arg_queue_time!(), queue_time),
                    fuchsia_trace::ArgValue::of(trace_arg_page_fault_time!(), page_fault_time),
                    fuchsia_trace::ArgValue::of(
                        trace_arg_lock_contention_time!(),
                        lock_contention_time,
                    ),
                ],
            );
        } else {
            fuchsia_trace::complete_duration(self.category, self.name, self.start_ticks, &[]);
        }
    }
}

macro_rules! trace_duration_begin_with_task_info {
    ($category:expr, $name:expr) => {
        if cfg!(not(feature = "disable_tracing")) {
            let category = fuchsia_trace::cstr!($category);
            if fuchsia_trace::category_enabled(category) {
                Some(crate::trace::TaskInfoDurationScope::start(
                    category,
                    fuchsia_trace::cstr!($name),
                ))
            } else {
                None
            }
        } else {
            None
        }
    };
}

macro_rules! trace_duration_end_with_task_info {
    ($start_info:expr) => {
        if cfg!(not(feature = "disable_tracing")) {
            if let Some(start_info) = $start_info {
                crate::trace::TaskInfoDurationScope::finish(start_info);
            }
        }
    };
}

macro_rules! trace_flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_begin!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_step!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}

macro_rules! trace_flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        let _flow_id: fuchsia_trace::Id = $flow_id;

        #[cfg(not(feature = "disable_tracing"))]
        fuchsia_trace::flow_end!($category, $name, _flow_id $(, $key => $val)*);

        ignore_unused_variables_if_disable_tracing!($($val),*);
    };
}
