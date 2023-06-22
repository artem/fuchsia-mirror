// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{create_endpoints, ClientEnd, ProtocolMarker, Proxy};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use netlink::{interfaces::InterfacesHandler, Netlink, NETLINK_LOG_TAG};
use once_cell::sync::OnceCell;
use std::{
    collections::{BTreeMap, HashSet},
    iter::FromIterator,
    sync::{Arc, Weak},
};

use crate::{
    device::{
        framebuffer::Framebuffer, input::InputFile, loopback::LoopDeviceRegistry, BinderDriver,
        DeviceMode, DeviceRegistry,
    },
    fs::{
        socket::{
            GenericMessage, GenericNetlink, NetlinkSenderReceiverProvider, NetlinkToClientSender,
            SocketAddress,
        },
        FileOps, FileSystemHandle, FsNode,
    },
    lock::RwLock,
    logging::{log_error, set_zx_name},
    mm::FutexTable,
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
    pub cmdline: Vec<u8>,

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
    // Owned by selinux.rs
    pub selinux_fs: OnceCell<FileSystemHandle>,
    // Owned by tracefs/fs.rs
    pub trace_fs: OnceCell<FileSystemHandle>,

    /// The registry of device drivers.
    pub device_registry: RwLock<DeviceRegistry>,

    // The features enabled for the container this kernel is associated with, as specified in
    // the container's configuration file.
    pub features: HashSet<String>,

    /// The service directory of the container.
    container_svc: Option<fio::DirectoryProxy>,

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

    /// An `InputFile` that can read input events from Fuchsia, and offer them to a component running
    /// under Starnix.
    ///
    /// If the container specifies the `framebuffer` features, this `InputFile` will be registered
    /// as a device.
    ///
    /// When a component is run in that container, and also specifies the `framebuffer` feature,
    /// Starnix will relay input events from Fuchsia to the component.
    pub input_file: Arc<InputFile>,

    /// The binder driver registered for this container, indexed by their device type.
    pub binders: RwLock<BTreeMap<DeviceType, Arc<BinderDriver>>>,

    /// The iptables used for filtering network packets.
    pub iptables: RwLock<IpTables>,

    /// The futexes shared across processes.
    pub shared_futexes: FutexTable,

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
}

/// An implementation of [`InterfacesHandler`].
///
/// This holds a `Weak<Kernel>` because it is held within a [`Netlink`] which
/// is itself held within an `Arc<Kernel>`. Holding an `Arc<T>` within an
/// `Arc<T>` prevents the `Arc`'s ref count from ever reaching 0, causing a
/// leak.
struct InterfacesHandlerImpl(Weak<Kernel>);

impl InterfacesHandlerImpl {
    fn with_netstack_devices<F: FnOnce(&Arc<NetstackDevices>, Option<&FileSystemHandle>)>(
        &mut self,
        f: F,
    ) {
        let Self(rc) = self;
        let Some(rc) = rc.upgrade() else {
            // The kernel may be getting torn-down.
            return
        };
        f(&rc.netstack_devices, rc.proc_fs.get())
    }
}

impl InterfacesHandler for InterfacesHandlerImpl {
    fn handle_new_link(&mut self, name: &str) {
        self.with_netstack_devices(|devs, proc_fs| devs.add_dev(name, proc_fs))
    }

    fn handle_deleted_link(&mut self, name: &str) {
        self.with_netstack_devices(|devs, _proc_fs| devs.remove_dev(name))
    }
}

impl Kernel {
    pub fn new(
        name: &[u8],
        cmdline: &[u8],
        features: &[String],
        container_svc: Option<fio::DirectoryProxy>,
    ) -> Result<Arc<Kernel>, zx::Status> {
        let unix_address_maker = Box::new(|x: Vec<u8>| -> SocketAddress { SocketAddress::Unix(x) });
        let vsock_address_maker = Box::new(|x: u32| -> SocketAddress { SocketAddress::Vsock(x) });
        let job = fuchsia_runtime::job_default().create_child_job()?;
        set_zx_name(&job, name);

        let (framebuffer, input_file) =
            Framebuffer::new_with_input(features.iter().find(|f| f.starts_with("aspect_ratio")))
                .expect("Failed to create framebuffer");

        Ok(Arc::new(Kernel {
            job,
            kthreads: KernelThreads::default(),
            pids: RwLock::new(PidTable::new()),
            default_abstract_socket_namespace: AbstractUnixSocketNamespace::new(unix_address_maker),
            default_abstract_vsock_namespace: AbstractVsockSocketNamespace::new(
                vsock_address_maker,
            ),
            cmdline: cmdline.to_vec(),
            anon_fs: OnceCell::new(),
            pipe_fs: OnceCell::new(),
            dev_tmp_fs: OnceCell::new(),
            dev_pts_fs: OnceCell::new(),
            proc_fs: OnceCell::new(),
            socket_fs: OnceCell::new(),
            sys_fs: OnceCell::new(),
            selinux_fs: OnceCell::new(),
            trace_fs: OnceCell::new(),
            device_registry: RwLock::new(DeviceRegistry::new_with_common_devices()),
            features: HashSet::from_iter(features.iter().cloned()),
            container_svc,
            loop_device_registry: Default::default(),
            framebuffer,
            input_file,
            binders: Default::default(),
            iptables: RwLock::new(IpTables::new()),
            shared_futexes: Default::default(),
            root_uts_ns: Arc::new(RwLock::new(UtsNamespace::default())),
            vdso: Vdso::new(),
            netstack_devices: Arc::default(),
            generic_netlink: OnceCell::new(),
            network_netlink: OnceCell::new(),
        }))
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
        let registry = self.device_registry.read();
        registry.open_device(current_task, node, flags, dev, mode)
    }

    /// Return a reference to the GenericNetlink implementation.
    ///
    /// This function follows the lazy initialization pattern, where the first
    /// call will instantiate the Generic Netlink server in a separate kthread.
    pub(crate) fn generic_netlink(&self) -> &GenericNetlink<NetlinkToClientSender<GenericMessage>> {
        self.generic_netlink.get_or_init(|| {
            let (generic_netlink, generic_netlink_fut) = GenericNetlink::new();
            self.kthreads.pool.dispatch(move || {
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
            self.kthreads.dispatch(move || {
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

    pub fn selinux_enabled(&self) -> bool {
        self.features.contains("selinux_enabled")
    }
}
