// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;
use extended_pstate::ExtendedPstateState;
use fuchsia_zircon::{
    self as zx, sys::zx_thread_state_general_regs_t, AsHandleRef, Signals, Task as _,
};
use starnix_lock::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use starnix_sync::{EventWaitGuard, WakeReason};
use std::{
    cmp,
    convert::TryFrom,
    ffi::CString,
    fmt,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use crate::{
    arch::{
        registers::RegisterState,
        task::{decode_page_fault_exception_report, get_signal_for_general_exception},
    },
    auth::{Credentials, FsCred},
    execution::{create_zircon_process, TaskInfo},
    fs::{
        FdFlags, FdNumber, FdTable, FileHandle, FsContext, FsStr, FsString, LookupContext,
        NamespaceNode, SymlinkMode, SymlinkTarget,
    },
    loader::{load_executable, resolve_executable, ResolvedElf},
    logging::{log_error, log_warn, not_implemented, set_zx_name},
    mm::{DumpPolicy, MemoryAccessor, MemoryAccessorExt, MemoryManager},
    signals::{send_signal, RunState, SignalActions, SignalDetail, SignalInfo, SignalState},
    syscalls::{decls::Syscall, SyscallResult},
    task::{
        AbstractUnixSocketNamespace, AbstractVsockSocketNamespace, Kernel, ProcessEntryRef,
        ProcessGroup, PtraceState, PtraceStatus, SchedulerPolicy, SeccompFilter,
        SeccompFilterContainer, SeccompNotifierHandle, SeccompState, SeccompStateValue,
        ThreadGroup, UtsNamespaceHandle, Waiter,
    },
    types::signals::{SigSet, Signal, UncheckedSignal, SIGBUS, SIGCONT, SIGILL, SIGSEGV, SIGTRAP},
    types::{
        errno, error, from_status_like_fdio, pid_t, release_on_error, robust_list_head,
        sock_filter, sock_fprog, ucred, Access, DeviceType, Errno, FileMode, OpenFlags,
        OwnedRefByRef, PtraceAccessMode, ReleasableByRef, TaskTimeStats, TempRef, UserAddress,
        UserRef, WeakRef, BPF_MAXINSNS, CAP_KILL, CAP_SYS_ADMIN, CAP_SYS_PTRACE, CLD_CONTINUED,
        CLD_DUMPED, CLD_EXITED, CLD_KILLED, CLD_STOPPED, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID,
        CLONE_FILES, CLONE_FS, CLONE_INTO_CGROUP, CLONE_NEWUTS, CLONE_PARENT_SETTID, CLONE_SETTLS,
        CLONE_SIGHAND, CLONE_SYSVSEM, CLONE_THREAD, CLONE_VFORK, CLONE_VM, FUTEX_BITSET_MATCH_ANY,
        FUTEX_OWNER_DIED, FUTEX_TID_MASK, PTRACE_EVENT_STOP, PTRACE_MODE_FSCREDS,
        PTRACE_MODE_REALCREDS, ROBUST_LIST_LIMIT, SECCOMP_FILTER_FLAG_LOG,
        SECCOMP_FILTER_FLAG_NEW_LISTENER, SECCOMP_FILTER_FLAG_TSYNC,
        SECCOMP_FILTER_FLAG_TSYNC_ESRCH, SI_KERNEL,
    },
};

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
    pub task: OwnedRefByRef<Task>,

    /// A copy of the registers associated with the Zircon thread. Up-to-date values can be read
    /// from `self.handle.read_state_general_regs()`. To write these values back to the thread, call
    /// `self.handle.write_state_general_regs(self.registers.into())`.
    pub registers: RegisterState,

    /// Copy of the current extended processor state including floating point and vector registers.
    pub extended_pstate: ExtendedPstateState,

    /// A custom function to resume a syscall that has been interrupted by SIGSTOP.
    /// To use, call set_syscall_restart_func and return ERESTART_RESTARTBLOCK. sys_restart_syscall
    /// will eventually call it.
    pub syscall_restart_func: Option<Box<SyscallRestartFunc>>,
}

type SyscallRestartFunc =
    dyn FnOnce(&mut CurrentTask) -> Result<SyscallResult, Errno> + Send + Sync;

impl ReleasableByRef for CurrentTask {
    type Context<'a> = ();

    fn release(&self, _: ()) {
        self.notify_robust_list();
        let _ignored = self.clear_child_tid_if_needed();
        self.task.release(self);
    }
}

impl std::ops::Deref for CurrentTask {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}

impl MemoryAccessor for CurrentTask {
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        self.mm.read_memory_to_slice(addr, bytes)
    }

    fn vmo_read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        self.mm.vmo_read_memory_to_slice(addr, bytes)
    }

    fn read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        self.mm.read_memory_partial_to_slice(addr, bytes)
    }

    fn vmo_read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        self.mm.vmo_read_memory_partial_to_slice(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.write_memory(addr, bytes)
    }

    fn vmo_write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.vmo_write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.write_memory_partial(addr, bytes)
    }

    fn vmo_write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.vmo_write_memory_partial(addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.mm.zero(addr, length)
    }
}

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
    // TODO: Other states.
}

impl StopState {
    /// This means a stop is either in progress or we've stopped.
    pub fn is_stopping_or_stopped(&self) -> bool {
        self.is_stopped() || self.is_stopping()
    }

    /// This means a stop is in progress.  Refers to any stop state ending in "ing".
    pub fn is_stopping(&self) -> bool {
        *self == StopState::GroupStopping || *self == StopState::SignalDeliveryStopping
    }

    /// This means task is stopped.
    pub fn is_stopped(&self) -> bool {
        *self == StopState::GroupStopped || *self == StopState::SignalDeliveryStopped
    }

    /// Returns the "ed" version of this StopState, if it is "ing".
    pub fn finalize(&self) -> Result<StopState, ()> {
        match *self {
            StopState::GroupStopping => Ok(StopState::GroupStopped),
            StopState::SignalDeliveryStopping => Ok(StopState::SignalDeliveryStopped),
            StopState::Waking => Ok(StopState::Awake),
            StopState::ForceWaking => Ok(StopState::ForceAwake),
            _ => Err(()),
        }
    }

    pub fn is_downgrade(&self, new_state: &StopState) -> bool {
        match *self {
            StopState::GroupStopped => *new_state == StopState::GroupStopping,
            StopState::SignalDeliveryStopped => *new_state == StopState::SignalDeliveryStopping,
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
    timerslack_ns: u64,

    /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
    /// task.
    ///
    /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
    /// across tasks.
    default_timerslack_ns: u64,

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

    pub fn set_ptrace(&mut self, tracer: Option<pid_t>) -> Result<(), Errno> {
        if let Some(tracer) = tracer {
            if self.ptrace.is_some() {
                return Err(errno!(EPERM));
            }
            self.ptrace = Some(PtraceState::new(tracer));
        } else {
            self.ptrace = None;
        }
        Ok(())
    }

    pub fn is_ptraced(&self) -> bool {
        self.ptrace.is_some()
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

pub enum ExceptionResult {
    /// The exception was handled and no further action is required.
    Handled,

    // The exception generated a signal that should be delivered.
    Signal(SignalInfo),
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
    pub mm: Arc<MemoryManager>,

    /// The file system for this task.
    fs: Arc<FsContext>,

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
        Arc::ptr_eq(&self.mm, &other.mm)
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
    fn new(
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
            mm,
            fs,
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
        &self.fs
    }

    // See "Ptrace access mode checking" in https://man7.org/linux/man-pages/man2/ptrace.2.html
    pub fn check_ptrace_access_mode(
        &self,
        mode: PtraceAccessMode,
        target: &Task,
    ) -> Result<(), Errno> {
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
        let dumpable = *target.mm.dumpable.lock();
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

    pub fn create_init_child_process(
        kernel: &Arc<Kernel>,
        binary_path: &CString,
    ) -> Result<CurrentTask, Errno> {
        let weak_init = kernel.pids.read().get_task(1);
        let init_task = weak_init.upgrade().ok_or_else(|| errno!(EINVAL))?;
        let task = Self::create_process_without_parent(
            kernel,
            binary_path.clone(),
            init_task.fs().fork(),
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

    /// Create a task that is the leader of a new thread group.
    ///
    /// This function creates an underlying Zircon process to host the new
    /// task.
    ///
    /// `fs` should only be None for the init task, and set_fs should be called as soon as the
    /// FsContext is build.
    pub fn create_process_without_parent(
        kernel: &Arc<Kernel>,
        initial_name: CString,
        fs: Arc<FsContext>,
    ) -> Result<CurrentTask, Errno> {
        let initial_name_bytes = initial_name.as_bytes().to_owned();
        Self::create_task(kernel, initial_name, fs, |pid, process_group| {
            create_zircon_process(
                kernel,
                None,
                pid,
                process_group,
                SignalActions::default(),
                &initial_name_bytes,
            )
        })
    }

    /// Create a task that runs inside the kernel.
    ///
    /// There is no underlying Zircon process to host the task.
    pub fn create_kernel_task(
        kernel: &Arc<Kernel>,
        initial_name: CString,
        fs: Arc<FsContext>,
    ) -> Result<CurrentTask, Errno> {
        Self::create_task(kernel, initial_name, fs, |pid, process_group| {
            let process = zx::Process::from(zx::Handle::invalid());
            let memory_manager = Arc::new(MemoryManager::new_empty());
            let thread_group = ThreadGroup::new(
                kernel.clone(),
                process,
                None,
                pid,
                process_group,
                SignalActions::default(),
            );
            Ok(TaskInfo { thread: None, thread_group, memory_manager })
        })
    }

    fn create_task<F>(
        kernel: &Arc<Kernel>,
        initial_name: CString,
        root_fs: Arc<FsContext>,
        task_info_factory: F,
    ) -> Result<CurrentTask, Errno>
    where
        F: FnOnce(i32, Arc<ProcessGroup>) -> Result<TaskInfo, Errno>,
    {
        let mut pids = kernel.pids.write();
        let pid = pids.allocate_pid();

        let process_group = ProcessGroup::new(pid, None);
        pids.add_process_group(&process_group);

        let TaskInfo { thread, thread_group, memory_manager } =
            task_info_factory(pid, process_group.clone())?;

        process_group.insert(&thread_group);

        // > The timer slack values of init (PID 1), the ancestor of all processes, are 50,000
        // > nanoseconds (50 microseconds).  The timer slack value is inherited by a child created
        // > via fork(2), and is preserved across execve(2).
        // https://man7.org/linux/man-pages/man2/prctl.2.html
        let default_timerslack = 50_000;
        let current_task = CurrentTask::new(Self::new(
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
            None,
            Default::default(),
            None,
            Default::default(),
            kernel.root_uts_ns.clone(),
            false,
            SeccompState::default(),
            SeccompFilterContainer::default(),
            UserAddress::NULL.into(),
            default_timerslack,
        ));
        release_on_error!(current_task, (), {
            let temp_task = current_task.temp_task();
            current_task.thread_group.add(&temp_task)?;

            pids.add_task(&temp_task);
            pids.add_thread_group(&current_task.thread_group);
            Ok(())
        });
        Ok(current_task)
    }

    /// Create a kernel task in the same ThreadGroup as the given `system_task`.
    ///
    /// There is no underlying Zircon thread to host the task.
    pub fn create_kernel_thread(
        system_task: &Task,
        initial_name: CString,
    ) -> Result<CurrentTask, Errno> {
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

        let current_task = CurrentTask::new(Self::new(
            pid,
            initial_name,
            Arc::clone(&system_task.thread_group),
            None,
            FdTable::default(),
            Arc::clone(&system_task.mm),
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
        ));
        release_on_error!(current_task, (), {
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
    pub fn clone_task(
        &self,
        flags: u64,
        child_exit_signal: Option<Signal>,
        user_parent_tid: UserRef<pid_t>,
        user_child_tid: UserRef<pid_t>,
    ) -> Result<CurrentTask, Errno> {
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
            | CLONE_VFORK) as u64;
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
            // TODO(fxbug.dev/114813) Implement CLONE_VM for child processes (not just child
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
            not_implemented!("CLONE_THREAD without CLONE_VM is not implemented");
            return error!(ENOSYS);
        }

        if flags & !IMPLEMENTED_FLAGS != 0 {
            not_implemented!("clone does not implement flags: 0x{:x}", flags & !IMPLEMENTED_FLAGS);
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

            no_new_privs = state.no_new_privs;
            seccomp_filters = state.seccomp_filters.clone();
            child_signal_mask = state.signals.mask();

            pid = pids.allocate_pid();
            command = self.command();
            creds = self.creds();
            // TODO(https://fxbug.dev/297961833) implement SCHED_RESET_ON_FORK
            scheduler_policy = state.scheduler_policy;
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
                let memory_manager = self.mm.clone();
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

        let child = CurrentTask::new(Self::new(
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

        release_on_error!(child, (), {
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
                self.mm.snapshot_to(&child.mm)?;
            }

            if flags & (CLONE_PARENT_SETTID as u64) != 0 {
                self.mm.write_object(user_parent_tid, &child.id)?;
            }

            if flags & (CLONE_CHILD_CLEARTID as u64) != 0 {
                child.write().clear_child_tid = user_child_tid;
            }

            if flags & (CLONE_CHILD_SETTID as u64) != 0 {
                child.mm.vmo_write_object(user_child_tid, &child.id)?;
            }
            Ok(())
        });
        Ok(child)
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

    /// The flags indicates only the flags as in clone3(), and does not use the low 8 bits for the
    /// exit signal as in clone().
    #[cfg(test)]
    pub fn clone_task_for_test(
        &self,
        flags: u64,
        exit_signal: Option<Signal>,
    ) -> crate::testing::AutoReleasableTask {
        let result = self
            .clone_task(flags, exit_signal, UserRef::default(), UserRef::default())
            .expect("failed to create task in test");

        // Take the lock on thread group and task in the correct order to ensure any wrong ordering
        // will trigger the tracing-mutex at the right call site.
        {
            let _l1 = result.thread_group.read();
            let _l2 = result.read();
        }

        result.into()
    }

    /// If needed, clear the child tid for this task.
    ///
    /// Userspace can ask us to clear the child tid and issue a futex wake at
    /// the child tid address when we tear down a task. For example, bionic
    /// uses this mechanism to implement pthread_join. The thread that calls
    /// pthread_join sleeps using FUTEX_WAIT on the child tid address. We wake
    /// them up here to let them know the thread is done.
    fn clear_child_tid_if_needed(&self) -> Result<(), Errno> {
        let mut state = self.write();
        let user_tid = state.clear_child_tid;
        if !user_tid.is_null() {
            let zero: pid_t = 0;
            self.mm.write_object(user_tid, &zero)?;
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

    pub fn read_argv(&self) -> Result<Vec<FsString>, Errno> {
        let (argv_start, argv_end) = {
            let mm_state = self.mm.state.read();
            (mm_state.argv_start, mm_state.argv_end)
        };

        self.mm.read_nul_delimited_c_string_list(argv_start, argv_end - argv_start)
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
        if self.get_pid() == self.get_tid() {
            set_zx_name(&self.thread_group.process, name.as_bytes());
        }

        // Truncate to 16 bytes, including null byte.
        let bytes = name.to_bytes();
        self.persistent_info.lock().command = if bytes.len() > 15 {
            // SAFETY: Substring of a CString will contain no null bytes.
            CString::new(&bytes[..15]).unwrap()
        } else {
            name
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
            // stopping because the thread group has stopped
            self.set_stopped(&mut *self.write(), StopState::GroupStopped, None);
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
        let task_state = self.write();

        // If we've woken up, return.
        let task_stop_state = self.load_stopped();
        let group_stop_state = self.thread_group.load_stopped();
        if (task_stop_state == StopState::GroupStopped && group_stop_state.is_waking_or_awake())
            || task_stop_state.is_waking_or_awake()
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
        if group_stop_state.is_stopped() || task_stop_state.is_stopped() {
            group_state.stopped_waiters.wait_async(&waiter);
            task_state.wait_on_ptracer(&waiter);
        }
        false
    }
}

impl ReleasableByRef for Task {
    type Context<'a> = &'a CurrentTask;

    fn release(&self, current_task: &CurrentTask) {
        // Disconnect from tracer, if one is present.
        let ptracer_pid = self.read().ptrace.as_ref().map_or(None, |ptrace| Some(ptrace.pid));
        if let Some(ptracer_pid) = ptracer_pid {
            if let Some(ProcessEntryRef::Process(tg)) =
                self.kernel().pids.read().get_process(ptracer_pid)
            {
                let pid = self.get_pid();
                tg.ptracees.lock().remove(&pid);
            }
            let _ = self.write().set_ptrace(None);
        }

        self.thread_group.remove(self);

        // Release the fd table.
        self.files.release(current_task);

        self.signal_vfork();
    }
}

impl CurrentTask {
    fn new(task: Task) -> CurrentTask {
        CurrentTask {
            task: OwnedRefByRef::new(task),
            registers: RegisterState::default(),
            extended_pstate: ExtendedPstateState::default(),
            syscall_restart_func: None,
        }
    }

    pub fn weak_task(&self) -> WeakRef<Task> {
        WeakRef::from(&self.task)
    }

    pub fn temp_task(&self) -> TempRef<'_, Task> {
        TempRef::from(&self.task)
    }

    pub fn set_syscall_restart_func<R: Into<SyscallResult>>(
        &mut self,
        f: impl FnOnce(&mut CurrentTask) -> Result<R, Errno> + Send + Sync + 'static,
    ) {
        self.syscall_restart_func = Some(Box::new(|current_task| Ok(f(current_task)?.into())));
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
            self.set_flags(&mut *state, TaskFlags::TEMPORARY_SIGNAL_MASK, true);
            state.signals.set_temporary_mask(signal_mask);
        }
        wait_function(self)
    }

    /// Set the RunState for the current task to the given value and then call the given callback.
    ///
    /// When the callback is done, the run_state is restored to `RunState::Running`.
    ///
    /// This function is typically used just before blocking the current task on some operation.
    /// The given `run_state` registers the mechasim for interrupting the blocking operation with
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
            if state.signals.is_any_pending() {
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
        let dir = if !path.is_empty() && path[0] == b'/' {
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
        Ok((dir, path))
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
        dir: NamespaceNode,
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
                            self.resolve_open_path(context, dir, &path, mode, flags)
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
        let (name, created) = self.resolve_open_path(&mut context, dir, path, mode, flags)?;

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
        self.lookup_parent(context, dir, path)
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
        dir: NamespaceNode,
        path: &'a FsStr,
    ) -> Result<(NamespaceNode, &'a FsStr), Errno> {
        context.update_for_path(path);

        let mut current_node = dir;
        let mut it = path.split(|c| *c == b'/').filter(|p| !p.is_empty());
        let mut current_path_component = it.next().unwrap_or(b"");
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
        let (parent, basename) = self.lookup_parent(context, dir, path)?;
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

        // TODO(https://fxbug.dev/132623): Starnix doesn't yet support running exec on a
        // multi-thread process.
        if self.thread_group.read().tasks_count() > 1 {
            not_implemented!("exec on multithread process is not supported");
            return error!(EINVAL);
        }

        if let Err(err) = self.finish_exec(path, resolved_elf) {
            // TODO(tbodt): Replace this panic with a log and force a SIGSEGV.
            log_warn!("unrecoverable error in exec: {err:?}");
            send_signal(
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

        self.mm
            .exec(resolved_elf.file.name.clone())
            .map_err(|status| from_status_like_fdio!(status))?;
        let start_info = load_executable(self, resolved_elf, &path)?;
        let regs: zx_thread_state_general_regs_t = start_info.into();
        self.registers = regs.into();

        {
            let mut state = self.write();
            let mut persistent_info = self.persistent_info.lock();
            state.signals.alt_stack = None;
            state.robust_list_head = UserAddress::NULL.into();

            // TODO(tbodt): Check whether capability xattrs are set on the file, and grant/limit
            // capabilities accordingly.
            persistent_info.creds.exec();
        }
        self.extended_pstate.reset();

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
        self.set_command_name(basename);
        crate::logging::set_current_task_info(self);

        Ok(())
    }

    pub fn add_seccomp_filter(
        &mut self,
        bpf_filter: UserAddress,
        flags: u32,
    ) -> Result<SyscallResult, Errno> {
        let fprog: sock_fprog = self.mm.read_object(UserRef::new(bpf_filter))?;

        if u32::from(fprog.len) > BPF_MAXINSNS || fprog.len == 0 {
            return Err(errno!(EINVAL));
        }

        let code: Vec<sock_filter> =
            self.read_objects_to_vec(fprog.filter.into(), fprog.len as usize)?;

        let new_filter = Arc::new(SeccompFilter::from_cbpf(
            &code,
            self.thread_group.next_seccomp_filter_id.fetch_add(1, Ordering::SeqCst),
            flags & SECCOMP_FILTER_FLAG_LOG != 0,
        )?);

        let mut maybe_fd: Option<FdNumber> = None;

        if flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0 {
            let mut task_state = self.mutable_state.write();
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
                let other_task_state = task.mutable_state.read();

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
                let mut other_task_state = task.mutable_state.write();

                other_task_state.enable_no_new_privs();
                other_task_state.seccomp_filters = filters.clone();
                task.set_seccomp_state(SeccompStateValue::UserDefined)?;
            }
        } else {
            let mut task_state = self.mutable_state.write();

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
        let result = self.mutable_state.read().seccomp_filters.run_all(self, syscall);

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
    // TODO(fxbug.dev/128610): This only sets the FUTEX_OWNER_DIED bit; it does
    // not wake up a waiter.
    pub fn notify_robust_list(&self) {
        let task_state = self.write();
        let robust_list_addr = task_state.robust_list_head.addr();
        if robust_list_addr == UserAddress::NULL {
            // No one has called set_robust_list.
            return;
        }
        let robust_list_res = self.mm.read_object(task_state.robust_list_head);

        let head = if let Ok(head) = robust_list_res {
            head
        } else {
            return;
        };

        let offset = head.futex_offset;

        let mut entries_count = 0;
        let mut curr_ptr = head.list.next;
        while curr_ptr.addr != robust_list_addr.into() && entries_count < ROBUST_LIST_LIMIT {
            let curr_ref = self.mm.read_object(curr_ptr.into());

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

            let futex_ref = UserRef::<u32>::new(UserAddress::from(futex_base));

            // TODO(b/299096230): Futex modification should be atomic.
            let futex = if let Ok(futex) = self.mm.read_object(futex_ref) {
                futex
            } else {
                return;
            };

            if (futex & FUTEX_TID_MASK) as i32 == self.id {
                let owner_died = FUTEX_OWNER_DIED | futex;
                if self.write_object(futex_ref, &owner_died).is_err() {
                    return;
                }
            }
            curr_ptr = curr.next;
            entries_count += 1;
        }
    }

    /// Returns a ref to this thread's SeccompNotifier.
    pub fn get_seccomp_notifier(&mut self) -> Option<SeccompNotifierHandle> {
        self.mutable_state.write().seccomp_filters.notifier.clone()
    }

    pub fn set_seccomp_notifier(&mut self, notifier: Option<SeccompNotifierHandle>) {
        self.mutable_state.write().seccomp_filters.notifier = notifier;
    }

    /// Processes a Zircon exception associated with this task.
    ///
    /// If the exception is fully handled, returns Ok(None)
    /// If the exception produces a signal, returns Ok(Some(SigInfo)).
    /// If the exception could not be handled returns Err(())
    pub fn process_exception(&self, report: &zx::sys::zx_exception_report_t) -> ExceptionResult {
        match report.header.type_ {
            zx::sys::ZX_EXCP_GENERAL => match get_signal_for_general_exception(&report.context) {
                Some(sig) => ExceptionResult::Signal(SignalInfo::default(sig)),
                None => {
                    log_warn!("Unrecognized general exception: {:?}", report);
                    ExceptionResult::Signal(SignalInfo::default(SIGILL))
                }
            },
            zx::sys::ZX_EXCP_FATAL_PAGE_FAULT => {
                // A page fault may be resolved by extending a growsdown mapping to cover the faulting
                // address. Ask the memory manager if it can extend a mapping to cover the faulting
                // address and if says that it's found a mapping that exists or that can be extended to
                // cover this address mark the exception as handled so that the instruction can try
                // again. Otherwise let the regular handling proceed.

                // We should only attempt growth on a not-present fault and we should only extend if the
                // access type matches the protection on the GROWSDOWN mapping.
                let decoded = decode_page_fault_exception_report(report);
                if decoded.not_present {
                    match self.mm.extend_growsdown_mapping_to_address(
                        UserAddress::from(decoded.faulting_address),
                        decoded.is_write,
                    ) {
                        Ok(true) => {
                            return ExceptionResult::Handled;
                        }
                        Err(e) => {
                            log_warn!("Error handling page fault: {e}")
                        }
                        _ => {}
                    }
                }
                // For this exception type, the synth_code field in the exception report's context is the
                // error generated by the page fault handler. For us this is used to distinguish between a
                // segmentation violation and a bus error. Unfortunately this detail is not documented in
                // Zircon's public documentation and is only described in the architecture-specific exception
                // definitions such as:
                // zircon/kernel/arch/x86/include/arch/x86.h
                // zircon/kernel/arch/arm64/include/arch/arm64.h
                let signo = match report.context.synth_code as zx::sys::zx_status_t {
                    zx::sys::ZX_ERR_OUT_OF_RANGE => SIGBUS,
                    _ => SIGSEGV,
                };
                ExceptionResult::Signal(SignalInfo::new(
                    signo,
                    SI_KERNEL as i32,
                    SignalDetail::SigFault { addr: decoded.faulting_address },
                ))
            }
            zx::sys::ZX_EXCP_UNDEFINED_INSTRUCTION => {
                ExceptionResult::Signal(SignalInfo::default(SIGILL))
            }
            zx::sys::ZX_EXCP_UNALIGNED_ACCESS => {
                ExceptionResult::Signal(SignalInfo::default(SIGBUS))
            }
            zx::sys::ZX_EXCP_SW_BREAKPOINT => ExceptionResult::Signal(SignalInfo::default(SIGTRAP)),
            _ => {
                log_error!("Unknown exception {:?}", report);
                ExceptionResult::Signal(SignalInfo::default(SIGSEGV))
            }
        }
    }
}

impl MemoryAccessor for Task {
    fn read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        self.mm.read_memory_to_slice(addr, bytes)
    }

    fn vmo_read_memory_to_slice(&self, addr: UserAddress, bytes: &mut [u8]) -> Result<(), Errno> {
        self.mm.vmo_read_memory_to_slice(addr, bytes)
    }

    fn read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        self.mm.read_memory_partial_to_slice(addr, bytes)
    }

    fn vmo_read_memory_partial_to_slice(
        &self,
        addr: UserAddress,
        bytes: &mut [u8],
    ) -> Result<usize, Errno> {
        self.mm.vmo_read_memory_partial_to_slice(addr, bytes)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.write_memory(addr, bytes)
    }

    fn vmo_write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.vmo_write_memory(addr, bytes)
    }

    fn write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.write_memory_partial(addr, bytes)
    }

    fn vmo_write_memory_partial(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        self.mm.vmo_write_memory_partial(addr, bytes)
    }

    fn zero(&self, addr: UserAddress, length: usize) -> Result<usize, Errno> {
        self.mm.zero(addr, length)
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

impl fmt::Debug for CurrentTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.task.fmt(f)
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
    use crate::{
        testing::*,
        types::signals::SIGCHLD,
        types::{rlimit, Resource},
    };

    #[::fuchsia::test]
    async fn test_tid_allocation() {
        let (kernel, current_task) = create_kernel_and_task();

        assert_eq!(current_task.get_tid(), 1);
        let another_current = create_task(&kernel, "another-task");
        // tid 2 gets assigned to kthreadd.
        assert_eq!(another_current.get_tid(), 3);

        let pids = kernel.pids.read();
        assert_eq!(pids.get_task(1).upgrade().unwrap().get_tid(), 1);
        assert_eq!(pids.get_task(2).upgrade().unwrap().get_tid(), 2);
        assert_eq!(pids.get_task(3).upgrade().unwrap().get_tid(), 3);
        assert!(pids.get_task(4).upgrade().is_none());
    }

    #[::fuchsia::test]
    async fn test_clone_pid_and_parent_pid() {
        let (_kernel, current_task) = create_kernel_and_task();
        let thread = current_task
            .clone_task_for_test((CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) as u64, Some(SIGCHLD));
        assert_eq!(current_task.get_pid(), thread.get_pid());
        assert_ne!(current_task.get_tid(), thread.get_tid());
        assert_eq!(current_task.thread_group.leader, thread.thread_group.leader);

        let child_task = current_task.clone_task_for_test(0, Some(SIGCHLD));
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
        let (_kernel, current_task) = create_kernel_and_task();
        let prev_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_ne!(prev_fsize, 10);
        current_task
            .thread_group
            .limits
            .lock()
            .set(Resource::FSIZE, rlimit { rlim_cur: 10, rlim_max: 100 });
        let current_fsize = current_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(current_fsize, 10);

        let child_task = current_task.clone_task_for_test(0, Some(SIGCHLD));
        let child_fsize = child_task.thread_group.get_rlimit(Resource::FSIZE);
        assert_eq!(child_fsize, 10)
    }
}
