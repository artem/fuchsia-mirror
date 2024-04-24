// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

pub mod virtualization;

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    num::NonZeroU16,
    pin::pin,
};

use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_dhcpv6 as fnet_dhcpv6;
use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_masquerade as fnet_masquerade;
use fidl_fuchsia_net_root as fnet_root;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fidl_fuchsia_net_stack as fnet_stack;
use fidl_fuchsia_netemul_network as fnetemul_network;
use fuchsia_async::{self as fasync, DurationExt as _, TimeoutExt as _};
use fuchsia_zircon as zx;

use anyhow::Context as _;
use assert_matches::assert_matches;
use const_unwrap::const_unwrap_option;
use fidl::endpoints::Proxy as _;
use futures::{
    future::{FutureExt as _, LocalBoxFuture, TryFutureExt as _},
    stream::{self, StreamExt as _, TryStreamExt as _},
};
use futures_util::AsyncWriteExt;
use net_declare::{
    fidl_ip, fidl_ip_v4, fidl_subnet, net_ip_v6, net_prefix_length_v4, net_subnet_v6, std_ip,
};
use net_types::{
    ethernet::Mac,
    ip::{self as net_types_ip, Ipv4},
};
use netemul::{RealmTcpListener, RealmTcpStream, RealmUdpSocket};
use netstack_testing_common::{
    dhcpv4 as dhcpv4_helper,
    interfaces::{self, TestInterfaceExt as _},
    nud::apply_nud_flake_workaround,
    realms::{
        KnownServiceProvider, ManagementAgent, Manager, ManagerConfig, NetCfgBasic, NetCfgVersion,
        Netstack, Netstack3, TestRealmExt as _, TestSandboxExt,
    },
    try_all, try_any, wait_for_component_stopped, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
    ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::{EmptyBuf, InnerPacketBuilder as _, ParsablePacket as _, Serializer as _};
use packet_formats::{
    ethernet::{
        EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck,
        ETHERNET_MIN_BODY_LEN_NO_TAG,
    },
    ip::{IpProto, Ipv6Proto},
    ipv6::Ipv6PacketBuilder,
    testutil::parse_ip_packet,
    udp::{UdpPacket, UdpPacketBuilder, UdpParseArgs},
};
use packet_formats_dhcp::v6 as dhcpv6;
use test_case::test_case;

async fn with_netcfg_owned_device<
    M: Manager,
    N: Netstack,
    F: for<'a> FnOnce(
        u64,
        &'a netemul::TestNetwork<'a>,
        &'a fnet_interfaces::StateProxy,
        &'a netemul::TestRealm<'a>,
        &'a netemul::TestSandbox,
    ) -> LocalBoxFuture<'a, ()>,
>(
    name: &str,
    manager_config: ManagerConfig,
    use_out_of_stack_dhcp_client: bool,
    extra_known_service_providers: impl IntoIterator<Item = KnownServiceProvider>,
    after_interface_up: F,
) -> String {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            [
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client,
                    config: manager_config,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ]
            .into_iter()
            .chain(extra_known_service_providers),
        )
        .expect("create netstack realm");

    // Add a device to the realm.
    let network = sandbox.create_network(name).await.expect("create network");
    let endpoint = network.create_endpoint(name).await.expect("create endpoint");
    endpoint.set_link_up(true).await.expect("set link up");
    let endpoint_mount_path = netemul::devfs_device_path("ep");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    realm.add_virtual_device(&endpoint, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device {}: {:?}", endpoint_mount_path.display(), e)
    });

    // Make sure the Netstack got the new device added.
    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None).fuse();
    let mut wait_for_netmgr = pin!(wait_for_netmgr);
    let (if_id, if_name): (u64, String) = interfaces::wait_for_non_loopback_interface_up(
        &interface_state,
        &mut wait_for_netmgr,
        None,
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
    )
    .await
    .expect("wait for non loopback interface");

    after_interface_up(if_id, &network, &interface_state, &realm, &sandbox).await;

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    realm.shutdown().await.expect("failed to shutdown realm");

    if_name
}

/// Test that NetCfg discovers a newly added device and it adds the device
/// to the Netstack.
#[netstack_test]
#[test_case(ManagerConfig::Empty, "eth"; "no_prefix")]
#[test_case(ManagerConfig::IfacePrefix, "testeth"; "with_prefix")]
async fn test_oir<M: Manager, N: Netstack>(name: &str, config: ManagerConfig, prefix: &str) {
    let if_name = with_netcfg_owned_device::<M, N, _>(
        name,
        config,
        false, /* use_out_of_stack_dhcp_client */
        [],
        |_if_id: u64,
         _: &netemul::TestNetwork<'_>,
         _: &fnet_interfaces::StateProxy,
         _: &netemul::TestRealm<'_>,
         _: &netemul::TestSandbox| async {}.boxed_local(),
    )
    .await;

    assert!(
        if_name.starts_with(prefix),
        "expected interface name to start with '{}', got = '{}'",
        prefix,
        if_name,
    );
}

// Create two realms with predefined port classes and one netstack each, managed
// by netcfg. Each realm has an endpoint, which are both on the same network.
// Send a UDP packet between Realm1 and Realm2, where Realm1 and Realm2 can be
// initialized with a predefined ManagerConfig.
#[netstack_test]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::PacketFilterEthernet,
    fhardware_network::PortClass::Ethernet,
    false; "receiver_eth_enabled__both_ports_eth")]
#[test_case(
    ManagerConfig::PacketFilterEthernet,
    ManagerConfig::Empty,
    fhardware_network::PortClass::Ethernet,
    false; "sender_eth_enabled__both_ports_eth")]
#[test_case(
    ManagerConfig::PacketFilterEthernet,
    ManagerConfig::PacketFilterEthernet,
    fhardware_network::PortClass::Ethernet,
    false; "both_eth_enabled__both_ports_eth")]
#[test_case(
    ManagerConfig::PacketFilterWlan,
    ManagerConfig::PacketFilterWlan,
    fhardware_network::PortClass::Ethernet,
    true; "both_wlan_enabled__both_ports_eth")]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::Empty,
    fhardware_network::PortClass::Ethernet,
    true; "both_no_filter__both_ports_eth")]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::PacketFilterWlan,
    fhardware_network::PortClass::Wlan,
    false; "receiver_wlan_enabled__both_ports_wlan")]
#[test_case(
    ManagerConfig::PacketFilterWlan,
    ManagerConfig::Empty,
    fhardware_network::PortClass::Wlan,
    false; "sender_wlan_enabled__both_ports_wlan")]
#[test_case(
    ManagerConfig::PacketFilterWlan,
    ManagerConfig::PacketFilterWlan,
    fhardware_network::PortClass::Wlan,
    false; "both_wlan_enabled__both_ports_wlan")]
#[test_case(
    ManagerConfig::PacketFilterEthernet,
    ManagerConfig::PacketFilterEthernet,
    fhardware_network::PortClass::Wlan,
    true; "both_eth_enabled__both_ports_wlan")]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::Empty,
    fhardware_network::PortClass::Wlan,
    true; "both_no_filter__both_ports_wlan")]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::PacketFilterWlan,
    fhardware_network::PortClass::WlanAp,
    false; "receiver_wlan_enabled__both_ports_wlan_ap")]
#[test_case(
    ManagerConfig::PacketFilterWlan,
    ManagerConfig::Empty,
    fhardware_network::PortClass::WlanAp,
    false; "sender_wlan_enabled__both_ports_wlan_ap")]
#[test_case(
    ManagerConfig::PacketFilterWlan,
    ManagerConfig::PacketFilterWlan,
    fhardware_network::PortClass::WlanAp,
    false; "both_wlan_enabled__both_ports_wlan_ap")]
#[test_case(
    ManagerConfig::PacketFilterEthernet,
    ManagerConfig::PacketFilterEthernet,
    fhardware_network::PortClass::WlanAp,
    true; "both_eth_enabled__both_ports_wlan_ap")]
#[test_case(
    ManagerConfig::Empty,
    ManagerConfig::Empty,
    fhardware_network::PortClass::WlanAp,
    true; "both_no_filter__both_ports_wlan_ap")]
async fn test_filtering_udp<M: Manager, N: Netstack>(
    name: &str,
    realm1_manager: ManagerConfig,
    realm2_manager: ManagerConfig,
    port_class: fhardware_network::PortClass,
    message_expected: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let eth_network = sandbox.create_network("eth_network").await.expect("create network");

    // Note: these ports correspond with the ports specified in the
    // packet filter netcfg configuration files.
    const SENDER_PORT: u16 = 1234;
    const RECEIVER_PORT: u16 = 8080;

    async fn setup_filtering_iface<'a, M: Manager, N: Netstack>(
        network: &'a netemul::TestNetwork<'a>,
        realm: &'a netemul::TestRealm<'a>,
        port: u16,
        name: String,
        port_class: fhardware_network::PortClass,
    ) -> (netemul::TestEndpoint<'a>, SocketAddr) {
        // Install a new device via devfs so netcfg can pick it up and install filtering rules.
        let ep = network
            .create_endpoint_with(
                format!("{name}-eth-ep"),
                fnetemul_network::EndpointConfig {
                    mtu: netemul::DEFAULT_MTU,
                    mac: None,
                    port_class,
                },
            )
            .await
            .expect("create endpoint");
        ep.set_link_up(true).await.expect("set link up");
        let endpoint_mount_path = netemul::devfs_device_path(format!("{name}-eth-ep").as_str());
        let endpoint_mount_path = endpoint_mount_path.as_path();

        realm.add_virtual_device(&ep, endpoint_mount_path).await.unwrap_or_else(|e| {
            panic!("add virtual device {}: {:?}", endpoint_mount_path.display(), e)
        });

        // Make sure the Netstack got the new device added.
        let interface_state = realm
            .connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("connect to fuchsia.net.interfaces/State service");
        let wait_for_netmgr =
            wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None)
                .fuse();
        let mut wait_for_netmgr = pin!(wait_for_netmgr);
        let (if_id, _if_name): (u64, String) = interfaces::wait_for_non_loopback_interface_up(
            &interface_state,
            &mut wait_for_netmgr,
            None,
            ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
        )
        .await
        .expect("wait for non loopback interface");

        // Get the link local address for the interface.
        let link_local_addr = interfaces::wait_for_v6_ll(&interface_state, if_id)
            .await
            .expect("netstack should have assigned a linklocal addr");
        let ep_addr = std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
            link_local_addr.into(),
            port,
            0,
            if_id.try_into().unwrap(),
        ));

        // Since this interface is installed via netcfg, we need to query into
        // fnet_root to get the Control handle to avoid flakes due to NUD failures.
        let root_interfaces = realm
            .connect_to_protocol::<fidl_fuchsia_net_root::InterfacesMarker>()
            .expect("connect to protocol");
        let (interface_control, interface_control_server_end) =
            fidl_fuchsia_net_interfaces_ext::admin::Control::create_endpoints()
                .expect("create proxy");
        root_interfaces
            .get_admin(if_id, interface_control_server_end)
            .expect("create root interfaces connection");
        apply_nud_flake_workaround(&interface_control).await.expect("nud flake workaround");

        (ep, ep_addr)
    }

    let sender_realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("{name}-sender_realm"),
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: true,
                    use_out_of_stack_dhcp_client: false,
                    config: realm1_manager,
                },
                // Include the DHCP server because we bring up a WLAN_AP device
                // in some test cases.
                KnownServiceProvider::DhcpServer { persistent: false },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("failed to create sender realm");
    let (_sender_ep, sender_ep_addr) = setup_filtering_iface::<M, N>(
        &eth_network,
        &sender_realm,
        SENDER_PORT,
        format!("sender-eth-ep"),
        port_class,
    )
    .await;

    let receiver_realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("{name}-receiver_realm"),
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    use_dhcp_server: true,
                    use_out_of_stack_dhcp_client: false,
                    config: realm2_manager,
                },
                // Include the DHCP server because we bring up a WLAN_AP device
                // in some test cases.
                KnownServiceProvider::DhcpServer { persistent: false },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("failed to create receiver realm");
    let (_receiver_ep, receiver_ep_addr) = setup_filtering_iface::<M, N>(
        &eth_network,
        &receiver_realm,
        RECEIVER_PORT,
        format!("receiver-eth-ep"),
        port_class,
    )
    .await;

    let sender_ep_sock = fasync::net::UdpSocket::bind_in_realm(&sender_realm, sender_ep_addr)
        .await
        .expect("failed to create sender socket");
    let receiver_ep_sock = fasync::net::UdpSocket::bind_in_realm(&receiver_realm, receiver_ep_addr)
        .await
        .expect("failed to create receiver socket");

    const PAYLOAD: &'static str = "Hello World";

    let sender_fut = async move {
        let r = sender_ep_sock
            .send_to(PAYLOAD.as_bytes(), receiver_ep_addr)
            .await
            .expect("sendto failed");
        assert_eq!(r, PAYLOAD.as_bytes().len());
    };
    let receiver_fut = async move {
        let mut buf = [0u8; 1024];
        let (_, from) = receiver_ep_sock.recv_from(&mut buf[..]).await.expect("recvfrom failed");
        assert_eq!(from, sender_ep_addr);
        Some(())
    };

    // Choose a timeout dependent on whether we are looking for a positive
    // check (message was received) or negative check (message was dropped).
    let timeout = if message_expected {
        ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
    } else {
        ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT
    };

    let ((), message_received) = futures::future::join(sender_fut, receiver_fut)
        .on_timeout(timeout.after_now(), || ((), None))
        .await;

    assert_eq!(message_received.is_some(), message_expected);

    // Wait for orderly shutdown of the test realms to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    sender_realm.shutdown().await.expect("failed to shutdown realm");
    receiver_realm.shutdown().await.expect("failed to shutdown realm");
}

// Test that Netcfg discovers a device, adds it to the Netstack,
// and does not provision the device (send DHCP packets).
#[netstack_test]
async fn test_install_only_no_provisioning<M: Manager, N: Netstack>(name: &str) {
    // RFC2131 and RFC8415 specify the ports that DHCP servers
    // must use for sending messages.
    const DHCPV4_SERVER_PORT: u16 = 67;
    const DHCPV4_CLIENT_PORT: u16 = 68;
    const DHCPV6_SERVER_PORT: u16 = 546;
    const DHCPV6_CLIENT_PORT: u16 = 547;

    let _if_name: String = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::AllDelegated,
        false, /* use_out_of_stack_dhcp_client */
        [KnownServiceProvider::Dhcpv6Client],
        |_if_id: u64,
         network: &netemul::TestNetwork<'_>,
         _: &fnet_interfaces::StateProxy,
         _: &netemul::TestRealm<'_>,
         _: &netemul::TestSandbox| {
            async {
                let fake_ep = network.create_fake_endpoint().expect("error creating fake ep");
                let stream = fake_ep
                    .frame_stream()
                    .map(|r| r.expect("error getting OnData event"))
                    .filter_map(|(data, dropped)| async move {
                        assert_eq!(dropped, 0);
                        let mut data = &data[..];

                        let eth =
                            EthernetFrame::parse(&mut data, EthernetFrameLengthCheck::NoCheck)
                                .expect("error parsing ethernet frame");

                        match eth.ethertype().expect("ethertype missing") {
                            packet_formats::ethernet::EtherType::Ipv4 => {
                                let (mut ipv4_body, src_ip, dst_ip, proto, _ttl) =
                                        packet_formats::testutil::parse_ip_packet::<
                                            net_types::ip::Ipv4,
                                        >(data)
                                        .expect("error parsing IPv4 packet");
                                if proto
                                    != packet_formats::ip::Ipv4Proto::Proto(
                                        packet_formats::ip::IpProto::Udp,
                                    )
                                {
                                    // Ignore non-UDP packets.
                                    return None;
                                }

                                let udp_v4_packet = packet_formats::udp::UdpPacket::parse(
                                    &mut ipv4_body,
                                    packet_formats::udp::UdpParseArgs::new(src_ip, dst_ip),
                                )
                                .expect("error parsing UDP datagram");

                                // Look for packets that are sent across the DHCP-specific ports.
                                let src_port =
                                    udp_v4_packet.src_port().expect("missing src port").get();
                                let dst_port = udp_v4_packet.dst_port().get();
                                if src_port == DHCPV4_CLIENT_PORT && dst_port == DHCPV4_SERVER_PORT
                                {
                                    return Some(());
                                }
                                return None;
                            }
                            packet_formats::ethernet::EtherType::Ipv6 => {
                                let (mut ipv6_body, src_ip, dst_ip, proto, _ttl) =
                                        packet_formats::testutil::parse_ip_packet::<
                                            net_types::ip::Ipv6,
                                        >(data)
                                        .expect("error parsing IPv4 packet");
                                if proto
                                    != packet_formats::ip::Ipv6Proto::Proto(
                                        packet_formats::ip::IpProto::Udp,
                                    )
                                {
                                    // Ignore non-UDP packets.
                                    return None;
                                }

                                let udp_v6_packet = packet_formats::udp::UdpPacket::parse(
                                    &mut ipv6_body,
                                    packet_formats::udp::UdpParseArgs::new(src_ip, dst_ip),
                                )
                                .expect("error parsing UDP datagram");

                                // Look for packets that are sent across the DHCP-specific ports.
                                let src_port =
                                    udp_v6_packet.src_port().expect("missing src port").get();
                                let dst_port = udp_v6_packet.dst_port().get();
                                if src_port == DHCPV6_CLIENT_PORT && dst_port == DHCPV6_SERVER_PORT
                                {
                                    return Some(());
                                }
                                return None;
                            }
                            packet_formats::ethernet::EtherType::Arp
                            | packet_formats::ethernet::EtherType::Other(_) => {
                                // Do nothing
                                return None;
                            }
                        }
                    });
                let mut stream = pin!(stream);
                assert!(stream
                    .next()
                    .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None)
                    .await
                    .is_none());
            }
            .boxed_local()
        },
    )
    .await;
}

// A simplified version of an `fnet_interfaces_ext::Event` for
// tracking only added and removed events.
#[derive(Debug)]
enum InterfaceWatcherEvent {
    Added { id: u64, name: String },
    Removed { id: u64 },
}

/// Tests that when two interfaces are added with the same PersistentIdentifier,
/// that the first interface is removed prior to adding the second interface.
#[netstack_test]
async fn test_oir_interface_name_conflict_uninstall_existing<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Empty,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");

    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None);

    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let interfaces_stream = fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
        &interface_state,
        fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
    )
    .expect("get interface event stream")
    .map(|r| r.expect("watcher error"))
    .filter_map(|event| {
        futures::future::ready(match event {
            fidl_fuchsia_net_interfaces::Event::Added(
                fidl_fuchsia_net_interfaces::Properties { id, name, .. },
            )
            | fidl_fuchsia_net_interfaces::Event::Existing(
                fidl_fuchsia_net_interfaces::Properties { id, name, .. },
            ) => Some(InterfaceWatcherEvent::Added {
                id: id.expect("missing interface ID"),
                name: name.expect("missing interface name"),
            }),
            fidl_fuchsia_net_interfaces::Event::Removed(id) => {
                Some(InterfaceWatcherEvent::Removed { id })
            }
            fidl_fuchsia_net_interfaces::Event::Idle(fidl_fuchsia_net_interfaces::Empty {})
            | fidl_fuchsia_net_interfaces::Event::Changed(
                fidl_fuchsia_net_interfaces::Properties { .. },
            ) => None,
        })
    });
    let interfaces_stream = futures::stream::select(
        interfaces_stream,
        futures::stream::once(wait_for_netmgr.map(|r| panic!("network manager exited {:?}", r))),
    )
    .fuse();
    let mut interfaces_stream = pin!(interfaces_stream);
    // Observe the initially existing loopback interface.
    assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Added { id: _, name: _ }
    );

    // Add a device to the realm and wait for it to be added to the netstack.
    //
    // Devices get their interface names from their MAC addresses. Using the
    // same MAC address for different devices will result in the first
    // interface being removed prior to installing the new one.
    let mac = || Some(fnet::MacAddress { octets: [2, 3, 4, 5, 6, 7] });
    let if1 = sandbox
        .create_endpoint_with("ep1", netemul::new_endpoint_config(netemul::DEFAULT_MTU, mac()))
        .await
        .expect("create ethx7");
    let endpoint_mount_path = netemul::devfs_device_path("ep1");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    realm.add_virtual_device(&if1, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device1 {}: {:?}", endpoint_mount_path.display(), e)
    });

    let (id1, name1) = assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Added { id, name } => (id, name)
    );
    assert_eq!(
        &name1, "ethx7",
        "first interface should use a stable name based on its MAC address"
    );

    // Add another device from the network manager with the same MAC address and wait for it
    // to be added to the netstack. Since the device has the same naming identifier, the
    // first interface should be removed prior to adding the second interface. The second
    // interface should have the same name as the first.
    let if2 = sandbox
        .create_endpoint_with("ep2", netemul::new_endpoint_config(netemul::DEFAULT_MTU, mac()))
        .await
        .expect("create ethx7");
    let endpoint_mount_path = netemul::devfs_device_path("ep2");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    realm.add_virtual_device(&if2, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device2 {}: {:?}", endpoint_mount_path.display(), e)
    });

    let id_removed = assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Removed { id } => id
    );
    assert_eq!(
        id_removed, id1,
        "the initial interface should be removed prior to adding the second interface"
    );

    let (_id2, name2) = assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Added { id, name } => (id, name)
    );
    assert_eq!(
        &name1, &name2,
        "second interface should use the same name as the initial interface"
    );

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    realm.shutdown().await.expect("failed to shutdown realm");
}

/// Tests that when a conflicting interface already exists with the same name,
/// that the new interface is rejected by Netcfg and not installed.
/// The conflicting interface is either an interface installed through Netstack
/// directly and not managed by Netcfg, or managed by Netcfg with a
/// different naming identifier and the same name.
#[netstack_test]
#[test_case(true; "netcfg_managed")]
#[test_case(false; "not_netcfg_managed")]
async fn test_oir_interface_name_conflict_reject<M: Manager, N: Netstack>(
    name: &str,
    is_conflicting_iface_netcfg_managed: bool,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::DuplicateNames,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");

    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None);

    let interface_state = realm
        .connect_to_protocol::<fnet_interfaces::StateMarker>()
        .expect("connect to fuchsia.net.interfaces/State service");
    let interfaces_stream = fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
        &interface_state,
        fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
    )
    .expect("get interface event stream")
    .map(|r| r.expect("watcher error"))
    .filter_map(|event| {
        futures::future::ready(match event {
            fidl_fuchsia_net_interfaces::Event::Added(
                fidl_fuchsia_net_interfaces::Properties { id, name, .. },
            )
            | fidl_fuchsia_net_interfaces::Event::Existing(
                fidl_fuchsia_net_interfaces::Properties { id, name, .. },
            ) => Some(InterfaceWatcherEvent::Added {
                id: id.expect("missing interface ID"),
                name: name.expect("missing interface name"),
            }),
            fidl_fuchsia_net_interfaces::Event::Removed(id) => {
                Some(InterfaceWatcherEvent::Removed { id })
            }
            fidl_fuchsia_net_interfaces::Event::Idle(fidl_fuchsia_net_interfaces::Empty {})
            | fidl_fuchsia_net_interfaces::Event::Changed(
                fidl_fuchsia_net_interfaces::Properties { .. },
            ) => None,
        })
    });
    let interfaces_stream = futures::stream::select(
        interfaces_stream,
        futures::stream::once(wait_for_netmgr.map(|r| panic!("network manager exited {:?}", r))),
    )
    .fuse();
    let mut interfaces_stream = pin!(interfaces_stream);
    // Observe the initially existing loopback interface.
    assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Added { id: _, name: _ }
    );

    // All interfaces installed through the Netcfg should have a name of "x".
    const INTERFACE_NAME: &str = "x";
    let _if1 = if is_conflicting_iface_netcfg_managed {
        // Add a device to the realm for it to be discovered by Netcfg
        // then added to the netstack.
        const MAC1: Option<fnet::MacAddress> =
            Some(fnet::MacAddress { octets: [2, 3, 4, 5, 6, 8] });
        let if1 = sandbox
            .create_endpoint_with("ep1", netemul::new_endpoint_config(netemul::DEFAULT_MTU, MAC1))
            .await
            .expect("create x");
        let endpoint_mount_path = netemul::devfs_device_path("ep1");
        let endpoint_mount_path = endpoint_mount_path.as_path();
        realm.add_virtual_device(&if1, endpoint_mount_path).await.unwrap_or_else(|e| {
            panic!("add virtual device1 {}: {:?}", endpoint_mount_path.display(), e)
        });
        either::Either::Left(if1)
    } else {
        // Create an interface that the network manager does not know about that will conflict
        // with the name generated for the second device.
        let if1 = sandbox.create_endpoint(INTERFACE_NAME).await.expect("create x");
        let if1 = if1
            .into_interface_in_realm_with_name(
                &realm,
                netemul::InterfaceConfig {
                    name: Some(INTERFACE_NAME.into()),
                    ..Default::default()
                },
            )
            .await
            .expect("install interface");
        either::Either::Right(if1)
    };

    // The device should have been installed into the Netstack.
    assert_matches!(
        interfaces_stream.select_next_some().await,
        InterfaceWatcherEvent::Added { name, .. } if name == INTERFACE_NAME,
        "first interface should use the configuration based static name"
    );

    // The following device should attempt to use the same name as the previous interface.
    const MAC2: Option<fnet::MacAddress> = Some(fnet::MacAddress { octets: [2, 3, 4, 5, 6, 7] });
    let if2 = sandbox
        .create_endpoint_with("ep2", netemul::new_endpoint_config(netemul::DEFAULT_MTU, MAC2))
        .await
        .expect("create x");
    let endpoint_mount_path = netemul::devfs_device_path("ep2");
    let endpoint_mount_path = endpoint_mount_path.as_path();
    realm.add_virtual_device(&if2, endpoint_mount_path).await.unwrap_or_else(|e| {
        panic!("add virtual device2 {}: {:?}", endpoint_mount_path.display(), e)
    });

    // No interfaces should be added or removed as a result of this new device.
    // The existing interface that is not managed by Netcfg will not be removed
    // since it is not Netcfg-managed. The interface that is managed by
    // Netcfg will not be removed due to having a different MAC address, which
    // produces a different naming identifier.
    assert_matches::assert_matches!(
        interfaces_stream.next().on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None).await,
        None
    );

    // Wait for orderly shutdown of the test realm to complete before allowing
    // test interfaces to be cleaned up.
    //
    // This is necessary to prevent test interfaces from being removed while
    // NetCfg is still in the process of configuring them after adding them to
    // the Netstack, which causes spurious errors.
    realm.shutdown().await.expect("failed to shutdown realm");
}

/// Make sure the DHCP server is configured to start serving requests when NetCfg discovers
/// a WLAN AP interface and stops serving when the interface is removed.
///
/// Also make sure that a new WLAN AP interface may be added after a previous interface has been
/// removed from the netstack.
#[netstack_test]
async fn test_wlan_ap_dhcp_server<M: Manager, N: Netstack>(name: &str) {
    // Use a large timeout to check for resolution.
    //
    // These values effectively result in a large timeout of 60s which should avoid
    // flakes. This test was run locally 100 times without flakes.
    /// Duration to sleep between polls.
    const POLL_WAIT: zx::Duration = zx::Duration::from_seconds(1);
    /// Maximum number of times we'll poll the DHCP server to check its parameters.
    const RETRY_COUNT: u64 = 120;

    /// Check if the DHCP server is started.
    async fn check_dhcp_status(dhcp_server: &fnet_dhcp::Server_Proxy, started: bool) {
        for _ in 0..RETRY_COUNT {
            fuchsia_async::Timer::new(POLL_WAIT.after_now()).await;

            if started == dhcp_server.is_serving().await.expect("query server status request") {
                return;
            }
        }

        panic!("timed out checking DHCP server status");
    }

    /// Make sure the DHCP server is configured to start serving requests when NetCfg discovers
    /// a WLAN AP interface and stops serving when the interface is removed.
    ///
    /// When `wlan_ap_dhcp_server_inner` returns successfully, the interface that it creates will
    /// have been removed.
    async fn wlan_ap_dhcp_server_inner<'a>(
        sandbox: &'a netemul::TestSandbox,
        realm: &netemul::TestRealm<'a>,
        offset: u8,
    ) {
        // These constants are all hard coded in NetCfg for the WLAN AP interface and
        // the DHCP server.
        const DHCP_LEASE_TIME: u32 = 24 * 60 * 60; // 1 day in seconds.
        const NETWORK_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.248");
        const NETWORK_PREFIX_LEN: u8 = 29;
        const INTERFACE_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.249");
        const DHCP_POOL_START_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.250");
        const DHCP_POOL_END_ADDR: fnet::Ipv4Address = fidl_ip_v4!("192.168.255.254");
        const NETWORK_ADDR_SUBNET: net_types_ip::Subnet<net_types_ip::Ipv4Addr> = unsafe {
            net_types_ip::Subnet::new_unchecked(
                net_types_ip::Ipv4Addr::new(NETWORK_ADDR.addr),
                NETWORK_PREFIX_LEN,
            )
        };

        // Add a device to the realm that looks like a WLAN AP from the
        // perspective of NetCfg.
        let network = sandbox
            .create_network(format!("dhcp-server-{}", offset))
            .await
            .expect("create network");
        let wlan_ap = network
            .create_endpoint_with(
                format!("wlanif-ap-dhcp-server-{}", offset),
                fnetemul_network::EndpointConfig {
                    mtu: netemul::DEFAULT_MTU,
                    mac: None,
                    port_class: fhardware_network::PortClass::WlanAp,
                },
            )
            .await
            .expect("create wlan ap");
        let path = netemul::devfs_device_path(&format!("dhcp-server-ep-{}", offset));
        realm
            .add_virtual_device(&wlan_ap, path.as_path())
            .await
            .unwrap_or_else(|e| panic!("add WLAN AP virtual device {}: {:?}", path.display(), e));
        wlan_ap.set_link_up(true).await.expect("set wlan ap link up");

        // Make sure the WLAN AP interface is added to the Netstack and is brought up with
        // the right IP address.
        let interface_state = realm
            .connect_to_protocol::<fnet_interfaces::StateMarker>()
            .expect("connect to fuchsia.net.interfaces/State service");
        let event_stream = fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
            &interface_state,
            fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .expect("get interface event stream");
        let mut event_stream = pin!(event_stream);
        let mut if_map =
            HashMap::<u64, fidl_fuchsia_net_interfaces_ext::PropertiesAndState<()>>::new();
        let (wlan_ap_id, wlan_ap_name) = fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| {
                if_map.iter().find_map(
                    |(
                        id,
                        fidl_fuchsia_net_interfaces_ext::PropertiesAndState {
                            properties:
                                fidl_fuchsia_net_interfaces_ext::Properties {
                                    name,
                                    online,
                                    addresses,
                                    ..
                                },
                            state: _,
                        },
                    )| {
                        (*online
                            && addresses.iter().any(
                                |&fidl_fuchsia_net_interfaces_ext::Address {
                                     addr: fnet::Subnet { addr, prefix_len: _ },
                                     valid_until: _,
                                     assignment_state,
                                 }| {
                                    assert_eq!(
                                        assignment_state,
                                        fnet_interfaces::AddressAssignmentState::Assigned
                                    );
                                    addr == INTERFACE_ADDR.into_ext()
                                },
                            ))
                        .then(|| (*id, name.clone()))
                    },
                )
            },
        )
        .map_err(anyhow::Error::from)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("timed out"))
        })
        .await
        .expect("failed to wait for presence of a WLAN AP interface");

        // Check the DHCP server's configured parameters.
        let dhcp_server = realm
            .connect_to_protocol::<fnet_dhcp::Server_Marker>()
            .expect("connect to DHCP server service");
        let checks = [
            (
                fnet_dhcp::ParameterName::IpAddrs,
                fnet_dhcp::Parameter::IpAddrs(vec![INTERFACE_ADDR]),
            ),
            (
                fnet_dhcp::ParameterName::LeaseLength,
                fnet_dhcp::Parameter::Lease(fnet_dhcp::LeaseLength {
                    default: Some(DHCP_LEASE_TIME),
                    max: Some(DHCP_LEASE_TIME),
                    ..Default::default()
                }),
            ),
            (
                fnet_dhcp::ParameterName::BoundDeviceNames,
                fnet_dhcp::Parameter::BoundDeviceNames(vec![wlan_ap_name]),
            ),
            (
                fnet_dhcp::ParameterName::AddressPool,
                fnet_dhcp::Parameter::AddressPool(fnet_dhcp::AddressPool {
                    prefix_length: Some(NETWORK_PREFIX_LEN),
                    range_start: Some(DHCP_POOL_START_ADDR),
                    range_stop: Some(DHCP_POOL_END_ADDR),
                    ..Default::default()
                }),
            ),
        ];

        let dhcp_server_ref = &dhcp_server;
        let checks_ref = &checks;
        if !try_any(stream::iter(0..RETRY_COUNT).then(|i| async move {
            fuchsia_async::Timer::new(POLL_WAIT.after_now()).await;
            try_all(stream::iter(checks_ref.iter()).then(|(param_name, param_value)| async move {
                Ok(dhcp_server_ref
                    .get_parameter(*param_name)
                    .await
                    .unwrap_or_else(|e| panic!("get {:?} parameter request: {:?}", param_name, e))
                    .unwrap_or_else(|e| {
                        panic!(
                            "error getting {:?} parameter: {}",
                            param_name,
                            zx::Status::from_raw(e)
                        )
                    })
                    == *param_value)
            }))
            .await
            .with_context(|| format!("{}-th iteration checking DHCP parameters", i))
        }))
        .await
        .expect("checking DHCP parameters")
        {
            // Too many retries.
            panic!("timed out waiting for DHCP server configurations");
        }

        // The DHCP server should be started.
        check_dhcp_status(&dhcp_server, true).await;

        // Add a host endpoint to the network. It should be configured by the DHCP server.
        let host = network
            .create_endpoint(format!("host-dhcp-client-{}", offset))
            .await
            .expect("create host");
        let path = netemul::devfs_device_path(&format!("dhcp-client-ep-{}", offset));
        realm
            .add_virtual_device(&host, path.as_path())
            .await
            .unwrap_or_else(|e| panic!("add host virtual device {}: {:?}", path.display(), e));
        host.set_link_up(true).await.expect("set host link up");
        let host_id = fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| {
                if_map.iter().find_map(
                    |(
                        id,
                        fidl_fuchsia_net_interfaces_ext::PropertiesAndState {
                            properties:
                                fidl_fuchsia_net_interfaces_ext::Properties {
                                    online, addresses, ..
                                },
                            state: _,
                        },
                    )| {
                        (*id != wlan_ap_id
                            && *online
                            && addresses.iter().any(
                                |&fidl_fuchsia_net_interfaces_ext::Address {
                                     addr: fnet::Subnet { addr, prefix_len: _ },
                                     valid_until: _,
                                     assignment_state,
                                 }| {
                                    assert_eq!(
                                        assignment_state,
                                        fnet_interfaces::AddressAssignmentState::Assigned
                                    );
                                    match addr {
                                        fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr }) => {
                                            NETWORK_ADDR_SUBNET
                                                .contains(&net_types_ip::Ipv4Addr::new(addr))
                                        }
                                        fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: _ }) => {
                                            false
                                        }
                                    }
                                },
                            ))
                        .then_some(*id)
                    },
                )
            },
        )
        .map_err(anyhow::Error::from)
        .on_timeout(ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("timed out"))
        })
        .await
        .expect("wait for host interface to be configured");

        // Take the interface down, the DHCP server should be stopped.
        wlan_ap.set_link_up(false).await.expect("set wlan ap link down");
        check_dhcp_status(&dhcp_server, false).await;

        // Bring the interface back up, the DHCP server should be started.
        wlan_ap.set_link_up(true).await.expect("set wlan ap link up");
        check_dhcp_status(&dhcp_server, true).await;
        // Remove the interface, the DHCP server should be stopped.
        drop(wlan_ap);
        check_dhcp_status(&dhcp_server, false).await;

        // Remove the host endpoint from the network and wait for the interface
        // to be removed from the netstack.
        //
        // This is necessary to ensure this function can be called multiple
        // times and observe DHCP address acquisition on a new interface each
        // time.
        drop(host);
        fidl_fuchsia_net_interfaces_ext::wait_interface(
            event_stream.by_ref(),
            &mut if_map,
            |if_map| (!if_map.contains_key(&host_id)).then_some(()),
        )
        .await
        .expect("wait for host interface to be removed");
    }

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Empty,
                    use_dhcp_server: true,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::DhcpServer { persistent: false },
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("create netstack realm");
    let wait_for_netmgr =
        wait_for_component_stopped(&realm, M::MANAGEMENT_AGENT.get_component_name(), None).fuse();
    let mut wait_for_netmgr = pin!(wait_for_netmgr);

    // Add a WLAN AP, make sure the DHCP server gets configured and starts or
    // stops when the interface is added and brought up or brought down/removed.
    // A loop is used to emulate interface flaps.
    for i in 0..=1 {
        let test_fut = wlan_ap_dhcp_server_inner(&sandbox, &realm, i).fuse();
        let mut test_fut = pin!(test_fut);
        futures::select! {
            () = test_fut => {},
            stopped_event = wait_for_netmgr => {
                panic!(
                    "NetCfg unexpectedly exited with exit status = {:?}",
                    stopped_event
                );
            }
        };
    }
}

/// Tests that netcfg observes component stop events and exits cleanly.
#[netstack_test]
async fn observes_stop_events<M: Manager, N: Netstack>(name: &str) {
    use component_events::events::{self};

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Empty,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");
    let mut event_stream = events::EventStream::open_at_path("/events/started_stopped")
        .await
        .expect("subscribe to events");

    let event_matcher = netstack_testing_common::get_child_component_event_matcher(
        &realm,
        M::MANAGEMENT_AGENT.get_component_name(),
    )
    .await
    .expect("get child moniker");

    // Wait for netcfg to start.
    let events::StartedPayload {} = event_matcher
        .clone()
        .wait::<events::Started>(&mut event_stream)
        .await
        .expect("got started event")
        .result()
        .expect("error event on started");

    realm.shutdown().await.expect("shutdown");

    let event =
        event_matcher.wait::<events::Stopped>(&mut event_stream).await.expect("got stopped event");
    // NB: event::result below borrows from event, it needs to be in a different
    // statement.
    let events::StoppedPayload { status } = event.result().expect("error event on stopped");
    assert_matches::assert_matches!(status, events::ExitStatus::Clean);
}

/// Test that NetCfg enables forwarding on interfaces when the device class is configured to have
/// that enabled.
#[netstack_test]
async fn test_forwarding<M: Manager, N: Netstack>(name: &str) {
    let _if_name: String = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Forwarding,
        false, /* use_out_of_stack_dhcp_client */
        [],
        |if_id,
         _: &netemul::TestNetwork<'_>,
         _: &fnet_interfaces::StateProxy,
         realm,
         _test_sandbox| {
            async move {
                let control = realm
                    .interface_control(if_id)
                    .expect("connect to fuchsia.net.interfaces.admin/Control for new interface");

                let fnet_interfaces_admin::Configuration { ipv4, ipv6, .. } = control
                    .get_configuration()
                    .await
                    .expect("get_configuration FIDL error")
                    .expect("get_configuration error");
                let ipv4 = ipv4.expect("missing ipv4");
                let ipv6 = ipv6.expect("missing ipv6");
                // The configuration installs forwarding on v4 on Virtual
                // interfaces and v6 on Ethernet. We should only observe the
                // configuration to be installed on v4 because the device
                // installed by this test doesn't match the Ethernet device
                // class.
                assert_eq!(ipv4.forwarding, Some(true));
                assert_eq!(ipv4.multicast_forwarding, Some(true));
                assert_eq!(ipv6.forwarding, Some(false));
                assert_eq!(ipv6.multicast_forwarding, Some(false));
            }
            .boxed_local()
        },
    )
    .await;
}

#[netstack_test]
async fn test_prefix_provider_not_supported<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Empty,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    // Attempt to Acquire a prefix when DHCPv6 is not supported (DHCPv6 client
    // is not made available to netcfg).
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider
        .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
        .expect("acquire prefix");
    assert_eq!(
        prefix_control
            .take_event_stream()
            .map_ok(fnet_dhcpv6::PrefixControlEvent::into_on_exit)
            .try_collect::<Vec<_>>()
            .await
            .expect("collect event stream")[..],
        [Some(fnet_dhcpv6::PrefixControlExitReason::NotSupported)],
    );
}

// TODO(https://fxbug.dev/42065403): Remove this test when multiple clients
// requesting prefixes is supported.
#[netstack_test]
async fn test_prefix_provider_already_acquiring<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Dhcpv6,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    {
        // Acquire a prefix.
        let (_prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
        prefix_provider
            .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
            .expect("acquire prefix");

        // Calling acquire_prefix a second time results in ALREADY_ACQUIRING.
        {
            let (prefix_control, server_end) =
                fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                    .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
            prefix_provider
                .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
                .expect("acquire prefix");
            let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
                .take_event_stream()
                .try_next()
                .await
                .expect("next PrefixControl event")
                .expect("PrefixControl event stream ended");
            assert_eq!(reason, fnet_dhcpv6::PrefixControlExitReason::AlreadyAcquiring);
        }

        // The PrefixControl channel is dropped here.
    }

    // Retry acquire_prefix in a loop (server may take some time to notice PrefixControl
    // closure) and expect that it succeeds eventually.
    loop {
        let (prefix_control, server_end) =
            fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
        prefix_provider
            .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
            .expect("acquire prefix");
        match prefix_control
            .take_event_stream()
            .next()
            .map(Some)
            .on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, || None)
            .await
        {
            None => {
                assert!(!prefix_control.is_closed());
                break;
            }
            Some(item) => {
                assert_matches::assert_matches!(
                    item,
                    Some(Ok(fnet_dhcpv6::PrefixControlEvent::OnExit {
                        reason: fnet_dhcpv6::PrefixControlExitReason::AlreadyAcquiring,
                    }))
                );
            }
        }
    }
}

#[netstack_test]
#[test_case(
    fnet_dhcpv6::AcquirePrefixConfig {
        interface_id: Some(42),
        ..Default::default()
    },
    fnet_dhcpv6::PrefixControlExitReason::InvalidInterface;
    "interface not found"
)]
#[test_case(
    fnet_dhcpv6::AcquirePrefixConfig {
        preferred_prefix_len: Some(129),
        ..Default::default()
    },
    fnet_dhcpv6::PrefixControlExitReason::InvalidPrefixLength;
    "invalid prefix length"
)]
async fn test_prefix_provider_config_error<M: Manager, N: Netstack>(
    name: &str,
    config: fnet_dhcpv6::AcquirePrefixConfig,
    want_reason: fnet_dhcpv6::PrefixControlExitReason,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Dhcpv6,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider.acquire_prefix(&config, server_end).expect("acquire prefix");
    let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
        .take_event_stream()
        .try_next()
        .await
        .expect("next PrefixControl event")
        .expect("PrefixControl event stream ended");
    assert_eq!(reason, want_reason);
}

#[netstack_test]
async fn test_prefix_provider_double_watch<M: Manager, N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            name,
            &[
                KnownServiceProvider::Manager {
                    agent: M::MANAGEMENT_AGENT,
                    config: ManagerConfig::Dhcpv6,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::Dhcpv6Client,
            ],
        )
        .expect("create netstack realm");

    let prefix_provider = realm
        .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
        .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
    // Acquire a prefix.
    let (prefix_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
            .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
    prefix_provider
        .acquire_prefix(&fnet_dhcpv6::AcquirePrefixConfig::default(), server_end)
        .expect("acquire prefix");

    let (res1, res2) =
        futures::future::join(prefix_control.watch_prefix(), prefix_control.watch_prefix()).await;
    for res in [res1, res2] {
        assert_matches::assert_matches!(res, Err(fidl::Error::ClientChannelClosed { status, .. }) => {
            assert_eq!(status, zx::Status::PEER_CLOSED);
        });
    }
    let fnet_dhcpv6::PrefixControlEvent::OnExit { reason } = prefix_control
        .take_event_stream()
        .try_next()
        .await
        .expect("next PrefixControl event")
        .expect("PrefixControl event stream ended");
    assert_eq!(reason, fnet_dhcpv6::PrefixControlExitReason::DoubleWatch);

    // TODO(https://fxbug.dev/42153903): Cannot expected `is_closed` to return true
    // even though PEER_CLOSED has already been observed on the channel.
    assert_eq!(prefix_control.on_closed().await, Ok(zx::Signals::CHANNEL_PEER_CLOSED));
    assert!(prefix_control.is_closed());
}

mod dhcpv6_helper {
    use super::*;

    pub(crate) const SERVER_ADDR: net_types_ip::Ipv6Addr = net_ip_v6!("fe80::5122");
    pub(crate) const SERVER_ID: [u8; 3] = [2, 5, 1];
    pub(crate) const PREFIX: net_types_ip::Subnet<net_types_ip::Ipv6Addr> =
        net_subnet_v6!("a::/64");
    pub(crate) const RENEWED_PREFIX: net_types_ip::Subnet<net_types_ip::Ipv6Addr> =
        net_subnet_v6!("b::/64");
    pub(crate) const DHCPV6_CLIENT_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(546));
    pub(crate) const DHCPV6_SERVER_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(547));
    pub(crate) const INFINITE_TIME_VALUE: u32 = u32::MAX;
    pub(crate) const ONE_SECOND_TIME_VALUE: u32 = 1;
    // The DHCPv6 Client always sends IAs with the first IAID starting at 0.
    pub(crate) const EXPECTED_IAID: dhcpv6::IAID = dhcpv6::IAID::new(0);

    pub(crate) struct Dhcpv6ClientMessage {
        pub(crate) tx_id: [u8; 3],
        pub(crate) client_id: Vec<u8>,
    }

    pub(crate) async fn send_dhcpv6_message(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        client_addr: net_types_ip::Ipv6Addr,
        prefix: Option<net_types_ip::Subnet<net_types_ip::Ipv6Addr>>,
        invalidated_prefix: Option<net_types_ip::Subnet<net_types_ip::Ipv6Addr>>,
        tx_id: [u8; 3],
        msg_type: dhcpv6::MessageType,
        client_id: Vec<u8>,
    ) {
        let iaprefix_options = prefix
            .into_iter()
            .map(|prefix| {
                dhcpv6::DhcpOption::IaPrefix(dhcpv6::IaPrefixSerializer::new(
                    INFINITE_TIME_VALUE,
                    INFINITE_TIME_VALUE,
                    prefix,
                    &[],
                ))
            })
            .chain(invalidated_prefix.into_iter().map(|prefix| {
                dhcpv6::DhcpOption::IaPrefix(dhcpv6::IaPrefixSerializer::new(0, 0, prefix, &[]))
            }))
            .collect::<Vec<_>>();

        let options = [
            dhcpv6::DhcpOption::ServerId(&SERVER_ID),
            dhcpv6::DhcpOption::ClientId(&client_id),
            dhcpv6::DhcpOption::IaPd(dhcpv6::IaPdSerializer::new(
                EXPECTED_IAID,
                ONE_SECOND_TIME_VALUE,
                INFINITE_TIME_VALUE,
                iaprefix_options.as_ref(),
            )),
        ]
        .into_iter()
        // If this is an Advertise message, include a preference option with
        // the maximum preference value so that clients stop server discovery
        // and use this server immediately.
        .chain(
            (msg_type == dhcpv6::MessageType::Advertise)
                .then_some(dhcpv6::DhcpOption::Preference(u8::MAX)),
        )
        .collect::<Vec<_>>();

        let buf: packet::Either<EmptyBuf, _> =
            dhcpv6::MessageBuilder::new(msg_type, tx_id, &options)
                .into_serializer()
                .encapsulate(UdpPacketBuilder::new(
                    SERVER_ADDR,
                    client_addr,
                    Some(DHCPV6_SERVER_PORT),
                    DHCPV6_CLIENT_PORT,
                ))
                .encapsulate(Ipv6PacketBuilder::new(
                    SERVER_ADDR,
                    client_addr,
                    64, /* ttl */
                    Ipv6Proto::Proto(IpProto::Udp),
                ))
                .encapsulate(EthernetFrameBuilder::new(
                    Mac::BROADCAST,
                    Mac::BROADCAST,
                    EtherType::Ipv6,
                    ETHERNET_MIN_BODY_LEN_NO_TAG,
                ))
                .serialize_vec_outer()
                .expect("error serializing dhcpv6 packet");

        let () =
            fake_ep.write(buf.unwrap_b().as_ref()).await.expect("error sending dhcpv6 message");
    }

    pub(crate) async fn wait_for_message(
        fake_ep: &netemul::TestFakeEndpoint<'_>,
        expected_src_ip: net_types_ip::Ipv6Addr,
        want_msg_type: dhcpv6::MessageType,
    ) -> Dhcpv6ClientMessage {
        let stream = fake_ep
            .frame_stream()
            .map(|r| r.expect("error getting OnData event"))
            .filter_map(|(data, dropped)| {
                async move {
                    assert_eq!(dropped, 0);
                    let mut data = &data[..];

                    let eth = EthernetFrame::parse(&mut data, EthernetFrameLengthCheck::NoCheck)
                        .expect("error parsing ethernet frame");

                    if eth.ethertype() != Some(EtherType::Ipv6) {
                        // Ignore non-IPv6 packets.
                        return None;
                    }

                    let (mut payload, src_ip, dst_ip, proto, _ttl) =
                        parse_ip_packet::<net_types_ip::Ipv6>(&data)
                            .expect("error parsing IPv6 packet");
                    if src_ip != expected_src_ip {
                        return None;
                    }

                    if proto != Ipv6Proto::Proto(IpProto::Udp) {
                        // Ignore non-UDP packets.
                        return None;
                    }

                    let udp = UdpPacket::parse(&mut payload, UdpParseArgs::new(src_ip, dst_ip))
                        .expect("error parsing ICMPv6 packet");
                    if udp.src_port() != Some(DHCPV6_CLIENT_PORT)
                        || udp.dst_port() != DHCPV6_SERVER_PORT
                    {
                        // Ignore packets with non-DHCPv6 ports.
                        return None;
                    }

                    let dhcpv6 = dhcpv6::Message::parse(&mut payload, ())
                        .expect("error parsing DHCPv6 message");

                    if dhcpv6.msg_type() != want_msg_type {
                        return None;
                    }

                    let mut client_id = None;
                    let mut saw_ia_pd = false;
                    for opt in dhcpv6.options() {
                        match opt {
                            dhcpv6::ParsedDhcpOption::ClientId(id) => {
                                assert_eq!(
                                    core::mem::replace(&mut client_id, Some(id.to_vec())),
                                    None
                                )
                            }
                            dhcpv6::ParsedDhcpOption::IaPd(iapd) => {
                                assert_eq!(iapd.iaid(), EXPECTED_IAID.get());
                                assert!(!saw_ia_pd);
                                saw_ia_pd = true;
                            }
                            _ => {}
                        }
                    }
                    assert!(saw_ia_pd);

                    Some(Dhcpv6ClientMessage {
                        tx_id: *dhcpv6.transaction_id(),
                        client_id: client_id.unwrap(),
                    })
                }
            });

        let mut stream = pin!(stream);
        stream.next().await.expect("expected DHCPv6 message")
    }
}

#[netstack_test]
async fn test_prefix_provider_full_integration<M: Manager, N: Netstack>(name: &str) {
    let _if_name: String = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Dhcpv6,
        false, /* use_out_of_stack_dhcp_client */
        [KnownServiceProvider::Dhcpv6Client],
        |if_id, network, interface_state, realm, _sandbox| {
            async move {
                // Fake endpoint to inject server packets and intercept client packets.
                let fake_ep = network.create_fake_endpoint().expect("create fake endpoint");

                // Request Prefixes to be acquired.
                let prefix_provider = realm
                    .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
                    .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
                let (prefix_control, server_end) =
                    fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                        .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
                prefix_provider
                    .acquire_prefix(
                        &fnet_dhcpv6::AcquirePrefixConfig {
                            interface_id: Some(if_id),
                            ..Default::default()
                        },
                        server_end,
                    )
                    .expect("acquire prefix");

                let if_ll_addr = interfaces::wait_for_v6_ll(interface_state, if_id)
                    .await
                    .expect("error waiting for link-local address");
                let fake_ep = &fake_ep;

                // Perform the prefix negotiation.
                for (expected, send) in [
                    (dhcpv6::MessageType::Solicit, dhcpv6::MessageType::Advertise),
                    (dhcpv6::MessageType::Request, dhcpv6::MessageType::Reply),
                ] {
                    let dhcpv6_helper::Dhcpv6ClientMessage { tx_id, client_id } =
                        dhcpv6_helper::wait_for_message(&fake_ep, if_ll_addr, expected).await;
                    dhcpv6_helper::send_dhcpv6_message(
                        &fake_ep,
                        if_ll_addr,
                        Some(dhcpv6_helper::PREFIX),
                        None,
                        tx_id,
                        send,
                        client_id,
                    )
                    .await;
                }
                assert_eq!(
                    prefix_control.watch_prefix().await.expect("error watching prefix"),
                    fnet_dhcpv6::PrefixEvent::Assigned(fnet_dhcpv6::Prefix {
                        prefix: fnet::Ipv6AddressWithPrefix {
                            addr: fnet::Ipv6Address {
                                addr: dhcpv6_helper::PREFIX.network().ipv6_bytes()
                            },
                            prefix_len: dhcpv6_helper::PREFIX.prefix(),
                        },
                        lifetimes: fnet_dhcpv6::Lifetimes {
                            valid_until: zx::Time::INFINITE.into_nanos(),
                            preferred_until: zx::Time::INFINITE.into_nanos(),
                        },
                    }),
                );

                for (new_prefix, old_prefix, res) in [
                    // Renew the IA with a new prefix and invalidate the old prefix.
                    (
                        Some(dhcpv6_helper::RENEWED_PREFIX),
                        Some(dhcpv6_helper::PREFIX),
                        fnet_dhcpv6::PrefixEvent::Assigned(fnet_dhcpv6::Prefix {
                            prefix: fnet::Ipv6AddressWithPrefix {
                                addr: fnet::Ipv6Address {
                                    addr: dhcpv6_helper::RENEWED_PREFIX.network().ipv6_bytes(),
                                },
                                prefix_len: dhcpv6_helper::RENEWED_PREFIX.prefix(),
                            },
                            lifetimes: fnet_dhcpv6::Lifetimes {
                                valid_until: zx::Time::INFINITE.into_nanos(),
                                preferred_until: zx::Time::INFINITE.into_nanos(),
                            },
                        }),
                    ),
                    // Invalidate the prefix.
                    (
                        None,
                        Some(dhcpv6_helper::RENEWED_PREFIX),
                        fnet_dhcpv6::PrefixEvent::Unassigned(fnet_dhcpv6::Empty {}),
                    ),
                ] {
                    let dhcpv6_helper::Dhcpv6ClientMessage { tx_id, client_id } =
                        dhcpv6_helper::wait_for_message(
                            &fake_ep,
                            if_ll_addr,
                            dhcpv6::MessageType::Renew,
                        )
                        .await;
                    dhcpv6_helper::send_dhcpv6_message(
                        &fake_ep,
                        if_ll_addr,
                        new_prefix,
                        old_prefix,
                        tx_id,
                        dhcpv6::MessageType::Reply,
                        client_id,
                    )
                    .await;
                    assert_eq!(
                        prefix_control.watch_prefix().await.expect("error watching prefix"),
                        res,
                    );
                }
            }
            .boxed_local()
        },
    )
    .await;
}

// Regression test for https://fxbug.dev/335892036, in which netcfg panicked if an interface was
// disabled while it was holding a DHCPv6 prefix for it.
#[netstack_test]
async fn disable_interface_while_having_dhcpv6_prefix<M: Manager, N: Netstack>(name: &str) {
    let _if_name: String = with_netcfg_owned_device::<M, N, _>(
        name,
        ManagerConfig::Dhcpv6,
        true, /* use_out_of_stack_dhcp_client */
        [KnownServiceProvider::Dhcpv6Client, KnownServiceProvider::DhcpClient],
        |if_id, network, interface_state, realm, _sandbox| {
            async move {
                // Fake endpoint to inject server packets and intercept client packets.
                let fake_ep = network.create_fake_endpoint().expect("create fake endpoint");

                // Request Prefixes to be acquired.
                let prefix_provider = realm
                    .connect_to_protocol::<fnet_dhcpv6::PrefixProviderMarker>()
                    .expect("connect to fuchsia.net.dhcpv6/PrefixProvider server");
                let (prefix_control, server_end) =
                    fidl::endpoints::create_proxy::<fnet_dhcpv6::PrefixControlMarker>()
                        .expect("create fuchsia.net.dhcpv6/PrefixControl proxy and server end");
                prefix_provider
                    .acquire_prefix(
                        &fnet_dhcpv6::AcquirePrefixConfig {
                            interface_id: Some(if_id),
                            ..Default::default()
                        },
                        server_end,
                    )
                    .expect("acquire prefix");

                let if_ll_addr = interfaces::wait_for_v6_ll(interface_state, if_id)
                    .await
                    .expect("error waiting for link-local address");
                let fake_ep = &fake_ep;

                // Perform the prefix negotiation.
                for (expected, send) in [
                    (dhcpv6::MessageType::Solicit, dhcpv6::MessageType::Advertise),
                    (dhcpv6::MessageType::Request, dhcpv6::MessageType::Reply),
                ] {
                    let dhcpv6_helper::Dhcpv6ClientMessage { tx_id, client_id } =
                        dhcpv6_helper::wait_for_message(&fake_ep, if_ll_addr, expected).await;
                    dhcpv6_helper::send_dhcpv6_message(
                        &fake_ep,
                        if_ll_addr,
                        Some(dhcpv6_helper::PREFIX),
                        None,
                        tx_id,
                        send,
                        client_id,
                    )
                    .await;
                }
                assert_eq!(
                    prefix_control.watch_prefix().await.expect("error watching prefix"),
                    fnet_dhcpv6::PrefixEvent::Assigned(fnet_dhcpv6::Prefix {
                        prefix: fnet::Ipv6AddressWithPrefix {
                            addr: fnet::Ipv6Address {
                                addr: dhcpv6_helper::PREFIX.network().ipv6_bytes()
                            },
                            prefix_len: dhcpv6_helper::PREFIX.prefix(),
                        },
                        lifetimes: fnet_dhcpv6::Lifetimes {
                            valid_until: zx::Time::INFINITE.into_nanos(),
                            preferred_until: zx::Time::INFINITE.into_nanos(),
                        },
                    }),
                );

                let root_interfaces = realm
                    .connect_to_protocol::<fnet_root::InterfacesMarker>()
                    .expect("connect to fuchsia.net.root.Interfaces");
                let (control, server_end) =
                    fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
                        .expect("create proxy");
                root_interfaces.get_admin(if_id, server_end).expect("get admin");

                let mut interface_event_stream = Box::pin(
                    realm.get_interface_event_stream().expect("get interface event stream"),
                );

                let mut if_state = fnet_interfaces_ext::existing(
                    interface_event_stream.by_ref(),
                    fnet_interfaces_ext::InterfaceState::Unknown::<()>(if_id),
                )
                .await
                .expect("collect initial state of interface");

                let fnet_interfaces_ext::PropertiesAndState {
                    properties: fnet_interfaces_ext::Properties { online, .. },
                    state: (),
                } = assert_matches!(
                    &if_state,
                    fnet_interfaces_ext::InterfaceState::Known(properties) => properties
                );
                assert!(online, "interface should start out online before disabling");

                // When netcfg had the issue that this regression test covers,
                // it would panic while handling the interface-disabled event.
                // This manifests as a test failure due to error log severity.
                assert!(control
                    .disable()
                    .await
                    .expect("disable should not have FIDL error")
                    .expect("disable should succeed"));

                fnet_interfaces_ext::wait_interface_with_id(
                    interface_event_stream,
                    &mut if_state,
                    |fnet_interfaces_ext::PropertiesAndState { properties, state: () }| {
                        (!properties.online).then_some(())
                    },
                )
                .await
                .expect("wait for interface to go offline");
            }
            .boxed_local()
        },
    )
    .await;
}

struct MasqueradeTestSetup {
    client_ip: std::net::IpAddr,
    client_subnet: fnet::Subnet,
    client_gateway: fnet::IpAddress,
    server_ip: std::net::IpAddr,
    server_subnet: fnet::Subnet,
    server_gateway: fnet::IpAddress,
    router_ip: std::net::IpAddr,
    router_client_ip: fnet::Subnet,
    router_server_ip: fnet::Subnet,
    router_if_config: fnet_interfaces_admin::Configuration,
}

#[netstack_test]
#[test_case(
    MasqueradeTestSetup {
        client_ip: std_ip!("192.168.1.2"),
        client_subnet: fidl_subnet!("192.168.1.2/24"),
        client_gateway: fidl_ip!("192.168.1.1"),
        server_ip: std_ip!("192.168.0.2"),
        server_subnet: fidl_subnet!("192.168.0.2/24"),
        server_gateway: fidl_ip!("192.168.0.1"),
        router_ip: std_ip!("192.168.0.1"),
        router_client_ip: fidl_subnet!("192.168.1.1/24"),
        router_server_ip: fidl_subnet!("192.168.0.1/24"),
        router_if_config: fnet_interfaces_admin::Configuration {
            ipv4: Some(fnet_interfaces_admin::Ipv4Configuration {
                forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    "ipv4"
)]
#[test_case(
    MasqueradeTestSetup {
        client_ip: std_ip!("fd00:0:0:1::2"),
        client_subnet: fidl_subnet!("fd00:0:0:1::2/64"),
        client_gateway: fidl_ip!("fd00:0:0:1::1"),
        server_ip: std_ip!("fd00:0:0:2::2"),
        server_subnet: fidl_subnet!("fd00:0:0:2::2/64"),
        server_gateway: fidl_ip!("fd00:0:0:2::1"),
        router_ip: std_ip!("fd00:0:0:2::1"),
        router_client_ip: fidl_subnet!("fd00:0:0:1::1/64"),
        router_server_ip: fidl_subnet!("fd00:0:0:2::1/64"),
        router_if_config: fnet_interfaces_admin::Configuration {
            ipv6: Some(fnet_interfaces_admin::Ipv6Configuration {
                forwarding: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    "ipv6"
)]
async fn test_masquerade<N: Netstack, M: Manager>(name: &str, setup: MasqueradeTestSetup) {
    let MasqueradeTestSetup {
        client_ip,
        client_subnet,
        client_gateway,
        server_ip,
        server_subnet,
        server_gateway,
        router_ip,
        router_client_ip,
        router_server_ip,
        router_if_config,
    } = setup;

    let sandbox = netemul::TestSandbox::new().expect("create sandbox");

    let client_net = sandbox.create_network("client").await.expect("create network");
    let server_net = sandbox.create_network("server").await.expect("create network");
    let client =
        sandbox.create_netstack_realm::<N, _>(format!("{}_client", name)).expect("create realm");
    let server =
        sandbox.create_netstack_realm::<N, _>(format!("{}_server", name)).expect("create realm");
    let router = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("{name}_router"),
            &[
                KnownServiceProvider::Manager {
                    agent: ManagementAgent::NetCfg(NetCfgVersion::Advanced),
                    config: ManagerConfig::Empty,
                    use_dhcp_server: false,
                    use_out_of_stack_dhcp_client: false,
                },
                KnownServiceProvider::DnsResolver,
                KnownServiceProvider::FakeClock,
            ],
        )
        .expect("create host netstack realm");

    let client_iface = client
        .join_network(&client_net, "client-ep")
        .await
        .expect("install interface in client netstack");
    client_iface.add_address_and_subnet_route(client_subnet).await.expect("configure address");
    client_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
    let server_iface = server
        .join_network(&server_net, "server-ep")
        .await
        .expect("install interface in server nestack");
    server_iface.add_address_and_subnet_route(server_subnet).await.expect("configure address");
    server_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    let router_client_iface = router
        .join_network(&client_net, "client-router-ep")
        .await
        .expect("router joins client network");
    router_client_iface
        .add_address_and_subnet_route(router_client_ip)
        .await
        .expect("configure address");
    router_client_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
    let router_server_iface = router
        .join_network(&server_net, "server-router-ep")
        .await
        .expect("router joins server network");
    router_server_iface
        .add_address_and_subnet_route(router_server_ip)
        .await
        .expect("configure address");
    router_server_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");

    async fn add_default_gateway(
        realm: &netemul::TestRealm<'_>,
        interface: &netemul::TestInterface<'_>,
        gateway: fnet::IpAddress,
    ) {
        let unspecified_address = fnet_ext::IpAddress(match gateway {
            fnet::IpAddress::Ipv4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            fnet::IpAddress::Ipv6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        })
        .into();
        let stack =
            realm.connect_to_protocol::<fnet_stack::StackMarker>().expect("connect to protocol");
        stack
            .add_forwarding_entry(&fnet_stack::ForwardingEntry {
                subnet: fnet::Subnet { addr: unspecified_address, prefix_len: 0 },
                device_id: interface.id(),
                next_hop: Some(Box::new(gateway)),
                metric: fnet_stack::UNSPECIFIED_METRIC,
            })
            .await
            .expect("call add forwarding entry")
            .expect("add forwarding entry");
    }
    add_default_gateway(&client, &client_iface, client_gateway).await;
    add_default_gateway(&server, &server_iface, server_gateway).await;

    async fn enable_forwarding(
        interface: &fnet_interfaces_ext::admin::Control,
        config: &fnet_interfaces_admin::Configuration,
    ) {
        let _prev_config: fnet_interfaces_admin::Configuration = interface
            .set_configuration(config)
            .await
            .expect("call set configuration")
            .expect("set interface configuration");
    }
    enable_forwarding(router_client_iface.control(), &router_if_config).await;
    enable_forwarding(router_server_iface.control(), &router_if_config).await;

    let masq = router
        .connect_to_protocol::<fnet_masquerade::FactoryMarker>()
        .expect("connect to fuchsia.net.masquerade/Factory server");
    let (masq_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_masquerade::ControlMarker>()
            .expect("create fuchsia.net.masquerade/Control proxy and server end");

    masq.create(
        &fnet_masquerade::ControlConfig {
            input_interface: router_client_iface.id(),
            output_interface: router_server_iface.id(),
            src_subnet: router_client_ip,
        },
        server_end,
    )
    .await
    .expect("masq create fidl")
    .expect("masq create");

    async fn check_source_ip(
        expected_ip: std::net::IpAddr,
        sockaddr: SocketAddr,
        client_realm: &netemul::TestRealm<'_>,
        server_realm: &netemul::TestRealm<'_>,
    ) {
        let client = async {
            let mut stream =
                fuchsia_async::net::TcpStream::connect_in_realm(client_realm, sockaddr)
                    .await
                    .expect("connect to server");

            // Write some data to ensure that the connection stays open long
            // enough for the server to accept.
            stream.write_all(&"data".as_bytes()).await.expect("failed to write to stream");
            stream.flush().await.expect("flush stream");
        };

        let listener = fuchsia_async::net::TcpListener::listen_in_realm(server_realm, sockaddr)
            .await
            .expect("bind to address");
        let server = async {
            let (_listener, _stream, remote) =
                listener.accept().await.expect("accept incoming connection");
            assert_eq!(remote.ip(), expected_ip, "Encountered unexpected ip on connect");
        };

        futures_util::future::join(client, server).await;
    }

    // Before masquerade, the source IP should be the client IP.
    check_source_ip(client_ip, SocketAddr::from((server_ip, 8080)), &client, &server).await;

    // Once masquerade is enabled, the source IP should appear to be router_ip instead.
    assert!(!masq_control.set_enabled(true).await.expect("set enabled fidl").expect("set enabled"));
    check_source_ip(router_ip, SocketAddr::from((server_ip, 8081)), &client, &server).await;

    // Ensure that we can turn off masquerade again.
    assert!(masq_control.set_enabled(false).await.expect("set enabled fidl").expect("set enabled"));
    check_source_ip(client_ip, SocketAddr::from((server_ip, 8082)), &client, &server).await;
}

#[fuchsia::test]
async fn dhcpv4_client_restarts_after_delay() {
    const SERVER_MAC: net_types::ethernet::Mac = net_declare::net_mac!("02:02:02:02:02:02");

    let _name = with_netcfg_owned_device::<NetCfgBasic, Netstack3, _>(
        "dhcpv4_client_restarts_after_delay",
        ManagerConfig::Empty,
        true, /* use_out_of_stack_dhcp_client */
        [KnownServiceProvider::DhcpClient],
        |client_interface_id, network, client_state, client_realm, sandbox| {
            async move {
                let server_realm: netemul::TestRealm<'_> = sandbox
                    .create_netstack_realm_with::<Netstack3, _, _>(
                        "server-realm",
                        &[KnownServiceProvider::DhcpServer { persistent: false }],
                    )
                    .expect("create realm should succeed");

                let server_iface = server_realm
                    .join_network_with(
                        &network,
                        "serveriface",
                        fnetemul_network::EndpointConfig {
                            mtu: netemul::DEFAULT_MTU,
                            mac: Some(Box::new(
                                fnet_ext::MacAddress { octets: SERVER_MAC.bytes() }.into(),
                            )),
                            port_class: fhardware_network::PortClass::Virtual,
                        },
                        netemul::InterfaceConfig {
                            name: Some("serveriface".into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("join network with realm should succeed");
                server_iface.apply_nud_flake_workaround().await.expect("nud flake workaround");

                let server_proxy = server_realm
                    .connect_to_protocol::<fnet_dhcp::Server_Marker>()
                    .expect("connect to Server_ protocol should succeed");

                let config = dhcpv4_helper::TestConfig {
                    // Intentionally put the server_addr outside the subnet of the
                    // address assigned to the client. That way, the server won't be reachable
                    // via the subnet route associated with the DHCP-acquired address.
                    server_addr: fnet::Ipv4Address { addr: [192, 168, 0, 1] },
                    managed_addrs: dhcpv4::configuration::ManagedAddresses {
                        mask: dhcpv4::configuration::SubnetMask::new(net_prefix_length_v4!(24)),
                        pool_range_start: std::net::Ipv4Addr::new(192, 168, 1, 1),
                        pool_range_stop: std::net::Ipv4Addr::new(192, 168, 1, 7),
                    },
                };

                server_iface
                    .add_address_and_subnet_route(config.server_addr_with_prefix().into_ext())
                    .await
                    .expect("add address should succeed");

                // Because we're not putting the server on the same subnet as its address pool,
                // we need to explicitly configure it with a route to its address pool's subnet.
                server_iface
                    .add_subnet_route(fnet::Subnet {
                        addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: [192, 168, 1, 0] }),
                        prefix_len: 24,
                    })
                    .await
                    .expect("add subnet route");

                dhcpv4_helper::set_server_settings(
                    &server_proxy,
                    config.dhcp_parameters().into_iter().chain([
                        fnet_dhcp::Parameter::BoundDeviceNames(vec![server_iface
                            .get_interface_name()
                            .await
                            .expect("get interface name should succeed")]),
                        fnet_dhcp::Parameter::Lease(fnet_dhcp::LeaseLength {
                            // short enough to expire during this test, which
                            // will prompt the client to go into Renewing state,
                            // from which it can observe a NetworkUnreachable
                            // once we later remove the default route.
                            default: Some(10),
                            ..Default::default()
                        }),
                    ]),
                    [fnet_dhcp::Option_::Router(vec![config.server_addr])],
                )
                .await;

                let routes_state: fnet_routes::StateV4Proxy = client_realm
                    .connect_to_protocol::<fnet_routes::StateV4Marker>()
                    .expect("connect to routes state");

                let mut routes_event_stream =
                    pin!(fnet_routes_ext::event_stream_from_state::<Ipv4>(&routes_state)
                        .expect("routes event stream"));

                // Collect the current route table state prior to starting
                // the DHCP server so that we ensure the default route the
                // DHCP client acquires is not already present when we wait
                // for the default route.
                let mut routes = fnet_routes_ext::collect_routes_until_idle::<Ipv4, HashSet<_>>(
                    routes_event_stream.by_ref(),
                )
                .await
                .expect("collect routes until idle");

                server_proxy
                    .start_serving()
                    .await
                    .expect("start_serving should not encounter FIDL error")
                    .expect("start_serving should succeed");

                let if_event_stream = fnet_interfaces_ext::event_stream_from_state(
                    client_state,
                    fnet_interfaces_ext::IncludedAddresses::OnlyAssigned,
                )
                .expect("event stream from state");
                let mut if_event_stream = pin!(if_event_stream);

                let mut client_if_state =
                    fnet_interfaces_ext::InterfaceState::Unknown(client_interface_id);

                let find_ipv4_addr = |properties: &fnet_interfaces_ext::Properties| {
                    properties.addresses.iter().find_map(|addr| match addr.addr.addr {
                        fnet::IpAddress::Ipv4(_) => Some(addr.clone()),
                        fnet::IpAddress::Ipv6(_) => None,
                    })
                };

                let dhcp_acquired_addr = fnet_interfaces_ext::wait_interface_with_id(
                    if_event_stream.by_ref(),
                    &mut client_if_state,
                    |fnet_interfaces_ext::PropertiesAndState { properties, state: () }| {
                        find_ipv4_addr(properties)
                    },
                )
                .await
                .expect("wait for DHCP-acquired addr");

                assert_eq!(
                    dhcp_acquired_addr.addr,
                    fnet::Subnet {
                        addr: fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: [192, 168, 1, 1] }),
                        prefix_len: 24
                    }
                );

                let find_default_route =
                    |routes: &HashSet<fnet_routes_ext::InstalledRoute<Ipv4>>| {
                        routes.iter().find_map(
                            |fnet_routes_ext::InstalledRoute { route, effective_properties: _ }| {
                                (route.destination.prefix() == 0).then_some(route.clone())
                            },
                        )
                    };

                // The DHCP client discovers a default route from the DHCP server, and netcfg
                // should install that default route.
                let default_route = fnet_routes_ext::wait_for_routes_map::<Ipv4, _, _, _>(
                    routes_event_stream.by_ref(),
                    &mut routes,
                    |routes| find_default_route(routes),
                )
                .await
                .expect("should install default route");

                // Removing the default route should cause the DHCP client to exit with a
                // NetworkUnreachable error due to having no route to the DHCP server. This
                // causes us to lose the DHCP-acquired address.

                let root_routes = client_realm
                    .connect_to_protocol::<fnet_root::RoutesV4Marker>()
                    .expect("connect to fuchsia.net.root.RoutesV4");

                let (global_route_set, server_end) =
                    fidl::endpoints::create_proxy::<fnet_routes_admin::RouteSetV4Marker>()
                        .expect("create RouteSetV4 proxy");
                root_routes.global_route_set(server_end).expect("create global RouteSetV4");

                let fnet_interfaces_admin::GrantForInterfaceAuthorization { interface_id, token } =
                    client_realm
                        .interface_control(client_interface_id)
                        .expect("get client interface Control")
                        .get_authorization_for_interface()
                        .await
                        .expect("get authorization");

                global_route_set
                    .authenticate_for_interface(
                        fnet_interfaces_admin::ProofOfInterfaceAuthorization {
                            interface_id,
                            token,
                        },
                    )
                    .await
                    .expect("authenticate for interface should not see FIDL error")
                    .expect("authenticate for interface should succeed");

                // If we're particularly unlucky with CQ timing flakes, the DHCP
                // client can actually try to renew before the default route is
                // even originally installed, and observe a NetworkUnreachable.
                // That's fine for the purposes of the test, so we just ignore
                // whether the default route was still present at the time that
                // we tried to remove it.
                let _: bool = fnet_routes_ext::admin::remove_route::<Ipv4>(
                    &global_route_set,
                    &default_route.try_into().expect("convert to FIDL route"),
                )
                .await
                .expect("should not see RouteSet FIDL error")
                .expect("should not see RouteSet error");

                // Observe the default route removal.
                fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(
                    routes_event_stream.by_ref(),
                    &mut routes,
                    |routes| find_default_route(routes).is_none(),
                )
                .await
                .expect("should remove default route");

                // Observe the address disappearance.
                fnet_interfaces_ext::wait_interface_with_id(
                    if_event_stream.by_ref(),
                    &mut client_if_state,
                    |fnet_interfaces_ext::PropertiesAndState { properties, state: () }| {
                        find_ipv4_addr(properties).is_none().then_some(())
                    },
                )
                .await
                .expect("wait for DHCP-acquired addr to disappear");

                // Eventually, the DHCP client should be restarted, and we should acquire an IPv4
                // address and default route again.
                let new_dhcp_acquired_addr = fnet_interfaces_ext::wait_interface_with_id(
                    if_event_stream.by_ref(),
                    &mut client_if_state,
                    |fnet_interfaces_ext::PropertiesAndState { properties, state: () }| {
                        find_ipv4_addr(properties)
                    },
                )
                .await
                .expect("wait for DHCP-acquired addr");

                assert_eq!(dhcp_acquired_addr.addr, new_dhcp_acquired_addr.addr);

                fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(
                    routes_event_stream.by_ref(),
                    &mut routes,
                    |routes| find_default_route(routes).is_some(),
                )
                .await
                .expect("should reinstall default route");
            }
            .boxed_local()
        },
    )
    .await;
}
