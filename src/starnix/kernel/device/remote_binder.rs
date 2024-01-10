// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{DeviceOps, RemoteBinderConnection},
    mm::{DesiredAddress, MappingOptions, MemoryAccessorExt, ProtectionFlags},
    task::{CurrentTask, Kernel, ThreadGroup, WaitQueue, Waiter},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, FdEvents, FileObject, FileOps, FsNode, FsString, NamespaceNode,
    },
};
use anyhow::{Context, Error};
use derivative::Derivative;
use fidl::{
    endpoints::{ClientEnd, ControlHandle, RequestStream, ServerEnd},
    AsHandleRef,
};
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_starnix_binder as fbinder;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot,
    future::{FutureExt, TryFutureExt},
    pin_mut, select,
    task::Poll,
    Future, Stream, StreamExt, TryStreamExt,
};
use starnix_lifecycle::DropWaiter;
use starnix_logging::{
    log_error, log_warn, trace_category_starnix, trace_duration, trace_flow_begin, trace_flow_end,
    trace_flow_step,
};
use starnix_sync::{Mutex, MutexGuard};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    device_type::DeviceType,
    errno, errno_from_code, error,
    errors::{Errno, ErrnoCode, EAGAIN, EINTR},
    open_flags::OpenFlags,
    pid_t, uapi,
    user_address::{UserAddress, UserCString, UserRef},
    PATH_MAX,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{Arc, Weak},
};

// The name used to track the duration of a remote binder ioctl.
fuchsia_trace::string_name_macro!(trace_name_remote_binder_ioctl, "remote_binder_ioctl");
fuchsia_trace::string_name_macro!(
    trace_name_remote_binder_ioctl_send_work,
    "remote_binder_ioctl_send_work"
);
fuchsia_trace::string_name_macro!(
    trace_name_remote_binder_ioctl_fidl_reply,
    "remote_binder_ioctl_fidl_reply"
);
fuchsia_trace::string_name_macro!(
    trace_name_remote_binder_ioctl_worker_process,
    "remote_binder_ioctl_worker_process"
);

trait RemoteControllerConnector: Send + Sync + 'static {
    fn connect_to_remote_controller(
        current_task: &CurrentTask,
        service_name: &str,
    ) -> Result<ClientEnd<fbinder::RemoteControllerMarker>, Errno>;
}

struct DefaultRemoteControllerConnector;

impl RemoteControllerConnector for DefaultRemoteControllerConnector {
    fn connect_to_remote_controller(
        current_task: &CurrentTask,
        service_name: &str,
    ) -> Result<ClientEnd<fbinder::RemoteControllerMarker>, Errno> {
        current_task
            .kernel()
            .connect_to_named_protocol_at_container_svc::<fbinder::RemoteControllerMarker>(
                service_name,
            )
    }
}

/// Device for starting a remote fuchsia component with access to the binder drivers on the starnix
/// container.
#[derive(Clone)]
pub struct RemoteBinderDevice {}

impl DeviceOps for RemoteBinderDevice {
    fn open(
        &self,
        current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(RemoteBinderFileOps::new(&current_task.thread_group))
    }
}

struct RemoteBinderFileOps(Arc<RemoteBinderHandle<DefaultRemoteControllerConnector>>);

impl RemoteBinderFileOps {
    fn new(thread_group: &Arc<ThreadGroup>) -> Box<Self> {
        Box::new(Self(RemoteBinderHandle::<DefaultRemoteControllerConnector>::new(thread_group)))
    }
}

impl FileOps for RemoteBinderFileOps {
    fileops_impl_nonseekable!();

    fn query_events(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::empty())
    }

    fn close(&self, _file: &FileObject, _current_task: &CurrentTask) {
        self.0.close();
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.0.ioctl(current_task, request, arg)
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(EOPNOTSUPP)
    }

    fn mmap(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _addr: DesiredAddress,
        _vmo_offset: u64,
        _length: usize,
        _prot_flags: ProtectionFlags,
        _mapping_options: MappingOptions,
        _filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        error!(EOPNOTSUPP)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }
}

/// The type of the responder function used in `TaskRequest` to send the result of a FIDL request
/// directly from the handler thread.
type SynchronousResponder = Box<dyn FnOnce(Result<(), Errno>) -> Result<(), fidl::Error> + Send>;

/// Request sent from the FIDL server thread to the running tasks. The requests that require a
/// response send a `Sender` to let the task return the response.
#[derive(Derivative)]
#[derivative(Debug)]
enum TaskRequest {
    /// Set the associated vmo for the binder connection. See the SetVmo method in the Binder FIDL
    /// protocol.
    SetVmo {
        #[derivative(Debug = "ignore")]
        remote_binder_connection: Arc<RemoteBinderConnection>,
        vmo: fidl::Vmo,
        mapped_address: u64,
        // a synchronous function avoids thread hops.
        #[derivative(Debug = "ignore")]
        responder: SynchronousResponder,
    },
    /// Execute the given ioctl. See the Ioctl method in the Binder FIDL
    /// protocol.
    Ioctl {
        // remote_binder_connection,
        #[derivative(Debug = "ignore")]
        remote_binder_connection: Arc<RemoteBinderConnection>,
        request: u32,
        parameter: u64,
        koid: u64,
        // a synchronous function avoids thread hops.
        #[derivative(Debug = "ignore")]
        responder: SynchronousResponder,
    },
    /// Open the binder device driver situated at `path` in the Task filesystem namespace.
    Open {
        path: FsString,
        process_accessor: ClientEnd<fbinder::ProcessAccessorMarker>,
        process: zx::Process,
        responder: oneshot::Sender<Result<Arc<RemoteBinderConnection>, Errno>>,
    },
    /// Have the task returns to userspace. `spawn_thread` must be returned to the caller through
    /// the ioctl command parameter.
    Return { spawn_thread: bool },
}

impl TaskRequest {
    fn remote_binder_connection(&self) -> Option<Arc<RemoteBinderConnection>> {
        match self {
            Self::SetVmo { remote_binder_connection, .. }
            | Self::Ioctl { remote_binder_connection, .. } => {
                Some(remote_binder_connection.clone())
            }
            Self::Open { .. } | Self::Return { .. } => None,
        }
    }
}

/// A `TaskRequest` that is associated with a given thread koid. Each thread koid must be
/// associated 1 to 1 with a Starnix task and only that task must handle the request.
#[derive(Debug)]
struct BoundTaskRequest {
    koid: u64,
    request: TaskRequest,
}

impl std::ops::Deref for BoundTaskRequest {
    type Target = TaskRequest;
    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

/// Returns the Errno in result if it is either EINTR or EAGAIN, None otherwise.
fn must_interrupt<R>(result: &Result<R, Errno>) -> Option<Errno> {
    match result {
        Err(e) if *e == EINTR => Some(errno!(EINTR)),
        Err(e) if *e == EAGAIN => Some(errno!(EAGAIN)),
        _ => None,
    }
}

/// Connection to the remote binder device. One connection is associated to one instance of a
/// remote fuchsia component.
struct RemoteBinderHandle<F: RemoteControllerConnector> {
    state: Mutex<RemoteBinderHandleState>,

    /// Marker struct, needed because the struct is parametrized by `F`.
    _phantom: std::marker::PhantomData<F>,
}

/// The state of the current request for a given task.
#[derive(Debug)]
enum PendingRequest {
    /// No request pending, the task is ready to accept a new one.
    None,
    /// A request is currently running. The task should not receive a new request.
    Running,
    /// The request the task should run. The task should not receive a new request.
    Some(BoundTaskRequest),
}

impl PendingRequest {
    /// Take the current pending request, if there is one. In this case, the state will move to
    /// `Running`.
    fn take(&mut self) -> Option<BoundTaskRequest> {
        match self {
            Self::Some(_) => {
                let value = std::mem::replace(self, PendingRequest::Running);
                if let Self::Some(v) = value {
                    Some(v)
                } else {
                    panic!();
                }
            }
            _ => None,
        }
    }

    /// Whether a request is currently waiting or running.
    fn is_pending(this: Option<Self>) -> bool {
        matches!(this, Some(Self::Running | Self::Some(_)))
    }
}

/// Internal state of RemoteBinderHandle.
///
/// This struct keep the state of the local starnix tasks and the remote process and its threads.
/// Each remote thread must be associated to a single starnix task so that all ioctl from the
/// remote thread is executed by the same starnix task. When a starnix task execute the wait ioctl,
/// it checks whether is is already associated with a remote thread. If that's the case, it will
/// poll for request that can be executed by any task, or request directed to itself. If not, it
/// will adds itself in the `unassigned_tasks` set and will poll for request that can either be
/// executed by any task, or requested directed to any unassigned task. Once it received a request
/// of an unassigned task, it will associate itself with the remote thread and from then on, only
/// accept request for that thread, or for any task.
#[derive(Derivative)]
#[derivative(Debug)]
struct RemoteBinderHandleState {
    /// The thread_group of the tasks that interact with this remote binder. This is used to
    /// interrupt a random thread in the task group is a taskless request needs to be handled.
    thread_group: Weak<ThreadGroup>,

    /// Mapping from the koid of the remote process to the local task.
    koid_to_task: HashMap<u64, pid_t>,

    /// Set of tasks that contacted the remote binder device driver but are not yet associated to a
    /// remote process. Once associated, a task will have an entry in `pending_requests`.
    unassigned_tasks: HashSet<pid_t>,

    /// Pending request for each associated task. Once as task is registered and associated with a
    /// remote process, it will have an entry in this map. If the entry is None, it has no work to
    /// do, otherwise, it must executed the given request.
    pending_requests: HashMap<pid_t, PendingRequest>,

    /// Queue of request that must be executed and for which no assigned task exists. The next time
    /// a unassigned task requires a new request, the first request will be retrieved and the task
    /// will be associated with the koid of the request.
    unassigned_requests: VecDeque<BoundTaskRequest>,

    /// Queue of request that can be executed by any task.
    taskless_requests: VecDeque<TaskRequest>,

    /// If present, any ioctl should immediately return the given value. Used to end the userspace
    /// process.
    exit: Option<Result<(), ErrnoCode>>,

    /// Channels that must receive a element at the time the handle exits.
    exit_notifiers: Vec<oneshot::Sender<()>>,

    /// Notification queue to wake tasks waiting for requests.
    #[derivative(Debug = "ignore")]
    waiters: WaitQueue,
}

impl<F: RemoteControllerConnector> RemoteBinderHandle<F> {
    fn lock(&self) -> MutexGuard<'_, RemoteBinderHandleState> {
        self.state.lock()
    }
}

impl RemoteBinderHandleState {
    /// Signal all task that they must exit.
    fn exit(&mut self, result: Result<(), Errno>) {
        // The task requests in state may refer to async FIDL streams and must be dropped before
        // dropping the executor.
        self.koid_to_task.clear();
        self.unassigned_tasks.clear();
        self.pending_requests.clear();
        self.unassigned_requests.clear();
        self.taskless_requests.clear();

        self.exit = Some(result.map_err(|e| e.code));
        self.waiters.notify_all();
        for notifier in std::mem::take(&mut self.exit_notifiers) {
            let _ = notifier.send(());
        }
    }

    /// Enqueue a request for the task associated with `koid`.
    fn enqueue_task_request(&mut self, request: BoundTaskRequest) {
        debug_assert!(self.unassigned_requests.iter().all(|r| r.koid != request.koid));
        if let Some(tid) = self.koid_to_task.get(&request.koid).copied() {
            // Find the task associated with the given koid. If one exist, we enqueue the request
            // for task. The task should never already have a task enqueued, as otherwise, it
            // should be blocked on a syscall, and should not be able to send another one.
            if PendingRequest::is_pending(
                self.pending_requests.insert(tid, PendingRequest::Some(request)),
            ) {
                log_error!("A single thread received 2 concurrent requests.");
                self.exit(error!(EINVAL));
                return;
            }
            self.waiters.notify_value(tid as u64);
        } else if let Some(tid) = self.unassigned_tasks.iter().next().copied() {
            // There was no task associated with the koid, but there exists an unassigned task.
            // Associated the task with the koid, and insert the pending request.
            self.unassigned_tasks.remove(&tid);
            self.koid_to_task.insert(request.koid, tid);
            if PendingRequest::is_pending(
                self.pending_requests.insert(tid, PendingRequest::Some(request)),
            ) {
                log_error!("A single thread received 2 concurrent requests.");
                self.exit(error!(EINVAL));
                return;
            }
            self.waiters.notify_value(tid as u64);
        } else {
            // Get the eventual RemoteBinderConnection.
            let remote_binder_connection = request.remote_binder_connection();
            // And add the request to the unassigned queue.
            self.unassigned_requests.push_back(request);
            // Not unassigned task ready. Request userspace to spawn a new one.
            self.enqueue_taskless_request(
                remote_binder_connection.as_deref(),
                TaskRequest::Return { spawn_thread: true },
            );
        }
    }

    /// Enqueue a request that can be run by any task.
    fn enqueue_taskless_request(
        &mut self,
        remote_binder_connection: Option<&RemoteBinderConnection>,
        request: TaskRequest,
    ) {
        self.taskless_requests.push_back(request);
        if let Some(remote_binder_connection) = remote_binder_connection {
            remote_binder_connection.interrupt();
        }
        // Interrupt a single task to handle the request.
        self.waiters.notify_unordered_count(1);
    }

    /// Called when a task starts waiting.
    fn register_waiting_task(&mut self, tid: pid_t) {
        if self.pending_requests.contains_key(&tid) || self.unassigned_tasks.contains(&tid) {
            // The task is already registered.
            return;
        }
        // This is the first time the task is seen.
        if let Some(request) = self.unassigned_requests.pop_front() {
            // There is an unassigned request. Associate it to the task.
            self.koid_to_task.insert(request.koid, tid);
            self.pending_requests.insert(tid, PendingRequest::Some(request));
        } else {
            // Otherwise, mark the task as unassigned and available.
            self.unassigned_tasks.insert(tid);
        }
    }
}

impl<F: RemoteControllerConnector> RemoteBinderHandle<F> {
    fn new(thread_group: &Arc<ThreadGroup>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RemoteBinderHandleState {
                thread_group: Arc::downgrade(thread_group),
                koid_to_task: Default::default(),
                unassigned_tasks: Default::default(),
                unassigned_requests: Default::default(),
                pending_requests: Default::default(),
                taskless_requests: Default::default(),
                exit: Default::default(),
                exit_notifiers: Default::default(),
                waiters: Default::default(),
            }),
            _phantom: Default::default(),
        })
    }

    fn close(self: &Arc<Self>) {
        self.lock().exit(Ok(()));
    }

    fn ioctl(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            uapi::REMOTE_BINDER_START => self.start(current_task, user_addr.into())?,
            uapi::REMOTE_BINDER_WAIT => self.wait(current_task, user_addr.into())?,
            _ => return error!(ENOTSUP),
        }
        Ok(SUCCESS)
    }

    /// Make a callback that can delegate to a FIDL async channel from a non-executor thread
    /// without crashing if the executor is dropped before the callback.
    ///
    /// For this, this builds a pair of a `SynchronousResponder` and a future such that:
    /// - The future resolve once the `SynchronousResponder` is called on another thread.
    /// - The responder is passed to the `SynchronousResponder` but is guaranteed to always be
    ///   dropped from the executor the future is bound to.
    /// - The responder is not called if the future is dropped.
    ///
    /// To use this, one should use the returned responder when they want `f` to be run, and ensure
    /// that the returned future is either waited, or dropped on the executor thread.
    fn make_synchronous_responder<R: Send + 'static, C>(
        responder: R,
        f: C,
    ) -> (SynchronousResponder, impl Future<Output = ()>)
    where
        C: FnOnce(R, Result<(), Errno>) -> Result<(), fidl::Error> + Send + 'static,
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        let responder = Arc::new(Mutex::new(Some(responder)));
        let closure = Box::new({
            let responder = Arc::downgrade(&responder);
            move |e| {
                scopeguard::defer! {
                    let _ = tx.send(());
                }
                if let Some(responder) = responder.upgrade() {
                    let mut guard = responder.lock();
                    // Keep the guard lock when the responder is on the stack to ensure that the
                    // executor is not dropped while the responder is still alive.
                    if let Some(responder) = guard.take() {
                        return f(responder, e);
                    }
                }
                Ok(())
            }
        });
        let waiter = async move {
            // Drop the responder in a scopeguard to ensure the responder is dropped even if the
            // future is cancelled.
            scopeguard::defer! {
                std::mem::drop(responder.lock().take());
            }
            let _ = rx.await;
        };
        (closure, waiter)
    }

    async fn serve_binder_request(
        &self,
        remote_binder_connection: Arc<RemoteBinderConnection>,
        request: fbinder::BinderRequest,
    ) -> Result<(), Error> {
        match request {
            fbinder::BinderRequest::SetVmo { vmo, mapped_address, control_handle } => {
                let (responder, waiter) =
                    Self::make_synchronous_responder(control_handle, |control_handle, e| {
                        if e.is_err() {
                            control_handle.shutdown();
                        }
                        Ok(())
                    });
                self.lock().enqueue_taskless_request(
                    Some(&remote_binder_connection),
                    TaskRequest::SetVmo {
                        remote_binder_connection: remote_binder_connection.clone(),
                        vmo,
                        mapped_address,
                        responder,
                    },
                );
                waiter.await;
            }
            fbinder::BinderRequest::Ioctl { tid, request, parameter, responder } => {
                trace_duration!(trace_category_starnix!(), trace_name_remote_binder_ioctl_send_work!(), "request" => request);
                trace_flow_begin!(trace_category_starnix!(), trace_name_remote_binder_ioctl!(), tid.into(), "request" => request);

                let (responder, waiter) =
                    Self::make_synchronous_responder(responder, move |responder, e| {
                        trace_duration!(
                            trace_category_starnix!(),
                            trace_name_remote_binder_ioctl_fidl_reply!()
                        );
                        trace_flow_end!(
                            trace_category_starnix!(),
                            trace_name_remote_binder_ioctl!(),
                            tid.into()
                        );

                        let e = e.map_err(|e| {
                            fposix::Errno::from_primitive(e.code.error_code() as i32)
                                .unwrap_or(fposix::Errno::Einval)
                        });

                        responder.send(e)
                    });
                self.lock().enqueue_task_request(BoundTaskRequest {
                    koid: tid,
                    request: TaskRequest::Ioctl {
                        remote_binder_connection: remote_binder_connection.clone(),
                        request,
                        parameter,
                        koid: tid,
                        responder,
                    },
                });
                waiter.await;
            }
            fbinder::BinderRequest::_UnknownMethod { ordinal, .. } => {
                log_warn!("Unknown Binder ordinal: {}", ordinal);
            }
        }
        Ok(())
    }

    /// Serve the LutexController protocol.
    async fn serve_lutex_controller(
        kernel: Arc<Kernel>,
        server_end: ServerEnd<fbinder::LutexControllerMarker>,
    ) -> Result<(), Error> {
        async fn handle_request(
            kernel: &Arc<Kernel>,
            event: fbinder::LutexControllerRequest,
        ) -> Result<(), Error> {
            match event {
                fbinder::LutexControllerRequest::WaitBitset { payload, responder } => {
                    let deadline_and_receiver = (|| {
                        let vmo = payload.vmo.ok_or_else(|| errno!(EINVAL))?;
                        let offset = payload.offset.ok_or_else(|| errno!(EINVAL))?;
                        let value = payload.value.ok_or_else(|| errno!(EINVAL))?;
                        let mask = payload.mask.unwrap_or(u32::MAX);
                        let deadline = payload.deadline.map(zx::Time::from_nanos);
                        kernel
                            .shared_futexes
                            .external_wait(vmo, offset, value, mask)
                            .map(|receiver| (deadline, receiver))
                    })();
                    let result = match deadline_and_receiver {
                        Ok((deadline, receiver)) => {
                            let receiver = receiver.map_err(|_| errno!(EINTR));
                            if let Some(deadline) = deadline {
                                let timer = fasync::Timer::new(deadline).map(|_| error!(ETIMEDOUT));
                                select_first(timer, receiver).await
                            } else {
                                receiver.await
                            }
                        }
                        Err(e) => Err(e),
                    };
                    let result = result.map_err(|e: Errno| {
                        fposix::Errno::from_primitive(e.code.error_code() as i32)
                            .unwrap_or(fposix::Errno::Einval)
                    });
                    responder
                        .send(result)
                        .context("Unable to send LutexControllerRequest::WaitBitset response")
                }
                fbinder::LutexControllerRequest::WakeBitset { payload, responder } => {
                    let result = (|| {
                        let vmo = payload.vmo.ok_or_else(|| errno!(EINVAL))?;
                        let offset = payload.offset.ok_or_else(|| errno!(EINVAL))?;
                        let count = payload.count.ok_or_else(|| errno!(EINVAL))?;
                        let mask = payload.mask.unwrap_or(u32::MAX);
                        kernel.shared_futexes.external_wake(vmo, offset, count as usize, mask)
                    })();
                    let result = result
                        .map(|count| fbinder::WakeResponse {
                            count: Some(count as u64),
                            ..fbinder::WakeResponse::default()
                        })
                        .map_err(|e: Errno| {
                            fposix::Errno::from_primitive(e.code.error_code() as i32)
                                .unwrap_or(fposix::Errno::Einval)
                        });
                    responder
                        .send(result)
                        .context("Unable to send LutexControllerRequest::WakeBitset response")
                }
                fbinder::LutexControllerRequest::_UnknownMethod { ordinal, .. } => {
                    log_warn!("Unknown LutexController ordinal: {}", ordinal);
                    Ok(())
                }
            }
        }
        let stream = fbinder::LutexControllerRequestStream::from_channel(
            fasync::Channel::from_channel(server_end.into_channel()),
        );
        stream
            .map(|result| result.context("failed fbinder::LutexController request"))
            .try_for_each_concurrent(None, |event| handle_request(&kernel, event))
            .await
    }

    /// Serve the given `binder` handle, by opening `path`.
    async fn open_binder(
        self: Arc<Self>,
        path: FsString,
        process_accessor: ClientEnd<fbinder::ProcessAccessorMarker>,
        process: zx::Process,
        binder: ServerEnd<fbinder::BinderMarker>,
    ) -> Result<(), Error> {
        // Open the device.
        let (sender, receiver) = oneshot::channel::<Result<Arc<RemoteBinderConnection>, Errno>>();
        self.lock().enqueue_taskless_request(
            None,
            TaskRequest::Open { path, process_accessor, process, responder: sender },
        );
        let remote_binder_connection = receiver.await??;

        scopeguard::defer! {
            // When leaving the current scope, close the connection, even if some operation are in
            // progress. This should kick the tasks back with an error.
            remote_binder_connection.close();
        }

        // Register a receiver to be notified of exit
        let (sender, receiver) = oneshot::channel::<()>();
        {
            let mut state = self.lock();
            if state.exit.is_some() {
                return Ok(());
            }
            state.exit_notifiers.push(sender);
        }

        // The stream for the Binder protocol
        let stream = fbinder::BinderRequestStream::from_channel(fasync::Channel::from_channel(
            binder.into_channel(),
        ));

        pin_mut!(receiver, stream);
        // The stream that will cancel once receiver returns a value.
        let stream = futures::stream::poll_fn(move |context| {
            if receiver.as_mut().poll(context).is_ready() {
                return Poll::Ready(None);
            }
            stream.as_mut().poll_next(context)
        });

        stream
            .map(|result| result.context("failed request"))
            .try_for_each_concurrent(usize::MAX, |event| {
                self.serve_binder_request(remote_binder_connection.clone(), event)
            })
            .await
    }

    /// Serve the DevBinder protocol.
    async fn serve_dev_binder(
        self: Arc<Self>,
        server_end: ServerEnd<fbinder::DevBinderMarker>,
    ) -> Result<(), Error> {
        let mut stream = fbinder::DevBinderRequestStream::from_channel(
            fasync::Channel::from_channel(server_end.into_channel()),
        );
        // Keep track of the current task serving the different Binder protocol. When a given
        // Binder is closed, this task will actually wait for the associated Binder task to finish,
        // to ensure that the same device is not opened multiple times because of concurrency.
        let binder_tasks = Rc::new(Mutex::new(HashMap::<zx::Koid, fasync::Task<()>>::new()));
        while let Some(event) = stream.try_next().await? {
            // The tasks must be freed when this method returns, binder_tasks should always have a
            // single owner, and the RC is only used temporarily to let tasks clean themselves.
            debug_assert_eq!(Rc::strong_count(&binder_tasks), 1);
            match event {
                fbinder::DevBinderRequest::Open { payload, control_handle } => {
                    // Extract the path, process_accessor and binder_server from the `payload`, and
                    // start serving the binder protocol.
                    // Returns the task serving the binder protocol, as well as the koid to the
                    // client handle for the binder protocol.
                    //
                    // This is wrapped in a closure so that any error can be evaluated.
                    let result: Result<_, Error> = (|| {
                        let path = payload.path.ok_or_else(|| errno!(EINVAL))?;
                        let process_accessor =
                            payload.process_accessor.ok_or_else(|| errno!(EINVAL))?;
                        let process = payload.process.ok_or_else(|| errno!(EINVAL))?;
                        let binder = payload.binder.ok_or_else(|| errno!(EINVAL))?;
                        let koid = binder.as_handle_ref().basic_info()?.related_koid;
                        let handle = self.clone();
                        Ok((
                            fasync::Task::local(handle.open_binder(
                                path.into(),
                                process_accessor,
                                process,
                                binder,
                            )),
                            koid,
                        ))
                    })();
                    match result {
                        // The request was valid. task is the local task currently serving the
                        // binder protocol, koid is the koid of the client handle for the binder
                        // protocol.
                        Ok((task, koid)) => {
                            // Wrap the task into a new local task that on exit will:
                            // 1. Unregister the task from `binder_tasks`
                            // 2. If the tasks ends up in error, disconnecting the binder protocol.
                            let mut task = fasync::Task::local({
                                // Keep a weak references to the tasks to unregister. Do not keep a
                                // strong reference as otherwise it creates a reference count loop.
                                let binder_tasks = Rc::downgrade(&binder_tasks);
                                async move {
                                    let result = task.await;
                                    if let Some(binder_tasks) = binder_tasks.upgrade() {
                                        binder_tasks.lock().remove(&koid);
                                    }
                                    if let Err(err) = result {
                                        log_warn!("DevBinder::Open failed: {err:?}");
                                        control_handle.shutdown();
                                    }
                                }
                            });
                            // If the task is not pending, it must not be registered into
                            // `binder_tasks`, as it will never be removed.
                            if futures::poll!(&mut task).is_pending() {
                                // Register the task associated with the koid of the remote handle.
                                binder_tasks.lock().insert(koid, task);
                            }
                        }
                        Err(err) => {
                            log_warn!("DevBinder::Open failed: {err:?}");
                            control_handle.shutdown();
                        }
                    }
                }
                fbinder::DevBinderRequest::Close { payload, control_handle } => {
                    // Retrieve the task using the koid of the remote handle. If the task is still
                    // registered, wait for it to terminate. This will happen promptly, because the
                    // remote handle is closed by this closure.
                    let result: Result<_, Error> = (|| {
                        let binder = payload.binder.ok_or_else(|| errno!(EINVAL))?;
                        let koid = binder.get_koid()?;
                        Ok(binder_tasks.lock().remove(&koid))
                    })();
                    match result {
                        Err(err) => {
                            log_warn!("DevBinder::Close failed: {err:?}");
                            control_handle.shutdown();
                        }
                        Ok(Some(task)) => {
                            task.await;
                        }
                        Ok(None) => {}
                    }
                }
                fbinder::DevBinderRequest::_UnknownMethod { ordinal, .. } => {
                    log_warn!("Unknown DevBinder ordinal: {}", ordinal);
                }
            }
        }
        Ok(())
    }

    /// Returns the next TaskRequest that `current_task` must handle, waiting if none is available.
    fn get_next_task(&self, current_task: &CurrentTask) -> Result<TaskRequest, Errno> {
        loop {
            let mut state = self.lock();
            // Exit immediately if requested.
            if let Some(result) = state.exit.as_ref() {
                return result
                    .map_err(|c| errno_from_code!(c.error_code() as i16))
                    .map(|_| TaskRequest::Return { spawn_thread: false });
            }
            // Taskless request have the highest priority.
            if let Some(request) = state.taskless_requests.pop_front() {
                return Ok(request);
            }
            let tid = current_task.get_tid();
            if let Some(request) = state.pending_requests.get_mut(&tid) {
                // This task is already associated with a remote koid. Check if some request is
                // available for this task.
                if let Some(request) = request.take() {
                    return Ok(request.request);
                }
            } else if let Some(request) = state.unassigned_requests.pop_front() {
                // The task is not associated with any remote koid, and there is an unassigned
                // request. Associate this task with the koid of the request, and return the
                // request.
                state.unassigned_tasks.remove(&tid);
                state.koid_to_task.insert(request.koid, tid);
                state.pending_requests.insert(tid, PendingRequest::Running);
                return Ok(request.request);
            }
            // Wait until some request is available.
            let waiter = Waiter::new();
            state.waiters.wait_async_value(&waiter, tid as u64);
            std::mem::drop(state);
            waiter.wait(current_task)?;
        }
    }

    /// Open a remote connection with the binder device at `path`.
    fn open(
        &self,
        current_task: &CurrentTask,
        path: FsString,
        process_accessor: ClientEnd<fbinder::ProcessAccessorMarker>,
        process: zx::Process,
    ) -> Result<Arc<RemoteBinderConnection>, Errno> {
        let node = current_task.lookup_path_from_root(path.as_ref())?;
        let device_type = node.entry.node.info().rdev;
        let connection = current_task
            .kernel()
            .binders
            .read()
            .get(&device_type)
            .ok_or_else(|| errno!(ENOTSUP))?
            .open_remote(current_task, process_accessor, process);
        Ok(connection)
    }

    /// Implementation of the REMOTE_BINDER_START ioctl.
    fn start(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        service_address_ref: UserRef<UserCString>,
    ) -> Result<(), Errno> {
        let service_address = current_task.read_object(service_address_ref)?;
        let service = current_task.read_c_string_to_vec(service_address, PATH_MAX as usize)?;
        let service_name = String::from_utf8(service.to_vec()).map_err(|_| errno!(EINVAL))?;
        let remote_controller_client =
            F::connect_to_remote_controller(current_task, &service_name)?;
        let remote_controller =
            fbinder::RemoteControllerSynchronousProxy::new(remote_controller_client.into_channel());
        let (dev_binder_server_end, dev_binder_client_end) = zx::Channel::create();
        let (lutex_controller_server_end, lutex_controller_client_end) = zx::Channel::create();
        remote_controller
            .start(fbinder::RemoteControllerStartRequest {
                dev_binder: Some(dev_binder_client_end.into()),
                lutex_controller: Some(lutex_controller_client_end.into()),
                ..Default::default()
            })
            .map_err(|_| errno!(EINVAL))?;
        let handle = self.clone();
        current_task.kernel().kthreads.spawner().spawn(move |_, _| {
            let mut executor = fasync::LocalExecutor::new();
            let result = executor.run_singlethreaded({
                let handle = handle.clone();
                async {
                    // Retrieve the Kernel and a `DropWaiter` for the thread_group, taking care not
                    // to keep a strong reference to the thread_group itself.
                    let kernel_and_drop_waiter = handle
                        .state
                        .lock()
                        .thread_group
                        .upgrade()
                        .map(|tg| (tg.kernel.clone(), tg.drop_notifier.waiter()));
                    let Some((kernel, drop_waiter)) = kernel_and_drop_waiter else {
                        return Ok(());
                    };
                    // Start the 2 servers.
                    let dev_binder_server =
                        fasync::Task::local(handle.serve_dev_binder(dev_binder_server_end.into()));
                    let lutex_controller_server = fasync::Task::local(
                        Self::serve_lutex_controller(kernel, lutex_controller_server_end.into()),
                    );
                    // Wait until both are done, or the task exits.
                    let binder_result = future_or_task_end(&drop_waiter, dev_binder_server).await;
                    let lutex_controller_result =
                        future_or_task_end(&drop_waiter, lutex_controller_server).await;
                    binder_result.and(lutex_controller_result)
                }
            });
            if let Err(e) = &result {
                log_error!("Error when servicing the DevBinder protocol: {e:#}");
            }
            handle.lock().exit(result.map_err(|_| errno!(ENOENT)));
        });

        error!(EAGAIN)
    }

    /// Implementation of the REMOTE_BINDER_WAIT ioctl.
    fn wait(
        &self,
        current_task: &CurrentTask,
        wait_command_ref: UserRef<uapi::remote_binder_wait_command>,
    ) -> Result<(), Errno> {
        self.lock().register_waiting_task(current_task.get_tid());
        loop {
            let interruption = match self.get_next_task(current_task)? {
                TaskRequest::Open { path, process_accessor, process, responder } => {
                    let result = self.open(current_task, path, process_accessor, process);
                    let interruption = must_interrupt(&result);
                    responder.send(result).map_err(|_| errno!(EINVAL))?;
                    interruption
                }
                TaskRequest::SetVmo {
                    remote_binder_connection,
                    vmo,
                    mapped_address,
                    responder,
                } => {
                    let result = remote_binder_connection.map_external_vmo(
                        current_task,
                        vmo,
                        mapped_address,
                    );
                    let interruption = must_interrupt(&result);
                    responder(result).map_err(|_| errno!(EINVAL))?;
                    interruption
                }
                TaskRequest::Ioctl {
                    remote_binder_connection,
                    request,
                    parameter,
                    koid,
                    responder,
                } => {
                    trace_duration!(
                        trace_category_starnix!(),
                        trace_name_remote_binder_ioctl_worker_process!()
                    );
                    trace_flow_step!(
                        trace_category_starnix!(),
                        trace_name_remote_binder_ioctl!(),
                        koid.into()
                    );
                    let result =
                        remote_binder_connection.ioctl(current_task, request, parameter.into());
                    // Once the potentially blocking calls is made, the task is ready to handle the
                    // next request.
                    self.lock()
                        .pending_requests
                        .insert(current_task.get_tid(), PendingRequest::None);
                    let interruption = must_interrupt(&result);
                    responder(result).map_err(|_| errno!(EINVAL))?;
                    interruption
                }
                TaskRequest::Return { spawn_thread } => {
                    let wait_command = uapi::remote_binder_wait_command {
                        spawn_thread: if spawn_thread { 1 } else { 0 },
                    };
                    current_task.write_object(wait_command_ref, &wait_command)?;
                    return Ok(());
                }
            };
            if let Some(errno) = interruption {
                return Err(errno);
            }
        }
    }
}

async fn future_or_task_end(
    drop_waiter: &DropWaiter,
    fut: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    let on_task_end = drop_waiter.on_closed().map(|r| r.map(|_| ()).map_err(anyhow::Error::from));
    select_first(fut, on_task_end).await
}

async fn select_first<O>(f1: impl Future<Output = O>, f2: impl Future<Output = O>) -> O {
    let f1 = f1.fuse();
    let f2 = f2.fuse();
    pin_mut!(f1, f2);
    select! {
        f1 = f1 => f1,
        f2 = f2 => f2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        device::{binder::tests::run_process_accessor, BinderFs},
        mm::MemoryAccessor,
        testing::*,
        vfs::{FileSystemOptions, WhatToMount},
    };
    use fidl::{
        endpoints::{create_endpoints, create_proxy, Proxy},
        HandleBased,
    };
    use once_cell::sync::Lazy;
    use rand::distributions::{Alphanumeric, DistString};
    use starnix_uapi::{file_mode::mode, mount_flags::MountFlags};
    use std::{collections::BTreeMap, ffi::CString, future::Future};

    static REMOTE_CONTROLLER_CLIENT: Lazy<
        Mutex<BTreeMap<String, ClientEnd<fbinder::RemoteControllerMarker>>>,
    > = Lazy::new(|| {
        Mutex::<BTreeMap<String, ClientEnd<fbinder::RemoteControllerMarker>>>::default()
    });

    struct TestRemoteControllerConnector {}

    impl RemoteControllerConnector for TestRemoteControllerConnector {
        fn connect_to_remote_controller(
            _current_task: &CurrentTask,
            service_name: &str,
        ) -> Result<ClientEnd<fbinder::RemoteControllerMarker>, Errno> {
            REMOTE_CONTROLLER_CLIENT.lock().remove(service_name).ok_or_else(|| errno!(ENOENT))
        }
    }

    /// Setup and run a test against the remote binder. The closure that is passed to this function
    /// will be called with a binder proxy that can be used to access the remote binder.
    async fn run_remote_binder_test<F, Fut>(f: F)
    where
        Fut: Future<Output = fbinder::BinderProxy>,
        F: FnOnce(fbinder::BinderProxy, fbinder::LutexControllerProxy) -> Fut,
    {
        let service_name = Alphanumeric.sample_string(&mut rand::thread_rng(), 16);
        let (remote_controller_client, remote_controller_server) =
            create_endpoints::<fbinder::RemoteControllerMarker>();
        REMOTE_CONTROLLER_CLIENT.lock().insert(service_name.clone(), remote_controller_client);
        // Simulate the remote binder user process.
        let (kernel, _init_task) = create_kernel_and_task();
        let starnix_thread = kernel.kthreads.spawner().spawn_and_get_result({
            let kernel = Arc::clone(&kernel);
            move |_, current_task| {
                current_task
                    .fs()
                    .root()
                    .create_node(&current_task, "dev".into(), mode!(IFDIR, 0o755), DeviceType::NONE)
                    .expect("mkdir dev");
                let dev = current_task
                    .lookup_path_from_root("/dev".into())
                    .expect("lookup_path_from_root");
                dev.mount(
                    WhatToMount::Fs(
                        BinderFs::new_fs(&kernel, FileSystemOptions::default()).expect("new_fs"),
                    ),
                    MountFlags::empty(),
                )
                .expect("mount");

                let task: AutoReleasableTask = CurrentTask::create_init_child_process(
                    &kernel,
                    &CString::new("remote_binder".to_string()).expect("CString"),
                )
                .expect("Task")
                .into();

                let remote_binder_handle =
                    RemoteBinderHandle::<TestRemoteControllerConnector>::new(&task.thread_group);

                let service_name_string =
                    CString::new(service_name.as_bytes()).expect("CString::new");
                let service_name_bytes = service_name_string.as_bytes_with_nul();
                let service_name_address =
                    map_memory(&task, UserAddress::default(), service_name_bytes.len() as u64);
                task.mm()
                    .write_memory(service_name_address, service_name_bytes)
                    .expect("write_memory");

                let start_command_address =
                    map_memory(&task, UserAddress::default(), std::mem::size_of::<u64>() as u64);
                task.mm()
                    .write_object(start_command_address.into(), &service_name_address.ptr())
                    .expect("write_object");

                let wait_command_address = map_memory(
                    &task,
                    UserAddress::default(),
                    std::mem::size_of::<uapi::remote_binder_wait_command>() as u64,
                );

                let start_result = remote_binder_handle.ioctl(
                    &task,
                    uapi::REMOTE_BINDER_START,
                    start_command_address.into(),
                );
                if must_interrupt(&start_result).is_none() {
                    panic!("Unexpected result for start ioctl: {start_result:?}");
                }
                loop {
                    let result = remote_binder_handle.ioctl(
                        &task,
                        uapi::REMOTE_BINDER_WAIT,
                        wait_command_address.into(),
                    );
                    if must_interrupt(&result).is_none() {
                        break result;
                    }
                }
            }
        });

        // Wait for the Start request
        let mut remote_controller_stream = fbinder::RemoteControllerRequestStream::from_channel(
            fasync::Channel::from_channel(remote_controller_server.into_channel()),
        );
        let (dev_binder_client_end, lutex_controller_client_end) =
            match remote_controller_stream.try_next().await {
                Ok(Some(fbinder::RemoteControllerRequest::Start { payload, .. })) => (
                    payload.dev_binder.expect("dev_binder"),
                    payload.lutex_controller.expect("lutex_controller"),
                ),
                x => panic!("Expected a start request, got: {x:?}"),
            };

        let lutex_controller = lutex_controller_client_end.into_proxy().expect("into_proxy");

        let (process_accessor_client_end, process_accessor_server_end) =
            create_endpoints::<fbinder::ProcessAccessorMarker>();
        let process_accessor_task =
            fasync::Task::local(run_process_accessor(process_accessor_server_end));

        let (binder, binder_server_end) =
            create_proxy::<fbinder::BinderMarker>().expect("create_proxy");

        let process =
            fuchsia_runtime::process_self().duplicate(zx::Rights::SAME_RIGHTS).expect("process");
        let dev_binder =
            fbinder::DevBinderSynchronousProxy::new(dev_binder_client_end.into_channel());
        dev_binder
            .open(fbinder::DevBinderOpenRequest {
                path: Some(b"/dev/binder".to_vec()),
                process_accessor: Some(process_accessor_client_end),
                process: Some(process),
                binder: Some(binder_server_end),
                ..Default::default()
            })
            .expect("open");

        // Do the test.
        let binder = f(binder, lutex_controller).await;

        // Notify of the close binder
        dev_binder
            .close(fbinder::DevBinderCloseRequest {
                binder: Some(binder.into_channel().expect("into_channel").into_zx_channel().into()),
                ..Default::default()
            })
            .expect("close");

        std::mem::drop(dev_binder);
        starnix_thread.await.expect("thread join").expect("thread result");
        process_accessor_task.await.expect("process accessor wait");
    }

    #[::fuchsia::test]
    async fn external_binder_connection() {
        run_remote_binder_test(|binder, _| async move {
            const VMO_SIZE: usize = 10 * 1024 * 1024;
            let vmo = zx::Vmo::create(VMO_SIZE as u64).expect("Vmo::create");
            let addr = fuchsia_runtime::vmar_root_self()
                .map(0, &vmo, 0, VMO_SIZE, zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE)
                .expect("map");
            scopeguard::defer! {
              // SAFETY This is a ffi call to a kernel syscall.
              unsafe { fuchsia_runtime::vmar_root_self().unmap(addr, VMO_SIZE).expect("unmap"); }
            }

            binder.set_vmo(vmo, addr as u64).expect("set_vmo");
            let mut version = uapi::binder_version { protocol_version: 0 };
            let version_ref = &mut version as *mut uapi::binder_version;
            binder
                .ioctl(42, uapi::BINDER_VERSION, version_ref as u64)
                .await
                .expect("ioctl")
                .expect("ioctl");
            // SAFETY This is safe, because version is repr(C)
            let version = unsafe { std::ptr::read_volatile(version_ref) };
            assert_eq!(version.protocol_version, uapi::BINDER_CURRENT_PROTOCOL_VERSION as i32);
            binder
        })
        .await;
    }

    #[::fuchsia::test]
    async fn lutex_controller() {
        run_remote_binder_test(|binder, lutex_controller| async move {
            const VMO_SIZE: usize = 4 * 1024;
            let vmo = zx::Vmo::create(VMO_SIZE as u64).expect("Vmo::create");
            // Wait on an incorrect value.
            let wait = lutex_controller
                .wait_bitset(fbinder::WaitBitsetRequest {
                    vmo: Some(
                        vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicate vmo"),
                    ),
                    offset: Some(0),
                    value: Some(1),
                    ..Default::default()
                })
                .await
                .expect("got_answer");
            assert_eq!(wait, Err(fposix::Errno::Eagain));

            // Wait with a timeout
            let wait = lutex_controller
                .wait_bitset(fbinder::WaitBitsetRequest {
                    vmo: Some(
                        vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicate vmo"),
                    ),
                    offset: Some(0),
                    value: Some(0),
                    deadline: Some(0),
                    ..Default::default()
                })
                .await
                .expect("got_answer");
            assert_eq!(wait, Err(fposix::Errno::Etimedout));

            let mut wait = lutex_controller.wait_bitset(fbinder::WaitBitsetRequest {
                vmo: Some(vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicate vmo")),
                offset: Some(0),
                value: Some(0),
                ..Default::default()
            });
            // The wait is correct, the future should stay pending until a wake.
            assert!(futures::poll!(&mut wait).is_pending());

            let waken_up = lutex_controller
                .wake_bitset(fbinder::WakeBitsetRequest {
                    vmo: Some(
                        vmo.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("duplicate vmo"),
                    ),
                    offset: Some(0),
                    count: Some(1),
                    ..Default::default()
                })
                .await
                .expect("wake_answer")
                .expect("wake_response");
            assert_eq!(waken_up.count, Some(1));

            // The wait should now return.
            assert!(wait.await.expect("await_answer").is_ok());

            binder
        })
        .await
    }
}
