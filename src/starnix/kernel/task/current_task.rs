// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    arch::{
        registers::RegisterState,
        task::{decode_page_fault_exception_report, get_signal_for_general_exception},
    },
    execution::{create_zircon_process, TaskInfo},
    loader::{load_executable, resolve_executable, ResolvedElf},
    mm::{MemoryAccessor, MemoryAccessorExt, MemoryManager, TaskMemoryAccessor},
    signals::{send_standard_signal, RunState, SignalActions, SignalInfo},
    task::{
        Kernel, PidTable, ProcessGroup, PtraceCoreState, PtraceEvent, PtraceEventData,
        PtraceOptions, SeccompFilter, SeccompFilterContainer, SeccompNotifierHandle, SeccompState,
        SeccompStateValue, StopState, Task, TaskFlags, ThreadGroup, Waiter,
    },
    vfs::{
        FdNumber, FdTable, FileHandle, FsContext, FsStr, LookupContext, NamespaceNode, SymlinkMode,
        SymlinkTarget,
    },
};
use extended_pstate::ExtendedPstateState;
use fuchsia_inspect_contrib::profile_duration;
use fuchsia_zircon::{
    sys::zx_thread_state_general_regs_t,
    {self as zx},
};
use starnix_logging::{log_error, log_warn, not_implemented, set_zx_name, track_file_not_found};
use starnix_sync::{
    EventWaitGuard, LockBefore, Locked, MmDumpable, RwLock, RwLockWriteGuard, TaskRelease,
    WakeReason,
};
use starnix_syscalls::{decls::Syscall, SyscallResult};
use starnix_uapi::{
    auth::{Credentials, CAP_SYS_ADMIN},
    clone_args,
    device_type::DeviceType,
    errno, error,
    errors::Errno,
    file_mode::{Access, FileMode},
    from_status_like_fdio,
    open_flags::OpenFlags,
    ownership::{release_on_error, OwnedRef, Releasable, TempRef, WeakRef},
    pid_t,
    signals::{SigSet, Signal, SIGBUS, SIGCHLD, SIGILL, SIGSEGV, SIGTRAP},
    sock_filter, sock_fprog,
    user_address::{UserAddress, UserRef},
    BPF_MAXINSNS, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID, CLONE_FILES, CLONE_FS,
    CLONE_INTO_CGROUP, CLONE_NEWUTS, CLONE_PARENT_SETTID, CLONE_PTRACE, CLONE_SETTLS,
    CLONE_SIGHAND, CLONE_SYSVSEM, CLONE_THREAD, CLONE_VFORK, CLONE_VM, FUTEX_OWNER_DIED,
    FUTEX_TID_MASK, ROBUST_LIST_LIMIT, SECCOMP_FILTER_FLAG_LOG, SECCOMP_FILTER_FLAG_NEW_LISTENER,
    SECCOMP_FILTER_FLAG_TSYNC, SECCOMP_FILTER_FLAG_TSYNC_ESRCH, SI_KERNEL,
};
use std::{ffi::CString, fmt, marker::PhantomData, mem::MaybeUninit, sync::Arc};

pub struct TaskBuilder {
    /// The underlying task object.
    pub task: OwnedRef<Task>,

    pub thread_state: ThreadState,
}

impl TaskBuilder {
    pub fn new(task: Task) -> Self {
        Self { task: OwnedRef::new(task), thread_state: Default::default() }
    }

    pub fn release<L>(self, locked: &mut Locked<'_, L>)
    where
        L: LockBefore<TaskRelease>,
    {
        let mut locked = locked.cast_locked::<TaskRelease>();
        Releasable::release(self, &mut locked);
    }
}

impl From<TaskBuilder> for CurrentTask {
    fn from(builder: TaskBuilder) -> Self {
        Self::new(builder.task, builder.thread_state)
    }
}

impl Releasable for TaskBuilder {
    type Context<'a> = &'a mut Locked<'a, TaskRelease>;

    fn release<'a>(self, locked: &'a mut Locked<'a, TaskRelease>) {
        let context = (self.thread_state, locked);
        self.task.release(context);
    }
}

impl std::ops::Deref for TaskBuilder {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}

/// The task object associated with the currently executing thread.
///
/// We often pass the `CurrentTask` as the first argument to functions if those functions need to
/// know contextual information about the thread on which they are running. For example, we often
/// use the `CurrentTask` to perform access checks, which ensures that the caller is authorized to
/// perform the requested operation.
///
/// The `CurrentTask` also has state that can be referenced only on the currently executing thread,
/// such as the register state for that thread. Syscalls are given a mutable references to the
/// `CurrentTask`, which lets them manipulate this state.
///
/// See also `Task` for more information about tasks.
pub struct CurrentTask {
    /// The underlying task object.
    pub task: OwnedRef<Task>,

    pub thread_state: ThreadState,

    /// Makes CurrentTask neither Sync not Send.
    _local_marker: PhantomData<*mut u8>,
}

/// The thread related information of a `CurrentTask`. The information should never be used  outside
/// of the thread owning the `CurrentTask`.
#[derive(Default)]
pub struct ThreadState {
    /// A copy of the registers associated with the Zircon thread. Up-to-date values can be read
    /// from `self.handle.read_state_general_regs()`. To write these values back to the thread, call
    /// `self.handle.write_state_general_regs(self.thread_state.registers.into())`.
    pub registers: RegisterState,

    /// Copy of the current extended processor state including floating point and vector registers.
    pub extended_pstate: ExtendedPstateState,

    /// A custom function to resume a syscall that has been interrupted by SIGSTOP.
    /// To use, call set_syscall_restart_func and return ERESTART_RESTARTBLOCK. sys_restart_syscall
    /// will eventually call it.
    pub syscall_restart_func: Option<Box<SyscallRestartFunc>>,
}

impl ThreadState {
    /// Returns a new `ThreadState` with the same `registers` as this one.
    fn snapshot(&self) -> Self {
        Self {
            registers: self.registers,
            extended_pstate: Default::default(),
            syscall_restart_func: None,
        }
    }

    pub fn extended_snapshot(&self) -> Self {
        Self {
            registers: self.registers.clone(),
            extended_pstate: self.extended_pstate.clone(),
            syscall_restart_func: None,
        }
    }
}

type SyscallRestartFunc =
    dyn FnOnce(&mut CurrentTask) -> Result<SyscallResult, Errno> + Send + Sync;

impl Releasable for CurrentTask {
    type Context<'a> = &'a mut Locked<'a, TaskRelease>;

    fn release<'a>(self, locked: &'a mut Locked<'a, TaskRelease>) {
        self.notify_robust_list();
        let _ignored = self.clear_child_tid_if_needed();
        let context = (self.thread_state, locked);
        self.task.release(context);
    }
}

impl std::ops::Deref for CurrentTask {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}

impl fmt::Debug for CurrentTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.task.fmt(f)
    }
}

impl CurrentTask {
    pub fn new(task: OwnedRef<Task>, thread_state: ThreadState) -> Self {
        Self { task, thread_state, _local_marker: Default::default() }
    }

    pub fn trigger_delayed_releaser(&self) {
        self.kernel().delayed_releaser.apply(self);
    }

    pub fn weak_task(&self) -> WeakRef<Task> {
        WeakRef::from(&self.task)
    }

    pub fn temp_task(&self) -> TempRef<'_, Task> {
        TempRef::from(&self.task)
    }

    pub fn release<L>(self, locked: &mut Locked<'_, L>)
    where
        L: LockBefore<TaskRelease>,
    {
        let mut locked = locked.cast_locked::<TaskRelease>();
        Releasable::release(self, &mut locked);
    }

    pub fn set_syscall_restart_func<R: Into<SyscallResult>>(
        &mut self,
        f: impl FnOnce(&mut CurrentTask) -> Result<R, Errno> + Send + Sync + 'static,
    ) {
        self.thread_state.syscall_restart_func =
            Some(Box::new(|current_task| Ok(f(current_task)?.into())));
    }

    /// Sets the task's signal mask to `signal_mask` and runs `wait_function`.
    ///
    /// Signals are dequeued prior to the original signal mask being restored. This is done by the
    /// signal machinery in the syscall dispatch loop.
    ///
    /// The returned result is the result returned from the wait function.
    pub fn wait_with_temporary_mask<F, T>(
        &mut self,
        signal_mask: SigSet,
        wait_function: F,
    ) -> Result<T, Errno>
    where
        F: FnOnce(&CurrentTask) -> Result<T, Errno>,
    {
        {
            let mut state = self.write();
            state.set_flags(TaskFlags::TEMPORARY_SIGNAL_MASK, true);
            state.signals.set_temporary_mask(signal_mask);
        }
        wait_function(self)
    }

    /// If waking, promotes from waking to awake.  If not waking, make waiter async
    /// wait until woken.  Returns true if woken.
    pub fn wake_or_wait_until_unstopped_async(&self, waiter: &Waiter) -> bool {
        let group_state = self.thread_group.read();
        let mut task_state = self.write();

        // Wake up if
        //   a) we should wake up, meaning:
        //      i) we're in group stop, and the thread group has exited group stop, or
        //      ii) we're waking up,
        //   b) and ptrace isn't stopping us from waking up, but
        //   c) always wake up if we got a SIGKILL.
        let task_stop_state = self.load_stopped();
        let group_stop_state = self.thread_group.load_stopped();
        if ((task_stop_state == StopState::GroupStopped && group_stop_state.is_waking_or_awake())
            || task_stop_state.is_waking_or_awake())
            && (!task_state.is_ptrace_listening() || task_stop_state.is_force())
        {
            let new_state = if task_stop_state.is_waking_or_awake() {
                task_stop_state.finalize()
            } else {
                group_stop_state.finalize()
            };
            if let Ok(new_state) = new_state {
                task_state.set_stopped(new_state, None, Some(self), None);
                drop(group_state);
                drop(task_state);
                // It is possible for the stop state to be changed by another
                // thread between when it is checked above and the following
                // invocation, but set_stopped does sufficient checking while
                // holding the lock to make sure that such a change won't result
                // in corrupted state.
                self.thread_group.set_stopped(new_state, None, false);
                return true;
            }
        }

        // We will wait.
        if self.thread_group.load_stopped().is_stopped() || task_stop_state.is_stopped() {
            // If we've stopped or PTRACE_LISTEN has been sent, wait for a
            // signal or instructions from the tracer.
            group_state.stopped_waiters.wait_async(&waiter);
            task_state.wait_on_ptracer(&waiter);
        } else if task_state.can_accept_ptrace_commands() {
            // If we're stopped because a tracer has seen the stop and not taken
            // further action, wait for further instructions from the tracer.
            task_state.wait_on_ptracer(&waiter);
        } else if task_state.is_ptrace_listening() {
            // A PTRACE_LISTEN is a state where we can get signals and notify a
            // ptracer, but otherwise remain blocked.
            if let Some(ref mut ptrace) = &mut task_state.ptrace {
                ptrace.set_last_signal(Some(SignalInfo::default(SIGTRAP)));
                ptrace.set_last_event(Some(PtraceEventData::new_from_event(PtraceEvent::Stop, 0)));
            }
            task_state.wait_on_ptracer(&waiter);
            task_state.notify_ptracers();
        }
        false
    }

    /// Set the RunState for the current task to the given value and then call the given callback.
    ///
    /// When the callback is done, the run_state is restored to `RunState::Running`.
    ///
    /// This function is typically used just before blocking the current task on some operation.
    /// The given `run_state` registers the mechanism for interrupting the blocking operation with
    /// the task and the given `callback` actually blocks the task.
    ///
    /// This function can only be called in the `RunState::Running` state and cannot set the
    /// run state to `RunState::Running`. For this reason, this function cannot be reentered.
    pub fn run_in_state<F, T>(&self, run_state: RunState, callback: F) -> Result<T, Errno>
    where
        F: FnOnce() -> Result<T, Errno>,
    {
        assert_ne!(run_state, RunState::Running);

        {
            let mut state = self.write();
            assert!(!state.signals.run_state.is_blocked());
            // A note on PTRACE_LISTEN - the thread cannot be scheduled
            // regardless of pending signals.
            if state.signals.is_any_pending() && !state.is_ptrace_listening() {
                return error!(EINTR);
            }
            state.signals.run_state = run_state.clone();
        }

        let result = callback();

        {
            let mut state = self.write();
            assert_eq!(
                state.signals.run_state, run_state,
                "SignalState run state changed while waiting!"
            );
            state.signals.run_state = RunState::Running;
        };

        result
    }

    pub fn block_until(&self, guard: EventWaitGuard<'_>, deadline: zx::Time) -> Result<(), Errno> {
        self.run_in_state(RunState::Event(guard.event().clone()), move || {
            guard.block_until(deadline).map_err(|e| match e {
                WakeReason::Interrupted => errno!(EINTR),
                WakeReason::DeadlineExpired => errno!(ETIMEDOUT),
            })
        })
    }

    /// Determine namespace node indicated by the dir_fd.
    ///
    /// Returns the namespace node and the path to use relative to that node.
    pub fn resolve_dir_fd<'a>(
        &self,
        dir_fd: FdNumber,
        mut path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        let dir = if path.starts_with(b"/") {
            path = &path[1..];
            self.fs().root()
        } else if dir_fd == FdNumber::AT_FDCWD {
            self.fs().cwd()
        } else {
            let file = self.files.get(dir_fd)?;
            file.name.clone()
        };
        if !path.is_empty() {
            if !dir.entry.node.is_dir() {
                return error!(ENOTDIR);
            }
            dir.check_access(self, Access::EXEC)?;
        }
        Ok((dir, path.into()))
    }

    /// A convenient wrapper for opening files relative to FdNumber::AT_FDCWD.
    ///
    /// Returns a FileHandle but does not install the FileHandle in the FdTable
    /// for this task.
    pub fn open_file(&self, path: &FsStr, flags: OpenFlags) -> Result<FileHandle, Errno> {
        if flags.contains(OpenFlags::CREAT) {
            // In order to support OpenFlags::CREAT we would need to take a
            // FileMode argument.
            return error!(EINVAL);
        }
        self.open_file_at(FdNumber::AT_FDCWD, path, flags, FileMode::default())
    }

    /// Resolves a path for open.
    ///
    /// If the final path component points to a symlink, the symlink is followed (as long as
    /// the symlink traversal limit has not been reached).
    ///
    /// If the final path component (after following any symlinks, if enabled) does not exist,
    /// and `flags` contains `OpenFlags::CREAT`, a new node is created at the location of the
    /// final path component.
    ///
    /// This returns the resolved node, and a boolean indicating whether the node has been created.
    fn resolve_open_path(
        &self,
        context: &mut LookupContext,
        dir: &NamespaceNode,
        path: &FsStr,
        mode: FileMode,
        flags: OpenFlags,
    ) -> Result<(NamespaceNode, bool), Errno> {
        context.update_for_path(path);
        let mut parent_content = context.with(SymlinkMode::Follow);
        let (parent, basename) = self.lookup_parent(&mut parent_content, dir, path)?;
        context.remaining_follows = parent_content.remaining_follows;

        let must_create = flags.contains(OpenFlags::CREAT) && flags.contains(OpenFlags::EXCL);

        // Lookup the child, without following a symlink or expecting it to be a directory.
        let mut child_context = context.with(SymlinkMode::NoFollow);
        child_context.must_be_directory = false;

        match parent.lookup_child(self, &mut child_context, basename) {
            Ok(name) => {
                if name.entry.node.is_lnk() {
                    if context.symlink_mode == SymlinkMode::NoFollow
                        || context.remaining_follows == 0
                    {
                        if must_create {
                            // Since `must_create` is set, and a node was found, this returns EEXIST
                            // instead of ELOOP.
                            return error!(EEXIST);
                        }
                        // A symlink was found, but too many symlink traversals have been
                        // attempted.
                        return error!(ELOOP);
                    }

                    context.remaining_follows -= 1;
                    match name.readlink(self)? {
                        SymlinkTarget::Path(path) => {
                            let dir = if path[0] == b'/' { self.fs().root() } else { parent };
                            self.resolve_open_path(context, &dir, path.as_ref(), mode, flags)
                        }
                        SymlinkTarget::Node(node) => Ok((node, false)),
                    }
                } else {
                    if must_create {
                        return error!(EEXIST);
                    }
                    Ok((name, false))
                }
            }
            Err(e) if e == errno!(ENOENT) && flags.contains(OpenFlags::CREAT) => {
                if context.must_be_directory {
                    return error!(EISDIR);
                }
                Ok((
                    parent.open_create_node(
                        self,
                        basename,
                        mode.with_type(FileMode::IFREG),
                        DeviceType::NONE,
                        flags,
                    )?,
                    true,
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// The primary entry point for opening files relative to a task.
    ///
    /// Absolute paths are resolve relative to the root of the FsContext for
    /// this task. Relative paths are resolve relative to dir_fd. To resolve
    /// relative to the current working directory, pass FdNumber::AT_FDCWD for
    /// dir_fd.
    ///
    /// Returns a FileHandle but does not install the FileHandle in the FdTable
    /// for this task.
    pub fn open_file_at(
        &self,
        dir_fd: FdNumber,
        path: &FsStr,
        flags: OpenFlags,
        mode: FileMode,
    ) -> Result<FileHandle, Errno> {
        if path.is_empty() {
            return error!(ENOENT);
        }

        let (dir, path) = self.resolve_dir_fd(dir_fd, path)?;
        self.open_namespace_node_at(dir, path, flags, mode)
    }

    pub fn open_namespace_node_at(
        &self,
        dir: NamespaceNode,
        path: &FsStr,
        flags: OpenFlags,
        mode: FileMode,
    ) -> Result<FileHandle, Errno> {
        // 64-bit kernels force the O_LARGEFILE flag to be on.
        let mut flags = flags | OpenFlags::LARGEFILE;
        if flags.contains(OpenFlags::PATH) {
            // When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
            // O_DIRECTORY, and O_NOFOLLOW are ignored.
            const ALLOWED_FLAGS: OpenFlags = OpenFlags::from_bits_truncate(
                OpenFlags::PATH.bits()
                    | OpenFlags::CLOEXEC.bits()
                    | OpenFlags::DIRECTORY.bits()
                    | OpenFlags::NOFOLLOW.bits(),
            );
            flags &= ALLOWED_FLAGS;
        }

        if flags.contains(OpenFlags::TMPFILE) && !flags.can_write() {
            return error!(EINVAL);
        }

        let nofollow = flags.contains(OpenFlags::NOFOLLOW);
        let must_create = flags.contains(OpenFlags::CREAT) && flags.contains(OpenFlags::EXCL);

        let symlink_mode =
            if nofollow || must_create { SymlinkMode::NoFollow } else { SymlinkMode::Follow };

        let mut context = LookupContext::new(symlink_mode);
        context.must_be_directory = flags.contains(OpenFlags::DIRECTORY);
        let (name, created) = match self.resolve_open_path(&mut context, &dir, path, mode, flags) {
            Ok((n, c)) => (n, c),
            Err(e) => {
                let mut abs_path = dir.path(&self.task);
                abs_path.extend(&**path);
                track_file_not_found(abs_path);
                return Err(e);
            }
        };

        let name = if flags.contains(OpenFlags::TMPFILE) {
            name.create_tmpfile(self, mode.with_type(FileMode::IFREG), flags)?
        } else {
            let mode = name.entry.node.info().mode;

            // These checks are not needed in the `O_TMPFILE` case because `mode` refers to the
            // file we are opening. With `O_TMPFILE`, that file is the regular file we just
            // created rather than the node we found by resolving the path.
            //
            // For example, we do not need to produce `ENOTDIR` when `must_be_directory` is set
            // because `must_be_directory` refers to the node we found by resolving the path.
            // If that node was not a directory, then `create_tmpfile` will produce an error.
            //
            // Similarly, we never need to call `truncate` because `O_TMPFILE` is newly created
            // and therefor already an empty file.

            if nofollow && mode.is_lnk() {
                return error!(ELOOP);
            }

            if mode.is_dir() {
                if flags.can_write()
                    || flags.contains(OpenFlags::CREAT)
                    || flags.contains(OpenFlags::TRUNC)
                {
                    return error!(EISDIR);
                }
                if flags.contains(OpenFlags::DIRECT) {
                    return error!(EINVAL);
                }
            } else if context.must_be_directory {
                return error!(ENOTDIR);
            }

            if flags.contains(OpenFlags::TRUNC) && mode.is_reg() && !created {
                // You might think we should check file.can_write() at this
                // point, which is what the docs suggest, but apparently we
                // are supposed to truncate the file if this task can write
                // to the underlying node, even if we are opening the file
                // as read-only. See OpenTest.CanTruncateReadOnly.
                name.truncate(self, 0)?;
            }

            name
        };

        // If the node has been created, the open operation should not verify access right:
        // From <https://man7.org/linux/man-pages/man2/open.2.html>
        //
        // > Note that mode applies only to future accesses of the newly created file; the
        // > open() call that creates a read-only file may well return a  read/write  file
        // > descriptor.

        name.open(self, flags, !created)
    }

    /// A wrapper for FsContext::lookup_parent_at that resolves the given
    /// dir_fd to a NamespaceNode.
    ///
    /// Absolute paths are resolve relative to the root of the FsContext for
    /// this task. Relative paths are resolve relative to dir_fd. To resolve
    /// relative to the current working directory, pass FdNumber::AT_FDCWD for
    /// dir_fd.
    pub fn lookup_parent_at<'a>(
        &self,
        context: &mut LookupContext,
        dir_fd: FdNumber,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        let (dir, path) = self.resolve_dir_fd(dir_fd, path)?;
        self.lookup_parent(context, &dir, path)
    }

    /// Lookup the parent of a namespace node.
    ///
    /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
    /// calling this function directly.
    ///
    /// This function resolves all but the last component of the given path.
    /// The function returns the parent directory of the last component as well
    /// as the last component.
    ///
    /// If path is empty, this function returns dir and an empty path.
    /// Similarly, if path ends with "." or "..", these components will be
    /// returned along with the parent.
    ///
    /// The returned parent might not be a directory.
    pub fn lookup_parent<'a>(
        &self,
        context: &mut LookupContext,
        dir: &NamespaceNode,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        context.update_for_path(path);

        let mut current_node = dir.clone();
        let mut it = path.split(|c| *c == b'/').filter(|p| !p.is_empty()).map(<&FsStr>::from);
        let mut current_path_component = it.next().unwrap_or_default();
        for next_path_component in it {
            current_node = current_node.lookup_child(self, context, current_path_component)?;
            current_path_component = next_path_component;
        }
        Ok((current_node, current_path_component))
    }

    /// Lookup a namespace node.
    ///
    /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
    /// calling this function directly.
    ///
    /// This function resolves the component of the given path.
    pub fn lookup_path(
        &self,
        context: &mut LookupContext,
        dir: NamespaceNode,
        path: &FsStr,
    ) -> Result<NamespaceNode, Errno> {
        let (parent, basename) = self.lookup_parent(context, &dir, path)?;
        parent.lookup_child(self, context, basename)
    }

    /// Lookup a namespace node starting at the root directory.
    ///
    /// Resolves symlinks.
    pub fn lookup_path_from_root(&self, path: &FsStr) -> Result<NamespaceNode, Errno> {
        let mut context = LookupContext::default();
        self.lookup_path(&mut context, self.fs().root(), path)
    }

    pub fn exec(
        &mut self,
        executable: FileHandle,
        path: CString,
        argv: Vec<CString>,
        environ: Vec<CString>,
    ) -> Result<(), Errno> {
        // Executable must be a regular file
        if !executable.name.entry.node.is_reg() {
            return error!(EACCES);
        }

        // File node must have EXEC mode permissions.
        // Note that the ability to execute a file is unrelated to the flags
        // used in the `open` call.
        executable.name.check_access(self, Access::EXEC)?;

        let resolved_elf = resolve_executable(self, executable, path.clone(), argv, environ)?;

        if self.thread_group.read().tasks_count() > 1 {
            not_implemented!(fxb@297434895, "exec on multithread process");
            return error!(EINVAL);
        }

        if let Err(err) = self.finish_exec(path, resolved_elf) {
            log_warn!("unrecoverable error in exec: {err:?}");

            send_standard_signal(
                self,
                SignalInfo { code: SI_KERNEL as i32, force: true, ..SignalInfo::default(SIGSEGV) },
            );
            return Err(err);
        }

        self.signal_vfork();

        Ok(())
    }

    /// After the memory is unmapped, any failure in exec is unrecoverable and results in the
    /// process crashing. This function is for that second half; any error returned from this
    /// function will be considered unrecoverable.
    fn finish_exec(&mut self, path: CString, resolved_elf: ResolvedElf) -> Result<(), Errno> {
        // Now that the exec will definitely finish (or crash), notify owners of
        // locked futexes for the current process, which will be impossible to
        // update after process image is replaced.  See get_robust_list(2).
        self.notify_robust_list();

        self.mm()
            .exec(resolved_elf.file.name.clone())
            .map_err(|status| from_status_like_fdio!(status))?;
        let start_info = load_executable(self, resolved_elf, &path)?;
        let regs: zx_thread_state_general_regs_t = start_info.into();
        self.thread_state.registers = regs.into();

        {
            let mut state = self.write();
            let mut persistent_info = self.persistent_info.lock();
            state.signals.alt_stack = None;
            state.robust_list_head = UserAddress::NULL.into();

            // TODO(tbodt): Check whether capability xattrs are set on the file, and grant/limit
            // capabilities accordingly.
            persistent_info.creds_mut().exec();
        }
        self.thread_state.extended_pstate.reset();

        self.thread_group.signal_actions.reset_for_exec();

        // TODO: The termination signal is reset to SIGCHLD.

        // TODO(https://fxbug.dev/132623): All threads other than the calling thread are destroyed.

        // TODO: The file descriptor table is unshared, undoing the effect of
        //       the CLONE_FILES flag of clone(2).
        //
        // To make this work, we can put the files in an RwLock and then cache
        // a reference to the files on the CurrentTask. That will let
        // functions that have CurrentTask access the FdTable without
        // needing to grab the read-lock.
        //
        // For now, we do not implement that behavior.
        self.files.exec();

        // TODO: POSIX timers are not preserved.

        self.thread_group.write().did_exec = true;

        // Get the basename of the path, which will be used as the name displayed with
        // `prctl(PR_GET_NAME)` and `/proc/self/stat`
        let basename = if let Some(idx) = memchr::memrchr(b'/', path.to_bytes()) {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&path.to_bytes()[idx + 1..]).unwrap()
        } else {
            path
        };
        set_zx_name(&fuchsia_runtime::thread_self(), basename.as_bytes());
        self.set_command_name(basename);

        Ok(())
    }

    pub fn add_seccomp_filter(
        &mut self,
        bpf_filter: UserAddress,
        flags: u32,
    ) -> Result<SyscallResult, Errno> {
        let fprog: sock_fprog = self.read_object(UserRef::new(bpf_filter))?;

        if u32::from(fprog.len) > BPF_MAXINSNS || fprog.len == 0 {
            return Err(errno!(EINVAL));
        }

        let code: Vec<sock_filter> =
            self.read_objects_to_vec(fprog.filter.into(), fprog.len as usize)?;

        let new_filter = Arc::new(SeccompFilter::from_cbpf(
            &code,
            self.thread_group.next_seccomp_filter_id.add(1),
            flags & SECCOMP_FILTER_FLAG_LOG != 0,
        )?);

        let mut maybe_fd: Option<FdNumber> = None;

        if flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0 {
            let mut task_state = self.task.write();
            maybe_fd = Some(task_state.seccomp_filters.create_listener(self)?);
        }

        // We take the process lock here because we can't change any of the threads
        // while doing a tsync.  So, you hold the process lock while making any changes.
        let state = self.thread_group.write();

        if flags & SECCOMP_FILTER_FLAG_TSYNC != 0 {
            // TSYNC synchronizes all filters for all threads in the current process to
            // the current thread's

            // We collect the filters for the current task upfront to save us acquiring
            // the task's lock a lot of times below.
            let mut filters: SeccompFilterContainer = self.read().seccomp_filters.clone();

            // For TSYNC to work, all of the other thread filters in this process have to
            // be a prefix of this thread's filters, and none of them can be in
            // strict mode.
            let tasks = state.tasks().collect::<Vec<_>>();
            for task in &tasks {
                if task.id == self.id {
                    continue;
                }
                let other_task_state = task.read();

                // Target threads cannot be in SECCOMP_MODE_STRICT
                if task.seccomp_filter_state.get() == SeccompStateValue::Strict {
                    return Self::seccomp_tsync_error(task.id, flags);
                }

                // Target threads' filters must be a subsequence of this thread's
                if !other_task_state.seccomp_filters.can_sync_to(&filters) {
                    return Self::seccomp_tsync_error(task.id, flags);
                }
            }

            // Now that we're sure we're allowed to do so, add the filter to all threads.
            filters.add_filter(new_filter, fprog.len)?;

            for task in &tasks {
                let mut other_task_state = task.write();

                other_task_state.enable_no_new_privs();
                other_task_state.seccomp_filters = filters.clone();
                task.set_seccomp_state(SeccompStateValue::UserDefined)?;
            }
        } else {
            let mut task_state = self.task.write();

            task_state.seccomp_filters.add_filter(new_filter, fprog.len)?;
            self.set_seccomp_state(SeccompStateValue::UserDefined)?;
        }

        if let Some(fd) = maybe_fd {
            Ok(fd.into())
        } else {
            Ok(().into())
        }
    }

    pub fn run_seccomp_filters(
        &mut self,
        syscall: &Syscall,
    ) -> Option<Result<SyscallResult, Errno>> {
        profile_duration!("RunSeccompFilters");
        // Implementation of SECCOMP_FILTER_STRICT, which has slightly different semantics
        // from user-defined seccomp filters.
        if self.seccomp_filter_state.get() == SeccompStateValue::Strict {
            return SeccompState::do_strict(self, syscall);
        }

        // Run user-defined seccomp filters
        let result = self.task.read().seccomp_filters.run_all(self, syscall);

        SeccompState::do_user_defined(result, self, syscall)
    }

    fn seccomp_tsync_error(id: i32, flags: u32) -> Result<SyscallResult, Errno> {
        // By default, TSYNC indicates failure state by returning the first thread
        // id not to be able to sync, rather than by returning -1 and setting
        // errno.  However, if TSYNC_ESRCH is set, it returns ESRCH.  This
        // prevents conflicts with fact that SECCOMP_FILTER_FLAG_NEW_LISTENER
        // makes seccomp return an fd.
        if flags & SECCOMP_FILTER_FLAG_TSYNC_ESRCH != 0 {
            Err(errno!(ESRCH))
        } else {
            Ok(id.into())
        }
    }

    // Notify all futexes in robust list.  The robust list is in user space, so we
    // are very careful about walking it, and there are a lot of quiet returns if
    // we fail to walk it.
    // TODO(https://fxbug.dev/128610): This only sets the FUTEX_OWNER_DIED bit; it does
    // not wake up a waiter.
    pub fn notify_robust_list(&self) {
        let task_state = self.write();
        let robust_list_addr = task_state.robust_list_head.addr();
        if robust_list_addr == UserAddress::NULL {
            // No one has called set_robust_list.
            return;
        }
        let robust_list_res = self.read_object(task_state.robust_list_head);

        let head = if let Ok(head) = robust_list_res {
            head
        } else {
            return;
        };

        let offset = head.futex_offset;

        let mut entries_count = 0;
        let mut curr_ptr = head.list.next;
        while curr_ptr.addr != robust_list_addr.into() && entries_count < ROBUST_LIST_LIMIT {
            let curr_ref = self.read_object(curr_ptr.into());

            let curr = if let Ok(curr) = curr_ref {
                curr
            } else {
                return;
            };

            let futex_base: u64;
            if let Some(fb) = curr_ptr.addr.addr.checked_add_signed(offset) {
                futex_base = fb;
            } else {
                return;
            }

            let futex_addr = UserAddress::from(futex_base);
            // TODO - What if this isn't 4 byte aligned?

            let futex = if let Ok(futex) = self.mm().atomic_load_u32_relaxed(futex_addr) {
                futex
            } else {
                return;
            };

            if (futex & FUTEX_TID_MASK) as i32 == self.id {
                let owner_died = FUTEX_OWNER_DIED | futex;
                if self.mm().atomic_store_u32_relaxed(futex_addr, owner_died).is_err() {
                    return;
                }
            }
            curr_ptr = curr.next;
            entries_count += 1;
        }
    }

    /// Returns a ref to this thread's SeccompNotifier.
    pub fn get_seccomp_notifier(&mut self) -> Option<SeccompNotifierHandle> {
        self.task.write().seccomp_filters.notifier.clone()
    }

    pub fn set_seccomp_notifier(&mut self, notifier: Option<SeccompNotifierHandle>) {
        self.task.write().seccomp_filters.notifier = notifier;
    }

    /// Processes a Zircon exception associated with this task.
    pub fn process_exception(&self, report: &zx::sys::zx_exception_report_t) -> ExceptionResult {
        match report.header.type_ {
            zx::sys::ZX_EXCP_GENERAL => match get_signal_for_general_exception(&report.context) {
                Some(sig) => ExceptionResult::Signal(SignalInfo::default(sig)),
                None => {
                    log_warn!("Unrecognized general exception: {:?}", report);
                    ExceptionResult::Signal(SignalInfo::default(SIGILL))
                }
            },
            zx::sys::ZX_EXCP_FATAL_PAGE_FAULT => self.mm().handle_page_fault(
                decode_page_fault_exception_report(report),
                zx::Status::from_raw(report.context.synth_code as zx::zx_status_t),
            ),
            zx::sys::ZX_EXCP_UNDEFINED_INSTRUCTION => {
                ExceptionResult::Signal(SignalInfo::default(SIGILL))
            }
            zx::sys::ZX_EXCP_UNALIGNED_ACCESS => {
                ExceptionResult::Signal(SignalInfo::default(SIGBUS))
            }
            zx::sys::ZX_EXCP_SW_BREAKPOINT => ExceptionResult::Signal(SignalInfo::default(SIGTRAP)),
            unknown => {
                not_implemented!("zircon exception", unknown);
                log_error!("Unknown exception {:?}", report);
                ExceptionResult::Signal(SignalInfo::default(SIGSEGV))
            }
        }
    }

    /// Create a process that is a child of the `init` process.
    ///
    /// The created process will be a task that is the leader of a new thread group.
    ///
    /// Most processes are created by userspace and are descendants of the `init` process. In
    /// some situations, the kernel needs to create a process itself. This function is the
    /// preferred way of creating an actual userspace process because making the process a child of
    /// `init` means that `init` is responsible for waiting on the process when it dies and thereby
    /// cleaning up its zombie.
    ///
    /// If you just need a kernel task, and not an entire userspace process, consider using
    /// `create_system_task` instead. Even better, consider using the `kthreads` threadpool.
    ///
    /// This function creates an underlying Zircon process to host the new task.
    pub fn create_init_child_process<L>(
        locked: &mut Locked<'_, L>,
        kernel: &Arc<Kernel>,
        initial_name: &CString,
    ) -> Result<TaskBuilder, Errno>
    where
        L: LockBefore<TaskRelease>,
    {
        let weak_init = kernel.pids.read().get_task(1);
        let init_task = weak_init.upgrade().ok_or_else(|| errno!(EINVAL))?;
        let initial_name_bytes = initial_name.as_bytes().to_owned();
        let task = Self::create_task(
            locked,
            kernel,
            initial_name.clone(),
            init_task.fs().fork(),
            |locked, pid, process_group| {
                create_zircon_process(
                    locked,
                    kernel,
                    None,
                    pid,
                    process_group,
                    SignalActions::default(),
                    &initial_name_bytes,
                )
            },
        )?;
        {
            let mut init_writer = init_task.thread_group.write();
            let mut new_process_writer = task.thread_group.write();
            new_process_writer.parent = Some(init_task.thread_group.clone());
            init_writer.children.insert(task.id, Arc::downgrade(&task.thread_group));
        }
        // A child process created via fork(2) inherits its parent's
        // resource limits.  Resource limits are preserved across execve(2).
        let limits = init_task.thread_group.limits.lock().clone();
        *task.thread_group.limits.lock() = limits;
        Ok(task)
    }

    /// Creates the initial process for a kernel.
    ///
    /// The created process will be a task that is the leader of a new thread group.
    ///
    /// The init process is special because it's the root of the parent/child relationship between
    /// tasks. If a task dies, the init process is ultimately responsible for waiting on that task
    /// and removing it from the zombie list.
    ///
    /// It's possible for the kernel to create tasks whose ultimate parent isn't init, but such
    /// tasks cannot be created by userspace directly.
    ///
    /// This function should only be called as part of booting a kernel instance. To create a
    /// process after the kernel has already booted, consider `create_init_child_process`
    /// or `create_system_task`.
    ///
    /// The process created by this function should always have pid 1. We require the caller to
    /// pass the `pid` as an argument to clarify that it's the callers responsibility to determine
    /// the pid for the process.
    pub fn create_init_process<L>(
        locked: &mut Locked<'_, L>,
        kernel: &Arc<Kernel>,
        pid: pid_t,
        initial_name: CString,
        fs: Arc<FsContext>,
    ) -> Result<TaskBuilder, Errno>
    where
        L: LockBefore<TaskRelease>,
    {
        let initial_name_bytes = initial_name.as_bytes().to_owned();
        let pids = kernel.pids.write();
        Self::create_task_with_pid(
            locked,
            kernel,
            pids,
            pid,
            initial_name,
            fs,
            |locked, pid, process_group| {
                create_zircon_process(
                    locked,
                    kernel,
                    None,
                    pid,
                    process_group,
                    SignalActions::default(),
                    &initial_name_bytes,
                )
            },
        )
    }

    /// Create a task that runs inside the kernel.
    ///
    /// There is no underlying Zircon process to host the task. Instead, the work done by this task
    /// is performed by a thread in the original Starnix process, possible as part of a thread
    /// pool.
    ///
    /// This function is the preferred way to create a context for doing background work inside the
    /// kernel.
    ///
    /// Rather than calling this function directly, consider using `kthreads`, which provides both
    /// a system task and a threadpool on which the task can do work.
    pub fn create_system_task<L>(
        locked: &mut Locked<'_, L>,
        kernel: &Arc<Kernel>,
        fs: Arc<FsContext>,
    ) -> Result<CurrentTask, Errno>
    where
        L: LockBefore<TaskRelease>,
    {
        let builder = Self::create_task(
            locked,
            kernel,
            CString::new("[kthreadd]").unwrap(),
            fs,
            |locked, pid, process_group| {
                let process = zx::Process::from(zx::Handle::invalid());
                let memory_manager = Arc::new(MemoryManager::new_empty());
                let thread_group = ThreadGroup::new(
                    locked,
                    kernel.clone(),
                    process,
                    None,
                    pid,
                    process_group,
                    SignalActions::default(),
                );
                Ok(TaskInfo { thread: None, thread_group, memory_manager })
            },
        )?;
        Ok(builder.into())
    }

    fn create_task<F, L>(
        locked: &mut Locked<'_, L>,
        kernel: &Arc<Kernel>,
        initial_name: CString,
        root_fs: Arc<FsContext>,
        task_info_factory: F,
    ) -> Result<TaskBuilder, Errno>
    where
        F: FnOnce(&mut Locked<'_, L>, i32, Arc<ProcessGroup>) -> Result<TaskInfo, Errno>,
        L: LockBefore<TaskRelease>,
    {
        let mut pids = kernel.pids.write();
        let pid = pids.allocate_pid();
        Self::create_task_with_pid(
            locked,
            kernel,
            pids,
            pid,
            initial_name,
            root_fs,
            task_info_factory,
        )
    }

    fn create_task_with_pid<F, L>(
        locked: &mut Locked<'_, L>,
        kernel: &Arc<Kernel>,
        mut pids: RwLockWriteGuard<'_, PidTable>,
        pid: pid_t,
        initial_name: CString,
        root_fs: Arc<FsContext>,
        task_info_factory: F,
    ) -> Result<TaskBuilder, Errno>
    where
        F: FnOnce(&mut Locked<'_, L>, i32, Arc<ProcessGroup>) -> Result<TaskInfo, Errno>,
        L: LockBefore<TaskRelease>,
    {
        debug_assert!(pids.get_task(pid).upgrade().is_none());

        let process_group = ProcessGroup::new(pid, None);
        pids.add_process_group(&process_group);

        let TaskInfo { thread, thread_group, memory_manager } =
            task_info_factory(locked, pid, process_group.clone())?;

        process_group.insert(locked, &thread_group);

        // > The timer slack values of init (PID 1), the ancestor of all processes, are 50,000
        // > nanoseconds (50 microseconds).  The timer slack value is inherited by a child created
        // > via fork(2), and is preserved across execve(2).
        // https://man7.org/linux/man-pages/man2/prctl.2.html
        let default_timerslack = 50_000;
        let builder = TaskBuilder {
            task: OwnedRef::new(Task::new(
                pid,
                initial_name,
                thread_group,
                thread,
                FdTable::default(),
                memory_manager,
                root_fs,
                Credentials::root(),
                Arc::clone(&kernel.default_abstract_socket_namespace),
                Arc::clone(&kernel.default_abstract_vsock_namespace),
                Some(SIGCHLD),
                Default::default(),
                None,
                Default::default(),
                kernel.root_uts_ns.clone(),
                false,
                SeccompState::default(),
                SeccompFilterContainer::default(),
                UserAddress::NULL.into(),
                default_timerslack,
            )),
            thread_state: Default::default(),
        };
        release_on_error!(builder, locked, {
            let temp_task = TempRef::from(&builder.task);
            builder.thread_group.add(&temp_task)?;

            pids.add_task(&temp_task);
            pids.add_thread_group(&builder.thread_group);
            Ok(())
        });
        Ok(builder)
    }

    /// Create a kernel task in the same ThreadGroup as the given `system_task`.
    ///
    /// There is no underlying Zircon thread to host the task.
    pub fn create_kernel_thread<L>(
        locked: &mut Locked<'_, L>,
        system_task: &Task,
        initial_name: CString,
    ) -> Result<CurrentTask, Errno>
    where
        L: LockBefore<TaskRelease>,
    {
        let mut pids = system_task.kernel().pids.write();
        let pid = pids.allocate_pid();

        let scheduler_policy;
        let uts_ns;
        let default_timerslack_ns;
        {
            let state = system_task.read();
            scheduler_policy = state.scheduler_policy;
            uts_ns = state.uts_ns.clone();
            default_timerslack_ns = state.default_timerslack_ns;
        }

        let current_task: CurrentTask = TaskBuilder::new(Task::new(
            pid,
            initial_name,
            Arc::clone(&system_task.thread_group),
            None,
            FdTable::default(),
            Arc::clone(system_task.mm()),
            Arc::clone(system_task.fs()),
            system_task.creds(),
            Arc::clone(&system_task.abstract_socket_namespace),
            Arc::clone(&system_task.abstract_vsock_namespace),
            None,
            Default::default(),
            None,
            scheduler_policy,
            uts_ns,
            false,
            SeccompState::default(),
            SeccompFilterContainer::default(),
            UserAddress::NULL.into(),
            default_timerslack_ns,
        ))
        .into();
        release_on_error!(current_task, locked, {
            let temp_task = current_task.temp_task();
            current_task.thread_group.add(&temp_task)?;
            pids.add_task(&temp_task);
            Ok(())
        });
        Ok(current_task)
    }

    /// Clone this task.
    ///
    /// Creates a new task object that shares some state with this task
    /// according to the given flags.
    ///
    /// Used by the clone() syscall to create both processes and threads.
    ///
    /// The exit signal is broken out from the flags parameter like clone3() rather than being
    /// bitwise-ORed like clone().
    pub fn clone_task<L>(
        &self,
        locked: &mut Locked<'_, L>,
        flags: u64,
        child_exit_signal: Option<Signal>,
        user_parent_tid: UserRef<pid_t>,
        user_child_tid: UserRef<pid_t>,
    ) -> Result<TaskBuilder, Errno>
    where
        L: LockBefore<MmDumpable>,
        L: LockBefore<TaskRelease>,
    {
        // TODO: Implement more flags.
        const IMPLEMENTED_FLAGS: u64 = (CLONE_VM
            | CLONE_FS
            | CLONE_FILES
            | CLONE_SIGHAND
            | CLONE_THREAD
            | CLONE_SYSVSEM
            | CLONE_SETTLS
            | CLONE_PARENT_SETTID
            | CLONE_CHILD_CLEARTID
            | CLONE_CHILD_SETTID
            | CLONE_VFORK
            | CLONE_PTRACE) as u64;
        // A mask with all valid flags set, because we want to return a different error code for an
        // invalid flag vs an unimplemented flag. Subtracting 1 from the largest valid flag gives a
        // mask with all flags below it set. Shift up by one to make sure the largest flag is also
        // set.
        const VALID_FLAGS: u64 = (CLONE_INTO_CGROUP << 1) - 1;

        // CLONE_SETTLS is implemented by sys_clone.

        let clone_thread = flags & (CLONE_THREAD as u64) != 0;
        let clone_vm = flags & (CLONE_VM as u64) != 0;
        let clone_sighand = flags & (CLONE_SIGHAND as u64) != 0;
        let clone_vfork = flags & (CLONE_VFORK as u64) != 0;

        let new_uts = flags & (CLONE_NEWUTS as u64) != 0;

        if clone_sighand && !clone_vm {
            return error!(EINVAL);
        }
        if clone_thread && !clone_sighand {
            return error!(EINVAL);
        }
        if flags & !VALID_FLAGS != 0 {
            return error!(EINVAL);
        }

        if clone_vm && !clone_thread {
            // TODO(https://fxbug.dev/114813) Implement CLONE_VM for child processes (not just child
            // threads). Currently this executes CLONE_VM (explicitly passed to clone() or as
            // used by vfork()) as a fork (the VM in the child is copy-on-write) which is almost
            // always OK.
            //
            // CLONE_VM is primarily as an optimization to avoid making a copy-on-write version of a
            // process' VM that will be immediately replaced with a call to exec(). The main users
            // (libc and language runtimes) don't actually rely on the memory being shared between
            // the two processes. And the vfork() man page explicitly allows vfork() to be
            // implemented as fork() which is what we do here.
            if !clone_vfork {
                log_warn!("CLONE_VM set without CLONE_THREAD. Ignoring CLONE_VM (doing a fork).");
            }
        } else if clone_thread && !clone_vm {
            not_implemented!("CLONE_THREAD without CLONE_VM");
            return error!(ENOSYS);
        }

        if flags & !IMPLEMENTED_FLAGS != 0 {
            not_implemented!("clone", flags & !IMPLEMENTED_FLAGS);
            return error!(ENOSYS);
        }

        let fs = if flags & (CLONE_FS as u64) != 0 { self.fs().clone() } else { self.fs().fork() };
        let files =
            if flags & (CLONE_FILES as u64) != 0 { self.files.clone() } else { self.files.fork() };

        let kernel = self.kernel();
        let mut pids = kernel.pids.write();

        let pid;
        let command;
        let creds;
        let scheduler_policy;
        let uts_ns;
        let no_new_privs;
        let seccomp_filters;
        let robust_list_head = UserAddress::NULL.into();
        let child_signal_mask;
        let timerslack_ns;

        let TaskInfo { thread, thread_group, memory_manager } = {
            // Make sure to drop these locks ASAP to avoid inversion
            let thread_group_state = self.thread_group.write();
            let state = self.read();

            no_new_privs = state.no_new_privs();
            seccomp_filters = state.seccomp_filters.clone();
            child_signal_mask = state.signals.mask();

            pid = pids.allocate_pid();
            command = self.command();
            creds = self.creds();
            scheduler_policy = state.scheduler_policy.fork();
            timerslack_ns = state.timerslack_ns;

            uts_ns = if new_uts {
                if !self.creds().has_capability(CAP_SYS_ADMIN) {
                    return error!(EPERM);
                }

                // Fork the UTS namespace of the existing task.
                let new_uts_ns = state.uts_ns.read().clone();
                Arc::new(RwLock::new(new_uts_ns))
            } else {
                // Inherit the UTS of the existing task.
                state.uts_ns.clone()
            };

            if clone_thread {
                let thread_group = self.thread_group.clone();
                let memory_manager = self.mm().clone();
                TaskInfo { thread: None, thread_group, memory_manager }
            } else {
                // Drop the lock on this task before entering `create_zircon_process`, because it will
                // take a lock on the new thread group, and locks on thread groups have a higher
                // priority than locks on the task in the thread group.
                std::mem::drop(state);
                let signal_actions = if clone_sighand {
                    self.thread_group.signal_actions.clone()
                } else {
                    self.thread_group.signal_actions.fork()
                };
                let process_group = thread_group_state.process_group.clone();
                create_zircon_process(
                    locked,
                    kernel,
                    Some(thread_group_state),
                    pid,
                    process_group,
                    signal_actions,
                    command.as_bytes(),
                )?
            }
        };

        // Only create the vfork event when the caller requested CLONE_VFORK.
        let vfork_event = if flags & (CLONE_VFORK as u64) != 0 {
            Some(Arc::new(zx::Event::create()))
        } else {
            None
        };

        let mut child = TaskBuilder::new(Task::new(
            pid,
            command,
            thread_group,
            thread,
            files,
            memory_manager,
            fs,
            creds,
            self.abstract_socket_namespace.clone(),
            self.abstract_vsock_namespace.clone(),
            child_exit_signal,
            child_signal_mask,
            vfork_event,
            scheduler_policy,
            uts_ns,
            no_new_privs,
            SeccompState::from(&self.seccomp_filter_state),
            seccomp_filters,
            robust_list_head,
            timerslack_ns,
        ));

        release_on_error!(child, locked, {
            let child_task = TempRef::from(&child.task);
            // Drop the pids lock as soon as possible after creating the child. Destroying the child
            // and removing it from the pids table itself requires the pids lock, so if an early exit
            // takes place we have a self deadlock.
            pids.add_task(&child_task);
            if !clone_thread {
                pids.add_thread_group(&child.thread_group);
            }
            std::mem::drop(pids);

            // Child lock must be taken before this lock. Drop the lock on the task, take a writable
            // lock on the child and take the current state back.

            #[cfg(any(test, debug_assertions))]
            {
                // Take the lock on the thread group and its child in the correct order to ensure any wrong ordering
                // will trigger the tracing-mutex at the right call site.
                if !clone_thread {
                    let _l1 = self.thread_group.read();
                    let _l2 = child.thread_group.read();
                }
            }

            if clone_thread {
                self.thread_group.add(&child_task)?;
            } else {
                child.thread_group.add(&child_task)?;
                let mut child_state = child.write();
                let state = self.read();
                child_state.signals.alt_stack = state.signals.alt_stack;
                child_state.signals.set_mask(state.signals.mask());
                self.mm().snapshot_to(locked, child.mm())?;
            }

            if flags & (CLONE_PARENT_SETTID as u64) != 0 {
                self.write_object(user_parent_tid, &child.id)?;
            }

            if flags & (CLONE_CHILD_CLEARTID as u64) != 0 {
                child.write().clear_child_tid = user_child_tid;
            }

            if flags & (CLONE_CHILD_SETTID as u64) != 0 {
                child.write_object(user_child_tid, &child.id)?;
            }
            child.thread_state = self.thread_state.snapshot();
            Ok(())
        });
        // Take the lock on thread group and task in the correct order to ensure any wrong ordering
        // will trigger the tracing-mutex at the right call site.
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = child.thread_group.read();
            let _l2 = child.read();
        }

        Ok(child)
    }

    /// Sets the stop state (per set_stopped), and also notifies all listeners,
    /// including the parent process if appropriate.
    pub fn set_stopped_and_notify(&self, stopped: StopState, siginfo: Option<SignalInfo>) {
        {
            let mut state = self.write();
            if let Some(ref mut ptrace) = &mut state.ptrace {
                ptrace.copy_state_from(self);
            }
            state.set_stopped(stopped, siginfo, Some(self), None);
        }

        if !stopped.is_in_progress() {
            let parent = self.thread_group.read().parent.clone();
            if let Some(parent) = parent {
                parent.write().child_status_waiters.notify_all();
            }
        }
    }

    /// If the task is stopping, set it as stopped. return whether the caller
    /// should stop.
    pub fn finalize_stop_state(&self) -> bool {
        // Stopping because the thread group is stopping.
        // Try to flip to GroupStopped - will fail if we shouldn't.
        if self.thread_group.set_stopped(StopState::GroupStopped, None, true)
            == StopState::GroupStopped
        {
            let signal = self.thread_group.read().last_signal.clone();
            // stopping because the thread group has stopped
            let event = Some(PtraceEventData::new_from_event(PtraceEvent::Stop, 0));
            self.write().set_stopped(StopState::GroupStopped, signal, Some(self), event);
            return true;
        }

        // Stopping because the task is stopping
        let stopped = self.load_stopped();
        if stopped.is_stopping_or_stopped() {
            if let Ok(stopped) = stopped.finalize() {
                self.set_stopped_and_notify(stopped, None);
            }
            return true;
        }

        false
    }

    /// Block the execution of `current_task` as long as the task is stopped and
    /// not terminated.
    pub fn block_while_stopped(&mut self) {
        // Upgrade the state from stopping to stopped if needed. Return if the task
        // should not be stopped.
        if !self.finalize_stop_state() {
            return;
        }

        let waiter = Waiter::new_ignoring_signals();
        loop {
            // If we've exited, unstop the threads and return without notifying
            // waiters.
            if self.is_exitted() {
                self.thread_group.set_stopped(StopState::ForceAwake, None, false);
                self.write().set_stopped(StopState::ForceAwake, None, Some(self), None);
                return;
            }

            if self.wake_or_wait_until_unstopped_async(&waiter) {
                return;
            }

            // Do the wait. Result is not needed, as this is not in a syscall.
            let _: Result<(), Errno> = waiter.wait(self);

            // Maybe go from stopping to stopped, if we are currently stopping
            // again.
            self.finalize_stop_state();
        }
    }

    /// For traced tasks, this will return the data neceessary for a cloned task
    /// to attach to the same tracer.
    pub fn get_ptrace_core_state_for_clone(
        &mut self,
        clone_args: &clone_args,
    ) -> (PtraceOptions, Option<PtraceCoreState>) {
        let state = self.write();
        if let Some(ref ptrace) = &state.ptrace {
            ptrace.get_core_state_for_clone(clone_args)
        } else {
            (PtraceOptions::empty(), None)
        }
    }

    /// If currently being ptraced with the given option, emit the appropriate
    /// event.  PTRACE_EVENTMSG will return the given message.
    pub fn ptrace_event(&mut self, trace_kind: PtraceOptions, msg: u32) {
        if !trace_kind.is_empty() {
            {
                let mut state = self.write();

                if state.ptrace.as_ref().map_or(true, |ptrace| !ptrace.has_option(trace_kind)) {
                    return;
                }
                state.set_stopped(
                    StopState::PtraceEventStopping,
                    Some(SignalInfo::default(starnix_uapi::signals::SIGTRAP)),
                    None,
                    Some(PtraceEventData::new(trace_kind, msg)),
                );
            }
            self.block_while_stopped();
        }
    }

    /// The flags indicates only the flags as in clone3(), and does not use the low 8 bits for the
    /// exit signal as in clone().
    pub fn clone_task_for_test<L>(
        &self,
        locked: &mut Locked<'_, L>,
        flags: u64,
        exit_signal: Option<Signal>,
    ) -> crate::testing::AutoReleasableTask
    where
        L: LockBefore<MmDumpable>,
        L: LockBefore<TaskRelease>,
    {
        let result = self
            .clone_task(locked, flags, exit_signal, UserRef::default(), UserRef::default())
            .expect("failed to create task in test");

        result.into()
    }
}

impl MemoryAccessor for CurrentTask {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().unified_read_memory(self, addr, bytes)
    }

    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().unified_read_memory_partial_until_null_byte(self, addr, bytes)
    }

    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().unified_read_memory_partial(self, addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().unified_write_memory(self, addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().unified_write_memory_partial(self, addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.mm().unified_zero(self, addr, length)
    }
}

impl TaskMemoryAccessor for CurrentTask {
    fn maximum_valid_address(&self) -> UserAddress {
        self.mm().maximum_valid_user_address
    }
}

pub enum ExceptionResult {
    /// The exception was handled and no further action is required.
    Handled,

    // The exception generated a signal that should be delivered.
    Signal(SignalInfo),
}
