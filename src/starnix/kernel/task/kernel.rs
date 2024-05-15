// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{
        android::bootloader_message_store::AndroidBootloaderMessageStore,
        device_mapper::DeviceMapperRegistry,
        framebuffer::{AspectRatio, Framebuffer},
        loop_device::LoopDeviceRegistry,
        remote_block_device::RemoteBlockDeviceRegistry,
        sync_fence_registry::SyncFenceRegistry,
        BinderDevice, DeviceMode, DeviceRegistry,
    },
    fs::proc::SystemLimits,
    mm::{FutexTable, SharedFutexKey},
    power::SuspendResumeManagerHandle,
    task::{
        AbstractUnixSocketNamespace, AbstractVsockSocketNamespace, CurrentTask, HrTimerManager,
        HrTimerManagerHandle, IpTables, KernelStats, KernelThreads, NetstackDevices, PidTable,
        StopState, Syslog, UtsNamespace, UtsNamespaceHandle,
    },
    vdso::vdso_loader::Vdso,
    vfs::{
        fuse::FuseCtlFs,
        socket::{
            GenericMessage, GenericNetlink, NetlinkSenderReceiverProvider, NetlinkToClientSender,
            SocketAddress,
        },
        DelayedReleaser, FileHandle, FileOps, FileSystemHandle, FsNode, FsString, Mounts,
    },
};
use bstr::BString;
use fidl::{
    endpoints::{create_endpoints, ClientEnd, DiscoverableProtocolMarker, ProtocolMarker, Proxy},
    AsHandleRef,
};
use fidl_fuchsia_io as fio;
use fidl_fuchsia_scheduler::RoleManagerSynchronousProxy;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::FutureExt;
use netlink::{interfaces::InterfacesHandler, Netlink, NETLINK_LOG_TAG};
use once_cell::sync::OnceCell;
use selinux::security_server::SecurityServer;
use starnix_lifecycle::{AtomicU32Counter, AtomicU64Counter};
use starnix_logging::{log_error, CoreDumpList};
use starnix_sync::{
    DeviceOpen, KernelIpTables, KernelSwapFiles, LockBefore, Locked, OrderedMutex, OrderedRwLock,
    RwLock,
};
use starnix_uapi::{
    device_type::DeviceType, errno, errors::Errno, from_status_like_fdio, open_flags::OpenFlags,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU16, AtomicU8},
        Arc, Weak,
    },
};

#[derive(Debug, Default, Clone, Copy)]
pub struct KernelFeatures {
    pub bpf_v2: bool,
    pub log_dump_on_exit: bool,
}

/// The shared, mutable state for the entire Starnix kernel.
///
/// The `Kernel` object holds all kernel threads, userspace tasks, and file system resources for a
/// single instance of the Starnix kernel. In production, there is one instance of this object for
/// the entire Starnix kernel. However, multiple instances of this object can be created in one
/// process during unit testing.
///
/// The structure of this object will likely need to evolve as we implement more namespacing and
/// isolation mechanisms, such as `namespaces(7)` and `pid_namespaces(7)`.
pub struct Kernel {
    /// The kernel threads running on behalf of this kernel.
    pub kthreads: KernelThreads,

    /// The feaures enabled for this kernel.
    pub features: KernelFeatures,

    /// The processes and threads running in this kernel, organized by pid_t.
    pub pids: RwLock<PidTable>,

    /// The default namespace for abstract AF_UNIX sockets in this kernel.
    ///
    /// Rather than use this default namespace, abstract socket addresses
    /// should be looked up in the AbstractSocketNamespace on each Task
    /// object because some Task objects might have a non-default namespace.
    pub default_abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

    /// The default namespace for abstract AF_VSOCK sockets in this kernel.
    pub default_abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

    /// The kernel command line. Shows up in /proc/cmdline.
    pub cmdline: BString,

    // Owned by anon_node.rs
    pub anon_fs: OnceCell<FileSystemHandle>,
    // Owned by pipe.rs
    pub pipe_fs: OnceCell<FileSystemHandle>,
    // Owned by socket.rs
    pub socket_fs: OnceCell<FileSystemHandle>,
    // Owned by devtmpfs.rs
    pub dev_tmp_fs: OnceCell<FileSystemHandle>,
    // Owned by devpts.rs
    pub dev_pts_fs: OnceCell<FileSystemHandle>,
    // Owned by procfs.rs
    pub proc_fs: OnceCell<FileSystemHandle>,
    // Owned by sysfs.rs
    pub sys_fs: OnceCell<FileSystemHandle>,
    // Owned by security/selinux_hooks/fs.rs
    pub selinux_fs: OnceCell<FileSystemHandle>,
    // The SELinux security server. Initialized if SELinux is enabled.
    pub security_server: Option<Arc<SecurityServer>>,
    // Owned by tracefs/fs.rs
    pub trace_fs: OnceCell<FileSystemHandle>,
    // Owned by vfs/fuse.rs
    pub fusectl_fs: OnceCell<Arc<FuseCtlFs>>,

    /// The registry of device drivers.
    pub device_registry: DeviceRegistry,

    /// The service directory of the container.
    container_svc: Option<fio::DirectoryProxy>,

    /// The data directory of the container.
    pub container_data_dir: Option<fio::DirectorySynchronousProxy>,

    /// The registry of active loop devices.
    ///
    /// See <https://man7.org/linux/man-pages/man4/loop.4.html>
    pub loop_device_registry: Arc<LoopDeviceRegistry>,

    /// The registry of active device mapper devices.
    pub device_mapper_registry: Arc<DeviceMapperRegistry>,

    /// The registry of block devices backed by a remote fuchsia.io file.
    pub remote_block_device_registry: Arc<RemoteBlockDeviceRegistry>,

    /// If a remote block device named "misc" is created, keep track of it; this is used by Android
    /// to pass boot parameters to the bootloader.  Since Starnix is acting as a de-facto bootloader
    /// for Android, we need to be able to peek into these messages.
    /// Note that this might never be initialized (if the "misc" device never gets registered).
    pub bootloader_message_store: OnceCell<AndroidBootloaderMessageStore>,

    /// A `Framebuffer` that can be used to display a view in the workstation UI. If the container
    /// specifies the `framebuffer` feature this framebuffer will be registered as a device.
    ///
    /// When a component is run in that container and also specifies the `framebuffer` feature, the
    /// framebuffer will be served as the view of the component.
    pub framebuffer: Arc<Framebuffer>,

    /// Implementation of fuchsia.starnix.device.SyncFenceRegistry. It is used to hold Linux
    /// compatible fences for Fuchsia.
    pub sync_fence_registry: Arc<SyncFenceRegistry>,

    /// The binder driver registered for this container, indexed by their device type.
    pub binders: RwLock<BTreeMap<DeviceType, BinderDevice>>,

    /// The iptables used for filtering network packets.
    pub iptables: OrderedRwLock<IpTables, KernelIpTables>,

    /// The futexes shared across processes.
    pub shared_futexes: FutexTable<SharedFutexKey>,

    /// The default UTS namespace for all tasks.
    ///
    /// Because each task can have its own UTS namespace, you probably want to use
    /// the UTS namespace handle of the task, which may/may not point to this one.
    pub root_uts_ns: UtsNamespaceHandle,

    /// A struct containing a VMO with a vDSO implementation, if implemented for a given architecture, and possibly an offset for a sigreturn function.
    pub vdso: Vdso,

    /// The table of devices installed on the netstack and their associated
    /// state local to this `Kernel`.
    pub netstack_devices: Arc<NetstackDevices>,

    /// Files that are currently available for swapping.
    /// Note: Starnix never actually swaps memory to these files. We just need to track them
    /// to pass conformance tests.
    pub swap_files: OrderedMutex<Vec<FileHandle>, KernelSwapFiles>,

    /// The implementation of generic Netlink protocol families.
    generic_netlink: OnceCell<GenericNetlink<NetlinkToClientSender<GenericMessage>>>,

    /// The implementation of networking-related Netlink protocol families.
    network_netlink: OnceCell<Netlink<NetlinkSenderReceiverProvider>>,

    /// Inspect instrumentation for this kernel instance.
    pub inspect_node: fuchsia_inspect::Node,

    /// Diagnostics information about crashed tasks.
    pub core_dumps: CoreDumpList,

    /// The kinds of seccomp action that gets logged, stored as a bit vector.
    /// Each potential SeccompAction gets a bit in the vector, as specified by
    /// SeccompAction::logged_bit_offset.  If the bit is set, that means the
    /// action should be logged when it is taken, subject to the caveats
    /// described in seccomp(2).  The value of the bit vector is exposed to users
    /// in a text form in the file /proc/sys/kernel/seccomp/actions_logged.
    pub actions_logged: AtomicU16,

    /// The manager for suspend/resume.
    pub suspend_resume_manager: SuspendResumeManagerHandle,

    /// Unique IDs for new mounts and mount namespaces.
    pub next_mount_id: AtomicU64Counter,
    pub next_peer_group_id: AtomicU64Counter,
    pub next_namespace_id: AtomicU64Counter,

    /// Unique IDs for file objects.
    pub next_file_object_id: AtomicU64Counter,

    /// Unique cookie used to link two inotify events, usually an IN_MOVE_FROM/IN_MOVE_TO pair.
    pub next_inotify_cookie: AtomicU32Counter,

    /// Controls which processes a process is allowed to ptrace.  See Documentation/security/Yama.txt
    pub ptrace_scope: AtomicU8,

    // The Fuchsia build version returned by `fuchsia.buildinfo.Provider`.
    pub build_version: OnceCell<String>,

    pub stats: Arc<KernelStats>,

    /// Resource limits that are exposed, for example, via sysctl.
    pub system_limits: SystemLimits,

    // The service to handle delayed releases. This is required for elements that requires to
    // execute some code when released and requires a known context (both in term of lock context,
    // as well as `CurrentTask`).
    pub delayed_releaser: DelayedReleaser,

    /// Proxy to the scheduler role manager for adjusting task priorities.
    pub role_manager: Option<RoleManagerSynchronousProxy>,

    /// The syslog manager.
    pub syslog: Syslog,

    /// All mounts.
    pub mounts: Mounts,

    /// The manager for creating and managing high-resolution timers.
    pub hrtimer_manager: HrTimerManagerHandle,
}

/// An implementation of [`InterfacesHandler`].
///
/// This holds a `Weak<Kernel>` because it is held within a [`Netlink`] which
/// is itself held within an `Arc<Kernel>`. Holding an `Arc<T>` within an
/// `Arc<T>` prevents the `Arc`'s ref count from ever reaching 0, causing a
/// leak.
struct InterfacesHandlerImpl(Weak<Kernel>);

impl InterfacesHandlerImpl {
    fn with_netstack_devices<
        F: FnOnce(
                &CurrentTask,
                &Arc<NetstackDevices>,
                Option<&FileSystemHandle>,
                Option<&FileSystemHandle>,
            ) + Sync
            + Send
            + 'static,
    >(
        &mut self,
        f: F,
    ) {
        if let Some(kernel) = self.0.upgrade() {
            kernel.kthreads.spawner().spawn(move |_, current_task| {
                let kernel = current_task.kernel();
                f(current_task, &kernel.netstack_devices, kernel.proc_fs.get(), kernel.sys_fs.get())
            });
        }
    }
}

impl InterfacesHandler for InterfacesHandlerImpl {
    fn handle_new_link(&mut self, name: &str) {
        let name = name.to_owned();
        self.with_netstack_devices(move |current_task, devs, proc_fs, sys_fs| {
            devs.add_dev(current_task, &name, proc_fs, sys_fs)
        })
    }

    fn handle_deleted_link(&mut self, name: &str) {
        let name = name.to_owned();
        self.with_netstack_devices(move |_current_task, devs, _proc_fs, _sys_fs| {
            devs.remove_dev(&name)
        })
    }
}

impl Kernel {
    pub fn new(
        cmdline: BString,
        features: KernelFeatures,
        container_svc: Option<fio::DirectoryProxy>,
        container_data_dir: Option<fio::DirectorySynchronousProxy>,
        role_manager: Option<RoleManagerSynchronousProxy>,
        inspect_node: fuchsia_inspect::Node,
        framebuffer_aspect_ratio: Option<&AspectRatio>,
        security_server: Option<Arc<SecurityServer>>,
    ) -> Result<Arc<Kernel>, zx::Status> {
        let unix_address_maker =
            Box::new(|x: FsString| -> SocketAddress { SocketAddress::Unix(x) });
        let vsock_address_maker = Box::new(|x: u32| -> SocketAddress { SocketAddress::Vsock(x) });
        let framebuffer =
            Framebuffer::new(framebuffer_aspect_ratio).expect("Failed to create framebuffer");

        let core_dumps = CoreDumpList::new(inspect_node.create_child("coredumps"));

        let this = Arc::new_cyclic(|kernel| Kernel {
            kthreads: KernelThreads::new(kernel.clone()),
            features,
            pids: RwLock::new(PidTable::new()),
            default_abstract_socket_namespace: AbstractUnixSocketNamespace::new(unix_address_maker),
            default_abstract_vsock_namespace: AbstractVsockSocketNamespace::new(
                vsock_address_maker,
            ),
            cmdline,
            anon_fs: Default::default(),
            pipe_fs: Default::default(),
            dev_tmp_fs: Default::default(),
            dev_pts_fs: Default::default(),
            proc_fs: Default::default(),
            socket_fs: Default::default(),
            sys_fs: Default::default(),
            selinux_fs: Default::default(),
            security_server,
            trace_fs: Default::default(),
            fusectl_fs: Default::default(),
            device_registry: DeviceRegistry::new(),
            container_svc,
            container_data_dir,
            loop_device_registry: Default::default(),
            device_mapper_registry: Default::default(),
            remote_block_device_registry: Default::default(),
            bootloader_message_store: OnceCell::new(),
            framebuffer,
            sync_fence_registry: SyncFenceRegistry::new(),
            binders: Default::default(),
            iptables: OrderedRwLock::new(IpTables::new()),
            shared_futexes: FutexTable::<SharedFutexKey>::default(),
            root_uts_ns: Arc::new(RwLock::new(UtsNamespace::default())),
            vdso: Vdso::new(),
            netstack_devices: Arc::default(),
            swap_files: Default::default(),
            generic_netlink: OnceCell::new(),
            network_netlink: OnceCell::new(),
            inspect_node,
            core_dumps,
            actions_logged: AtomicU16::new(0),
            suspend_resume_manager: Default::default(),
            next_mount_id: AtomicU64Counter::new(1),
            next_peer_group_id: AtomicU64Counter::new(1),
            next_namespace_id: AtomicU64Counter::new(1),
            next_inotify_cookie: AtomicU32Counter::new(1),
            next_file_object_id: Default::default(),
            system_limits: SystemLimits::default(),
            ptrace_scope: AtomicU8::new(0),
            build_version: OnceCell::new(),
            stats: Arc::new(KernelStats::default()),
            delayed_releaser: Default::default(),
            role_manager,
            syslog: Default::default(),
            mounts: Mounts::new(),
            hrtimer_manager: HrTimerManager::new(),
        });

        // Make a copy of this Arc for the inspect lazy node to use but don't create an Arc cycle
        // because the inspect node that owns this reference is owned by the kernel.
        let kernel = Arc::downgrade(&this);
        this.inspect_node.record_lazy_child("thread_groups", move || {
            if let Some(kernel) = kernel.upgrade() {
                let inspector = kernel.get_thread_groups_inspect();
                async move { Ok(inspector) }.boxed()
            } else {
                async move { Err(anyhow::format_err!("kernel was dropped")) }.boxed()
            }
        });

        Ok(this)
    }

    /// Opens a device file (driver) identified by `dev`.
    pub fn open_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        dev: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        self.device_registry.open_device(locked, current_task, node, flags, dev, mode)
    }

    /// Return a reference to the GenericNetlink implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Generic Netlink server in a separate kthread.
    pub(crate) fn generic_netlink(&self) -> &GenericNetlink<NetlinkToClientSender<GenericMessage>> {
        self.generic_netlink.get_or_init(|| {
            let (generic_netlink, generic_netlink_fut) = GenericNetlink::new();
            self.kthreads.spawn_future(async move {
                generic_netlink_fut.await;
                log_error!("Generic Netlink future unexpectedly exited");
            });
            generic_netlink
        })
    }

    /// Return a reference to the [`netlink::Netlink`] implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Netlink implementation.
    pub(crate) fn network_netlink<'a>(
        self: &'a Arc<Self>,
    ) -> &'a Netlink<NetlinkSenderReceiverProvider> {
        self.network_netlink.get_or_init(|| {
            let (network_netlink, network_netlink_async_worker) =
                Netlink::new(InterfacesHandlerImpl(Arc::downgrade(self)));
            self.kthreads.spawn(move |_, _| {
                fasync::LocalExecutor::new().run_singlethreaded(network_netlink_async_worker);
                log_error!(tag = NETLINK_LOG_TAG, "Netlink async worker unexpectedly exited");
            });
            network_netlink
        })
    }

    /// Returns a Proxy to the service used by the container at `filename`.
    #[allow(unused)]
    pub fn connect_to_named_protocol_at_container_svc<P: ProtocolMarker>(
        &self,
        filename: &str,
    ) -> Result<ClientEnd<P>, Errno> {
        let svc = self.container_svc.as_ref().ok_or_else(|| errno!(ENOENT))?;
        let (client_end, server_end) = create_endpoints::<P>();
        fdio::service_connect_at(svc.as_channel().as_ref(), filename, server_end.into_channel())
            .map_err(|status| from_status_like_fdio!(status))?;
        Ok(client_end)
    }

    /// Returns a Proxy to the service `P` used by the container.
    pub fn connect_to_protocol_at_container_svc<P: DiscoverableProtocolMarker>(
        &self,
    ) -> Result<ClientEnd<P>, Errno> {
        self.connect_to_named_protocol_at_container_svc::<P>(P::PROTOCOL_NAME)
    }

    /// Returns true if SELinux is enabled with a hard-coded fake policy.
    /// This is a temporary API, for use at call-sites which the SELinux
    /// implementation does not yet support.
    pub fn has_fake_selinux(&self) -> bool {
        self.security_server.as_ref().is_some_and(|s| s.is_fake())
    }

    fn get_thread_groups_inspect(&self) -> fuchsia_inspect::Inspector {
        let inspector = fuchsia_inspect::Inspector::default();

        let thread_groups = inspector.root();
        for thread_group in self.pids.read().get_thread_groups() {
            let tg = thread_group.read();

            let tg_node = thread_groups.create_child(format!("{}", thread_group.leader));
            if let Ok(koid) = &thread_group.process.get_koid() {
                tg_node.record_int("koid", koid.raw_koid() as i64);
            }
            tg_node.record_int("pid", thread_group.leader as i64);
            tg_node.record_int("ppid", tg.get_ppid() as i64);
            tg_node.record_bool("stopped", thread_group.load_stopped() == StopState::GroupStopped);

            let tasks_node = tg_node.create_child("tasks");
            for task in tg.tasks() {
                let set_properties = |node: &fuchsia_inspect::Node| {
                    node.record_string("command", task.command().to_str().unwrap_or("{err}"));

                    let sched_policy = task.read().scheduler_policy;
                    if !sched_policy.is_default() {
                        node.record_string("sched_policy", format!("{sched_policy:?}"));
                    }
                };
                if task.id == thread_group.leader {
                    set_properties(&tg_node);
                } else {
                    tasks_node.record_child(task.id.to_string(), |task_node| {
                        set_properties(task_node);
                    });
                };
            }
            tg_node.record(tasks_node);
            thread_groups.record(tg_node);
        }

        inspector
    }
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel").finish()
    }
}
