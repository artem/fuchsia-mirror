// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_process as fprocess;
#[cfg(feature = "syscall_stats")]
use fuchsia_inspect::NumericProperty;
use fuchsia_runtime::{HandleInfo, HandleType};
use fuchsia_zircon::{self as zx};
use lock_sequence::{Locked, Unlocked};
use std::{convert::TryFrom, sync::Arc};

use crate::{
    arch::execution::new_syscall,
    fs::fuchsia::{create_file_from_handle, RemoteBundle, RemoteFs, SyslogFile},
    logging::log_trace,
    mm::MemoryManager,
    signals::{dequeue_signal, prepare_to_restart_syscall},
    syscalls::table::dispatch_syscall,
    task::{
        CurrentTask, ExitStatus, Kernel, SeccompStateValue, StopState, TaskFlags, ThreadGroup,
        ThreadState, Waiter,
    },
    vfs::{FdNumber, FdTable, FileSystemCreator, FileSystemHandle, FileSystemOptions},
};
use starnix_syscalls::{
    decls::{Syscall, SyscallDecl},
    SyscallResult,
};
use starnix_uapi::{errno, errors::Errno, mount_flags::MountFlags};

/// Contains context to track the most recently failing system call.
///
/// When a task exits with a non-zero exit code, this context is logged to help debugging which
/// system call may have triggered the failure.
pub struct ErrorContext {
    /// The system call that failed.
    pub syscall: Syscall,

    /// The error that was returned for the system call.
    pub error: Errno,
}

/// Result returned when creating new Zircon threads and processes for tasks.
pub struct TaskInfo {
    /// The thread that was created for the task.
    pub thread: Option<zx::Thread>,

    /// The thread group that the task should be added to.
    pub thread_group: Arc<ThreadGroup>,

    /// The memory manager to use for the task.
    pub memory_manager: Arc<MemoryManager>,
}

/// Executes the provided `syscall` in `current_task`.
///
/// Returns an `ErrorContext` if the system call returned an error.
#[inline(never)] // Inlining this function breaks the CFI directives used to unwind into user code.
pub fn execute_syscall(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &mut CurrentTask,
    syscall_decl: SyscallDecl,
) -> Option<ErrorContext> {
    #[cfg(feature = "syscall_stats")]
    crate::syscalls::syscall_stats::syscall_stats_property(syscall_decl.number).add(1);

    let syscall = new_syscall(syscall_decl, current_task);

    current_task.thread_state.registers.save_registers_for_restart(syscall.decl.number);

    log_trace!("{:?}", syscall);

    let result: Result<SyscallResult, Errno> =
        if current_task.seccomp_filter_state.get() != SeccompStateValue::None {
            // Inlined fast path for seccomp, so that we don't incur the cost
            // of a method call when running the filters.
            if let Some(res) = current_task.run_seccomp_filters(&syscall) {
                res
            } else {
                dispatch_syscall(locked, current_task, &syscall)
            }
        } else {
            dispatch_syscall(locked, current_task, &syscall)
        };

    current_task.trigger_delayed_releaser();

    match result {
        Ok(return_value) => {
            log_trace!("-> {:#x}", return_value.value());
            current_task.thread_state.registers.set_return_register(return_value.value());
            None
        }
        Err(errno) => {
            log_trace!("!-> {:?}", errno);
            current_task.thread_state.registers.set_return_register(errno.return_value());
            Some(ErrorContext { error: errno, syscall })
        }
    }
}

/// Finishes `current_task` updates after a restricted mode exit such as a syscall, exception, or kick.
///
/// Returns an `ExitStatus` if the task is meant to exit.
pub fn process_completed_restricted_exit(
    current_task: &mut CurrentTask,
    error_context: &Option<ErrorContext>,
) -> Result<Option<ExitStatus>, Errno> {
    // Checking for a signal might cause the task to exit, so check before processing exit
    {
        let flags = current_task.flags();
        {
            let CurrentTask { task, thread_state: ThreadState { registers, extended_pstate, .. } } =
                current_task;
            let task_state = task.write();
            if flags.contains(TaskFlags::TEMPORARY_SIGNAL_MASK)
                || (!flags.contains(TaskFlags::EXITED)
                    && flags.contains(TaskFlags::SIGNALS_AVAILABLE))
            {
                if !task.is_exitted() {
                    dequeue_signal(task, task_state, registers, extended_pstate);
                }
            }
            // The syscall may need to restart for a non-signal-related
            // reason. This call does nothing if we aren't restarting.
            prepare_to_restart_syscall(registers, None);
        }
    }

    let exit_status = current_task.exit_status();
    if let Some(exit_status) = exit_status {
        log_trace!("exiting with status {:?}", exit_status);
        if let Some(error_context) = error_context {
            match exit_status {
                ExitStatus::Exit(value) if value == 0 => {}
                _ => {
                    log_trace!(
                        "last failing syscall before exit: {:?}, failed with {:?}",
                        error_context.syscall,
                        error_context.error
                    );
                }
            };
        }

        Ok(Some(exit_status))
    } else {
        // Block a stopped process after it's had a chance to handle signals, since a signal might
        // cause it to stop.
        block_while_stopped(current_task);

        Ok(None)
    }
}

/// Creates a `StartupHandles` from the provided handles.
///
/// The `numbered_handles` of type `HandleType::FileDescriptor` are used to
/// create files, and the handles are required to be of type `zx::Socket`.
///
/// If there is a `numbered_handles` of type `HandleType::User0`, that is
/// interpreted as the server end of the ShellController protocol.
pub fn parse_numbered_handles(
    current_task: &CurrentTask,
    numbered_handles: Option<Vec<fprocess::HandleInfo>>,
    files: &FdTable,
) -> Result<(), Error> {
    if let Some(numbered_handles) = numbered_handles {
        for numbered_handle in numbered_handles {
            let info = HandleInfo::try_from(numbered_handle.id)?;
            if info.handle_type() == HandleType::FileDescriptor {
                files.insert(
                    current_task,
                    FdNumber::from_raw(info.arg().into()),
                    create_file_from_handle(current_task, numbered_handle.handle)?,
                )?;
            }
        }
    }

    let stdio = SyslogFile::new_file(current_task);
    // If no numbered handle is provided for each stdio handle, default to syslog.
    for i in [0, 1, 2] {
        if files.get(FdNumber::from_raw(i)).is_err() {
            files.insert(current_task, FdNumber::from_raw(i), stdio.clone())?;
        }
    }

    Ok(())
}

/// Create a filesystem to access the content of the fuchsia directory available at `fs_src` inside
/// `pkg`.
pub fn create_remotefs_filesystem(
    kernel: &Arc<Kernel>,
    root: &fio::DirectorySynchronousProxy,
    rights: fio::OpenFlags,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let root = syncio::directory_open_directory_async(
        root,
        std::str::from_utf8(&options.source)
            .map_err(|_| errno!(EINVAL, "source path is not utf8"))?,
        rights,
    )
    .map_err(|e| errno!(EIO, format!("Failed to open root: {e}")))?;
    RemoteFs::new_fs(kernel, root.into_channel(), options, rights)
}

pub fn create_filesystem_from_spec<'a>(
    creator: &impl FileSystemCreator,
    pkg: &fio::DirectorySynchronousProxy,
    spec: &'a str,
) -> Result<(&'a [u8], FileSystemHandle), Error> {
    let kernel = creator.kernel();

    let mut iter = spec.splitn(4, ':');
    let mount_point =
        iter.next().ok_or_else(|| anyhow!("mount point is missing from {:?}", spec))?;
    let fs_type = iter.next().ok_or_else(|| anyhow!("fs type is missing from {:?}", spec))?;
    let fs_src = match iter.next() {
        Some(src) if !src.is_empty() => src,
        _ => ".",
    };
    let params = iter.next().unwrap_or("");

    let options = FileSystemOptions {
        source: fs_src.as_bytes().to_vec(),
        flags: MountFlags::empty(),
        params: params.as_bytes().to_vec(),
    };

    // Default rights for remotefs.
    let rights = fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE;

    // The filesystem types handled in this match are the ones that can only be specified in a
    // manifest file, for whatever reason. Anything else is passed to create_filesystem, which is
    // common code that also handles the mount() system call.
    let fs = match fs_type {
        "remote_bundle" => RemoteBundle::new_fs(kernel, pkg, rights, fs_src)?,
        "remotefs" => create_remotefs_filesystem(kernel, pkg, rights, options)?,
        _ => creator.create_filesystem(fs_type.as_bytes(), options)?,
    };
    Ok((mount_point.as_bytes(), fs))
}

/// Block the execution of `current_task` as long as the task is stopped and not terminated.
fn block_while_stopped(current_task: &mut CurrentTask) {
    // Upgrade the state from stopping to stopped if needed. Return if the task
    // should not be stopped.
    if !current_task.finalize_stop_state() {
        return;
    }

    let waiter = Waiter::new_ignoring_signals();
    loop {
        // If we've exited, unstop the threads and return without notifying
        // waiters.
        if current_task.is_exitted() {
            current_task.thread_group.set_stopped(StopState::ForceAwake, None, false);
            current_task.set_stopped(&mut *current_task.write(), StopState::ForceAwake, None);
            return;
        }

        if current_task.wake_or_wait_until_unstopped_async(&waiter) {
            return;
        }

        // Do the wait. Result is not needed, as this is not in a syscall.
        let _: Result<(), Errno> = waiter.wait(current_task);

        // Maybe go from stopping to stopped, if we are currently stopping
        // again.
        current_task.finalize_stop_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{signals::SignalInfo, testing::*};
    use starnix_uapi::signals::{SIGCONT, SIGSTOP};

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_continue() {
        let (_kernel, mut task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is not stopped.
        block_while_stopped(&mut task);

        // Stop the task.
        task.thread_group.set_stopped(
            StopState::GroupStopping,
            Some(SignalInfo::default(SIGSTOP)),
            false,
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                let task = task.upgrade().expect("task must be alive");
                // Wait for the task to have a waiter.
                while !task.read().signals.run_state.is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }

                // Continue the task.
                task.thread_group.set_stopped(
                    StopState::Waking,
                    Some(SignalInfo::default(SIGCONT)),
                    false,
                );
            }
        });

        // Block until continued.
        block_while_stopped(&mut task);

        // Join the thread, which will ensure set_stopped terminated.
        thread.join().expect("joined");

        // The task should not be blocked anymore.
        block_while_stopped(&mut task);
    }

    #[::fuchsia::test]
    async fn test_block_while_stopped_stop_and_exit() {
        let (_kernel, mut task) = create_kernel_and_task();

        // block_while_stopped must immediately returned if the task is neither stopped nor exited.
        block_while_stopped(&mut task);

        // Stop the task.
        task.thread_group.set_stopped(
            StopState::GroupStopping,
            Some(SignalInfo::default(SIGSTOP)),
            false,
        );

        let thread = std::thread::spawn({
            let task = task.weak_task();
            move || {
                let task = task.upgrade().expect("task must be alive");
                // Wait for the task to have a waiter.
                while !task.read().signals.run_state.is_blocked() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }

                // exit the task.
                task.thread_group.exit(ExitStatus::Exit(1));
            }
        });

        // Block until continued.
        block_while_stopped(&mut task);

        // Join the task, which will ensure thread_group.exit terminated.
        thread.join().expect("joined");

        // The task should not be blocked because it is stopped.
        block_while_stopped(&mut task);
    }
}
