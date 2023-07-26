// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use assert_matches::assert_matches;
use fidl_fuchsia_hardware_network as fhardware_network;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_debug as fnet_debug;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext;
use fidl_fuchsia_net_root as fnet_root;
use fuchsia_zircon::{self as zx, AsHandleRef as _};
use futures::TryStreamExt as _;
use net_declare::fidl_mac;
use netstack_testing_common::{
    devices::{create_ip_tun_port, create_tun_device, install_device},
    realms::{Netstack, TestRealmExt as _, TestSandboxExt as _},
};
use netstack_testing_macros::netstack_test;

async fn get_loopback_id(realm: &netemul::TestRealm<'_>) -> u64 {
    let fnet_interfaces_ext::Properties {
        id,
        name: _,
        device_class: _,
        online: _,
        addresses: _,
        has_default_ipv4_route: _,
        has_default_ipv6_route: _,
    } = realm.loopback_properties().await.expect("loopback properties").expect("loopback missing");
    id.get()
}

#[netstack_test]
async fn get_admin_unknown<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");

    let id = get_loopback_id(&realm).await;

    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    // Request unknown NIC ID, expect request channel to be closed.
    let (admin_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    let () = root_interfaces.get_admin(id + 1, server_end).expect("get admin failed");

    let events = admin_control.take_event_stream().try_collect::<Vec<_>>().await;
    assert_matches!(
        events,
        Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FOUND, .. })
    );
}

#[netstack_test]
async fn get_admin_loopback<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    let (admin_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");
    let id = get_loopback_id(&realm).await;
    root_interfaces.get_admin(id, server_end).expect("get admin failed");

    // Actuate the admin API to verify it's hooked up correctly.
    assert_eq!(admin_control.get_id().await.expect("get id"), id);
}

#[netstack_test]
async fn get_admin_netemul_endpoint<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");
    let device = sandbox.create_endpoint(name).await.expect("create netemul endpoint");
    // Retain `_control` and `_device_control` to keep the FIDL channel open.
    let (id, _control, _device_control) = device
        .add_to_stack(&realm, netemul::InterfaceConfig::default())
        .await
        .expect("add to stack");
    let (admin_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");

    root_interfaces.get_admin(id, server_end).expect("get admin failed");

    // Actuate the admin API to verify it's hooked up correctly.
    assert_eq!(admin_control.get_id().await.expect("get id"), id);
}

// Retrieve the MAC address for the given device id, expecting no FIDL errors.
//
// This helper extracts the MAC from its `Box` making matching easier. See
// https://doc.rust-lang.org/beta/unstable-book/language-features/box-patterns.html.
async fn get_mac(
    id: u64,
    root_interfaces: &fnet_root::InterfacesProxy,
) -> Result<Option<fnet::MacAddress>, fnet_root::InterfacesGetMacError> {
    let mac = root_interfaces.get_mac(id).await.expect("get mac");
    mac.map(|option| option.map(|box_| *box_))
}

#[netstack_test]
async fn get_mac_not_found<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    let loopback_id = get_loopback_id(&realm).await;
    // Unknown device ID produces an error.
    assert_matches!(
        get_mac(loopback_id + 1, &root_interfaces).await,
        Err(fnet_root::InterfacesGetMacError::NotFound)
    );
}

#[netstack_test]
async fn get_mac_loopback<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    let loopback_id = get_loopback_id(&realm).await;
    // Loopback has the all-zero MAC address.
    assert_matches!(
        get_mac(loopback_id, &root_interfaces).await,
        Ok(Some(fnet::MacAddress { octets: [0, 0, 0, 0, 0, 0] }))
    );
}

// Add a pure IP interface to the given device/port, returning the created
// `fuchsia.net.interfaces.admin/Control` handle.
async fn add_pure_ip_interface(
    network_port: &fhardware_network::PortProxy,
    admin_device_control: &fnet_interfaces_admin::DeviceControlProxy,
    interface_name: &str,
) -> fnet_interfaces_admin::ControlProxy {
    let fhardware_network::PortInfo { id, .. } = network_port.get_info().await.expect("get info");
    let port_id = id.expect("port id");

    let (admin_control, server_end) =
        fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
            .expect("create proxy");

    let () = admin_device_control
        .create_interface(
            &port_id,
            server_end,
            &fnet_interfaces_admin::Options {
                name: Some(interface_name.to_string()),
                ..Default::default()
            },
        )
        .expect("create interface");
    admin_control
}

#[netstack_test]
async fn get_mac_pure_ip<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    const PORT_ID: u8 = 7; // Arbitrary nonzero to avoid masking default value assumptions.
    const INTERFACE_NAME: &str = "ihazmac";
    let (tun_device, network_device) = create_tun_device();
    let admin_device_control = install_device(&realm, network_device);
    // Retain `_tun_port` to keep the FIDL channel open.
    let (_tun_port, network_port) = create_ip_tun_port(&tun_device, PORT_ID).await;
    let admin_control =
        add_pure_ip_interface(&network_port, &admin_device_control, INTERFACE_NAME).await;
    let virtual_id = admin_control.get_id().await.expect("get id");
    // Pure IP interfaces do not have MAC addresses.
    assert_matches!(get_mac(virtual_id, &root_interfaces).await, Ok(None));
}

#[netstack_test]
async fn get_mac_netemul_endpoint<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let root_interfaces =
        realm.connect_to_protocol::<fnet_root::InterfacesMarker>().expect("connect to protocol");

    const DEFAULT_MAC: fnet::MacAddress = fidl_mac!("00:03:00:00:00:00");
    let device = sandbox
        .create_endpoint_with(
            "get_mac",
            netemul::new_endpoint_config(netemul::DEFAULT_MTU, Some(DEFAULT_MAC)),
        )
        .await
        .expect("create netemul endpoint");
    // Retain `_control` and `_device_control` to keep the FIDL channel open.
    let (id, _control, _device_control) = device
        .add_to_stack(&realm, netemul::InterfaceConfig::default())
        .await
        .expect("add to stack");
    assert_matches!(get_mac(id.into(), &root_interfaces).await, Ok(Some(DEFAULT_MAC)));
}

#[netstack_test]
async fn get_port<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let debug_interfaces =
        realm.connect_to_protocol::<fnet_debug::InterfacesMarker>().expect("connect to protocol");

    let device = sandbox.create_endpoint(name).await.expect("create netemul endpoint");
    // Retain `_control` and `_device_control` to keep the FIDL channel open.
    let (id, _control, _device_control) = device
        .add_to_stack(&realm, netemul::InterfaceConfig::default())
        .await
        .expect("add to stack");

    {
        let (port, server_end) = fidl::endpoints::create_proxy().expect("create_proxy");
        debug_interfaces.get_port(id, server_end).expect("calling get_port");
        assert_matches!(port.get_info().await, Ok(fhardware_network::PortInfo { .. }));
    }
    {
        let (port, server_end) = fidl::endpoints::create_proxy().expect("create_proxy");
        // Try some bogus identifier.
        debug_interfaces.get_port(id + 100, server_end).expect("calling get_port");
        assert_matches!(
            port.get_info().await,
            Err(fidl::Error::ClientChannelClosed {
                status: zx::Status::NOT_FOUND,
                protocol_name: _
            })
        );
    }
}

// A smoke test for fuchsia.net.debug/Diagnostics.LogDebugInfoToSyslog. This
// test is only asserting that the capability is properly routed, and that the
// call completes. Checking the output in syslog would be too much of a change
// detector.
#[netstack_test]
async fn log_debug_info_to_syslog<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let diagnostics =
        realm.connect_to_protocol::<fnet_debug::DiagnosticsMarker>().expect("connect to protocol");
    diagnostics.log_debug_info_to_syslog().await.expect("calling log_debug_info_to_syslog");
}

#[netstack_test]
async fn get_process_handle_for_inspection<N: Netstack>(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<N, _>(name).expect("create realm");
    let diagnostics =
        realm.connect_to_protocol::<fnet_debug::DiagnosticsMarker>().expect("connect to protocol");
    let process =
        diagnostics.get_process_handle_for_inspection().await.expect("call get_process_handle");
    let zx::HandleBasicInfo { rights, .. } = process.basic_info().expect("get process basic info");
    assert_eq!(rights.bits(), zx::sys::ZX_RIGHT_INSPECT);
}
