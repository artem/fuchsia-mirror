// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::{cell::RefCell, collections::HashSet, pin::pin, time::Duration};

use async_utils::async_once::Once;
use dhcpv4::protocol::IntoFidlExt as _;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_dhcp as fnet_dhcp;
use fidl_fuchsia_net_ext::{self as fnet_ext, FromExt as _, IntoExt as _};
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fuchsia_async::{net::DatagramSocket, DurationExt as _, TimeoutExt as _};
use fuchsia_zircon as zx;
use fuchsia_zircon_types::zx_time_t;
use futures::{
    future::TryFutureExt as _,
    stream::{self, StreamExt as _, TryStreamExt as _},
};
use net_declare::{fidl_ip_v4, fidl_ip_v4_with_prefix, fidl_mac, net_subnet_v4};
use net_types::ip::Ipv4;
use netemul::{DhcpClient, DEFAULT_MTU};
use netstack_testing_common::{
    annotate, dhcpv4 as dhcpv4_helper,
    interfaces::{self, TestInterfaceExt as _},
    realms::{
        constants, DhcpClientVersion, KnownServiceProvider, Netstack, NetstackAndDhcpClient,
        NetstackVersion, TestSandboxExt as _,
    },
    Result, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT, ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use packet::ParsablePacket as _;
use sockaddr::{IntoSockAddr as _, TryToSockaddrLl as _};
use test_case::test_case;

const DEFAULT_NETWORK_NAME: &str = "net1";

enum DhcpEndpointType {
    Client {
        expected_acquired: fidl_fuchsia_net::Subnet,
        static_address: Option<fidl_fuchsia_net::Ipv4AddressWithPrefix>,
    },
    Server {
        static_addrs: Vec<fidl_fuchsia_net::Subnet>,
    },
    Unbound {
        static_addrs: Vec<fidl_fuchsia_net::Subnet>,
    },
}

struct DhcpTestEndpointConfig<'a> {
    ep_type: DhcpEndpointType,
    network: &'a DhcpTestNetwork<'a>,
}

// Struct for encapsulating a netemul network alongside various metadata.
// We rely heavily on the interior mutability pattern here so that we can
// pass around multiple references to the network while still modifying
// its internals.
struct DhcpTestNetwork<'a> {
    name: &'a str,
    network: Once<netemul::TestNetwork<'a>>,
    next_ep_idx: RefCell<usize>,
    sandbox: &'a netemul::TestSandbox,
}

impl<'a> DhcpTestNetwork<'a> {
    fn new(name: &'a str, sandbox: &'a netemul::TestSandbox) -> DhcpTestNetwork<'a> {
        DhcpTestNetwork { name, network: Once::new(), next_ep_idx: RefCell::new(0), sandbox }
    }

    async fn create_endpoint(&self) -> netemul::TestEndpoint<'_> {
        let DhcpTestNetwork { name, network, next_ep_idx, sandbox } = self;
        let net = network
            .get_or_try_init(async { sandbox.create_network(name.to_string()).await })
            .await
            .expect("failed to create network");
        let curr_idx: usize = next_ep_idx.replace_with(|&mut old| old + 1);
        let ep_name = format!("{}-{}", name, curr_idx);
        let endpoint = net.create_endpoint(ep_name).await.expect("failed to create endpoint");
        endpoint
    }
}

struct TestServerConfig<'a> {
    endpoints: &'a [DhcpTestEndpointConfig<'a>],
    settings: Settings<'a>,
}

struct TestNetstackRealmConfig<'a> {
    clients: &'a [DhcpTestEndpointConfig<'a>],
    servers: &'a mut [TestServerConfig<'a>],
    netstack_version: NetstackVersion,
}

const DEBUG_PRINT_INTERVAL: Duration = Duration::from_secs(10);

async fn assert_client_acquires_addr<D: DhcpClient>(
    client_realm: &netemul::TestRealm<'_>,
    client_interface: &netemul::TestInterface<'_>,
    expected_acquired: fidl_fuchsia_net::Subnet,
    cycles: usize,
    client_renews: bool,
) {
    let client_interface_state = client_realm
        .connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
        .expect("failed to connect to client fuchsia.net.interfaces/State");
    let event_stream = fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
        &client_interface_state,
        fidl_fuchsia_net_interfaces_ext::IncludedAddresses::OnlyAssigned,
    )
    .expect("event stream from state");
    let mut event_stream = pin!(event_stream);

    let mut properties =
        fidl_fuchsia_net_interfaces_ext::InterfaceState::<()>::Unknown(client_interface.id());
    for cycle in 0..cycles {
        // Enable the interface and assert that binding fails before the address is acquired.
        let () = client_interface.stop_dhcp::<D>().await.expect("failed to stop DHCP");
        let () = client_interface.set_link_up(true).await.expect("failed to bring link up");
        assert_matches::assert_matches!(
            bind(&client_realm, expected_acquired).await,
            Err(e @ anyhow::Error {..})
                if e.downcast_ref::<std::io::Error>()
                    .expect("bind() did not return std::io::Error")
                    .raw_os_error() == Some(libc::EADDRNOTAVAIL)
        );

        let () = client_interface.start_dhcp::<D>().await.expect("failed to start DHCP");

        let valid_until = annotate(
            assert_interface_assigned_addr(
                client_realm,
                expected_acquired,
                |_| true,
                event_stream.by_ref(),
                &mut properties,
            ),
            DEBUG_PRINT_INTERVAL,
            "initial assert_interface_assigned_addr",
        )
        .await;

        // If test covers renewal behavior, check that a subsequent interface changed event
        // occurs where the client retains its address, i.e. that it successfully renewed its
        // lease. It will take lease_length/2 duration for the client to renew its address
        // and trigger the subsequent interface changed event.
        if client_renews {
            let _: zx_time_t = annotate(
                assert_interface_assigned_addr(
                    client_realm,
                    expected_acquired,
                    |new_valid_until| new_valid_until > valid_until,
                    event_stream.by_ref(),
                    &mut properties,
                ),
                DEBUG_PRINT_INTERVAL,
                "renewal assert_interface_assigned_addr",
            )
            .await;
        }

        if cycle == cycles - 1 {
            // last cycle, so just return.
            return;
        }

        // Set interface online signal to down and wait for address to be removed.
        let () = client_interface.set_link_up(false).await.expect("failed to bring link down");
        let () = client_interface.stop_dhcp::<D>().await.expect("failed to stop DHCP");

        let () = annotate(
            fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
                event_stream.by_ref(),
                &mut properties,
                |iface| {
                    if iface.properties.addresses.iter().any(
                        |&fidl_fuchsia_net_interfaces_ext::Address {
                             addr,
                             valid_until: _,
                             assignment_state,
                         }| {
                            assert_eq!(
                                assignment_state,
                                fidl_fuchsia_net_interfaces::AddressAssignmentState::Assigned
                            );
                            addr == expected_acquired
                        },
                    ) {
                        None
                    } else {
                        Some(())
                    }
                },
            ),
            DEBUG_PRINT_INTERVAL,
            "address removal",
        )
        .await
        .expect("failed to wait for address removal");
    }
}

async fn assert_interface_assigned_addr(
    client_realm: &netemul::TestRealm<'_>,
    expected_acquired: fidl_fuchsia_net::Subnet,
    filter_valid_until: impl Fn(zx_time_t) -> bool,
    event_stream: impl futures::Stream<
        Item = std::result::Result<fidl_fuchsia_net_interfaces::Event, fidl::Error>,
    >,
    mut properties: &mut fidl_fuchsia_net_interfaces_ext::InterfaceState<()>,
) -> zx_time_t {
    let valid_until = fidl_fuchsia_net_interfaces_ext::wait_interface_with_id(
        event_stream,
        &mut properties,
        |iface| {
            iface.properties.addresses.iter().find_map(
                |&fidl_fuchsia_net_interfaces_ext::Address {
                     addr: subnet,
                     valid_until,
                     assignment_state,
                 }| {
                    assert_eq!(
                        assignment_state,
                        fidl_fuchsia_net_interfaces::AddressAssignmentState::Assigned
                    );
                    (subnet == expected_acquired && filter_valid_until(valid_until))
                        .then_some(valid_until)
                },
            )
        },
    )
    .map_err(anyhow::Error::from)
    .on_timeout(
        // Netstack's DHCP client retries every 3 seconds. At the time of writing, dhcpd
        // loses the race here and only starts after the first request from the DHCP
        // client, which results in a 3 second toll. This test typically takes ~4.5
        // seconds; we apply a large multiple to be safe.
        fuchsia_async::Time::after(zx::Duration::from_seconds(30)),
        || Err(anyhow::anyhow!("timed out")),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "failed to observe DHCP acquisition of {:?} on client ep: {:?}",
            expected_acquired, e
        )
    });

    // Address acquired; bind is expected to succeed.
    let _: std::net::UdpSocket =
        bind(&client_realm, expected_acquired).await.expect("binding to UDP socket failed");

    valid_until
}

fn bind<'a>(
    client_realm: &'a netemul::TestRealm<'_>,
    fidl_fuchsia_net::Subnet { addr, prefix_len: _ }: fidl_fuchsia_net::Subnet,
) -> impl futures::Future<Output = Result<std::net::UdpSocket>> + 'a {
    use netemul::RealmUdpSocket as _;

    let fidl_fuchsia_net_ext::IpAddress(ip_address) = addr.into();
    std::net::UdpSocket::bind_in_realm(client_realm, std::net::SocketAddr::new(ip_address, 0))
}

struct Settings<'a> {
    parameters: &'a mut [fidl_fuchsia_net_dhcp::Parameter],
    options: &'a mut [fidl_fuchsia_net_dhcp::Option_],
}

#[netstack_test]
#[test_case(true; "remove dhcp address")]
#[test_case(false; "remove non-dhcp address")]
async fn removing_acquired_address_stops_dhcp<SERVER: Netstack, CLIENT: NetstackAndDhcpClient>(
    name: &str,
    remove_dhcp_address: bool,
) {
    // TODO(https://github.com/rust-lang/libc/issues/3331): Use ETH_P_ALL from
    // libc once it is made available.
    const ETH_P_ALL: u16 = 0x0003;
    const ETH_P_IP_BE: u16 = (libc::ETH_P_IP as u16).to_be();
    const STATIC_ADDRESS: fidl_fuchsia_net::Ipv4AddressWithPrefix =
        fidl_ip_v4_with_prefix!("192.0.2.1/24");
    const DHCPV4_SERVER_PORT: u16 = 67;
    const DHCPV4_CLIENT_PORT: u16 = 68;
    const SHORT_LEASE_LENGTH_SECS: u32 = 20;

    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = DhcpTestNetwork::new(DEFAULT_NETWORK_NAME, &sandbox);
    let expected_acquired = dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired();
    let mut netstack_config = [
        TestNetstackRealmConfig {
            clients: &[DhcpTestEndpointConfig {
                ep_type: DhcpEndpointType::Client {
                    expected_acquired,
                    static_address: Some(STATIC_ADDRESS),
                },
                network: &network,
            }],
            servers: &mut [],
            netstack_version: CLIENT::Netstack::VERSION,
        },
        TestNetstackRealmConfig {
            clients: &[],
            servers: &mut [TestServerConfig {
                endpoints: &[DhcpTestEndpointConfig {
                    ep_type: DhcpEndpointType::Server {
                        static_addrs: vec![dhcpv4_helper::DEFAULT_TEST_CONFIG
                            .server_addr_with_prefix()
                            .into_ext()],
                    },
                    network: &network,
                }],
                settings: Settings {
                    parameters: &mut dhcpv4_helper::DEFAULT_TEST_CONFIG
                        .dhcp_parameters()
                        .into_iter()
                        .chain(std::iter::once(fnet_dhcp::Parameter::Lease(
                            fnet_dhcp::LeaseLength {
                                // Specify a shorter lease length so that the
                                // client attempts to renew the lease within the
                                // test timeout.
                                default: {
                                    assert!(
                                        zx::Duration::from_seconds(SHORT_LEASE_LENGTH_SECS.into())
                                            < ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT
                                    );
                                    Some(SHORT_LEASE_LENGTH_SECS)
                                },
                                max: Some(SHORT_LEASE_LENGTH_SECS),
                                ..Default::default()
                            },
                        )))
                        .collect::<Vec<_>>(),
                    options: &mut [],
                },
            }],
            netstack_version: SERVER::VERSION,
        },
    ];

    let dhcp_objects =
        test_dhcp::<CLIENT::DhcpClient>(name, &sandbox, &mut netstack_config, 1, false).await;
    let TestDhcpRealmAndInterfaces { realm, client_ifaces, _server_ifaces } = &dhcp_objects[0];
    let client_iface = &client_ifaces[0];
    let client = client_iface.control();
    let (remove_address, timeout) = if remove_dhcp_address {
        (expected_acquired, ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT)
    } else {
        (STATIC_ADDRESS.into_ext(), ASYNC_EVENT_POSITIVE_CHECK_TIMEOUT)
    };
    assert!(client
        .remove_address(&remove_address)
        .await
        .expect("send address removal request")
        .expect("remove DHCP acquired address"),);
    assert!(client.disable().await.expect("send disable request").expect("disable interface"));
    let socket = realm
        .packet_socket(fidl_fuchsia_posix_socket_packet::Kind::Network)
        .await
        .expect("get packet socket");
    let sockaddr = libc::sockaddr_ll {
        sll_family: u16::try_from(libc::AF_PACKET).unwrap(),
        sll_protocol: ETH_P_ALL.to_be(), // Only ETH_P_ALL receives RX.
        sll_ifindex: i32::try_from(client_iface.id()).unwrap(),
        sll_hatype: 0,
        sll_pkttype: 0,
        sll_halen: 0,
        sll_addr: [0; 8],
    };
    socket.bind(&sockaddr.into_sockaddr()).expect("bind packet socket to client interface");
    assert!(client.enable().await.expect("send enable request").expect("enable interface"));
    let socket = DatagramSocket::new_from_socket(socket).unwrap();
    let fut = async {
        let mut buf = [0; DEFAULT_MTU as usize];
        loop {
            let (n, sockaddr) = socket.recv_from(&mut buf).await.expect("recvfrom packet socket");
            let data = &buf[..n];

            let sockaddr = sockaddr.try_to_sockaddr_ll().unwrap();
            if sockaddr.sll_protocol != ETH_P_IP_BE {
                // Ignore non-IPv4 packets.
                continue;
            }

            let (mut ipv4_body, src_ip, dst_ip, proto, _ttl) =
                packet_formats::testutil::parse_ip_packet::<net_types::ip::Ipv4>(data)
                    .expect("error parsing IPv4 packet");
            if proto != packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp) {
                // Ignore non-UDP packets.
                continue;
            }

            let udp_v4_packet = packet_formats::udp::UdpPacket::parse(
                &mut ipv4_body,
                packet_formats::udp::UdpParseArgs::new(src_ip, dst_ip),
            )
            .expect("error parsing UDP datagram");

            // Look for packets that are sent across the DHCP-specific ports.
            let src_port = udp_v4_packet.src_port().expect("missing src port").get();
            let dst_port = udp_v4_packet.dst_port().get();
            if src_port == DHCPV4_CLIENT_PORT || dst_port == DHCPV4_SERVER_PORT {
                break;
            }
        }
        Some(())
    };
    assert_eq!(fut.on_timeout(timeout.after_now(), || None).await.is_none(), remove_dhcp_address);
}

#[netstack_test]
async fn acquire_with_dhcpd_bound_device<SERVER: Netstack, CLIENT: NetstackAndDhcpClient>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = DhcpTestNetwork::new(DEFAULT_NETWORK_NAME, &sandbox);

    let _ = test_dhcp::<CLIENT::DhcpClient>(
        name,
        &sandbox,
        &mut [
            TestNetstackRealmConfig {
                clients: &[DhcpTestEndpointConfig {
                    ep_type: DhcpEndpointType::Client {
                        expected_acquired: dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired(),
                        static_address: None,
                    },
                    network: &network,
                }],
                servers: &mut [],
                netstack_version: CLIENT::Netstack::VERSION,
            },
            TestNetstackRealmConfig {
                clients: &[],
                servers: &mut [TestServerConfig {
                    endpoints: &[DhcpTestEndpointConfig {
                        ep_type: DhcpEndpointType::Server {
                            static_addrs: vec![dhcpv4_helper::DEFAULT_TEST_CONFIG
                                .server_addr_with_prefix()
                                .into_ext()],
                        },
                        network: &network,
                    }],
                    settings: Settings {
                        parameters: &mut dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters(),
                        options: &mut [],
                    },
                }],
                netstack_version: SERVER::VERSION,
            },
        ],
        1,
        false,
    )
    .await;
}

// Regression test for https://fxbug.dev/42081372.
#[netstack_test]
async fn does_not_crash_with_overlapping_subnet_route<
    SERVER: Netstack,
    CLIENT: NetstackAndDhcpClient,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = DhcpTestNetwork::new(DEFAULT_NETWORK_NAME, &sandbox);

    let expected_acquired = dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired();
    let mut netstack_configs = [
        TestNetstackRealmConfig {
            clients: &[DhcpTestEndpointConfig {
                ep_type: DhcpEndpointType::Client {
                    expected_acquired: expected_acquired.clone(),
                    static_address: None,
                },
                network: &network,
            }],
            servers: &mut [],
            netstack_version: CLIENT::Netstack::VERSION,
        },
        TestNetstackRealmConfig {
            clients: &[],
            servers: &mut [TestServerConfig {
                endpoints: &[DhcpTestEndpointConfig {
                    ep_type: DhcpEndpointType::Server {
                        static_addrs: vec![dhcpv4_helper::DEFAULT_TEST_CONFIG
                            .server_addr_with_prefix()
                            .into_ext()],
                    },
                    network: &network,
                }],
                settings: Settings {
                    parameters: &mut dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters(),
                    options: &mut [],
                },
            }],
            netstack_version: SERVER::VERSION,
        },
    ];

    let realms_and_interfaces =
        test_dhcp::<CLIENT::DhcpClient>(name, &sandbox, &mut netstack_configs, 1, false).await;
    let (client_realm, client_interfaces) = match &realms_and_interfaces[..] {
        [TestDhcpRealmAndInterfaces { realm, client_ifaces, _server_ifaces: _ }, TestDhcpRealmAndInterfaces { realm: _, client_ifaces: _, _server_ifaces: _ }] => {
            (realm, client_ifaces)
        }
        _ => panic!("should have a client realm and a server realm: {:?}", &realms_and_interfaces),
    };

    let dhcp_added_subnet = fnet_ext::apply_subnet_mask(expected_acquired);
    let state_v4 = client_realm
        .connect_to_protocol::<fnet_routes::StateV4Marker>()
        .expect("connect to routes StateV4");
    let routes_event_stream =
        fnet_routes_ext::event_stream_from_state::<Ipv4>(&state_v4).expect("routes event stream");
    let mut routes_event_stream = pin!(routes_event_stream);

    let mut routes = HashSet::new();
    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(
        &mut routes_event_stream,
        &mut routes,
        |routes| {
            routes.iter().any(|installed_route| {
                &fnet::Subnet::from_ext(installed_route.route.destination.clone())
                    == &dhcp_added_subnet
            })
        },
    )
    .await
    .expect("wait for DHCP-acquired subnet route");

    let dhcp_added_route = routes
        .iter()
        .find_map(|installed_route| {
            (&fnet::Subnet::from_ext(installed_route.route.destination.clone())
                == &dhcp_added_subnet)
                .then_some(installed_route.route.clone())
        })
        .expect("should find DHCP-added route in table");

    // Add a second, identical route independently of the DHCP-installed one.
    let set_provider = client_realm
        .connect_to_protocol::<fnet_routes_admin::RouteTableV4Marker>()
        .expect("connect to routes-admin V4");
    let route_set = fnet_routes_ext::admin::new_route_set::<Ipv4>(&set_provider)
        .expect("new route set should succeed");

    let client_interface = client_interfaces.iter().last().expect("should have client interface");
    let grant = client_interface.get_authorization().await.expect("getting grant should succeed");
    let proof = fidl_fuchsia_net_interfaces_ext::admin::proof_from_grant(&grant);
    fnet_routes_ext::admin::authenticate_for_interface::<Ipv4>(&route_set, proof)
        .await
        .expect("no FIDL error")
        .expect("authentication should succeed");

    assert!(fnet_routes_ext::admin::add_route::<Ipv4>(
        &route_set,
        &dhcp_added_route.try_into().expect("convert to FIDL route")
    )
    .await
    .expect("should have no FIDL error")
    .expect("add route should succeed"));

    let route_to_use_for_flushing = fnet_routes_ext::Route {
        destination: net_subnet_v4!("199.198.197.0/32"),
        action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
            outbound_interface: client_interface.id(),
            next_hop: None,
        }),
        properties: fnet_routes_ext::RouteProperties {
            specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty),
            },
        },
    };

    // If the netstack is behaving correctly, the identical route we added would
    // never appear on the watcher, but we need to wait long enough for it to
    // get processed by the netstack. Thus, we add an unrelated route and watch
    // for it in order to "flush" the watcher.
    assert!(fnet_routes_ext::admin::add_route::<Ipv4>(
        &route_set,
        &route_to_use_for_flushing.clone().try_into().expect("convert to FIDL route")
    )
    .await
    .expect("should have no FIDL error")
    .expect("add route should succeed"));

    fnet_routes_ext::wait_for_routes::<Ipv4, _, _>(routes_event_stream, &mut routes, |routes| {
        routes.iter().any(|installed_route| &installed_route.route == &route_to_use_for_flushing)
    })
    .await
    .expect("wait for other route");
}

#[netstack_test]
async fn acquire_then_renew_with_dhcpd_bound_device<
    SERVER: Netstack,
    CLIENT: NetstackAndDhcpClient,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = DhcpTestNetwork::new(DEFAULT_NETWORK_NAME, &sandbox);

    // A realistic lease length that won't expire within the test timeout of 2 minutes.
    const LONG_LEASE: u32 = 60 * 60 * 24;
    // A short client renewal time which will trigger well before the test timeout of 2 minutes.
    const SHORT_RENEW: u32 = 3;

    let mut parameters = dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters();
    parameters.push(fidl_fuchsia_net_dhcp::Parameter::Lease(fidl_fuchsia_net_dhcp::LeaseLength {
        default: Some(LONG_LEASE),
        max: Some(LONG_LEASE),
        ..Default::default()
    }));

    let _ = test_dhcp::<CLIENT::DhcpClient>(
        name,
        &sandbox,
        &mut [
            TestNetstackRealmConfig {
                clients: &[DhcpTestEndpointConfig {
                    ep_type: DhcpEndpointType::Client {
                        expected_acquired: dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired(),
                        static_address: None,
                    },
                    network: &network,
                }],
                servers: &mut [],
                netstack_version: CLIENT::Netstack::VERSION,
            },
            TestNetstackRealmConfig {
                clients: &[],
                servers: &mut [TestServerConfig {
                    endpoints: &[DhcpTestEndpointConfig {
                        ep_type: DhcpEndpointType::Server {
                            static_addrs: vec![dhcpv4_helper::DEFAULT_TEST_CONFIG
                                .server_addr_with_prefix()
                                .into_ext()],
                        },
                        network: &network,
                    }],
                    settings: Settings {
                        parameters: &mut parameters.to_vec(),
                        options: &mut [fidl_fuchsia_net_dhcp::Option_::RenewalTimeValue(
                            SHORT_RENEW,
                        )],
                    },
                }],
                netstack_version: SERVER::VERSION,
            },
        ],
        1,
        true,
    )
    .await;
}

// TODO(https://fxbug.dev/42077260): Enable this test for Netstack3. Note that
// the test will need to be updated to make DAD more robust to CQ timing
// variability. This could be done by configuring the number of IPV4
// `dad_transmits` to some large value.
#[netstack_test]
async fn acquire_with_dhcpd_bound_device_dup_addr<
    SERVER: Netstack,
    CLIENT: NetstackAndDhcpClient,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");
    let network = DhcpTestNetwork::new(DEFAULT_NETWORK_NAME, &sandbox);

    let expected_acquired = dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired();
    let expected_addr = match expected_acquired.addr {
        fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address { addr: mut octets }) => {
            // We expect to assign the address numericaly succeeding the default client address
            // since the default client address will be assigned to a neighbor of the client so
            // the client should decline the offer and restart DHCP.
            *octets.iter_mut().last().expect("IPv4 addresses have a non-zero number of octets") +=
                1;

            fidl_fuchsia_net::Subnet {
                addr: fidl_fuchsia_net::IpAddress::Ipv4(fidl_fuchsia_net::Ipv4Address {
                    addr: octets,
                }),
                ..expected_acquired
            }
        }
        fidl_fuchsia_net::IpAddress::Ipv6(a) => {
            panic!("expected IPv4 address; got IPv6 address = {:?}", a)
        }
    };

    let _ = test_dhcp::<CLIENT::DhcpClient>(
        name,
        &sandbox,
        &mut [
            TestNetstackRealmConfig {
                clients: &[DhcpTestEndpointConfig {
                    ep_type: DhcpEndpointType::Client {
                        expected_acquired: expected_addr,
                        static_address: None,
                    },
                    network: &network,
                }],
                servers: &mut [],
                netstack_version: CLIENT::Netstack::VERSION,
            },
            TestNetstackRealmConfig {
                clients: &[],
                servers: &mut [TestServerConfig {
                    endpoints: &[
                        DhcpTestEndpointConfig {
                            ep_type: DhcpEndpointType::Server {
                                static_addrs: vec![dhcpv4_helper::DEFAULT_TEST_CONFIG
                                    .server_addr_with_prefix()
                                    .into_ext()],
                            },
                            network: &network,
                        },
                        DhcpTestEndpointConfig {
                            ep_type: DhcpEndpointType::Unbound {
                                static_addrs: vec![expected_acquired],
                            },
                            network: &network,
                        },
                    ],
                    settings: Settings {
                        parameters: &mut dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters(),
                        options: &mut [],
                    },
                }],
                netstack_version: SERVER::VERSION,
            },
        ],
        1,
        false,
    )
    .await;
}

#[derive(Debug)]
struct TestDhcpRealmAndInterfaces<'a> {
    realm: netemul::TestRealm<'a>,
    client_ifaces: Vec<netemul::TestInterface<'a>>,
    _server_ifaces: Vec<netemul::TestInterface<'a>>,
}

/// test_dhcp provides a flexible way to test DHCP acquisition across various network topologies.
///
/// This method will:
///   -- For each passed netstack config:
///        -- Start a netstack in a new realm
///        -- Create netemul endpoints in the referenced network and install them on the netstack
///        -- Start DHCP servers on each server endpoint
///        -- Start DHCP clients on each client endpoint and verify that they acquire the expected
///           addresses
fn test_dhcp<'a, D: DhcpClient>(
    test_name: &'a str,
    sandbox: &'a netemul::TestSandbox,
    netstack_configs: &'a mut [TestNetstackRealmConfig<'a>],
    dhcp_loop_cycles: usize,
    expect_client_renews: bool,
) -> impl futures::Future<Output = Vec<TestDhcpRealmAndInterfaces<'a>>> + 'a {
    async move {
        let dhcp_objects = stream::iter(netstack_configs.into_iter())
            .enumerate()
            .then(|(id, netstack)| async move {
                let TestNetstackRealmConfig { servers, clients, netstack_version } = netstack;
                let netstack_realm = sandbox.create_realm(
                        format!("netstack_realm_{}_{}", test_name, id),
                        &[
                            KnownServiceProvider::Netstack(*netstack_version),
                            KnownServiceProvider::DhcpServer { persistent: false },
                            KnownServiceProvider::FakeClock,
                            KnownServiceProvider::SecureStash,
                        ].into_iter().chain(match D::DHCP_CLIENT_VERSION {
                            DhcpClientVersion::InStack => None,
                            DhcpClientVersion::OutOfStack => Some(KnownServiceProvider::DhcpClient),
                        }).collect::<Vec<_>>(),
                    )
                    .expect("failed to create netstack realm");
                let netstack_realm_ref = &netstack_realm;

                let servers = stream::iter(servers.into_iter())
                    .then(|server| async move {
                        let TestServerConfig {
                            endpoints,
                            settings: Settings { options, parameters },
                        } = server;

                        let (ifaces, names_to_bind): (
                            Vec<netemul::TestInterface<'_>>,
                            Vec<String>,
                        ) = stream::iter(endpoints.into_iter())
                            .enumerate()
                            .then(|(idx, endpoint)| async move {
                                let DhcpTestEndpointConfig { ep_type, network } = endpoint;
                                let endpoint = network
                                    .create_endpoint()
                                    .await;
                                let if_name = format!("{}{}", "testeth", idx);
                                let iface = netstack_realm_ref
                                    .install_endpoint(
                                        endpoint,
                                        netemul::InterfaceConfig {
                                            name: Some(if_name.clone().into()), ..Default::default() },
                                    )
                                    .await
                                    .expect("failed to install server endpoint");
                                iface.apply_nud_flake_workaround().await.expect("nud flake workaround");
                                let (static_addrs, server_should_bind) = match ep_type {
                                    DhcpEndpointType::Client { expected_acquired: _, static_address: _ } => {
                                        panic!(
                                            "found client endpoint instead of server or unbound endpoint"
                                        )
                                    }
                                    DhcpEndpointType::Server { static_addrs } => {
                                        (static_addrs, true)
                                    }
                                    DhcpEndpointType::Unbound { static_addrs } => {
                                        (static_addrs, false)
                                    }
                                };
                                for subnet in static_addrs.into_iter().copied() {
                                    let address_state_provider = interfaces::add_subnet_address_and_route_wait_assigned(
                                        &iface,
                                        subnet,
                                        fidl_fuchsia_net_interfaces_admin::AddressParameters::default(),
                                    )
                                        .await
                                        .expect("add subnet address and route");
                                    let () = address_state_provider.detach()
                                        .expect("detach address lifetime");
                                }
                                (iface, server_should_bind.then(|| if_name))
                            })
                            .fold(
                                (Vec::new(), Vec::new()),
                                |(mut ifaces, mut names_to_bind), (iface, if_name)| async {
                                    let () = ifaces.push(iface);
                                    if let Some(if_name) = if_name {
                                        let () = names_to_bind.push(if_name);
                                    }
                                    (ifaces, names_to_bind)
                                },
                            )
                            .await;

                        let dhcp_server = netstack_realm_ref
                            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
                            .expect("failed to connect to DHCP server");

                        let parameters = parameters
                            .iter()
                            .cloned()
                            .chain(std::iter::once(
                                fidl_fuchsia_net_dhcp::Parameter::BoundDeviceNames(names_to_bind)
                            ));
                        let () = dhcpv4_helper::set_server_settings(
                            &dhcp_server,
                            parameters,
                            options.iter().cloned(),
                        ).await;

                        dhcp_server
                            .start_serving()
                            .await
                            .expect("failed to call dhcp/Server.StartServing")
                            .map_err(zx::Status::from_raw)
                            .expect("dhcp/Server.StartServing returned error");
                        (dhcp_server, ifaces)
                    })
                    .collect::<Vec<_>>()
                    .await;

                let clients = stream::iter(clients.into_iter())
                    .then(|client| async move {
                        let DhcpTestEndpointConfig { ep_type, network } = client;
                        let endpoint = network
                            .create_endpoint()
                            .await;
                        let iface = netstack_realm_ref
                            .install_endpoint(endpoint, Default::default())
                            .await
                            .expect("failed to install client endpoint");
                        let expected_acquired = match ep_type {
                            DhcpEndpointType::Client {
                                expected_acquired,
                                static_address,
                            } => {
                                if let Some(static_address) = static_address {
                                    iface.add_address(static_address.clone().into_ext()).await.expect("add static address");
                                }
                                expected_acquired
                            },
                            DhcpEndpointType::Server { static_addrs: _ } => panic!(
                                "found server endpoint instead of client endpoint"
                            ),
                            DhcpEndpointType::Unbound { static_addrs: _ } => panic!(
                                "found unbound endpoint instead of client endpoint"
                            ),
                        };
                        (iface, expected_acquired)
                    })
                    .collect::<Vec<_>>()
                    .await;

                Result::Ok((netstack_realm, servers, clients))
            })
            .try_collect::<Vec<_>>()
            .await
            .expect("failed to create DHCP domain objects");

        // Gather all server MAC and IP addresses so we can add them to clients'
        // neighbor tables to avoid flakes.
        let server_ifaces = dhcp_objects
            .iter()
            .flat_map(|(_realm, servers, _clients)| servers)
            .flat_map(|(_proxy, ifaces)| ifaces);

        let all_server_macs_and_addrs = stream::iter(server_ifaces)
            .then(|iface| async move {
                let addrs = iface
                    .get_addrs(fidl_fuchsia_net_interfaces_ext::IncludedAddresses::OnlyAssigned)
                    .await
                    .expect("get addresses");
                (iface.mac().await, addrs.iter().map(|addr| addr.addr.addr).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>()
            .await;

        let mut realms = Vec::new();
        for (netstack_realm, servers, clients) in dhcp_objects {
            let mut client_ifaces = Vec::new();

            for (client, expected_acquired) in clients {
                // Add all servers as neighbor entries to avoid flakes.
                for (mac, addr) in all_server_macs_and_addrs
                    .iter()
                    .flat_map(|(mac, addrs)| addrs.iter().map(|addr| (*mac, *addr)))
                {
                    netstack_realm
                        .add_neighbor_entry(client.id(), addr, mac)
                        .await
                        .expect("add neighbor entry");
                }

                assert_client_acquires_addr::<D>(
                    &netstack_realm,
                    &client,
                    *expected_acquired,
                    dhcp_loop_cycles,
                    expect_client_renews,
                )
                .await;
                client_ifaces.push(client);
            }
            let mut server_ifaces = Vec::new();
            for (server, mut ifaces) in servers {
                let () = server.stop_serving().await.expect("failed to stop server");
                server_ifaces.append(&mut ifaces);
            }
            realms.push(TestDhcpRealmAndInterfaces {
                realm: netstack_realm,
                client_ifaces,
                _server_ifaces: server_ifaces,
            });
        }

        realms
    }
}

#[derive(Copy, Clone)]
enum PersistenceMode {
    Persistent,
    Ephemeral,
}

impl std::fmt::Display for PersistenceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersistenceMode::Persistent => write!(f, "persistent"),
            PersistenceMode::Ephemeral => write!(f, "ephemeral"),
        }
    }
}

impl PersistenceMode {
    fn dhcpd_params_after_restart(
        &self,
        if_name: String,
    ) -> Vec<(fidl_fuchsia_net_dhcp::ParameterName, fidl_fuchsia_net_dhcp::Parameter)> {
        match self {
            Self::Persistent => {
                let params = test_dhcpd_parameters(if_name);
                params.into_iter().map(|p| (param_name(&p), p)).collect()
            }
            Self::Ephemeral => vec![
                fidl_fuchsia_net_dhcp::Parameter::IpAddrs(vec![]),
                fidl_fuchsia_net_dhcp::Parameter::AddressPool(fidl_fuchsia_net_dhcp::AddressPool {
                    prefix_length: Some(0),
                    range_start: Some(fidl_ip_v4!("0.0.0.0")),
                    range_stop: Some(fidl_ip_v4!("0.0.0.0")),
                    ..Default::default()
                }),
                fidl_fuchsia_net_dhcp::Parameter::Lease(fidl_fuchsia_net_dhcp::LeaseLength {
                    default: Some(86400),
                    max: Some(86400),
                    ..Default::default()
                }),
                fidl_fuchsia_net_dhcp::Parameter::PermittedMacs(vec![]),
                fidl_fuchsia_net_dhcp::Parameter::StaticallyAssignedAddrs(vec![]),
                fidl_fuchsia_net_dhcp::Parameter::ArpProbe(false),
                fidl_fuchsia_net_dhcp::Parameter::BoundDeviceNames(vec![]),
            ]
            .into_iter()
            .map(|p| (param_name(&p), p))
            .collect(),
        }
    }
}

// This collection of parameters is defined as a function because we need to allocate a Vec which
// cannot be done statically, i.e. as a constant.
fn test_dhcpd_parameters(if_name: String) -> Vec<fidl_fuchsia_net_dhcp::Parameter> {
    vec![
        fidl_fuchsia_net_dhcp::Parameter::IpAddrs(vec![
            dhcpv4_helper::DEFAULT_TEST_CONFIG.server_addr,
        ]),
        fidl_fuchsia_net_dhcp::Parameter::AddressPool(
            dhcpv4_helper::DEFAULT_TEST_CONFIG.managed_addrs.into_fidl(),
        ),
        fidl_fuchsia_net_dhcp::Parameter::Lease(fidl_fuchsia_net_dhcp::LeaseLength {
            default: Some(60),
            max: Some(60),
            ..Default::default()
        }),
        fidl_fuchsia_net_dhcp::Parameter::PermittedMacs(vec![fidl_mac!("aa:bb:cc:dd:ee:ff")]),
        fidl_fuchsia_net_dhcp::Parameter::StaticallyAssignedAddrs(vec![
            fidl_fuchsia_net_dhcp::StaticAssignment {
                host: Some(fidl_mac!("aa:bb:cc:dd:ee:ff")),
                assigned_addr: Some(fidl_ip_v4!("192.168.0.2")),
                ..Default::default()
            },
        ]),
        fidl_fuchsia_net_dhcp::Parameter::ArpProbe(true),
        fidl_fuchsia_net_dhcp::Parameter::BoundDeviceNames(vec![if_name]),
    ]
}

fn param_name(param: &fidl_fuchsia_net_dhcp::Parameter) -> fidl_fuchsia_net_dhcp::ParameterName {
    match param {
        fidl_fuchsia_net_dhcp::Parameter::IpAddrs(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::IpAddrs
        }
        fidl_fuchsia_net_dhcp::Parameter::AddressPool(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::AddressPool
        }
        fidl_fuchsia_net_dhcp::Parameter::Lease(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::LeaseLength
        }
        fidl_fuchsia_net_dhcp::Parameter::PermittedMacs(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::PermittedMacs
        }
        fidl_fuchsia_net_dhcp::Parameter::StaticallyAssignedAddrs(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::StaticallyAssignedAddrs
        }
        fidl_fuchsia_net_dhcp::Parameter::ArpProbe(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::ArpProbe
        }
        fidl_fuchsia_net_dhcp::Parameter::BoundDeviceNames(_) => {
            fidl_fuchsia_net_dhcp::ParameterName::BoundDeviceNames
        }
        fidl_fuchsia_net_dhcp::ParameterUnknown!() => {
            panic!("attempted to retrieve name of Parameter::Unknown");
        }
    }
}

// This test guards against regression for the issue found in https://fxbug.dev/42141442. The test
// attempts to create an inconsistent state on the dhcp server by allowing the server to complete a
// transaction with a client, thereby creating a record of a lease. The server is then restarted;
// if the linked issue has not been fixed, then the server will inadvertently erase its
// configuration parameters from persistent storage, which will lead to an inconsistent server
// state on the next restart.  Finally, the server is restarted one more time, and then its
// clear_leases() function is triggered, which will cause a panic if the server is in an
// inconsistent state.
#[netstack_test]
async fn acquire_persistent_dhcp_server_after_restart<
    SERVER: Netstack,
    CLIENT: NetstackAndDhcpClient,
>(
    name: &str,
) {
    let mode = PersistenceMode::Persistent;
    acquire_dhcp_server_after_restart::<SERVER, CLIENT>(&format!("{}_{}", name, mode), mode).await
}

// An ephemeral dhcp server cannot become inconsistent with its persistent state because it has
// none.  However, without persistent state, an ephemeral dhcp server cannot run without explicit
// configuration.  This test verifies that an ephemeral dhcp server will return an error if run
// after restarting.
#[netstack_test]
async fn acquire_ephemeral_dhcp_server_after_restart<
    SERVER: Netstack,
    CLIENT: NetstackAndDhcpClient,
>(
    name: &str,
) {
    let mode = PersistenceMode::Ephemeral;
    acquire_dhcp_server_after_restart::<SERVER, CLIENT>(&format!("{}_{}", name, mode), mode).await
}

async fn acquire_dhcp_server_after_restart<SERVER: Netstack, CLIENT: NetstackAndDhcpClient>(
    name: &str,
    mode: PersistenceMode,
) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");

    let server_realm = sandbox
        .create_netstack_realm_with::<SERVER, _, _>(
            format!("{}_server", name),
            &[
                match mode {
                    PersistenceMode::Ephemeral => {
                        KnownServiceProvider::DhcpServer { persistent: false }
                    }
                    PersistenceMode::Persistent => {
                        KnownServiceProvider::DhcpServer { persistent: true }
                    }
                },
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("failed to create server realm");

    let client_realm = sandbox
        .create_netstack_realm_with::<CLIENT::Netstack, _, _>(
            format!("{}_client", name),
            match CLIENT::DhcpClient::DHCP_CLIENT_VERSION {
                DhcpClientVersion::InStack => None,
                DhcpClientVersion::OutOfStack => Some(&KnownServiceProvider::DhcpClient),
            },
        )
        .expect("failed to create client realm");

    let network = sandbox.create_network(name).await.expect("failed to create network");
    let if_name = "testeth";
    let endpoint = network.create_endpoint("server-ep").await.expect("failed to create endpoint");
    let server_ep = server_realm
        .install_endpoint(
            endpoint,
            netemul::InterfaceConfig { name: Some(if_name.into()), ..Default::default() },
        )
        .await
        .expect("failed to create server network endpoint");
    {
        let fidl_fuchsia_net::Ipv4AddressWithPrefix { addr, prefix_len } =
            dhcpv4_helper::DEFAULT_TEST_CONFIG.server_addr_with_prefix();
        server_ep
            .add_address_and_subnet_route(fidl_fuchsia_net::Subnet {
                addr: fidl_fuchsia_net::IpAddress::Ipv4(addr),
                prefix_len,
            })
            .await
            .expect("configure address");
    }
    let client_ep = client_realm
        .join_network(&network, "client-ep")
        .await
        .expect("failed to create client network endpoint");

    // Complete initial DHCP transaction in order to store a lease record in the server's
    // persistent storage.
    {
        let dhcp_server = server_realm
            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
            .expect("failed to connect to DHCP server");
        let parameters = dhcpv4_helper::DEFAULT_TEST_CONFIG.dhcp_parameters().into_iter().chain(
            std::iter::once(fidl_fuchsia_net_dhcp::Parameter::BoundDeviceNames(vec![
                if_name.to_string()
            ])),
        );
        let () =
            dhcpv4_helper::set_server_settings(&dhcp_server, parameters, std::iter::empty()).await;
        let () = dhcp_server
            .start_serving()
            .await
            .expect("failed to call dhcp/Server.StartServing")
            .map_err(zx::Status::from_raw)
            .expect("dhcp/Server.StartServing returned error");
        let () = assert_client_acquires_addr::<CLIENT::DhcpClient>(
            &client_realm,
            &client_ep,
            dhcpv4_helper::DEFAULT_TEST_CONFIG.expected_acquired(),
            1,
            false,
        )
        .await;
        let () = dhcp_server.stop_serving().await.expect("failed to call dhcp/Server.StopServing");
        let () = server_realm
            .stop_child_component(constants::dhcp_server::COMPONENT_NAME)
            .await
            .expect("failed to stop dhcpd");
    }

    // Restart the server in an attempt to force the server's persistent storage into an
    // inconsistent state whereby the addresses leased to clients do not agree with the contents of
    // the server's address pool. If the server is in ephemeral mode, it will fail at the call to
    // start_serving() since it will not have retained its parameters.
    {
        let dhcp_server = server_realm
            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
            .expect("failed to connect to DHCP server");
        let () = match mode {
            PersistenceMode::Persistent => {
                let () = dhcp_server
                    .start_serving()
                    .await
                    .expect("failed to call dhcp/Server.StartServing")
                    .map_err(zx::Status::from_raw)
                    .expect("dhcp/Server.StartServing returned error");
                dhcp_server.stop_serving().await.expect("failed to call dhcp/Server.StopServing")
            }
            PersistenceMode::Ephemeral => {
                assert_matches::assert_matches!(
                    dhcp_server
                        .start_serving()
                        .await
                        .expect("failed to call dhcp/Server.StartServing")
                        .map_err(zx::Status::from_raw),
                    Err(zx::Status::INVALID_ARGS)
                );
            }
        };
        let () = server_realm
            .stop_child_component(constants::dhcp_server::COMPONENT_NAME)
            .await
            .expect("failed to stop dhcpd");
    }

    // Restart the server again in order to load the inconsistent state into the server's runtime
    // representation. Call clear_leases() to trigger a panic resulting from inconsistent state,
    // provided that the issue motivating this test is unfixed/regressed. If the server is in
    // ephemeral mode, it will fail at the call to start_serving() since it will not have retained
    // its parameters.
    {
        let dhcp_server = server_realm
            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
            .expect("failed to connect to DHCP server");
        let () = match mode {
            PersistenceMode::Persistent => {
                let () = dhcp_server
                    .start_serving()
                    .await
                    .expect("failed to call dhcp/Server.StartServing")
                    .map_err(zx::Status::from_raw)
                    .expect("dhcp/Server.StartServing returned error");
                let () = dhcp_server
                    .stop_serving()
                    .await
                    .expect("failed to call dhcp/Server.StopServing");
                dhcp_server
                    .clear_leases()
                    .await
                    .expect("failed to call dhcp/Server.ClearLeases")
                    .map_err(zx::Status::from_raw)
                    .expect("dhcp/Server.ClearLeases returned error");
            }
            PersistenceMode::Ephemeral => {
                assert_matches::assert_matches!(
                    dhcp_server
                        .start_serving()
                        .await
                        .expect("failed to call dhcp/Server.StartServing")
                        .map_err(zx::Status::from_raw),
                    Err(zx::Status::INVALID_ARGS)
                );
            }
        };
        let () = server_realm
            .stop_child_component(constants::dhcp_server::COMPONENT_NAME)
            .await
            .expect("failed to stop dhcpd");
    }
}

#[netstack_test]
async fn dhcp_server_persistence_mode_persistent<N: Netstack>(name: &str) {
    let mode = PersistenceMode::Persistent;
    test_dhcp_server_persistence_mode::<N>(&format!("{}_{}", name, mode), mode).await
}

#[netstack_test]
async fn dhcp_server_persistence_mode_ephemeral<N: Netstack>(name: &str) {
    let mode = PersistenceMode::Ephemeral;
    test_dhcp_server_persistence_mode::<N>(&format!("{}_{}", name, mode), mode).await
}

async fn test_dhcp_server_persistence_mode<N: Netstack>(name: &str, mode: PersistenceMode) {
    let sandbox = netemul::TestSandbox::new().expect("failed to create sandbox");

    let server_realm = sandbox
        .create_netstack_realm_with::<N, _, _>(
            format!("{}_server", name),
            &[
                match mode {
                    PersistenceMode::Ephemeral => {
                        KnownServiceProvider::DhcpServer { persistent: false }
                    }
                    PersistenceMode::Persistent => {
                        KnownServiceProvider::DhcpServer { persistent: true }
                    }
                },
                KnownServiceProvider::FakeClock,
                KnownServiceProvider::SecureStash,
            ],
        )
        .expect("failed to create server realm");

    let network = sandbox.create_network(name).await.expect("failed to create network");
    let if_name = "testeth";
    let endpoint = network.create_endpoint("server-ep").await.expect("failed to create endpoint");
    let server_ep = server_realm
        .install_endpoint(
            endpoint,
            netemul::InterfaceConfig { name: Some(if_name.into()), ..Default::default() },
        )
        .await
        .expect("failed to create server network endpoint");
    server_ep
        .add_address_and_subnet_route(
            dhcpv4_helper::DEFAULT_TEST_CONFIG.server_addr_with_prefix().into_ext(),
        )
        .await
        .expect("configure address");

    // Configure the server with parameters and then restart it.
    {
        let dhcp_server = server_realm
            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
            .expect("failed to connect to server");
        let () = dhcpv4_helper::set_server_settings(
            &dhcp_server,
            test_dhcpd_parameters(if_name.to_string()),
            std::iter::empty(),
        )
        .await;
        let () = server_realm
            .stop_child_component(constants::dhcp_server::COMPONENT_NAME)
            .await
            .expect("failed to stop dhcpd");
    }

    // Assert that configured parameters after the restart correspond to the persistence mode of the
    // server.
    {
        let dhcp_server = server_realm
            .connect_to_protocol::<fidl_fuchsia_net_dhcp::Server_Marker>()
            .expect("failed to connect to server");
        let dhcp_server = &dhcp_server;
        let params = mode.dhcpd_params_after_restart(if_name.to_string());
        let () = stream::iter(params.into_iter())
            .for_each_concurrent(None, |(name, parameter)| async move {
                assert_eq!(
                    dhcp_server
                        .get_parameter(name)
                        .await
                        .unwrap_or_else(|e| {
                            panic!("dhcp/Server.GetParameter({:?}): {:?}", name, e)
                        })
                        .map_err(zx::Status::from_raw)
                        .unwrap_or_else(|e| {
                            panic!("dhcp/Server.GetParameter({:?}): {:?}", name, e)
                        }),
                    parameter
                )
            })
            .await;
        let () = server_realm
            .stop_child_component(constants::dhcp_server::COMPONENT_NAME)
            .await
            .expect("failed to stop dhcpd");
    }
}
