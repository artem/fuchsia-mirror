// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Internet Control Message Protocol (ICMP).

pub(crate) mod socket;

use core::{
    convert::TryInto as _,
    fmt::Debug,
    num::{NonZeroU16, NonZeroU8},
    ops::ControlFlow,
};

use lock_order::{lock::UnlockedAccess, relation::LockBefore, wrap::prelude::*};
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, IpMarked, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr,
        Mtu, SubnetError,
    },
    LinkLocalAddress, LinkLocalUnicastAddr, MulticastAddress, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{
    BufferMut, InnerPacketBuilder as _, ParsablePacket as _, ParseBuffer, Serializer,
    TruncateDirection, TruncatingSerializer,
};
use packet_formats::{
    icmp::{
        ndp::{
            options::{NdpOption, NdpOptionBuilder},
            NdpPacket, NeighborAdvertisement, NonZeroNdpLifetime, OptionSequenceBuilder,
        },
        peek_message_type, IcmpDestUnreachable, IcmpEchoRequest, IcmpMessage, IcmpMessageType,
        IcmpPacket, IcmpPacketBuilder, IcmpPacketRaw, IcmpParseArgs, IcmpTimeExceeded,
        IcmpUnusedCode, Icmpv4DestUnreachableCode, Icmpv4Packet, Icmpv4ParameterProblem,
        Icmpv4ParameterProblemCode, Icmpv4RedirectCode, Icmpv4TimeExceededCode,
        Icmpv6DestUnreachableCode, Icmpv6Packet, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
        Icmpv6ParameterProblemCode, Icmpv6TimeExceededCode, MessageBody, OriginalPacket,
    },
    ip::{Ipv4Proto, Ipv6Proto},
    ipv4::{Ipv4FragmentType, Ipv4Header},
    ipv6::{ExtHdrParseError, Ipv6Header},
};
use tracing::{debug, error, trace};
use zerocopy::ByteSlice;

use crate::{
    context::{CounterContext, InstantContext, RngContext},
    counters::Counter,
    data_structures::token_bucket::TokenBucket,
    device::{AnyDevice, DeviceIdContext, FrameDestination, WeakId},
    ip::{
        device::{
            nud::{ConfirmationFlags, NudIpHandler},
            route_discovery::Ipv6DiscoveredRoute,
            IpAddressState, IpDeviceHandler, Ipv6DeviceHandler,
        },
        icmp::socket::{BoundSockets, IcmpAddrSpec, IcmpEchoBindingsContext, IcmpSockets},
        path_mtu::PmtuHandler,
        socket::{DefaultSendOptions, IpSocketHandler},
        AddressStatus, EitherDeviceId, IpDeviceStateContext, IpExt, IpLayerHandler,
        IpTransportContext, Ipv6PresentAddressStatus, MulticastMembershipHandler, SendIpPacketMeta,
        TransportIpContext, TransportReceiveError, IPV6_DEFAULT_SUBNET,
    },
    socket::{
        address::{AddrIsMappedError, AddrVecIter, ConnAddr, ConnIpAddr, SocketIpAddr},
        datagram::{self},
        AddrVec,
    },
    sync::Mutex,
    BindingsContext, CoreCtx, StackState,
};

/// The IP packet hop limit for all NDP packets.
///
/// See [RFC 4861 section 4.1], [RFC 4861 section 4.2], [RFC 4861 section 4.2],
/// [RFC 4861 section 4.3], [RFC 4861 section 4.4], and [RFC 4861 section 4.5]
/// for more information.
///
/// [RFC 4861 section 4.1]: https://tools.ietf.org/html/rfc4861#section-4.1
/// [RFC 4861 section 4.2]: https://tools.ietf.org/html/rfc4861#section-4.2
/// [RFC 4861 section 4.3]: https://tools.ietf.org/html/rfc4861#section-4.3
/// [RFC 4861 section 4.4]: https://tools.ietf.org/html/rfc4861#section-4.4
/// [RFC 4861 section 4.5]: https://tools.ietf.org/html/rfc4861#section-4.5
pub(crate) const REQUIRED_NDP_IP_PACKET_HOP_LIMIT: u8 = 255;

/// The default number of ICMP error messages to send per second.
///
/// Beyond this rate, error messages will be silently dropped.
pub const DEFAULT_ERRORS_PER_SECOND: u64 = 2 << 16;

/// An ICMPv4 error type and code.
///
/// Each enum variant corresponds to a particular error type, and contains the
/// possible codes for that error type.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Icmpv4ErrorCode {
    DestUnreachable(Icmpv4DestUnreachableCode),
    Redirect(Icmpv4RedirectCode),
    TimeExceeded(Icmpv4TimeExceededCode),
    ParameterProblem(Icmpv4ParameterProblemCode),
}

impl<I: IcmpIpExt> GenericOverIp<I> for Icmpv4ErrorCode {
    type Type = I::ErrorCode;
}

/// An ICMPv6 error type and code.
///
/// Each enum variant corresponds to a particular error type, and contains the
/// possible codes for that error type.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Icmpv6ErrorCode {
    DestUnreachable(Icmpv6DestUnreachableCode),
    PacketTooBig,
    TimeExceeded(Icmpv6TimeExceededCode),
    ParameterProblem(Icmpv6ParameterProblemCode),
}

impl<I: IcmpIpExt> GenericOverIp<I> for Icmpv6ErrorCode {
    type Type = I::ErrorCode;
}

/// An ICMP error of either IPv4 or IPv6.
#[derive(Debug, Clone, Copy)]
pub enum IcmpErrorCode {
    /// ICMPv4 error.
    V4(Icmpv4ErrorCode),
    /// ICMPv6 error.
    V6(Icmpv6ErrorCode),
}

impl From<Icmpv4ErrorCode> for IcmpErrorCode {
    fn from(v4_err: Icmpv4ErrorCode) -> Self {
        IcmpErrorCode::V4(v4_err)
    }
}

impl From<Icmpv6ErrorCode> for IcmpErrorCode {
    fn from(v6_err: Icmpv6ErrorCode) -> Self {
        IcmpErrorCode::V6(v6_err)
    }
}

pub(crate) struct IcmpState<I: IpExt + datagram::DualStackIpExt, Instant, D: WeakId> {
    pub(crate) sockets: IcmpSockets<I, D>,
    pub(crate) error_send_bucket: Mutex<TokenBucket<Instant>>,
    pub(crate) tx_counters: IcmpTxCounters<I>,
    pub(crate) rx_counters: IcmpRxCounters<I>,
}

/// ICMP tx path counters.
pub type IcmpTxCounters<I> = IpMarked<I, IcmpTxCountersInner>;

/// ICMP tx path counters.
#[derive(Default)]
pub struct IcmpTxCountersInner {
    /// Count of reply messages sent.
    pub reply: Counter,
    /// Count of protocol unreachable messages sent.
    pub protocol_unreachable: Counter,
    /// Count of port unreachable messages sent.
    pub port_unreachable: Counter,
    /// Count of net unreachable messages sent.
    pub net_unreachable: Counter,
    /// Count of ttl expired messages sent.
    pub ttl_expired: Counter,
    /// Count of packet too big messages sent.
    pub packet_too_big: Counter,
    /// Count of parameter problem messages sent.
    pub parameter_problem: Counter,
    /// Count of destination unreachable messages sent.
    pub dest_unreachable: Counter,
    /// Count of error messages sent.
    pub error: Counter,
}

/// ICMP rx path counters.
pub type IcmpRxCounters<I> = IpMarked<I, IcmpRxCountersInner>;

/// ICMP rx path counters.
#[derive(Default)]
pub struct IcmpRxCountersInner {
    /// Count of error messages received.
    pub error: Counter,
    /// Count of error messages received at the transport layer.
    pub(crate) error_at_transport_layer: Counter,
    /// Count of error messages delivered to a socket.
    pub(crate) error_at_socket: Counter,
    /// Count of echo request messages received.
    pub echo_request: Counter,
    /// Count of echo reply messages received.
    pub echo_reply: Counter,
    /// Count of timestamp request messages received.
    pub timestamp_request: Counter,
    /// Count of destination unreachable messages received.
    pub dest_unreachable: Counter,
    /// Count of time exceeded messages received.
    pub time_exceeded: Counter,
    /// Count of parameter problem messages received.
    pub parameter_problem: Counter,
    /// Count of packet too big messages received.
    pub packet_too_big: Counter,
}

/// Counters for NDP messages.
#[derive(Default)]
pub struct NdpCounters {
    /// Count of neighbor solicitation messages received.
    pub rx_neighbor_solicitation: Counter,
    /// Count of neighbor advertisement messages received.
    pub rx_neighbor_advertisement: Counter,
    /// Count of router advertisement messages received.
    pub rx_router_advertisement: Counter,
    /// Count of router solicitation messages received.
    pub rx_router_solicitation: Counter,
    /// Count of neighbor advertisement messages sent.
    pub tx_neighbor_advertisement: Counter,
    /// Count of neighbor solicitation messages sent.
    pub tx_neighbor_solicitation: Counter,
}

impl<BC: BindingsContext, I: Ip> UnlockedAccess<crate::lock_ordering::IcmpTxCounters<I>>
    for StackState<BC>
{
    type Data = IcmpTxCounters<I>;
    type Guard<'l> = &'l IcmpTxCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.icmp_tx_counters()
    }
}

impl<BC: BindingsContext, I: Ip, L> CounterContext<IcmpTxCounters<I>> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&IcmpTxCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IcmpTxCounters<I>>())
    }
}

impl<BC: BindingsContext, I: Ip> UnlockedAccess<crate::lock_ordering::IcmpRxCounters<I>>
    for StackState<BC>
{
    type Data = IcmpRxCounters<I>;
    type Guard<'l> = &'l IcmpRxCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.icmp_rx_counters()
    }
}

impl<BC: BindingsContext, I: Ip, L> CounterContext<IcmpRxCounters<I>> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&IcmpRxCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IcmpRxCounters<I>>())
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::NdpCounters> for StackState<BC> {
    type Data = NdpCounters;
    type Guard<'l> = &'l NdpCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ndp_counters()
    }
}

impl<BC: BindingsContext, L> CounterContext<NdpCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&NdpCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::NdpCounters>())
    }
}

/// A builder for ICMPv4 state.
#[derive(Copy, Clone)]
pub(crate) struct Icmpv4StateBuilder {
    send_timestamp_reply: bool,
    errors_per_second: u64,
}

impl Default for Icmpv4StateBuilder {
    fn default() -> Icmpv4StateBuilder {
        Icmpv4StateBuilder {
            send_timestamp_reply: false,
            errors_per_second: DEFAULT_ERRORS_PER_SECOND,
        }
    }
}

impl Icmpv4StateBuilder {
    /// Enable or disable replying to ICMPv4 Timestamp Request messages with
    /// Timestamp Reply messages (default: disabled).
    ///
    /// Enabling this can introduce a very minor vulnerability in which an
    /// attacker can learn the system clock's time, which in turn can aid in
    /// attacks against time-based authentication systems.
    #[cfg(test)]
    pub(crate) fn send_timestamp_reply(&mut self, send_timestamp_reply: bool) -> &mut Self {
        self.send_timestamp_reply = send_timestamp_reply;
        self
    }

    pub(crate) fn build<Instant, D: WeakId>(self) -> Icmpv4State<Instant, D> {
        Icmpv4State {
            inner: IcmpState {
                sockets: Default::default(),
                error_send_bucket: Mutex::new(TokenBucket::new(self.errors_per_second)),
                tx_counters: Default::default(),
                rx_counters: Default::default(),
            },
            send_timestamp_reply: self.send_timestamp_reply,
        }
    }
}

/// The state associated with the ICMPv4 protocol.
pub(crate) struct Icmpv4State<Instant, D: WeakId> {
    pub(crate) inner: IcmpState<Ipv4, Instant, D>,
    send_timestamp_reply: bool,
}

// Used by `receive_icmp_echo_reply`.
impl<Instant, D: WeakId> AsRef<IcmpState<Ipv4, Instant, D>> for Icmpv4State<Instant, D> {
    fn as_ref(&self) -> &IcmpState<Ipv4, Instant, D> {
        &self.inner
    }
}

// Used by `send_icmpv4_echo_request_inner`.
impl<Instant, D: WeakId> AsMut<IcmpState<Ipv4, Instant, D>> for Icmpv4State<Instant, D> {
    fn as_mut(&mut self) -> &mut IcmpState<Ipv4, Instant, D> {
        &mut self.inner
    }
}

/// A builder for ICMPv6 state.
#[derive(Copy, Clone)]
pub(crate) struct Icmpv6StateBuilder {
    errors_per_second: u64,
}

impl Default for Icmpv6StateBuilder {
    fn default() -> Icmpv6StateBuilder {
        Icmpv6StateBuilder { errors_per_second: DEFAULT_ERRORS_PER_SECOND }
    }
}

impl Icmpv6StateBuilder {
    pub(crate) fn build<Instant, D: WeakId>(self) -> Icmpv6State<Instant, D> {
        Icmpv6State {
            inner: IcmpState {
                sockets: Default::default(),
                error_send_bucket: Mutex::new(TokenBucket::new(self.errors_per_second)),
                tx_counters: Default::default(),
                rx_counters: Default::default(),
            },
            ndp_counters: Default::default(),
        }
    }
}

/// The state associated with the ICMPv6 protocol.
pub(crate) struct Icmpv6State<Instant, D: WeakId> {
    pub(crate) inner: IcmpState<Ipv6, Instant, D>,
    pub(crate) ndp_counters: NdpCounters,
}

// Used by `receive_icmp_echo_reply`.
impl<Instant, D: WeakId> AsRef<IcmpState<Ipv6, Instant, D>> for Icmpv6State<Instant, D> {
    fn as_ref(&self) -> &IcmpState<Ipv6, Instant, D> {
        &self.inner
    }
}

// Used by `send_icmpv6_echo_request_inner`.
impl<Instant, D: WeakId> AsMut<IcmpState<Ipv6, Instant, D>> for Icmpv6State<Instant, D> {
    fn as_mut(&mut self) -> &mut IcmpState<Ipv6, Instant, D> {
        &mut self.inner
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) struct IcmpAddr<A: IpAddress> {
    local_addr: SocketIpAddr<A>,
    remote_addr: SocketIpAddr<A>,
    icmp_id: u16,
}

/// An extension trait adding extra ICMP-related functionality to IP versions.
pub trait IcmpIpExt: packet_formats::ip::IpExt + packet_formats::icmp::IcmpIpExt {
    /// The type of error code for this version of ICMP - [`Icmpv4ErrorCode`] or
    /// [`Icmpv6ErrorCode`].
    type ErrorCode: Debug
        + Copy
        + PartialEq
        + GenericOverIp<Self, Type = Self::ErrorCode>
        + GenericOverIp<Ipv4, Type = Icmpv4ErrorCode>
        + GenericOverIp<Ipv6, Type = Icmpv6ErrorCode>
        + Into<IcmpErrorCode>;
}

impl IcmpIpExt for Ipv4 {
    type ErrorCode = Icmpv4ErrorCode;
}

impl IcmpIpExt for Ipv6 {
    type ErrorCode = Icmpv6ErrorCode;
}

/// An extension trait providing ICMP handler properties.
pub(crate) trait IcmpHandlerIpExt: Ip {
    type SourceAddress: Witness<Self::Addr>;
    type IcmpError;
}

impl IcmpHandlerIpExt for Ipv4 {
    type SourceAddress = SpecifiedAddr<Ipv4Addr>;
    type IcmpError = Icmpv4Error;
}

impl IcmpHandlerIpExt for Ipv6 {
    type SourceAddress = UnicastAddr<Ipv6Addr>;
    type IcmpError = Icmpv6ErrorKind;
}

/// A kind of ICMPv4 error.
pub(crate) enum Icmpv4ErrorKind {
    ParameterProblem {
        code: Icmpv4ParameterProblemCode,
        pointer: u8,
        fragment_type: Ipv4FragmentType,
    },
    TtlExpired {
        proto: Ipv4Proto,
        fragment_type: Ipv4FragmentType,
    },
    NetUnreachable {
        proto: Ipv4Proto,
        fragment_type: Ipv4FragmentType,
    },
    ProtocolUnreachable,
    PortUnreachable,
}

/// An ICMPv4 error.
pub(crate) struct Icmpv4Error {
    pub(super) kind: Icmpv4ErrorKind,
    pub(super) header_len: usize,
}

/// A kind of ICMPv6 error.
pub(crate) enum Icmpv6ErrorKind {
    ParameterProblem { code: Icmpv6ParameterProblemCode, pointer: u32, allow_dst_multicast: bool },
    TtlExpired { proto: Ipv6Proto, header_len: usize },
    NetUnreachable { proto: Ipv6Proto, header_len: usize },
    PacketTooBig { proto: Ipv6Proto, header_len: usize, mtu: Mtu },
    ProtocolUnreachable { header_len: usize },
    PortUnreachable,
}

/// The handler exposed by ICMP.
pub(crate) trait IcmpErrorHandler<I: IcmpHandlerIpExt, BC>:
    DeviceIdContext<AnyDevice>
{
    /// Sends an error message in response to an incoming packet.
    ///
    /// `src_ip` and `dst_ip` are the source and destination addresses of the
    /// incoming packet.
    fn send_icmp_error_message<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        frame_dst: FrameDestination,
        src_ip: I::SourceAddress,
        dst_ip: SpecifiedAddr<I::Addr>,
        original_packet: B,
        error: I::IcmpError,
    );
}

impl<
        BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
        CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
    > IcmpErrorHandler<Ipv4, BC> for CC
{
    fn send_icmp_error_message<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        frame_dst: FrameDestination,
        src_ip: SpecifiedAddr<Ipv4Addr>,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_packet: B,
        Icmpv4Error { kind, header_len }: Icmpv4Error,
    ) {
        let src_ip = SocketIpAddr::new_ipv4_specified(src_ip);
        let dst_ip = SocketIpAddr::new_ipv4_specified(dst_ip);
        match kind {
            Icmpv4ErrorKind::ParameterProblem { code, pointer, fragment_type } => {
                send_icmpv4_parameter_problem(
                    self,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    code,
                    Icmpv4ParameterProblem::new(pointer),
                    original_packet,
                    header_len,
                    fragment_type,
                )
            }
            Icmpv4ErrorKind::TtlExpired { proto, fragment_type } => send_icmpv4_ttl_expired(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
                fragment_type,
            ),
            Icmpv4ErrorKind::NetUnreachable { proto, fragment_type } => {
                send_icmpv4_net_unreachable(
                    self,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    proto,
                    original_packet,
                    header_len,
                    fragment_type,
                )
            }
            Icmpv4ErrorKind::ProtocolUnreachable => send_icmpv4_protocol_unreachable(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
                header_len,
            ),
            Icmpv4ErrorKind::PortUnreachable => send_icmpv4_port_unreachable(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
                header_len,
            ),
        }
    }
}

impl<
        BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
        CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
    > IcmpErrorHandler<Ipv6, BC> for CC
{
    fn send_icmp_error_message<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        frame_dst: FrameDestination,
        src_ip: UnicastAddr<Ipv6Addr>,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_packet: B,
        error: Icmpv6ErrorKind,
    ) {
        let src_ip: SocketIpAddr<Ipv6Addr> = match src_ip.into_specified().try_into() {
            Ok(addr) => addr,
            Err(AddrIsMappedError {}) => {
                trace!("send_icmpv6_error_message: src_ip is mapped");
                return;
            }
        };
        let dst_ip: SocketIpAddr<Ipv6Addr> = match dst_ip.try_into() {
            Ok(addr) => addr,
            Err(AddrIsMappedError {}) => {
                trace!("send_icmpv6_error_message: dst_ip is mapped");
                return;
            }
        };

        match error {
            Icmpv6ErrorKind::ParameterProblem { code, pointer, allow_dst_multicast } => {
                send_icmpv6_parameter_problem(
                    self,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    code,
                    Icmpv6ParameterProblem::new(pointer),
                    original_packet,
                    allow_dst_multicast,
                )
            }
            Icmpv6ErrorKind::TtlExpired { proto, header_len } => send_icmpv6_ttl_expired(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::NetUnreachable { proto, header_len } => send_icmpv6_net_unreachable(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::PacketTooBig { proto, header_len, mtu } => send_icmpv6_packet_too_big(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                mtu,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::ProtocolUnreachable { header_len } => {
                send_icmpv6_protocol_unreachable(
                    self,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    original_packet,
                    header_len,
                )
            }
            Icmpv6ErrorKind::PortUnreachable => send_icmpv6_port_unreachable(
                self,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
            ),
        }
    }
}

/// A marker for all the contexts provided by bindings require by the ICMP
/// module.
pub trait IcmpBindingsContext<I: IcmpIpExt, D>:
    InstantContext + IcmpEchoBindingsContext<I, D> + RngContext
{
}
impl<I: IcmpIpExt, BC: InstantContext + IcmpEchoBindingsContext<I, D> + RngContext, D>
    IcmpBindingsContext<I, D> for BC
{
}

/// Empty trait to work around coherence issues.
///
/// This serves only to convince the coherence checker that a particular blanket
/// trait implementation could only possibly conflict with other blanket impls
/// in this crate. It can be safely implemented for any type.
/// TODO(https://github.com/rust-lang/rust/issues/97811): Remove this once the
/// coherence checker doesn't require it.
pub trait IcmpStateContext {}

/// The execution context shared by ICMP(v4) and ICMPv6 for the internal
/// operations of the IP stack.
///
/// Unlike [`IcmpEchoBindingsContext`], `InnerIcmpContext` is not exposed outside of
/// this crate.
pub trait InnerIcmpContext<I: IcmpIpExt + IpExt, BC: IcmpBindingsContext<I, Self::DeviceId>>:
    IpSocketHandler<I, BC> + DeviceIdContext<AnyDevice>
{
    type DualStackContext: datagram::DualStackDatagramBoundStateContext<
        I,
        BC,
        socket::Icmp,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >
    where
        I: datagram::IpExt;
    /// The core context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, BC>
        + MulticastMembershipHandler<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + IcmpStateContext
        + CounterContext<IcmpTxCounters<I>>
        + CounterContext<IcmpRxCounters<I>>;
    // TODO(joshlf): If we end up needing to respond to these messages with new
    // outbound packets, then perhaps it'd be worth passing the original buffer
    // so that it can be reused?
    //
    // NOTE(joshlf): We don't guarantee the packet body length here for two
    // reasons:
    // - It's possible that some IPv4 protocol does or will exist for which
    //   valid packets are less than 8 bytes in length. If we were to reject all
    //   packets with bodies of less than 8 bytes, we might silently discard
    //   legitimate error messages for such protocols.
    // - Even if we were to guarantee this, there's no good way to encode such a
    //   guarantee in the type system, and so the caller would have no recourse
    //   but to panic, and panics have a habit of becoming bugs or DoS
    //   vulnerabilities when invariants change.

    /// Receives an ICMP error message and demultiplexes it to a transport layer
    /// protocol.
    ///
    /// All arguments beginning with `original_` are fields from the IP packet
    /// that triggered the error. The `original_body` is provided here so that
    /// the error can be associated with a transport-layer socket. `device`
    /// identifies the device on which the packet was received.
    ///
    /// While ICMPv4 error messages are supposed to contain the first 8 bytes of
    /// the body of the offending packet, and ICMPv6 error messages are supposed
    /// to contain as much of the offending packet as possible without violating
    /// the IPv6 minimum MTU, the caller does NOT guarantee that either of these
    /// hold. It is `receive_icmp_error`'s responsibility to handle any length
    /// of `original_body`, and to perform any necessary validation.
    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        original_proto: I::Proto,
        original_body: &[u8],
        err: I::ErrorCode,
    );

    /// Calls the function with an immutable reference to ICMP sockets.
    fn with_icmp_sockets<O, F: FnOnce(&BoundSockets<I, Self::WeakDeviceId>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to `IpSocketsCtx` and
    /// a mutable reference to ICMP sockets.
    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to ICMP error send tocket
    /// bucket.
    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// The execution context for ICMPv4.
///
/// `InnerIcmpv4Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerIcmpv4Context<BC: IcmpBindingsContext<Ipv4, Self::DeviceId>>:
    InnerIcmpContext<Ipv4, BC>
{
    /// Returns true if a timestamp reply may be sent.
    fn should_send_timestamp_reply(&self) -> bool;
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>
    for StackState<BC>
{
    type Data = bool;
    type Guard<'l> = &'l bool where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.icmp.send_timestamp_reply
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv4>>,
    > InnerIcmpv4Context<BC> for CoreCtx<'_, BC, L>
{
    fn should_send_timestamp_reply(&self) -> bool {
        *self.unlocked_access::<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>()
    }
}

/// The execution context for ICMPv6.
///
/// `InnerIcmpv6Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerIcmpv6Context<BC: IcmpBindingsContext<Ipv6, Self::DeviceId>>:
    InnerIcmpContext<Ipv6, BC>
{
}

impl<BC: IcmpBindingsContext<Ipv6, Self::DeviceId>, CC: InnerIcmpContext<Ipv6, BC>>
    InnerIcmpv6Context<BC> for CC
{
}

/// Attempt to send an ICMP or ICMPv6 error message, applying a rate limit.
///
/// `try_send_error!($core_ctx, $bindings_ctx, $e)` attempts to consume a token from the
/// token bucket at `$core_ctx.get_state_mut().error_send_bucket`. If it
/// succeeds, it invokes the expression `$e`, and otherwise does nothing. It
/// assumes that the type of `$e` is `Result<(), _>` and, in the case that the
/// rate limit is exceeded and it does not invoke `$e`, returns `Ok(())`.
///
/// [RFC 4443 Section 2.4] (f) requires that we MUST limit the rate of outbound
/// ICMPv6 error messages. To our knowledge, there is no similar requirement for
/// ICMPv4, but the same rationale applies, so we do it for ICMPv4 as well.
///
/// [RFC 4443 Section 2.4]: https://tools.ietf.org/html/rfc4443#section-2.4
macro_rules! try_send_error {
    ($core_ctx:expr, $bindings_ctx:expr, $e:expr) => {{
        // TODO(joshlf): Figure out a way to avoid querying for the current time
        // unconditionally. See the documentation on the `CachedInstantCtx` type
        // for more information.
        let instant_ctx = crate::context::new_cached_instant_context($bindings_ctx);
        let send = $core_ctx.with_error_send_bucket_mut(|error_send_bucket| {
            error_send_bucket.try_take(&instant_ctx)
        });

        if send {
            $core_ctx.with_counters(|counters| {
                counters.error.increment();
            });
            $e
        } else {
            trace!("ip::icmp::try_send_error!: dropping rate-limited ICMP error message");
            Ok(())
        }
    }};
}

/// An implementation of [`IpTransportContext`] for ICMP.
pub(crate) enum IcmpIpTransportContext {}

fn receive_ip_transport_icmp_error<
    I: IcmpIpExt + IpExt,
    CC: InnerIcmpContext<I, BC> + CounterContext<IcmpRxCounters<I>>,
    BC: IcmpBindingsContext<I, CC::DeviceId>,
>(
    core_ctx: &mut CC,
    original_src_ip: Option<SpecifiedAddr<I::Addr>>,
    original_dst_ip: SpecifiedAddr<I::Addr>,
    mut original_body: &[u8],
    err: I::ErrorCode,
) {
    core_ctx.with_counters(|counters| {
        counters.error_at_transport_layer.increment();
    });
    trace!("IcmpIpTransportContext::receive_icmp_error({:?})", err);

    let echo_request =
        if let Ok(echo_request) = original_body.parse::<IcmpPacketRaw<I, _, IcmpEchoRequest>>() {
            echo_request
        } else {
            // NOTE: This might just mean that the error message was in response
            // to a packet that we sent that wasn't an echo request, so we just
            // silently ignore it.
            return;
        };

    let original_src_ip = match original_src_ip {
        Some(ip) => ip,
        None => {
            trace!("IcmpIpTransportContext::receive_icmp_error: unspecified source IP address");
            return;
        }
    };
    let original_src_ip: SocketIpAddr<_> = match original_src_ip.try_into() {
        Ok(ip) => ip,
        Err(AddrIsMappedError {}) => {
            trace!("IcmpIpTransportContext::receive_icmp_error: mapped source IP address");
            return;
        }
    };
    let original_dst_ip: SocketIpAddr<_> = match original_dst_ip.try_into() {
        Ok(ip) => ip,
        Err(AddrIsMappedError {}) => {
            trace!("IcmpIpTransportContext::receive_icmp_error: mapped destination IP address");
            return;
        }
    };

    let id = echo_request.message().id();
    core_ctx.with_icmp_ctx_and_sockets_mut(|core_ctx, sockets| {
            if let Some(conn) = sockets.socket_map.conns().get_by_addr(&ConnAddr {
                ip: ConnIpAddr {
                    local: (original_src_ip, NonZeroU16::new(id).unwrap()),
                    remote: (original_dst_ip, ())
                },
                device: None,
            }) {
                core_ctx.with_counters(|counters: &IcmpRxCounters<I>| {
                    counters.error_at_socket.increment();
                });
                // NB: At the moment bindings has no need to consume ICMP
                // errors, so we swallow them here.
                debug!(
                    "ICMP received ICMP error {:?} from {:?}, to {:?} on socket {:?}",
                    err,
                    original_dst_ip,
                    original_src_ip,
                    conn
                )
            } else {
                trace!("IcmpIpTransportContext::receive_icmp_error: Got ICMP error message for nonexistent ICMP echo socket; either the socket responsible has since been removed, or the error message was sent in error or corrupted");
            }
        })
}

impl<
        BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
        CC: InnerIcmpv4Context<BC>
            + PmtuHandler<Ipv4, BC>
            + CounterContext<IcmpRxCounters<Ipv4>>
            + CounterContext<IcmpTxCounters<Ipv4>>,
    > IpTransportContext<Ipv4, BC, CC> for IcmpIpTransportContext
{
    fn receive_icmp_error(
        core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<Ipv4Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_body: &[u8],
        err: Icmpv4ErrorCode,
    ) {
        receive_ip_transport_icmp_error(
            core_ctx,
            original_src_ip,
            original_dst_ip,
            original_body,
            err,
        )
    }

    fn receive_ip_packet<B: BufferMut>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        trace!(
            "<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet({}, {})",
            src_ip,
            dst_ip
        );
        let packet =
            match buffer.parse_with::<_, Icmpv4Packet<_>>(IcmpParseArgs::new(src_ip, dst_ip)) {
                Ok(packet) => packet,
                Err(_) => return Ok(()), // TODO(joshlf): Do something else here?
            };

        match packet {
            Icmpv4Packet::EchoRequest(echo_request) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.echo_request.increment();
                });

                if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
                    let req = *echo_request.message();
                    let code = echo_request.code();
                    let (local_ip, remote_ip) = (dst_ip, src_ip);
                    // TODO(joshlf): Do something if send_icmp_reply returns an
                    // error?
                    let _ = send_icmp_reply(
                        core_ctx,
                        bindings_ctx,
                        Some(device),
                        SocketIpAddr::new_ipv4_specified(remote_ip),
                        SocketIpAddr::new_ipv4_specified(local_ip),
                        |src_ip| {
                            buffer.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                                src_ip,
                                remote_ip,
                                code,
                                req.reply(),
                            ))
                        },
                    );
                } else {
                    trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received echo request with an unspecified source address");
                }
            }
            Icmpv4Packet::EchoReply(echo_reply) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.echo_reply.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received an EchoReply message");
                let id = echo_reply.message().id();
                let meta = echo_reply.parse_metadata();
                buffer.undo_parse(meta);
                let device = core_ctx.downgrade_device_id(device);
                receive_icmp_echo_reply(core_ctx, bindings_ctx, src_ip, dst_ip, id, buffer, device);
            }
            Icmpv4Packet::TimestampRequest(timestamp_request) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.timestamp_request.increment();
                });
                if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
                    if core_ctx.should_send_timestamp_reply() {
                        trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Responding to Timestamp Request message");
                        // We're supposed to respond with the time that we
                        // processed this message as measured in milliseconds
                        // since midnight UT. However, that would require that
                        // we knew the local time zone and had a way to convert
                        // `InstantContext::Instant` to a `u32` value. We can't
                        // do that, and probably don't want to introduce all of
                        // the machinery necessary just to support this one use
                        // case. Luckily, RFC 792 page 17 provides us with an
                        // out:
                        //
                        //   If the time is not available in miliseconds [sic]
                        //   or cannot be provided with respect to midnight UT
                        //   then any time can be inserted in a timestamp
                        //   provided the high order bit of the timestamp is
                        //   also set to indicate this non-standard value.
                        //
                        // Thus, we provide a zero timestamp with the high order
                        // bit set.
                        const NOW: u32 = 0x80000000;
                        let reply = timestamp_request.message().reply(NOW, NOW);
                        let (local_ip, remote_ip) = (dst_ip, src_ip);
                        // We don't actually want to use any of the _contents_
                        // of the buffer, but we would like to reuse it as
                        // scratch space. Eventually, `IcmpPacketBuilder` will
                        // implement `InnerPacketBuilder` for messages without
                        // bodies, but until that happens, we need to give it an
                        // empty buffer.
                        buffer.shrink_front_to(0);
                        // TODO(joshlf): Do something if send_icmp_reply returns
                        // an error?
                        let _ = send_icmp_reply(
                            core_ctx,
                            bindings_ctx,
                            Some(device),
                            SocketIpAddr::new_ipv4_specified(remote_ip),
                            SocketIpAddr::new_ipv4_specified(local_ip),
                            |src_ip| {
                                buffer.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                                    src_ip,
                                    remote_ip,
                                    IcmpUnusedCode,
                                    reply,
                                ))
                            },
                        );
                    } else {
                        trace!(
                            "<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Silently ignoring Timestamp Request message"
                        );
                    }
                } else {
                    trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received timestamp request with an unspecified source address");
                }
            }
            Icmpv4Packet::TimestampReply(_) => {
                // TODO(joshlf): Support sending Timestamp Requests and
                // receiving Timestamp Replies?
                debug!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received unsolicited Timestamp Reply message");
            }
            Icmpv4Packet::DestUnreachable(dest_unreachable) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.dest_unreachable.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received a Destination Unreachable message");

                if dest_unreachable.code() == Icmpv4DestUnreachableCode::FragmentationRequired {
                    if let Some(next_hop_mtu) = dest_unreachable.message().next_hop_mtu() {
                        // We are updating the path MTU from the destination
                        // address of this `packet` (which is an IP address on
                        // this node) to some remote (identified by the source
                        // address of this `packet`).
                        //
                        // `update_pmtu_if_less` may return an error, but it
                        // will only happen if the Dest Unreachable message's
                        // MTU field had a value that was less than the IPv4
                        // minimum MTU (which as per IPv4 RFC 791, must not
                        // happen).
                        core_ctx.update_pmtu_if_less(
                            bindings_ctx,
                            dst_ip.get(),
                            src_ip,
                            Mtu::new(u32::from(next_hop_mtu.get())),
                        );
                    } else {
                        // If the Next-Hop MTU from an incoming ICMP message is
                        // `0`, then we assume the source node of the ICMP
                        // message does not implement RFC 1191 and therefore
                        // does not actually use the Next-Hop MTU field and
                        // still considers it as an unused field.
                        //
                        // In this case, the only information we have is the
                        // size of the original IP packet that was too big (the
                        // original packet header should be included in the ICMP
                        // response). Here we will simply reduce our PMTU
                        // estimate to a value less than the total length of the
                        // original packet. See RFC 1191 Section 5.
                        //
                        // `update_pmtu_next_lower` may return an error, but it
                        // will only happen if no valid lower value exists from
                        // the original packet's length. It is safe to silently
                        // ignore the error when we have no valid lower PMTU
                        // value as the node from `src_ip` would not be IP RFC
                        // compliant and we expect this to be very rare (for
                        // IPv4, the lowest MTU value for a link can be 68
                        // bytes).
                        let original_packet_buf = dest_unreachable.body().bytes();
                        if original_packet_buf.len() >= 4 {
                            // We need the first 4 bytes as the total length
                            // field is at bytes 2/3 of the original packet
                            // buffer.
                            let total_len =
                                u16::from_be_bytes(original_packet_buf[2..4].try_into().unwrap());

                            trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Next-Hop MTU is 0 so using the next best PMTU value from {}", total_len);

                            core_ctx.update_pmtu_next_lower(
                                bindings_ctx,
                                dst_ip.get(),
                                src_ip,
                                Mtu::new(u32::from(total_len)),
                            );
                        } else {
                            // Ok to silently ignore as RFC 792 requires nodes
                            // to send the original IP packet header + 64 bytes
                            // of the original IP packet's body so the node
                            // itself is already violating the RFC.
                            trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Original packet buf is too small to get original packet len so ignoring");
                        }
                    }
                }

                receive_icmpv4_error(
                    core_ctx,
                    bindings_ctx,
                    device,
                    &dest_unreachable,
                    Icmpv4ErrorCode::DestUnreachable(dest_unreachable.code()),
                );
            }
            Icmpv4Packet::TimeExceeded(time_exceeded) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.time_exceeded.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received a Time Exceeded message");

                receive_icmpv4_error(
                    core_ctx,
                    bindings_ctx,
                    device,
                    &time_exceeded,
                    Icmpv4ErrorCode::TimeExceeded(time_exceeded.code()),
                );
            }
            Icmpv4Packet::Redirect(_) => log_unimplemented!(
                (),
                "<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet::redirect"
            ),
            Icmpv4Packet::ParameterProblem(parameter_problem) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv4>| {
                    counters.parameter_problem.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv4>>::receive_ip_packet: Received a Parameter Problem message");

                receive_icmpv4_error(
                    core_ctx,
                    bindings_ctx,
                    device,
                    &parameter_problem,
                    Icmpv4ErrorCode::ParameterProblem(parameter_problem.code()),
                );
            }
        }

        Ok(())
    }
}

pub(crate) fn send_ndp_packet<BC, CC, S, M>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    body: S,
    code: M::Code,
    message: M,
) -> Result<(), S>
where
    CC: IpLayerHandler<Ipv6, BC>,
    S: Serializer,
    S::Buffer: BufferMut,
    M: IcmpMessage<Ipv6>,
{
    // TODO(https://fxbug.dev/95359): Send through ICMPv6 send path.
    IpLayerHandler::<Ipv6, _>::send_ip_packet_from_device(
        core_ctx,
        bindings_ctx,
        SendIpPacketMeta {
            device: device_id,
            src_ip,
            dst_ip,
            next_hop: dst_ip,
            ttl: NonZeroU8::new(REQUIRED_NDP_IP_PACKET_HOP_LIMIT),
            proto: Ipv6Proto::Icmpv6,
            mtu: None,
        },
        body.encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
            src_ip.map_or(Ipv6::UNSPECIFIED_ADDRESS, |a| a.get()),
            dst_ip.get(),
            code,
            message,
        )),
    )
    .map_err(|s| s.into_inner())
}

fn send_neighbor_advertisement<
    BC,
    CC: Ipv6DeviceHandler<BC>
        + IpDeviceHandler<Ipv6, BC>
        + IpLayerHandler<Ipv6, BC>
        + CounterContext<NdpCounters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    solicited: bool,
    device_addr: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
) {
    core_ctx.with_counters(|counters| {
        counters.tx_neighbor_advertisement.increment();
    });
    debug!("send_neighbor_advertisement from {:?} to {:?}", device_addr, dst_ip);
    // We currently only allow the destination address to be:
    // 1) a unicast address.
    // 2) a multicast destination but the message should be an unsolicited
    //    neighbor advertisement.
    // NOTE: this assertion may need change if more messages are to be allowed
    // in the future.
    debug_assert!(dst_ip.is_valid_unicast() || (!solicited && dst_ip.is_multicast()));

    // We must call into the higher level send_ip_packet_from_device function
    // because it is not guaranteed that we actually know the link-layer
    // address of the destination IP. Typically, the solicitation request will
    // carry that information, but it is not necessary. So it is perfectly valid
    // that trying to send this advertisement will end up triggering a neighbor
    // solicitation to be sent.
    let src_ll = core_ctx.get_link_layer_addr_bytes(&device_id);

    // Nothing reasonable to do with the error.
    let advertisement = NeighborAdvertisement::new(
        core_ctx.is_router_device(&device_id),
        solicited,
        false,
        device_addr.get(),
    );
    let _: Result<(), _> = send_ndp_packet(
        core_ctx,
        bindings_ctx,
        &device_id,
        Some(device_addr.into_specified()),
        dst_ip,
        OptionSequenceBuilder::new(
            src_ll.as_ref().map(AsRef::as_ref).map(NdpOptionBuilder::TargetLinkLayerAddress).iter(),
        )
        .into_serializer(),
        IcmpUnusedCode,
        advertisement,
    );
}

fn receive_ndp_packet<
    B: ByteSlice,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC>
        + Ipv6DeviceHandler<BC>
        + IpDeviceHandler<Ipv6, BC>
        + IpDeviceStateContext<Ipv6, BC>
        + NudIpHandler<Ipv6, BC>
        + IpLayerHandler<Ipv6, BC>
        + CounterContext<NdpCounters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    src_ip: Ipv6SourceAddr,
    packet: NdpPacket<B>,
) {
    // TODO(https://fxbug.dev/97319): Make sure IP's hop limit is set to 255 as
    // per RFC 4861 section 6.1.2.

    match packet {
        NdpPacket::RouterSolicitation(_) | NdpPacket::Redirect(_) => {}
        NdpPacket::NeighborSolicitation(ref p) => {
            let target_address = p.message().target_address();
            let target_address = match UnicastAddr::new(*target_address) {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NS from {} with non-unicast target={:?}",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            core_ctx.with_counters(|counters| {
                counters.rx_neighbor_solicitation.increment();
            });

            match src_ip {
                Ipv6SourceAddr::Unspecified => {
                    // The neighbor is performing Duplicate address detection.
                    //
                    // As per RFC 4861 section 4.3,
                    //
                    //   Source Address
                    //       Either an address assigned to the interface from
                    //       which this message is sent or (if Duplicate Address
                    //       Detection is in progress [ADDRCONF]) the
                    //       unspecified address.
                    match Ipv6DeviceHandler::remove_duplicate_tentative_address(
                        core_ctx,
                        bindings_ctx,
                        &device_id,
                        target_address,
                    ) {
                        IpAddressState::Assigned => {
                            // Address is assigned to us to we let the
                            // remote node performing DAD that we own the
                            // address.
                            send_neighbor_advertisement(
                                core_ctx,
                                bindings_ctx,
                                &device_id,
                                false,
                                target_address,
                                Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.into_specified(),
                            );
                        }
                        IpAddressState::Tentative => {
                            // Nothing further to do in response to DAD
                            // messages.
                        }
                        IpAddressState::Unavailable => {
                            // Nothing further to do for unassigned target
                            // addresses.
                        }
                    }

                    return;
                }
                Ipv6SourceAddr::Unicast(src_ip) => {
                    // Neighbor is performing link address resolution.
                    match core_ctx
                        .address_status_for_device(target_address.into_specified(), device_id)
                    {
                        AddressStatus::Present(Ipv6PresentAddressStatus::UnicastAssigned) => {}
                        AddressStatus::Present(
                            Ipv6PresentAddressStatus::UnicastTentative
                            | Ipv6PresentAddressStatus::Multicast,
                        )
                        | AddressStatus::Unassigned => {
                            // Address is not considered assigned to us as a
                            // unicast so don't send a neighbor advertisement
                            // reply.
                            return;
                        }
                    }

                    let link_addr = p.body().iter().find_map(|o| match o {
                        NdpOption::SourceLinkLayerAddress(a) => Some(a),
                        NdpOption::TargetLinkLayerAddress(_)
                        | NdpOption::PrefixInformation(_)
                        | NdpOption::RedirectedHeader { .. }
                        | NdpOption::RecursiveDnsServer(_)
                        | NdpOption::RouteInformation(_)
                        | NdpOption::Mtu(_) => None,
                    });

                    if let Some(link_addr) = link_addr {
                        NudIpHandler::handle_neighbor_probe(
                            core_ctx,
                            bindings_ctx,
                            &device_id,
                            src_ip.into_specified(),
                            link_addr,
                        );
                    }

                    send_neighbor_advertisement(
                        core_ctx,
                        bindings_ctx,
                        &device_id,
                        true,
                        target_address,
                        src_ip.into_specified(),
                    );
                }
            }
        }
        NdpPacket::NeighborAdvertisement(ref p) => {
            // TODO(https://fxbug.dev/97311): Invalidate discovered routers when
            // neighbor entry's IsRouter field transitions to false.

            let target_address = p.message().target_address();

            let src_ip = match src_ip {
                Ipv6SourceAddr::Unicast(src_ip) => src_ip,
                Ipv6SourceAddr::Unspecified => {
                    trace!("dropping NA with unspecified source and target = {:?}", target_address);
                    return;
                }
            };

            let target_address = match UnicastAddr::new(*target_address) {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NA from {} with non-unicast target={:?}",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            core_ctx.with_counters(|counters| {
                counters.rx_neighbor_advertisement.increment();
            });

            match Ipv6DeviceHandler::remove_duplicate_tentative_address(
                core_ctx,
                bindings_ctx,
                &device_id,
                target_address,
            ) {
                IpAddressState::Assigned => {
                    // A neighbor is advertising that it owns an address
                    // that we also have assigned. This is out of scope
                    // for DAD.
                    //
                    // As per RFC 4862 section 5.4.4,
                    //
                    //   2.  If the target address matches a unicast address
                    //       assigned to the receiving interface, it would
                    //       possibly indicate that the address is a
                    //       duplicate but it has not been detected by the
                    //       Duplicate Address Detection procedure (recall
                    //       that Duplicate Address Detection is not
                    //       completely reliable). How to handle such a case
                    //       is beyond the scope of this document.
                    //
                    // TODO(https://fxbug.dev/36238): Signal to bindings
                    // that a duplicate address is detected.
                    error!(
                        "NA from {src_ip} with target address {target_address} that is also \
                        assigned on device {device_id:?}",
                    );
                }
                IpAddressState::Tentative => {
                    // Nothing further to do for an NA from a neighbor that
                    // targets an address we also have assigned.
                    return;
                }
                IpAddressState::Unavailable => {
                    // Address not targeting us so we know its for a neighbor.
                    //
                    // TODO(https://fxbug.dev/99830): Move NUD to IP.
                }
            }

            let link_addr = p.body().iter().find_map(|o| match o {
                NdpOption::TargetLinkLayerAddress(a) => Some(a),
                NdpOption::SourceLinkLayerAddress(_)
                | NdpOption::PrefixInformation(_)
                | NdpOption::RedirectedHeader { .. }
                | NdpOption::RecursiveDnsServer(_)
                | NdpOption::RouteInformation(_)
                | NdpOption::Mtu(_) => None,
            });
            let link_addr = match link_addr {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NA from {} targetting {} with no TLL option",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            NudIpHandler::handle_neighbor_confirmation(
                core_ctx,
                bindings_ctx,
                &device_id,
                target_address.into_specified(),
                link_addr,
                ConfirmationFlags {
                    solicited_flag: p.message().solicited_flag(),
                    override_flag: p.message().override_flag(),
                },
            );
        }
        NdpPacket::RouterAdvertisement(ref p) => {
            // As per RFC 4861 section 6.1.2,
            //
            //   A node MUST silently discard any received Router Advertisement
            //   messages that do not satisfy all of the following validity
            //   checks:
            //
            //      - IP Source Address is a link-local address.  Routers must
            //        use their link-local address as the source for Router
            //        Advertisement and Redirect messages so that hosts can
            //        uniquely identify routers.
            //
            //        ...
            let src_ip = match src_ip {
                Ipv6SourceAddr::Unicast(ip) => match LinkLocalUnicastAddr::new(*ip) {
                    Some(ip) => ip,
                    None => return,
                },
                Ipv6SourceAddr::Unspecified => return,
            };

            core_ctx.with_counters(|counters| {
                counters.rx_router_advertisement.increment();
            });

            let ra = p.message();

            // As per RFC 4861 section 6.3.4,
            //   The RetransTimer variable SHOULD be copied from the Retrans
            //   Timer field, if it is specified.
            //
            // TODO(https://fxbug.dev/101357): Control whether or not we should
            // update the retransmit timer.
            if let Some(retransmit_timer) = ra.retransmit_timer() {
                Ipv6DeviceHandler::set_discovered_retrans_timer(
                    core_ctx,
                    bindings_ctx,
                    &device_id,
                    retransmit_timer,
                );
            }

            // As per RFC 4861 section 6.3.4:
            //   If the received Cur Hop Limit value is specified, the host
            //   SHOULD set its CurHopLimit variable to the received value.
            //
            // TODO(https://fxbug.dev/101357): Control whether or not we should
            // update the default hop limit.
            if let Some(hop_limit) = ra.current_hop_limit() {
                trace!("receive_ndp_packet: NDP RA: updating device's hop limit to {:?} for router: {:?}", ra.current_hop_limit(), src_ip);
                IpDeviceHandler::set_default_hop_limit(core_ctx, &device_id, hop_limit);
            }

            // TODO(https://fxbug.dev/126654): Support default router preference.
            Ipv6DeviceHandler::update_discovered_ipv6_route(
                core_ctx,
                bindings_ctx,
                &device_id,
                Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) },
                p.message().router_lifetime().map(NonZeroNdpLifetime::Finite),
            );

            for option in p.body().iter() {
                match option {
                    NdpOption::TargetLinkLayerAddress(_)
                    | NdpOption::RedirectedHeader { .. }
                    | NdpOption::RecursiveDnsServer(_) => {}
                    NdpOption::SourceLinkLayerAddress(addr) => {
                        // As per RFC 4861 section 6.3.4,
                        //
                        //   If the advertisement contains a Source Link-Layer
                        //   Address option, the link-layer address SHOULD be
                        //   recorded in the Neighbor Cache entry for the router
                        //   (creating an entry if necessary) and the IsRouter
                        //   flag in the Neighbor Cache entry MUST be set to
                        //   TRUE. If no Source Link-Layer Address is included,
                        //   but a corresponding Neighbor Cache entry exists,
                        //   its IsRouter flag MUST be set to TRUE. The IsRouter
                        //   flag is used by Neighbor Unreachability Detection
                        //   to determine when a router changes to being a host
                        //   (i.e., no longer capable of forwarding packets).
                        //   If a Neighbor Cache entry is created for the
                        //   router, its reachability state MUST be set to STALE
                        //   as specified in Section 7.3.3.  If a cache entry
                        //   already exists and is updated with a different
                        //   link-layer address, the reachability state MUST
                        //   also be set to STALE.if a Neighbor Cache entry
                        //
                        // We do not yet support NUD as described in RFC 4861
                        // so for now we just record the link-layer address in
                        // our neighbor table.
                        //
                        // TODO(https://fxbug.dev/133436): Add support for routers in NUD.
                        NudIpHandler::handle_neighbor_probe(
                            core_ctx,
                            bindings_ctx,
                            &device_id,
                            {
                                let src_ip: UnicastAddr<_> = src_ip.into_addr();
                                src_ip.into_specified()
                            },
                            addr,
                        );
                    }
                    NdpOption::PrefixInformation(prefix_info) => {
                        // As per RFC 4861 section 6.3.4,
                        //
                        //   For each Prefix Information option with the on-link
                        //   flag set, a host does the following:
                        //
                        //      - If the prefix is the link-local prefix,
                        //        silently ignore the Prefix Information option.
                        //
                        // Also as per RFC 4862 section 5.5.3,
                        //
                        //   For each Prefix-Information option in the Router
                        //   Advertisement:
                        //
                        //    ..
                        //
                        //    b)  If the prefix is the link-local prefix,
                        //        silently ignore the Prefix Information option.
                        if prefix_info.prefix().is_link_local() {
                            continue;
                        }

                        let subnet = match prefix_info.subnet() {
                            Ok(subnet) => subnet,
                            Err(err) => match err {
                                SubnetError::PrefixTooLong | SubnetError::HostBitsSet => continue,
                            },
                        };

                        match UnicastAddr::new(subnet.network()) {
                            Some(UnicastAddr { .. }) => {}
                            None => continue,
                        }

                        let valid_lifetime = prefix_info.valid_lifetime();

                        if prefix_info.on_link_flag() {
                            // TODO(https://fxbug.dev/126654): Support route preference.
                            Ipv6DeviceHandler::update_discovered_ipv6_route(
                                core_ctx,
                                bindings_ctx,
                                &device_id,
                                Ipv6DiscoveredRoute { subnet, gateway: None },
                                valid_lifetime,
                            )
                        }

                        if prefix_info.autonomous_address_configuration_flag() {
                            Ipv6DeviceHandler::apply_slaac_update(
                                core_ctx,
                                bindings_ctx,
                                &device_id,
                                subnet,
                                prefix_info.preferred_lifetime(),
                                valid_lifetime,
                            );
                        }
                    }
                    NdpOption::RouteInformation(rio) => {
                        // TODO(https://fxbug.dev/126654): Support route preference.
                        Ipv6DeviceHandler::update_discovered_ipv6_route(
                            core_ctx,
                            bindings_ctx,
                            &device_id,
                            Ipv6DiscoveredRoute {
                                subnet: rio.prefix().clone(),
                                gateway: Some(src_ip),
                            },
                            rio.route_lifetime(),
                        )
                    }
                    NdpOption::Mtu(mtu) => {
                        // TODO(https://fxbug.dev/101357): Control whether or
                        // not we should update the link's MTU in response to
                        // RAs.
                        Ipv6DeviceHandler::set_link_mtu(core_ctx, &device_id, Mtu::new(mtu));
                    }
                }
            }
        }
    }
}

impl<
        BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
        CC: InnerIcmpv6Context<BC>
            + InnerIcmpContext<Ipv6, BC>
            + Ipv6DeviceHandler<BC>
            + IpDeviceHandler<Ipv6, BC>
            + IpDeviceStateContext<Ipv6, BC>
            + PmtuHandler<Ipv6, BC>
            + NudIpHandler<Ipv6, BC>
            + IpLayerHandler<Ipv6, BC>
            + CounterContext<IcmpRxCounters<Ipv6>>
            + CounterContext<IcmpTxCounters<Ipv6>>
            + CounterContext<NdpCounters>,
    > IpTransportContext<Ipv6, BC, CC> for IcmpIpTransportContext
{
    fn receive_icmp_error(
        core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_body: &[u8],
        err: Icmpv6ErrorCode,
    ) {
        receive_ip_transport_icmp_error(
            core_ctx,
            original_src_ip,
            original_dst_ip,
            original_body,
            err,
        )
    }

    fn receive_ip_packet<B: BufferMut>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        trace!(
            "<IcmpIpTransportContext as IpTransportContext<Ipv6>>::receive_ip_packet({:?}, {})",
            src_ip,
            dst_ip
        );

        let packet = match buffer
            .parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(src_ip.get(), dst_ip))
        {
            Ok(packet) => packet,
            Err(_) => return Ok(()), // TODO(joshlf): Do something else here?
        };

        match packet {
            Icmpv6Packet::EchoRequest(echo_request) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv6>| {
                    counters.echo_request.increment();
                });

                if let Some(src_ip) = SocketIpAddr::new_from_ipv6_source(src_ip) {
                    match SocketIpAddr::try_from(dst_ip) {
                        Ok(dst_ip) => {
                            let req = *echo_request.message();
                            let code = echo_request.code();
                            let (local_ip, remote_ip) = (dst_ip, src_ip);
                            // TODO(joshlf): Do something if send_icmp_reply returns an
                            // error?
                            let _ = send_icmp_reply(
                                core_ctx,
                                bindings_ctx,
                                Some(device),
                                remote_ip,
                                local_ip,
                                |src_ip| {
                                    buffer.encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                                        src_ip,
                                        remote_ip.addr(),
                                        code,
                                        req.reply(),
                                    ))
                                },
                            );
                        }
                        Err(AddrIsMappedError {}) => {
                            trace!("IpTransportContext<Ipv6>::receive_ip_packet: Received echo request with an ipv4-mapped-ipv6 destination address");
                        }
                    }
                } else {
                    trace!("<IcmpIpTransportContext as IpTransportContext<Ipv6>>::receive_ip_packet: Received echo request with an unspecified source address");
                }
            }
            Icmpv6Packet::EchoReply(echo_reply) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv6>| {
                    counters.echo_reply.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv6>>::receive_ip_packet: Received an EchoReply message");
                // We don't allow creating echo sockets connected to the
                // unspecified address, so it's OK to bail early here if the
                // source address is unspecified.
                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    let id = echo_reply.message().id();
                    let meta = echo_reply.parse_metadata();
                    buffer.undo_parse(meta);
                    let device = core_ctx.downgrade_device_id(device);
                    receive_icmp_echo_reply(
                        core_ctx,
                        bindings_ctx,
                        src_ip.get(),
                        dst_ip,
                        id,
                        buffer,
                        device,
                    );
                }
            }
            Icmpv6Packet::Ndp(packet) => {
                receive_ndp_packet(core_ctx, bindings_ctx, device, src_ip, packet)
            }
            Icmpv6Packet::PacketTooBig(packet_too_big) => {
                core_ctx.with_counters(|counters: &IcmpRxCounters<Ipv6>| {
                    counters.packet_too_big.increment();
                });
                trace!("<IcmpIpTransportContext as IpTransportContext<Ipv6>>::receive_ip_packet: Received a Packet Too Big message");
                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    // We are updating the path MTU from the destination address
                    // of this `packet` (which is an IP address on this node) to
                    // some remote (identified by the source address of this
                    // `packet`).
                    //
                    // `update_pmtu_if_less` may return an error, but it will
                    // only happen if the Packet Too Big message's MTU field had
                    // a value that was less than the IPv6 minimum MTU (which as
                    // per IPv6 RFC 8200, must not happen).
                    core_ctx.update_pmtu_if_less(
                        bindings_ctx,
                        dst_ip.get(),
                        src_ip.get(),
                        Mtu::new(packet_too_big.message().mtu()),
                    );
                }
                receive_icmpv6_error(
                    core_ctx,
                    bindings_ctx,
                    device,
                    &packet_too_big,
                    Icmpv6ErrorCode::PacketTooBig,
                );
            }
            Icmpv6Packet::Mld(packet) => {
                core_ctx.receive_mld_packet(bindings_ctx, &device, src_ip, dst_ip, packet);
            }
            Icmpv6Packet::DestUnreachable(dest_unreachable) => receive_icmpv6_error(
                core_ctx,
                bindings_ctx,
                device,
                &dest_unreachable,
                Icmpv6ErrorCode::DestUnreachable(dest_unreachable.code()),
            ),
            Icmpv6Packet::TimeExceeded(time_exceeded) => receive_icmpv6_error(
                core_ctx,
                bindings_ctx,
                device,
                &time_exceeded,
                Icmpv6ErrorCode::TimeExceeded(time_exceeded.code()),
            ),
            Icmpv6Packet::ParameterProblem(parameter_problem) => receive_icmpv6_error(
                core_ctx,
                bindings_ctx,
                device,
                &parameter_problem,
                Icmpv6ErrorCode::ParameterProblem(parameter_problem.code()),
            ),
        }

        Ok(())
    }
}

/// Sends an ICMP reply to a remote host.
///
/// `send_icmp_reply` sends a reply to a non-error message (e.g., "echo request"
/// or "timestamp request" messages). It takes the ingress device, source IP,
/// and destination IP of the packet *being responded to*. It uses ICMP-specific
/// logic to figure out whether and how to send an ICMP reply.
///
/// `get_body_from_src_ip` returns a `Serializer` with the bytes of the ICMP
/// packet, and, when called, is given the source IP address chosen for the
/// outbound packet. This allows `get_body_from_src_ip` to properly compute the
/// ICMP checksum, which relies on both the source and destination IP addresses
/// of the IP packet it's encapsulated in.
fn send_icmp_reply<I, BC, CC, S, F>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: Option<&CC::DeviceId>,
    original_src_ip: SocketIpAddr<I::Addr>,
    original_dst_ip: SocketIpAddr<I::Addr>,
    get_body_from_src_ip: F,
) where
    I: crate::ip::IpExt,
    BC: IcmpBindingsContext<I, CC::DeviceId>,
    CC: IpSocketHandler<I, BC> + DeviceIdContext<AnyDevice> + CounterContext<IcmpTxCounters<I>>,
    S: Serializer,
    S::Buffer: BufferMut,
    F: FnOnce(SpecifiedAddr<I::Addr>) -> S,
{
    trace!("send_icmp_reply({:?}, {}, {})", device, original_src_ip, original_dst_ip);
    core_ctx.with_counters(|counters| {
        counters.reply.increment();
    });
    core_ctx
        .send_oneshot_ip_packet(
            bindings_ctx,
            None,
            Some(original_dst_ip),
            original_src_ip,
            I::ICMP_IP_PROTO,
            DefaultSendOptions,
            |src_ip| get_body_from_src_ip(src_ip.into()),
            None,
        )
        .unwrap_or_else(|(err, DefaultSendOptions)| {
            debug!("failed to send ICMP reply: {}", err);
        })
}

/// Receive an ICMP(v4) error message.
///
/// `receive_icmpv4_error` handles an incoming ICMP error message by parsing the
/// original IPv4 packet and then delegating to the context.
fn receive_icmpv4_error<
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC>,
    B: ByteSlice,
    M: IcmpMessage<Ipv4, Body<B> = OriginalPacket<B>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    packet: &IcmpPacket<Ipv4, B, M>,
    err: Icmpv4ErrorCode,
) {
    packet.with_original_packet(|res| match res {
        Ok(original_packet) => {
            let dst_ip = match SpecifiedAddr::new(original_packet.dst_ip()) {
                Some(ip) => ip,
                None => {
                    trace!("receive_icmpv4_error: Got ICMP error message whose original IPv4 packet contains an unspecified destination address; discarding");
                    return;
                },
            };
            InnerIcmpContext::receive_icmp_error(
                core_ctx,
                bindings_ctx,
                device,
                SpecifiedAddr::new(original_packet.src_ip()),
                dst_ip,
                original_packet.proto(),
                original_packet.body().into_inner(),
                err,
            );
        }
        Err(_) => debug!(
            "receive_icmpv4_error: Got ICMP error message with unparsable original IPv4 packet"
        ),
    })
}

/// Receive an ICMPv6 error message.
///
/// `receive_icmpv6_error` handles an incoming ICMPv6 error message by parsing
/// the original IPv6 packet and then delegating to the context.
fn receive_icmpv6_error<
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC>,
    B: ByteSlice,
    M: IcmpMessage<Ipv6, Body<B> = OriginalPacket<B>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    packet: &IcmpPacket<Ipv6, B, M>,
    err: Icmpv6ErrorCode,
) {
    packet.with_original_packet(|res| match res {
        Ok(original_packet) => {
            let dst_ip = match SpecifiedAddr::new(original_packet.dst_ip()) {
                Some(ip)=>ip,
                None => {
                    trace!("receive_icmpv6_error: Got ICMP error message whose original IPv6 packet contains an unspecified destination address; discarding");
                    return;
                },
            };
            match original_packet.body_proto() {
                Ok((body, proto)) => {
                    InnerIcmpContext::receive_icmp_error(
                        core_ctx,
                        bindings_ctx,
                        device,
                        SpecifiedAddr::new(original_packet.src_ip()),
                        dst_ip,
                        proto,
                        body.into_inner(),
                        err,
                    );
                }
                Err(ExtHdrParseError) => {
                    trace!("receive_icmpv6_error: We could not parse the original packet's extension headers, and so we don't know where the original packet's body begins; discarding");
                    // There's nothing we can do in this case, so we just
                    // return.
                    return;
                }
            }
        }
        Err(_body) => debug!(
            "receive_icmpv6_error: Got ICMPv6 error message with unparsable original IPv6 packet"
        ),
    })
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unsupported IPv4 protocol.
///
/// `send_icmpv4_protocol_unreachable` sends the appropriate ICMP message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` identifying an
/// unsupported protocol - in particular, a "destination unreachable" message
/// with a "protocol unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the IP header. This must be a whole packet, not a packet fragment.
/// `header_len` is the length of the header including all options.
pub(crate) fn send_icmpv4_protocol_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.protocol_unreachable.increment();
    });

    send_icmpv4_dest_unreachable(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestProtocolUnreachable,
        original_packet,
        header_len,
        // If we are sending a protocol unreachable error it is correct to assume that, if the
        // packet was initially fragmented, it has been successfully reassembled by now. It
        // guarantees that we won't send more than one ICMP Destination Unreachable message for
        // different fragments of the same original packet, so we should behave as if we are
        // handling an initial fragment.
        Ipv4FragmentType::InitialFragment,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unsupported Next Header.
///
/// `send_icmpv6_protocol_unreachable` is like
/// [`send_icmpv4_protocol_unreachable`], but for ICMPv6. It sends an ICMPv6
/// "parameter problem" message with an "unrecognized next header type" code.
///
/// `header_len` is the length of all IPv6 headers (including extension headers)
/// *before* the payload with the problematic Next Header type.
pub(crate) fn send_icmpv6_protocol_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.protocol_unreachable.increment();
    });

    send_icmpv6_parameter_problem(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
        // Per RFC 4443, the pointer refers to the first byte of the packet
        // whose Next Header field was unrecognized. It is measured as an offset
        // from the beginning of the first IPv6 header. E.g., a pointer of 40
        // (the length of a single IPv6 header) would indicate that the Next
        // Header field from that header - and hence of the first encapsulated
        // packet - was unrecognized.
        //
        // NOTE: Since header_len is a usize, this could theoretically be a
        // lossy conversion. However, all that means in practice is that, if a
        // remote host somehow managed to get us to process a frame with a 4GB
        // IP header and send an ICMP response, the pointer value would be
        // wrong. It's not worth wasting special logic to avoid generating a
        // malformed packet in a case that will almost certainly never happen.
        Icmpv6ParameterProblem::new(header_len as u32),
        original_packet,
        false,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unreachable local transport-layer port.
///
/// `send_icmpv4_port_unreachable` sends the appropriate ICMP message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` with an
/// unreachable local transport-layer port. In particular, this is an ICMP
/// "destination unreachable" message with a "port unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the IP header. This must be a whole packet, not a packet fragment.
/// `header_len` is the length of the header including all options.
pub(crate) fn send_icmpv4_port_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.port_unreachable.increment();
    });

    send_icmpv4_dest_unreachable(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestPortUnreachable,
        original_packet,
        header_len,
        // If we are sending a port unreachable error it is correct to assume that, if the packet
        // was initially fragmented, it has been successfully reassembled by now. It guarantees that
        // we won't send more than one ICMP Destination Unreachable message for different fragments
        // of the same original packet, so we should behave as if we are handling an initial
        // fragment.
        Ipv4FragmentType::InitialFragment,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unreachable local transport-layer port.
///
/// `send_icmpv6_port_unreachable` is like [`send_icmpv4_port_unreachable`], but
/// for ICMPv6.
///
/// `original_packet` contains the contents of the entire original packet,
/// including extension headers.
pub(crate) fn send_icmpv6_port_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    original_packet: B,
) {
    core_ctx.with_counters(|counters| {
        counters.port_unreachable.increment();
    });

    send_icmpv6_dest_unreachable(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6DestUnreachableCode::PortUnreachable,
        original_packet,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unreachable network.
///
/// `send_icmpv4_net_unreachable` sends the appropriate ICMP message in response
/// to receiving an IP packet from `src_ip` to an unreachable `dst_ip`. In
/// particular, this is an ICMP "destination unreachable" message with a "net
/// unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet -
/// including all IP headers. `header_len` is the length of the IPv4 header. It
/// is ignored for IPv6.
pub(crate) fn send_icmpv4_net_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    core_ctx.with_counters(|counters| {
        counters.net_unreachable.increment();
    });

    // Check whether we MUST NOT send an ICMP error message
    // because the original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv4>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv4_dest_unreachable(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestNetworkUnreachable,
        original_packet,
        header_len,
        fragment_type,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unreachable network.
///
/// `send_icmpv6_net_unreachable` is like [`send_icmpv4_net_unreachable`], but
/// for ICMPv6. It sends an ICMPv6 "destination unreachable" message with a "no
/// route to destination" code.
///
/// `original_packet` contains the contents of the entire original packet
/// including extension headers. `header_len` is the length of the IP header and
/// all extension headers.
pub(crate) fn send_icmpv6_net_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.net_unreachable.increment();
    });

    // Check whether we MUST NOT send an ICMP error message
    // because the original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_dest_unreachable(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6DestUnreachableCode::NoRoute,
        original_packet,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet whose TTL has
/// expired.
///
/// `send_icmpv4_ttl_expired` sends the appropriate ICMP in response to
/// receiving an IP packet from `src_ip` to `dst_ip` whose TTL has expired. In
/// particular, this is an ICMP "time exceeded" message with a "time to live
/// exceeded in transit" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the header. `header_len` is the length of the IP header including
/// options.
pub(crate) fn send_icmpv4_ttl_expired<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    core_ctx.with_counters(|counters| {
        counters.ttl_expired.increment();
    });

    // Check whether we MUST NOT send an ICMP error message because the original
    // packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv4>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv4_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4TimeExceededCode::TtlExpired,
        IcmpTimeExceeded::default(),
        original_packet,
        header_len,
        fragment_type,
    )
}

/// Send an ICMPv6 message in response to receiving a packet whose hop limit has
/// expired.
///
/// `send_icmpv6_ttl_expired` is like [`send_icmpv4_ttl_expired`], but for
/// ICMPv6. It sends an ICMPv6 "time exceeded" message with a "hop limit
/// exceeded in transit" code.
///
/// `original_packet` contains the contents of the entire original packet
/// including extension headers. `header_len` is the length of the IP header and
/// all extension headers.
pub(crate) fn send_icmpv6_ttl_expired<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.ttl_expired.increment();
    });

    // Check whether we MUST NOT send an ICMP error message because the
    // original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6TimeExceededCode::HopLimitExceeded,
        IcmpTimeExceeded::default(),
        original_packet,
        false, /* allow_dst_multicast */
    )
}

// TODO(joshlf): Test send_icmpv6_packet_too_big once we support fake IPv6 test
// setups.

/// Send an ICMPv6 message in response to receiving a packet whose size exceeds
/// the MTU of the next hop interface.
///
/// `send_icmpv6_packet_too_big` sends an ICMPv6 "packet too big" message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` whose size
/// exceeds the `mtu` of the next hop interface.
pub(crate) fn send_icmpv6_packet_too_big<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    mtu: Mtu,
    original_packet: B,
    header_len: usize,
) {
    core_ctx.with_counters(|counters| {
        counters.packet_too_big.increment();
    });
    // Check whether we MUST NOT send an ICMP error message because the
    // original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        IcmpUnusedCode,
        Icmpv6PacketTooBig::new(mtu.into()),
        original_packet,
        // As per RFC 4443 section 2.4.e,
        //
        //   An ICMPv6 error message MUST NOT be originated as a result of
        //   receiving the following:
        //
        //     (e.3) A packet destined to an IPv6 multicast address.  (There are
        //           two exceptions to this rule: (1) the Packet Too Big Message
        //           (Section 3.2) to allow Path MTU discovery to work for IPv6
        //           multicast, and (2) the Parameter Problem Message, Code 2
        //           (Section 3.4) reporting an unrecognized IPv6 option (see
        //           Section 4.2 of [IPv6]) that has the Option Type highest-
        //           order two bits set to 10).
        //
        //     (e.4) A packet sent as a link-layer multicast (the exceptions
        //           from e.3 apply to this case, too).
        //
        // Thus, we explicitly allow sending a Packet Too Big error if the
        // destination was a multicast packet.
        true, /* allow_dst_multicast */
    )
}

pub(crate) fn send_icmpv4_parameter_problem<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    code: Icmpv4ParameterProblemCode,
    parameter_problem: Icmpv4ParameterProblem,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    core_ctx.with_counters(|counters| {
        counters.parameter_problem.increment();
    });

    send_icmpv4_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        parameter_problem,
        original_packet,
        header_len,
        fragment_type,
    )
}

/// Send an ICMPv6 Parameter Problem error message.
///
/// If the error message is Code 2 reporting an unrecognized IPv6 option that
/// has the Option Type highest-order two bits set to 10, `allow_dst_multicast`
/// must be set to `true`. See [`should_send_icmpv6_error`] for more details.
///
/// # Panics
///
/// Panics if `allow_multicast_addr` is set to `true`, but this Parameter
/// Problem's code is not 2 (Unrecognized IPv6 Option).
pub(crate) fn send_icmpv6_parameter_problem<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    code: Icmpv6ParameterProblemCode,
    parameter_problem: Icmpv6ParameterProblem,
    original_packet: B,
    allow_dst_multicast: bool,
) {
    // Only allow the `allow_dst_multicast` parameter to be set if the code is
    // the unrecognized IPv6 option as that is one of the few exceptions where
    // we can send an ICMP packet in response to a packet that was destined for
    // a multicast address.
    assert!(!allow_dst_multicast || code == Icmpv6ParameterProblemCode::UnrecognizedIpv6Option);

    core_ctx.with_counters(|counters| {
        counters.parameter_problem.increment();
    });

    send_icmpv6_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        parameter_problem,
        original_packet,
        allow_dst_multicast,
    )
}

fn send_icmpv4_dest_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv4Addr>,
    dst_ip: SocketIpAddr<Ipv4Addr>,
    code: Icmpv4DestUnreachableCode,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    core_ctx.with_counters(|counters| {
        counters.dest_unreachable.increment();
    });
    send_icmpv4_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        IcmpDestUnreachable::default(),
        original_packet,
        header_len,
        fragment_type,
    )
}

fn send_icmpv6_dest_unreachable<
    B: BufferMut,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SocketIpAddr<Ipv6Addr>,
    dst_ip: SocketIpAddr<Ipv6Addr>,
    code: Icmpv6DestUnreachableCode,
    original_packet: B,
) {
    send_icmpv6_error_message(
        core_ctx,
        bindings_ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        IcmpDestUnreachable::default(),
        original_packet,
        false, /* allow_dst_multicast */
    )
}

fn send_icmpv4_error_message<
    B: BufferMut,
    M: IcmpMessage<Ipv4>,
    BC: IcmpBindingsContext<Ipv4, CC::DeviceId>,
    CC: InnerIcmpv4Context<BC> + CounterContext<IcmpTxCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    original_src_ip: SocketIpAddr<Ipv4Addr>,
    original_dst_ip: SocketIpAddr<Ipv4Addr>,
    code: M::Code,
    message: M,
    mut original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    // TODO(https://fxbug.dev/95827): Come up with rules for when to send ICMP
    // error messages.

    if !should_send_icmpv4_error(
        frame_dst,
        original_src_ip.into(),
        original_dst_ip.into(),
        fragment_type,
    ) {
        return;
    }

    // Per RFC 792, body contains entire IPv4 header + 64 bytes of original
    // body.
    original_packet.shrink_back_to(header_len + 64);

    // TODO(https://fxbug.dev/95828): Improve source address selection for ICMP
    // errors sent from unnumbered/router interfaces.
    let _ = try_send_error!(
        core_ctx,
        bindings_ctx,
        core_ctx.send_oneshot_ip_packet(
            bindings_ctx,
            Some(EitherDeviceId::Strong(device)),
            None,
            original_src_ip,
            Ipv4Proto::Icmp,
            DefaultSendOptions,
            |local_ip| {
                original_packet.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                    local_ip.addr(),
                    original_src_ip.addr(),
                    code,
                    message,
                ))
            },
            None
        )
    );
}

fn send_icmpv6_error_message<
    B: BufferMut,
    M: IcmpMessage<Ipv6>,
    BC: IcmpBindingsContext<Ipv6, CC::DeviceId>,
    CC: InnerIcmpv6Context<BC> + CounterContext<IcmpTxCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    original_src_ip: SocketIpAddr<Ipv6Addr>,
    original_dst_ip: SocketIpAddr<Ipv6Addr>,
    code: M::Code,
    message: M,
    original_packet: B,
    allow_dst_multicast: bool,
) {
    // TODO(https://fxbug.dev/95827): Come up with rules for when to send ICMP
    // error messages.

    if !should_send_icmpv6_error(
        frame_dst,
        original_src_ip.into(),
        original_dst_ip.into(),
        allow_dst_multicast,
    ) {
        return;
    }

    // TODO(https://fxbug.dev/95828): Improve source address selection for ICMP
    // errors sent from unnumbered/router interfaces.
    let _ = try_send_error!(
        core_ctx,
        bindings_ctx,
        core_ctx.send_oneshot_ip_packet(
            bindings_ctx,
            Some(EitherDeviceId::Strong(device)),
            None,
            original_src_ip,
            Ipv6Proto::Icmpv6,
            DefaultSendOptions,
            |local_ip| {
                let icmp_builder = IcmpPacketBuilder::<Ipv6, _>::new(
                    local_ip.addr(),
                    original_src_ip.addr(),
                    code,
                    message,
                );

                // Per RFC 4443, body contains as much of the original body as
                // possible without exceeding IPv6 minimum MTU.
                TruncatingSerializer::new(original_packet, TruncateDirection::DiscardBack)
                    .encapsulate(icmp_builder)
            },
            Some(Ipv6::MINIMUM_LINK_MTU.get()),
        )
    );
}

/// Should we send an ICMP(v4) response?
///
/// `should_send_icmpv4_error` implements the logic described in RFC 1122
/// Section 3.2.2. It decides whether, upon receiving an incoming packet with
/// the given parameters, we should send an ICMP response or not. In particular,
/// we do not send an ICMP response if we've received:
/// - a packet destined to a broadcast or multicast address
/// - a packet sent in a link-layer broadcast
/// - a non-initial fragment
/// - a packet whose source address does not define a single host (a
///   zero/unspecified address, a loopback address, a broadcast address, a
///   multicast address, or a Class E address)
///
/// Note that `should_send_icmpv4_error` does NOT check whether the incoming
/// packet contained an ICMP error message. This is because that check is
/// unnecessary for some ICMP error conditions. The ICMP error message check can
/// be performed separately with `is_icmp_error_message`.
fn should_send_icmpv4_error(
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    fragment_type: Ipv4FragmentType,
) -> bool {
    // NOTE: We do not explicitly implement the "unspecified address" check, as
    // it is enforced by the types of the arguments.

    // TODO(joshlf): Implement the rest of the rules:
    // - a packet destined to a subnet broadcast address
    // - a packet whose source address is a subnet broadcast address

    // NOTE: The FrameDestination type has variants for unicast, multicast, and
    // broadcast. One implication of the fact that we only check for broadcast
    // here (in compliance with the RFC) is that we could, in one very unlikely
    // edge case, respond with an ICMP error message to an IP packet which was
    // sent in a link-layer multicast frame. In particular, that can happen if
    // we subscribe to a multicast IP group and, as a result, subscribe to the
    // corresponding multicast MAC address, and we receive a unicast IP packet
    // in a multicast link-layer frame destined to that MAC address.
    //
    // TODO(joshlf): Should we filter incoming multicast IP traffic to make sure
    // that it matches the multicast MAC address of the frame it was
    // encapsulated in?
    fragment_type == Ipv4FragmentType::InitialFragment
        && !(dst_ip.is_multicast()
            || dst_ip.is_limited_broadcast()
            || frame_dst.is_broadcast()
            || src_ip.is_loopback()
            || src_ip.is_limited_broadcast()
            || src_ip.is_multicast()
            || src_ip.is_class_e())
}

/// Should we send an ICMPv6 response?
///
/// `should_send_icmpv6_error` implements the logic described in RFC 4443
/// Section 2.4.e. It decides whether, upon receiving an incoming packet with
/// the given parameters, we should send an ICMP response or not. In particular,
/// we do not send an ICMP response if we've received:
/// - a packet destined to a multicast address
///   - Two exceptions to this rules:
///     1) the Packet Too Big Message to allow Path MTU discovery to work for
///        IPv6 multicast
///     2) the Parameter Problem Message, Code 2 reporting an unrecognized IPv6
///        option that has the Option Type highest-order two bits set to 10
/// - a packet sent as a link-layer multicast or broadcast
///   - same exceptions apply here as well.
/// - a packet whose source address does not define a single host (a
///   zero/unspecified address, a loopback address, or a multicast address)
///
/// If an ICMP response will be a Packet Too Big Message or a Parameter Problem
/// Message, Code 2 reporting an unrecognized IPv6 option that has the Option
/// Type highest-order two bits set to 10, `info.allow_dst_multicast` must be
/// set to `true` so this function will allow the exception mentioned above.
///
/// Note that `should_send_icmpv6_error` does NOT check whether the incoming
/// packet contained an ICMP error message. This is because that check is
/// unnecessary for some ICMP error conditions. The ICMP error message check can
/// be performed separately with `is_icmp_error_message`.
fn should_send_icmpv6_error(
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    allow_dst_multicast: bool,
) -> bool {
    // NOTE: We do not explicitly implement the "unspecified address" check, as
    // it is enforced by the types of the arguments.
    !((!allow_dst_multicast
        && (dst_ip.is_multicast() || frame_dst.is_multicast() || frame_dst.is_broadcast()))
        || src_ip.is_loopback()
        || src_ip.is_multicast())
}

/// Determine whether or not an IP packet body contains an ICMP error message
/// for the purposes of determining whether or not to send an ICMP response.
///
/// `is_icmp_error_message` checks whether `proto` is ICMP(v4) for IPv4 or
/// ICMPv6 for IPv6 and, if so, attempts to parse `buf` as an ICMP packet in
/// order to determine whether it is an error message or not. If parsing fails,
/// it conservatively assumes that it is an error packet in order to avoid
/// violating the MUST NOT directives of RFC 1122 Section 3.2.2 and [RFC 4443
/// Section 2.4.e].
///
/// [RFC 4443 Section 2.4.e]: https://tools.ietf.org/html/rfc4443#section-2.4
fn is_icmp_error_message<I: IcmpIpExt>(proto: I::Proto, buf: &[u8]) -> bool {
    proto == I::ICMP_IP_PROTO
        && peek_message_type::<I::IcmpMessageType>(buf).map(IcmpMessageType::is_err).unwrap_or(true)
}

/// Common logic for receiving an ICMP echo reply.
fn receive_icmp_echo_reply<
    I: IcmpIpExt + IpExt,
    B: BufferMut,
    BC: IcmpBindingsContext<I, CC::DeviceId>,
    CC: InnerIcmpContext<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    src_ip: I::Addr,
    dst_ip: SpecifiedAddr<I::Addr>,
    id: u16,
    body: B,
    device: CC::WeakDeviceId,
) {
    let src_ip = match SpecifiedAddr::new(src_ip) {
        Some(src_ip) => src_ip,
        None => {
            trace!("receive_icmp_echo_reply: unspecified source address");
            return;
        }
    };
    let src_ip: SocketIpAddr<_> = match src_ip.try_into() {
        Ok(src_ip) => src_ip,
        Err(AddrIsMappedError {}) => {
            trace!("receive_icmp_echo_reply: mapped source address");
            return;
        }
    };
    let dst_ip: SocketIpAddr<_> = match dst_ip.try_into() {
        Ok(dst_ip) => dst_ip,
        Err(AddrIsMappedError {}) => {
            trace!("receive_icmp_echo_reply: mapped destination address");
            return;
        }
    };
    core_ctx.with_icmp_ctx_and_sockets_mut(|device_ctx, sockets| {
        if let Some((id, strong_device)) =
            NonZeroU16::new(id).zip(device_ctx.upgrade_weak_device_id(&device))
        {
            let mut addrs_to_search = AddrVecIter::<I, CC::WeakDeviceId, IcmpAddrSpec>::with_device(
                ConnIpAddr { local: (dst_ip, id), remote: (src_ip, ()) }.into(),
                device,
            );
            let socket = match addrs_to_search.try_for_each(|addr_vec| {
                match addr_vec {
                    AddrVec::Conn(c) => {
                        if let Some(id) = sockets.socket_map.conns().get_by_addr(&c) {
                            return ControlFlow::Break(id);
                        }
                    }
                    AddrVec::Listen(l) => {
                        if let Some(id) = sockets.socket_map.listeners().get_by_addr(&l) {
                            return ControlFlow::Break(id);
                        }
                    }
                }
                ControlFlow::Continue(())
            }) {
                ControlFlow::Continue(()) => None,
                ControlFlow::Break(id) => Some(id),
            };
            if let Some(socket) = socket {
                trace!("receive_icmp_echo_reply: Received echo reply for local socket");
                bindings_ctx.receive_icmp_echo_reply(
                    *socket,
                    &strong_device,
                    src_ip.addr(),
                    dst_ip.addr(),
                    id.get(),
                    body,
                );
                return;
            }
        }
        // TODO(https://fxbug.dev/47952): Neither the ICMPv4 or ICMPv6 RFCs
        // explicitly state what to do in case we receive an "unsolicited"
        // echo reply. We only expose the replies if we have a registered
        // connection for the IcmpAddr of the incoming reply for now. Given
        // that a reply should only be sent in response to a request, an
        // ICMP unreachable-type message is probably not appropriate for
        // unsolicited replies. However, it's also possible that we sent a
        // request and then closed the socket before receiving the reply, so
        // this doesn't necessarily indicate a buggy or malicious remote
        // host. We should figure this out definitively.
        //
        // If we do decide to send an ICMP error message, the appropriate
        // thing to do is probably to have this function return a `Result`,
        // and then have the top-level implementation of
        // `IpTransportContext::receive_ip_packet` return the
        // appropriate error.
        trace!("receive_icmp_echo_reply: Received echo reply with no local socket");
    })
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use core::{convert::TryInto, fmt::Debug, num::NonZeroU16, time::Duration};

    use net_types::{
        ip::{Ip, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet},
        ZonedAddr,
    };
    use packet::{Buf, Serializer};
    use packet_formats::{
        ethernet::EthernetFrameLengthCheck,
        icmp::{
            mld::MldPacket, IcmpEchoRequest, IcmpMessage, IcmpPacket, IcmpUnusedCode,
            Icmpv4TimestampRequest, MessageBody,
        },
        ip::{IpPacketBuilder, IpProto},
        testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
        udp::UdpPacketBuilder,
        utils::NonZeroDuration,
    };

    use super::*;
    use crate::{
        context::testutil::{
            FakeBindingsCtx, FakeCoreCtx, FakeCtxWithCoreCtx, FakeInstant, Wrapped,
        },
        context::CounterContext,
        device::{
            testutil::{set_forwarding_enabled, FakeDeviceId, FakeWeakDeviceId},
            DeviceId, FrameDestination,
        },
        ip::{
            device::{
                route_discovery::Ipv6DiscoveredRoute, state::IpDeviceStateIpExt, IpDeviceAddr,
                IpDeviceHandler,
            },
            icmp::socket::{IcmpEchoSocketApi, SocketId, SocketsState, StateContext},
            path_mtu::testutil::FakePmtuState,
            receive_ip_packet,
            socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx},
            testutil::DualStackSendIpPacketMeta,
            types::RoutableIpAddr,
            IpCounters, SendIpPacketMeta,
        },
        socket::address::SocketZonedIpAddr,
        state::StackStateBuilder,
        testutil::{assert_empty, Ctx, TestIpExt, FAKE_CONFIG_V4, FAKE_CONFIG_V6},
        transport::udp::UdpStateBuilder,
        uninstantiable::UninstantiableWrapper,
    };

    /// The FakeCoreCtx held as the inner state of the [`WrappedFakeCoreCtx`] that
    /// is [`FakeCoreCtx`].
    type FakeBufferCoreCtx = FakeCoreCtx<
        FakeDualStackIpSocketCtx<FakeDeviceId>,
        DualStackSendIpPacketMeta<FakeDeviceId>,
        FakeDeviceId,
    >;

    impl<Inner, I: Ip, D> CounterContext<IpCounters<I>>
        for Wrapped<
            Inner,
            FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>,
        >
    {
        fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
            cb(self.as_ref().get_ref().get_common_counters::<I>())
        }
    }

    impl<I: Ip, D> CounterContext<IcmpTxCounters<I>>
        for FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>
    {
        fn with_counters<O, F: FnOnce(&IcmpTxCounters<I>) -> O>(&self, cb: F) -> O {
            cb(self.get_ref().icmp_tx_counters::<I>())
        }
    }

    impl<Inner, I: Ip, D> CounterContext<IcmpTxCounters<I>>
        for Wrapped<
            Inner,
            FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>,
        >
    {
        fn with_counters<O, F: FnOnce(&IcmpTxCounters<I>) -> O>(&self, cb: F) -> O {
            cb(self.as_ref().get_ref().icmp_tx_counters::<I>())
        }
    }

    impl<I: Ip, D> CounterContext<IcmpRxCounters<I>>
        for FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>
    {
        fn with_counters<O, F: FnOnce(&IcmpRxCounters<I>) -> O>(&self, cb: F) -> O {
            cb(self.get_ref().icmp_rx_counters::<I>())
        }
    }

    impl<Inner, I: Ip, D> CounterContext<IcmpRxCounters<I>>
        for Wrapped<
            Inner,
            FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>,
        >
    {
        fn with_counters<O, F: FnOnce(&IcmpRxCounters<I>) -> O>(&self, cb: F) -> O {
            cb(self.as_ref().get_ref().icmp_rx_counters::<I>())
        }
    }

    impl<D> CounterContext<NdpCounters>
        for FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>
    {
        fn with_counters<O, F: FnOnce(&NdpCounters) -> O>(&self, cb: F) -> O {
            cb(&self.get_ref().ndp_counters)
        }
    }

    impl<Inner, D> CounterContext<NdpCounters>
        for Wrapped<
            Inner,
            FakeCoreCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>,
        >
    {
        fn with_counters<O, F: FnOnce(&NdpCounters) -> O>(&self, cb: F) -> O {
            cb(&self.as_ref().get_ref().ndp_counters)
        }
    }

    /// `FakeCoreCtx` specialized for ICMP.
    type FakeIcmpCoreCtx<I> =
        Wrapped<SocketsState<I, FakeWeakDeviceId<FakeDeviceId>>, FakeIcmpInnerCoreCtx<I>>;

    type FakeIcmpInnerCoreCtx<I> =
        Wrapped<FakeIcmpInnerCoreCtxState<I, FakeWeakDeviceId<FakeDeviceId>>, FakeBufferCoreCtx>;

    /// `FakeBindingsCtx` specialized for ICMP.
    type FakeIcmpBindingsCtx<I> = FakeBindingsCtx<(), (), FakeIcmpBindingsCtxState<I>>;

    /// A fake ICMP bindings and core contexts.
    ///
    /// This is exposed to super so it can be shared with the socket tests.
    pub(super) type FakeIcmpCtx<I> =
        FakeCtxWithCoreCtx<FakeIcmpCoreCtx<I>, (), (), FakeIcmpBindingsCtxState<I>>;

    pub(super) struct FakeIcmpInnerCoreCtxState<I: IpExt, W: WeakId> {
        bound_socket_map_and_allocator: BoundSockets<I, W>,
        error_send_bucket: TokenBucket<FakeInstant>,
        receive_icmp_error: Vec<I::ErrorCode>,
        pmtu_state: FakePmtuState<I::Addr>,
    }

    impl<I: IpExt, W: WeakId> FakeIcmpInnerCoreCtxState<I, W> {
        fn with_errors_per_second(errors_per_second: u64) -> Self {
            Self {
                bound_socket_map_and_allocator: Default::default(),
                error_send_bucket: TokenBucket::new(errors_per_second),
                receive_icmp_error: Default::default(),
                pmtu_state: Default::default(),
            }
        }
    }

    impl<I: TestIpExt> Default for FakeIcmpInnerCoreCtx<I> {
        fn default() -> Self {
            Wrapped::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new(core::iter::once(FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![I::FAKE_CONFIG.remote_ip],
                })),
                FakeIcmpInnerCoreCtxState::with_errors_per_second(DEFAULT_ERRORS_PER_SECOND),
            )
        }
    }

    impl<I: datagram::IpExt> IcmpStateContext for FakeIcmpInnerCoreCtx<I> {}
    impl IcmpStateContext for FakeBufferCoreCtx {}
    impl<I: datagram::IpExt> IcmpStateContext for FakeIcmpCoreCtx<I> {}
    impl IcmpStateContext for StackState<crate::testutil::FakeBindingsCtx> {}

    impl<I: datagram::IpExt + IpDeviceStateIpExt> InnerIcmpContext<I, FakeIcmpBindingsCtx<I>>
        for FakeIcmpInnerCoreCtx<I>
    {
        type DualStackContext = UninstantiableWrapper<Self>;
        type IpSocketsCtx<'a> = FakeBufferCoreCtx;
        fn receive_icmp_error(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<I>,
            _device: &Self::DeviceId,
            original_src_ip: Option<SpecifiedAddr<I::Addr>>,
            original_dst_ip: SpecifiedAddr<I::Addr>,
            original_proto: I::Proto,
            original_body: &[u8],
            err: I::ErrorCode,
        ) {
            let Self { outer, inner } = self;
            inner.with_counters(|counters: &IcmpRxCounters<I>| {
                counters.error.increment();
            });
            outer.receive_icmp_error.push(err);
            if original_proto == I::ICMP_IP_PROTO {
                receive_ip_transport_icmp_error(
                    self,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
        }

        fn with_icmp_sockets<O, F: FnOnce(&BoundSockets<I, Self::WeakDeviceId>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner: _ } = self;
            cb(&outer.bound_socket_map_and_allocator)
        }

        fn with_icmp_ctx_and_sockets_mut<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSockets<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, &mut outer.bound_socket_map_and_allocator)
        }

        fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<FakeInstant>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner: _ } = self;
            cb(&mut outer.error_send_bucket)
        }
    }

    impl<I: datagram::IpExt + IpDeviceStateIpExt> StateContext<I, FakeIcmpBindingsCtx<I>>
        for FakeIcmpCoreCtx<I>
    {
        type SocketStateCtx<'a> = FakeIcmpInnerCoreCtx<I>;

        fn with_sockets_state<
            O,
            F: FnOnce(&mut Self::SocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_sockets_state_mut<
            O,
            F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.inner)
        }
    }

    impl<I: IcmpIpExt, D> IcmpEchoBindingsContext<I, D> for FakeIcmpBindingsCtx<I> {
        fn receive_icmp_echo_reply<B: BufferMut>(
            &mut self,
            conn: SocketId<I>,
            _device_id: &D,
            src_ip: I::Addr,
            dst_ip: I::Addr,
            id: u16,
            data: B,
        ) {
            self.state_mut().receive_icmp_echo_reply.push(ReceiveIcmpEchoReply {
                conn,
                src_ip,
                dst_ip,
                id,
                data: data.as_ref().to_vec(),
            });
        }
    }

    // Tests that require an entire IP stack.

    /// Test that receiving a particular IP packet results in a particular ICMP
    /// response.
    ///
    /// Test that receiving an IP packet from remote host
    /// `I::FAKE_CONFIG.remote_ip` to host `dst_ip` with `ttl` and `proto`
    /// results in all of the counters in `assert_counters` being triggered at
    /// least once.
    ///
    /// If `expect_message_code` is `Some`, expect that exactly one ICMP packet
    /// was sent in response with the given message and code, and invoke the
    /// given function `f` on the packet. Otherwise, if it is `None`, expect
    /// that no response was sent.
    ///
    /// `modify_packet_builder` is invoked on the `PacketBuilder` before the
    /// packet is serialized.
    ///
    /// `modify_stack_state_builder` is invoked on the `StackStateBuilder`
    /// before it is used to build the context.
    ///
    /// The state is initialized to `I::FAKE_CONFIG` when testing.
    #[allow(clippy::too_many_arguments)]
    fn test_receive_ip_packet<
        I: TestIpExt + IcmpIpExt,
        C: PartialEq + Debug,
        M: IcmpMessage<I, Code = C> + PartialEq + Debug,
        PBF: FnOnce(&mut <I as packet_formats::ip::IpExt>::PacketBuilder),
        SSBF: FnOnce(&mut StackStateBuilder),
        F: for<'a> FnOnce(&IcmpPacket<I, &'a [u8], M>),
    >(
        modify_packet_builder: PBF,
        modify_stack_state_builder: SSBF,
        body: &mut [u8],
        dst_ip: SpecifiedAddr<I::Addr>,
        ttl: u8,
        proto: I::Proto,
        assert_counters: &[&str],
        expect_message_code: Option<(M, C)>,
        f: F,
    ) {
        crate::testutil::set_logger_for_test();
        let mut pb = <I as packet_formats::ip::IpExt>::PacketBuilder::new(
            *I::FAKE_CONFIG.remote_ip,
            dst_ip.get(),
            ttl,
            proto,
        );
        modify_packet_builder(&mut pb);
        let buffer = Buf::new(body, ..).encapsulate(pb).serialize_vec_outer().unwrap();

        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            I::FAKE_CONFIG.into_builder().build_with_modifications(modify_stack_state_builder);
        let core_ctx = &core_ctx;

        let device: DeviceId<_> = device_ids[0].clone().into();
        set_forwarding_enabled::<_, I>(&core_ctx, &mut bindings_ctx, &device, true)
            .expect("error setting routing enabled");
        match I::VERSION {
            IpVersion::V4 => receive_ip_packet::<_, _, Ipv4>(
                &core_ctx,
                &mut bindings_ctx,
                &device,
                FrameDestination::Individual { local: true },
                buffer,
            ),
            IpVersion::V6 => receive_ip_packet::<_, _, Ipv6>(
                &core_ctx,
                &mut bindings_ctx,
                &device,
                FrameDestination::Individual { local: true },
                buffer,
            ),
        }

        for counter in assert_counters {
            // TODO(https://fxbug.dev/134635): Redesign iterating through
            // assert_counters once CounterContext is removed.
            let count = match *counter {
                "send_ipv4_packet" => core_ctx.state.ipv4.inner.counters().send_ip_packet.get(),
                "send_ipv6_packet" => core_ctx.state.ipv6.inner.counters().send_ip_packet.get(),
                "echo_request" => core_ctx.state.icmp_rx_counters::<I>().echo_request.get(),
                "timestamp_request" => {
                    core_ctx.state.icmp_rx_counters::<I>().timestamp_request.get()
                }
                "protocol_unreachable" => {
                    core_ctx.state.icmp_tx_counters::<I>().protocol_unreachable.get()
                }
                "port_unreachable" => core_ctx.state.icmp_tx_counters::<I>().port_unreachable.get(),
                "net_unreachable" => core_ctx.state.icmp_tx_counters::<I>().net_unreachable.get(),
                "ttl_expired" => core_ctx.state.icmp_tx_counters::<I>().ttl_expired.get(),
                "packet_too_big" => core_ctx.state.icmp_tx_counters::<I>().packet_too_big.get(),
                "parameter_problem" => {
                    core_ctx.state.icmp_tx_counters::<I>().parameter_problem.get()
                }
                "dest_unreachable" => core_ctx.state.icmp_tx_counters::<I>().dest_unreachable.get(),
                "error" => core_ctx.state.icmp_tx_counters::<I>().error.get(),
                c => panic!("unrecognized counter: {c}"),
            };
            assert!(count > 0, "counter at zero: {counter}");
        }

        if let Some((expect_message, expect_code)) = expect_message_code {
            assert_eq!(bindings_ctx.frames_sent().len(), 1);
            let (src_mac, dst_mac, src_ip, dst_ip, _, message, code) =
                parse_icmp_packet_in_ip_packet_in_ethernet_frame::<I, _, M, _>(
                    &bindings_ctx.frames_sent()[0].1,
                    EthernetFrameLengthCheck::NoCheck,
                    f,
                )
                .unwrap();

            assert_eq!(src_mac, I::FAKE_CONFIG.local_mac.get());
            assert_eq!(dst_mac, I::FAKE_CONFIG.remote_mac.get());
            assert_eq!(src_ip, I::FAKE_CONFIG.local_ip.get());
            assert_eq!(dst_ip, I::FAKE_CONFIG.remote_ip.get());
            assert_eq!(message, expect_message);
            assert_eq!(code, expect_code);
        } else {
            assert_empty(bindings_ctx.frames_sent().iter());
        }
    }

    #[test]
    fn test_receive_echo() {
        crate::testutil::set_logger_for_test();

        // Test that, when receiving an echo request, we respond with an echo
        // reply with the appropriate parameters.

        fn test<I: TestIpExt + IcmpIpExt>(assert_counters: &[&str]) {
            let req = IcmpEchoRequest::new(0, 0);
            let req_body = &[1, 2, 3, 4];
            let mut buffer = Buf::new(req_body.to_vec(), ..)
                .encapsulate(IcmpPacketBuilder::<I, _>::new(
                    I::FAKE_CONFIG.remote_ip.get(),
                    I::FAKE_CONFIG.local_ip.get(),
                    IcmpUnusedCode,
                    req,
                ))
                .serialize_vec_outer()
                .unwrap();
            test_receive_ip_packet::<I, _, _, _, _, _>(
                |_| {},
                |_| {},
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                I::ICMP_IP_PROTO,
                assert_counters,
                Some((req.reply(), IcmpUnusedCode)),
                |packet| assert_eq!(packet.original_packet().bytes(), req_body),
            );
        }

        test::<Ipv4>(&["echo_request", "send_ipv4_packet"]);
        test::<Ipv6>(&["echo_request", "send_ipv6_packet"]);
    }

    #[test]
    fn test_receive_timestamp() {
        crate::testutil::set_logger_for_test();

        let req = Icmpv4TimestampRequest::new(1, 2, 3);
        let mut buffer = Buf::new(Vec::new(), ..)
            .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                FAKE_CONFIG_V4.remote_ip,
                FAKE_CONFIG_V4.local_ip,
                IcmpUnusedCode,
                req,
            ))
            .serialize_vec_outer()
            .unwrap();
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |builder| {
                let _: &mut Icmpv4StateBuilder =
                    builder.ipv4_builder().icmpv4_builder().send_timestamp_reply(true);
            },
            buffer.as_mut(),
            FAKE_CONFIG_V4.local_ip,
            64,
            Ipv4Proto::Icmp,
            &["timestamp_request", "send_ipv4_packet"],
            Some((req.reply(0x80000000, 0x80000000), IcmpUnusedCode)),
            |_| {},
        );
    }

    #[test]
    fn test_protocol_unreachable() {
        // Test receiving an IP packet for an unreachable protocol. Check to
        // make sure that we respond with the appropriate ICMP message.
        //
        // Currently, for IPv4, we test for all unreachable protocols, while for
        // IPv6, we only test for IGMP and TCP. See the comment below for why
        // that limitation exists. Once the limitation is fixed, we should test
        // with all unreachable protocols for both versions.

        for proto in 0u8..=255 {
            let v4proto = Ipv4Proto::from(proto);
            match v4proto {
                Ipv4Proto::Other(_) => {
                    test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
                        |_| {},
                        |_| {},
                        &mut [0u8; 128],
                        FAKE_CONFIG_V4.local_ip,
                        64,
                        v4proto,
                        &["protocol_unreachable"],
                        Some((
                            IcmpDestUnreachable::default(),
                            Icmpv4DestUnreachableCode::DestProtocolUnreachable,
                        )),
                        // Ensure packet is truncated to the right length.
                        |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
                    );
                }
                Ipv4Proto::Icmp
                | Ipv4Proto::Igmp
                | Ipv4Proto::Proto(IpProto::Udp)
                | Ipv4Proto::Proto(IpProto::Tcp) => {}
            }

            // TODO(https://fxbug.dev/47953): We seem to fail to parse an IPv6 packet if
            // its Next Header value is unrecognized (rather than treating this
            // as a valid parsing but then replying with a parameter problem
            // error message). We should a) fix this and, b) expand this test to
            // ensure we don't regress.
            let v6proto = Ipv6Proto::from(proto);
            match v6proto {
                Ipv6Proto::Icmpv6
                | Ipv6Proto::NoNextHeader
                | Ipv6Proto::Proto(IpProto::Udp)
                | Ipv6Proto::Proto(IpProto::Tcp)
                | Ipv6Proto::Other(_) => {}
            }
        }
    }

    #[test]
    fn test_port_unreachable() {
        // TODO(joshlf): Test TCP as well.

        // Receive an IP packet for an unreachable UDP port (1234). Check to
        // make sure that we respond with the appropriate ICMP message. Then, do
        // the same for a stack which has the UDP `send_port_unreachable` option
        // disable, and make sure that we DON'T respond with an ICMP message.

        fn test<I: TestIpExt + IcmpIpExt, C: PartialEq + Debug>(
            code: C,
            assert_counters: &[&str],
            original_packet_len: usize,
        ) where
            IcmpDestUnreachable:
                for<'a> IcmpMessage<I, Code = C, Body<&'a [u8]> = OriginalPacket<&'a [u8]>>,
        {
            let mut buffer = Buf::new(vec![0; 128], ..)
                .encapsulate(UdpPacketBuilder::new(
                    I::FAKE_CONFIG.remote_ip.get(),
                    I::FAKE_CONFIG.local_ip.get(),
                    None,
                    NonZeroU16::new(1234).unwrap(),
                ))
                .serialize_vec_outer()
                .unwrap();
            test_receive_ip_packet::<I, _, _, _, _, _>(
                |_| {},
                // Enable the `send_port_unreachable` feature.
                |builder| {
                    let _: &mut UdpStateBuilder =
                        builder.transport_builder().udp_builder().send_port_unreachable(true);
                },
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                IpProto::Udp.into(),
                assert_counters,
                Some((IcmpDestUnreachable::default(), code)),
                // Ensure packet is truncated to the right length.
                |packet| assert_eq!(packet.original_packet().bytes().len(), original_packet_len),
            );
            test_receive_ip_packet::<I, C, IcmpDestUnreachable, _, _, _>(
                |_| {},
                // Leave the `send_port_unreachable` feature disabled.
                |_: &mut StackStateBuilder| {},
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                IpProto::Udp.into(),
                &[],
                None,
                |_| {},
            );
        }

        test::<Ipv4, _>(Icmpv4DestUnreachableCode::DestPortUnreachable, &["port_unreachable"], 84);
        test::<Ipv6, _>(Icmpv6DestUnreachableCode::PortUnreachable, &["port_unreachable"], 176);
    }

    #[test]
    fn test_net_unreachable() {
        // Receive an IP packet for an unreachable destination address. Check to
        // make sure that we respond with the appropriate ICMP message.
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &["net_unreachable"],
            Some((
                IcmpDestUnreachable::default(),
                Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            )),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
        );
        test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv6Addr::from_bytes([
                1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8,
            ]))
            .unwrap(),
            64,
            IpProto::Udp.into(),
            &["net_unreachable"],
            Some((IcmpDestUnreachable::default(), Icmpv6DestUnreachableCode::NoRoute)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 168),
        );
        // Same test for IPv4 but with a non-initial fragment. No ICMP error
        // should be sent.
        test_receive_ip_packet::<Ipv4, _, IcmpDestUnreachable, _, _, _>(
            |pb| pb.fragment_offset(64),
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &[],
            None,
            |_| {},
        );
    }

    #[test]
    fn test_ttl_expired() {
        // Receive an IP packet with an expired TTL. Check to make sure that we
        // respond with the appropriate ICMP message.
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            FAKE_CONFIG_V4.remote_ip,
            1,
            IpProto::Udp.into(),
            &["ttl_expired"],
            Some((IcmpTimeExceeded::default(), Icmpv4TimeExceededCode::TtlExpired)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
        );
        test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            FAKE_CONFIG_V6.remote_ip,
            1,
            IpProto::Udp.into(),
            &["ttl_expired"],
            Some((IcmpTimeExceeded::default(), Icmpv6TimeExceededCode::HopLimitExceeded)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 168),
        );
        // Same test for IPv4 but with a non-initial fragment. No ICMP error
        // should be sent.
        test_receive_ip_packet::<Ipv4, _, IcmpTimeExceeded, _, _, _>(
            |pb| pb.fragment_offset(64),
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &[],
            None,
            |_| {},
        );
    }

    #[test]
    fn test_should_send_icmpv4_error() {
        let src_ip = FAKE_CONFIG_V4.local_ip;
        let dst_ip = FAKE_CONFIG_V4.remote_ip;
        let frame_dst = FrameDestination::Individual { local: true };
        let multicast_ip_1 = SpecifiedAddr::new(Ipv4Addr::new([224, 0, 0, 1])).unwrap();
        let multicast_ip_2 = SpecifiedAddr::new(Ipv4Addr::new([224, 0, 0, 2])).unwrap();

        // Should Send, unless non initial fragment.
        assert!(should_send_icmpv4_error(
            frame_dst,
            src_ip,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because destined for IP broadcast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because destined for multicast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because Link Layer Broadcast.
        assert!(!should_send_icmpv4_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from loopback addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LOOPBACK_ADDRESS,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LOOPBACK_ADDRESS,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from limited broadcast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from multicast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from class E addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            SpecifiedAddr::new(Ipv4Addr::new([240, 0, 0, 1])).unwrap(),
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            SpecifiedAddr::new(Ipv4Addr::new([240, 0, 0, 1])).unwrap(),
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));
    }

    #[test]
    fn test_should_send_icmpv6_error() {
        let src_ip = FAKE_CONFIG_V6.local_ip;
        let dst_ip = FAKE_CONFIG_V6.remote_ip;
        let frame_dst = FrameDestination::Individual { local: true };
        let multicast_ip_1 =
            SpecifiedAddr::new(Ipv6Addr::new([0xff00, 0, 0, 0, 0, 0, 0, 1])).unwrap();
        let multicast_ip_2 =
            SpecifiedAddr::new(Ipv6Addr::new([0xff00, 0, 0, 0, 0, 0, 0, 2])).unwrap();

        // Should Send.
        assert!(should_send_icmpv6_error(
            frame_dst, src_ip, dst_ip, false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            frame_dst, src_ip, dst_ip, true /* allow_dst_multicast */
        ));

        // Should not send because destined for multicast addr, unless exception
        // applies.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            true /* allow_dst_multicast */
        ));

        // Should not send because Link Layer Broadcast, unless exception
        // applies.
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from loopback addr.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            Ipv6::LOOPBACK_ADDRESS,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            Ipv6::LOOPBACK_ADDRESS,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from multicast addr.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from multicast addr, even though dest
        // multicast exception applies.
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            multicast_ip_2,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            multicast_ip_2,
            dst_ip,
            true /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            multicast_ip_1,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            multicast_ip_1,
            true /* allow_dst_multicast */
        ));
    }

    // Tests that only require an ICMP stack. Unlike the preceding tests, these
    // only test the ICMP stack and state, and fake everything else. We define
    // the `FakeIcmpv4Ctx` and `FakeIcmpv6Ctx` types, which we wrap in a
    // `FakeCtx` to provide automatic implementations of a number of required
    // traits. The rest we implement manually.

    // The arguments to `InnerIcmpContext::send_icmp_reply`.
    #[derive(Debug, PartialEq)]
    struct SendIcmpReplyArgs<A: IpAddress> {
        device: Option<FakeDeviceId>,
        src_ip: SpecifiedAddr<A>,
        dst_ip: SpecifiedAddr<A>,
        body: Vec<u8>,
    }

    // The arguments to `InnerIcmpContext::send_icmp_error_message`.
    #[derive(Debug, PartialEq)]
    struct SendIcmpErrorMessageArgs<I: IcmpIpExt> {
        src_ip: SpecifiedAddr<I::Addr>,
        dst_ip: SpecifiedAddr<I::Addr>,
        body: Vec<u8>,
        ip_mtu: Option<u32>,
    }

    // The arguments to `BufferIcmpContext::receive_icmp_echo_reply`.
    #[allow(unused)] // TODO(joshlf): Remove once we access these fields.
    struct ReceiveIcmpEchoReply<I: Ip> {
        conn: SocketId<I>,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        data: Vec<u8>,
    }

    #[derive(Default)]
    pub(super) struct FakeIcmpBindingsCtxState<I: IcmpIpExt> {
        receive_icmp_echo_reply: Vec<ReceiveIcmpEchoReply<I>>,
    }

    impl InnerIcmpv4Context<FakeIcmpBindingsCtx<Ipv4>> for FakeIcmpInnerCoreCtx<Ipv4> {
        fn should_send_timestamp_reply(&self) -> bool {
            false
        }
    }
    impl<I: datagram::IpExt> AsMut<FakePmtuState<I::Addr>> for FakeIcmpInnerCoreCtx<I> {
        fn as_mut(&mut self) -> &mut FakePmtuState<I::Addr> {
            &mut self.outer.pmtu_state
        }
    }
    impl_pmtu_handler!(FakeIcmpInnerCoreCtx<Ipv4>, FakeIcmpBindingsCtx<Ipv4>, Ipv4);
    impl_pmtu_handler!(FakeIcmpInnerCoreCtx<Ipv6>, FakeIcmpBindingsCtx<Ipv6>, Ipv6);

    impl<I: datagram::IpExt + IpDeviceStateIpExt>
        crate::ip::socket::IpSocketContext<I, FakeIcmpBindingsCtx<I>> for FakeIcmpInnerCoreCtx<I>
    {
        fn lookup_route(
            &mut self,
            bindings_ctx: &mut FakeIcmpBindingsCtx<I>,
            device: Option<&FakeDeviceId>,
            local_ip: Option<IpDeviceAddr<I::Addr>>,
            addr: RoutableIpAddr<I::Addr>,
        ) -> Result<crate::ip::types::ResolvedRoute<I, FakeDeviceId>, crate::ip::ResolveRouteError>
        {
            self.inner.lookup_route(bindings_ctx, device, local_ip, addr)
        }

        fn send_ip_packet<S>(
            &mut self,
            bindings_ctx: &mut FakeIcmpBindingsCtx<I>,
            meta: SendIpPacketMeta<I, &FakeDeviceId, SpecifiedAddr<I::Addr>>,
            body: S,
        ) -> Result<(), S>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            crate::ip::socket::IpSocketContext::<_, _>::send_ip_packet(
                &mut self.inner,
                bindings_ctx,
                meta,
                body,
            )
        }
    }

    impl IpDeviceHandler<Ipv6, FakeIcmpBindingsCtx<Ipv6>> for FakeIcmpInnerCoreCtx<Ipv6> {
        fn is_router_device(&mut self, _device_id: &Self::DeviceId) -> bool {
            unimplemented!()
        }

        fn set_default_hop_limit(&mut self, _device_id: &Self::DeviceId, _hop_limit: NonZeroU8) {
            unreachable!()
        }
    }

    impl IpDeviceStateContext<Ipv6, FakeIcmpBindingsCtx<Ipv6>> for FakeIcmpInnerCoreCtx<Ipv6> {
        fn with_next_packet_id<O, F: FnOnce(&()) -> O>(&self, cb: F) -> O {
            cb(&())
        }

        fn get_local_addr_for_remote(
            &mut self,
            _device_id: &Self::DeviceId,
            _remote: Option<SpecifiedAddr<Ipv6Addr>>,
        ) -> Option<SocketIpAddr<Ipv6Addr>> {
            unimplemented!()
        }

        fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
            unimplemented!()
        }

        fn address_status_for_device(
            &mut self,
            _addr: SpecifiedAddr<Ipv6Addr>,
            _device_id: &Self::DeviceId,
        ) -> AddressStatus<Ipv6PresentAddressStatus> {
            unimplemented!()
        }
    }

    impl Ipv6DeviceHandler<FakeIcmpBindingsCtx<Ipv6>> for FakeIcmpInnerCoreCtx<Ipv6> {
        type LinkLayerAddr = [u8; 0];

        fn get_link_layer_addr_bytes(&mut self, _device_id: &Self::DeviceId) -> Option<[u8; 0]> {
            unimplemented!()
        }

        fn set_discovered_retrans_timer(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _retrans_timer: NonZeroDuration,
        ) {
            unimplemented!()
        }

        fn remove_duplicate_tentative_address(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _addr: UnicastAddr<Ipv6Addr>,
        ) -> IpAddressState {
            unimplemented!()
        }

        fn set_link_mtu(&mut self, _device_id: &Self::DeviceId, _mtu: Mtu) {
            unimplemented!()
        }

        fn update_discovered_ipv6_route(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _route: Ipv6DiscoveredRoute,
            _lifetime: Option<NonZeroNdpLifetime>,
        ) {
            unimplemented!()
        }

        fn apply_slaac_update(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _subnet: Subnet<Ipv6Addr>,
            _preferred_lifetime: Option<NonZeroNdpLifetime>,
            _valid_lifetime: Option<NonZeroNdpLifetime>,
        ) {
            unimplemented!()
        }

        fn receive_mld_packet<B: ByteSlice>(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device: &FakeDeviceId,
            _src_ip: Ipv6SourceAddr,
            _dst_ip: SpecifiedAddr<Ipv6Addr>,
            _packet: MldPacket<B>,
        ) {
            unimplemented!()
        }
    }

    impl IpLayerHandler<Ipv6, FakeIcmpBindingsCtx<Ipv6>> for FakeIcmpInnerCoreCtx<Ipv6> {
        fn send_ip_packet_from_device<S>(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _meta: SendIpPacketMeta<Ipv6, &Self::DeviceId, Option<SpecifiedAddr<Ipv6Addr>>>,
            _body: S,
        ) -> Result<(), S> {
            unimplemented!()
        }
    }

    impl NudIpHandler<Ipv6, FakeIcmpBindingsCtx<Ipv6>> for FakeIcmpInnerCoreCtx<Ipv6> {
        fn handle_neighbor_probe(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _link_addr: &[u8],
        ) {
            unimplemented!()
        }

        fn handle_neighbor_confirmation(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _link_addr: &[u8],
            _flags: ConfirmationFlags,
        ) {
            unimplemented!()
        }

        fn flush_neighbor_table(
            &mut self,
            _bindings_ctx: &mut FakeIcmpBindingsCtx<Ipv6>,
            _device_id: &Self::DeviceId,
        ) {
            unimplemented!()
        }
    }

    const REMOTE_ID: u16 = 1;

    #[test]
    fn test_receive_icmpv4_error() {
        // Chosen arbitrarily to be a) non-zero (it's easy to accidentally get
        // the value 0) and, b) different from each other.
        const ICMP_ID: u16 = 0x0F;
        const SEQ_NUM: u16 = 0xF0;

        /// Test receiving an ICMP error message.
        ///
        /// Test that receiving an ICMP error message with the given code and
        /// message contents, and containing the given original IPv4 packet,
        /// results in the counter values in `assert_counters`. After that
        /// assertion passes, `f` is called on the context so that the caller
        /// can perform whatever extra validation they want.
        ///
        /// The error message will be sent from `FAKE_CONFIG_V4.remote_ip` to
        /// `FAKE_CONFIG_V4.local_ip`. Before the message is sent, an ICMP
        /// socket will be established with the ID `ICMP_ID`, and
        /// `test_receive_icmpv4_error_helper` will assert that its `SocketId`
        /// is 0. This allows the caller to craft the `original_packet` so that
        /// it should be delivered to this socket.
        fn test_receive_icmpv4_error_helper<
            C: Debug,
            M: IcmpMessage<Ipv4, Code = C> + Debug,
            F: Fn(&FakeIcmpCtx<Ipv4>),
        >(
            original_packet: &mut [u8],
            code: C,
            msg: M,
            assert_counters: &[(&str, u64)],
            f: F,
        ) {
            crate::testutil::set_logger_for_test();

            let mut ctx: FakeIcmpCtx<Ipv4> = FakeIcmpCtx::default();
            let mut socket_api = IcmpEchoSocketApi::<Ipv4, _>::new(ctx.as_mut());

            let conn = socket_api.create();
            // NOTE: This assertion is not a correctness requirement. It's just
            // that the rest of this test assumes that the new connection has ID
            // 0. If this assertion fails in the future, that isn't necessarily
            // evidence of a bug; we may just have to update this test to
            // accommodate whatever new ID allocation scheme is being used.
            assert_eq!(conn, SocketId::new(0));
            socket_api.bind(&conn, None, NonZeroU16::new(ICMP_ID)).unwrap();
            socket_api
                .connect(
                    &conn,
                    Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(FAKE_CONFIG_V4.remote_ip))),
                    REMOTE_ID,
                )
                .unwrap();

            let FakeCtxWithCoreCtx { core_ctx, bindings_ctx } = &mut ctx;
            <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FAKE_CONFIG_V4.remote_ip.get(),
                FAKE_CONFIG_V4.local_ip,
                Buf::new(original_packet, ..)
                    .encapsulate(IcmpPacketBuilder::new(
                        FAKE_CONFIG_V4.remote_ip,
                        FAKE_CONFIG_V4.local_ip,
                        code,
                        msg,
                    ))
                    .serialize_vec_outer()
                    .unwrap(),
            )
            .unwrap();

            for (ctr, expected) in assert_counters {
                let actual = match *ctr {
                    "InnerIcmpContext::receive_icmp_error" => {
                        core_ctx.inner.inner.state.icmp_rx_counters::<Ipv4>().error.get()
                    }
                    "IcmpIpTransportContext::receive_icmp_error" => core_ctx
                        .inner
                        .inner
                        .state
                        .icmp_rx_counters::<Ipv4>()
                        .error_at_transport_layer
                        .get(),
                    "IcmpEchoBindingsContext::receive_icmp_error" => {
                        core_ctx.inner.inner.state.icmp_rx_counters::<Ipv4>().error_at_socket.get()
                    }
                    c => panic!("unrecognized counter: {c}"),
                };
                assert_eq!(actual, *expected, "wrong count for {ctr}");
            }
            f(&ctx);
        }
        // Test that, when we receive various ICMPv4 error messages, we properly
        // pass them up to the IP layer and, sometimes, to the transport layer.

        // First, test with an original packet containing an ICMP message. Since
        // this test fake supports ICMP sockets, this error can be delivered all
        // the way up the stack.

        // A buffer containing an ICMP echo request with ID `ICMP_ID` and
        // sequence number `SEQ_NUM` from the local IP to the remote IP. Any
        // ICMP error message which contains this as its original packet should
        // be delivered to the socket created in
        // `test_receive_icmpv4_error_helper`.
        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                IcmpUnusedCode,
                IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
            ))
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                Ipv4Proto::Icmp,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        // Second, test with an original packet containing a malformed ICMP
        // packet (we accomplish this by leaving the IP packet's body empty). We
        // should process this packet in
        // `IcmpIpTransportContext::receive_icmp_error`, but we should go no
        // further - in particular, we should not call
        // `IcmpEchoBindingsContext::receive_icmp_error`.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                Ipv4Proto::Icmp,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        // Third, test with an original packet containing a UDP packet. This
        // allows us to verify that protocol numbers are handled properly by
        // checking that `IcmpIpTransportContext::receive_icmp_error` was NOT
        // called.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );
    }

    #[test]
    fn test_receive_icmpv6_error() {
        // Chosen arbitrarily to be a) non-zero (it's easy to accidentally get
        // the value 0) and, b) different from each other.
        const ICMP_ID: u16 = 0x0F;
        const SEQ_NUM: u16 = 0xF0;

        /// Test receiving an ICMPv6 error message.
        ///
        /// Test that receiving an ICMP error message with the given code and
        /// message contents, and containing the given original IPv4 packet,
        /// results in the counter values in `assert_counters`. After that
        /// assertion passes, `f` is called on the context so that the caller
        /// can perform whatever extra validation they want.
        ///
        /// The error message will be sent from `FAKE_CONFIG_V6.remote_ip` to
        /// `FAKE_CONFIG_V6.local_ip`. Before the message is sent, an ICMP
        /// socket will be established with the ID `ICMP_ID`, and
        /// `test_receive_icmpv6_error_helper` will assert that its `SocketId`
        /// is 0. This allows the caller to craft the `original_packet` so that
        /// it should be delivered to this socket.
        fn test_receive_icmpv6_error_helper<
            C: Debug,
            M: IcmpMessage<Ipv6, Code = C> + Debug,
            F: Fn(&FakeIcmpCtx<Ipv6>),
        >(
            original_packet: &mut [u8],
            code: C,
            msg: M,
            assert_counters: &[(&str, u64)],
            f: F,
        ) {
            crate::testutil::set_logger_for_test();

            let mut ctx = FakeIcmpCtx::<Ipv6>::default();
            let mut socket_api = IcmpEchoSocketApi::<Ipv6, _>::new(ctx.as_mut());
            let conn = socket_api.create();
            // NOTE: This assertion is not a correctness requirement. It's just
            // that the rest of this test assumes that the new connection has ID
            // 0. If this assertion fails in the future, that isn't necessarily
            // evidence of a bug; we may just have to update this test to
            // accommodate whatever new ID allocation scheme is being used.
            assert_eq!(conn, SocketId::new(0));
            socket_api.bind(&conn, None, NonZeroU16::new(ICMP_ID)).unwrap();
            socket_api
                .connect(
                    &conn,
                    Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(FAKE_CONFIG_V6.remote_ip))),
                    REMOTE_ID,
                )
                .unwrap();

            let FakeCtxWithCoreCtx { core_ctx, bindings_ctx } = &mut ctx;
            <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FAKE_CONFIG_V6.remote_ip.get().try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip,
                Buf::new(original_packet, ..)
                    .encapsulate(IcmpPacketBuilder::new(
                        FAKE_CONFIG_V6.remote_ip,
                        FAKE_CONFIG_V6.local_ip,
                        code,
                        msg,
                    ))
                    .serialize_vec_outer()
                    .unwrap(),
            )
            .unwrap();

            for (ctr, count) in assert_counters {
                match *ctr {
                    "InnerIcmpContext::receive_icmp_error" => assert_eq!(
                        core_ctx.inner.inner.state.icmp_rx_counters::<Ipv6>().error.get(),
                        *count,
                        "wrong count for counter {ctr}",
                    ),
                    "IcmpIpTransportContext::receive_icmp_error" => assert_eq!(
                        core_ctx
                            .inner
                            .inner
                            .state
                            .icmp_rx_counters::<Ipv6>()
                            .error_at_transport_layer
                            .get(),
                        *count,
                        "wrong count for counter {ctr}",
                    ),
                    "IcmpEchoBindingsContext::receive_icmp_error" => assert_eq!(
                        core_ctx.inner.inner.state.icmp_rx_counters::<Ipv6>().error_at_socket.get(),
                        *count,
                        "wrong count for counter {ctr}",
                    ),
                    c => assert!(false, "unrecognized counter: {c}"),
                }
            }
            f(&ctx);
        }
        // Test that, when we receive various ICMPv6 error messages, we properly
        // pass them up to the IP layer and, sometimes, to the transport layer.

        // First, test with an original packet containing an ICMPv6 message.
        // Since this test fake supports ICMPv6 sockets, this error can be
        // delivered all the way up the stack.

        // A buffer containing an ICMPv6 echo request with ID `ICMP_ID` and
        // sequence number `SEQ_NUM` from the local IP to the remote IP. Any
        // ICMPv6 error message which contains this as its original packet
        // should be delivered to the socket created in
        // `test_receive_icmpv6_error_helper`.
        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                IcmpUnusedCode,
                IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
            ))
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 1),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        // Second, test with an original packet containing a malformed ICMPv6
        // packet (we accomplish this by leaving the IP packet's body empty). We
        // should process this packet in
        // `IcmpIpTransportContext::receive_icmp_error`, but we should go no
        // further - in particular, we should not call
        // `IcmpEchoBindingsContext::receive_icmp_error`.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        // Third, test with an original packet containing a UDP packet. This
        // allows us to verify that protocol numbers are handled properly by
        // checking that `IcmpIpTransportContext::receive_icmp_error` was NOT
        // called.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpEchoBindingsContext::receive_icmp_error", 0),
            ],
            |FakeCtxWithCoreCtx { core_ctx, bindings_ctx: _ }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(core_ctx.inner.outer.receive_icmp_error, [err]);
            },
        );
    }

    #[test]
    fn test_error_rate_limit() {
        crate::testutil::set_logger_for_test();

        /// Call `send_icmpv4_ttl_expired` with fake values.
        fn send_icmpv4_ttl_expired_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv4>,
        ) {
            send_icmpv4_ttl_expired(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V4.local_ip.try_into().unwrap(),
                IpProto::Udp.into(),
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv4_parameter_problem` with fake values.
        fn send_icmpv4_parameter_problem_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv4>,
        ) {
            send_icmpv4_parameter_problem(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V4.local_ip.try_into().unwrap(),
                Icmpv4ParameterProblemCode::PointerIndicatesError,
                Icmpv4ParameterProblem::new(0),
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv4_dest_unreachable` with fake values.
        fn send_icmpv4_dest_unreachable_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv4>,
        ) {
            send_icmpv4_dest_unreachable(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V4.local_ip.try_into().unwrap(),
                Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv6_ttl_expired` with fake values.
        fn send_icmpv6_ttl_expired_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv6>,
        ) {
            send_icmpv6_ttl_expired(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V6.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip.try_into().unwrap(),
                IpProto::Udp.into(),
                Buf::new(&mut [], ..),
                0,
            );
        }

        /// Call `send_icmpv6_packet_too_big` with fake values.
        fn send_icmpv6_packet_too_big_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv6>,
        ) {
            send_icmpv6_packet_too_big(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V6.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip.try_into().unwrap(),
                IpProto::Udp.into(),
                Mtu::new(0),
                Buf::new(&mut [], ..),
                0,
            );
        }

        /// Call `send_icmpv6_parameter_problem` with fake values.
        fn send_icmpv6_parameter_problem_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv6>,
        ) {
            send_icmpv6_parameter_problem(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V6.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip.try_into().unwrap(),
                Icmpv6ParameterProblemCode::ErroneousHeaderField,
                Icmpv6ParameterProblem::new(0),
                Buf::new(&mut [], ..),
                false,
            );
        }

        /// Call `send_icmpv6_dest_unreachable` with fake values.
        fn send_icmpv6_dest_unreachable_helper(
            FakeCtxWithCoreCtx { core_ctx, bindings_ctx }: &mut FakeIcmpCtx<Ipv6>,
        ) {
            send_icmpv6_dest_unreachable(
                &mut core_ctx.inner,
                bindings_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V6.remote_ip.try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip.try_into().unwrap(),
                Icmpv6DestUnreachableCode::NoRoute,
                Buf::new(&mut [], ..),
            );
        }

        // Run tests for each function that sends error messages to make sure
        // they're all properly rate limited.

        fn run_test<
            I: datagram::IpExt,
            W: Fn(u64) -> FakeIcmpCtx<I>,
            S: Fn(&mut FakeIcmpCtx<I>),
        >(
            with_errors_per_second: W,
            send: S,
        ) {
            // Note that we could theoretically have more precise tests here
            // (e.g., a test that we send at the correct rate over the long
            // term), but those would amount to testing the `TokenBucket`
            // implementation, which has its own exhaustive tests. Instead, we
            // just have a few sanity checks to make sure that we're actually
            // invoking it when we expect to (as opposed to bypassing it
            // entirely or something).

            // Test that, if no time has elapsed, we can successfully send up to
            // `ERRORS_PER_SECOND` error messages, but no more.

            // Don't use `DEFAULT_ERRORS_PER_SECOND` because it's 2^16 and it
            // makes this test take a long time.
            const ERRORS_PER_SECOND: u64 = 64;

            let mut ctx = with_errors_per_second(ERRORS_PER_SECOND);

            for i in 0..ERRORS_PER_SECOND {
                send(&mut ctx);
                assert_eq!(
                    ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(),
                    i + 1
                );
            }

            assert_eq!(
                ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(),
                ERRORS_PER_SECOND
            );
            send(&mut ctx);
            assert_eq!(
                ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(),
                ERRORS_PER_SECOND
            );

            // Test that, if we set a rate of 0, we are not able to send any
            // error messages regardless of how much time has elapsed.

            let mut ctx = with_errors_per_second(0);
            send(&mut ctx);
            assert_eq!(ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(), 0);
            ctx.bindings_ctx.sleep_skip_timers(Duration::from_secs(1));
            send(&mut ctx);
            assert_eq!(ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(), 0);
            ctx.bindings_ctx.sleep_skip_timers(Duration::from_secs(1));
            send(&mut ctx);
            assert_eq!(ctx.core_ctx.inner.inner.state.icmp_tx_counters::<I>().error.get(), 0);
        }

        fn with_errors_per_second_v4(errors_per_second: u64) -> FakeIcmpCtx<Ipv4> {
            FakeCtxWithCoreCtx::with_core_ctx(Wrapped {
                outer: Default::default(),
                inner: Wrapped {
                    outer: FakeIcmpInnerCoreCtxState::with_errors_per_second(errors_per_second),
                    inner: Default::default(),
                },
            })
        }
        run_test::<Ipv4, _, _>(with_errors_per_second_v4, send_icmpv4_ttl_expired_helper);
        run_test::<Ipv4, _, _>(with_errors_per_second_v4, send_icmpv4_parameter_problem_helper);
        run_test::<Ipv4, _, _>(with_errors_per_second_v4, send_icmpv4_dest_unreachable_helper);

        fn with_errors_per_second_v6(errors_per_second: u64) -> FakeIcmpCtx<Ipv6> {
            FakeCtxWithCoreCtx::with_core_ctx(Wrapped {
                outer: Default::default(),
                inner: Wrapped {
                    outer: FakeIcmpInnerCoreCtxState::with_errors_per_second(errors_per_second),
                    inner: Default::default(),
                },
            })
        }

        run_test::<Ipv6, _, _>(with_errors_per_second_v6, send_icmpv6_ttl_expired_helper);
        run_test::<Ipv6, _, _>(with_errors_per_second_v6, send_icmpv6_packet_too_big_helper);
        run_test::<Ipv6, _, _>(with_errors_per_second_v6, send_icmpv6_parameter_problem_helper);
        run_test::<Ipv6, _, _>(with_errors_per_second_v6, send_icmpv6_dest_unreachable_helper);
    }
}
