// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use super::{
    devices::{DeviceSpecificInfo, DynamicCommonInfo, DynamicNetdeviceInfo, NetdeviceInfo},
    BindingsNonSyncCtxImpl, Ctx, DeviceIdExt, StackTime, StaticCommonInfo,
};
use fuchsia_inspect::ArrayProperty as _;
use net_types::{
    ip::{Ip, IpVersion, Ipv4, Ipv6},
    Witness as _,
};
use netstack3_core::{
    device::{self, DeviceId},
    ip,
    transport::tcp,
};
use std::{fmt, string::ToString as _};

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    /// Convert a [`tcp::socket::SocketId`] into a unique integer.
    ///
    /// Guarantees that no two unique `SocketId`s (even for different IP
    /// versions) will have have the same output value.
    fn transform_id<I: Ip>(id: tcp::socket::SocketId<I>) -> usize {
        let unique_for_ip_version: usize = id.into();
        2 * unique_for_ip_version
            + match I::VERSION {
                IpVersion::V4 => 0,
                IpVersion::V6 => 1,
            }
    }

    struct Visitor(fuchsia_inspect::Inspector);
    impl tcp::socket::InfoVisitor for &'_ mut Visitor {
        type VisitResult = ();
        fn visit<I: Ip, D: fmt::Display>(
            self,
            per_socket: impl Iterator<Item = tcp::socket::SocketStats<I, D>>,
        ) -> Self::VisitResult {
            let Visitor(inspector) = self;
            for socket in per_socket {
                let tcp::socket::SocketStats { id, local, remote } = socket;
                inspector.root().record_child(format!("{}", transform_id(id)), |node| {
                    node.record_string("TransportProtocol", "TCP");
                    node.record_string(
                        "NetworkProtocol",
                        match I::VERSION {
                            IpVersion::V4 => "IPv4",
                            IpVersion::V6 => "IPv6",
                        },
                    );
                    node.record_string(
                        "LocalAddress",
                        local.map_or("[NOT BOUND]".into(), |socket| format!("{}", socket)),
                    );
                    node.record_string(
                        "RemoteAddress",
                        remote.map_or("[NOT CONNECTED]".into(), |socket| format!("{}", socket)),
                    )
                })
            }
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let mut visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    tcp::socket::with_info::<Ipv4, _, _>(sync_ctx, &mut visitor);
    tcp::socket::with_info::<Ipv6, _, _>(sync_ctx, &mut visitor);
    let Visitor(inspector) = visitor;
    inspector
}

/// Publishes netstack3 routing table diagnostics data to Inspect.
pub(crate) fn routes(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    struct Visitor(fuchsia_inspect::Inspector);
    impl<'a> ip::forwarding::RoutesVisitor<'a, BindingsNonSyncCtxImpl> for &'_ mut Visitor {
        type VisitResult = ();
        fn visit<'b, I: Ip>(
            self,
            per_route: impl Iterator<Item = &'b ip::types::Entry<I::Addr, DeviceId<BindingsNonSyncCtxImpl>>>
                + 'b,
        ) -> Self::VisitResult
        where
            'a: 'b,
        {
            let Visitor(inspector) = self;
            for (i, route) in per_route.enumerate() {
                inspector.root().record_child(format!("{}", i), |node| {
                    let ip::types::Entry { subnet, device, gateway, metric } = route;
                    node.record_string("Destination", format!("{}", subnet));
                    node.record_uint(
                        "InterfaceId",
                        device.external_state().static_common_info().binding_id.into(),
                    );
                    match gateway {
                        Some(gateway) => {
                            node.record_string("Gateway", format!("{}", gateway));
                        }
                        None => {
                            node.record_string("Gateway", "[NONE]");
                        }
                    }
                    match metric {
                        ip::types::Metric::MetricTracksInterface(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", true);
                        }
                        ip::types::Metric::ExplicitMetric(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", false);
                        }
                    }
                })
            }
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let mut visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    ip::forwarding::with_routes::<Ipv4, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    ip::forwarding::with_routes::<Ipv6, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    let Visitor(inspector) = visitor;
    inspector
}

pub(crate) fn devices(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    struct Visitor(fuchsia_inspect::Inspector);
    impl device::DevicesVisitor<BindingsNonSyncCtxImpl> for Visitor {
        fn visit_devices(
            &self,
            devices: impl Iterator<Item = device::InspectDeviceState<BindingsNonSyncCtxImpl>>,
        ) {
            use crate::bindings::DeviceIdExt as _;
            let Self(inspector) = self;
            for device::InspectDeviceState { device_id, addresses } in devices {
                let external_state = device_id.external_state();
                let StaticCommonInfo { binding_id, name, tx_notifier: _ } =
                    external_state.static_common_info();
                inspector.root().record_child(format!("{binding_id}"), |node| {
                    node.record_string("Name", &name);
                    node.record_uint("InterfaceId", (*binding_id).into());
                    let ip_addresses = node.create_string_array("IpAddresses", addresses.len());
                    for (j, address) in addresses.iter().enumerate() {
                        ip_addresses.set(j, address.to_string());
                    }
                    node.record(ip_addresses);
                    external_state.with_common_info(
                        |DynamicCommonInfo {
                             admin_enabled,
                             mtu,
                             addresses: _,
                             control_hook: _,
                             events: _,
                         }| {
                            node.record_bool("AdminEnabled", *admin_enabled);
                            node.record_uint("MTU", mtu.get().into());
                        },
                    );
                    match external_state {
                        DeviceSpecificInfo::Netdevice(
                            info @ NetdeviceInfo {
                                mac,
                                dynamic: _,
                                handler: _,
                                static_common_info: _,
                            },
                        ) => {
                            node.record_bool("Loopback", false);
                            node.record_child("NetworkDevice", |node| {
                                node.record_string("MacAddress", mac.get().to_string());
                                info.with_dynamic_info(
                                    |DynamicNetdeviceInfo { phy_up, common_info: _ }| {
                                        node.record_bool("PhyUp", *phy_up);
                                    },
                                );
                            });
                        }
                        DeviceSpecificInfo::Loopback(_info) => {
                            node.record_bool("Loopback", true);
                        }
                    }
                })
            }
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    device::inspect_devices::<BindingsNonSyncCtxImpl, _>(sync_ctx, &visitor);
    let Visitor(inspector) = visitor;
    inspector
}

pub(crate) fn neighbors(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    struct Visitor(fuchsia_inspect::Inspector);
    impl device::NeighborVisitor<BindingsNonSyncCtxImpl, StackTime> for Visitor {
        fn visit_neighbors<LinkAddress: fmt::Debug>(
            &self,
            device: DeviceId<BindingsNonSyncCtxImpl>,
            neighbors: impl Iterator<
                Item = ip::device::nud::NeighborStateInspect<LinkAddress, StackTime>,
            >,
        ) {
            use crate::bindings::DeviceIdExt as _;
            let Self(inspector) = self;
            let device_state = device.external_state();
            let name = &device_state.static_common_info().name;
            inspector.root().record_child(format!("{name}"), |node| {
                for (i, neighbor) in neighbors.enumerate() {
                    let ip::device::nud::NeighborStateInspect {
                        state,
                        ip_address,
                        link_address,
                        last_confirmed_at,
                    } = neighbor;
                    node.record_child(format!("{i}"), |node| {
                        node.record_string("State", state);
                        node.record_string("IpAddress", format!("{}", ip_address));
                        if let Some(link_address) = link_address {
                            node.record_string("LinkAddress", format!("{:?}", link_address));
                        };
                        if let Some(StackTime(last_confirmed_at)) = last_confirmed_at {
                            node.record_int("LastConfirmedAt", last_confirmed_at.into_nanos());
                        }
                    })
                }
            });
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    device::inspect_neighbors::<BindingsNonSyncCtxImpl, _>(sync_ctx, &visitor);
    let Visitor(inspector) = visitor;
    inspector
}
