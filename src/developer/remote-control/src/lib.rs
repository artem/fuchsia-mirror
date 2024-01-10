// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::host_identifier::HostIdentifier,
    anyhow::{Context as _, Result},
    component_debug::dirs::*,
    component_debug::lifecycle::*,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_developer_remotecontrol as rcs,
    fidl_fuchsia_developer_remotecontrol_connector as connector,
    fidl_fuchsia_diagnostics as diagnostics, fidl_fuchsia_io as fio, fidl_fuchsia_io as io,
    fidl_fuchsia_sys2 as fsys,
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_zircon as zx,
    futures::prelude::*,
    moniker::Moniker,
    std::{borrow::Borrow, cell::RefCell, collections::HashMap, rc::Rc, rc::Weak},
    tracing::*,
};

mod host_identifier;

pub struct RemoteControlService {
    ids: RefCell<Vec<Weak<RefCell<Vec<u64>>>>>,
    id_allocator: Box<dyn Fn() -> Result<HostIdentifier>>,
    connector: Box<dyn Fn(fidl::Socket)>,
    moniker_map: HashMap<String, String>,
}

struct Client {
    // Maintain reference-counts to this client's ids.
    // The ids may be shared (e.g. when Overnet maintains two
    // connections to the target -- legacy + CSO), so we can't
    // just maintain a list of RCS's ids and remove when one
    // disappars.  Instead, when these are freed due to the client
    // being dropped, the RCS Weak references will become invalid.
    allocated_ids: Rc<RefCell<Vec<u64>>>,
}

impl RemoteControlService {
    pub async fn new(connector: impl Fn(fidl::Socket) + 'static) -> Self {
        let moniker_map = Self::load_moniker_map().await;
        Self::new_with_allocator(connector, Box::new(|| HostIdentifier::new()), moniker_map)
    }

    async fn load_moniker_map() -> HashMap<String, String> {
        let f = match fuchsia_fs::file::open_in_namespace(
            "/pkg/data/moniker-map.json",
            io::OpenFlags::RIGHT_READABLE,
        ) {
            Ok(f) => f,
            Err(e) => {
                error!(%e, "failed to open moniker maps json file");
                return HashMap::default();
            }
        };
        let bytes = match fuchsia_fs::file::read(&f).await {
            Ok(b) => b,
            Err(e) => {
                error!(?e, "failed to read bytes from moniker map json");
                return HashMap::default();
            }
        };
        match serde_json::from_slice(bytes.as_slice()) {
            Ok(m) => m,
            Err(e) => {
                error!(?e, "failed to parse moniker map json");
                HashMap::default()
            }
        }
    }

    pub(crate) fn new_with_allocator(
        connector: impl Fn(fidl::Socket) + 'static,
        id_allocator: impl Fn() -> Result<HostIdentifier> + 'static,
        moniker_map: HashMap<String, String>,
    ) -> Self {
        Self {
            id_allocator: Box::new(id_allocator),
            ids: Default::default(),
            connector: Box::new(connector),
            moniker_map,
        }
    }

    // Some of the ID-lists may be gone because old clients have shut down.
    // They will have a strong_count of 0.  Drop 'em.
    fn remove_old_ids(self: &Rc<Self>) {
        self.ids.borrow_mut().retain(|wirc| wirc.strong_count() > 0);
    }

    async fn handle_connector(
        self: &Rc<Self>,
        client: &Client,
        request: connector::ConnectorRequest,
    ) -> Result<()> {
        let connector::ConnectorRequest::EstablishCircuit { id, socket, responder } = request;
        (self.connector)(socket);
        client.allocated_ids.borrow_mut().push(id);
        responder.send()?;
        Ok(())
    }

    async fn handle(self: &Rc<Self>, request: rcs::RemoteControlRequest) -> Result<()> {
        match request {
            rcs::RemoteControlRequest::EchoString { value, responder } => {
                info!("Received echo string {}", value);
                responder.send(&value)?;
                Ok(())
            }
            rcs::RemoteControlRequest::LogMessage { tag, message, severity, responder } => {
                match severity {
                    diagnostics::Severity::Trace => trace!(%tag, "{}", message),
                    diagnostics::Severity::Debug => debug!(%tag, "{}", message),
                    diagnostics::Severity::Info => info!(%tag, "{}", message),
                    diagnostics::Severity::Warn => warn!(%tag, "{}", message),
                    diagnostics::Severity::Error => error!(%tag, "{}", message),
                    // Tracing crate doesn't have a Fatal level, just log an error with a FATAL message embedded.
                    diagnostics::Severity::Fatal => error!(%tag, "<FATAL> {}", message),
                }
                responder.send()?;
                Ok(())
            }
            rcs::RemoteControlRequest::IdentifyHost { responder } => {
                self.clone().identify_host(responder).await?;
                Ok(())
            }
            rcs::RemoteControlRequest::OpenCapability {
                moniker,
                capability_set,
                capability_name,
                server_channel,
                flags,
                responder,
            } => {
                responder.send(
                    self.clone()
                        .open_capability(
                            moniker,
                            capability_set,
                            capability_name,
                            flags,
                            server_channel,
                        )
                        .await,
                )?;
                Ok(())
            }
            rcs::RemoteControlRequest::GetTime { responder } => {
                responder.send(fuchsia_zircon::Time::get_monotonic().into_nanos())?;
                Ok(())
            }
            rcs::RemoteControlRequest::_UnknownMethod { ordinal, .. } => {
                warn!("Received unknown request with ordinal {ordinal}");
                Ok(())
            }
        }
    }

    pub async fn serve_connector_stream(self: Rc<Self>, stream: connector::ConnectorRequestStream) {
        // When the stream ends, the client (and its ids) will drop
        let allocated_ids = Rc::new(RefCell::new(vec![]));
        self.ids.borrow_mut().push(Rc::downgrade(&allocated_ids));
        let client = Client { allocated_ids };
        stream
            .for_each_concurrent(None, |request| async {
                match request {
                    Ok(request) => {
                        let _ = self
                            .handle_connector(&client, request)
                            .await
                            .map_err(|e| warn!("stream request handling error: {:?}", e));
                    }
                    Err(e) => warn!("stream error: {:?}", e),
                }
            })
            .await;
    }

    pub async fn serve_stream(self: Rc<Self>, stream: rcs::RemoteControlRequestStream) {
        stream
            .for_each_concurrent(None, |request| async {
                match request {
                    Ok(request) => {
                        let _ = self
                            .handle(request)
                            .await
                            .map_err(|e| warn!("stream request handling error: {:?}", e));
                    }
                    Err(e) => warn!("stream error: {:?}", e),
                }
            })
            .await;
    }

    fn map_moniker(self: &Rc<Self>, moniker: String) -> String {
        self.moniker_map.get(&moniker).cloned().unwrap_or(moniker)
    }

    pub async fn identify_host(
        self: &Rc<Self>,
        responder: rcs::RemoteControlIdentifyHostResponder,
    ) -> Result<()> {
        let identifier = match (self.id_allocator)() {
            Ok(i) => i,
            Err(e) => {
                error!(%e, "Allocating host identifier");
                return responder
                    .send(Err(rcs::IdentifyHostError::ProxyConnectionFailed))
                    .context("responding to client");
            }
        };

        // We need to clean up the ids at some point. Let's do
        // it when those IDs are asked for.
        self.remove_old_ids();
        // Now the only vecs should be ones which are still held with a strong
        // Rc reference. Extract those.
        let ids: Vec<u64> = self
            .ids
            .borrow()
            .iter()
            .flat_map(|w| -> Vec<u64> {
                // This is all sadmac's fault. Grr. (Because he suggested, correctly, that
                // we use a Rc<Vec<_>> instead of Vec<Rc<_>>)
                <Rc<RefCell<Vec<u64>>> as Borrow<RefCell<Vec<u64>>>>::borrow(
                    &w.upgrade().expect("Didn't we just clear out refs with expired values??"),
                )
                .borrow()
                .clone()
            })
            .collect();
        let target_identity = identifier.identify().await.map(move |mut i| {
            i.ids = Some(ids);
            i
        });
        responder.send(target_identity.as_ref().map_err(|e| *e)).context("responding to client")?;
        Ok(())
    }

    /// Connects to a capability identified by the given moniker in the specified set of
    /// capabilities at the given capability name.
    async fn open_capability(
        self: &Rc<Self>,
        moniker: String,
        capability_set: fsys::OpenDirType,
        capability_name: String,
        flags: io::OpenFlags,
        server_end: zx::Channel,
    ) -> Result<(), rcs::ConnectCapabilityError> {
        let moniker = self.map_moniker(moniker);
        // Connect to the root LifecycleController protocol
        let lifecycle = connect_to_protocol_at_path::<fsys::LifecycleControllerMarker>(
            "/svc/fuchsia.sys2.LifecycleController.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to lifecycle controller");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })?;

        // Connect to the root RealmQuery protocol
        let query = connect_to_protocol_at_path::<fsys::RealmQueryMarker>(
            "/svc/fuchsia.sys2.RealmQuery.root",
        )
        .map_err(|err| {
            error!(%err, "could not connect to realm query");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })?;

        let moniker = Moniker::try_from(moniker.as_str())
            .map_err(|_| rcs::ConnectCapabilityError::InvalidMoniker)?;
        connect_to_capability_at_moniker(
            moniker,
            capability_set,
            capability_name,
            server_end,
            flags,
            lifecycle,
            query,
        )
        .await
    }
}

/// Connect to the capability at the provided moniker in the specified set of capabilities under
/// the provided capability name.
async fn connect_to_capability_at_moniker(
    moniker: Moniker,
    capability_set: fsys::OpenDirType,
    capability_name: String,
    server_end: zx::Channel,
    flags: io::OpenFlags,
    lifecycle: fsys::LifecycleControllerProxy,
    query: fsys::RealmQueryProxy,
) -> Result<(), rcs::ConnectCapabilityError> {
    // This is a no-op if already resolved.
    resolve_instance(&lifecycle, &moniker)
        .map_err(|err| match err {
            ResolveError::ActionError(ActionError::InstanceNotFound) => {
                rcs::ConnectCapabilityError::NoMatchingComponent
            }
            err => {
                error!(?err, "error resolving component");
                rcs::ConnectCapabilityError::CapabilityConnectFailed
            }
        })
        .await?;

    let dir = open_instance_dir_root_readable(&moniker, capability_set.into(), &query)
        .map_err(|err| {
            error!(?err, "error opening exposed dir");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        })
        .await?;

    connect_to_capability_in_dir(&dir, &capability_name, server_end, flags).await?;
    Ok(())
}

async fn connect_to_capability_in_dir(
    dir: &io::DirectoryProxy,
    capability_name: &str,
    server_end: zx::Channel,
    flags: io::OpenFlags,
) -> Result<(), rcs::ConnectCapabilityError> {
    check_entry_exists(dir, capability_name).await?;

    // Connect to the capability
    dir.open(flags, io::ModeType::empty(), capability_name, ServerEnd::new(server_end)).map_err(
        |err| {
            error!(%err, "error opening capability from exposed dir");
            rcs::ConnectCapabilityError::CapabilityConnectFailed
        },
    )
}

// Checks that the given directory contains an entry with the given name.
async fn check_entry_exists(
    dir: &io::DirectoryProxy,
    capability_name: &str,
) -> Result<(), rcs::ConnectCapabilityError> {
    let dir_idx = capability_name.rfind('/');
    let (capability_name, entries) = match dir_idx {
        Some(dir_idx) => {
            let dirname = &capability_name[0..dir_idx];
            let basename = &capability_name[dir_idx + 1..];
            let nested_dir =
                fuchsia_fs::directory::open_directory(dir, dirname, fio::OpenFlags::RIGHT_READABLE)
                    .await
                    .map_err(|_| rcs::ConnectCapabilityError::NoMatchingCapabilities)?;
            let entries = fuchsia_fs::directory::readdir(&nested_dir)
                .await
                .map_err(|_| rcs::ConnectCapabilityError::CapabilityConnectFailed)?;
            (basename, entries)
        }
        None => {
            let entries = fuchsia_fs::directory::readdir(dir)
                .await
                .map_err(|_| rcs::ConnectCapabilityError::CapabilityConnectFailed)?;
            (capability_name, entries)
        }
    };
    if entries.iter().any(|e| e.name == capability_name) {
        Ok(())
    } else {
        Err(rcs::ConnectCapabilityError::NoMatchingCapabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_buildinfo as buildinfo;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fidl_fuchsia_device as fdevice;
    use fidl_fuchsia_hwinfo as hwinfo;
    use fidl_fuchsia_io as fio;
    use fidl_fuchsia_net as fnet;
    use fidl_fuchsia_net_interfaces as fnet_interfaces;
    use fidl_fuchsia_sysinfo as sysinfo;
    use fuchsia_async as fasync;
    use fuchsia_component::server::ServiceFs;
    use fuchsia_zircon as zx;

    const NODENAME: &'static str = "thumb-set-human-shred";
    const BOOT_TIME: u64 = 123456789000000000;
    const SYSINFO_SERIAL: &'static str = "test_sysinfo_serial";
    const SERIAL: &'static str = "test_serial";
    const BOARD_CONFIG: &'static str = "test_board_name";
    const PRODUCT_CONFIG: &'static str = "core";

    const IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];
    const IPV6_ADDR: [u8; 16] = [127, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1, 2, 3, 4, 5, 6];
    const FAKE_SERVICE_MONIKER: &'static str = "my/component";
    const MAPPED_SERVICE_MONIKER: &'static str = "my/other/component";

    fn setup_fake_device_service() -> hwinfo::DeviceProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<hwinfo::DeviceMarker>().unwrap();
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    hwinfo::DeviceRequest::GetInfo { responder } => {
                        let _ = responder.send(&hwinfo::DeviceInfo {
                            serial_number: Some(String::from(SERIAL)),
                            ..Default::default()
                        });
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_sysinfo_service(status: zx::Status) -> sysinfo::SysInfoProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<sysinfo::SysInfoMarker>().unwrap();
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    sysinfo::SysInfoRequest::GetSerialNumber { responder } => {
                        let _ = responder.send(
                            Result::from(status)
                                .map(|_| SYSINFO_SERIAL)
                                .map_err(zx::Status::into_raw),
                        );
                    }
                    _ => panic!("unexpected request: {req:?}"),
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_build_info_service() -> buildinfo::ProviderProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<buildinfo::ProviderMarker>().unwrap();
        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    buildinfo::ProviderRequest::GetBuildInfo { responder } => {
                        let _ = responder.send(&buildinfo::BuildInfo {
                            board_config: Some(String::from(BOARD_CONFIG)),
                            product_config: Some(String::from(PRODUCT_CONFIG)),
                            ..Default::default()
                        });
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_name_provider_service() -> fdevice::NameProviderProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fdevice::NameProviderMarker>().unwrap();

        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fdevice::NameProviderRequest::GetDeviceName { responder } => {
                        let _ = responder.send(Ok(NODENAME));
                    }
                }
            }
        })
        .detach();

        proxy
    }

    fn setup_fake_interface_state_service() -> fnet_interfaces::StateProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_interfaces::StateMarker>().unwrap();

        fasync::Task::spawn(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    fnet_interfaces::StateRequest::GetWatcher {
                        options: _,
                        watcher,
                        control_handle: _,
                    } => {
                        let mut stream = watcher.into_stream().unwrap();
                        let mut first = true;
                        while let Ok(Some(req)) = stream.try_next().await {
                            match req {
                                fnet_interfaces::WatcherRequest::Watch { responder } => {
                                    let event = if first {
                                        first = false;
                                        fnet_interfaces::Event::Existing(
                                            fnet_interfaces::Properties {
                                                id: Some(1),
                                                addresses: Some(
                                                    IntoIterator::into_iter([
                                                        fnet::Subnet {
                                                            addr: fnet::IpAddress::Ipv4(
                                                                fnet::Ipv4Address {
                                                                    addr: IPV4_ADDR,
                                                                },
                                                            ),
                                                            prefix_len: 4,
                                                        },
                                                        fnet::Subnet {
                                                            addr: fnet::IpAddress::Ipv6(
                                                                fnet::Ipv6Address {
                                                                    addr: IPV6_ADDR,
                                                                },
                                                            ),
                                                            prefix_len: 110,
                                                        },
                                                    ])
                                                    .map(Some)
                                                    .map(|addr| fnet_interfaces::Address {
                                                        addr,
                                                        valid_until: Some(1),
                                                        assignment_state: Some(fnet_interfaces::AddressAssignmentState::Assigned),
                                                        ..Default::default()
                                                    })
                                                    .collect(),
                                                ),
                                                online: Some(true),
                                                device_class: Some(
                                                    fnet_interfaces::DeviceClass::Loopback(
                                                        fnet_interfaces::Empty {},
                                                    ),
                                                ),
                                                has_default_ipv4_route: Some(false),
                                                has_default_ipv6_route: Some(false),
                                                name: Some(String::from("eth0")),
                                                ..Default::default()
                                            },
                                        )
                                    } else {
                                        fnet_interfaces::Event::Idle(fnet_interfaces::Empty {})
                                    };
                                    let () = responder.send(&event).unwrap();
                                }
                            }
                        }
                    }
                }
            }
        })
        .detach();

        proxy
    }

    #[derive(Default)]
    #[non_exhaustive]
    struct RcsEnv {
        moniker_map: HashMap<String, String>,
        system_info_proxy: Option<sysinfo::SysInfoProxy>,
    }

    fn make_rcs_from_env(env: RcsEnv) -> Rc<RemoteControlService> {
        let RcsEnv { moniker_map, system_info_proxy } = env;
        Rc::new(RemoteControlService::new_with_allocator(
            |_| (),
            move || {
                Ok(HostIdentifier {
                    interface_state_proxy: setup_fake_interface_state_service(),
                    name_provider_proxy: setup_fake_name_provider_service(),
                    device_info_proxy: setup_fake_device_service(),
                    system_info_proxy: system_info_proxy
                        .clone()
                        .unwrap_or_else(|| setup_fake_sysinfo_service(zx::Status::INTERNAL)),
                    build_info_proxy: setup_fake_build_info_service(),
                    boot_timestamp_nanos: BOOT_TIME,
                })
            },
            moniker_map,
        ))
    }

    fn make_rcs_with_maps(moniker_map: HashMap<String, String>) -> Rc<RemoteControlService> {
        make_rcs_from_env(RcsEnv { moniker_map, ..Default::default() })
    }

    fn setup_rcs_proxy_from_env(
        env: RcsEnv,
    ) -> (rcs::RemoteControlProxy, connector::ConnectorProxy) {
        let service = make_rcs_from_env(env);

        let (rcs_proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<rcs::RemoteControlMarker>().unwrap();
        fasync::Task::local({
            let service = Rc::clone(&service);
            async move {
                service.serve_stream(stream).await;
            }
        })
        .detach();
        let (connector_proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<connector::ConnectorMarker>().unwrap();
        fasync::Task::local(async move {
            service.serve_connector_stream(stream).await;
        })
        .detach();

        (rcs_proxy, connector_proxy)
    }

    fn setup_rcs_proxy() -> rcs::RemoteControlProxy {
        setup_rcs_proxy_from_env(Default::default()).0
    }

    fn setup_rcs_proxy_with_connector() -> (rcs::RemoteControlProxy, connector::ConnectorProxy) {
        setup_rcs_proxy_from_env(Default::default())
    }

    fn setup_fake_lifecycle_controller() -> fsys::LifecycleControllerProxy {
        fidl::endpoints::spawn_stream_handler(
            move |request: fsys::LifecycleControllerRequest| async move {
                match request {
                    fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                        assert_eq!(moniker, "core/my_component");
                        responder.send(Ok(())).unwrap()
                    }
                    _ => panic!("unexpected request: {:?}", request),
                }
            },
        )
        .unwrap()
    }

    fn setup_exposed_dir(server: ServerEnd<fio::DirectoryMarker>) {
        let mut fs = ServiceFs::new();
        fs.add_fidl_service(move |_: hwinfo::BoardRequestStream| {});
        fs.dir("svc").add_fidl_service(move |_: hwinfo::BoardRequestStream| {});
        fs.serve_connection(server).unwrap();
        fasync::Task::spawn(fs.collect::<()>()).detach();
    }

    /// Set up a fake realm query which asserts a requests coming in have the
    /// right options set, including which of a component's capability sets
    /// (ie. incoming namespace, outgoing directory, etc) the capability is
    /// expected to be requested from.
    fn setup_fake_realm_query(capability_set: fsys::OpenDirType) -> fsys::RealmQueryProxy {
        fidl::endpoints::spawn_stream_handler(move |request: fsys::RealmQueryRequest| async move {
            match request {
                fsys::RealmQueryRequest::Open {
                    moniker,
                    dir_type,
                    flags,
                    mode,
                    path,
                    object,
                    responder,
                } => {
                    assert_eq!(moniker, "core/my_component");
                    assert_eq!(dir_type, capability_set);
                    assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
                    assert_eq!(mode, fio::ModeType::empty());
                    assert_eq!(path, ".");

                    setup_exposed_dir(object.into_channel().into());

                    responder.send(Ok(())).unwrap()
                }
                _ => panic!("unexpected request: {:?}", request),
            }
        })
        .unwrap()
    }

    #[fuchsia::test]
    async fn test_connect_to_component_capability() -> Result<()> {
        for dir_type in vec![
            fsys::OpenDirType::ExposedDir,
            fsys::OpenDirType::NamespaceDir,
            fsys::OpenDirType::OutgoingDir,
        ] {
            let (_client, server) = zx::Channel::create();
            let lifecycle = setup_fake_lifecycle_controller();
            let query = setup_fake_realm_query(dir_type);
            connect_to_capability_at_moniker(
                Moniker::try_from("./core/my_component").unwrap(),
                dir_type,
                "fuchsia.hwinfo.Board".to_string(),
                server,
                io::OpenFlags::RIGHT_READABLE,
                lifecycle,
                query,
            )
            .await
            .unwrap();
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_connect_to_component_capability_in_subdirectory() -> Result<()> {
        for dir_type in vec![
            fsys::OpenDirType::ExposedDir,
            fsys::OpenDirType::NamespaceDir,
            fsys::OpenDirType::OutgoingDir,
        ] {
            let (_client, server) = zx::Channel::create();
            let lifecycle = setup_fake_lifecycle_controller();
            let query = setup_fake_realm_query(dir_type);
            connect_to_capability_at_moniker(
                Moniker::try_from("./core/my_component").unwrap(),
                dir_type,
                "svc/fuchsia.hwinfo.Board".to_string(),
                server,
                io::OpenFlags::RIGHT_READABLE,
                lifecycle,
                query,
            )
            .await
            .unwrap();
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_connect_to_capability_not_available() -> Result<()> {
        for dir_type in vec![
            fsys::OpenDirType::ExposedDir,
            fsys::OpenDirType::NamespaceDir,
            fsys::OpenDirType::OutgoingDir,
        ] {
            let (_client, server) = zx::Channel::create();
            let lifecycle = setup_fake_lifecycle_controller();
            let query = setup_fake_realm_query(dir_type);
            let error = connect_to_capability_at_moniker(
                Moniker::try_from("./core/my_component").unwrap(),
                dir_type,
                "fuchsia.not.exposed".to_string(),
                server,
                io::OpenFlags::RIGHT_READABLE,
                lifecycle,
                query,
            )
            .await
            .unwrap_err();
            assert_eq!(error, rcs::ConnectCapabilityError::NoMatchingCapabilities);
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_connect_to_capability_not_available_in_subdirectory() -> Result<()> {
        for dir_type in vec![
            fsys::OpenDirType::ExposedDir,
            fsys::OpenDirType::NamespaceDir,
            fsys::OpenDirType::OutgoingDir,
        ] {
            let (_client, server) = zx::Channel::create();
            let lifecycle = setup_fake_lifecycle_controller();
            let query = setup_fake_realm_query(dir_type);
            let error = connect_to_capability_at_moniker(
                Moniker::try_from("./core/my_component").unwrap(),
                dir_type,
                "svc/fuchsia.not.exposed".to_string(),
                server,
                io::OpenFlags::RIGHT_READABLE,
                lifecycle,
                query,
            )
            .await
            .unwrap_err();
            assert_eq!(error, rcs::ConnectCapabilityError::NoMatchingCapabilities);
        }
        Ok(())
    }

    #[fuchsia::test]
    async fn test_identify_host() -> Result<()> {
        let rcs_proxy = setup_rcs_proxy();

        let resp = rcs_proxy.identify_host().await.unwrap().unwrap();

        assert_eq!(resp.serial_number.unwrap(), SERIAL);
        assert_eq!(resp.board_config.unwrap(), BOARD_CONFIG);
        assert_eq!(resp.product_config.unwrap(), PRODUCT_CONFIG);
        assert_eq!(resp.nodename.unwrap(), NODENAME);

        let addrs = resp.addresses.unwrap();
        assert_eq!(
            addrs[..],
            [
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: IPV4_ADDR }),
                    prefix_len: 4,
                },
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: IPV6_ADDR }),
                    prefix_len: 110,
                }
            ]
        );

        assert_eq!(resp.boot_timestamp_nanos.unwrap(), BOOT_TIME);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_identify_host_sysinfo_serial() -> Result<()> {
        let (rcs_proxy, _) = setup_rcs_proxy_from_env(RcsEnv {
            system_info_proxy: Some(setup_fake_sysinfo_service(zx::Status::OK)),
            ..Default::default()
        });

        let resp = rcs_proxy.identify_host().await.unwrap().unwrap();

        assert_eq!(resp.serial_number.unwrap(), SYSINFO_SERIAL);
        assert_eq!(resp.board_config.unwrap(), BOARD_CONFIG);
        assert_eq!(resp.product_config.unwrap(), PRODUCT_CONFIG);
        assert_eq!(resp.nodename.unwrap(), NODENAME);

        let addrs = resp.addresses.unwrap();
        assert_eq!(
            addrs[..],
            [
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: IPV4_ADDR }),
                    prefix_len: 4,
                },
                fnet::Subnet {
                    addr: fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: IPV6_ADDR }),
                    prefix_len: 110,
                }
            ]
        );

        assert_eq!(resp.boot_timestamp_nanos.unwrap(), BOOT_TIME);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_ids_in_host_identify() -> Result<()> {
        let (rcs_proxy, connector_proxy) = setup_rcs_proxy_with_connector();

        let ident = rcs_proxy.identify_host().await.unwrap().unwrap();
        assert_eq!(ident.ids, Some(vec![]));

        let (pumpkin_a, _) = fidl::Socket::create_stream();
        let (pumpkin_b, _) = fidl::Socket::create_stream();
        connector_proxy.establish_circuit(1234, pumpkin_a).await.unwrap();
        connector_proxy.establish_circuit(4567, pumpkin_b).await.unwrap();

        let ident = rcs_proxy.identify_host().await.unwrap().unwrap();
        let ids = ident.ids.unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(1234u64, ids[0]);
        assert_eq!(4567u64, ids[1]);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_map_moniker() -> Result<()> {
        let map = [(FAKE_SERVICE_MONIKER.to_string(), MAPPED_SERVICE_MONIKER.to_string())]
            .into_iter()
            .collect();

        let service = make_rcs_with_maps(map);
        assert_eq!(service.map_moniker(FAKE_SERVICE_MONIKER.to_string()), MAPPED_SERVICE_MONIKER);

        let service = make_rcs_with_maps(HashMap::new());
        assert_eq!(service.map_moniker(FAKE_SERVICE_MONIKER.to_string()), FAKE_SERVICE_MONIKER);
        Ok(())
    }
}
