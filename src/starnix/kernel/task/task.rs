// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{DumpPolicy, MemoryAccessor, MemoryAccessorExt, MemoryManager},
    signals::{SignalInfo, SignalState},
    task::{
        set_thread_role, AbstractUnixSocketNamespace, AbstractVsockSocketNamespace, CurrentTask,
        Kernel, ProcessEntryRef, PtraceState, PtraceStatus, SchedulerPolicy,
        SeccompFilterContainer, SeccompState, SeccompStateValue, ThreadGroup, ThreadState,
        UtsNamespaceHandle, Waiter,
    },
    vfs::{FdFlags, FdNumber, FdTable, FileHandle, FsContext, FsString},
};
use bitflags::bitflags;
use fuchsia_inspect_contrib::profile_duration;
use fuchsia_zircon::{
    AsHandleRef, Signals, Task as _, {self as zx},
};
use once_cell::sync::OnceCell;
use starnix_logging::{
    log_debug, log_warn, set_zx_name, {self},
};
use starnix_sync::{LockBefore, Locked, MmDumpable};
use starnix_sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use starnix_uapi::{
    auth::{
        Credentials, FsCred, PtraceAccessMode, CAP_KILL, CAP_SYS_PTRACE, PTRACE_MODE_FSCREDS,
        PTRACE_MODE_REALCREDS,
    },
    errno, error,
    errors::Errno,
    from_status_like_fdio,
    ownership::{OwnedRef, Releasable, ReleasableByRef, ReleaseGuard, TempRef, WeakRef},
    pid_t, robust_list_head,
    signals::{SigSet, Signal, UncheckedSignal, SIGCONT, SIGTRAP},
    stats::TaskTimeStats,
    ucred,
    user_address::{UserAddress, UserRef},
    CLD_CONTINUED, CLD_DUMPED, CLD_EXITED, CLD_KILLED, CLD_STOPPED, FUTEX_BITSET_MATCH_ANY,
    PTRACE_EVENT_STOP,
};
use std::{
    cmp,
    convert::TryFrom,
    ffi::CString,
    fmt,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Exit(u8),
    Kill(SignalInfo),
    CoreDump(SignalInfo),
    Stop(SignalInfo),
    Continue(SignalInfo),
}
impl ExitStatus {
    /// Converts the given exit status to a status code suitable for returning from wait syscalls.
    pub fn wait_status(&self) -> i32 {
        let maybe_ptrace = |siginfo: &SignalInfo| {
            if ((siginfo.code >> 8) as u32) == PTRACE_EVENT_STOP {
                (PTRACE_EVENT_STOP << 16) as i32
            } else {
                0
            }
        };
        match self {
            ExitStatus::Exit(status) => (*status as i32) << 8,
            ExitStatus::Kill(siginfo) => siginfo.signal.number() as i32,
            ExitStatus::CoreDump(siginfo) => (siginfo.signal.number() as i32) | 0x80,
            ExitStatus::Continue(siginfo) => {
                if maybe_ptrace(siginfo) != 0 {
                    (siginfo.signal.number() as i32) | maybe_ptrace(siginfo)
                } else {
                    0xffff
                }
            }
            ExitStatus::Stop(siginfo) => {
                (0x7f + ((siginfo.signal.number() as i32) << 8)) | maybe_ptrace(siginfo)
            }
        }
    }

    pub fn signal_info_code(&self) -> i32 {
        match self {
            ExitStatus::Exit(_) => CLD_EXITED as i32,
            ExitStatus::Kill(_) => CLD_KILLED as i32,
            ExitStatus::CoreDump(_) => CLD_DUMPED as i32,
            ExitStatus::Stop(_) => CLD_STOPPED as i32,
            ExitStatus::Continue(_) => CLD_CONTINUED as i32,
        }
    }

    pub fn signal_info_status(&self) -> i32 {
        match self {
            ExitStatus::Exit(status) => *status as i32,
            ExitStatus::Kill(siginfo)
            | ExitStatus::CoreDump(siginfo)
            | ExitStatus::Continue(siginfo)
            | ExitStatus::Stop(siginfo) => siginfo.signal.number() as i32,
        }
    }
}

pub struct AtomicStopState {
    inner: AtomicU8,
}

impl AtomicStopState {
    pub fn new(state: StopState) -> Self {
        Self { inner: AtomicU8::new(state as u8) }
    }

    pub fn load(&self, ordering: Ordering) -> StopState {
        let v = self.inner.load(ordering);
        // SAFETY: we only ever store to the atomic a value originating
        // from a valid `StopState`.
        unsafe { std::mem::transmute(v) }
    }

    pub fn store(&self, state: StopState, ordering: Ordering) {
        self.inner.store(state as u8, ordering)
    }
}

/// This enum describes the state that a task or thread group can be in when being stopped.
/// The names are taken from ptrace(2).
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum StopState {
    /// In this state, the process has been told to wake up, but has not yet been woken.
    /// Individual threads may still be stopped.
    Waking,
    /// In this state, at least one thread is awake.
    Awake,
    /// Same as the above, but you are not allowed to make further transitions.  Used
    /// to kill the task / group.  These names are not in ptrace(2).
    ForceWaking,
    ForceAwake,

    /// In this state, the process has been told to stop via a signal, but has not yet stopped.
    GroupStopping,
    /// In this state, at least one thread of the process has stopped
    GroupStopped,
    /// In this state, the task has received a signal, and it is being traced, so it will
    /// stop at the next opportunity.
    SignalDeliveryStopping,
    /// Same as the last one, but has stopped.
    SignalDeliveryStopped,
    /// Stop for a ptrace event: a variety of events defined by ptrace and
    /// enabled with the use of various ptrace features, such as the
    /// PTRACE_O_TRACE_* options.  The parameter indicates the type of
    /// event. Examples include PTRACE_EVENT_FORK (the event is a fork),
    /// PTRACE_EVENT_EXEC (the event is exec), and other similar events.
    PtraceEventStopping,
    /// Same as the last one, but has stopped
    PtraceEventStopped,
    // TODO: Other states.
}

impl StopState {
    /// This means a stop is either in progress or we've stopped.
    pub fn is_stopping_or_stopped(&self) -> bool {
        self.is_stopped() || self.is_stopping()
    }

    /// This means a stop is in progress.  Refers to any stop state ending in "ing".
    pub fn is_stopping(&self) -> bool {
        match *self {
            StopState::GroupStopping
            | StopState::SignalDeliveryStopping
            | StopState::PtraceEventStopping => true,
            _ => false,
        }
    }

    /// This means task is stopped.
    pub fn is_stopped(&self) -> bool {
        match *self {
            StopState::GroupStopped
            | StopState::SignalDeliveryStopped
            | StopState::PtraceEventStopped => true,
            _ => false,
        }
    }

    /// Returns the "ed" version of this StopState, if it is "ing".
    pub fn finalize(&self) -> Result<StopState, ()> {
        match *self {
            StopState::GroupStopping => Ok(StopState::GroupStopped),
            StopState::SignalDeliveryStopping => Ok(StopState::SignalDeliveryStopped),
            StopState::PtraceEventStopping => Ok(StopState::PtraceEventStopped),
            StopState::Waking => Ok(StopState::Awake),
            StopState::ForceWaking => Ok(StopState::ForceAwake),
            _ => Err(()),
        }
    }

    pub fn is_downgrade(&self, new_state: &StopState) -> bool {
        match *self {
            StopState::GroupStopped => *new_state == StopState::GroupStopping,
            StopState::SignalDeliveryStopped => *new_state == StopState::SignalDeliveryStopping,
            StopState::PtraceEventStopped => *new_state == StopState::PtraceEventStopped,
            StopState::Awake => *new_state == StopState::Waking,
            _ => false,
        }
    }

    pub fn is_waking_or_awake(&self) -> bool {
        *self == StopState::Waking
            || *self == StopState::Awake
            || *self == StopState::ForceWaking
            || *self == StopState::ForceAwake
    }

    /// Indicate if the transition to the stopped / awake state is not finished.  This
    /// function is typically used to determine when it is time to notify waiters.
    pub fn is_in_progress(&self) -> bool {
        *self == StopState::Waking
            || *self == StopState::ForceWaking
            || *self == StopState::GroupStopping
            || *self == StopState::SignalDeliveryStopping
            || *self == StopState::PtraceEventStopping
    }

    pub fn ptrace_only(&self) -> bool {
        !self.is_waking_or_awake()
            && *self != StopState::GroupStopped
            && *self != StopState::GroupStopping
    }

    pub fn is_illegal_transition(&self, new_state: StopState) -> bool {
        *self == StopState::ForceAwake
            || (*self == StopState::ForceWaking && new_state != StopState::ForceAwake)
            || new_state == *self
            || self.is_downgrade(&new_state)
    }

    pub fn is_force(&self) -> bool {
        *self == StopState::ForceAwake || *self == StopState::ForceWaking
    }
}

bitflags! {
    pub struct TaskFlags: u8 {
        const EXITED = 0x1;
        const SIGNALS_AVAILABLE = 0x2;
        const TEMPORARY_SIGNAL_MASK = 0x4;
        /// Whether the executor should dump the stack of this task when it exits.
        /// Currently used to implement ExitStatus::CoreDump.
        const DUMP_ON_EXIT = 0x8;
    }
}

pub struct AtomicTaskFlags {
    flags: AtomicU8,
}

impl AtomicTaskFlags {
    fn new(flags: TaskFlags) -> Self {
        Self { flags: AtomicU8::new(flags.bits()) }
    }

    fn load(&self, ordering: Ordering) -> TaskFlags {
        let flags = self.flags.load(ordering);
        // SAFETY: We only ever store values from a `TaskFlags`.
        unsafe { TaskFlags::from_bits_unchecked(flags) }
    }

    fn swap(&self, flags: TaskFlags, ordering: Ordering) -> TaskFlags {
        let flags = self.flags.swap(flags.bits(), ordering);
        // SAFETY: We only ever store values from a `TaskFlags`.
        unsafe { TaskFlags::from_bits_unchecked(flags) }
    }
}

pub struct TaskMutableState {
    // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
    pub clear_child_tid: UserRef<pid_t>,

    /// Signal handler related state. This is grouped together for when atomicity is needed during
    /// signal sending and delivery.
    pub signals: SignalState,

    /// The exit status that this task exited with.
    exit_status: Option<ExitStatus>,

    /// Desired scheduler policy for the task.
    pub scheduler_policy: SchedulerPolicy,

    /// The UTS namespace assigned to this thread.
    ///
    /// This field is kept in the mutable state because the UTS namespace of a thread
    /// can be forked using `clone()` or `unshare()` syscalls.
    ///
    /// We use UtsNamespaceHandle because the UTS properties can be modified
    /// by any other thread that shares this namespace.
    pub uts_ns: UtsNamespaceHandle,

    /// Bit that determines whether a newly started program can have privileges its parent does
    /// not have.  See Documentation/prctl/no_new_privs.txt in the Linux kernel for details.
    /// Note that Starnix does not currently implement the relevant privileges (e.g.,
    /// setuid/setgid binaries).  So, you can set this, but it does nothing other than get
    /// propagated to children.
    ///
    /// The documentation indicates that this can only ever be set to
    /// true, and it cannot be reverted to false.  Accessor methods
    /// for this field ensure this property.
    no_new_privs: bool,

    /// Userspace hint about how to adjust the OOM score for this process.
    pub oom_score_adj: i32,

    /// List of currently installed seccomp_filters
    pub seccomp_filters: SeccompFilterContainer,

    /// A pointer to the head of the robust futex list of this thread in
    /// userspace. See get_robust_list(2)
    pub robust_list_head: UserRef<robust_list_head>,

    /// The timer slack used to group timer expirations for the calling thread.
    ///
    /// Timers may expire up to `timerslack_ns` late, but never early.
    ///
    /// If this value is 0, the task's default timerslack is used.
    pub timerslack_ns: u64,

    /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
    /// task.
    ///
    /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
    /// across tasks.
    pub default_timerslack_ns: u64,

    /// Information that a tracer needs to communicate with this process, if it
    /// is being traced.
    pub ptrace: Option<PtraceState>,
}

impl TaskMutableState {
    pub fn no_new_privs(&self) -> bool {
        self.no_new_privs
    }

    /// Sets the value of no_new_privs to true.  It is an error to set
    /// it to anything else.
    pub fn enable_no_new_privs(&mut self) {
        self.no_new_privs = true;
    }

    pub fn get_timerslack_ns(&self) -> u64 {
        self.timerslack_ns
    }

    /// Sets the current timerslack of the task to `ns`.
    ///
    /// If `ns` is zero, the current timerslack gets reset to the task's default timerslack.
    pub fn set_timerslack_ns(&mut self, ns: u64) {
        if ns == 0 {
            self.timerslack_ns = self.default_timerslack_ns;
        } else {
            self.timerslack_ns = ns;
        }
    }

    pub fn set_ptrace(&mut self, tracer: Option<PtraceState>) -> Result<(), Errno> {
        if tracer.is_some() && self.ptrace.is_some() {
            return Err(errno!(EPERM));
        }
        self.ptrace = tracer;
        Ok(())
    }

    pub fn is_ptraced(&self) -> bool {
        self.ptrace.is_some()
    }

    pub fn is_ptrace_listening(&self) -> bool {
        self.ptrace.as_ref().map_or(false, |ptrace| ptrace.stop_status == PtraceStatus::Listening)
    }

    pub fn ptrace_on_signal_consume(&mut self) -> bool {
        self.ptrace.as_mut().map_or(false, |ptrace: &mut PtraceState| {
            if ptrace.stop_status == PtraceStatus::Continuing {
                ptrace.stop_status = PtraceStatus::Default;
                false
            } else {
                true
            }
        })
    }

    pub fn notify_ptracers(&mut self) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracer_waiters.notify_all();
        }
    }

    pub fn wait_on_ptracer(&self, waiter: &Waiter) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracee_waiters.wait_async(&waiter);
        }
    }

    pub fn notify_ptracees(&mut self) {
        if let Some(ptrace) = &self.ptrace {
            ptrace.tracee_waiters.notify_all();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStateCode {
    // Task is being executed.
    Running,

    // Task is waiting for an event.
    Sleeping,

    // Task has exited.
    Zombie,
}

impl TaskStateCode {
    pub fn code_char(&self) -> char {
        match self {
            TaskStateCode::Running => 'R',
            TaskStateCode::Sleeping => 'S',
            TaskStateCode::Zombie => 'Z',
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            TaskStateCode::Running => "running",
            TaskStateCode::Sleeping => "sleeping",
            TaskStateCode::Zombie => "zombie",
        }
    }
}

/// The information of the task that needs to be available to the `ThreadGroup` while computing
/// which process a wait can target. It is necessary to shared this data with the `ThreadGroup` so
/// that it is available while the task is being dropped and so is not accessible from a weak
/// pointer.
#[derive(Clone, Debug)]
pub struct TaskPersistentInfoState {
    /// Immutable information about the task
    tid: pid_t,
    pid: pid_t,

    /// The command of this task.
    command: CString,

    /// The security credentials for this task.
    creds: Credentials,

    /// The signal this task generates on exit.
    exit_signal: Option<Signal>,
}

impl TaskPersistentInfoState {
    fn new(
        tid: pid_t,
        pid: pid_t,
        command: CString,
        creds: Credentials,
        exit_signal: Option<Signal>,
    ) -> TaskPersistentInfo {
        Arc::new(Mutex::new(Self { tid, pid, command, creds, exit_signal }))
    }

    pub fn tid(&self) -> pid_t {
        self.tid
    }

    pub fn pid(&self) -> pid_t {
        self.pid
    }

    pub fn command(&self) -> &CString {
        &self.command
    }

    pub fn creds(&self) -> &Credentials {
        &self.creds
    }

    pub fn creds_mut(&mut self) -> &mut Credentials {
        &mut self.creds
    }

    pub fn exit_signal(&self) -> &Option<Signal> {
        &self.exit_signal
    }
}

pub type TaskPersistentInfo = Arc<Mutex<TaskPersistentInfoState>>;

/// A unit of execution.
///
/// A task is the primary unit of execution in the Starnix kernel. Most tasks are *user* tasks,
/// which have an associated Zircon thread. The Zircon thread switches between restricted mode,
/// in which the thread runs userspace code, and normal mode, in which the thread runs Starnix
/// code.
///
/// Tasks track the resources used by userspace by referencing various objects, such as an
/// `FdTable`, a `MemoryManager`, and an `FsContext`. Many tasks can share references to these
/// objects. In principle, which objects are shared between which tasks can be largely arbitrary,
/// but there are common patterns of sharing. For example, tasks created with `pthread_create`
/// will share the `FdTable`, `MemoryManager`, and `FsContext` and are often called "threads" by
/// userspace programmers. Tasks created by `posix_spawn` do not share these objects and are often
/// called "processes" by userspace programmers. However, inside the kernel, there is no clear
/// definition of a "thread" or a "process".
///
/// During boot, the kernel creates the first task, often called `init`. The vast majority of other
/// tasks are created as transitive clones (e.g., using `clone(2)`) of that task. Sometimes, the
/// kernel will create new tasks from whole cloth, either with a corresponding userspace component
/// or to represent some background work inside the kernel.
///
/// See also `CurrentTask`, which represents the task corresponding to the thread that is currently
/// executing.
pub struct Task {
    /// A unique identifier for this task.
    ///
    /// This value can be read in userspace using `gettid(2)`. In general, this value
    /// is different from the value return by `getpid(2)`, which returns the `id` of the leader
    /// of the `thread_group`.
    pub id: pid_t,

    /// The thread group to which this task belongs.
    ///
    /// The group of tasks in a thread group roughly corresponds to the userspace notion of a
    /// process.
    pub thread_group: Arc<ThreadGroup>,

    /// A handle to the underlying Zircon thread object.
    ///
    /// Some tasks lack an underlying Zircon thread. These tasks are used internally by the
    /// Starnix kernel to track background work, typically on a `kthread`.
    pub thread: RwLock<Option<zx::Thread>>,

    /// The file descriptor table for this task.
    ///
    /// This table can be share by many tasks.
    pub files: FdTable,

    /// The memory manager for this task.
    mm: Option<Arc<MemoryManager>>,

    /// The file system for this task.
    fs: Option<Arc<FsContext>>,

    /// The namespace for abstract AF_UNIX sockets for this task.
    pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The namespace for AF_VSOCK for this task.
    pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The stop state of the task, distinct from the stop state of the thread group.
    ///
    /// Must only be set when the `mutable_state` write lock is held.
    stop_state: AtomicStopState,

    /// The flags for the task.
    ///
    /// Must only be set the then `mutable_state` write lock is held.
    flags: AtomicTaskFlags,

    /// The mutable state of the Task.
    mutable_state: RwLock<TaskMutableState>,

    /// The information of the task that needs to be available to the `ThreadGroup` while computing
    /// which process a wait can target.
    /// Contains the command line, the task credentials and the exit signal.
    /// See `TaskPersistentInfo` for more information.
    pub persistent_info: TaskPersistentInfo,

    /// For vfork and clone() with CLONE_VFORK, this is set when the task exits or calls execve().
    /// It allows the calling task to block until the fork has been completed. Only populated
    /// when created with the CLONE_VFORK flag.
    vfork_event: Option<Arc<zx::Event>>,

    /// Variable that can tell you whether there are currently seccomp
    /// filters without holding a lock
    pub seccomp_filter_state: SeccompState,

    /// Used to ensure that all logs related to this task carry the same metadata about the task.
    logging_span: OnceCell<starnix_logging::Span>,
}

/// The decoded cross-platform parts we care about for page fault exception reports.
pub struct PageFaultExceptionReport {
    pub faulting_address: u64,
    pub not_present: bool, // Set when the page fault was due to a not-present page.
    pub is_write: bool,    // Set when the triggering memory operation was a write.
}

impl Task {
    pub fn kernel(&self) -> &Arc<Kernel> {
        &self.thread_group.kernel
    }

    pub fn has_same_address_space(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.mm(), other.mm())
    }

    pub fn flags(&self) -> TaskFlags {
        self.flags.load(Ordering::Relaxed)
    }

    fn check_mutable_state_lock_held(&self, guard: &mut TaskMutableState) {
        // We don't actually use the guard but we require it to enforce that the
        // caller holds the task's mutable state lock (identified by mutable
        // access to the task's mutable state).
        let _ = guard;
        // Ideally we would assert `!self.mutable_state.is_locked_exclusive()`
        // but this tries to take the lock underneath which triggers a lock
        // dependency check, resulting in a panic.
    }

    pub fn update_flags(&self, guard: &mut TaskMutableState, clear: TaskFlags, set: TaskFlags) {
        self.check_mutable_state_lock_held(guard);

        debug_assert_eq!(clear ^ set, clear | set);
        let observed = self.flags();
        let swapped = self.flags.swap((observed | set) & !clear, Ordering::Relaxed);
        debug_assert_eq!(swapped, observed);
    }

    pub fn set_flags(&self, guard: &mut TaskMutableState, flag: TaskFlags, v: bool) {
        let (clear, set) = if v { (TaskFlags::empty(), flag) } else { (flag, TaskFlags::empty()) };

        self.update_flags(guard, clear, set);
    }

    pub fn set_exit_status(&self, guard: &mut TaskMutableState, status: ExitStatus) {
        self.set_flags(guard, TaskFlags::EXITED, true);
        guard.exit_status = Some(status);
    }

    pub fn set_exit_status_if_not_already(&self, guard: &mut TaskMutableState, status: ExitStatus) {
        self.set_flags(guard, TaskFlags::EXITED, true);
        guard.exit_status.get_or_insert(status);
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.is_exitted().then(|| self.read().exit_status.clone()).flatten()
    }

    pub fn is_exitted(&self) -> bool {
        self.flags().contains(TaskFlags::EXITED)
    }

    pub fn load_stopped(&self) -> StopState {
        self.stop_state.load(Ordering::Relaxed)
    }

    fn store_stopped(&self, state: StopState, guard: &mut TaskMutableState) {
        self.check_mutable_state_lock_held(guard);

        self.stop_state.store(state, Ordering::Relaxed)
    }

    pub fn set_stopped(
        &self,
        guard: &mut TaskMutableState,
        stopped: StopState,
        siginfo: Option<SignalInfo>,
    ) {
        if stopped.ptrace_only() && guard.ptrace.is_none() {
            return;
        }

        if self.load_stopped().is_illegal_transition(stopped) {
            return;
        }

        // TODO(https://g-issues.fuchsia.dev/issues/306438676): When task can be
        // stopped inside user code, task will need to be either restarted or
        // stopped here.
        self.store_stopped(stopped, guard);
        if let Some(ref mut ptrace) = &mut guard.ptrace {
            ptrace.set_last_signal(siginfo);
        }
        if stopped == StopState::Waking || stopped == StopState::ForceWaking {
            guard.notify_ptracees();
        }
        if !stopped.is_in_progress() {
            guard.notify_ptracers();
        }
    }

    pub fn can_accept_ptrace_commands(&self, guard: &TaskMutableState) -> bool {
        !self.load_stopped().is_waking_or_awake()
            && guard.is_ptraced()
            && !guard.is_ptrace_listening()
    }

    /// Upgrade a Reference to a Task, returning a ESRCH errno if the reference cannot be borrowed.
    pub fn from_weak(weak: &WeakRef<Task>) -> Result<TempRef<'_, Task>, Errno> {
        weak.upgrade().ok_or_else(|| errno!(ESRCH))
    }

    /// Internal function for creating a Task object. Useful when you need to specify the value of
    /// every field. create_process and create_thread are more likely to be what you want.
    ///
    /// Any fields that should be initialized fresh for every task, even if the task was created
    /// with fork, are initialized to their defaults inside this function. All other fields are
    /// passed as parameters.
    #[allow(clippy::let_and_return)]
    pub fn new(
        id: pid_t,
        command: CString,
        thread_group: Arc<ThreadGroup>,
        thread: Option<zx::Thread>,
        files: FdTable,
        mm: Arc<MemoryManager>,
        // The only case where fs should be None if when building the initial task that is the
        // used to build the initial FsContext.
        fs: Arc<FsContext>,
        creds: Credentials,
        abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,
        abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,
        exit_signal: Option<Signal>,
        signal_mask: SigSet,
        vfork_event: Option<Arc<zx::Event>>,
        scheduler_policy: SchedulerPolicy,
        uts_ns: UtsNamespaceHandle,
        no_new_privs: bool,
        seccomp_filter_state: SeccompState,
        seccomp_filters: SeccompFilterContainer,
        robust_list_head: UserRef<robust_list_head>,
        timerslack_ns: u64,
    ) -> Self {
        let pid = thread_group.leader;
        let task = Task {
            id,
            thread_group,
            thread: RwLock::new(thread),
            files,
            mm: Some(mm),
            fs: Some(fs),
            abstract_socket_namespace,
            abstract_vsock_namespace,
            vfork_event,
            stop_state: AtomicStopState::new(StopState::Awake),
            flags: AtomicTaskFlags::new(TaskFlags::empty()),
            mutable_state: RwLock::new(TaskMutableState {
                clear_child_tid: UserRef::default(),
                signals: SignalState::with_mask(signal_mask),
                exit_status: None,
                scheduler_policy,
                uts_ns,
                no_new_privs,
                oom_score_adj: Default::default(),
                seccomp_filters,
                robust_list_head,
                timerslack_ns,
                // The default timerslack is set to the current timerslack of the creating thread.
                default_timerslack_ns: timerslack_ns,
                ptrace: None,
            }),
            persistent_info: TaskPersistentInfoState::new(id, pid, command, creds, exit_signal),
            seccomp_filter_state,
            logging_span: OnceCell::new(),
        };
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = task.read();
            let _l2 = task.persistent_info.lock();
        }
        task
    }

    /// Access mutable state with a read lock.
    pub fn read(&self) -> RwLockReadGuard<'_, TaskMutableState> {
        self.mutable_state.read()
    }

    /// Access mutable state with a write lock.
    pub fn write(&self) -> RwLockWriteGuard<'_, TaskMutableState> {
        self.mutable_state.write()
    }

    pub fn add_file(&self, file: FileHandle, flags: FdFlags) -> Result<FdNumber, Errno> {
        self.files.add_with_flags(self, file, flags)
    }

    pub fn creds(&self) -> Credentials {
        self.persistent_info.lock().creds.clone()
    }

    pub fn exit_signal(&self) -> Option<Signal> {
        self.persistent_info.lock().exit_signal
    }

    pub fn set_creds(&self, creds: Credentials) {
        self.persistent_info.lock().creds = creds;
    }

    pub fn fs(&self) -> &Arc<FsContext> {
        self.fs.as_ref().expect("fs must be set")
    }

    pub fn mm(&self) -> &Arc<MemoryManager> {
        self.mm.as_ref().expect("mm must be set")
    }

    /// Overwrite the existing scheduler policy with a new one and update the task's thread's role.
    pub fn set_scheduler_policy(&self, policy: SchedulerPolicy) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|sched_policy| *sched_policy = policy)
    }

    /// Update the nice value of the scheduler policy and update the task's thread's role.
    pub fn update_scheduler_nice(&self, raw_priority: u8) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|sched_policy| sched_policy.set_raw_nice(raw_priority))
    }

    /// Update the task's thread's role based on its current scheduler policy without making any
    /// changes to the policy.
    ///
    /// This should be called on tasks that have newly created threads, e.g. after cloning.
    pub fn sync_scheduler_policy_to_role(&self) -> Result<(), Errno> {
        self.update_sched_policy_then_role(|_| {})
    }

    fn update_sched_policy_then_role(
        &self,
        updater: impl FnOnce(&mut SchedulerPolicy),
    ) -> Result<(), Errno> {
        profile_duration!("UpdateTaskThreadRole");
        let new_scheduler_policy = {
            // Hold the task state lock as briefly as possible, it's not needed to update the role.
            let mut state = self.write();
            updater(&mut state.scheduler_policy);
            state.scheduler_policy
        };

        let Some(profile_provider) = &self.thread_group.kernel.profile_provider else {
            log_debug!("thread role update requested in kernel without ProfileProvider, skipping");
            return Ok(());
        };
        let thread = self.thread.read();
        let Some(thread) = thread.as_ref() else {
            log_debug!("thread role update requested for task without thread, skipping");
            return Ok(());
        };
        set_thread_role(profile_provider, thread, new_scheduler_policy)?;
        Ok(())
    }

    // See "Ptrace access mode checking" in https://man7.org/linux/man-pages/man2/ptrace.2.html
    pub fn check_ptrace_access_mode<L>(
        &self,
        locked: &mut Locked<'_, L>,
        mode: PtraceAccessMode,
        target: &Task,
    ) -> Result<(), Errno>
    where
        L: LockBefore<MmDumpable>,
    {
        // (1)  If the calling thread and the target thread are in the same
        //      thread group, access is always allowed.
        if self.thread_group.leader == target.thread_group.leader {
            return Ok(());
        }

        // (2)  If the access mode specifies PTRACE_MODE_FSCREDS, then, for
        //      the check in the next step, employ the caller's filesystem
        //      UID and GID.  (As noted in credentials(7), the filesystem
        //      UID and GID almost always have the same values as the
        //      corresponding effective IDs.)
        //
        //      Otherwise, the access mode specifies PTRACE_MODE_REALCREDS,
        //      so use the caller's real UID and GID for the checks in the
        //      next step.  (Most APIs that check the caller's UID and GID
        //      use the effective IDs.  For historical reasons, the
        //      PTRACE_MODE_REALCREDS check uses the real IDs instead.)
        let creds = self.creds();
        let (uid, gid) = if mode.contains(PTRACE_MODE_FSCREDS) {
            let fscred = creds.as_fscred();
            (fscred.uid, fscred.gid)
        } else if mode.contains(PTRACE_MODE_REALCREDS) {
            (creds.uid, creds.gid)
        } else {
            unreachable!();
        };

        // (3)  Deny access if neither of the following is true:
        //
        //      -  The real, effective, and saved-set user IDs of the target
        //         match the caller's user ID, and the real, effective, and
        //         saved-set group IDs of the target match the caller's
        //         group ID.
        //
        //      -  The caller has the CAP_SYS_PTRACE capability in the user
        //         namespace of the target.
        let target_creds = target.creds();
        if !creds.has_capability(CAP_SYS_PTRACE)
            && !(target_creds.uid == uid
                && target_creds.euid == uid
                && target_creds.saved_uid == uid
                && target_creds.gid == gid
                && target_creds.egid == gid
                && target_creds.saved_gid == gid)
        {
            return error!(EPERM);
        }

        // (4)  Deny access if the target process "dumpable" attribute has a
        //      value other than 1 (SUID_DUMP_USER; see the discussion of
        //      PR_SET_DUMPABLE in prctl(2)), and the caller does not have
        //      the CAP_SYS_PTRACE capability in the user namespace of the
        //      target process.
        let dumpable = *target.mm().dumpable.lock(locked);
        if dumpable != DumpPolicy::User && !creds.has_capability(CAP_SYS_PTRACE) {
            return error!(EPERM);
        }

        // TODO: Implement the LSM security_ptrace_access_check() interface.
        //
        // (5)  The kernel LSM security_ptrace_access_check() interface is
        //      invoked to see if ptrace access is permitted.

        // (6)  If access has not been denied by any of the preceding steps,
        //      then access is allowed.
        Ok(())
    }

    /// Signals the vfork event, if any, to unblock waiters.
    pub fn signal_vfork(&self) {
        if let Some(event) = &self.vfork_event {
            if let Err(status) = event.signal_handle(Signals::NONE, Signals::USER_0) {
                log_warn!("Failed to set vfork signal {status}");
            }
        };
    }

    /// Blocks the caller until the task has exited or executed execve(). This is used to implement
    /// vfork() and clone(... CLONE_VFORK, ...). The task musy have created with CLONE_EXECVE.
    pub fn wait_for_execve(&self, task_to_wait: WeakRef<Task>) -> Result<(), Errno> {
        let event = task_to_wait.upgrade().and_then(|t| t.vfork_event.clone());
        if let Some(event) = event {
            event
                .wait_handle(zx::Signals::USER_0, zx::Time::INFINITE)
                .map_err(|status| from_status_like_fdio!(status))?;
        }
        Ok(())
    }

    /// If needed, clear the child tid for this task.
    ///
    /// Userspace can ask us to clear the child tid and issue a futex wake at
    /// the child tid address when we tear down a task. For example, bionic
    /// uses this mechanism to implement pthread_join. The thread that calls
    /// pthread_join sleeps using FUTEX_WAIT on the child tid address. We wake
    /// them up here to let them know the thread is done.
    pub fn clear_child_tid_if_needed(&self) -> Result<(), Errno> {
        let mut state = self.write();
        let user_tid = state.clear_child_tid;
        if !user_tid.is_null() {
            let zero: pid_t = 0;
            self.mm().write_object(user_tid, &zero)?;
            self.kernel().shared_futexes.wake(
                self,
                user_tid.addr(),
                usize::MAX,
                FUTEX_BITSET_MATCH_ANY,
            )?;
            state.clear_child_tid = UserRef::default();
        }
        Ok(())
    }

    pub fn get_task(&self, pid: pid_t) -> WeakRef<Task> {
        self.kernel().pids.read().get_task(pid)
    }

    pub fn get_pid(&self) -> pid_t {
        self.thread_group.leader
    }

    pub fn get_tid(&self) -> pid_t {
        self.id
    }

    pub fn is_leader(&self) -> bool {
        self.get_pid() == self.get_tid()
    }

    pub fn read_argv(&self) -> Result<Vec<FsString>, Errno> {
        let (argv_start, argv_end) = {
            let mm_state = self.mm().state.read();
            (mm_state.argv_start, mm_state.argv_end)
        };

        self.mm().read_nul_delimited_c_string_list(argv_start, argv_end - argv_start)
    }

    pub fn thread_runtime_info(&self) -> Result<zx::TaskRuntimeInfo, Errno> {
        self.thread
            .read()
            .as_ref()
            .ok_or_else(|| errno!(EINVAL))?
            .get_runtime_info()
            .map_err(|status| from_status_like_fdio!(status))
    }

    pub fn as_ucred(&self) -> ucred {
        let creds = self.creds();
        ucred { pid: self.get_pid(), uid: creds.uid, gid: creds.gid }
    }

    pub fn as_fscred(&self) -> FsCred {
        self.creds().as_fscred()
    }

    pub fn can_signal(&self, target: &Task, unchecked_signal: UncheckedSignal) -> bool {
        // If both the tasks share a thread group the signal can be sent. This is not documented
        // in kill(2) because kill does not support task-level granularity in signal sending.
        if self.thread_group == target.thread_group {
            return true;
        }

        let self_creds = self.creds();

        if self_creds.has_capability(CAP_KILL) {
            return true;
        }

        if self_creds.has_same_uid(&target.creds()) {
            return true;
        }

        // TODO(lindkvist): This check should also verify that the sessions are the same.
        if Signal::try_from(unchecked_signal) == Ok(SIGCONT) {
            return true;
        }

        false
    }

    /// Interrupts the current task.
    ///
    /// This will interrupt any blocking syscalls if the task is blocked on one.
    /// The signal_state of the task must not be locked.
    pub fn interrupt(&self) {
        self.read().signals.run_state.wake();
        if let Some(thread) = self.thread.read().as_ref() {
            crate::execution::interrupt_thread(thread);
        }
    }

    pub fn command(&self) -> CString {
        self.persistent_info.lock().command.clone()
    }

    pub fn set_command_name(&self, name: CString) {
        // Set the name on the Linux thread.
        if let Some(thread) = self.thread.read().as_ref() {
            set_zx_name(thread, name.as_bytes());
        }
        // If this is the thread group leader, use this name for the process too.
        if self.is_leader() {
            set_zx_name(&self.thread_group.process, name.as_bytes());
        }

        let debug_info = starnix_logging::TaskDebugInfo {
            pid: self.thread_group.leader,
            tid: self.id,
            command: name,
        };
        self.update_logging_span(&debug_info);

        // Truncate to 16 bytes, including null byte.
        let bytes = debug_info.command.to_bytes();

        self.persistent_info.lock().command = if bytes.len() > 15 {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&bytes[..15]).unwrap()
        } else {
            debug_info.command
        };
    }

    pub fn set_seccomp_state(&self, state: SeccompStateValue) -> Result<(), Errno> {
        self.seccomp_filter_state.set(&state)
    }

    pub fn state_code(&self) -> TaskStateCode {
        let status = self.read();
        if status.exit_status.is_some() {
            TaskStateCode::Zombie
        } else if status.signals.run_state.is_blocked() {
            TaskStateCode::Sleeping
        } else {
            TaskStateCode::Running
        }
    }

    pub fn time_stats(&self) -> TaskTimeStats {
        let info = match &*self.thread.read() {
            Some(thread) => zx::Task::get_runtime_info(thread).expect("Failed to get thread stats"),
            None => return TaskTimeStats::default(),
        };

        TaskTimeStats {
            user_time: zx::Duration::from_nanos(info.cpu_time),
            // TODO(fxbug.dev/127682): How can we calculate system time?
            system_time: zx::Duration::default(),
        }
    }

    /// Sets the stop state (per set_stopped), and also notifies all listeners,
    /// including the parent process if appropriate.
    pub fn set_stopped_and_notify(&self, stopped: StopState, siginfo: Option<SignalInfo>) {
        self.set_stopped(&mut *self.write(), stopped, siginfo);

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
            self.set_stopped(&mut *self.write(), StopState::GroupStopped, signal);
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
                drop(group_state);
                drop(task_state);
                self.thread_group.set_stopped(new_state, None, false);
                self.set_stopped(&mut *self.write(), new_state, None);
                return true;
            }
        }

        // We will wait.
        if self.thread_group.load_stopped().is_stopped() || task_stop_state.is_stopped() {
            // If we've stopped or PTRACE_LISTEN has been sent, wait for a
            // signal or instructions from the tracer.
            group_state.stopped_waiters.wait_async(&waiter);
            task_state.wait_on_ptracer(&waiter);
        } else if self.can_accept_ptrace_commands(&task_state) {
            // If we're stopped because a tracer has seen the stop and not taken
            // further action, wait for further instructions from the tracer.
            task_state.wait_on_ptracer(&waiter);
        } else if task_state.is_ptrace_listening() {
            // A PTRACE_LISTEN is a state where we can get signals and notify a
            // ptracer, but otherwise remain blocked.
            if let Some(ref mut ptrace) = &mut task_state.ptrace {
                ptrace.set_last_signal(Some(SignalInfo::default(SIGTRAP)));
            }
            task_state.wait_on_ptracer(&waiter);
            task_state.notify_ptracers();
        }
        false
    }

    pub fn logging_span(&self) -> starnix_logging::Span {
        let logging_span = self.logging_span.get_or_init(|| {
            starnix_logging::Span::new(&starnix_logging::TaskDebugInfo {
                pid: self.thread_group.leader,
                tid: self.id,
                command: self.command(),
            })
        });
        logging_span.clone()
    }

    fn update_logging_span(&self, debug_info: &starnix_logging::TaskDebugInfo) {
        let logging_span =
            self.logging_span.get_or_init(|| starnix_logging::Span::new(&debug_info));
        logging_span.update(debug_info);
    }
}

impl Releasable for Task {
    type Context<'a> = ThreadState;

    fn release(mut self, thread_state: ThreadState) {
        // Disconnect from tracer, if one is present.
        let ptracer_pid = self.mutable_state.get_mut().ptrace.as_ref().map(|ptrace| ptrace.pid);
        if let Some(ptracer_pid) = ptracer_pid {
            if let Some(ProcessEntryRef::Process(tg)) =
                self.kernel().pids.read().get_process(ptracer_pid)
            {
                let pid = self.get_pid();
                tg.ptracees.lock().remove(&pid);
            }
            let _ = self.mutable_state.get_mut().set_ptrace(None);
        }

        self.thread_group.remove(&self);

        // Release the fd table.
        self.files.release(());

        self.signal_vfork();

        // Drop fields that can end up owning a FsNode to ensure no FsNode are owned by this task.
        self.fs = None;
        self.mm = None;
        // Rebuild a temporary CurrentTask to run the release actions that requires a CurrentState.
        let current_task = CurrentTask::new(OwnedRef::new(self), thread_state);

        // Apply any delayed releasers left.
        current_task.trigger_delayed_releaser();

        // Drop the task now that is has been released. This requires to take it fro the OwnedRef
        // and from the resulting ReleaseGuard.
        let CurrentTask { mut task, .. } = current_task;
        let task = OwnedRef::take(&mut task).expect("task should not have been re-owned");
        let task: Self = ReleaseGuard::take(task);

        // It is safe to drop the task, as it has been release by this method.
        std::mem::drop(task);
    }
}

impl MemoryAccessor for Task {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().read_memory(addr, bytes)
    }

    fn vmo_read_memory<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().vmo_read_memory(addr, bytes)
    }

    fn read_memory_partial_until_null_byte<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().read_memory_partial_until_null_byte(addr, bytes)
    }

    fn read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().read_memory_partial(addr, bytes)
    }

    fn vmo_read_memory_partial<'a>(
        &self,
        addr: UserAddress,
        bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        self.mm().vmo_read_memory_partial(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().write_memory(addr, bytes)
    }

    fn vmo_write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().vmo_write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().write_memory_partial(addr, bytes)
    }

    fn vmo_write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm().vmo_write_memory_partial(addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.mm().zero(addr, length)
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}[{}]",
            self.thread_group.leader,
            self.id,
            self.persistent_info.lock().command.to_string_lossy()
        )
    }
}

impl cmp::PartialEq for Task {
    fn eq(&self, other: &Self) -> bool {
        let ptr: *const Task = self;
        let other_ptr: *const Task = other;
        ptr == other_ptr
    }
}

impl cmp::Eq for Task {}

impl From<&Task> for FsCred {
    fn from(t: &Task) -> FsCred {
        t.creds().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use starnix_uapi::{
        auth::CAP_SYS_ADMIN, resource_limits::Resource, rlimit, signals::SIGCHLD, CLONE_SIGHAND,
        CLONE_THREAD, CLONE_VM,
    };

    #[::fuchsia::test]
    async fn test_tid_allocation() {
        let (kernel, current_task) = create_kernel_and_task();

        assert_eq!(current_task.get_tid(), 1);
        let another_current = create_task(&kernel, "another-task");
        let another_tid = another_current.get_tid();
        assert!(another_tid >= 2);

        let pids = kernel.pids.read();
        assert_eq!(pids.get_task(1).upgrade().unwrap().get_tid(), 1);
        assert_eq!(pids.get_task(another_tid).upgrade().unwrap().get_tid(), another_tid);
        assert!(pids.get_task(another_tid + 1).upgrade().is_none());
    }

    #[::fuchsia::test]
    async fn test_clone_pid_and_parent_pid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let thread = current_task.clone_task_for_test(
            &mut locked,
            (CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) as u64,
            Some(SIGCHLD),
        );
        assert_eq!(current_task.get_pid(), thread.get_pid());
        assert_ne!(current_task.get_tid(), thread.get_tid());
        assert_eq!(current_task.thread_group.leader, thread.thread_group.leader);

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_ne!(current_task.get_pid(), child_task.get_pid());
        assert_ne!(current_task.get_tid(), child_task.get_tid());
        assert_eq!(current_task.get_pid(), child_task.thread_group.read().get_ppid());
    }

    #[::fuchsia::test]
    async fn test_root_capabilities() {
        let (_kernel, current_task) = create_kernel_and_task();
        assert!(current_task.creds().has_capability(CAP_SYS_ADMIN));
        current_task.set_creds(Credentials::with_ids(1, 1));
        assert!(!current_task.creds().has_capability(CAP_SYS_ADMIN));
    }

    #[::fuchsia::test]
    async fn test_clone_rlimit() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let prev_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_ne!(prev_fsize, 10);
        current_task
            .thread_group
            .limits
            .lock()
            .set(Resource::FSIZE, rlimit { rlim_cur: 10, rlim_max: 100 });
        let current_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(current_fsize, 10);

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child_fsize = child_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(child_fsize, 10)
    }
}
