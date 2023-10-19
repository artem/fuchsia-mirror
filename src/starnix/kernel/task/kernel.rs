// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::BString;
use fidl::{
    endpoints::{create_endpoints, ClientEnd, ProtocolMarker, Proxy},
    AsHandleRef,
};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::FutureExt;
use netlink::{interfaces::InterfacesHandler, Netlink, NETLINK_LOG_TAG};
use once_cell::sync::OnceCell;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        atomic::{AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8},
        Arc, Weak,
    },
};

use crate::{
    device::{
        framebuffer::Framebuffer, input::InputDevice, loop_device::LoopDeviceRegistry,
        BinderDriver, DeviceMode, DeviceOps, DeviceRegistry,
    },
    diagnostics::CoreDumpList,
    fs::{
        devtmpfs::devtmpfs_create_device,
        inotify::InotifyLimits,
        kobject::*,
        socket::{
            GenericMessage, GenericNetlink, NetlinkSenderReceiverProvider, NetlinkToClientSender,
            SocketAddress,
        },
        sysfs::{BlockDeviceDirectory, DeviceDirectory, SysFsDirectory},
        FileOps, FileSystemHandle, FsNode,
    },
    lock::RwLock,
    logging::{log_error, set_zx_name},
    mm::{FutexTable, SharedFutexKey},
    power::PowerManager,
    selinux::SecurityServer,
    task::*,
    types::{DeviceType, Errno, OpenFlags, *},
    vdso::vdso_loader::Vdso,
};

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
    /// The Zircon job object that holds the processes running in this kernel.
    pub job: zx::Job,

    /// The kernel threads running on behalf of this kernel.
    pub kthreads: KernelThreads,

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
    // Owned by selinux/fs.rs
    pub selinux_fs: OnceCell<FileSystemHandle>,
    // The SELinux security server. Initialized if SELinux is enabled.
    pub security_server: Option<SecurityServer>,
    // Owned by tracefs/fs.rs
    pub trace_fs: OnceCell<FileSystemHandle>,

    /// The registry of device drivers.
    pub device_registry: DeviceRegistry,

    // The features enabled for the container this kernel is associated with, as specified in
    // the container's configuration file.
    pub features: HashSet<String>,

    /// The service directory of the container.
    container_svc: Option<fio::DirectoryProxy>,

    /// The data directory of the container.
    pub container_data_dir: Option<fio::DirectorySynchronousProxy>,

    /// The registry of active loop devices.
    ///
    /// See <https://man7.org/linux/man-pages/man4/loop.4.html>
    pub loop_device_registry: Arc<LoopDeviceRegistry>,

    /// A `Framebuffer` that can be used to display a view in the workstation UI. If the container
    /// specifies the `framebuffer` feature this framebuffer will be registered as a device.
    ///
    /// When a component is run in that container and also specifies the `framebuffer` feature, the
    /// framebuffer will be served as the view of the component.
    pub framebuffer: Arc<Framebuffer>,

    /// An `InputDevice` that can be opened to read input events from Fuchsia.
    ///
    /// If the container specifies the `framebuffer` features, this `InputDevice` will be registered
    /// as a device.
    ///
    /// When a component is run in that container, and also specifies the `framebuffer` feature,
    /// Starnix will relay input events from Fuchsia to the component.
    pub input_device: Arc<InputDevice>,

    /// The binder driver registered for this container, indexed by their device type.
    pub binders: RwLock<BTreeMap<DeviceType, Arc<BinderDriver>>>,

    /// The iptables used for filtering network packets.
    pub iptables: RwLock<IpTables>,

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

    /// The implementation of generic Netlink protocol families.
    generic_netlink: OnceCell<GenericNetlink<NetlinkToClientSender<GenericMessage>>>,

    /// The implementation of networking-related Netlink protocol families.
    network_netlink: OnceCell<Netlink<NetlinkSenderReceiverProvider>>,

    /// Inspect instrumentation for this kernel instance.
    inspect_node: fuchsia_inspect::Node,

    /// Diagnostics information about crashed tasks.
    pub core_dumps: CoreDumpList,

    // The kinds of seccomp action that gets logged, stored as a bit vector.
    // Each potential SeccompAction gets a bit in the vector, as specified by
    // SeccompAction::logged_bit_offset.  If the bit is set, that means the
    // action should be logged when it is taken, subject to the caveats
    // described in seccomp(2).  The value of the bit vector is exposed to users
    // in a text form in the file /proc/sys/kernel/seccomp/actions_logged.
    pub actions_logged: AtomicU16,

    /// The manager for power subsystems including reboot and suspend.
    pub power_manager: PowerManager,

    /// Unique IDs for new mounts and mount namespaces.
    pub next_mount_id: AtomicU64,
    pub next_peer_group_id: AtomicU64,
    pub next_namespace_id: AtomicU64,

    /// Unique cookie used to link two inotify events, usually an IN_MOVE_FROM/IN_MOVE_TO pair.
    pub next_inotify_cookie: AtomicU32,

    pub inotify_limits: InotifyLimits,

    // Controls which processes a process is allowed to ptrace.  See Documentation/security/Yama.txt
    pub ptrace_scope: AtomicU8,
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
        F: FnOnce(&Arc<NetstackDevices>, Option<&FileSystemHandle>, Option<&FileSystemHandle>),
    >(
        &mut self,
        f: F,
    ) {
        let Self(rc) = self;
        let Some(rc) = rc.upgrade() else {
            // The kernel may be getting torn-down.
            return;
        };
        f(&rc.netstack_devices, rc.proc_fs.get(), rc.sys_fs.get())
    }
}

impl InterfacesHandler for InterfacesHandlerImpl {
    fn handle_new_link(&mut self, name: &str) {
        self.with_netstack_devices(|devs, proc_fs, sys_fs| devs.add_dev(name, proc_fs, sys_fs))
    }

    fn handle_deleted_link(&mut self, name: &str) {
        self.with_netstack_devices(|devs, _proc_fs, _sys_fs| devs.remove_dev(name))
    }
}

impl Kernel {
    pub fn new(
        name: &[u8],
        cmdline: BString,
        features: HashSet<String>,
        container_svc: Option<fio::DirectoryProxy>,
        container_data_dir: Option<fio::DirectorySynchronousProxy>,
        inspect_node: fuchsia_inspect::Node,
    ) -> Result<Arc<Kernel>, zx::Status> {
        let unix_address_maker = Box::new(|x: Vec<u8>| -> SocketAddress { SocketAddress::Unix(x) });
        let vsock_address_maker = Box::new(|x: u32| -> SocketAddress { SocketAddress::Vsock(x) });
        let job = fuchsia_runtime::job_default().create_child_job()?;
        set_zx_name(&job, name);

        let framebuffer = Framebuffer::new(features.iter().find(|f| f.starts_with("aspect_ratio")))
            .expect("Failed to create framebuffer");
        let input_device = InputDevice::new(framebuffer.clone(), &inspect_node);

        let core_dumps = CoreDumpList::new(inspect_node.create_child("coredumps"));

        let security_server = features.contains("selinux").then(|| SecurityServer::new());

        let this = Arc::new(Kernel {
            job,
            kthreads: KernelThreads::default(),
            pids: RwLock::new(PidTable::new()),
            default_abstract_socket_namespace: AbstractUnixSocketNamespace::new(unix_address_maker),
            default_abstract_vsock_namespace: AbstractVsockSocketNamespace::new(
                vsock_address_maker,
            ),
            cmdline,
            anon_fs: OnceCell::new(),
            pipe_fs: OnceCell::new(),
            dev_tmp_fs: OnceCell::new(),
            dev_pts_fs: OnceCell::new(),
            proc_fs: OnceCell::new(),
            socket_fs: OnceCell::new(),
            sys_fs: OnceCell::new(),
            selinux_fs: OnceCell::new(),
            security_server,
            trace_fs: OnceCell::new(),
            device_registry: DeviceRegistry::new(),
            features,
            container_svc,
            container_data_dir,
            loop_device_registry: Default::default(),
            framebuffer,
            input_device,
            binders: Default::default(),
            iptables: RwLock::new(IpTables::new()),
            shared_futexes: FutexTable::<SharedFutexKey>::default(),
            root_uts_ns: Arc::new(RwLock::new(UtsNamespace::default())),
            vdso: Vdso::new(),
            netstack_devices: Arc::default(),
            generic_netlink: OnceCell::new(),
            network_netlink: OnceCell::new(),
            inspect_node,
            core_dumps,
            actions_logged: AtomicU16::new(0),
            power_manager: PowerManager::default(),
            next_mount_id: AtomicU64::new(1),
            next_peer_group_id: AtomicU64::new(1),
            next_namespace_id: AtomicU64::new(1),
            next_inotify_cookie: AtomicU32::new(1),
            inotify_limits: InotifyLimits {
                max_queued_events: AtomicI32::new(16384),
                max_user_instances: AtomicI32::new(128),
                max_user_watches: AtomicI32::new(1048576),
            },
            ptrace_scope: AtomicU8::new(0),
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

    /// Add a device in the hierarchy tree.
    ///
    /// If it's a Block device, the device will be added under "block" class.
    pub fn add_device(self: &Arc<Self>, dev_attr: KObjectDeviceAttribute) {
        let kobj_device = match dev_attr.device.mode {
            DeviceMode::Char => {
                assert!(dev_attr.class.is_some(), "no class is associated with the device.");
                dev_attr.class.unwrap().get_or_create_child(
                    &dev_attr.name,
                    KType::Device(dev_attr.device.clone()),
                    DeviceDirectory::new,
                )
            }
            DeviceMode::Block => {
                let block_class = self.device_registry.virtual_bus().get_or_create_child(
                    b"block",
                    KType::Class,
                    SysFsDirectory::new,
                );
                block_class.get_or_create_child(
                    &dev_attr.name,
                    KType::Device(dev_attr.device.clone()),
                    BlockDeviceDirectory::new,
                )
            }
        };
        self.device_registry.dispatch_uevent(UEventAction::Add, kobj_device);
        match devtmpfs_create_device(self, dev_attr.device.clone()) {
            Ok(_) => (),
            Err(err) => {
                log_error!("Cannot add block device {:?} in devtmpfs ({:?})", dev_attr.device, err)
            }
        };
    }

    /// Add a device in the hierarchy tree and register its DeviceOps.
    pub fn add_and_register_device(
        self: &Arc<Self>,
        dev_attr: KObjectDeviceAttribute,
        dev_ops: impl DeviceOps,
    ) {
        match match dev_attr.device.mode {
            DeviceMode::Char => self.device_registry.register_chrdev(
                dev_attr.device.device_type.major(),
                dev_attr.device.device_type.minor(),
                1,
                dev_ops,
            ),
            DeviceMode::Block => self.device_registry.register_blkdev(
                dev_attr.device.device_type.major(),
                dev_attr.device.device_type.minor(),
                1,
                dev_ops,
            ),
        } {
            Ok(_) => (),
            Err(err) => log_error!("Cannot register device {:?} ({:?})", dev_attr.device, err),
        }
        self.add_device(dev_attr);
    }

    /// Opens a device file (driver) identified by `dev`.
    pub fn open_device(
        &self,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        dev: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.device_registry.open_device(current_task, node, flags, dev, mode)
    }

    /// Return a reference to the GenericNetlink implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Generic Netlink server in a separate kthread.
    pub(crate) fn generic_netlink(&self) -> &GenericNetlink<NetlinkToClientSender<GenericMessage>> {
        self.generic_netlink.get_or_init(|| {
            let (generic_netlink, generic_netlink_fut) = GenericNetlink::new();
            self.kthreads.spawner.spawn(move || {
                fasync::LocalExecutor::new().run_singlethreaded(generic_netlink_fut);
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
            self.kthreads.spawn(move || {
                fasync::LocalExecutor::new().run_singlethreaded(network_netlink_async_worker);
                log_error!(tag = NETLINK_LOG_TAG, "Netlink async worker unexpectedly exited");
            });
            network_netlink
        })
    }

    /// Returns a Proxy to the service exposed to the container at `filename`.
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

    pub fn mock_selinux(&self) -> bool {
        self.features.contains("mock_selinux")
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
            tg_node.record_bool("stopped", tg.stopped == StopState::GroupStopped);

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
