// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::vec::Vec;
use assert_matches::assert_matches;

use ip_test_macro::ip_test;
use net_declare::net_mac;
use net_types::{
    ethernet::Mac,
    ip::{AddrSubnet, Ip, IpAddr, IpAddress, IpVersion, Ipv4, Ipv6, Ipv6Addr},
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use netstack3_base::FrameDestination;
use netstack3_core::{
    device::{DeviceId, EthernetCreationProperties, EthernetLinkDevice, RecvEthernetFrameMeta},
    error::NotFoundError,
    ip::{
        AddIpAddrSubnetError, IpDeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
        SlaacConfiguration,
    },
    testutil::{
        new_rng, CtxPairExt as _, FakeBindingsCtx, FakeCoreCtx, FakeCtx, FakeCtxBuilder, TestIpExt,
        DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE, TEST_ADDRS_V4,
    },
    IpExt,
};
use netstack3_device::ethernet;
use netstack3_ip::device::{IpAddressId as _, IpDeviceStateContext};
use packet::{Buf, Serializer as _};
use packet_formats::{
    ethernet::{EthernetFrameBuilder, EthernetFrameLengthCheck, ETHERNET_MIN_BODY_LEN_NO_TAG},
    icmp::IcmpDestUnreachable,
    ip::{IpPacketBuilder, IpProto},
    testdata::{dns_request_v4, dns_request_v6},
    testutil::{
        parse_icmp_packet_in_ip_packet_in_ethernet_frame, parse_ip_packet_in_ethernet_frame,
    },
};
use rand::Rng;
use test_case::test_case;

fn contains_addr<A: IpAddress>(
    core_ctx: &FakeCoreCtx,
    device: &DeviceId<FakeBindingsCtx>,
    addr: SpecifiedAddr<A>,
) -> bool {
    match addr.into() {
        IpAddr::V4(addr) => IpDeviceStateContext::<Ipv4, _>::with_address_ids(
            &mut core_ctx.context(),
            device,
            |mut addrs, _core_ctx| addrs.any(|a| a.addr().addr() == addr.get()),
        ),
        IpAddr::V6(addr) => IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut core_ctx.context(),
            device,
            |mut addrs, _core_ctx| addrs.any(|a| a.addr().addr() == addr.get()),
        ),
    }
}

#[ip_test]
#[test_case(true; "enabled")]
#[test_case(false; "disabled")]
fn test_receive_ip_frame<I: Ip + TestIpExt + IpExt>(enable: bool) {
    // Should only receive a frame if the device is enabled.

    let config = I::TEST_ADDRS;
    let mut ctx = FakeCtx::default();
    let eth_device = ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
        EthernetCreationProperties {
            mac: config.local_mac,
            max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        DEFAULT_INTERFACE_METRIC,
    );
    let device = eth_device.clone().into();
    let mut bytes = match I::VERSION {
        IpVersion::V4 => dns_request_v4::ETHERNET_FRAME,
        IpVersion::V6 => dns_request_v6::ETHERNET_FRAME,
    }
    .bytes
    .to_vec();

    let mac_bytes = config.local_mac.bytes();
    bytes[0..6].copy_from_slice(&mac_bytes);

    let expected_received = if enable {
        ctx.test_api().enable_device(&device);
        1
    } else {
        0
    };

    ctx.core_api()
        .device::<EthernetLinkDevice>()
        .receive_frame(RecvEthernetFrameMeta { device_id: eth_device }, Buf::new(bytes, ..));

    assert_eq!(ctx.core_ctx.common_ip::<I>().counters().receive_ip_packet.get(), expected_received);
}

#[test]
fn initialize_once() {
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: TEST_ADDRS_V4.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);
}

#[ip_test]
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[netstack3_macros::context_ip_bounds(I::OtherVersion, FakeBindingsCtx)]
fn test_set_ip_routing<I: Ip + TestIpExt + IpExt>()
where
    I::OtherVersion: IpExt,
{
    fn check_icmp<I: Ip>(buf: &[u8]) {
        match I::VERSION {
            IpVersion::V4 => {
                let _ = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                    Ipv4,
                    _,
                    IcmpDestUnreachable,
                    _,
                >(buf, EthernetFrameLengthCheck::NoCheck, |_| {})
                .unwrap();
            }
            IpVersion::V6 => {
                let _ = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
                    Ipv6,
                    _,
                    IcmpDestUnreachable,
                    _,
                >(buf, EthernetFrameLengthCheck::NoCheck, |_| {})
                .unwrap();
            }
        }
    }

    let src_ip = I::get_other_ip_address(3);
    let src_mac = UnicastAddr::new(Mac::new([10, 11, 12, 13, 14, 15])).unwrap();
    let config = I::TEST_ADDRS;
    let frame_dst = FrameDestination::Individual { local: true };
    let mut rng = new_rng(70812476915813);
    let mut body: Vec<u8> = core::iter::repeat_with(|| rng.gen()).take(100).collect();
    let buf = Buf::new(&mut body[..], ..)
        .encapsulate(I::PacketBuilder::new(
            src_ip.get(),
            config.remote_ip.get(),
            64,
            IpProto::Tcp.into(),
        ))
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .unwrap_b();

    // Test with netstack no forwarding

    let mut builder = FakeCtxBuilder::with_addrs(config.clone());
    let device_builder_id = 0;
    builder.add_arp_or_ndp_table_entry(device_builder_id, src_ip, src_mac);
    let (mut ctx, device_ids) = builder.build();
    let device: DeviceId<_> = device_ids[device_builder_id].clone().into();

    // Should not be a router (default).
    assert!(!ctx.test_api().is_forwarding_enabled::<I>(&device));
    assert!(!ctx.test_api().is_forwarding_enabled::<I::OtherVersion>(&device));

    // Receiving a packet not destined for the node should only result in a
    // dest unreachable message if routing is enabled.
    ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf.clone());
    assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);

    // Set routing and expect packets to be forwarded.
    ctx.test_api().set_forwarding_enabled::<I>(&device, true);
    assert!(ctx.test_api().is_forwarding_enabled::<I>(&device));
    // Should not update other Ip routing status.
    assert!(!ctx.test_api().is_forwarding_enabled::<I::OtherVersion>(&device));

    // Should route the packet since routing fully enabled (netstack &
    // device).
    ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf.clone());
    {
        let frames = ctx.bindings_ctx.take_ethernet_frames();
        let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
        let (packet_buf, _, _, packet_src_ip, packet_dst_ip, proto, ttl) =
            parse_ip_packet_in_ethernet_frame::<I>(&frame[..], EthernetFrameLengthCheck::NoCheck)
                .unwrap();
        assert_eq!(src_ip.get(), packet_src_ip);
        assert_eq!(config.remote_ip.get(), packet_dst_ip);
        assert_eq!(proto, IpProto::Tcp.into());
        assert_eq!(body, packet_buf);
        assert_eq!(ttl, 63);
    }

    // Test routing a packet to an unknown address.
    let buf_unknown_dest = Buf::new(&mut body[..], ..)
        .encapsulate(I::PacketBuilder::new(
            src_ip.get(),
            // Addr must be remote, otherwise this will cause an NDP/ARP
            // request rather than ICMP unreachable.
            I::get_other_remote_ip_address(10).get(),
            64,
            IpProto::Tcp.into(),
        ))
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .unwrap_b();
    ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf_unknown_dest);
    let frames = ctx.bindings_ctx.take_ethernet_frames();
    let (_dev, frame) = assert_matches!(&frames[..], [frame] => frame);
    check_icmp::<I>(&frame);

    // Attempt to unset router
    ctx.test_api().set_forwarding_enabled::<I>(&device, false);
    assert!(!ctx.test_api().is_forwarding_enabled::<I>(&device));
    assert!(!ctx.test_api().is_forwarding_enabled::<I::OtherVersion>(&device));

    // Should not route packets anymore
    ctx.test_api().receive_ip_packet::<I, _>(&device, Some(frame_dst), buf);
    assert_matches!(ctx.bindings_ctx.take_ethernet_frames()[..], []);
}

#[ip_test]
#[test_case(UnicastAddr::new(net_mac!("12:13:14:15:16:17")).unwrap(), true; "unicast")]
#[test_case(MulticastAddr::new(net_mac!("13:14:15:16:17:18")).unwrap(), false; "multicast")]
fn test_promiscuous_mode<I: Ip + TestIpExt + IpExt>(
    other_mac: impl Witness<Mac>,
    is_other_host: bool,
) {
    // Test that frames not destined for a device will still be accepted
    // when the device is put into promiscuous mode. In all cases, frames
    // that are destined for a device must always be accepted.

    let config = I::TEST_ADDRS;
    let (mut ctx, device_ids) = FakeCtxBuilder::with_addrs(config.clone()).build();
    let eth_device = &device_ids[0];

    let buf = Buf::new(Vec::new(), ..)
        .encapsulate(I::PacketBuilder::new(
            config.remote_ip.get(),
            config.local_ip.get(),
            64,
            IpProto::Tcp.into(),
        ))
        .encapsulate(EthernetFrameBuilder::new(
            config.remote_mac.get(),
            config.local_mac.get(),
            I::ETHER_TYPE,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .unwrap_b();

    // Accept packet destined for this device if promiscuous mode is off.
    ethernet::set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, false);
    ctx.core_api()
        .device::<EthernetLinkDevice>()
        .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

    assert_eq!(ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet.get(), 1);
    assert_eq!(
        ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet_other_host.get(),
        0
    );

    // Accept packet destined for this device if promiscuous mode is on.
    ethernet::set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, true);
    ctx.core_api()
        .device::<EthernetLinkDevice>()
        .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf);

    assert_eq!(ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet.get(), 2);
    assert_eq!(
        ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet_other_host.get(),
        0
    );

    let buf = Buf::new(Vec::new(), ..)
        .encapsulate(I::PacketBuilder::new(
            config.remote_ip.get(),
            config.local_ip.get(),
            64,
            IpProto::Tcp.into(),
        ))
        .encapsulate(EthernetFrameBuilder::new(
            config.remote_mac.get(),
            other_mac.get(),
            I::ETHER_TYPE,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        ))
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .unwrap_b();

    // Reject packet not destined for this device if promiscuous mode is
    // off.
    ethernet::set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, false);
    ctx.core_api()
        .device::<EthernetLinkDevice>()
        .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf.clone());

    assert_eq!(ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet.get(), 2);
    assert_eq!(
        ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet_other_host.get(),
        0
    );

    // Accept packet not destined for this device if promiscuous mode is on.
    ethernet::set_promiscuous_mode(&mut ctx.core_ctx(), &eth_device, true);
    ctx.core_api()
        .device::<EthernetLinkDevice>()
        .receive_frame(RecvEthernetFrameMeta { device_id: eth_device.clone() }, buf);

    assert_eq!(ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet.get(), 3);
    assert_eq!(
        ctx.core_ctx.common_ip::<I>().counters().dispatch_receive_ip_packet_other_host.get(),
        u64::from(is_other_host)
    );
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test]
fn test_add_remove_ip_addresses<I: Ip + TestIpExt + IpExt>() {
    let config = I::TEST_ADDRS;
    let mut ctx = FakeCtx::default();

    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: config.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

    ctx.test_api().enable_device(&device);

    let ip1 = I::get_other_ip_address(1);
    let ip2 = I::get_other_ip_address(2);
    let ip3 = I::get_other_ip_address(3);

    let prefix = I::Addr::BYTES * 8;
    let as1 = AddrSubnet::new(ip1.get(), prefix).unwrap();
    let as2 = AddrSubnet::new(ip2.get(), prefix).unwrap();

    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Add ip1 (ok)
    ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as1).unwrap();
    assert!(contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Add ip2 (ok)
    ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as2).unwrap();
    assert!(contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Del ip1 (ok)
    assert_eq!(
        ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1).unwrap().into_removed(),
        as1
    );
    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Del ip1 again (ip1 not found)
    assert_matches!(ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1), Err(NotFoundError));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Add ip2 again (ip2 already exists)
    assert_eq!(
        ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as2),
        Err(AddIpAddrSubnetError::Exists),
    );
    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));

    // Add ip2 with different subnet (ip2 already exists)
    assert_eq!(
        ctx.core_api()
            .device_ip::<I>()
            .add_ip_addr_subnet(&device, AddrSubnet::new(ip2.get(), prefix - 1).unwrap()),
        Err(AddIpAddrSubnetError::Exists),
    );
    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip3));
}

fn receive_simple_ip_packet_test<A: IpAddress>(
    ctx: &mut FakeCtx,
    device: &DeviceId<FakeBindingsCtx>,
    src_ip: A,
    dst_ip: A,
    expected: u64,
) where
    A::Version: TestIpExt + IpExt,
{
    let buf = Buf::new(Vec::new(), ..)
        .encapsulate(
            <<A::Version as packet_formats::ip::IpExt>::PacketBuilder as IpPacketBuilder<_>>::new(
                src_ip,
                dst_ip,
                64,
                IpProto::Tcp.into(),
            ),
        )
        .serialize_vec_outer()
        .ok()
        .unwrap()
        .into_inner();

    ctx.test_api().receive_ip_packet::<A::Version, _>(
        device,
        Some(FrameDestination::Individual { local: true }),
        buf,
    );
    assert_eq!(
        ctx.core_ctx.common_ip::<A::Version>().counters().dispatch_receive_ip_packet.get(),
        expected
    );
}

#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[ip_test]
fn test_multiple_ip_addresses<I: Ip + TestIpExt + IpExt>() {
    let config = I::TEST_ADDRS;
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: config.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);

    let ip1 = I::get_other_ip_address(1);
    let ip2 = I::get_other_ip_address(2);
    let from_ip = I::get_other_ip_address(3).get();

    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip2));

    // Should not receive packets on any IP.
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 0);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 0);

    let as1 = AddrSubnet::new(ip1.get(), I::Addr::BYTES * 8).unwrap();
    // Add ip1 to device.
    ctx.core_api().device_ip::<I>().add_ip_addr_subnet(&device, as1).unwrap();
    assert!(contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(!contains_addr(&ctx.core_ctx, &device, ip2));

    // Should receive packets on ip1 but not ip2
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 1);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 1);

    // Add ip2 to device.
    ctx.core_api()
        .device_ip::<I>()
        .add_ip_addr_subnet(&device, AddrSubnet::new(ip2.get(), I::Addr::BYTES * 8).unwrap())
        .unwrap();
    assert!(contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));

    // Should receive packets on both ips
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 2);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 3);

    // Remove ip1
    assert_eq!(
        ctx.core_api().device_ip::<I>().del_ip_addr(&device, ip1).unwrap().into_removed(),
        as1
    );
    assert!(!contains_addr(&ctx.core_ctx, &device, ip1));
    assert!(contains_addr(&ctx.core_ctx, &device, ip2));

    // Should receive packets on ip2
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 3);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 4);
}

/// Test that we can join and leave a multicast group, but we only truly
/// leave it after calling `leave_ip_multicast` the same number of times as
/// `join_ip_multicast`.
#[ip_test]
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
fn test_ip_join_leave_multicast_addr_ref_count<I: Ip + TestIpExt + IpExt>() {
    let config = I::TEST_ADDRS;
    let mut ctx = FakeCtx::default();

    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: config.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);
    let multicast_addr = I::get_multicast_addr(3);
    let mut test_api = ctx.test_api();

    // Should not be in the multicast group yet.
    assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

    // Join the multicast group.
    test_api.join_ip_multicast(&device, multicast_addr);
    assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

    // Leave the multicast group.
    test_api.leave_ip_multicast(&device, multicast_addr);
    assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

    // Join the multicst group.
    test_api.join_ip_multicast(&device, multicast_addr);
    assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

    // Join it again...
    test_api.join_ip_multicast(&device, multicast_addr);
    assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

    // Leave it (still in it because we joined twice).
    test_api.leave_ip_multicast(&device, multicast_addr);
    assert!(test_api.is_in_ip_multicast(&device, multicast_addr));

    // Leave it again... (actually left now).
    test_api.leave_ip_multicast(&device, multicast_addr);
    assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));
}

/// Test leaving a multicast group a device has not yet joined.
///
/// # Panics
///
/// This method should always panic as leaving an unjoined multicast group
/// is a panic condition.
#[ip_test]
#[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx)]
#[should_panic(expected = "attempted to leave IP multicast group we were not a member of:")]
fn test_ip_leave_unjoined_multicast<I: Ip + TestIpExt + IpExt>() {
    let config = I::TEST_ADDRS;
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: config.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);
    let multicast_addr = I::get_multicast_addr(3);

    let mut test_api = ctx.test_api();

    // Should not be in the multicast group yet.
    assert!(!test_api.is_in_ip_multicast(&device, multicast_addr));

    // Leave it (this should panic).
    test_api.leave_ip_multicast(&device, multicast_addr);
}

#[test]
fn test_ipv6_duplicate_solicited_node_address() {
    // Test that we still receive packets destined to a solicited-node
    // multicast address of an IP address we deleted because another
    // (distinct) IP address that is still assigned uses the same
    // solicited-node multicast address.

    let config = Ipv6::TEST_ADDRS;
    let mut ctx = FakeCtx::default();
    let device = ctx
        .core_api()
        .device::<EthernetLinkDevice>()
        .add_device_with_default_state(
            EthernetCreationProperties {
                mac: config.local_mac,
                max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            },
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
    ctx.test_api().enable_device(&device);

    let ip1 = SpecifiedAddr::new(Ipv6Addr::new([0, 0, 0, 1, 0, 0, 0, 1])).unwrap();
    let ip2 = SpecifiedAddr::new(Ipv6Addr::new([0, 0, 0, 2, 0, 0, 0, 1])).unwrap();
    let from_ip = Ipv6Addr::new([0, 0, 0, 3, 0, 0, 0, 1]);

    // ip1 and ip2 are not equal but their solicited node addresses are the
    // same.
    assert_ne!(ip1, ip2);
    assert_eq!(ip1.to_solicited_node_address(), ip2.to_solicited_node_address());
    let sn_addr = ip1.to_solicited_node_address().get();

    let addr_sub1 = AddrSubnet::new(ip1.get(), 64).unwrap();
    let addr_sub2 = AddrSubnet::new(ip2.get(), 64).unwrap();

    assert_eq!(ctx.core_ctx.common_ip::<Ipv6>().counters().dispatch_receive_ip_packet.get(), 0);

    // Add ip1 to the device.
    //
    // Should get packets destined for the solicited node address and ip1.
    ctx.core_api().device_ip::<Ipv6>().add_ip_addr_subnet(&device, addr_sub1).unwrap();

    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 1);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 1);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 2);

    // Add ip2 to the device.
    //
    // Should get packets destined for the solicited node address, ip1 and
    // ip2.
    ctx.core_api().device_ip::<Ipv6>().add_ip_addr_subnet(&device, addr_sub2).unwrap();
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 3);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 4);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 5);

    // Remove ip1 from the device.
    //
    // Should get packets destined for the solicited node address and ip2.
    assert_eq!(
        ctx.core_api().device_ip::<Ipv6>().del_ip_addr(&device, ip1).unwrap().into_removed(),
        addr_sub1
    );
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip1.get(), 5);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, ip2.get(), 6);
    receive_simple_ip_packet_test(&mut ctx, &device, from_ip, sn_addr, 7);
}

#[test]
fn test_add_ip_addr_subnet_link_local() {
    // Test that `add_ip_addr_subnet` allows link-local addresses.

    let config = Ipv6::TEST_ADDRS;
    let mut ctx = FakeCtx::default();

    let eth_device = ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
        EthernetCreationProperties {
            mac: config.local_mac,
            max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        DEFAULT_INTERFACE_METRIC,
    );
    let device = eth_device.clone().into();
    let eth_device = eth_device.device_state(&ctx.core_ctx.device().origin);

    // Enable the device and configure it to generate a link-local address.
    let _: Ipv6DeviceConfigurationUpdate = ctx
        .core_api()
        .device_ip::<Ipv6>()
        .update_configuration(
            &device,
            Ipv6DeviceConfigurationUpdate {
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();

    // Verify that there is a single assigned address.
    assert_eq!(
        eth_device
            .ip
            .ip_state::<Ipv6>()
            .addrs()
            .read()
            .iter()
            .map(|entry| entry.addr_sub().addr().get())
            .collect::<Vec<UnicastAddr<_>>>(),
        [config.local_mac.to_ipv6_link_local().addr().get()]
    );
    ctx.core_api()
        .device_ip::<Ipv6>()
        .add_ip_addr_subnet(
            &device,
            AddrSubnet::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network(), 128).unwrap(),
        )
        .unwrap();
    // Assert that the new address got added.
    let addr_subs: Vec<_> = eth_device
        .ip
        .ip_state::<Ipv6>()
        .addrs()
        .read()
        .iter()
        .map(|entry| entry.addr_sub().addr().into())
        .collect::<Vec<Ipv6Addr>>();
    assert_eq!(
        addr_subs,
        [
            config.local_mac.to_ipv6_link_local().addr().get(),
            Ipv6::LINK_LOCAL_UNICAST_SUBNET.network()
        ]
    );
}
