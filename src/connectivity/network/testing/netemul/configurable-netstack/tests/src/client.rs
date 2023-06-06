// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use configurable_netstack_test::{server_ips, BUS_NAME, REQUEST, RESPONSE, SERVER_NAME};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_debug as fnet_debug;
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_root as fnet_root;
use fidl_fuchsia_net_routes as fnet_routes;
use fidl_fuchsia_net_routes_ext as fnet_routes_ext;
use fuchsia_component::client::connect_to_protocol;
use futures_util::StreamExt as _;
use net_declare::{fidl_mac, net_ip_v4};
use net_types::ip::Ipv4;
use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
};
use test_case::test_case;

pub const CLIENT_NAME: &str = "client";

#[fuchsia_async::run_singlethreaded(test)]
async fn connect_to_server() {
    netemul_sync::Bus::subscribe(BUS_NAME, CLIENT_NAME)
        .expect("subscribe to bus")
        .wait_for_client(SERVER_NAME)
        .await
        .expect("wait for server to join bus");

    for addr in server_ips() {
        let mut stream = std::net::TcpStream::connect(&addr).expect("connect to server");
        let request = REQUEST.as_bytes();
        assert_eq!(stream.write(request).expect("write to socket"), request.len());
        stream.flush().expect("flush stream");

        let mut buffer = [0; 512];
        let read = stream.read(&mut buffer).expect("read from socket");
        let response = String::from_utf8_lossy(&buffer[0..read]);
        assert_eq!(response, RESPONSE, "got unexpected response from server: {}", response);
    }
}

const MAC_ADDR: fnet::MacAddress = fidl_mac!("aa:bb:cc:dd:ee:ff");

#[fuchsia_async::run_singlethreaded(test)]
async fn without_autogenerated_addresses() {
    let state = connect_to_protocol::<fnet_interfaces::StateMarker>().expect("connect to protocol");
    let stream = fnet_interfaces_ext::event_stream_from_state(&state)
        .expect("event stream from interfaces state");
    let interfaces = fnet_interfaces_ext::existing(stream, HashMap::<u64, _>::new())
        .await
        .expect("list existing interfaces")
        .into_values();
    let debug = connect_to_protocol::<fnet_debug::InterfacesMarker>().expect("connect to protocol");

    // Find the interface that corresponds to `MAC_ADDR` by querying
    // `fuchsia.net.debug/Interfaces.GetMac` with the ID of each existing interface.
    //
    // Once we've found the matching interface, retrieve its IPv4 and link-local
    // IPv6 addresses to ensure any auto-generated addresses were removed by the
    // netemul runner during test setup.
    let addresses = futures_util::stream::iter(interfaces).filter_map(
        |fnet_interfaces_ext::Properties { id, addresses, .. }| {
            let debug = &debug;
            async move {
                match debug.get_mac(id.get()).await.expect("get mac") {
                    Err(fnet_debug::InterfacesGetMacError::NotFound) => None,
                    Ok(mac_address) => {
                        let mac_address = mac_address.expect("mac address not set");
                        (mac_address.octets == MAC_ADDR.octets).then(|| addresses)
                    }
                }
            }
        },
    );
    futures_util::pin_mut!(addresses);
    let addresses = addresses
        .next()
        .await
        .expect("could not find interface")
        .into_iter()
        .filter_map(
            |fnet_interfaces_ext::Address {
                 addr: fnet::Subnet { addr, prefix_len: _ },
                 valid_until: _,
             }| match addr {
                ip_addr @ fnet::IpAddress::Ipv4(_) => Some(ip_addr),
                ip_addr @ fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr }) => {
                    let v6_addr = net_types::ip::Ipv6Addr::from_bytes(addr);
                    v6_addr.is_unicast_link_local().then(|| ip_addr)
                }
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(addresses, vec![], "found unexpected addresses on interface");
}

const GATEWAY: net_types::ip::Ipv4Addr = net_ip_v4!("192.168.0.1");

#[fuchsia_async::run_singlethreaded(test)]
async fn default_gateway() {
    let ipv4_routing_table = {
        let state_v4 =
            connect_to_protocol::<fnet_routes::StateV4Marker>().expect("connect to protocol");
        let stream = fnet_routes_ext::event_stream_from_state::<Ipv4>(&state_v4)
            .expect("failed to connect to watcher");
        futures::pin_mut!(stream);
        fnet_routes_ext::collect_routes_until_idle::<_, Vec<_>>(stream)
            .await
            .expect("failed to get routing table")
    };
    let found = ipv4_routing_table.iter().any(
        |fnet_routes_ext::InstalledRoute {
             route: fnet_routes_ext::Route { destination, action, properties: _ },
             effective_properties: _,
         }| {
            match action {
                fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
                    outbound_interface: _,
                    next_hop,
                }) => next_hop
                    // NB: Because destination is a `net_types::ip::Subnet`, a
                    // `prefix` of 0 ensures the `network` is unspecified.
                    .map(|next_hop| *next_hop == GATEWAY && destination.prefix() == 0)
                    .unwrap_or(false),
                fnet_routes_ext::RouteAction::Unknown => panic!("route with unknown action"),
            }
        },
    );
    assert!(found, "could not find default route to gateway {:?}", GATEWAY);
}

const IPV4_FWD_ENABLED_MAC_ADDR: fnet::MacAddress = fidl_mac!("88:99:aa:bb:cc:dd");
const IPV6_FWD_ENABLED_MAC_ADDR: fnet::MacAddress = fidl_mac!("cc:dd:ee:ff:aa:bb");

#[test_case(
    IPV4_FWD_ENABLED_MAC_ADDR,
    true,
    false;
    "interface with IPv4 forwarding enabled"
)]
#[test_case(
    IPV6_FWD_ENABLED_MAC_ADDR,
    false,
    true;
    "interface with IPv6 forwarding enabled"
)]
#[fuchsia_async::run_singlethreaded(test)]
async fn enable_forwarding(
    interface_mac_address: fnet::MacAddress,
    expected_ipv4_forwarding: bool,
    expected_ipv6_forwarding: bool,
) {
    let state = connect_to_protocol::<fnet_interfaces::StateMarker>().expect("connect to protocol");
    let stream = fnet_interfaces_ext::event_stream_from_state(&state)
        .expect("event stream from interfaces state");
    let interfaces = fnet_interfaces_ext::existing(stream, HashMap::<u64, _>::new())
        .await
        .expect("list existing interfaces")
        .into_keys();
    let debug = connect_to_protocol::<fnet_debug::InterfacesMarker>().expect("connect to protocol");

    // Find the interface that corresponds to `interface_mac_address` by querying
    // `fuchsia.net.debug/Interfaces.GetMac` with the ID of each existing interface.
    let matching_interface = futures_util::stream::iter(interfaces).filter_map(|id| {
        let debug = &debug;
        async move {
            match debug.get_mac(id).await.expect("get mac") {
                Err(fnet_debug::InterfacesGetMacError::NotFound) => None,
                Ok(mac_address) => {
                    let mac_address = mac_address.expect("mac address not set");
                    (mac_address.octets == interface_mac_address.octets).then(|| id)
                }
            }
        }
    });
    futures_util::pin_mut!(matching_interface);
    let id = matching_interface.next().await.expect("could not find interface");
    let root = connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");
    let (control, server_end) =
        fnet_interfaces_ext::admin::Control::create_endpoints().expect("create endpoints");
    root.get_admin(id, server_end).expect("get control handle to interface");

    let fnet_interfaces_admin::Configuration { ipv4, ipv6, .. } = control
        .get_configuration()
        .await
        .expect("call get configuration")
        .expect("get interface configuration");
    let fnet_interfaces_admin::Ipv4Configuration { forwarding: ipv4_forwarding, .. } =
        ipv4.expect("extract ipv4 configuration");
    let fnet_interfaces_admin::Ipv6Configuration { forwarding: ipv6_forwarding, .. } =
        ipv6.expect("extract ipv6 configuration");
    assert_eq!(ipv4_forwarding, Some(expected_ipv4_forwarding));
    assert_eq!(ipv6_forwarding, Some(expected_ipv6_forwarding));
}
