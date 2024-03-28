// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, num::NonZeroU16};

use net_types::ip::{GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use packet::{Buf, Nested, ParseBuffer, Serializer};
use packet_formats::{
    icmp::{
        IcmpDestUnreachable, IcmpEchoReply, IcmpEchoRequest, IcmpMessage, IcmpPacketBuilder,
        IcmpParseArgs, IcmpTimeExceeded, Icmpv4Packet, Icmpv4ParameterProblem,
        Icmpv4TimestampReply, Icmpv6Packet, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
    },
    ip::{IpExt, IpPacket as _, IpProto, Ipv4Proto, Ipv6Proto},
    ipv4::Ipv4Packet,
    ipv6::Ipv6Packet,
    tcp::{TcpParseArgs, TcpSegment, TcpSegmentBuilderWithOptions},
    udp::{UdpPacket, UdpPacketBuilder, UdpParseArgs},
};
use zerocopy::ByteSliceMut;

/// An IP packet that provides header inspection.
//
// TODO(https://fxbug.dev/321013529): provide the necessary methods and associated
// type for packet header modification.
pub trait IpPacket<I: IpExt> {
    /// The type that provides access to transport-layer header inspection, if a
    /// transport header is contained in the body of the IP packet.
    type TransportPacket<'a>: MaybeTransportPacket
    where
        Self: 'a;

    /// The source IP address of the packet.
    fn src_addr(&self) -> I::Addr;

    /// The destination IP address of the packet.
    fn dst_addr(&self) -> I::Addr;

    /// The IP protocol of the packet.
    fn protocol(&self) -> I::Proto;

    /// Returns a type that provides access to the transport-layer packet contained
    /// in the body of the IP packet, if one exists.
    ///
    /// This method returns an owned type parameterized on a lifetime that is tied
    /// to the lifetime of Self, rather than, for example, a reference to a
    /// non-parameterized type (`&Self::TransportPacket`). This is because
    /// implementors may need to parse the transport header from the body of the IP
    /// packet and materialize the results into a new type when this is called, but
    /// that type may also need to retain a reference to the backing buffer in order
    /// to modify the transport header.
    fn transport_packet<'a>(&'a self) -> Self::TransportPacket<'a>;
}

/// A payload of an IP packet that may be a valid transport layer packet.
///
/// This trait exists to allow bubbling up the trait bound that a serializer
/// type implement `MaybeTransportPacket` from the IP socket layer to, for
/// example, the ICMP layer, where it can be implemented separately on each
/// concrete ICMP message type depending on whether it supports packet header
/// inspection.
pub trait MaybeTransportPacket {
    /// The type that provides access to transport-layer header inspection, if this
    /// is indeed a valid transport packet.
    type TransportPacket: TransportPacket;

    /// Optionally returns a type that provides access to this transport-layer
    /// packet.
    fn transport_packet(&self) -> Option<&Self::TransportPacket>;
}

/// A serializer that may also be a valid transport layer packet.
pub trait TransportPacketSerializer: Serializer + MaybeTransportPacket {}

impl<S: Serializer + MaybeTransportPacket> TransportPacketSerializer for S {}

impl<T: ?Sized> MaybeTransportPacket for &T
where
    T: MaybeTransportPacket,
{
    type TransportPacket = T::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        (*self).transport_packet()
    }
}

impl<T: TransportPacket> MaybeTransportPacket for Option<T> {
    type TransportPacket = T;

    fn transport_packet(&self) -> Option<&T> {
        self.as_ref()
    }
}

/// A transport layer packet that provides header inspection.
//
// TODO(https://fxbug.dev/321013529): provide the necessary methods and associated
// type for packet header modification.
pub trait TransportPacket {
    /// The source port or identifier of the packet.
    fn src_port(&self) -> u16;

    /// The destination port or identifier of the packet.
    fn dst_port(&self) -> u16;
}

impl<B: ByteSliceMut + ParseBuffer> IpPacket<Ipv4> for Ipv4Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> Ipv4Addr {
        self.src_ip()
    }

    fn dst_addr(&self) -> Ipv4Addr {
        self.dst_ip()
    }

    fn protocol(&self) -> Ipv4Proto {
        self.proto()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        parse_transport_header_in_ipv4_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            self.body(),
        )
    }
}

impl<B: ByteSliceMut + ParseBuffer> IpPacket<Ipv6> for Ipv6Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> Ipv6Addr {
        self.src_ip()
    }

    fn dst_addr(&self) -> Ipv6Addr {
        self.dst_ip()
    }

    fn protocol(&self) -> Ipv6Proto {
        self.proto()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        parse_transport_header_in_ipv6_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            self.body(),
        )
    }
}

/// An incoming IP packet that has been parsed into its constituent parts for
/// either local delivery or forwarding.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct RxPacket<'a, B, I: IpExt> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    body: &'a B,
}

impl<'a, B: ParseBuffer, I: IpExt> RxPacket<'a, B, I> {
    /// Create a new [`RxPacket`] from its IP header fields and payload.
    pub fn new(src_addr: I::Addr, dst_addr: I::Addr, protocol: I::Proto, body: &'a B) -> Self {
        Self { src_addr, dst_addr, protocol, body }
    }
}

impl<B: ParseBuffer, I: IpExt> IpPacket<I> for RxPacket<'_, B, I> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        I::map_ip(
            self,
            |RxPacket { src_addr, dst_addr, protocol, body }| {
                parse_transport_header_in_ipv4_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    Buf::new(body, ..),
                )
            },
            |RxPacket { src_addr, dst_addr, protocol, body }| {
                parse_transport_header_in_ipv6_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    Buf::new(body, ..),
                )
            },
        )
    }
}

/// An outgoing IP packet that has not yet been wrapped into an outer serializer
/// type.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct TxPacket<'a, S: MaybeTransportPacket, I: IpExt> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    serializer: &'a S,
}

impl<'a, S: MaybeTransportPacket, I: IpExt> TxPacket<'a, S, I> {
    /// Create a new [`TxPacket`] from its IP header fields and payload.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        serializer: &'a S,
    ) -> Self {
        Self { src_addr, dst_addr, protocol, serializer }
    }
}

impl<S: MaybeTransportPacket, I: IpExt> IpPacket<I> for TxPacket<'_, S, I> {
    type TransportPacket<'a> = &'a S where Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        self.serializer
    }
}

impl<T: ?Sized> TransportPacket for &T
where
    T: TransportPacket,
{
    fn src_port(&self) -> u16 {
        (*self).src_port()
    }

    fn dst_port(&self) -> u16 {
        (*self).dst_port()
    }
}

impl<A: IpAddress, Inner> MaybeTransportPacket for Nested<Inner, UdpPacketBuilder<A>> {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

impl<A: IpAddress, Inner> TransportPacket for Nested<Inner, UdpPacketBuilder<A>> {
    fn src_port(&self) -> u16 {
        self.outer().src_port().map_or(0, NonZeroU16::get)
    }

    fn dst_port(&self) -> u16 {
        self.outer().dst_port().map_or(0, NonZeroU16::get)
    }
}

impl<A: IpAddress, O, Inner> MaybeTransportPacket
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>>
{
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

impl<A: IpAddress, O, Inner> TransportPacket for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>> {
    fn src_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::src_port(self.outer()).map_or(0, NonZeroU16::get)
    }

    fn dst_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::dst_port(self.outer()).map_or(0, NonZeroU16::get)
    }
}

impl<I: IpExt, Inner, M: IcmpMessage<I> + MaybeTransportPacket> MaybeTransportPacket
    for Nested<Inner, IcmpPacketBuilder<I, M>>
{
    type TransportPacket = M::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        self.outer().message().transport_packet()
    }
}

impl MaybeTransportPacket for IcmpEchoReply {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

// TODO(https://fxbug.dev/328064082): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl TransportPacket for IcmpEchoReply {
    fn src_port(&self) -> u16 {
        0
    }

    fn dst_port(&self) -> u16 {
        self.id()
    }
}

impl MaybeTransportPacket for IcmpEchoRequest {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

// TODO(https://fxbug.dev/328064082): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl TransportPacket for IcmpEchoRequest {
    fn src_port(&self) -> u16 {
        self.id()
    }

    fn dst_port(&self) -> u16 {
        0
    }
}

impl TransportPacket for Never {
    fn src_port(&self) -> u16 {
        match *self {}
    }

    fn dst_port(&self) -> u16 {
        match *self {}
    }
}

// TODO(https://fxbug.dev/328057704): parse the IP packet contained in the ICMP
// error message payload so NAT can be applied to it.
impl MaybeTransportPacket for Icmpv4TimestampReply {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

// Transport layer packet inspection is not currently supported for any ICMP
// error message types.
//
// TODO(https://fxbug.dev/328057704): parse the IP packet contained in the ICMP
// error message payload so NAT can be applied to it.
macro_rules! icmp_error_message {
    ($message:ty) => {
        impl MaybeTransportPacket for $message {
            type TransportPacket = Never;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                None
            }
        }
    };
}

icmp_error_message!(IcmpDestUnreachable);

icmp_error_message!(IcmpTimeExceeded);

icmp_error_message!(Icmpv4ParameterProblem);

icmp_error_message!(Icmpv6ParameterProblem);

icmp_error_message!(Icmpv6PacketTooBig);

#[derive(GenericOverIp)]
#[generic_over_ip()]
pub struct ParsedTransportHeader {
    src_port: u16,
    dst_port: u16,
}

impl TransportPacket for ParsedTransportHeader {
    fn src_port(&self) -> u16 {
        self.src_port
    }

    fn dst_port(&self) -> u16 {
        self.dst_port
    }
}

fn parse_transport_header_in_ipv4_packet<B: ParseBuffer>(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    proto: Ipv4Proto,
    body: B,
) -> Option<ParsedTransportHeader> {
    match proto {
        Ipv4Proto::Proto(IpProto::Udp) => parse_udp_header(body, UdpParseArgs::new(src_ip, dst_ip)),
        Ipv4Proto::Proto(IpProto::Tcp) => parse_tcp_header(body, TcpParseArgs::new(src_ip, dst_ip)),
        Ipv4Proto::Icmp => parse_icmpv4_header(body, IcmpParseArgs::new(src_ip, dst_ip)),
        Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
    }
}

fn parse_transport_header_in_ipv6_packet<B: ParseBuffer>(
    src_ip: Ipv6Addr,
    dst_ip: Ipv6Addr,
    proto: Ipv6Proto,
    body: B,
) -> Option<ParsedTransportHeader> {
    match proto {
        Ipv6Proto::Proto(IpProto::Udp) => parse_udp_header(body, UdpParseArgs::new(src_ip, dst_ip)),
        Ipv6Proto::Proto(IpProto::Tcp) => parse_tcp_header(body, TcpParseArgs::new(src_ip, dst_ip)),
        Ipv6Proto::Icmpv6 => parse_icmpv6_header(body, IcmpParseArgs::new(src_ip, dst_ip)),
        Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => None,
    }
}

fn parse_udp_header<B: ParseBuffer, A: IpAddress>(
    mut body: B,
    args: UdpParseArgs<A>,
) -> Option<ParsedTransportHeader> {
    let packet = body.parse_with::<_, UdpPacket<_>>(args).ok()?;
    Some(ParsedTransportHeader {
        src_port: packet.src_port().map(|port| port.get()).unwrap_or(0),
        dst_port: packet.dst_port().get(),
    })
}

fn parse_tcp_header<B: ParseBuffer, A: IpAddress>(
    mut body: B,
    args: TcpParseArgs<A>,
) -> Option<ParsedTransportHeader> {
    let packet = body.parse_with::<_, TcpSegment<_>>(args).ok()?;
    Some(ParsedTransportHeader {
        src_port: packet.src_port().get(),
        dst_port: packet.dst_port().get(),
    })
}

fn parse_icmpv4_header<B: ParseBuffer>(
    mut body: B,
    args: IcmpParseArgs<Ipv4Addr>,
) -> Option<ParsedTransportHeader> {
    let packet = body.parse_with::<_, Icmpv4Packet<_>>(args).ok()?;
    let (src_port, dst_port) = match packet {
        Icmpv4Packet::EchoRequest(packet) => Some((packet.message().id(), 0)),
        Icmpv4Packet::EchoReply(packet) => Some((0, packet.message().id())),
        // TODO(https://fxbug.dev/328057704): parse packet contained in ICMP error
        // message payload so NAT can be applied to it.
        Icmpv4Packet::DestUnreachable(_)
        | Icmpv4Packet::Redirect(_)
        | Icmpv4Packet::TimeExceeded(_)
        | Icmpv4Packet::ParameterProblem(_)
        | Icmpv4Packet::TimestampRequest(_)
        | Icmpv4Packet::TimestampReply(_) => None,
    }?;
    Some(ParsedTransportHeader { src_port, dst_port })
}

fn parse_icmpv6_header<B: ParseBuffer>(
    mut body: B,
    args: IcmpParseArgs<Ipv6Addr>,
) -> Option<ParsedTransportHeader> {
    let packet = body.parse_with::<_, Icmpv6Packet<_>>(args).ok()?;
    let (src_port, dst_port) = match packet {
        Icmpv6Packet::EchoRequest(packet) => Some((packet.message().id(), 0)),
        Icmpv6Packet::EchoReply(packet) => Some((0, packet.message().id())),
        // TODO(https://fxbug.dev/328057704): parse packet contained in ICMP error
        // message payload so NAT can be applied to it.
        Icmpv6Packet::DestUnreachable(_)
        | Icmpv6Packet::PacketTooBig(_)
        | Icmpv6Packet::TimeExceeded(_)
        | Icmpv6Packet::ParameterProblem(_)
        | Icmpv6Packet::Ndp(_)
        | Icmpv6Packet::Mld(_) => None,
    }?;
    Some(ParsedTransportHeader { src_port, dst_port })
}

#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    use packet::{BufferMut, EmptyBuf, InnerSerializer};

    use super::*;

    impl<B: BufferMut> MaybeTransportPacket for Nested<B, ()> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            None
        }
    }

    impl MaybeTransportPacket for InnerSerializer<&[u8], EmptyBuf> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            None
        }
    }

    #[cfg(test)]
    pub(crate) mod internal {
        use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
        use net_types::ip::{IpInvariant, Subnet};

        use super::*;

        pub trait TestIpExt: IpExt {
            const SRC_IP: Self::Addr;
            const DST_IP: Self::Addr;
            const IP_OUTSIDE_SUBNET: Self::Addr;
            const SUBNET: Subnet<Self::Addr>;
        }

        impl TestIpExt for Ipv4 {
            const SRC_IP: Self::Addr = net_ip_v4!("192.0.2.1");
            const DST_IP: Self::Addr = net_ip_v4!("192.0.2.2");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v4!("192.0.2.4");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/30");
        }

        impl TestIpExt for Ipv6 {
            const SRC_IP: Self::Addr = net_ip_v6!("2001:db8::1");
            const DST_IP: Self::Addr = net_ip_v6!("2001:db8::2");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v6!("2001:db8::4");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::/126");
        }

        pub struct FakeIpPacket<I: IpExt, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            pub src_ip: I::Addr,
            pub dst_ip: I::Addr,
            pub body: T,
        }

        pub trait TransportPacketExt<I: IpExt>: MaybeTransportPacket {
            fn proto() -> I::Proto;
        }

        impl<I: IpExt, T> IpPacket<I> for FakeIpPacket<I, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            type TransportPacket<'a> = &'a T where T: 'a;

            fn src_addr(&self) -> I::Addr {
                self.src_ip
            }

            fn dst_addr(&self) -> I::Addr {
                self.dst_ip
            }

            fn protocol(&self) -> I::Proto {
                <&T>::proto()
            }

            fn transport_packet(&self) -> Self::TransportPacket<'_> {
                &self.body
            }
        }

        pub struct FakeTcpSegment {
            pub src_port: u16,
            pub dst_port: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeTcpSegment {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Proto(IpProto::Tcp),
                    |IpInvariant(())| Ipv6Proto::Proto(IpProto::Tcp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeTcpSegment {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeTcpSegment {
            fn src_port(&self) -> u16 {
                self.src_port
            }

            fn dst_port(&self) -> u16 {
                self.dst_port
            }
        }

        pub struct FakeUdpPacket {
            pub src_port: u16,
            pub dst_port: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeUdpPacket {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Proto(IpProto::Udp),
                    |IpInvariant(())| Ipv6Proto::Proto(IpProto::Udp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeUdpPacket {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeUdpPacket {
            fn src_port(&self) -> u16 {
                self.src_port
            }

            fn dst_port(&self) -> u16 {
                self.dst_port
            }
        }

        pub struct FakeIcmpEchoRequest {
            pub id: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeIcmpEchoRequest {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Icmp,
                    |IpInvariant(())| Ipv6Proto::Icmpv6,
                )
            }
        }

        impl MaybeTransportPacket for &FakeIcmpEchoRequest {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeIcmpEchoRequest {
            fn src_port(&self) -> u16 {
                self.id
            }

            fn dst_port(&self) -> u16 {
                0
            }
        }

        pub trait ArbitraryValue {
            fn arbitrary_value() -> Self;
        }

        impl<I, T> ArbitraryValue for FakeIpPacket<I, T>
        where
            I: TestIpExt,
            T: ArbitraryValue,
            for<'a> &'a T: TransportPacketExt<I>,
        {
            fn arbitrary_value() -> Self {
                FakeIpPacket { src_ip: I::SRC_IP, dst_ip: I::DST_IP, body: T::arbitrary_value() }
            }
        }

        impl ArbitraryValue for FakeTcpSegment {
            fn arbitrary_value() -> Self {
                FakeTcpSegment { src_port: 33333, dst_port: 44444 }
            }
        }

        impl ArbitraryValue for FakeUdpPacket {
            fn arbitrary_value() -> Self {
                FakeUdpPacket { src_port: 33333, dst_port: 44444 }
            }
        }

        impl ArbitraryValue for FakeIcmpEchoRequest {
            fn arbitrary_value() -> Self {
                FakeIcmpEchoRequest { id: 1 }
            }
        }
    }
}
