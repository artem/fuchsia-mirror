// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::terminal::{ControllingSession, Terminal},
    mutable_state::{state_accessor, state_implementation},
    selinux::hooks::thread_group_hooks::SeLinuxThreadGroupState,
    signals::{
        send_signal, send_standard_signal, syscalls::WaitingOptions, SignalActions, SignalDetail,
        SignalInfo,
    },
    task::{
        interval_timer::IntervalTimerHandle, ptrace_detach, AtomicStopState, ClockId,
        ControllingTerminal, CurrentTask, ExitStatus, Kernel, PidTable, ProcessGroup,
        PtraceAllowedPtracers, PtraceEvent, PtraceOptions, PtraceStatus, Session, StopState, Task,
        TaskMutableState, TaskPersistentInfo, TaskPersistentInfoState, TimerId, TimerTable,
        WaitQueue,
    },
    time::utc,
};
use fuchsia_zircon as zx;
use itertools::Itertools;
use macro_rules_attribute::apply;
use selinux::SecurityId;
use starnix_lifecycle::{AtomicU64Counter, DropNotifier};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_sync::{LockBefore, Locked, Mutex, MutexGuard, ProcessGroupState, RwLock};
use starnix_uapi::{
    auth::{Credentials, CAP_SYS_ADMIN, CAP_SYS_RESOURCE},
    errno, error,
    errors::Errno,
    itimerval,
    ownership::{OwnedRef, Releasable, TempRef, WeakRef},
    personality::PersonalityFlags,
    pid_t,
    resource_limits::{Resource, ResourceLimits},
    rlimit,
    signals::{Signal, UncheckedSignal, SIGCHLD, SIGCONT, SIGHUP, SIGKILL, SIGTTOU},
    stats::TaskTimeStats,
    time::{duration_from_timeval, timeval_from_duration},
    uid_t,
    user_address::UserAddress,
    CLOCK_REALTIME, ITIMER_PROF, ITIMER_REAL, ITIMER_VIRTUAL, SIG_IGN,
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{atomic::Ordering, Arc, Weak},
};

/// The mutable state of the ThreadGroup.
pub struct ThreadGroupMutableState {
    /// The parent thread group.
    ///
    /// The value needs to be writable so that it can be re-parent to the correct subreaper if the
    /// parent ends before the child.
    pub parent: Option<Arc<ThreadGroup>>,

    /// The tasks in the thread group.
    ///
    /// The references to Task is weak to prevent cycles as Task have a Arc reference to their
    /// thread group.
    /// It is still expected that these weak references are always valid, as tasks must unregister
    /// themselves before they are deleted.
    tasks: BTreeMap<pid_t, TaskContainer>,

    /// The children of this thread group.
    ///
    /// The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc reference
    /// to their parent.
    /// It is still expected that these weak references are always valid, as thread groups must unregister
    /// themselves before they are deleted.
    pub children: BTreeMap<pid_t, Weak<ThreadGroup>>,

    /// Child tasks that have exited, but not yet been waited for.
    pub zombie_children: Vec<OwnedRef<ZombieProcess>>,

    /// ptracees that have exited, but not yet been waited for.
    pub zombie_ptracees: Vec<OwnedRef<ZombieProcess>>,

    /// WaitQueue for updates to the WaitResults of tasks in this group.
    pub child_status_waiters: WaitQueue,

    /// Whether this thread group will inherit from children of dying processes in its descendant
    /// tree.
    pub is_child_subreaper: bool,

    /// The IDs used to perform shell job control.
    pub process_group: Arc<ProcessGroup>,

    /// The timers for this thread group (from timer_create(), etc.).
    pub timers: TimerTable,

    pub did_exec: bool,

    /// Wait queue for updates to `stopped`.
    pub stopped_waiters: WaitQueue,

    /// A signal that indicates whether the process is going to become waitable
    /// via waitid and waitpid for either WSTOPPED or WCONTINUED, depending on
    /// the value of `stopped`. If not None, contains the SignalInfo to return.
    pub last_signal: Option<SignalInfo>,

    pub leader_exit_info: Option<ProcessExitInfo>,

    pub terminating: bool,

    /// The SELinux operations for this thread group.
    pub selinux_state: Option<SeLinuxThreadGroupState>,

    /// Time statistics accumulated from the children.
    pub children_time_stats: TaskTimeStats,

    /// Personality flags set with `sys_personality()`.
    pub personality: PersonalityFlags,

    /// Thread groups allowed to trace tasks in this this thread group.
    pub allowed_ptracers: PtraceAllowedPtracers,
}

/// A collection of `Task` objects that roughly correspond to a "process".
///
/// Userspace programmers often think about "threads" and "process", but those concepts have no
/// clear analogs inside the kernel because tasks are typically created using `clone(2)`, which
/// takes a complex set of flags that describes how much state is shared between the original task
/// and the new task.
///
/// If a new task is created with the `CLONE_THREAD` flag, the new task will be placed in the same
/// `ThreadGroup` as the original task. Userspace typically uses this flag in conjunction with the
/// `CLONE_FILES`, `CLONE_VM`, and `CLONE_FS`, which corresponds to the userspace notion of a
/// "thread". For example, that's how `pthread_create` behaves. In that sense, a `ThreadGroup`
/// normally corresponds to the set of "threads" in a "process". However, this pattern is purely a
/// userspace convention, and nothing stops userspace from using `CLONE_THREAD` without
/// `CLONE_FILES`, for example.
///
/// In Starnix, a `ThreadGroup` corresponds to a Zicon process, which means we do not support the
/// `CLONE_THREAD` flag without the `CLONE_VM` flag. If we run into problems with this limitation,
/// we might need to revise this correspondence.
///
/// Each `Task` in a `ThreadGroup` has the same thread group ID (`tgid`). The task with the same
/// `pid` as the `tgid` is called the thread group leader.
///
/// Thread groups are destroyed when the last task in the group exits.
pub struct ThreadGroup {
    /// The kernel to which this thread group belongs.
    pub kernel: Arc<Kernel>,

    /// A handle to the underlying Zircon process object.
    ///
    /// Currently, we have a 1-to-1 mapping between thread groups and zx::process
    /// objects. This approach might break down if/when we implement CLONE_VM
    /// without CLONE_THREAD because that creates a situation where two thread
    /// groups share an address space. To implement that situation, we might
    /// need to break the 1-to-1 mapping between thread groups and zx::process
    /// or teach zx::process to share address spaces.
    pub process: zx::Process,

    /// The lead task of this thread group.
    ///
    /// The lead task is typically the initial thread created in the thread group.
    pub leader: pid_t,

    /// The signal actions that are registered for this process.
    pub signal_actions: Arc<SignalActions>,

    /// A mechanism to be notified when this `ThreadGroup` is destroyed.
    pub drop_notifier: DropNotifier,

    /// Whether the process is currently stopped.
    ///
    /// Must only be set when the `mutable_state` write lock is held.
    stop_state: AtomicStopState,

    /// The mutable state of the ThreadGroup.
    mutable_state: RwLock<ThreadGroupMutableState>,

    /// The resource limits for this thread group.  This is outside mutable_state
    /// to avoid deadlocks where the thread_group lock is held when acquiring
    /// the task lock, and vice versa.
    pub limits: Mutex<ResourceLimits>,

    /// The next unique identifier for a seccomp filter.  These are required to be
    /// able to distinguish identical seccomp filters, which are treated differently
    /// for the purposes of SECCOMP_FILTER_FLAG_TSYNC.  Inherited across clone because
    /// seccomp filters are also inherited across clone.
    pub next_seccomp_filter_id: AtomicU64Counter,

    /// Timer id of ITIMER_REAL.
    itimer_real_id: TimerId,

    /// Tasks ptraced by this process
    pub ptracees: Mutex<BTreeMap<pid_t, TaskContainer>>,
}

impl fmt::Debug for ThreadGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.leader)
    }
}

impl PartialEq for ThreadGroup {
    fn eq(&self, other: &Self) -> bool {
        self.leader == other.leader
    }
}

/// A selector that can match a process. Works as a representation of the pid argument to syscalls
/// like wait and kill.
#[derive(Debug, Clone, Copy)]
pub enum ProcessSelector {
    /// Matches any process at all.
    Any,
    /// Matches only the process with the specified pid
    Pid(pid_t),
    /// Matches all the processes in the given process group
    Pgid(pid_t),
}

impl ProcessSelector {
    pub fn do_match(&self, pid: pid_t, pid_table: &PidTable) -> bool {
        match *self {
            ProcessSelector::Pid(p) => pid == p,
            ProcessSelector::Any => true,
            ProcessSelector::Pgid(pgid) => {
                if let Some(task_ref) = pid_table.get_task(pid).upgrade() {
                    pid_table.get_process_group(pgid).as_ref()
                        == Some(&task_ref.thread_group.read().process_group)
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExitInfo {
    pub status: ExitStatus,
    pub exit_signal: Option<Signal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitResult {
    pub pid: pid_t,
    pub uid: uid_t,

    pub exit_info: ProcessExitInfo,

    /// Cumulative time stats for the process and its children.
    pub time_stats: TaskTimeStats,
}

impl WaitResult {
    // According to wait(2) man page, SignalInfo.signal needs to always be set to SIGCHLD
    pub fn as_signal_info(&self) -> SignalInfo {
        SignalInfo::new(
            SIGCHLD,
            self.exit_info.status.signal_info_code(),
            SignalDetail::SIGCHLD {
                pid: self.pid,
                uid: self.uid,
                status: self.exit_info.status.signal_info_status(),
            },
        )
    }
}

#[derive(Debug)]
pub struct ZombieProcess {
    pub pid: pid_t,
    pub pgid: pid_t,
    pub uid: uid_t,

    pub exit_info: ProcessExitInfo,

    /// Cumulative time stats for the process and its children.
    pub time_stats: TaskTimeStats,

    /// Whether dropping this ZombieProcess should imply removing the pid from
    /// the PidTable
    pub is_canonical: bool,
}

impl ZombieProcess {
    pub fn new(
        thread_group: ThreadGroupStateRef<'_>,
        credentials: &Credentials,
        exit_info: ProcessExitInfo,
    ) -> OwnedRef<Self> {
        let time_stats = thread_group.base.time_stats() + thread_group.children_time_stats;
        OwnedRef::new(ZombieProcess {
            pid: thread_group.base.leader,
            pgid: thread_group.process_group.leader,
            uid: credentials.uid,
            exit_info,
            time_stats,
            is_canonical: true,
        })
    }

    pub fn to_wait_result(&self) -> WaitResult {
        WaitResult {
            pid: self.pid,
            uid: self.uid,
            exit_info: self.exit_info.clone(),
            time_stats: self.time_stats,
        }
    }
}

impl Releasable for ZombieProcess {
    type Context<'a> = &'a mut PidTable;

    fn release(self, pids: &mut PidTable) {
        if self.is_canonical {
            pids.remove_zombie(self.pid);
        }
    }
}

impl ThreadGroup {
    pub fn new<L>(
        locked: &mut Locked<'_, L>,
        kernel: Arc<Kernel>,
        process: zx::Process,
        parent: Option<ThreadGroupWriteGuard<'_>>,
        leader: pid_t,
        process_group: Arc<ProcessGroup>,
        signal_actions: Arc<SignalActions>,
    ) -> Arc<ThreadGroup>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let timers = TimerTable::new();
        let itimer_real_id = timers.create(CLOCK_REALTIME as ClockId, None).unwrap();
        // TODO(http://b/316181721): propagate initial contexts to tasks.
        let selinux_state =
            kernel.security_server.as_ref().map(|ss| SeLinuxThreadGroupState::new_default(ss));
        let mut thread_group = ThreadGroup {
            kernel,
            process,
            leader,
            signal_actions,
            itimer_real_id,
            drop_notifier: Default::default(),
            // A child process created via fork(2) inherits its parent's
            // resource limits.  Resource limits are preserved across execve(2).
            limits: Mutex::new(
                parent.as_ref().map(|p| p.base.limits.lock().clone()).unwrap_or(Default::default()),
            ),
            next_seccomp_filter_id: Default::default(),
            ptracees: Default::default(),
            stop_state: AtomicStopState::new(StopState::Awake),
            mutable_state: RwLock::new(ThreadGroupMutableState {
                parent: parent.as_ref().map(|p| Arc::clone(p.base)),
                tasks: BTreeMap::new(),
                children: BTreeMap::new(),
                zombie_children: vec![],
                zombie_ptracees: vec![],
                child_status_waiters: WaitQueue::default(),
                is_child_subreaper: false,
                process_group: Arc::clone(&process_group),
                timers,
                did_exec: false,
                stopped_waiters: WaitQueue::default(),
                last_signal: None,
                leader_exit_info: None,
                terminating: false,
                selinux_state,
                children_time_stats: Default::default(),
                personality: parent.as_ref().map(|p| p.personality).unwrap_or(Default::default()),
                allowed_ptracers: PtraceAllowedPtracers::None,
            }),
        };

        let thread_group = if let Some(mut parent) = parent {
            thread_group.next_seccomp_filter_id.reset(parent.base.next_seccomp_filter_id.get());
            let thread_group = Arc::new(thread_group);
            parent.children.insert(leader, Arc::downgrade(&thread_group));
            process_group.insert(locked, &thread_group);
            thread_group
        } else {
            Arc::new(thread_group)
        };
        thread_group
    }

    state_accessor!(ThreadGroup, mutable_state, Arc<ThreadGroup>);

    pub fn load_stopped(&self) -> StopState {
        self.stop_state.load(Ordering::Relaxed)
    }

    // Causes the thread group to exit.  If this is being called from a task
    // that is part of the current thread group, the caller should pass
    // `current_task`.  If ownership issues prevent passing `current_task`, then
    // callers should use CurrentTask::thread_group_exit instead.
    pub fn exit(
        self: &Arc<Self>,
        exit_status: ExitStatus,
        mut current_task: Option<&mut CurrentTask>,
    ) {
        if let Some(ref mut current_task) = current_task {
            current_task
                .ptrace_event(PtraceOptions::TRACEEXIT, exit_status.signal_info_status() as u64);
        }
        let mut pids = self.kernel.pids.write();
        let mut state = self.write();
        if state.terminating {
            // The thread group is already terminating and all threads in the thread group have
            // already been interrupted.
            return;
        }
        state.terminating = true;

        // Drop ptrace zombies
        for zombie in state.zombie_ptracees.drain(..) {
            zombie.release(&mut pids);
        }

        // Interrupt each task. Unlock the group because send_signal will lock the group in order
        // to call set_stopped.
        // SAFETY: tasks is kept on the stack. The static is required to ensure the lock on
        // ThreadGroup can be dropped.
        let tasks = state.tasks().map(TempRef::into_static).collect::<Vec<_>>();
        drop(state);

        // Detach from any ptraced tasks.
        let tracees = self.ptracees.lock().keys().cloned().collect::<Vec<_>>();
        for tracee in tracees {
            if let Some(task_ref) = pids.get_task(tracee).clone().upgrade() {
                let _ = ptrace_detach(self.clone(), task_ref.as_ref(), &UserAddress::NULL);
            }
        }

        for task in tasks {
            task.write().set_exit_status(exit_status.clone());
            send_standard_signal(&task, SignalInfo::default(SIGKILL));
        }
    }

    pub fn add(self: &Arc<Self>, task: &TempRef<'_, Task>) -> Result<(), Errno> {
        let mut state = self.write();
        if state.terminating {
            return error!(EINVAL);
        }
        state.tasks.insert(task.id, task.into());
        Ok(())
    }

    pub fn remove<L>(self: &Arc<Self>, locked: &mut Locked<'_, L>, task: &Task)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let mut pids = self.kernel.pids.write();
        let mut state = self.write();

        pids.remove_task(task.id);
        let persistent_info: TaskPersistentInfo =
            if let Some(container) = state.tasks.remove(&task.id) {
                container.into()
            } else {
                // The task has never been added. The only expected case is that this thread was
                // already terminating.
                debug_assert!(state.terminating);
                return;
            };

        if task.id == self.leader {
            let exit_status = task.exit_status().unwrap_or_else(|| {
                log_error!("Exiting without an exit code.");
                ExitStatus::Exit(u8::MAX)
            });
            state.leader_exit_info = Some(ProcessExitInfo {
                status: exit_status,
                exit_signal: *persistent_info.lock().exit_signal(),
            });
        }

        if state.tasks.is_empty() {
            state.terminating = true;

            // Replace PID table entry with a zombie.
            let exit_info =
                state.leader_exit_info.take().expect("Failed to capture leader exit status");
            let zombie =
                ZombieProcess::new(state.as_ref(), persistent_info.lock().creds(), exit_info);
            pids.kill_process(self.leader, OwnedRef::downgrade(&zombie));

            state.leave_process_group(locked, &mut pids);

            // I have no idea if dropping the lock here is correct, and I don't want to think about
            // it. If problems do turn up with another thread observing an intermediate state of
            // this exit operation, the solution is to unify locks. It should be sensible and
            // possible for there to be a single lock that protects all (or nearly all) of the
            // data accessed by both exit and wait. In gvisor and linux this is the lock on the
            // equivalent of the PidTable. This is made more difficult by rust locks being
            // containers that only lock the data they contain, but see
            // https://docs.google.com/document/d/1YHrhBqNhU1WcrsYgGAu3JwwlVmFXPlwWHTJLAbwRebY/edit
            // for an idea.
            std::mem::drop(state);

            // We will need the immediate parent and the reaper. Once we have them, we can make
            // sure to take the locks in the right order: parent before child.
            let parent = self.read().parent.clone();
            let reaper = self.find_reaper();

            {
                // Reparent the children.
                if let Some(reaper) = reaper {
                    let mut reaper = reaper.write();
                    let mut state = self.write();
                    for (_pid, child) in std::mem::take(&mut state.children) {
                        if let Some(child) = child.upgrade() {
                            child.write().parent = Some(Arc::clone(reaper.base));
                            reaper.children.insert(child.leader, Arc::downgrade(&child));
                        }
                    }
                    reaper.zombie_children.append(&mut state.zombie_children);
                } else {
                    // If we don't have a reaper then just drop the zombies.
                    let mut state = self.write();
                    for zombie in state.zombie_children.drain(..) {
                        zombie.release(&mut pids);
                    }
                }
            }

            if let Some(ref parent) = parent {
                let mut parent = parent.write();

                parent.children.remove(&self.leader);

                // Send signals
                if let Some(exit_signal) = zombie.exit_info.exit_signal {
                    if let Some(signal_target) = parent.get_signal_target(exit_signal.into()) {
                        let mut signal_info = zombie.to_wait_result().as_signal_info();
                        signal_info.signal = exit_signal;
                        send_signal(&signal_target, signal_info).unwrap_or_else(|e| {
                            log_warn!("Failed to send exit signal: {}", e);
                        });
                    }
                }
                parent.zombie_children.push(zombie);
                parent.child_status_waiters.notify_all();
            } else {
                zombie.release(&mut pids);
            }

            // TODO: Set the error_code on the Zircon process object. Currently missing a way
            // to do this in Zircon. Might be easier in the new execution model.

            // Once the last zircon thread stops, the zircon process will also stop executing.

            if let Some(parent) = parent {
                parent.check_orphans(locked);
            }
        }
    }

    /// Find the task which will adopt our children after we die.
    fn find_reaper(self: &Arc<Self>) -> Option<Arc<Self>> {
        let mut parent = Arc::clone(self.read().parent.as_ref()?);
        loop {
            parent = {
                let parent_state = parent.read();
                if parent_state.is_child_subreaper {
                    break;
                }
                match parent_state.parent {
                    Some(ref next_parent) => Arc::clone(next_parent),
                    None => break,
                }
            }
        }
        Some(parent)
    }

    pub fn setsid<L>(self: &Arc<Self>, locked: &mut Locked<'_, L>) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        {
            let mut pids = self.kernel.pids.write();
            if pids.get_process_group(self.leader).is_some() {
                return error!(EPERM);
            }
            let process_group = ProcessGroup::new(self.leader, None);
            pids.add_process_group(&process_group);
            self.write().set_process_group(locked, process_group, &mut pids);
        }
        self.check_orphans(locked);

        Ok(())
    }

    pub fn setpgid<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        target: &Task,
        pgid: pid_t,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        {
            let mut pids = self.kernel.pids.write();

            let current_process_group = Arc::clone(&self.read().process_group);

            // The target process must be either the current process of a child of the current process
            let mut target_thread_group = target.thread_group.write();
            let is_target_current_process_child =
                target_thread_group.parent.as_ref().map(|tg| tg.leader) == Some(self.leader);
            if target_thread_group.leader() != self.leader && !is_target_current_process_child {
                return error!(ESRCH);
            }

            // If the target process is a child of the current task, it must not have executed one of the exec
            // function.
            if is_target_current_process_child && target_thread_group.did_exec {
                return error!(EACCES);
            }

            let new_process_group;
            {
                let target_process_group = &target_thread_group.process_group;

                // The target process must not be a session leader and must be in the same session as the current process.
                if target_thread_group.leader() == target_process_group.session.leader
                    || current_process_group.session != target_process_group.session
                {
                    return error!(EPERM);
                }

                let target_pgid = if pgid == 0 { target_thread_group.leader() } else { pgid };
                if target_pgid < 0 {
                    return error!(EINVAL);
                }

                if target_pgid == target_process_group.leader {
                    return Ok(());
                }

                // If pgid is not equal to the target process id, the associated process group must exist
                // and be in the same session as the target process.
                if target_pgid != target_thread_group.leader() {
                    new_process_group =
                        pids.get_process_group(target_pgid).ok_or_else(|| errno!(EPERM))?;
                    if new_process_group.session != target_process_group.session {
                        return error!(EPERM);
                    }
                } else {
                    // Create a new process group
                    new_process_group =
                        ProcessGroup::new(target_pgid, Some(target_process_group.session.clone()));
                    pids.add_process_group(&new_process_group);
                }
            }

            target_thread_group.set_process_group(locked, new_process_group, &mut pids);
        }
        target.thread_group.check_orphans(locked);

        Ok(())
    }

    fn itimer_real(self: &Arc<Self>) -> IntervalTimerHandle {
        self.write().timers.get_timer(self.itimer_real_id).expect("no ITIMER_REAL exists")
    }

    pub fn set_itimer(self: &Arc<Self>, which: u32, value: itimerval) -> Result<itimerval, Errno> {
        if which == ITIMER_PROF || which == ITIMER_VIRTUAL {
            // We don't support setting these timers.
            // The gvisor test suite clears ITIMER_PROF as part of its test setup logic, so we support
            // clearing these values.
            if value.it_value.tv_sec == 0 && value.it_value.tv_usec == 0 {
                return Ok(itimerval::default());
            }
            track_stub!(TODO("https://fxbug.dev/322874521"), "Unsupported itimer type", which);
            return Err(errno!(ENOTSUP));
        }

        if which != ITIMER_REAL {
            return Err(errno!(EINVAL));
        }
        let itimer_real = self.itimer_real();
        let prev_remaining = itimer_real.time_remaining();
        if value.it_value.tv_sec != 0 || value.it_value.tv_usec != 0 {
            itimer_real.arm(
                &self.kernel,
                Arc::downgrade(self),
                utc::utc_now() + duration_from_timeval(value.it_value)?,
                duration_from_timeval(value.it_interval)?,
            );
        } else {
            itimer_real.disarm();
        }
        Ok(itimerval {
            it_value: timeval_from_duration(prev_remaining.remainder),
            it_interval: timeval_from_duration(prev_remaining.interval),
        })
    }

    pub fn get_itimer(self: &Arc<Self>, which: u32) -> Result<itimerval, Errno> {
        if which == ITIMER_PROF || which == ITIMER_VIRTUAL {
            // We don't support setting these timers, so we can accurately report that these are not set.
            return Ok(itimerval::default());
        }
        if which != ITIMER_REAL {
            return Err(errno!(EINVAL));
        }
        let remaining = self.itimer_real().time_remaining();
        Ok(itimerval {
            it_value: timeval_from_duration(remaining.remainder),
            it_interval: timeval_from_duration(remaining.interval),
        })
    }

    /// Check whether the stop state is compatible with `new_stopped`. If it is return it,
    /// otherwise, return None.
    fn check_stopped_state(
        self: &Arc<Self>,
        new_stopped: StopState,
        finalize_only: bool,
    ) -> Option<StopState> {
        let stopped = self.load_stopped();
        if finalize_only && !stopped.is_stopping_or_stopped() {
            return Some(stopped);
        }

        if stopped.is_illegal_transition(new_stopped) {
            return Some(stopped);
        }

        return None;
    }

    /// Set the stop status of the process.  If you pass |siginfo| of |None|,
    /// does not update the signal.  If |finalize_only| is set, will check that
    /// the set will be a finalize (Stopping -> Stopped or Stopped -> Stopped)
    /// before executing it.
    ///
    /// Returns the latest stop state after any changes.
    pub fn set_stopped(
        self: &Arc<Self>,
        new_stopped: StopState,
        siginfo: Option<SignalInfo>,
        finalize_only: bool,
    ) -> StopState {
        // Perform an early return check to see if we can avoid taking the lock.
        if let Some(stopped) = self.check_stopped_state(new_stopped, finalize_only) {
            return stopped;
        }

        self.write().set_stopped(new_stopped, siginfo, finalize_only)
    }

    /// Ensures |session| is the controlling session inside of |controlling_session|, and returns a
    /// reference to the |ControllingSession|.
    fn check_controlling_session<'a>(
        session: &Arc<Session>,
        controlling_session: &'a Option<ControllingSession>,
    ) -> Result<&'a ControllingSession, Errno> {
        if let Some(controlling_session) = controlling_session {
            if controlling_session.session.as_ptr() == Arc::as_ptr(session) {
                return Ok(controlling_session);
            }
        }
        error!(ENOTTY)
    }

    pub fn get_foreground_process_group(
        self: &Arc<Self>,
        terminal: &Arc<Terminal>,
        is_main: bool,
    ) -> Result<pid_t, Errno> {
        let state = self.read();
        let process_group = &state.process_group;
        let terminal_state = terminal.read();
        let controlling_session = terminal_state.get_controlling_session(is_main);

        // "When fd does not refer to the controlling terminal of the calling
        // process, -1 is returned" - tcgetpgrp(3)
        let cs = Self::check_controlling_session(&process_group.session, controlling_session)?;
        Ok(cs.foregound_process_group_leader)
    }

    pub fn set_foreground_process_group<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        is_main: bool,
        pgid: pid_t,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let process_group;
        let send_ttou;
        {
            // Keep locks to ensure atomicity.
            let pids = self.kernel.pids.read();
            let state = self.read();
            process_group = Arc::clone(&state.process_group);
            let mut terminal_state = terminal.write();
            let controlling_session = terminal_state.get_controlling_session_mut(is_main);
            let cs = Self::check_controlling_session(&process_group.session, controlling_session)?;

            // pgid must be positive.
            if pgid < 0 {
                return error!(EINVAL);
            }

            let new_process_group = pids.get_process_group(pgid).ok_or_else(|| errno!(ESRCH))?;
            if new_process_group.session != process_group.session {
                return error!(EPERM);
            }

            // If the calling process is a member of a background group and not ignoring SIGTTOU, a
            // SIGTTOU signal is sent to all members of this background process group.
            send_ttou = process_group.leader != cs.foregound_process_group_leader
                && !current_task.read().signals.mask().has_signal(SIGTTOU)
                && self.signal_actions.get(SIGTTOU).sa_handler != SIG_IGN;
            if !send_ttou {
                *controlling_session = controlling_session
                    .as_ref()
                    .unwrap()
                    .set_foregound_process_group(&new_process_group);
            }
        }

        // Locks must not be held when sending signals.
        if send_ttou {
            process_group.send_signals(locked, &[SIGTTOU]);
            return error!(EINTR);
        }

        Ok(())
    }

    pub fn set_controlling_terminal(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        is_main: bool,
        steal: bool,
        is_readable: bool,
    ) -> Result<(), Errno> {
        // Keep locks to ensure atomicity.
        let state = self.read();
        let process_group = &state.process_group;
        let mut terminal_state = terminal.write();
        let controlling_session = terminal_state.get_controlling_session_mut(is_main);
        let mut session_writer = process_group.session.write();

        // "The calling process must be a session leader and not have a
        // controlling terminal already." - tty_ioctl(4)
        if process_group.session.leader != self.leader
            || session_writer.controlling_terminal.is_some()
        {
            return error!(EINVAL);
        }

        let has_admin = current_task.creds().has_capability(CAP_SYS_ADMIN);

        // "If this terminal is already the controlling terminal of a different
        // session group, then the ioctl fails with EPERM, unless the caller
        // has the CAP_SYS_ADMIN capability and arg equals 1, in which case the
        // terminal is stolen, and all processes that had it as controlling
        // terminal lose it." - tty_ioctl(4)
        if let Some(other_session) =
            controlling_session.as_ref().and_then(|cs| cs.session.upgrade())
        {
            if other_session != process_group.session {
                if !has_admin || !steal {
                    return error!(EPERM);
                }

                // Steal the TTY away. Unlike TIOCNOTTY, don't send signals.
                other_session.write().controlling_terminal = None;
            }
        }

        if !is_readable && !has_admin {
            return error!(EPERM);
        }

        session_writer.controlling_terminal =
            Some(ControllingTerminal::new(terminal.clone(), is_main));
        *controlling_session = ControllingSession::new(process_group);
        Ok(())
    }

    pub fn release_controlling_terminal<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        _current_task: &CurrentTask,
        terminal: &Arc<Terminal>,
        is_main: bool,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let process_group;
        {
            // Keep locks to ensure atomicity.
            let state = self.read();
            process_group = Arc::clone(&state.process_group);
            let mut terminal_state = terminal.write();
            let controlling_session = terminal_state.get_controlling_session_mut(is_main);
            let mut session_writer = process_group.session.write();

            // tty must be the controlling terminal.
            Self::check_controlling_session(&process_group.session, controlling_session)?;

            // "If the process was session leader, then send SIGHUP and SIGCONT to the foreground
            // process group and all processes in the current session lose their controlling terminal."
            // - tty_ioctl(4)

            // Remove tty as the controlling tty for each process in the session, then
            // send them SIGHUP and SIGCONT.

            session_writer.controlling_terminal = None;
            *controlling_session = None;
        }

        if process_group.session.leader == self.leader {
            process_group.send_signals(locked, &[SIGHUP, SIGCONT]);
        }

        Ok(())
    }

    fn check_orphans<L>(self: &Arc<Self>, locked: &mut Locked<'_, L>)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let mut thread_groups = self.read().children().collect::<Vec<_>>();
        thread_groups.push(Arc::clone(self));
        let process_groups =
            thread_groups.iter().map(|tg| Arc::clone(&tg.read().process_group)).unique();
        for pg in process_groups {
            pg.check_orphaned(locked);
        }
    }

    pub fn get_rlimit(self: &Arc<Self>, resource: Resource) -> u64 {
        self.limits.lock().get(resource).rlim_cur
    }

    pub fn adjust_rlimits(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        resource: Resource,
        maybe_new_limit: Option<rlimit>,
    ) -> Result<rlimit, Errno> {
        let can_increase_rlimit = current_task.creds().has_capability(CAP_SYS_RESOURCE);
        let mut limit_state = self.limits.lock();
        limit_state.get_and_set(resource, maybe_new_limit, can_increase_rlimit)
    }

    pub fn time_stats(&self) -> TaskTimeStats {
        let process: &zx::Process = if zx::AsHandleRef::as_handle_ref(&self.process).is_invalid() {
            // `process` must be valid for all tasks, except `kthreads`. In that case get the
            // stats from starnix process.
            assert_eq!(
                self as *const ThreadGroup,
                Arc::as_ptr(self.kernel.kthreads.system_thread_group()),
            );
            &self.kernel.kthreads.starnix_process
        } else {
            &self.process
        };

        let info =
            zx::Task::get_runtime_info(process).expect("Failed to get starnix process stats");
        TaskTimeStats {
            user_time: zx::Duration::from_nanos(info.cpu_time),
            // TODO(https://fxbug.dev/42078242): How can we calculate system time?
            system_time: zx::Duration::default(),
        }
    }

    /// For each task traced by this thread_group that matches the given
    /// selector, acquire its TaskMutableState and ptracees lock and execute the
    /// given function.
    pub fn get_ptracees_and(
        &self,
        selector: ProcessSelector,
        pids: &PidTable,
        f: &mut dyn FnMut(WeakRef<Task>, &TaskMutableState),
    ) {
        for tracee in self
            .ptracees
            .lock()
            .keys()
            .filter(|tracee_pid| selector.do_match(**tracee_pid, &pids))
            .map(|tracee_pid| pids.get_task(*tracee_pid).clone())
        {
            if let Some(task_ref) = tracee.clone().upgrade() {
                let task_state = task_ref.write();
                if task_state.ptrace.is_some() {
                    f(tracee, &task_state);
                }
            }
        }
    }

    /// Returns a tracee whose state has changed, so that waitpid can report on
    /// it. If this returns a value, and the pid is being traced, the tracer
    /// thread is deemed to have seen the tracee ptrace-stop for the purposes of
    /// PTRACE_LISTEN.
    pub fn get_waitable_ptracee(
        self: &Arc<Self>,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> Option<WaitResult> {
        let mut tasks = vec![];

        {
            let mut state = self.write();
            let waitable_zombie = state.get_waitable_zombie(
                &|state: &mut ThreadGroupMutableState| &mut state.zombie_ptracees,
                selector,
                options,
                pids,
            );
            if waitable_zombie.is_some() {
                return waitable_zombie;
            }
        }

        self.get_ptracees_and(selector, pids, &mut |task: WeakRef<Task>, _| {
            tasks.push(task);
        });
        for task in tasks {
            let Some(task_ref) = task.upgrade() else {
                continue;
            };

            let process_state = &mut task_ref.thread_group.write();
            let mut task_state = task_ref.write();
            if task_state
                .ptrace
                .as_ref()
                .is_some_and(|ptrace| ptrace.is_waitable(task_ref.load_stopped(), options))
            {
                // We've identified a potential target.  Need to return either
                // the process's information (if we are in group-stop) or the
                // thread's information (if we are in a different stop).

                // The shared information:
                let mut pid: i32 = 0;
                let info = process_state.tasks.values().next().unwrap().info().clone();
                let uid = info.creds().uid;
                let mut exit_status = None;
                let exit_signal = info.exit_signal();
                let time_stats =
                    process_state.base.time_stats() + process_state.children_time_stats;
                let task_stopped = task_ref.load_stopped();

                #[derive(PartialEq)]
                enum ExitType {
                    None,
                    Cont,
                    Stop,
                    Kill,
                }
                if process_state.is_waitable() {
                    let ptrace = &mut task_state.ptrace;
                    // The information for processes, if we were in group stop.
                    let process_stopped = process_state.base.load_stopped();
                    let mut fn_type = ExitType::None;
                    if process_stopped == StopState::Awake && options.wait_for_continued {
                        fn_type = ExitType::Cont;
                    }
                    let mut event = ptrace
                        .as_ref()
                        .map_or(PtraceEvent::None, |ptrace| {
                            ptrace.event_data.as_ref().map_or(PtraceEvent::None, |data| data.event)
                        })
                        .clone();
                    // Tasks that are ptrace'd always get stop notifications.
                    if process_stopped == StopState::GroupStopped
                        && (options.wait_for_stopped || ptrace.is_some())
                    {
                        fn_type = ExitType::Stop;
                    }
                    if fn_type != ExitType::None {
                        let siginfo = if options.keep_waitable_state {
                            process_state.last_signal.clone()
                        } else {
                            process_state.last_signal.take()
                        };
                        if let Some(mut siginfo) = siginfo {
                            if task_ref.thread_group.load_stopped() == StopState::GroupStopped
                                && ptrace.as_ref().is_some_and(|ptrace| ptrace.is_seized())
                            {
                                if event == PtraceEvent::None {
                                    event = PtraceEvent::Stop;
                                }
                                siginfo.code |= (PtraceEvent::Stop as i32) << 8;
                            }
                            if siginfo.signal == SIGKILL {
                                fn_type = ExitType::Kill;
                            }
                            exit_status = match fn_type {
                                ExitType::Stop => Some(ExitStatus::Stop(siginfo, event)),
                                ExitType::Cont => Some(ExitStatus::Continue(siginfo, event)),
                                ExitType::Kill => Some(ExitStatus::Kill(siginfo)),
                                _ => None,
                            };
                        }
                        // Clear the wait status of the ptrace, because we're
                        // using the tg status instead.
                        ptrace
                            .as_mut()
                            .map(|ptrace| ptrace.get_last_signal(options.keep_waitable_state));
                    }
                    pid = process_state.base.leader;
                }
                if exit_status == None {
                    if let Some(ptrace) = task_state.ptrace.as_mut() {
                        // The information for the task, if we were in a non-group stop.
                        let mut fn_type = ExitType::None;
                        let event = ptrace
                            .event_data
                            .as_ref()
                            .map_or(PtraceEvent::None, |event| event.event);
                        if task_stopped == StopState::Awake {
                            fn_type = ExitType::Cont;
                        }
                        if task_stopped.is_stopping_or_stopped()
                            || ptrace.stop_status == PtraceStatus::Listening
                        {
                            fn_type = ExitType::Stop;
                        }
                        if fn_type != ExitType::None {
                            if let Some(siginfo) =
                                ptrace.get_last_signal(options.keep_waitable_state)
                            {
                                if siginfo.signal == SIGKILL {
                                    fn_type = ExitType::Kill;
                                }
                                exit_status = match fn_type {
                                    ExitType::Stop => Some(ExitStatus::Stop(siginfo, event)),
                                    ExitType::Cont => Some(ExitStatus::Continue(siginfo, event)),
                                    ExitType::Kill => Some(ExitStatus::Kill(siginfo)),
                                    _ => None,
                                };
                            }
                        }
                        pid = task_ref.get_tid();
                    }
                }
                if let Some(exit_status) = exit_status {
                    return Some(WaitResult {
                        pid,
                        uid,
                        exit_info: ProcessExitInfo {
                            status: exit_status,
                            exit_signal: *exit_signal,
                        },
                        time_stats,
                    });
                }
            }
        }
        None
    }

    /// Get the SELinux security ID of the thread group, or `None` if not set.
    pub fn get_current_sid(&self) -> Option<SecurityId> {
        // TODO(http://b/316181721): to avoid TOCTOU issues, once initial security contexts are
        // propagated to tasks in the system, in some cases using this API will need to be replaced
        // with call sites holding the state lock while making updates.
        self.mutable_state.read().selinux_state.as_ref().map(|state| state.current_sid.clone())
    }
}

#[apply(state_implementation!)]
impl ThreadGroupMutableState<Base = ThreadGroup, BaseType = Arc<ThreadGroup>> {
    pub fn leader(&self) -> pid_t {
        self.base.leader
    }

    pub fn children(&self) -> impl Iterator<Item = Arc<ThreadGroup>> + '_ {
        self.children.values().map(|v| {
            v.upgrade().expect("Weak references to processes in ThreadGroup must always be valid")
        })
    }

    pub fn tasks(&self) -> impl Iterator<Item = TempRef<'_, Task>> + '_ {
        self.tasks.values().flat_map(|t| t.upgrade())
    }

    pub fn task_ids(&self) -> impl Iterator<Item = &pid_t> {
        self.tasks.keys()
    }

    pub fn contains_task(&self, tid: pid_t) -> bool {
        self.tasks.contains_key(&tid)
    }

    pub fn get_task(&self, tid: pid_t) -> Option<TempRef<'_, Task>> {
        self.tasks.get(&tid).and_then(|t| t.upgrade())
    }

    pub fn tasks_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn get_ppid(&self) -> pid_t {
        match &self.parent {
            Some(parent) => parent.leader,
            None => self.leader(),
        }
    }

    fn set_process_group<L>(
        &mut self,
        locked: &mut Locked<'_, L>,
        process_group: Arc<ProcessGroup>,
        pids: &mut PidTable,
    ) where
        L: LockBefore<ProcessGroupState>,
    {
        if self.process_group == process_group {
            return;
        }
        self.leave_process_group(locked, pids);
        self.process_group = process_group;
        self.process_group.insert(locked, self.base);
    }

    fn leave_process_group<L>(&mut self, locked: &mut Locked<'_, L>, pids: &mut PidTable)
    where
        L: LockBefore<ProcessGroupState>,
    {
        if self.process_group.remove(locked, self.base) {
            self.process_group.session.write().remove(self.process_group.leader);
            pids.remove_process_group(self.process_group.leader);
        }
    }

    /// Indicates whether the thread group is waitable via waitid and waitpid for
    /// either WSTOPPED or WCONTINUED.
    pub fn is_waitable(&self) -> bool {
        return self.last_signal.is_some() && !self.base.load_stopped().is_in_progress();
    }

    pub fn get_waitable_zombie(
        &mut self,
        zombie_list: &dyn Fn(&mut ThreadGroupMutableState) -> &mut Vec<OwnedRef<ZombieProcess>>,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> Option<WaitResult> {
        // The zombies whose pid matches the pid selector queried.
        let zombie_matches_pid_selector = |zombie: &OwnedRef<ZombieProcess>| match selector {
            ProcessSelector::Any => true,
            ProcessSelector::Pid(pid) => zombie.pid == pid,
            ProcessSelector::Pgid(pgid) => zombie.pgid == pgid,
        };

        // The zombies whose exit signal matches the waiting options queried.
        let zombie_matches_wait_options: Box<dyn Fn(&OwnedRef<ZombieProcess>) -> bool> = if options
            .wait_for_all
        {
            Box::new(|_zombie: &OwnedRef<ZombieProcess>| true)
        } else {
            // A "clone" zombie is one which has delivered no signal, or a
            // signal other than SIGCHLD to its parent upon termination.
            Box::new(|zombie: &OwnedRef<ZombieProcess>| {
                Self::is_correct_exit_signal(options.wait_for_clone, zombie.exit_info.exit_signal)
            })
        };

        // We look for the last zombie in the vector that matches pid selector and waiting options
        let selected_zombie_position = zombie_list(self)
            .iter()
            .rev()
            .position(|zombie: &OwnedRef<ZombieProcess>| {
                zombie_matches_wait_options(zombie) && zombie_matches_pid_selector(zombie)
            })
            .map(|position_starting_from_the_back| {
                zombie_list(self).len() - 1 - position_starting_from_the_back
            });

        selected_zombie_position.map(|position| {
            if options.keep_waitable_state {
                zombie_list(self)[position].to_wait_result()
            } else {
                let zombie = zombie_list(self).remove(position);
                self.children_time_stats += zombie.time_stats;
                let result = zombie.to_wait_result();
                zombie.release(pids);
                result
            }
        })
    }

    fn is_correct_exit_signal(for_clone: bool, exit_code: Option<Signal>) -> bool {
        for_clone == (exit_code != Some(SIGCHLD))
    }

    fn get_waitable_running_children(
        &self,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &PidTable,
    ) -> Result<Option<WaitResult>, Errno> {
        // The children whose pid matches the pid selector queried.
        let filter_children_by_pid_selector = |child: &Arc<ThreadGroup>| match selector {
            ProcessSelector::Any => true,
            ProcessSelector::Pid(pid) => child.leader == pid,
            ProcessSelector::Pgid(pgid) => {
                pids.get_process_group(pgid).as_ref() == Some(&child.read().process_group)
            }
        };

        // The children whose exit signal matches the waiting options queried.
        let filter_children_by_waiting_options = |child: &Arc<ThreadGroup>| {
            if options.wait_for_all {
                return true;
            }
            let child_state = child.read();
            if child_state.terminating {
                // Child is terminating.  In addition to its original location,
                // the leader may have exited, and its exit signal may be in the
                // leader_exit_info.
                if let Some(info) = &child_state.leader_exit_info {
                    if info.exit_signal.is_some() {
                        return Self::is_correct_exit_signal(
                            options.wait_for_clone,
                            info.exit_signal,
                        );
                    }
                }
            };

            child_state.tasks.values().any(|container| {
                let info = container.info();
                Self::is_correct_exit_signal(options.wait_for_clone, *info.exit_signal())
            })
        };

        // If wait_for_exited flag is disabled or no terminated children were found we look for living children.
        let mut selected_children = self
            .children
            .values()
            .map(|t| t.upgrade().unwrap())
            .filter(filter_children_by_pid_selector)
            .filter(filter_children_by_waiting_options)
            .peekable();
        if selected_children.peek().is_none() {
            return error!(ECHILD);
        }
        for child in selected_children {
            let child = child.write();
            if child.last_signal.is_some() {
                let build_wait_result = |mut child: ThreadGroupWriteGuard<'_>,
                                         exit_status: &dyn Fn(SignalInfo) -> ExitStatus|
                 -> WaitResult {
                    let siginfo = if options.keep_waitable_state {
                        child.last_signal.clone().unwrap()
                    } else {
                        child.last_signal.take().unwrap()
                    };
                    let exit_status = if siginfo.signal == SIGKILL {
                        // This overrides the stop/continue choice.
                        ExitStatus::Kill(siginfo)
                    } else {
                        exit_status(siginfo)
                    };
                    let info = child.tasks.values().next().unwrap().info();
                    WaitResult {
                        pid: child.base.leader,
                        uid: info.creds().uid,
                        exit_info: ProcessExitInfo {
                            status: exit_status,
                            exit_signal: *info.exit_signal(),
                        },
                        time_stats: child.base.time_stats() + child.children_time_stats,
                    }
                };
                let child_stopped = child.base.load_stopped();
                if child_stopped == StopState::Awake && options.wait_for_continued {
                    return Ok(Some(build_wait_result(child, &|siginfo| {
                        ExitStatus::Continue(siginfo, PtraceEvent::None)
                    })));
                }
                if child_stopped == StopState::GroupStopped && options.wait_for_stopped {
                    return Ok(Some(build_wait_result(child, &|siginfo| {
                        ExitStatus::Stop(siginfo, PtraceEvent::None)
                    })));
                }
            }
        }

        Ok(None)
    }

    /// Returns any waitable child matching the given `selector` and `options`. Returns None if no
    /// child matching the selector is waitable. Returns ECHILD if no child matches the selector at
    /// all.
    ///
    /// Will remove the waitable status from the child depending on `options`.
    pub fn get_waitable_child(
        &mut self,
        selector: ProcessSelector,
        options: &WaitingOptions,
        pids: &mut PidTable,
    ) -> Result<Option<WaitResult>, Errno> {
        if options.wait_for_exited {
            if let Some(waitable_zombie) = self.get_waitable_zombie(
                &|state: &mut ThreadGroupMutableState| &mut state.zombie_children,
                selector,
                options,
                pids,
            ) {
                return Ok(Some(waitable_zombie));
            }
        }

        self.get_waitable_running_children(selector, options, pids)
    }

    /// Returns a task in the current thread group.
    pub fn get_live_task(&self) -> Result<TempRef<'_, Task>, Errno> {
        self.tasks
            .get(&self.leader())
            .and_then(|t| t.upgrade())
            .or_else(|| self.tasks().next())
            .ok_or_else(|| errno!(ESRCH))
    }

    /// Return the appropriate task in |thread_group| to send the given signal.
    pub fn get_signal_target(&self, _signal: UncheckedSignal) -> Option<TempRef<'_, Task>> {
        // TODO(https://fxbug.dev/42178771): Consider more than the main thread or the first thread in the thread group
        // to dispatch the signal.
        self.get_live_task().ok()
    }

    /// Set the stop status of the process.  If you pass |siginfo| of |None|,
    /// does not update the signal.  If |finalize_only| is set, will check that
    /// the set will be a finalize (Stopping -> Stopped or Stopped -> Stopped)
    /// before executing it.
    ///
    /// Returns the latest stop state after any changes.
    pub fn set_stopped(
        mut self,
        new_stopped: StopState,
        siginfo: Option<SignalInfo>,
        finalize_only: bool,
    ) -> StopState {
        if let Some(stopped) = self.base.check_stopped_state(new_stopped, finalize_only) {
            return stopped;
        }

        // TODO(https://g-issues.fuchsia.dev/issues/306438676): When thread
        // group can be stopped inside user code, tasks/thread groups will
        // need to be either restarted or stopped here.
        self.store_stopped(new_stopped);
        if let Some(signal) = &siginfo {
            // We don't want waiters to think the process was unstopped
            // because of a sigkill.  They will get woken when the
            // process dies.
            if signal.signal != SIGKILL {
                self.last_signal = siginfo;
            }
        }
        if new_stopped == StopState::Waking || new_stopped == StopState::ForceWaking {
            self.stopped_waiters.notify_all();
        };

        let parent = (!new_stopped.is_in_progress()).then(|| self.parent.clone()).flatten();

        // Drop the lock before locking the parent.
        std::mem::drop(self);
        if let Some(parent) = parent {
            parent.write().child_status_waiters.notify_all();
        }

        new_stopped
    }

    fn store_stopped(&mut self, state: StopState) {
        // We don't actually use the guard but we require it to enforce that the
        // caller holds the thread group's mutable state lock (identified by
        // mutable access to the thread group's mutable state).

        self.base.stop_state.store(state, Ordering::Relaxed)
    }
}

/// Container around a weak task and a strong `TaskPersistentInfo`. It is needed to keep the
/// information even when the task is not upgradable, because when the task is dropped, there is a
/// moment where the task is not yet released, yet the weak pointer is not upgradeable anymore.
/// During this time, it is still necessary to access the persistent info to compute the state of
/// the thread for the different wait syscalls.
pub struct TaskContainer(WeakRef<Task>, TaskPersistentInfo);

impl From<&TempRef<'_, Task>> for TaskContainer {
    fn from(task: &TempRef<'_, Task>) -> Self {
        Self(WeakRef::from(task), task.persistent_info.clone())
    }
}

impl From<TaskContainer> for TaskPersistentInfo {
    fn from(container: TaskContainer) -> TaskPersistentInfo {
        container.1
    }
}

impl TaskContainer {
    fn upgrade(&self) -> Option<TempRef<'_, Task>> {
        self.0.upgrade()
    }

    fn info(&self) -> MutexGuard<'_, TaskPersistentInfoState> {
        self.1.lock()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use itertools::Itertools;

    #[::fuchsia::test]
    async fn test_setsid() {
        fn get_process_group(task: &Task) -> Arc<ProcessGroup> {
            Arc::clone(&task.thread_group.read().process_group)
        }
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        assert_eq!(current_task.thread_group.setsid(&mut locked), error!(EPERM));

        let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_eq!(get_process_group(&current_task), get_process_group(&child_task));

        let old_process_group = child_task.thread_group.read().process_group.clone();
        assert_eq!(child_task.thread_group.setsid(&mut locked), Ok(()));
        assert_eq!(
            child_task.thread_group.read().process_group.session.leader,
            child_task.get_pid()
        );
        assert!(!old_process_group
            .read(&mut locked)
            .thread_groups()
            .contains(&child_task.thread_group));
    }

    #[::fuchsia::test]
    async fn test_exit_status() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let child = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        child.thread_group.exit(ExitStatus::Exit(42), None);
        std::mem::drop(child);
        assert_eq!(
            current_task.thread_group.read().zombie_children[0].exit_info.status,
            ExitStatus::Exit(42)
        );
    }

    #[::fuchsia::test]
    async fn test_setgpid() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        assert_eq!(current_task.thread_group.setsid(&mut locked), error!(EPERM));

        let child_task1 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let child_task2 = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        let execd_child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        execd_child_task.thread_group.write().did_exec = true;
        let other_session_child_task =
            current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
        assert_eq!(other_session_child_task.thread_group.setsid(&mut locked), Ok(()));

        assert_eq!(child_task1.thread_group.setpgid(&mut locked, &current_task, 0), error!(ESRCH));
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &execd_child_task, 0),
            error!(EACCES)
        );
        assert_eq!(current_task.thread_group.setpgid(&mut locked, &current_task, 0), error!(EPERM));
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &other_session_child_task, 0),
            error!(EPERM)
        );
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task1, -1),
            error!(EINVAL)
        );
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task1, 255),
            error!(EPERM)
        );
        assert_eq!(
            current_task.thread_group.setpgid(
                &mut locked,
                &child_task1,
                other_session_child_task.id
            ),
            error!(EPERM)
        );

        assert_eq!(child_task1.thread_group.setpgid(&mut locked, &child_task1, 0), Ok(()));
        assert_eq!(child_task1.thread_group.read().process_group.session.leader, current_task.id);
        assert_eq!(child_task1.thread_group.read().process_group.leader, child_task1.id);

        let old_process_group = child_task2.thread_group.read().process_group.clone();
        assert_eq!(
            current_task.thread_group.setpgid(&mut locked, &child_task2, child_task1.id),
            Ok(())
        );
        assert_eq!(child_task2.thread_group.read().process_group.leader, child_task1.id);
        assert!(!old_process_group
            .read(&mut locked)
            .thread_groups()
            .contains(&child_task2.thread_group));
    }

    #[::fuchsia::test]
    async fn test_adopt_children() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let task1 = current_task.clone_task_for_test(&mut locked, 0, None);
        let task2 = task1.clone_task_for_test(&mut locked, 0, None);
        let task3 = task2.clone_task_for_test(&mut locked, 0, None);

        assert_eq!(task3.thread_group.read().get_ppid(), task2.id);

        task2.thread_group.exit(ExitStatus::Exit(0), None);
        std::mem::drop(task2);

        // Task3 parent should be current_task.
        assert_eq!(task3.thread_group.read().get_ppid(), current_task.id);
    }
}
