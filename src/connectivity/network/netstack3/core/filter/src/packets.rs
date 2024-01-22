// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_formats::ip::IpExt;

/// An IP packet that provides header inspection.
//
// TODO(https://fxbug.dev/321013529): provide the necessary methods and associated
// type for packet header modification.
pub trait IpPacket<B, I: IpExt> {
    /// The type that provides access to transport-layer header inspection, if a
    /// transport header is contained in the body of the IP packet.
    type TransportPacket<'a>: TransportPacket
    where
        Self: 'a;

    /// The source IP address of the packet.
    fn src_addr(&self) -> I::Addr;

    /// The destination IP address of the packet.
    fn dst_addr(&self) -> I::Addr;

    /// The IP protocol of the packet.
    fn protocol(&self) -> I::Proto;

    /// Optionally provides the transport-layer packet contained in the body of
    /// the IP packet, if one exists.
    ///
    /// This method returns an owned type parameterized on a lifetime that is
    /// tied to the lifetime of Self, rather than, for example, a reference to a
    /// non-parameterized type (`&Self::TransportPacket`). This is because
    /// implementors may need to parse the transport header from the body of the
    /// IP packet and materialize the results into a new type when this is
    /// called, but that type may also need to retain a reference to the backing
    /// buffer in order to modify the transport header.
    fn transport_packet<'a>(&'a self) -> Option<Self::TransportPacket<'a>>;
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

#[cfg(test)]
pub(crate) mod testutil {
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::ip::{IpInvariant, Ipv4, Ipv6, Subnet};
    use packet_formats::ip::{IpProto, Ipv4Proto, Ipv6Proto};

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

    pub struct FakeBuffer;

    pub struct FakeIpPacket<I: IpExt, T>
    where
        for<'a> &'a T: TransportPacketExt<I>,
    {
        pub src_ip: I::Addr,
        pub dst_ip: I::Addr,
        pub body: T,
    }

    pub trait TransportPacketExt<I: IpExt>: TransportPacket {
        fn proto() -> I::Proto;
    }

    impl<I: IpExt, T> IpPacket<FakeBuffer, I> for FakeIpPacket<I, T>
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

        fn transport_packet(&self) -> Option<Self::TransportPacket<'_>> {
            Some(&self.body)
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
