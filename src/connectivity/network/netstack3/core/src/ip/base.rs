// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{borrow::Cow, vec::Vec};
use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU32, NonZeroU8},
    sync::atomic::{self, AtomicU16},
};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use explicit::ResultExt as _;
use lock_order::lock::UnlockedAccess;
use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    wrap::prelude::*,
};
#[cfg(test)]
use net_types::ip::IpVersion;
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, IpInvariant, IpMarked, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
        Ipv6SourceAddr, Mtu, Subnet,
    },
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{Buf, BufferMut, ParseMetadata, Serializer};
use packet_formats::{
    error::IpParseError,
    ip::{IpPacket, IpPacketBuilder as _, IpProto, Ipv4Proto, Ipv6Proto},
    ipv4::{Ipv4FragmentType, Ipv4Packet},
    ipv6::Ipv6Packet,
};
use thiserror::Error;
use tracing::{debug, trace};

use crate::{
    context::{
        CounterContext, EventContext, InstantContext, NonTestCtxMarker, TimerHandler,
        TracingContext,
    },
    counters::Counter,
    data_structures::token_bucket::TokenBucket,
    device::{
        AnyDevice, DeviceId, DeviceIdContext, DeviceLayerTypes, FrameDestination, Id, StrongId,
        WeakDeviceId,
    },
    ip::{
        device::{
            self, slaac::SlaacCounters, state::IpDeviceStateIpExt, IpDeviceBindingsContext,
            IpDeviceIpExt, IpDeviceSendContext,
        },
        forwarding::{ForwardingTable, IpForwardingDeviceContext},
        icmp,
        icmp::{
            IcmpErrorHandler, IcmpHandlerIpExt, IcmpIpExt, IcmpIpTransportContext, IcmpRxCounters,
            IcmpTxCounters, Icmpv4Error, Icmpv4ErrorCode, Icmpv4ErrorKind, Icmpv4State,
            Icmpv4StateBuilder, Icmpv6ErrorCode, Icmpv6ErrorKind, Icmpv6State, Icmpv6StateBuilder,
            InnerIcmpContext, NdpCounters,
        },
        ipv6,
        ipv6::Ipv6PacketAction,
        path_mtu::{PmtuCache, PmtuTimerId},
        reassembly::{
            FragmentCacheKey, FragmentHandler, FragmentProcessingState, IpPacketFragmentCache,
        },
        socket::{IpSocketBindingsContext, IpSocketContext, IpSocketHandler},
        types,
        types::{Destination, NextHop, ResolvedRoute},
    },
    sync::{LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
    transport::{tcp::socket::TcpIpTransportContext, udp::UdpIpTransportContext},
    uninstantiable::UninstantiableWrapper,
    BindingsContext, BindingsTypes, CoreCtx, Instant, StackState, SyncCtx,
};

/// Default IPv4 TTL.
pub(crate) const DEFAULT_TTL: NonZeroU8 = const_unwrap_option(NonZeroU8::new(64));

/// Hop limits for packets sent to multicast and unicast destinations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HopLimits {
    pub(crate) unicast: NonZeroU8,
    pub(crate) multicast: NonZeroU8,
}

/// Default hop limits for sockets.
pub(crate) const DEFAULT_HOP_LIMITS: HopLimits =
    HopLimits { unicast: DEFAULT_TTL, multicast: const_unwrap_option(NonZeroU8::new(1)) };

/// The IPv6 subnet that contains all addresses; `::/0`.
// Safe because 0 is less than the number of IPv6 address bits.
pub(crate) const IPV6_DEFAULT_SUBNET: Subnet<Ipv6Addr> =
    unsafe { Subnet::new_unchecked(Ipv6::UNSPECIFIED_ADDRESS, 0) };

/// An error encountered when receiving a transport-layer packet.
#[derive(Debug)]
pub(crate) struct TransportReceiveError {
    inner: TransportReceiveErrorInner,
}

impl TransportReceiveError {
    // NOTE: We don't expose a constructor for the "protocol unsupported" case.
    // This ensures that the only way that we send a "protocol unsupported"
    // error is if the implementation of `IpTransportContext` provided for a
    // given protocol number is `()`. That's because `()` is the only type whose
    // `receive_ip_packet` function is implemented in this module, and thus it's
    // the only type that is able to construct a "protocol unsupported"
    // `TransportReceiveError`.

    /// Constructs a new `TransportReceiveError` to indicate an unreachable
    /// port.
    pub(crate) fn new_port_unreachable() -> TransportReceiveError {
        TransportReceiveError { inner: TransportReceiveErrorInner::PortUnreachable }
    }
}

#[derive(Debug)]
enum TransportReceiveErrorInner {
    ProtocolUnsupported,
    PortUnreachable,
}

/// An [`Ip`] extension trait adding functionality specific to the IP layer.
pub trait IpExt: packet_formats::ip::IpExt + IcmpIpExt {
    /// The type used to specify an IP packet's source address in a call to
    /// [`IpTransportContext::receive_ip_packet`].
    ///
    /// For IPv4, this is `Ipv4Addr`. For IPv6, this is [`Ipv6SourceAddr`].
    type RecvSrcAddr: Into<Self::Addr>;
    /// The length of an IP header without any IP options.
    const IP_HEADER_LENGTH: NonZeroU32;
    /// The maximum payload size an IP payload can have.
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32;
}

impl IpExt for Ipv4 {
    type RecvSrcAddr = Ipv4Addr;
    const IP_HEADER_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(packet_formats::ipv4::HDR_PREFIX_LEN as u32));
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(u16::MAX as u32 - Self::IP_HEADER_LENGTH.get()));
}

impl IpExt for Ipv6 {
    type RecvSrcAddr = Ipv6SourceAddr;
    const IP_HEADER_LENGTH: NonZeroU32 =
        const_unwrap_option(NonZeroU32::new(packet_formats::ipv6::IPV6_FIXED_HDR_LEN as u32));
    const IP_MAX_PAYLOAD_LENGTH: NonZeroU32 = const_unwrap_option(NonZeroU32::new(u16::MAX as u32));
}

/// The execution context provided by a transport layer protocol to the IP
/// layer.
///
/// An implementation for `()` is provided which indicates that a particular
/// transport layer protocol is unsupported.
pub(crate) trait IpTransportContext<I: IpExt, BC, CC: DeviceIdContext<AnyDevice> + ?Sized> {
    /// Receive an ICMP error message.
    ///
    /// All arguments beginning with `original_` are fields from the IP packet
    /// that triggered the error. The `original_body` is provided here so that
    /// the error can be associated with a transport-layer socket. `device`
    /// identifies the device that received the ICMP error message packet.
    ///
    /// While ICMPv4 error messages are supposed to contain the first 8 bytes of
    /// the body of the offending packet, and ICMPv6 error messages are supposed
    /// to contain as much of the offending packet as possible without violating
    /// the IPv6 minimum MTU, the caller does NOT guarantee that either of these
    /// hold. It is `receive_icmp_error`'s responsibility to handle any length
    /// of `original_body`, and to perform any necessary validation.
    fn receive_icmp_error(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        original_body: &[u8],
        err: I::ErrorCode,
    );

    /// Receive a transport layer packet in an IP packet.
    ///
    /// In the event of an unreachable port, `receive_ip_packet` returns the
    /// buffer in its original state (with the transport packet un-parsed) in
    /// the `Err` variant.
    fn receive_ip_packet<B: BufferMut>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        buffer: B,
    ) -> Result<(), (B, TransportReceiveError)>;
}

impl<I: IpExt, BC, CC: DeviceIdContext<AnyDevice> + ?Sized> IpTransportContext<I, BC, CC> for () {
    fn receive_icmp_error(
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        _original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        _original_dst_ip: SpecifiedAddr<I::Addr>,
        _original_body: &[u8],
        err: I::ErrorCode,
    ) {
        trace!("IpTransportContext::receive_icmp_error: Received ICMP error message ({:?}) for unsupported IP protocol", err);
    }

    fn receive_ip_packet<B: BufferMut>(
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _device: &CC::DeviceId,
        _src_ip: I::RecvSrcAddr,
        _dst_ip: SpecifiedAddr<I::Addr>,
        buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        Err((
            buffer,
            TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
        ))
    }
}

/// The execution context provided by the IP layer to transport layer protocols.
pub trait TransportIpContext<I: IpExt, BC>:
    DeviceIdContext<AnyDevice> + IpSocketHandler<I, BC>
{
    type DevicesWithAddrIter<'s>: Iterator<Item = Self::DeviceId>
    where
        Self: 's;

    /// Is this one of our local addresses, and is it in the assigned state?
    ///
    /// Returns an iterator over all the local interfaces for which `addr` is an
    /// associated address, and, for IPv6, for which it is in the "assigned"
    /// state.
    fn get_devices_with_assigned_addr(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Self::DevicesWithAddrIter<'_>;

    /// Get default hop limits.
    ///
    /// If `device` is not `None` and exists, its hop limits will be returned.
    /// Otherwise the system defaults are returned.
    fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits;

    /// Confirms the provided destination is reachable.
    ///
    /// Implementations must retrieve the next hop given the provided
    /// destination and confirm neighbor reachability for the resolved target
    /// device.
    fn confirm_reachable_with_destination(
        &mut self,
        bindings_ctx: &mut BC,
        dst: SpecifiedAddr<I::Addr>,
        device: Option<&Self::DeviceId>,
    );
}

/// Abstraction over the ability to join and leave multicast groups.
pub(crate) trait MulticastMembershipHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    /// Requests that the specified device join the given multicast group.
    ///
    /// If this method is called multiple times with the same device and
    /// address, the device will remain joined to the multicast group until
    /// [`MulticastTransportIpContext::leave_multicast_group`] has been called
    /// the same number of times.
    fn join_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    );

    /// Requests that the specified device leave the given multicast group.
    ///
    /// Each call to this method must correspond to an earlier call to
    /// [`MulticastTransportIpContext::join_multicast_group`]. The device
    /// remains a member of the multicast group so long as some call to
    /// `join_multicast_group` has been made without a corresponding call to
    /// `leave_multicast_group`.
    fn leave_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        addr: MulticastAddr<I::Addr>,
    );
}

// TODO(joshlf): With all 256 protocol numbers (minus reserved ones) given their
// own associated type in both traits, running `cargo check` on a 2018 MacBook
// Pro takes over a minute. Eventually - and before we formally publish this as
// a library - we should identify the bottleneck in the compiler and optimize
// it. For the time being, however, we only support protocol numbers that we
// actually use (TCP and UDP).

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: IpExt,
        BC,
        CC: IpDeviceContext<I, BC> + IpSocketHandler<I, BC> + IpStateContext<I, BC> + NonTestCtxMarker,
    > TransportIpContext<I, BC> for CC
{
    type DevicesWithAddrIter<'s> = <Vec<CC::DeviceId> as IntoIterator>::IntoIter where CC: 's;

    fn get_devices_with_assigned_addr(
        &mut self,
        addr: SpecifiedAddr<<I as Ip>::Addr>,
    ) -> Self::DevicesWithAddrIter<'_> {
        self.with_address_statuses(addr, |it| {
            it.filter_map(|(device, state)| is_unicast_assigned::<I>(&state).then_some(device))
                .collect::<Vec<_>>()
        })
        .into_iter()
    }

    fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits {
        match device {
            Some(device) => HopLimits {
                unicast: IpDeviceStateContext::<I, _>::get_hop_limit(self, device),
                ..DEFAULT_HOP_LIMITS
            },
            None => DEFAULT_HOP_LIMITS,
        }
    }

    fn confirm_reachable_with_destination(
        &mut self,
        bindings_ctx: &mut BC,
        dst: SpecifiedAddr<<I as Ip>::Addr>,
        device: Option<&Self::DeviceId>,
    ) {
        match self
            .with_ip_routing_table(|core_ctx, routes| routes.lookup(core_ctx, device, dst.get()))
        {
            Some(Destination { next_hop, device }) => {
                let neighbor = match next_hop {
                    NextHop::RemoteAsNeighbor => dst,
                    NextHop::Gateway(gateway) => gateway,
                };
                self.confirm_reachable(bindings_ctx, &device, neighbor);
            }
            None => {
                tracing::debug!("can't confirm {dst:?}@{device:?} as reachable: no route");
            }
        }
    }
}

#[derive(Copy, Clone)]
pub enum EitherDeviceId<S, W> {
    Strong(S),
    Weak(W),
}

impl<S: PartialEq, W: PartialEq + PartialEq<S>> PartialEq for EitherDeviceId<S, W> {
    fn eq(&self, other: &EitherDeviceId<S, W>) -> bool {
        match (self, other) {
            (EitherDeviceId::Strong(this), EitherDeviceId::Strong(other)) => this == other,
            (EitherDeviceId::Strong(this), EitherDeviceId::Weak(other)) => other == this,
            (EitherDeviceId::Weak(this), EitherDeviceId::Strong(other)) => this == other,
            (EitherDeviceId::Weak(this), EitherDeviceId::Weak(other)) => this == other,
        }
    }
}

impl<S: Id, W: Id> EitherDeviceId<&'_ S, &'_ W> {
    pub(crate) fn as_strong_ref<
        'a,
        CC: DeviceIdContext<AnyDevice, DeviceId = S, WeakDeviceId = W>,
    >(
        &'a self,
        core_ctx: &CC,
    ) -> Option<Cow<'a, CC::DeviceId>> {
        match self {
            EitherDeviceId::Strong(s) => Some(Cow::Borrowed(s)),
            EitherDeviceId::Weak(w) => core_ctx.upgrade_weak_device_id(w).map(Cow::Owned),
        }
    }
}

impl<S, W> EitherDeviceId<S, W> {
    pub(crate) fn as_ref<'a, S2, W2>(&'a self) -> EitherDeviceId<&'a S2, &'a W2>
    where
        S: Borrow<S2>,
        W: Borrow<W2>,
    {
        match self {
            EitherDeviceId::Strong(s) => EitherDeviceId::Strong(s.borrow()),
            EitherDeviceId::Weak(w) => EitherDeviceId::Weak(w.borrow()),
        }
    }
}

impl<S: Id, W: Id> EitherDeviceId<S, W> {
    pub(crate) fn as_strong<'a, CC: DeviceIdContext<AnyDevice, DeviceId = S, WeakDeviceId = W>>(
        &'a self,
        core_ctx: &CC,
    ) -> Option<Cow<'a, CC::DeviceId>> {
        match self {
            EitherDeviceId::Strong(s) => Some(Cow::Borrowed(s)),
            EitherDeviceId::Weak(w) => core_ctx.upgrade_weak_device_id(w).map(Cow::Owned),
        }
    }

    pub(crate) fn as_weak<'a, CC: DeviceIdContext<AnyDevice, DeviceId = S, WeakDeviceId = W>>(
        &'a self,
        core_ctx: &CC,
    ) -> Cow<'a, CC::WeakDeviceId> {
        match self {
            EitherDeviceId::Strong(s) => Cow::Owned(core_ctx.downgrade_device_id(s)),
            EitherDeviceId::Weak(w) => Cow::Borrowed(w),
        }
    }
}

/// The status of an IP address on an interface.
pub(crate) enum AddressStatus<S> {
    Present(S),
    Unassigned,
}

impl<S> AddressStatus<S> {
    fn into_present(self) -> Option<S> {
        match self {
            Self::Present(s) => Some(s),
            Self::Unassigned => None,
        }
    }
}

impl<S: GenericOverIp<I>, I: Ip> GenericOverIp<I> for AddressStatus<S> {
    type Type = AddressStatus<S::Type>;
}

/// The status of an IPv4 address.
pub enum Ipv4PresentAddressStatus {
    LimitedBroadcast,
    SubnetBroadcast,
    Multicast,
    Unicast,
}

/// The status of an IPv6 address.
pub enum Ipv6PresentAddressStatus {
    Multicast,
    UnicastAssigned,
    UnicastTentative,
}

/// An extension trait providing IP layer properties.
pub trait IpLayerIpExt: IpExt {
    type AddressStatus;
    type State<I: Instant, StrongDeviceId: StrongId>: AsRef<IpStateInner<Self, I, StrongDeviceId>>;
    type PacketIdState;
    type PacketId;
    fn next_packet_id_from_state(state: &Self::PacketIdState) -> Self::PacketId;
}

impl IpLayerIpExt for Ipv4 {
    type AddressStatus = Ipv4PresentAddressStatus;
    type State<I: Instant, StrongDeviceId: StrongId> = Ipv4State<I, StrongDeviceId>;
    type PacketIdState = AtomicU16;
    type PacketId = u16;
    fn next_packet_id_from_state(next_packet_id: &Self::PacketIdState) -> Self::PacketId {
        // Relaxed ordering as we only need atomicity without synchronization. See
        // https://en.cppreference.com/w/cpp/atomic/memory_order#Relaxed_ordering
        // for more details.
        //
        // TODO(https://fxbug.dev/87588): Generate IPv4 IDs unpredictably
        next_packet_id.fetch_add(1, atomic::Ordering::Relaxed)
    }
}

impl IpLayerIpExt for Ipv6 {
    type AddressStatus = Ipv6PresentAddressStatus;
    type State<I: Instant, StrongDeviceId: StrongId> = Ipv6State<I, StrongDeviceId>;
    type PacketIdState = ();
    type PacketId = ();
    fn next_packet_id_from_state((): &Self::PacketIdState) -> Self::PacketId {
        ()
    }
}

/// The state context provided to the IP layer.
pub(crate) trait IpStateContext<I: IpLayerIpExt, BC>: DeviceIdContext<AnyDevice> {
    type IpDeviceIdCtx<'a>: DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + IpForwardingDeviceContext<I>
        + IpDeviceStateContext<I, BC>;

    /// Calls the function with an immutable reference to IP routing table.
    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to IP routing table.
    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provices access to an IP device's state for the IP layer.
pub(crate) trait IpDeviceStateContext<I: IpLayerIpExt, BC>:
    DeviceIdContext<AnyDevice>
{
    /// Calls the callback with the next packet ID.
    fn with_next_packet_id<O, F: FnOnce(&I::PacketIdState) -> O>(&self, cb: F) -> O;

    /// Returns the best local address for communicating with the remote.
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<I::Addr>>,
    ) -> Option<SpecifiedAddr<I::Addr>>;

    /// Returns the hop limit.
    fn get_hop_limit(&mut self, device_id: &Self::DeviceId) -> NonZeroU8;

    /// Gets the status of an address.
    ///
    /// Only the specified device will be checked for the address. Returns
    /// [`AddressStatus::Unassigned`] if the address is not assigned to the
    /// device.
    fn address_status_for_device(
        &mut self,
        addr: SpecifiedAddr<I::Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<I::AddressStatus>;
}

/// The IP device context provided to the IP layer.
pub(crate) trait IpDeviceContext<I: IpLayerIpExt, BC>: IpDeviceStateContext<I, BC> {
    /// Is the device enabled?
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool;

    type DeviceAndAddressStatusIter<'a, 's>: Iterator<Item = (Self::DeviceId, I::AddressStatus)>
    where
        Self: IpDeviceContext<I, BC> + 's;

    /// Provides access to the status of an address.
    ///
    /// Calls the provided callback with an iterator over the devices for which
    /// the address is assigned and the status of the assignment for each
    /// device.
    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<I::Addr>,
        cb: F,
    ) -> R;

    /// Returns true iff the device has forwarding enabled.
    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool;

    /// Returns the MTU of the device.
    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu;

    /// Confirm transport-layer forward reachability to the specified neighbor
    /// through the specified device.
    fn confirm_reachable(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
    );
}

/// Events observed at the IP layer.
#[derive(Debug, Eq, Hash, PartialEq)]
pub enum IpLayerEvent<DeviceId, I: Ip> {
    /// A route needs to be added.
    AddRoute(types::AddableEntry<I::Addr, DeviceId>),
    /// Routes matching these specifiers need to be removed.
    RemoveRoutes {
        /// Destination subnet
        subnet: Subnet<I::Addr>,
        /// Outgoing interface
        device: DeviceId,
        /// Gateway/next-hop
        gateway: Option<SpecifiedAddr<I::Addr>>,
    },
}

/// The bindings execution context for the IP layer.
pub(crate) trait IpLayerBindingsContext<I: Ip, DeviceId>:
    InstantContext + EventContext<IpLayerEvent<DeviceId, I>> + TracingContext
{
}
impl<
        I: Ip,
        DeviceId,
        BC: InstantContext + EventContext<IpLayerEvent<DeviceId, I>> + TracingContext,
    > IpLayerBindingsContext<I, DeviceId> for BC
{
}

/// The execution context for the IP layer.
pub(crate) trait IpLayerContext<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>,
>: IpStateContext<I, BC> + IpDeviceContext<I, BC>
{
}

impl<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: IpStateContext<I, BC> + IpDeviceContext<I, BC>,
    > IpLayerContext<I, BC> for CC
{
}

fn is_unicast_assigned<I: IpLayerIpExt>(status: &I::AddressStatus) -> bool {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct WrapAddressStatus<'a, I: IpLayerIpExt>(&'a I::AddressStatus);

    I::map_ip(
        WrapAddressStatus(status),
        |WrapAddressStatus(status)| match status {
            Ipv4PresentAddressStatus::Unicast => true,
            Ipv4PresentAddressStatus::LimitedBroadcast
            | Ipv4PresentAddressStatus::SubnetBroadcast
            | Ipv4PresentAddressStatus::Multicast => false,
        },
        |WrapAddressStatus(status)| match status {
            Ipv6PresentAddressStatus::UnicastAssigned => true,
            Ipv6PresentAddressStatus::Multicast | Ipv6PresentAddressStatus::UnicastTentative => {
                false
            }
        },
    )
}

fn is_local_assigned_address<
    I: Ip + IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>,
>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
    local_ip: SpecifiedAddr<I::Addr>,
) -> bool {
    match core_ctx.address_status_for_device(local_ip, device) {
        AddressStatus::Present(status) => is_unicast_assigned::<I>(&status),
        AddressStatus::Unassigned => false,
    }
}

// Returns the local IP address to use for sending packets from the
// given device to `addr`, restricting to `local_ip` if it is not
// `None`.
fn get_local_addr<
    I: Ip + IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>,
>(
    core_ctx: &mut CC,
    local_ip: Option<SpecifiedAddr<I::Addr>>,
    device: &CC::DeviceId,
    remote_addr: Option<SpecifiedAddr<I::Addr>>,
) -> Result<SpecifiedAddr<I::Addr>, ResolveRouteError> {
    if let Some(local_ip) = local_ip {
        is_local_assigned_address(core_ctx, device, local_ip)
            .then_some(local_ip)
            .ok_or(ResolveRouteError::NoSrcAddr)
    } else {
        core_ctx.get_local_addr_for_remote(device, remote_addr).ok_or(ResolveRouteError::NoSrcAddr)
    }
}

/// An error occurred while resolving the route to a destination
#[derive(Error, Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub enum ResolveRouteError {
    /// A source address could not be selected.
    #[error("a source address could not be selected")]
    NoSrcAddr,
    /// The destination in unreachable.
    #[error("no route exists to the destination IP address")]
    Unreachable,
}

// Returns the forwarding instructions for reaching the given destination.
//
// If a `device` is specified, the resolved route is limited to those that
// egress over the device.
//
// If `local_ip` is specified the resolved route is limited to those that egress
// over a device with the address assigned.
fn resolve_route_to_destination<
    I: Ip + IpDeviceStateIpExt + IpDeviceIpExt + IpLayerIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId> + IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC>
        + device::IpDeviceConfigurationContext<I, BC>
        + IpDeviceStateContext<I, BC>
        + NonTestCtxMarker,
>(
    core_ctx: &mut CC,
    device: Option<&CC::DeviceId>,
    local_ip: Option<SpecifiedAddr<I::Addr>>,
    addr: Option<SpecifiedAddr<I::Addr>>,
) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
    enum LocalDelivery<A, D> {
        WeakLoopback(A),
        StrongForDevice(D),
    }

    // Check if locally destined. If the destination is an address assigned
    // on an interface, and an egress interface wasn't specifically
    // selected, route via the loopback device. This lets us operate as a
    // strong host when an outgoing interface is explicitly requested while
    // still enabling local delivery via the loopback interface, which is
    // acting as a weak host. Note that if the loopback interface is
    // requested as an outgoing interface, route selection is still
    // performed as a strong host! This makes the loopback interface behave
    // more like the other interfaces on the system.
    //
    // TODO(https://fxbug.dev/93870): Encode the delivery of locally-
    // destined packets to loopback in the route table.
    let local_delivery_instructions: Option<LocalDelivery<SpecifiedAddr<I::Addr>, &CC::DeviceId>> =
        addr.and_then(|addr| {
            match device {
                Some(device) => match core_ctx.address_status_for_device(addr, device) {
                    AddressStatus::Present(status) => {
                        // If the destination is an address assigned to the
                        // requested egress interface, route locally via the strong
                        // host model.
                        is_unicast_assigned::<I>(&status)
                            .then_some(LocalDelivery::StrongForDevice(device))
                    }
                    AddressStatus::Unassigned => None,
                },
                None => {
                    // If the destination is an address assigned on an interface,
                    // and an egress interface wasn't specifically selected, route
                    // via the loopback device operating in a weak host model.
                    core_ctx
                        .with_address_statuses(addr, |mut it| {
                            it.any(|(_device, status)| is_unicast_assigned::<I>(&status))
                        })
                        .then_some(LocalDelivery::WeakLoopback(addr))
                }
            }
        });

    match local_delivery_instructions {
        Some(local_delivery) => {
            let loopback = match core_ctx.loopback_id() {
                None => return Err(ResolveRouteError::Unreachable),
                Some(loopback) => loopback,
            };

            let local_ip = match local_delivery {
                LocalDelivery::WeakLoopback(addr) => match local_ip {
                    Some(local_ip) => core_ctx
                        .with_address_statuses(local_ip, |mut it| {
                            it.any(|(_device, status)| is_unicast_assigned::<I>(&status))
                        })
                        .then_some(local_ip)
                        .ok_or(ResolveRouteError::NoSrcAddr)?,
                    None => addr,
                },
                LocalDelivery::StrongForDevice(device) => {
                    get_local_addr(core_ctx, local_ip, device, addr)?
                }
            };
            Ok(ResolvedRoute {
                src_addr: local_ip,
                device: loopback,
                next_hop: NextHop::RemoteAsNeighbor,
            })
        }
        None => {
            core_ctx
                .with_ip_routing_table(|core_ctx, table| {
                    let mut matching_with_addr = table.lookup_filter_map(
                        core_ctx,
                        device,
                        addr.map_or(I::UNSPECIFIED_ADDRESS, |a| *a),
                        |core_ctx, d| Some(get_local_addr(core_ctx, local_ip, d, addr)),
                    );

                    let first_error = match matching_with_addr.next() {
                        Some((Destination { device, next_hop }, Ok(addr))) => {
                            return Ok((Destination { device: device.clone(), next_hop }, addr))
                        }
                        Some((_, Err(e))) => e,
                        None => return Err(ResolveRouteError::Unreachable),
                    };

                    matching_with_addr
                        .filter_map(|(d, r)| {
                            // Select successful routes. We ignore later errors
                            // since we've already saved the first one.
                            r.ok_checked::<ResolveRouteError>().map(|a| (d, a))
                        })
                        .next()
                        .map_or(Err(first_error), |(Destination { device, next_hop }, addr)| {
                            Ok((Destination { device: device.clone(), next_hop }, addr))
                        })
                })
                .map(|(Destination { device, next_hop }, local_ip)| ResolvedRoute {
                    src_addr: local_ip,
                    device,
                    next_hop,
                })
        }
    }
}

impl<
        I: Ip + IpDeviceStateIpExt + IpDeviceIpExt + IpLayerIpExt,
        BC: IpDeviceBindingsContext<I, CC::DeviceId>
            + IpLayerBindingsContext<I, CC::DeviceId>
            + IpSocketBindingsContext,
        CC: IpLayerContext<I, BC>
            + device::IpDeviceConfigurationContext<I, BC>
            + IpDeviceStateContext<I, BC>
            + NonTestCtxMarker
            + IpDeviceSendContext<I, BC>,
    > IpSocketContext<I, BC> for CC
{
    fn lookup_route(
        &mut self,
        _bindings_ctx: &mut BC,
        device: Option<&CC::DeviceId>,
        local_ip: Option<SpecifiedAddr<I::Addr>>,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Result<ResolvedRoute<I, CC::DeviceId>, ResolveRouteError> {
        resolve_route_to_destination(self, device, local_ip, Some(addr))
    }

    fn send_ip_packet<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<
            I,
            &<CC as DeviceIdContext<AnyDevice>>::DeviceId,
            SpecifiedAddr<I::Addr>,
        >,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta.into(), body)
    }
}

impl<BC: BindingsContext, I: Ip, L> CounterContext<IpCounters<I>> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&IpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::IpStateCounters<I>>())
    }
}

impl<BC: BindingsContext, L> CounterContext<Ipv4Counters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&Ipv4Counters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::Ipv4StateCounters>())
    }
}

impl<BC: BindingsContext, L> CounterContext<Ipv6Counters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&Ipv6Counters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::Ipv6StateCounters>())
    }
}

impl<I, BC, L> IpStateContext<I, BC> for CoreCtx<'_, BC, L>
where
    I: IpLayerIpExt,
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::IpStateRoutingTable<I>>,

    // These bounds ensure that we can fulfill all the traits for the associated
    // type `IpDeviceIdCtx` below and keep the compiler happy where we don't
    // have implementations that are generic on Ip.
    for<'a> CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>:
        DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
            + IpForwardingDeviceContext<I>
            + IpDeviceStateContext<I, BC>,
{
    type IpDeviceIdCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IpStateRoutingTable<I>>;

    fn with_ip_routing_table<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (cache, mut locked) =
            self.read_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &cache)
    }

    fn with_ip_routing_table_mut<
        O,
        F: FnOnce(&mut Self::IpDeviceIdCtx<'_>, &mut ForwardingTable<I, Self::DeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut cache, mut locked) =
            self.write_lock_and::<crate::lock_ordering::IpStateRoutingTable<I>>();
        cb(&mut locked, &mut cache)
    }
}

/// The IP context providing dispatch to the available transport protocols.
///
/// This trait acts like a demux on the transport protocol for ingress IP
/// packets.
pub(crate) trait IpTransportDispatchContext<I: IpLayerIpExt, BC>:
    DeviceIdContext<AnyDevice>
{
    /// Dispatches a received incoming IP packet to the appropriate protocol.
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        proto: I::Proto,
        body: B,
    ) -> Result<(), (B, TransportReceiveError)>;
}

/// A marker trait for all the contexts required for IP ingress.
///
/// This is a shorthand for all the traits required to operate on an ingress IP
/// frame.
pub(crate) trait IpLayerIngressContext<
    I: IpLayerIpExt + IcmpHandlerIpExt,
    BC: IpLayerBindingsContext<I, Self::DeviceId>,
>:
    IpTransportDispatchContext<I, BC>
    + IpDeviceStateContext<I, BC>
    + IpDeviceSendContext<I, BC>
    + IcmpErrorHandler<I, BC>
    + IpLayerContext<I, BC>
    + FragmentHandler<I, BC>
{
}

impl<
        I: IpLayerIpExt + IcmpHandlerIpExt,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
        CC: IpTransportDispatchContext<I, BC>
            + IpDeviceStateContext<I, BC>
            + IpDeviceSendContext<I, BC>
            + IcmpErrorHandler<I, BC>
            + IpLayerContext<I, BC>
            + FragmentHandler<I, BC>,
    > IpLayerIngressContext<I, BC> for CC
{
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpSocketsTable<Ipv4>>>
    IpTransportDispatchContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        proto: Ipv4Proto,
        body: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        // TODO(https://fxbug.dev/93955): Deliver the packet to interested raw
        // sockets.

        match proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                )
            }
            Ipv4Proto::Igmp => {
                device::receive_igmp_packet(self, bindings_ctx, device, src_ip, dst_ip, body);
                Ok(())
            }
            Ipv4Proto::Proto(IpProto::Udp) => <UdpIpTransportContext as IpTransportContext<
                Ipv4,
                _,
                _,
            >>::receive_ip_packet(
                self, bindings_ctx, device, src_ip, dst_ip, body
            ),
            Ipv4Proto::Proto(IpProto::Tcp) => <TcpIpTransportContext as IpTransportContext<
                Ipv4,
                _,
                _,
            >>::receive_ip_packet(
                self, bindings_ctx, device, src_ip, dst_ip, body
            ),
            // TODO(joshlf): Once all IP protocol numbers are covered, remove
            // this default case.
            _ => Err((
                body,
                TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
            )),
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IcmpSocketsTable<Ipv6>>>
    IpTransportDispatchContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn dispatch_receive_ip_packet<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        proto: Ipv6Proto,
        body: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        // TODO(https://fxbug.dev/93955): Deliver the packet to interested raw
        // sockets.

        match proto {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_ip_packet(
                    self,
                    bindings_ctx,
                    device,
                    src_ip,
                    dst_ip,
                    body,
                )
            }
            // A value of `Ipv6Proto::NoNextHeader` tells us that there is no
            // header whatsoever following the last lower-level header so we stop
            // processing here.
            Ipv6Proto::NoNextHeader => Ok(()),
            Ipv6Proto::Proto(IpProto::Tcp) => <TcpIpTransportContext as IpTransportContext<
                Ipv6,
                _,
                _,
            >>::receive_ip_packet(
                self, bindings_ctx, device, src_ip, dst_ip, body
            ),
            Ipv6Proto::Proto(IpProto::Udp) => <UdpIpTransportContext as IpTransportContext<
                Ipv6,
                _,
                _,
            >>::receive_ip_packet(
                self, bindings_ctx, device, src_ip, dst_ip, body
            ),
            // TODO(joshlf): Once all IP Next Header numbers are covered, remove
            // this default case.
            _ => Err((
                body,
                TransportReceiveError { inner: TransportReceiveErrorInner::ProtocolUnsupported },
            )),
        }
    }
}

/// A builder for IPv4 state.
#[derive(Copy, Clone, Default)]
pub(crate) struct Ipv4StateBuilder {
    icmp: Icmpv4StateBuilder,
}

impl Ipv4StateBuilder {
    /// Get the builder for the ICMPv4 state.
    #[cfg(test)]
    pub(crate) fn icmpv4_builder(&mut self) -> &mut Icmpv4StateBuilder {
        &mut self.icmp
    }

    pub(crate) fn build<Instant: crate::Instant, StrongDeviceId: StrongId>(
        self,
    ) -> Ipv4State<Instant, StrongDeviceId> {
        let Ipv4StateBuilder { icmp } = self;

        Ipv4State {
            inner: Default::default(),
            icmp: icmp.build(),
            next_packet_id: Default::default(),
            counters: Default::default(),
        }
    }
}

/// A builder for IPv6 state.
#[derive(Copy, Clone, Default)]
pub(crate) struct Ipv6StateBuilder {
    icmp: Icmpv6StateBuilder,
}

impl Ipv6StateBuilder {
    pub(crate) fn build<Instant: crate::Instant, StrongDeviceId: StrongId>(
        self,
    ) -> Ipv6State<Instant, StrongDeviceId> {
        let Ipv6StateBuilder { icmp } = self;

        Ipv6State {
            inner: Default::default(),
            icmp: icmp.build(),
            counters: Default::default(),
            slaac_counters: Default::default(),
        }
    }
}

pub struct Ipv4State<Instant: crate::Instant, StrongDeviceId: StrongId> {
    pub(super) inner: IpStateInner<Ipv4, Instant, StrongDeviceId>,
    pub(super) icmp: Icmpv4State<Instant, StrongDeviceId::Weak>,
    pub(super) next_packet_id: AtomicU16,
    pub(super) counters: Ipv4Counters,
}

impl<Instant: crate::Instant, StrongDeviceId: StrongId> Ipv4State<Instant, StrongDeviceId> {
    pub(crate) fn counters(&self) -> &Ipv4Counters {
        &self.counters
    }

    pub(crate) fn icmp_tx_counters(&self) -> &IcmpTxCounters<Ipv4> {
        &self.icmp.inner.tx_counters
    }

    pub(crate) fn icmp_rx_counters(&self) -> &IcmpRxCounters<Ipv4> {
        &self.icmp.inner.rx_counters
    }
}

impl<I: Instant, StrongDeviceId: StrongId> AsRef<IpStateInner<Ipv4, I, StrongDeviceId>>
    for Ipv4State<I, StrongDeviceId>
{
    fn as_ref(&self) -> &IpStateInner<Ipv4, I, StrongDeviceId> {
        &self.inner
    }
}

pub(super) fn gen_ip_packet_id<I: IpLayerIpExt, BC, CC: IpDeviceStateContext<I, BC>>(
    core_ctx: &mut CC,
) -> I::PacketId {
    core_ctx.with_next_packet_id(|state| I::next_packet_id_from_state(state))
}

pub struct Ipv6State<Instant: crate::Instant, StrongDeviceId: StrongId> {
    pub(super) inner: IpStateInner<Ipv6, Instant, StrongDeviceId>,
    pub(super) icmp: Icmpv6State<Instant, StrongDeviceId::Weak>,
    pub(super) counters: Ipv6Counters,
    pub(super) slaac_counters: SlaacCounters,
}

impl<Instant: crate::Instant, StrongDeviceId: StrongId> Ipv6State<Instant, StrongDeviceId> {
    pub(crate) fn counters(&self) -> &Ipv6Counters {
        &self.counters
    }

    pub(crate) fn icmp_tx_counters(&self) -> &IcmpTxCounters<Ipv6> {
        &self.icmp.inner.tx_counters
    }

    pub(crate) fn icmp_rx_counters(&self) -> &IcmpRxCounters<Ipv6> {
        &self.icmp.inner.rx_counters
    }

    pub(crate) fn ndp_counters(&self) -> &NdpCounters {
        &self.icmp.ndp_counters
    }

    pub(crate) fn slaac_counters(&self) -> &SlaacCounters {
        &self.slaac_counters
    }
}

impl<I: Instant, StrongDeviceId: StrongId> AsRef<IpStateInner<Ipv6, I, StrongDeviceId>>
    for Ipv6State<I, StrongDeviceId>
{
    fn as_ref(&self) -> &IpStateInner<Ipv6, I, StrongDeviceId> {
        &self.inner
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IpStateFragmentCache<I>> for StackState<BT>
where
    I: Ip,
    BT: BindingsTypes,
{
    type Data = IpPacketFragmentCache<I, BT::Instant>;
    type Guard<'l> = LockGuard<'l, IpPacketFragmentCache<I, BT::Instant>> where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: Ip, II>(LockGuard<'l, IpPacketFragmentCache<I, II>>);

        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.inner.fragment_cache.lock()),
            |()| Wrap(self.ipv6.inner.fragment_cache.lock()),
        );
        guard
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IpStatePmtuCache<I>> for StackState<BT>
where
    I: Ip,
    BT: BindingsTypes,
{
    type Data = PmtuCache<I, BT::Instant>;
    type Guard<'l> = LockGuard<'l, PmtuCache<I, BT::Instant>> where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: Ip, II>(LockGuard<'l, PmtuCache<I, II>>);

        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.inner.pmtu_cache.lock()),
            |()| Wrap(self.ipv6.inner.pmtu_cache.lock()),
        );
        guard
    }
}

impl<I: Ip, BT: BindingsTypes> RwLockFor<crate::lock_ordering::IpStateRoutingTable<I>>
    for StackState<BT>
{
    type Data = ForwardingTable<I, DeviceId<BT>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, ForwardingTable<I, DeviceId<BT>>>
        where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, ForwardingTable<I, DeviceId<BT>>>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: Ip, BT: DeviceLayerTypes>(
            RwLockReadGuard<'l, ForwardingTable<I, DeviceId<BT>>>,
        );

        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.inner.table.read()),
            |()| Wrap(self.ipv6.inner.table.read()),
        );
        guard
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: Ip, BT: DeviceLayerTypes>(
            RwLockWriteGuard<'l, ForwardingTable<I, DeviceId<BT>>>,
        );

        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.inner.table.write()),
            |()| Wrap(self.ipv6.inner.table.write()),
        );
        guard
    }
}

impl<I, BT> RwLockFor<crate::lock_ordering::IcmpBoundMap<I>> for StackState<BT>
where
    I: IpExt,
    BT: BindingsTypes,
{
    type Data = icmp::socket::BoundSockets<I, WeakDeviceId<BT>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: IpExt, D: crate::device::WeakId>(
            RwLockReadGuard<'l, icmp::socket::BoundSockets<I, D>>,
        );
        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.icmp.inner.sockets.bound_and_id_allocator.read()),
            |()| Wrap(self.ipv6.icmp.inner.sockets.bound_and_id_allocator.read()),
        );
        guard
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: IpExt, D: crate::device::WeakId>(
            RwLockWriteGuard<'l, icmp::socket::BoundSockets<I, D>>,
        );
        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.icmp.inner.sockets.bound_and_id_allocator.write()),
            |()| Wrap(self.ipv6.icmp.inner.sockets.bound_and_id_allocator.write()),
        );
        guard
    }
}

impl<I, BT> RwLockFor<crate::lock_ordering::IcmpSocketsTable<I>> for StackState<BT>
where
    I: crate::socket::datagram::DualStackIpExt,
    BT: BindingsTypes,
{
    type Data = icmp::socket::SocketsState<I, WeakDeviceId<BT>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: crate::socket::datagram::DualStackIpExt, D: crate::device::WeakId>(
            RwLockReadGuard<'l, icmp::socket::SocketsState<I, D>>,
        );
        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.icmp.inner.sockets.state.read()),
            |()| Wrap(self.ipv6.icmp.inner.sockets.state.read()),
        );
        guard
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<'l, I: crate::socket::datagram::DualStackIpExt, D: crate::device::WeakId>(
            RwLockWriteGuard<'l, icmp::socket::SocketsState<I, D>>,
        );
        let Wrap(guard) = I::map_ip(
            (),
            |()| Wrap(self.ipv4.icmp.inner.sockets.state.write()),
            |()| Wrap(self.ipv6.icmp.inner.sockets.state.write()),
        );
        guard
    }
}

impl<I, BT> LockFor<crate::lock_ordering::IcmpTokenBucket<I>> for StackState<BT>
where
    I: Ip,
    BT: BindingsTypes,
{
    type Data = TokenBucket<BT::Instant>;
    type Guard<'l> = LockGuard<'l, TokenBucket<BT::Instant>>
        where Self: 'l;

    fn lock(&self) -> Self::Guard<'_> {
        let IpInvariant(guard) = I::map_ip(
            (),
            |()| IpInvariant(self.ipv4.icmp.as_ref().error_send_bucket.lock()),
            |()| IpInvariant(self.ipv6.icmp.as_ref().error_send_bucket.lock()),
        );
        guard
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::Ipv4StateNextPacketId>
    for StackState<BC>
{
    type Data = AtomicU16;
    type Guard<'l> = &'l AtomicU16 where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.ipv4.next_packet_id
    }
}

/// IP layer counters.
pub type IpCounters<I> = IpMarked<I, IpCountersInner>;

/// Ip layer counters for a specific IP version.
#[derive(Default)]
pub struct IpCountersInner {
    /// Count of incoming IP packets that are dispatched to the appropriate protocol.
    pub dispatch_receive_ip_packet: Counter,
    /// Count of incoming IP packets destined to another host.
    pub dispatch_receive_ip_packet_other_host: Counter,
    /// Count of incoming IP packets received by the stack.
    pub receive_ip_packet: Counter,
    /// Count of sent outgoing IP packets.
    pub send_ip_packet: Counter,
    /// Count of packets to be forwarded which are instead dropped because
    /// routing is disabled.
    pub routing_disabled_per_device: Counter,
    /// Count of incoming packets forwarded to another host.
    pub forward: Counter,
    /// Count of incoming packets which cannot be forwarded because there is no
    /// route to the destination host.
    pub no_route_to_host: Counter,
    /// Count of incoming packets which cannot be forwarded because the MTU has
    /// been exceeded.
    pub mtu_exceeded: Counter,
    /// Count of incoming packets which cannot be forwarded because the TTL has
    /// expired.
    pub ttl_expired: Counter,
    /// Count of ICMP error messages received.
    pub receive_icmp_error: Counter,
    /// Count of IP fragment reassembly errors.
    pub fragment_reassembly_error: Counter,
    /// Count of IP fragments that could not be reassembled because more
    /// fragments were needed.
    pub need_more_fragments: Counter,
    /// Count of IP fragments that could not be reassembled because the fragment
    /// was invalid.
    pub invalid_fragment: Counter,
    /// Count of IP fragments that could not be reassembled because the stack's
    /// per-IP-protocol fragment cache was full.
    pub fragment_cache_full: Counter,
    /// Count of incoming IP packets not delivered because of a parameter problem.
    pub parameter_problem: Counter,
    /// Count of incoming IP packets with an unspecified destination address.
    pub unspecified_destination: Counter,
    /// Count of incoming IP packets with an unspecified source address.
    pub unspecified_source: Counter,
    /// Count of incoming IP packets dropped.
    pub dropped: Counter,
}

/// IPv4-specific counters.
#[derive(Default)]
pub struct Ipv4Counters {
    /// Count of incoming IPv4 packets delivered.
    pub deliver: Counter,
}

/// IPv6-specific counters.
#[derive(Default)]
pub struct Ipv6Counters {
    /// Count of incoming IPv6 multicast packets delivered.
    pub deliver_multicast: Counter,
    /// Count of incoming IPv6 unicast packets delivered.
    pub deliver_unicast: Counter,
    /// Count of incoming IPv6 packets dropped because the destination address
    /// is only tentatively assigned to the device.
    pub drop_for_tentative: Counter,
    /// Count of incoming IPv6 packets dropped due to a non-unicast source address.
    pub non_unicast_source: Counter,
    /// Count of incoming IPv6 packets discarded while processing extension
    /// headers.
    pub extension_header_discard: Counter,
}

impl<BC: BindingsContext, I: Ip> UnlockedAccess<crate::lock_ordering::IpStateCounters<I>>
    for StackState<BC>
{
    type Data = IpCounters<I>;
    type Guard<'l> = &'l IpCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ip_counters()
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::Ipv4StateCounters>
    for StackState<BC>
{
    type Data = Ipv4Counters;
    type Guard<'l> = &'l Ipv4Counters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ipv4().counters()
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::Ipv6StateCounters>
    for StackState<BC>
{
    type Data = Ipv6Counters;
    type Guard<'l> = &'l Ipv6Counters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ipv6().counters()
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct IpStateInner<I: Ip, Instant: crate::Instant, DeviceId> {
    table: RwLock<ForwardingTable<I, DeviceId>>,
    fragment_cache: Mutex<IpPacketFragmentCache<I, Instant>>,
    pmtu_cache: Mutex<PmtuCache<I, Instant>>,
    counters: IpCounters<I>,
}

impl<I: Ip, Instant: crate::Instant, DeviceId> IpStateInner<I, Instant, DeviceId> {
    pub(crate) fn counters(&self) -> &IpCounters<I> {
        &self.counters
    }
}

/// The identifier for timer events in the IP layer.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) enum IpLayerTimerId {
    /// A timer event for IPv4 packet reassembly timers.
    ReassemblyTimeoutv4(FragmentCacheKey<Ipv4Addr>),
    /// A timer event for IPv6 packet reassembly timers.
    ReassemblyTimeoutv6(FragmentCacheKey<Ipv6Addr>),
    /// A timer event for IPv4 path MTU discovery.
    PmtuTimeoutv4(PmtuTimerId<Ipv4>),
    /// A timer event for IPv6 path MTU discovery.
    PmtuTimeoutv6(PmtuTimerId<Ipv6>),
}

impl From<FragmentCacheKey<Ipv4Addr>> for IpLayerTimerId {
    fn from(timer: FragmentCacheKey<Ipv4Addr>) -> IpLayerTimerId {
        IpLayerTimerId::ReassemblyTimeoutv4(timer)
    }
}

impl From<FragmentCacheKey<Ipv6Addr>> for IpLayerTimerId {
    fn from(timer: FragmentCacheKey<Ipv6Addr>) -> IpLayerTimerId {
        IpLayerTimerId::ReassemblyTimeoutv6(timer)
    }
}

impl From<PmtuTimerId<Ipv4>> for IpLayerTimerId {
    fn from(timer: PmtuTimerId<Ipv4>) -> IpLayerTimerId {
        IpLayerTimerId::PmtuTimeoutv4(timer)
    }
}

impl From<PmtuTimerId<Ipv6>> for IpLayerTimerId {
    fn from(timer: PmtuTimerId<Ipv6>) -> IpLayerTimerId {
        IpLayerTimerId::PmtuTimeoutv6(timer)
    }
}

impl_timer_context!(
    IpLayerTimerId,
    FragmentCacheKey<Ipv4Addr>,
    IpLayerTimerId::ReassemblyTimeoutv4(id),
    id
);
impl_timer_context!(
    IpLayerTimerId,
    FragmentCacheKey<Ipv6Addr>,
    IpLayerTimerId::ReassemblyTimeoutv6(id),
    id
);
impl_timer_context!(IpLayerTimerId, PmtuTimerId<Ipv4>, IpLayerTimerId::PmtuTimeoutv4(id), id);
impl_timer_context!(IpLayerTimerId, PmtuTimerId<Ipv6>, IpLayerTimerId::PmtuTimeoutv6(id), id);

/// Handle a timer event firing in the IP layer.
pub(crate) fn handle_timer<BC: BindingsContext>(
    core_ctx: &mut CoreCtx<'_, BC, crate::lock_ordering::Unlocked>,
    bindings_ctx: &mut BC,
    id: IpLayerTimerId,
) {
    match id {
        IpLayerTimerId::ReassemblyTimeoutv4(key) => {
            TimerHandler::handle_timer(core_ctx, bindings_ctx, key)
        }
        IpLayerTimerId::ReassemblyTimeoutv6(key) => {
            TimerHandler::handle_timer(core_ctx, bindings_ctx, key)
        }
        IpLayerTimerId::PmtuTimeoutv4(id) => TimerHandler::handle_timer(core_ctx, bindings_ctx, id),
        IpLayerTimerId::PmtuTimeoutv6(id) => TimerHandler::handle_timer(core_ctx, bindings_ctx, id),
    }
}

// TODO(joshlf): Once we support multiple extension headers in IPv6, we will
// need to verify that the callers of this function are still sound. In
// particular, they may accidentally pass a parse_metadata argument which
// corresponds to a single extension header rather than all of the IPv6 headers.

/// Dispatch a received IPv4 packet to the appropriate protocol.
///
/// `device` is the device the packet was received on. `parse_metadata` is the
/// parse metadata associated with parsing the IP headers. It is used to undo
/// that parsing. Both `device` and `parse_metadata` are required in order to
/// send ICMP messages in response to unrecognized protocols or ports. If either
/// of `device` or `parse_metadata` is `None`, the caller promises that the
/// protocol and port are recognized.
///
/// # Panics
///
/// `dispatch_receive_ipv4_packet` panics if the protocol is unrecognized and
/// `parse_metadata` is `None`. If an IGMP message is received but it is not
/// coming from a device, i.e., `device` given is `None`,
/// `dispatch_receive_ip_packet` will also panic.
fn dispatch_receive_ipv4_packet<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: Ipv4Addr,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    body: B,
    parse_metadata: Option<ParseMetadata>,
) {
    core_ctx.with_counters(|counters| {
        counters.dispatch_receive_ip_packet.increment();
    });

    match frame_dst {
        FrameDestination::Individual { local: false } => {
            core_ctx.with_counters(|counters| {
                counters.dispatch_receive_ip_packet_other_host.increment();
            });
        }
        FrameDestination::Individual { local: true }
        | FrameDestination::Multicast
        | FrameDestination::Broadcast => (),
    };

    let (mut body, err) = match core_ctx.dispatch_receive_ip_packet(
        bindings_ctx,
        device,
        src_ip,
        dst_ip,
        proto,
        body,
    ) {
        Ok(()) => return,
        Err(e) => e,
    };
    // All branches promise to return the buffer in the same state it was in
    // when they were executed. Thus, all we have to do is undo the parsing
    // of the IP packet header, and the buffer will be back to containing
    // the entire original IP packet.
    let meta = parse_metadata.unwrap();
    body.undo_parse(meta);

    if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
        match err.inner {
            TransportReceiveErrorInner::ProtocolUnsupported => {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    body,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::ProtocolUnreachable,
                        header_len: meta.header_len(),
                    },
                );
            }
            TransportReceiveErrorInner::PortUnreachable => {
                // TODO(joshlf): What if we're called from a loopback
                // handler, and device and parse_metadata are None? In other
                // words, what happens if we attempt to send to a loopback
                // port which is unreachable? We will eventually need to
                // restructure the control flow here to handle that case.
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    body,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::PortUnreachable,
                        header_len: meta.header_len(),
                    },
                );
            }
        }
    } else {
        trace!("dispatch_receive_ipv4_packet: Cannot send ICMP error message in response to a packet from the unspecified address");
    }
}

/// Dispatch a received IPv6 packet to the appropriate protocol.
///
/// `dispatch_receive_ipv6_packet` has the same semantics as
/// `dispatch_receive_ipv4_packet`, but for IPv6.
fn dispatch_receive_ipv6_packet<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: Ipv6SourceAddr,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    body: B,
    parse_metadata: Option<ParseMetadata>,
) {
    // TODO(https://fxbug.dev/21227): Once we support multiple extension
    // headers in IPv6, we will need to verify that the callers of this
    // function are still sound. In particular, they may accidentally pass a
    // parse_metadata argument which corresponds to a single extension
    // header rather than all of the IPv6 headers.

    core_ctx.with_counters(|counters| {
        counters.dispatch_receive_ip_packet.increment();
    });

    match frame_dst {
        FrameDestination::Individual { local: false } => {
            core_ctx.with_counters(|counters| {
                counters.dispatch_receive_ip_packet_other_host.increment();
            });
        }
        FrameDestination::Individual { local: true }
        | FrameDestination::Multicast
        | FrameDestination::Broadcast => (),
    };

    let (mut body, err) = match core_ctx.dispatch_receive_ip_packet(
        bindings_ctx,
        device,
        src_ip,
        dst_ip,
        proto,
        body,
    ) {
        Ok(()) => return,
        Err(e) => e,
    };

    // All branches promise to return the buffer in the same state it was in
    // when they were executed. Thus, all we have to do is undo the parsing
    // of the IP packet header, and the buffer will be back to containing
    // the entire original IP packet.
    let meta = parse_metadata.unwrap();
    body.undo_parse(meta);

    match err.inner {
        TransportReceiveErrorInner::ProtocolUnsupported => {
            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    body,
                    Icmpv6ErrorKind::ProtocolUnreachable { header_len: meta.header_len() },
                );
            }
        }
        TransportReceiveErrorInner::PortUnreachable => {
            // TODO(joshlf): What if we're called from a loopback handler,
            // and device and parse_metadata are None? In other words, what
            // happens if we attempt to send to a loopback port which is
            // unreachable? We will eventually need to restructure the
            // control flow here to handle that case.
            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                core_ctx.send_icmp_error_message(
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    body,
                    Icmpv6ErrorKind::PortUnreachable,
                );
            }
        }
    }
}

/// Drop a packet and undo the effects of parsing it.
///
/// `drop_packet_and_undo_parse!` takes a `$packet` and a `$buffer` which the
/// packet was parsed from. It saves the results of the `src_ip()`, `dst_ip()`,
/// `proto()`, and `parse_metadata()` methods. It drops `$packet` and uses the
/// result of `parse_metadata()` to undo the effects of parsing the packet.
/// Finally, it returns the source IP, destination IP, protocol, and parse
/// metadata.
macro_rules! drop_packet_and_undo_parse {
    ($packet:expr, $buffer:expr) => {{
        let (src_ip, dst_ip, proto, meta) = $packet.into_metadata();
        $buffer.undo_parse(meta);
        (src_ip, dst_ip, proto, meta)
    }};
}

/// Process a fragment and reassemble if required.
///
/// Attempts to process a potential fragment packet and reassemble if we are
/// ready to do so. If the packet isn't fragmented, or a packet was reassembled,
/// attempt to dispatch the packet.
macro_rules! process_fragment {
    ($core_ctx:expr, $bindings_ctx:expr, $dispatch:ident, $device:ident, $frame_dst:expr, $buffer:expr, $packet:expr, $src_ip:expr, $dst_ip:expr, $ip:ident) => {{
        match FragmentHandler::<$ip, _>::process_fragment::<&mut [u8]>(
            $core_ctx,
            $bindings_ctx,
            $packet,
        ) {
            // Handle the packet right away since reassembly is not needed.
            FragmentProcessingState::NotNeeded(packet) => {
                trace!("receive_ip_packet: not fragmented");
                // TODO(joshlf):
                // - Check for already-expired TTL?
                let (_, _, proto, meta) = packet.into_metadata();
                $dispatch(
                    $core_ctx,
                    $bindings_ctx,
                    $device,
                    $frame_dst,
                    $src_ip,
                    $dst_ip,
                    proto,
                    $buffer,
                    Some(meta),
                );
            }
            // Ready to reassemble a packet.
            FragmentProcessingState::Ready { key, packet_len } => {
                trace!("receive_ip_packet: fragmented, ready for reassembly");
                // Allocate a buffer of `packet_len` bytes.
                let mut buffer = Buf::new(alloc::vec![0; packet_len], ..);

                // Attempt to reassemble the packet.
                match FragmentHandler::<$ip, _>::reassemble_packet(
                    $core_ctx,
                    $bindings_ctx,
                    &key,
                    buffer.buffer_view_mut(),
                ) {
                    // Successfully reassembled the packet, handle it.
                    Ok(packet) => {
                        trace!("receive_ip_packet: fragmented, reassembled packet: {:?}", packet);
                        // TODO(joshlf):
                        // - Check for already-expired TTL?
                        let (_, _, proto, meta) = packet.into_metadata();
                        $dispatch::<_, Buf<Vec<u8>>, _>(
                            $core_ctx,
                            $bindings_ctx,
                            $device,
                            $frame_dst,
                            $src_ip,
                            $dst_ip,
                            proto,
                            buffer,
                            Some(meta),
                        );
                    }
                    // TODO(ghanan): Handle reassembly errors, remove
                    // `allow(unreachable_patterns)` when complete.
                    _ => return,
                    #[allow(unreachable_patterns)]
                    Err(e) => {
                        $core_ctx.with_counters(|counters: &IpCounters<$ip>| {
                            counters.fragment_reassembly_error.increment();
                        });
                        trace!("receive_ip_packet: fragmented, failed to reassemble: {:?}", e);
                    }
                }
            }
            // Cannot proceed since we need more fragments before we
            // can reassemble a packet.
            FragmentProcessingState::NeedMoreFragments => {
                $core_ctx.with_counters(|counters: &IpCounters<$ip>| {
                    counters.need_more_fragments.increment();
                });
                trace!("receive_ip_packet: fragmented, need more before reassembly")
            }
            // TODO(ghanan): Handle invalid fragments.
            FragmentProcessingState::InvalidFragment => {
                $core_ctx.with_counters(|counters: &IpCounters<$ip>| {
                    counters.invalid_fragment.increment();
                });
                trace!("receive_ip_packet: fragmented, invalid")
            }
            FragmentProcessingState::OutOfMemory => {
                $core_ctx.with_counters(|counters: &IpCounters<$ip>| {
                    counters.fragment_cache_full.increment();
                });
                trace!("receive_ip_packet: fragmented, dropped because OOM")
            }
        };
    }};
}

// TODO(joshlf): Can we turn `try_parse_ip_packet` into a function? So far, I've
// been unable to get the borrow checker to accept it.

/// Try to parse an IP packet from a buffer.
///
/// If parsing fails, return the buffer to its original state so that its
/// contents can be used to send an ICMP error message. When invoked, the macro
/// expands to an expression whose type is `Result<P, P::Error>`, where `P` is
/// the parsed packet type.
macro_rules! try_parse_ip_packet {
    ($buffer:expr) => {{
        let p_len = $buffer.prefix_len();
        let s_len = $buffer.suffix_len();

        let result = $buffer.parse_mut();

        if let Err(err) = result {
            // Revert `buffer` to it's original state.
            let n_p_len = $buffer.prefix_len();
            let n_s_len = $buffer.suffix_len();

            if p_len > n_p_len {
                $buffer.grow_front(p_len - n_p_len);
            }

            if s_len > n_s_len {
                $buffer.grow_back(s_len - n_s_len);
            }

            Err(err)
        } else {
            result
        }
    }};
}

/// Receive an IP packet from a device.
///
/// `receive_ip_packet` calls [`receive_ipv4_packet`] or [`receive_ipv6_packet`]
/// depending on the type parameter, `I`.
#[cfg(test)]
pub(crate) fn receive_ip_packet<B: BufferMut, BC: BindingsContext, I: Ip>(
    core_ctx: &SyncCtx<BC>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    frame_dst: FrameDestination,
    buffer: B,
) {
    let mut core_ctx = CoreCtx::new_deprecated(core_ctx);
    match I::VERSION {
        IpVersion::V4 => {
            receive_ipv4_packet(&mut core_ctx, bindings_ctx, device, frame_dst, buffer)
        }
        IpVersion::V6 => {
            receive_ipv6_packet(&mut core_ctx, bindings_ctx, device, frame_dst, buffer)
        }
    }
}

/// Receive an IPv4 packet from a device.
///
/// `frame_dst` specifies how this packet was received; see [`FrameDestination`]
/// for options.
pub(crate) fn receive_ipv4_packet<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv4, BC>
        + CounterContext<IpCounters<Ipv4>>
        + CounterContext<Ipv4Counters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    mut buffer: B,
) {
    if !core_ctx.is_ip_device_enabled(&device) {
        return;
    }

    core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
        counters.receive_ip_packet.increment();
    });
    trace!("receive_ip_packet({device:?})");

    let mut packet: Ipv4Packet<_> = match try_parse_ip_packet!(buffer) {
        Ok(packet) => packet,
        // Conditionally send an ICMP response if we encountered a parameter
        // problem error when parsing an IPv4 packet. Note, we do not always
        // send back an ICMP response as it can be used as an attack vector for
        // DDoS attacks. We only send back an ICMP response if the RFC requires
        // that we MUST send one, as noted by `must_send_icmp` and `action`.
        // TODO(https://fxbug.dev/77598): test this code path once
        // `Ipv4Packet::parse` can return an `IpParseError::ParameterProblem`
        // error.
        Err(IpParseError::ParameterProblem {
            src_ip,
            dst_ip,
            code,
            pointer,
            must_send_icmp,
            header_len,
            action,
        }) if must_send_icmp && action.should_send_icmp(&dst_ip) => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                counters.parameter_problem.increment();
            });
            // `should_send_icmp_to_multicast` should never return `true` for IPv4.
            assert!(!action.should_send_icmp_to_multicast());
            let dst_ip = match SpecifiedAddr::new(dst_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                        counters.unspecified_destination.increment();
                    });
                    debug!("receive_ipv4_packet: Received packet with unspecified destination IP address; dropping");
                    return;
                }
            };
            let src_ip = match SpecifiedAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                        counters.unspecified_source.increment();
                    });
                    trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv4Error {
                    kind: Icmpv4ErrorKind::ParameterProblem {
                        code,
                        pointer,
                        // When the call to `action.should_send_icmp` returns true, it always means that
                        // the IPv4 packet that failed parsing is an initial fragment.
                        fragment_type: Ipv4FragmentType::InitialFragment,
                    },
                    header_len,
                },
            );
            return;
        }
        _ => return, // TODO(joshlf): Do something with ICMP here?
    };

    let dst_ip = match SpecifiedAddr::new(packet.dst_ip()) {
        Some(ip) => ip,
        None => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                counters.unspecified_destination.increment();
            });
            debug!("receive_ipv4_packet: Received packet with unspecified destination IP address; dropping");
            return;
        }
    };

    // TODO(ghanan): Act upon options.

    match receive_ipv4_packet_action(core_ctx, bindings_ctx, device, dst_ip) {
        ReceivePacketAction::Deliver => {
            trace!("receive_ipv4_packet: delivering locally");
            let src_ip = packet.src_ip();

            // Process a potential IPv4 fragment if the destination is this
            // host.
            //
            // We process IPv4 packet reassembly here because, for IPv4, the
            // fragment data is in the header itself so we can handle it right
            // away.
            //
            // Note, the `process_fragment` function (which is called by the
            // `process_fragment!` macro) could panic if the packet does not
            // have fragment data. However, we are guaranteed that it will not
            // panic because the fragment data is in the fixed header so it is
            // always present (even if the fragment data has values that implies
            // that the packet is not fragmented).
            process_fragment!(
                core_ctx,
                bindings_ctx,
                dispatch_receive_ipv4_packet,
                device,
                frame_dst,
                buffer,
                packet,
                src_ip,
                dst_ip,
                Ipv4
            );
        }
        ReceivePacketAction::Forward { dst: Destination { device: dst_device, next_hop } } => {
            let next_hop = match next_hop {
                NextHop::RemoteAsNeighbor => dst_ip,
                NextHop::Gateway(gateway) => gateway,
            };
            let ttl = packet.ttl();
            if ttl > 1 {
                trace!("receive_ipv4_packet: forwarding");

                packet.set_ttl(ttl - 1);
                let _: (Ipv4Addr, Ipv4Addr, Ipv4Proto, ParseMetadata) =
                    drop_packet_and_undo_parse!(packet, buffer);
                match IpDeviceSendContext::<Ipv4, _>::send_ip_frame(
                    core_ctx,
                    bindings_ctx,
                    &dst_device,
                    next_hop,
                    buffer,
                ) {
                    Ok(()) => (),
                    Err(b) => {
                        let _: B = b;
                        core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                            counters.mtu_exceeded.increment();
                        });
                        // TODO(https://fxbug.dev/86247): Encode the MTU error
                        // more obviously in the type system.
                        debug!("failed to forward IPv4 packet: MTU exceeded");
                    }
                }
            } else {
                // TTL is 0 or would become 0 after decrement; see "TTL"
                // section, https://tools.ietf.org/html/rfc791#page-14
                use packet_formats::ipv4::Ipv4Header as _;
                core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                    counters.ttl_expired.increment();
                });
                debug!("received IPv4 packet dropped due to expired TTL");
                let fragment_type = packet.fragment_type();
                let (src_ip, _, proto, meta): (_, Ipv4Addr, _, _) =
                    drop_packet_and_undo_parse!(packet, buffer);
                let src_ip = match SpecifiedAddr::new(src_ip) {
                    Some(ip) => ip,
                    None => {
                        core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                            counters.unspecified_source.increment();
                        });
                        trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                        return;
                    }
                };
                IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                    core_ctx,
                    bindings_ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    buffer,
                    Icmpv4Error {
                        kind: Icmpv4ErrorKind::TtlExpired { proto, fragment_type },
                        header_len: meta.header_len(),
                    },
                );
            }
        }
        ReceivePacketAction::SendNoRouteToDest => {
            use packet_formats::ipv4::Ipv4Header as _;
            core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                counters.no_route_to_host.increment();
            });
            debug!("received IPv4 packet with no known route to destination {}", dst_ip);
            let fragment_type = packet.fragment_type();
            let (src_ip, _, proto, meta): (_, Ipv4Addr, _, _) =
                drop_packet_and_undo_parse!(packet, buffer);
            let src_ip = match SpecifiedAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                        counters.unspecified_source.increment();
                    });
                    trace!("receive_ipv4_packet: Cannot send ICMP error in response to packet with unspecified source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv4, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv4Error {
                    kind: Icmpv4ErrorKind::NetUnreachable { proto, fragment_type },
                    header_len: meta.header_len(),
                },
            );
        }
        ReceivePacketAction::Drop { reason } => {
            let src_ip = packet.src_ip();
            core_ctx.with_counters(|counters: &IpCounters<Ipv4>| {
                counters.dropped.increment();
            });
            debug!(
                "receive_ipv4_packet: dropping packet from {src_ip} to {dst_ip} received on \
                {device:?}: {reason:?}",
            );
        }
    }
}

/// Receive an IPv6 packet from a device.
///
/// `frame_dst` specifies how this packet was received; see [`FrameDestination`]
/// for options.
pub(crate) fn receive_ipv6_packet<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    B: BufferMut,
    CC: IpLayerIngressContext<Ipv6, BC>
        + CounterContext<IpCounters<Ipv6>>
        + CounterContext<Ipv6Counters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    frame_dst: FrameDestination,
    mut buffer: B,
) {
    if !core_ctx.is_ip_device_enabled(&device) {
        return;
    }

    core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
        counters.receive_ip_packet.increment();
    });
    trace!("receive_ipv6_packet({:?})", device);

    let mut packet: Ipv6Packet<_> = match try_parse_ip_packet!(buffer) {
        Ok(packet) => packet,
        // Conditionally send an ICMP response if we encountered a parameter
        // problem error when parsing an IPv4 packet. Note, we do not always
        // send back an ICMP response as it can be used as an attack vector for
        // DDoS attacks. We only send back an ICMP response if the RFC requires
        // that we MUST send one, as noted by `must_send_icmp` and `action`.
        Err(IpParseError::ParameterProblem {
            src_ip,
            dst_ip,
            code,
            pointer,
            must_send_icmp,
            header_len: _,
            action,
        }) if must_send_icmp && action.should_send_icmp(&dst_ip) => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                counters.parameter_problem.increment();
            });
            let dst_ip = match SpecifiedAddr::new(dst_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                        counters.unspecified_destination.increment();
                    });
                    debug!("receive_ipv6_packet: Received packet with unspecified destination IP address; dropping");
                    return;
                }
            };
            let src_ip = match UnicastAddr::new(src_ip) {
                Some(ip) => ip,
                None => {
                    core_ctx.with_counters(|counters: &Ipv6Counters| {
                        counters.non_unicast_source.increment();
                    });
                    trace!("receive_ipv6_packet: Cannot send ICMP error in response to packet with non unicast source IP address");
                    return;
                }
            };
            IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                core_ctx,
                bindings_ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                buffer,
                Icmpv6ErrorKind::ParameterProblem {
                    code,
                    pointer,
                    allow_dst_multicast: action.should_send_icmp_to_multicast(),
                },
            );
            return;
        }
        _ => return, // TODO(joshlf): Do something with ICMP here?
    };

    trace!("receive_ipv6_packet: parsed packet: {:?}", packet);

    // TODO(ghanan): Act upon extension headers.

    let src_ip = match packet.src_ipv6() {
        Some(ip) => ip,
        None => {
            debug!(
                "receive_ipv6_packet: received packet from non-unicast source {}; dropping",
                packet.src_ip()
            );
            core_ctx.with_counters(|counters: &Ipv6Counters| {
                counters.non_unicast_source.increment();
            });
            return;
        }
    };
    let dst_ip = match SpecifiedAddr::new(packet.dst_ip()) {
        Some(ip) => ip,
        None => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                counters.unspecified_destination.increment();
            });
            debug!("receive_ipv6_packet: Received packet with unspecified destination IP address; dropping");
            return;
        }
    };

    match receive_ipv6_packet_action(core_ctx, bindings_ctx, device, dst_ip) {
        ReceivePacketAction::Deliver => {
            trace!("receive_ipv6_packet: delivering locally");

            // Process a potential IPv6 fragment if the destination is this
            // host.
            //
            // We need to process extension headers in the order they appear in
            // the header. With some extension headers, we do not proceed to the
            // next header, and do some action immediately. For example, say we
            // have an IPv6 packet with two extension headers (routing extension
            // header before a fragment extension header). Until we get to the
            // final destination node in the routing header, we would need to
            // reroute the packet to the next destination without reassembling.
            // Once the packet gets to the last destination in the routing
            // header, that node will process the fragment extension header and
            // handle reassembly.
            match ipv6::handle_extension_headers(core_ctx, device, frame_dst, &packet, true) {
                Ipv6PacketAction::_Discard => {
                    core_ctx.with_counters(|counters: &Ipv6Counters| {
                        counters.extension_header_discard.increment();
                    });
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: discarding packet"
                    );
                }
                Ipv6PacketAction::Continue => {
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: dispatching packet"
                    );

                    // TODO(joshlf):
                    // - Do something with ICMP if we don't have a handler for
                    //   that protocol?
                    // - Check for already-expired TTL?
                    let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) = packet.into_metadata();
                    dispatch_receive_ipv6_packet(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        src_ip,
                        dst_ip,
                        proto,
                        buffer,
                        Some(meta),
                    );
                }
                Ipv6PacketAction::ProcessFragment => {
                    trace!(
                        "receive_ipv6_packet: handled IPv6 extension headers: handling fragmented packet"
                    );

                    // Note, the `IpPacketFragmentCache::process_fragment`
                    // method (which is called by the `process_fragment!` macro)
                    // could panic if the packet does not have fragment data.
                    // However, we are guaranteed that it will not panic for an
                    // IPv6 packet because the fragment data is in an (optional)
                    // fragment extension header which we attempt to handle by
                    // calling `ipv6::handle_extension_headers`. We will only
                    // end up here if its return value is
                    // `Ipv6PacketAction::ProcessFragment` which is only
                    // possible when the packet has the fragment extension
                    // header (even if the fragment data has values that implies
                    // that the packet is not fragmented).
                    //
                    // TODO(ghanan): Handle extension headers again since there
                    //               could be some more in a reassembled packet
                    //               (after the fragment header).
                    process_fragment!(
                        core_ctx,
                        bindings_ctx,
                        dispatch_receive_ipv6_packet,
                        device,
                        frame_dst,
                        buffer,
                        packet,
                        src_ip,
                        dst_ip,
                        Ipv6
                    );
                }
            }
        }
        ReceivePacketAction::Forward { dst: Destination { device: dst_device, next_hop } } => {
            let ttl = packet.ttl();
            if ttl > 1 {
                trace!("receive_ipv6_packet: forwarding");

                // Handle extension headers first.
                match ipv6::handle_extension_headers(core_ctx, device, frame_dst, &packet, false) {
                    Ipv6PacketAction::_Discard => {
                        core_ctx.with_counters(|counters: &Ipv6Counters| {
                            counters.extension_header_discard.increment();
                        });
                        trace!("receive_ipv6_packet: handled IPv6 extension headers: discarding packet");
                        return;
                    }
                    Ipv6PacketAction::Continue => {
                        trace!("receive_ipv6_packet: handled IPv6 extension headers: forwarding packet");
                    }
                    Ipv6PacketAction::ProcessFragment => unreachable!("When forwarding packets, we should only ever look at the hop by hop options extension header (if present)"),
                }

                let next_hop = match next_hop {
                    NextHop::RemoteAsNeighbor => dst_ip,
                    NextHop::Gateway(gateway) => gateway,
                };
                packet.set_ttl(ttl - 1);
                let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) =
                    drop_packet_and_undo_parse!(packet, buffer);
                if let Err(buffer) = IpDeviceSendContext::<Ipv6, _>::send_ip_frame(
                    core_ctx,
                    bindings_ctx,
                    &dst_device,
                    next_hop,
                    buffer,
                ) {
                    // TODO(https://fxbug.dev/86247): Encode the MTU error more
                    // obviously in the type system.
                    core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                        counters.mtu_exceeded.increment();
                    });
                    debug!("failed to forward IPv6 packet: MTU exceeded");
                    if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                        trace!("receive_ipv6_packet: Sending ICMPv6 Packet Too Big");
                        // TODO(joshlf): Increment the TTL since we just
                        // decremented it. The fact that we don't do this is
                        // technically a violation of the ICMP spec (we're not
                        // encapsulating the original packet that caused the
                        // issue, but a slightly modified version of it), but
                        // it's not that big of a deal because it won't affect
                        // the sender's ability to figure out the minimum path
                        // MTU. This may break other logic, though, so we should
                        // still fix it eventually.
                        let mtu = core_ctx.get_mtu(&device);
                        IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                            core_ctx,
                            bindings_ctx,
                            device,
                            frame_dst,
                            *src_ip,
                            dst_ip,
                            buffer,
                            Icmpv6ErrorKind::PacketTooBig {
                                proto,
                                header_len: meta.header_len(),
                                mtu,
                            },
                        );
                    }
                }
            } else {
                // Hop Limit is 0 or would become 0 after decrement; see RFC
                // 2460 Section 3.
                core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                    counters.ttl_expired.increment();
                });
                debug!("received IPv6 packet dropped due to expired Hop Limit");

                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) =
                        drop_packet_and_undo_parse!(packet, buffer);
                    IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                        core_ctx,
                        bindings_ctx,
                        device,
                        frame_dst,
                        *src_ip,
                        dst_ip,
                        buffer,
                        Icmpv6ErrorKind::TtlExpired { proto, header_len: meta.header_len() },
                    );
                }
            }
        }
        ReceivePacketAction::SendNoRouteToDest => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                counters.no_route_to_host.increment();
            });
            let (_, _, proto, meta): (Ipv6Addr, Ipv6Addr, _, _) =
                drop_packet_and_undo_parse!(packet, buffer);
            debug!("received IPv6 packet with no known route to destination {}", dst_ip);

            if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                IcmpErrorHandler::<Ipv6, _>::send_icmp_error_message(
                    core_ctx,
                    bindings_ctx,
                    device,
                    frame_dst,
                    *src_ip,
                    dst_ip,
                    buffer,
                    Icmpv6ErrorKind::NetUnreachable { proto, header_len: meta.header_len() },
                );
            }
        }
        ReceivePacketAction::Drop { reason } => {
            core_ctx.with_counters(|counters: &IpCounters<Ipv6>| {
                counters.dropped.increment();
            });
            let src_ip = packet.src_ip();
            debug!(
                "receive_ipv6_packet: dropping packet from {src_ip} to {dst_ip} received on \
                {device:?}: {reason:?}",
            );
        }
    }
}

/// The action to take in order to process a received IP packet.
#[cfg_attr(test, derive(Debug, PartialEq))]
enum ReceivePacketAction<A: IpAddress, DeviceId> {
    /// Deliver the packet locally.
    Deliver,
    /// Forward the packet to the given destination.
    Forward { dst: Destination<A, DeviceId> },
    /// Send a Destination Unreachable ICMP error message to the packet's sender
    /// and drop the packet.
    ///
    /// For ICMPv4, use the code "net unreachable". For ICMPv6, use the code "no
    /// route to destination".
    SendNoRouteToDest,
    /// Silently drop the packet.
    ///
    /// `reason` describes why the packet was dropped.
    Drop { reason: DropReason },
}

#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug)]
enum DropReason {
    /// Remote packet destined to tentative address.
    Tentative,
    /// Packet should be forwarded but packet's inbound interface has forwarding
    /// disabled.
    ForwardingDisabledInboundIface,
}

/// Computes the action to take in order to process a received IPv4 packet.
fn receive_ipv4_packet_action<
    BC: IpLayerBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpLayerContext<Ipv4, BC> + CounterContext<IpCounters<Ipv4>> + CounterContext<Ipv4Counters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
) -> ReceivePacketAction<Ipv4Addr, CC::DeviceId> {
    // If the packet arrived at the loopback interface, check if any local
    // interface has the destination address assigned. This effectively lets
    // the loopback interface operate as a weak host for incoming packets.
    //
    // Note that (as of writing) the stack sends all locally destined traffic to
    // the loopback interface so we need this hack to allow the stack to accept
    // packets that arrive at the loopback interface (after being looped back)
    // but destined to an address that is assigned to another local interface.
    //
    // TODO(https://fxbug.dev/93870): This should instead be controlled by the
    // routing table.

    // Since we treat all addresses identically, it doesn't matter whether one
    // or more than one device has the address assigned. That means we can just
    // take the first status and ignore the rest.
    let first_status = if device.is_loopback() {
        core_ctx.with_address_statuses(dst_ip, |it| it.map(|(_device, status)| status).next())
    } else {
        core_ctx.address_status_for_device(dst_ip, device).into_present()
    };
    match first_status {
        Some(
            Ipv4PresentAddressStatus::LimitedBroadcast
            | Ipv4PresentAddressStatus::SubnetBroadcast
            | Ipv4PresentAddressStatus::Multicast
            | Ipv4PresentAddressStatus::Unicast,
        ) => {
            core_ctx.with_counters(|counters: &Ipv4Counters| {
                counters.deliver.increment();
            });
            ReceivePacketAction::Deliver
        }
        None => {
            receive_ip_packet_action_common::<Ipv4, _, _>(core_ctx, bindings_ctx, dst_ip, device)
        }
    }
}

/// Computes the action to take in order to process a received IPv6 packet.
fn receive_ipv6_packet_action<
    BC: IpLayerBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpLayerContext<Ipv6, BC> + CounterContext<IpCounters<Ipv6>> + CounterContext<Ipv6Counters>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
) -> ReceivePacketAction<Ipv6Addr, CC::DeviceId> {
    // If the packet arrived at the loopback interface, check if any local
    // interface has the destination address assigned. This effectively lets
    // the loopback interface operate as a weak host for incoming packets.
    //
    // Note that (as of writing) the stack sends all locally destined traffic to
    // the loopback interface so we need this hack to allow the stack to accept
    // packets that arrive at the loopback interface (after being looped back)
    // but destined to an address that is assigned to another local interface.
    //
    // TODO(https://fxbug.dev/93870): This should instead be controlled by the
    // routing table.

    // It's possible that there is more than one device with the address
    // assigned. Since IPv6 addresses are either multicast or unicast, we
    // don't expect to see one device with `UnicastAssigned` or
    // `UnicastTentative` and another with `Multicast`. We might see one
    // assigned and one tentative status, though, in which case we should
    // prefer the former.
    fn choose_highest_priority(
        address_statuses: impl Iterator<Item = Ipv6PresentAddressStatus>,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
    ) -> Option<Ipv6PresentAddressStatus> {
        address_statuses.max_by(|lhs, rhs| {
            use Ipv6PresentAddressStatus::*;
            match (lhs, rhs) {
                (UnicastAssigned | UnicastTentative, Multicast)
                | (Multicast, UnicastAssigned | UnicastTentative) => {
                    unreachable!("the IPv6 address {:?} is not both unicast and multicast", dst_ip)
                }
                (UnicastAssigned, UnicastTentative) => Ordering::Greater,
                (UnicastTentative, UnicastAssigned) => Ordering::Less,
                (UnicastTentative, UnicastTentative)
                | (UnicastAssigned, UnicastAssigned)
                | (Multicast, Multicast) => Ordering::Equal,
            }
        })
    }

    let highest_priority = if device.is_loopback() {
        core_ctx.with_address_statuses(dst_ip, |it| {
            let it = it.map(|(_device, status)| status);
            choose_highest_priority(it, dst_ip)
        })
    } else {
        core_ctx.address_status_for_device(dst_ip, device).into_present()
    };
    match highest_priority {
        Some(Ipv6PresentAddressStatus::Multicast) => {
            core_ctx.with_counters(|counters: &Ipv6Counters| {
                counters.deliver_multicast.increment();
            });
            ReceivePacketAction::Deliver
        }
        Some(Ipv6PresentAddressStatus::UnicastAssigned) => {
            core_ctx.with_counters(|counters: &Ipv6Counters| {
                counters.deliver_unicast.increment();
            });
            ReceivePacketAction::Deliver
        }
        Some(Ipv6PresentAddressStatus::UnicastTentative) => {
            // If the destination address is tentative (which implies that
            // we are still performing NDP's Duplicate Address Detection on
            // it), then we don't consider the address "assigned to an
            // interface", and so we drop packets instead of delivering them
            // locally.
            //
            // As per RFC 4862 section 5.4:
            //
            //   An address on which the Duplicate Address Detection
            //   procedure is applied is said to be tentative until the
            //   procedure has completed successfully. A tentative address
            //   is not considered "assigned to an interface" in the
            //   traditional sense.  That is, the interface must accept
            //   Neighbor Solicitation and Advertisement messages containing
            //   the tentative address in the Target Address field, but
            //   processes such packets differently from those whose Target
            //   Address matches an address assigned to the interface. Other
            //   packets addressed to the tentative address should be
            //   silently discarded. Note that the "other packets" include
            //   Neighbor Solicitation and Advertisement messages that have
            //   the tentative (i.e., unicast) address as the IP destination
            //   address and contain the tentative address in the Target
            //   Address field.  Such a case should not happen in normal
            //   operation, though, since these messages are multicasted in
            //   the Duplicate Address Detection procedure.
            //
            // That is, we accept no packets destined to a tentative
            // address. NS and NA packets should be addressed to a multicast
            // address that we would have joined during DAD so that we can
            // receive those packets.
            core_ctx.with_counters(|counters: &Ipv6Counters| {
                counters.drop_for_tentative.increment();
            });
            ReceivePacketAction::Drop { reason: DropReason::Tentative }
        }
        None => {
            receive_ip_packet_action_common::<Ipv6, _, _>(core_ctx, bindings_ctx, dst_ip, device)
        }
    }
}

/// Computes the remaining protocol-agnostic actions on behalf of
/// [`receive_ipv4_packet_action`] and [`receive_ipv6_packet_action`].
fn receive_ip_packet_action_common<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC> + CounterContext<IpCounters<I>>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    dst_ip: SpecifiedAddr<I::Addr>,
    device_id: &CC::DeviceId,
) -> ReceivePacketAction<I::Addr, CC::DeviceId> {
    // The packet is not destined locally, so we attempt to forward it.
    if !core_ctx.is_device_forwarding_enabled(device_id) {
        // Forwarding is disabled; we are operating only as a host.
        //
        // For IPv4, per RFC 1122 Section 3.2.1.3, "A host MUST silently discard
        // an incoming datagram that is not destined for the host."
        //
        // For IPv6, per RFC 4443 Section 3.1, the only instance in which a host
        // sends an ICMPv6 Destination Unreachable message is when a packet is
        // destined to that host but on an unreachable port (Code 4 - "Port
        // unreachable"). Since the only sensible error message to send in this
        // case is a Destination Unreachable message, we interpret the RFC text
        // to mean that, consistent with IPv4's behavior, we should silently
        // discard the packet in this case.
        core_ctx.with_counters(|counters| {
            counters.routing_disabled_per_device.increment();
        });
        ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
    } else {
        match lookup_route_table(core_ctx, bindings_ctx, None, *dst_ip) {
            Some(dst) => {
                core_ctx.with_counters(|counters| {
                    counters.forward.increment();
                });
                ReceivePacketAction::Forward { dst }
            }
            None => {
                core_ctx.with_counters(|counters| {
                    counters.no_route_to_host.increment();
                });
                ReceivePacketAction::SendNoRouteToDest
            }
        }
    }
}

// Look up the route to a host.
fn lookup_route_table<
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, CC::DeviceId>,
    CC: IpLayerContext<I, BC>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    device: Option<&CC::DeviceId>,
    dst_ip: I::Addr,
) -> Option<Destination<I::Addr, CC::DeviceId>> {
    core_ctx.with_ip_routing_table(|core_ctx, table| table.lookup(core_ctx, device, dst_ip))
}

/// Get all the routes.
pub fn get_all_routes<BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
) -> Vec<types::EntryEither<DeviceId<BC>>> {
    {
        let mut core_ctx = CoreCtx::new_deprecated(core_ctx);
        IpStateContext::<Ipv4, _>::with_ip_routing_table(&mut core_ctx, |core_ctx, ipv4| {
            IpStateContext::<Ipv6, _>::with_ip_routing_table(core_ctx, |_, ipv6| {
                ipv4.iter_table()
                    .cloned()
                    .map(From::from)
                    .chain(ipv6.iter_table().cloned().map(From::from))
                    .collect()
            })
        })
    }
}

/// The metadata associated with an outgoing IP packet.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SendIpPacketMeta<I: packet_formats::ip::IpExt, D, Src> {
    /// The outgoing device.
    pub(crate) device: D,

    /// The source address of the packet.
    pub(crate) src_ip: Src,

    /// The destination address of the packet.
    pub(crate) dst_ip: SpecifiedAddr<I::Addr>,

    /// The next-hop node that the packet should be sent to.
    pub(crate) next_hop: SpecifiedAddr<I::Addr>,

    /// The upper-layer protocol held in the packet's payload.
    pub(crate) proto: I::Proto,

    /// The time-to-live (IPv4) or hop limit (IPv6) for the packet.
    ///
    /// If not set, a default TTL may be used.
    pub(crate) ttl: Option<NonZeroU8>,

    /// An MTU to artificially impose on the whole IP packet.
    ///
    /// Note that the device's MTU will still be imposed on the packet.
    pub(crate) mtu: Option<u32>,
}

impl<I: packet_formats::ip::IpExt, D> From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>
    for SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>>
{
    fn from(
        SendIpPacketMeta { device, src_ip, dst_ip, next_hop, proto, ttl, mtu }: SendIpPacketMeta<
            I,
            D,
            SpecifiedAddr<I::Addr>,
        >,
    ) -> SendIpPacketMeta<I, D, Option<SpecifiedAddr<I::Addr>>> {
        SendIpPacketMeta { device, src_ip: Some(src_ip), dst_ip, next_hop, proto, ttl, mtu }
    }
}

pub(crate) trait IpLayerHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    fn send_ip_packet_from_device<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &Self::DeviceId, Option<SpecifiedAddr<I::Addr>>>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

impl<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: IpDeviceStateContext<I, BC> + IpDeviceSendContext<I, BC> + NonTestCtxMarker,
    > IpLayerHandler<I, BC> for CC
{
    fn send_ip_packet_from_device<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &CC::DeviceId, Option<SpecifiedAddr<I::Addr>>>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        send_ip_packet_from_device(self, bindings_ctx, meta, body)
    }
}

/// Sends an Ip packet with the specified metadata.
///
/// # Panics
///
/// Panics if either the source or destination address is the loopback address
/// and the device is a non-loopback device.
pub(crate) fn send_ip_packet_from_device<I, BC, CC, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    meta: SendIpPacketMeta<
        I,
        &<CC as DeviceIdContext<AnyDevice>>::DeviceId,
        Option<SpecifiedAddr<I::Addr>>,
    >,
    body: S,
) -> Result<(), S>
where
    I: IpLayerIpExt,
    BC: IpLayerBindingsContext<I, <CC as DeviceIdContext<AnyDevice>>::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + IpDeviceSendContext<I, BC>,
    S: Serializer,
    S::Buffer: BufferMut,
{
    let SendIpPacketMeta { device, src_ip, dst_ip, next_hop, proto, ttl, mtu } = meta;
    let next_packet_id = gen_ip_packet_id(core_ctx);
    let ttl = ttl.unwrap_or_else(|| core_ctx.get_hop_limit(device)).get();
    let src_ip = src_ip.map_or(I::UNSPECIFIED_ADDRESS, |a| a.get());
    assert!(
        (!I::LOOPBACK_SUBNET.contains(&src_ip) && !I::LOOPBACK_SUBNET.contains(&dst_ip))
            || device.is_loopback()
    );
    let mut builder = I::PacketBuilder::new(src_ip, dst_ip.get(), ttl, proto);

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Wrap<'a, I: IpLayerIpExt> {
        builder: &'a mut I::PacketBuilder,
        next_packet_id: I::PacketId,
    }

    I::map_ip::<_, ()>(
        Wrap { builder: &mut builder, next_packet_id },
        |Wrap { builder, next_packet_id }| {
            builder.id(next_packet_id);
        },
        |Wrap { builder: _, next_packet_id: () }| {
            // IPv6 doesn't have packet IDs.
        },
    );

    let body = body.encapsulate(builder);

    if let Some(mtu) = mtu {
        let body = body.with_size_limit(mtu as usize);
        core_ctx
            .send_ip_frame(bindings_ctx, device, next_hop, body)
            .map_err(|ser| ser.into_inner().into_inner())
    } else {
        core_ctx.send_ip_frame(bindings_ctx, device, next_hop, body).map_err(|ser| ser.into_inner())
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv4>>,
    > InnerIcmpContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv4>>;
    type DualStackContext = UninstantiableWrapper<Self>;
    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv4Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_proto: Ipv4Proto,
        original_body: &[u8],
        err: Icmpv4ErrorCode,
    ) {
        self.with_counters(|counters: &IpCounters<Ipv4>| {
            counters.receive_icmp_error.increment();
        });
        trace!("InnerIcmpContext<Ipv4>::receive_icmp_error({:?})", err);

        match original_proto {
            Ipv4Proto::Icmp => {
                <IcmpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv4Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_icmp_sockets<
        O,
        F: FnOnce(&icmp::socket::BoundSockets<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::IcmpBoundMap<Ipv4>>())
    }

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::socket::BoundSockets<Ipv4, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv4>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv4>>())
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpBoundMap<Ipv6>>
            + LockBefore<crate::lock_ordering::TcpAllSocketsSet<Ipv6>>
            + LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv6>>,
    > InnerIcmpContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpSocketsCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpBoundMap<Ipv6>>;
    type DualStackContext = UninstantiableWrapper<Self>;
    fn receive_icmp_error(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        original_src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        original_dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_next_header: Ipv6Proto,
        original_body: &[u8],
        err: Icmpv6ErrorCode,
    ) {
        self.with_counters(|counters: &IpCounters<Ipv6>| {
            counters.receive_icmp_error.increment();
        });
        trace!("InnerIcmpContext<Ipv6>::receive_icmp_error({:?})", err);

        match original_next_header {
            Ipv6Proto::Icmpv6 => {
                <IcmpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Tcp) => {
                <TcpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            Ipv6Proto::Proto(IpProto::Udp) => {
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    self,
                    bindings_ctx,
                    device,
                    original_src_ip,
                    original_dst_ip,
                    original_body,
                    err,
                )
            }
            // TODO(joshlf): Once all IP protocol numbers are covered,
            // remove this default case.
            _ => <() as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                self,
                bindings_ctx,
                device,
                original_src_ip,
                original_dst_ip,
                original_body,
                err,
            ),
        }
    }

    fn with_icmp_sockets<
        O,
        F: FnOnce(&icmp::socket::BoundSockets<Ipv6, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        cb(&self.read_lock::<crate::lock_ordering::IcmpBoundMap<Ipv6>>())
    }

    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut icmp::socket::BoundSockets<Ipv6, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpBoundMap<Ipv6>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<BC::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.lock::<crate::lock_ordering::IcmpTokenBucket<Ipv6>>())
    }
}

impl<L, BC: BindingsContext> icmp::IcmpStateContext for CoreCtx<'_, BC, L> {}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpSocketsTable<Ipv6>>
            + LockBefore<crate::lock_ordering::TcpDemux<Ipv6>>
            + LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv6>>,
    > icmp::socket::StateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpSocketsTable<Ipv6>>;

    fn with_sockets_state<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &icmp::socket::SocketsState<Ipv6, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (sockets, mut core_ctx) =
            self.read_lock_and::<crate::lock_ordering::IcmpSocketsTable<Ipv6>>();
        cb(&mut core_ctx, &sockets)
    }

    fn with_sockets_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &mut icmp::socket::SocketsState<Ipv6, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpSocketsTable<Ipv6>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }
}

impl<
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::IcmpSocketsTable<Ipv4>>
            + LockBefore<crate::lock_ordering::TcpDemux<Ipv4>>
            + LockBefore<crate::lock_ordering::UdpSocketsTable<Ipv4>>,
    > icmp::socket::StateContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type SocketStateCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IcmpSocketsTable<Ipv4>>;

    fn with_sockets_state<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &icmp::socket::SocketsState<Ipv4, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (sockets, mut core_ctx) =
            self.read_lock_and::<crate::lock_ordering::IcmpSocketsTable<Ipv4>>();
        cb(&mut core_ctx, &sockets)
    }

    fn with_sockets_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketStateCtx<'_>,
            &mut icmp::socket::SocketsState<Ipv4, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (mut sockets, mut core_ctx) =
            self.write_lock_and::<crate::lock_ordering::IcmpSocketsTable<Ipv4>>();
        cb(&mut core_ctx, &mut sockets)
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        cb(&mut self.cast_locked())
    }
}

/// Resolve the route to a given destination.
///
/// Returns `Some` [`ResolvedRoute`] with details for reaching the destination,
/// or `None` if the destination is unreachable.
pub fn resolve_route<I: Ip, BC: BindingsContext>(
    core_ctx: &SyncCtx<BC>,
    destination: I::Addr,
) -> Result<ResolvedRoute<I, DeviceId<BC>>, ResolveRouteError> {
    let core_ctx = CoreCtx::new_deprecated(core_ctx);
    let destination = SpecifiedAddr::new(destination);
    I::map_ip(
        (IpInvariant(core_ctx), destination),
        |(IpInvariant(mut core_ctx), destination)| {
            resolve_route_to_destination::<Ipv4, _, _>(&mut core_ctx, None, None, destination)
        },
        |(IpInvariant(mut core_ctx), destination)| {
            resolve_route_to_destination::<Ipv6, _, _>(&mut core_ctx, None, None, destination)
        },
    )
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use alloc::collections::HashSet;

    use net_types::{ip::IpAddr, MulticastAddr};

    use crate::{
        context::{testutil::FakeInstant, RngContext},
        device::testutil::{FakeStrongDeviceId, FakeWeakDeviceId},
        testutil::{FakeBindingsCtx, FakeCoreCtx},
    };

    impl<S: AsRef<FakeIpDeviceIdCtx<D>>, Meta, D: StrongId + 'static> DeviceIdContext<AnyDevice>
        for crate::context::testutil::FakeCoreCtx<S, Meta, D>
    where
        FakeIpDeviceIdCtx<D>: DeviceIdContext<AnyDevice, DeviceId = D, WeakDeviceId = D::Weak>,
    {
        type DeviceId = D;
        type WeakDeviceId = D::Weak;

        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            self.get_ref().as_ref().downgrade_device_id(device_id)
        }

        fn upgrade_weak_device_id(&self, device_id: &Self::WeakDeviceId) -> Option<Self::DeviceId> {
            self.get_ref().as_ref().upgrade_weak_device_id(device_id)
        }
    }

    impl<Outer, Inner: DeviceIdContext<AnyDevice>> DeviceIdContext<AnyDevice>
        for crate::context::testutil::Wrapped<Outer, Inner>
    {
        type DeviceId = Inner::DeviceId;
        type WeakDeviceId = Inner::WeakDeviceId;

        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            self.inner.downgrade_device_id(device_id)
        }

        fn upgrade_weak_device_id(&self, device_id: &Self::WeakDeviceId) -> Option<Self::DeviceId> {
            self.inner.upgrade_weak_device_id(device_id)
        }
    }

    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip()]
    pub(crate) enum DualStackSendIpPacketMeta<D> {
        V4(SendIpPacketMeta<Ipv4, D, SpecifiedAddr<Ipv4Addr>>),
        V6(SendIpPacketMeta<Ipv6, D, SpecifiedAddr<Ipv6Addr>>),
    }

    impl<I: packet_formats::ip::IpExt, D> From<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>
        for DualStackSendIpPacketMeta<D>
    {
        fn from(value: SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>) -> Self {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<I: packet_formats::ip::IpExt, D>(
                SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>,
            );
            use DualStackSendIpPacketMeta::*;
            let IpInvariant(dual_stack) = I::map_ip(
                Wrap(value),
                |Wrap(value)| IpInvariant(V4(value)),
                |Wrap(value)| IpInvariant(V6(value)),
            );
            dual_stack
        }
    }

    #[cfg(test)]
    impl<I: packet_formats::ip::IpExt, S, Id, Event: Debug, DeviceId, BindingsCtxState>
        crate::context::SendFrameContext<
            crate::context::testutil::FakeBindingsCtx<Id, Event, BindingsCtxState>,
            SendIpPacketMeta<I, DeviceId, SpecifiedAddr<I::Addr>>,
        >
        for crate::context::testutil::FakeCoreCtx<S, DualStackSendIpPacketMeta<DeviceId>, DeviceId>
    {
        fn send_frame<SS>(
            &mut self,
            bindings_ctx: &mut crate::context::testutil::FakeBindingsCtx<
                Id,
                Event,
                BindingsCtxState,
            >,
            metadata: SendIpPacketMeta<I, DeviceId, SpecifiedAddr<I::Addr>>,
            frame: SS,
        ) -> Result<(), SS>
        where
            SS: Serializer,
            SS::Buffer: BufferMut,
        {
            self.send_frame(bindings_ctx, DualStackSendIpPacketMeta::from(metadata), frame)
        }
    }

    #[derive(Debug)]
    pub(crate) struct WrongIpVersion;

    impl<D> DualStackSendIpPacketMeta<D> {
        pub(crate) fn try_as<I: packet_formats::ip::IpExt>(
            &self,
        ) -> Result<&SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>, WrongIpVersion> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: packet_formats::ip::IpExt, D>(
                Option<&'a SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>>,
            );
            use DualStackSendIpPacketMeta::*;
            let Wrap(dual_stack) = I::map_ip(
                self,
                |value| {
                    Wrap(match value {
                        V4(meta) => Some(meta),
                        V6(_) => None,
                    })
                },
                |value| {
                    Wrap(match value {
                        V4(_) => None,
                        V6(meta) => Some(meta),
                    })
                },
            );
            dual_stack.ok_or(WrongIpVersion)
        }
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeIpDeviceIdCtx<D> {
        devices_removed: HashSet<D>,
    }

    impl<D: Eq + Hash> FakeIpDeviceIdCtx<D> {
        pub(crate) fn set_device_removed(&mut self, device: D, removed: bool) {
            let Self { devices_removed } = self;
            let _existed: bool = if removed {
                devices_removed.insert(device)
            } else {
                devices_removed.remove(&device)
            };
        }
    }

    impl<DeviceId: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeIpDeviceIdCtx<DeviceId> {
        type DeviceId = DeviceId;
        type WeakDeviceId = FakeWeakDeviceId<DeviceId>;

        fn downgrade_device_id(&self, device_id: &DeviceId) -> FakeWeakDeviceId<DeviceId> {
            FakeWeakDeviceId(device_id.clone())
        }

        fn upgrade_weak_device_id(
            &self,
            FakeWeakDeviceId(device_id): &FakeWeakDeviceId<DeviceId>,
        ) -> Option<DeviceId> {
            let Self { devices_removed } = self;
            (!devices_removed.contains(&device_id)).then(|| device_id.clone())
        }
    }

    pub(crate) fn is_in_ip_multicast<A: IpAddress>(
        core_ctx: &FakeCoreCtx,
        device: &DeviceId<FakeBindingsCtx>,
        addr: MulticastAddr<A>,
    ) -> bool {
        let mut core_ctx = CoreCtx::new_deprecated(core_ctx);
        match addr.into() {
            IpAddr::V4(addr) => {
                match IpDeviceStateContext::<Ipv4, _>::address_status_for_device(
                    &mut core_ctx,
                    addr.into_specified(),
                    device,
                ) {
                    AddressStatus::Present(Ipv4PresentAddressStatus::Multicast) => true,
                    AddressStatus::Unassigned => false,
                    AddressStatus::Present(
                        Ipv4PresentAddressStatus::Unicast
                        | Ipv4PresentAddressStatus::LimitedBroadcast
                        | Ipv4PresentAddressStatus::SubnetBroadcast,
                    ) => unreachable!(),
                }
            }
            IpAddr::V6(addr) => {
                match IpDeviceStateContext::<Ipv6, _>::address_status_for_device(
                    &mut core_ctx,
                    addr.into_specified(),
                    device,
                ) {
                    AddressStatus::Present(Ipv6PresentAddressStatus::Multicast) => true,
                    AddressStatus::Unassigned => false,
                    AddressStatus::Present(
                        Ipv6PresentAddressStatus::UnicastAssigned
                        | Ipv6PresentAddressStatus::UnicastTentative,
                    ) => unreachable!(),
                }
            }
        }
    }

    impl<
            I: Ip,
            BC: RngContext + InstantContext<Instant = FakeInstant>,
            D: FakeStrongDeviceId,
            State: MulticastMembershipHandler<I, BC, DeviceId = D>,
            Meta,
        > MulticastMembershipHandler<I, BC>
        for crate::context::testutil::FakeCoreCtx<State, Meta, D>
    where
        Self: DeviceIdContext<AnyDevice, DeviceId = D>,
    {
        fn join_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.get_mut().join_multicast_group(bindings_ctx, device, addr)
        }

        fn leave_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            self.get_mut().leave_multicast_group(bindings_ctx, device, addr)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::{convert::TryFrom, num::NonZeroU16, time::Duration};

    use ip_test_macro::ip_test;
    use net_types::{
        ethernet::Mac,
        ip::{AddrSubnet, IpAddr, Ipv4Addr, Ipv6Addr},
        MulticastAddr, UnicastAddr,
    };
    use packet::{Buf, ParseBuffer};
    use packet_formats::{
        ethernet::{
            EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck,
            ETHERNET_MIN_BODY_LEN_NO_TAG,
        },
        icmp::{
            IcmpDestUnreachable, IcmpEchoRequest, IcmpPacketBuilder, IcmpParseArgs, IcmpUnusedCode,
            Icmpv4DestUnreachableCode, Icmpv6Packet, Icmpv6PacketTooBig,
            Icmpv6ParameterProblemCode, MessageBody,
        },
        ip::{IpPacketBuilder, Ipv6ExtHdrType},
        ipv4::Ipv4PacketBuilder,
        ipv6::{ext_hdrs::ExtensionHeaderOptionAction, Ipv6PacketBuilder},
        testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
    };
    use rand::Rng;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{handle_timer_helper_with_sc_ref, FakeInstant, FakeTimerCtxExt as _},
        device::{
            self,
            testutil::{set_forwarding_enabled, update_ipv6_configuration},
            DeviceId, FrameDestination,
        },
        ip::{
            device::{
                slaac::SlaacConfiguration, state::AddrSubnetAndManualConfigEither,
                IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
                Ipv6DeviceConfigurationUpdate,
            },
            testutil::is_in_ip_multicast,
            types::{AddableEntryEither, AddableMetric, RawMetric},
        },
        state::StackState,
        testutil::{
            assert_empty, handle_timer, new_rng, set_logger_for_test, Ctx, FakeBindingsCtx,
            FakeCtx, FakeEventDispatcherBuilder, TestIpExt, DEFAULT_INTERFACE_METRIC,
            FAKE_CONFIG_V4, FAKE_CONFIG_V6, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        UnlockedCoreCtx,
    };

    // Some helper functions

    /// Verify that an ICMP Parameter Problem packet was actually sent in
    /// response to a packet with an unrecognized IPv6 extension header option.
    ///
    /// `verify_icmp_for_unrecognized_ext_hdr_option` verifies that the next
    /// frame in `net` is an ICMP packet with code set to `code`, and pointer
    /// set to `pointer`.
    fn verify_icmp_for_unrecognized_ext_hdr_option(
        bindings_ctx: &mut FakeBindingsCtx,
        code: Icmpv6ParameterProblemCode,
        pointer: u32,
        offset: usize,
    ) {
        // Check the ICMP that bob attempted to send to alice
        let device_frames = bindings_ctx.frames_sent();
        assert!(!device_frames.is_empty());
        let mut buffer = Buf::new(device_frames[offset].1.as_slice(), ..);
        let _frame =
            buffer.parse_with::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::Check).unwrap();
        let packet = buffer.parse::<<Ipv6 as packet_formats::ip::IpExt>::Packet<_>>().unwrap();
        let (src_ip, dst_ip, proto, _): (_, _, _, ParseMetadata) = packet.into_metadata();
        assert_eq!(dst_ip, FAKE_CONFIG_V6.remote_ip.get());
        assert_eq!(src_ip, FAKE_CONFIG_V6.local_ip.get());
        assert_eq!(proto, Ipv6Proto::Icmpv6);
        let icmp =
            buffer.parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(src_ip, dst_ip)).unwrap();
        if let Icmpv6Packet::ParameterProblem(icmp) = icmp {
            assert_eq!(icmp.code(), code);
            assert_eq!(icmp.message().pointer(), pointer);
        } else {
            panic!("Expected ICMPv6 Parameter Problem: {:?}", icmp);
        }
    }

    /// Populate a buffer `bytes` with data required to test unrecognized
    /// options.
    ///
    /// The unrecognized option type will be located at index 48. `bytes` must
    /// be at least 64 bytes long. If `to_multicast` is `true`, the destination
    /// address of the packet will be a multicast address.
    fn buf_for_unrecognized_ext_hdr_option_test(
        bytes: &mut [u8],
        action: ExtensionHeaderOptionAction,
        to_multicast: bool,
    ) -> Buf<&mut [u8]> {
        assert!(bytes.len() >= 64);

        let action: u8 = action.into();

        // Unrecognized Option type.
        let oty = 63 | (action << 6);

        #[rustfmt::skip]
        bytes[40..64].copy_from_slice(&[
            // Destination Options Extension Header
            IpProto::Udp.into(),      // Next Header
            1,                        // Hdr Ext Len (In 8-octet units, not including first 8 octets)
            0,                        // Pad1
            1,   0,                   // Pad2
            1,   1, 0,                // Pad3
            oty, 6, 0, 0, 0, 0, 0, 0, // Unrecognized type w/ action = discard

            // Body
            1, 2, 3, 4, 5, 6, 7, 8
        ][..]);
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);

        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());

        bytes[6] = Ipv6ExtHdrType::DestinationOptions.into();
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());

        if to_multicast {
            bytes[24..40].copy_from_slice(
                &[255, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32][..],
            );
        } else {
            bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        }

        Buf::new(bytes, ..)
    }

    /// Create an IPv4 packet builder.
    fn get_ipv4_builder() -> Ipv4PacketBuilder {
        Ipv4PacketBuilder::new(
            FAKE_CONFIG_V4.remote_ip,
            FAKE_CONFIG_V4.local_ip,
            10,
            IpProto::Udp.into(),
        )
    }

    /// Process an IP fragment depending on the `Ip` `process_ip_fragment` is
    /// specialized with.
    fn process_ip_fragment<I: Ip, BC: BindingsContext>(
        core_ctx: &mut &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        match I::VERSION {
            IpVersion::V4 => process_ipv4_fragment(
                core_ctx,
                bindings_ctx,
                device,
                fragment_id,
                fragment_offset,
                fragment_count,
            ),
            IpVersion::V6 => process_ipv6_fragment(
                core_ctx,
                bindings_ctx,
                device,
                fragment_id,
                fragment_offset,
                fragment_count,
            ),
        }
    }

    /// Generate and 'receive' an IPv4 fragment packet.
    ///
    /// `fragment_offset` is the fragment offset. `fragment_count` is the number
    /// of fragments for a packet. The generated packet will have a body of size
    /// 8 bytes.
    fn process_ipv4_fragment<BC: BindingsContext>(
        core_ctx: &mut &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        assert!(fragment_offset < fragment_count);

        let m_flag = fragment_offset < (fragment_count - 1);

        let mut builder = get_ipv4_builder();
        builder.id(fragment_id);
        builder.fragment_offset(fragment_offset as u16);
        builder.mf_flag(m_flag);
        let mut body: Vec<u8> = Vec::new();
        body.extend(fragment_offset * 8..fragment_offset * 8 + 8);
        let buffer =
            Buf::new(body, ..).encapsulate(builder).serialize_vec_outer().unwrap().into_inner();
        receive_ip_packet::<_, _, Ipv4>(
            core_ctx,
            bindings_ctx,
            device,
            FrameDestination::Individual { local: true },
            buffer,
        );
    }

    /// Generate and 'receive' an IPv6 fragment packet.
    ///
    /// `fragment_offset` is the fragment offset. `fragment_count` is the number
    /// of fragments for a packet. The generated packet will have a body of size
    /// 8 bytes.
    fn process_ipv6_fragment<BC: BindingsContext>(
        core_ctx: &mut &SyncCtx<BC>,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        fragment_id: u16,
        fragment_offset: u8,
        fragment_count: u8,
    ) {
        assert!(fragment_offset < fragment_count);

        let m_flag = fragment_offset < (fragment_count - 1);

        let mut bytes = vec![0; 48];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        bytes[6] = Ipv6ExtHdrType::Fragment.into(); // Next Header
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        bytes[40] = IpProto::Udp.into();
        bytes[42] = fragment_offset >> 5;
        bytes[43] = ((fragment_offset & 0x1F) << 3) | if m_flag { 1 } else { 0 };
        bytes[44..48].copy_from_slice(&(u32::try_from(fragment_id).unwrap().to_be_bytes()));
        bytes.extend(fragment_offset * 8..fragment_offset * 8 + 8);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        let buffer = Buf::new(bytes, ..);
        receive_ip_packet::<_, _, Ipv6>(
            core_ctx,
            bindings_ctx,
            device,
            FrameDestination::Individual { local: true },
            buffer,
        );
    }

    #[test]
    fn test_ipv6_icmp_parameter_problem_non_must() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();

        // Test parsing an IPv6 packet with invalid next header value which
        // we SHOULD send an ICMP response for (but we don't since its not a
        // MUST).

        #[rustfmt::skip]
        let bytes: &mut [u8] = &mut [
            // FixedHeader (will be replaced later)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,

            // Body
            1, 2, 3, 4, 5,
        ][..];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        bytes[6] = 255; // Invalid Next Header
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        let buf = Buf::new(bytes, ..);

        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            FrameDestination::Individual { local: true },
            buf,
        );

        assert_eq!(core_ctx.state.ipv4.icmp_tx_counters().parameter_problem.get(), 0);
        assert_eq!(core_ctx.state.ipv6.icmp_tx_counters().parameter_problem.get(), 0);
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 0);
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);
    }

    #[test]
    fn test_ipv6_icmp_parameter_problem_must() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();

        // Test parsing an IPv6 packet where we MUST send an ICMP parameter problem
        // response (invalid routing type for a routing extension header).

        #[rustfmt::skip]
        let bytes: &mut [u8] = &mut [
            // FixedHeader (will be replaced later)
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,

            // Routing Extension Header
            IpProto::Udp.into(),         // Next Header
            4,                                  // Hdr Ext Len (In 8-octet units, not including first 8 octets)
            255,                                // Routing Type (Invalid)
            1,                                  // Segments Left
            0, 0, 0, 0,                         // Reserved
            // Addresses for Routing Header w/ Type 0
            0,  1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12, 13, 14, 15,
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,

            // Body
            1, 2, 3, 4, 5,
        ][..];
        bytes[..4].copy_from_slice(&[0x60, 0x20, 0x00, 0x77][..]);
        let payload_len = u16::try_from(bytes.len() - 40).unwrap();
        bytes[4..6].copy_from_slice(&payload_len.to_be_bytes());
        bytes[6] = Ipv6ExtHdrType::Routing.into();
        bytes[7] = 64;
        bytes[8..24].copy_from_slice(FAKE_CONFIG_V6.remote_ip.bytes());
        bytes[24..40].copy_from_slice(FAKE_CONFIG_V6.local_ip.bytes());
        let buf = Buf::new(bytes, ..);
        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            FrameDestination::Individual { local: true },
            buf,
        );
        assert_eq!(bindings_ctx.frames_sent().len(), 1);
        verify_icmp_for_unrecognized_ext_hdr_option(
            &mut bindings_ctx,
            Icmpv6ParameterProblemCode::ErroneousHeaderField,
            42,
            0,
        );
    }

    #[test]
    fn test_ipv6_unrecognized_ext_hdr_option() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V6).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let mut expected_icmps = 0;
        let mut bytes = [0; 64];
        let frame_dst = FrameDestination::Individual { local: true };

        // Test parsing an IPv6 packet where we MUST send an ICMP parameter
        // problem due to an unrecognized extension header option.

        // Test with unrecognized option type set with action = skip & continue.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::SkipAndContinue,
            false,
        );
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);

        // Test with unrecognized option type set with
        // action = discard.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacket,
            false,
        );
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);

        // Test with unrecognized option type set with
        // action = discard & send icmp
        // where dest addr is a unicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmp,
            false,
        );
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        expected_icmps += 1;
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);
        verify_icmp_for_unrecognized_ext_hdr_option(
            &mut bindings_ctx,
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            usize::try_from(expected_icmps).unwrap() - 1,
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp
        // where dest addr is a multicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmp,
            true,
        );
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        expected_icmps += 1;
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);
        verify_icmp_for_unrecognized_ext_hdr_option(
            &mut bindings_ctx,
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            usize::try_from(expected_icmps).unwrap() - 1,
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp if not multicast addr
        // where dest addr is a unicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmpNoMulticast,
            false,
        );
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        expected_icmps += 1;
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);
        verify_icmp_for_unrecognized_ext_hdr_option(
            &mut bindings_ctx,
            Icmpv6ParameterProblemCode::UnrecognizedIpv6Option,
            48,
            usize::try_from(expected_icmps).unwrap() - 1,
        );

        // Test with unrecognized option type set with
        // action = discard & send icmp if not multicast addr
        // but dest addr is a multicast addr.

        let buf = buf_for_unrecognized_ext_hdr_option_test(
            &mut bytes,
            ExtensionHeaderOptionAction::DiscardPacketSendIcmpNoMulticast,
            true,
        );
        // Do not expect an ICMP response for this packet
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        assert_eq!(
            core_ctx.state.icmp_tx_counters::<Ipv6>().parameter_problem.get(),
            expected_icmps
        );
        assert_eq!(u64::try_from(bindings_ctx.frames_sent().len()).unwrap(), expected_icmps);

        // None of our tests should have sent an icmpv4 packet, or dispatched an
        // IP packet after the first.

        assert_eq!(core_ctx.state.icmp_tx_counters::<Ipv4>().parameter_problem.get(), 0);
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_not_needed<I: Ip + TestIpExt>() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let mut core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Test that a non fragmented packet gets dispatched right away.

        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 0, 1);

        // Make sure the packet got dispatched.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly<I: Ip + TestIpExt>() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let mut core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        // Test that the received packet gets dispatched only after receiving
        // all the fragments.

        // Process fragment #0
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 0, 3);

        // Process fragment #1
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 1, 3);

        // Make sure no packets got dispatched yet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Process fragment #2
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 2, 3);

        // Make sure the packet finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_with_packets_arriving_out_of_order<I: Ip + TestIpExt>() {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let mut core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id_0 = 5;
        let fragment_id_1 = 10;
        let fragment_id_2 = 15;

        // Test that received packets gets dispatched only after receiving all
        // the fragments with out of order arrival of fragments.

        // Process packet #0, fragment #1
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_0, 1, 3);

        // Process packet #1, fragment #2
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_1, 2, 3);

        // Process packet #1, fragment #0
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_1, 0, 3);

        // Make sure no packets got dispatched yet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Process a packet that does not require reassembly (packet #2, fragment #0).
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_2, 0, 1);

        // Make packet #1 got dispatched since it didn't need reassembly.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Process packet #0, fragment #2
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_0, 2, 3);

        // Make sure no other packets got dispatched yet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Process packet #0, fragment #0
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_0, 0, 3);

        // Make sure that packet #0 finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);

        // Process packet #1, fragment #1
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id_1, 1, 3);

        // Make sure the packet finally got dispatched now that the final
        // fragment has been 'received'.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 3);
    }

    #[ip_test]
    fn test_ip_packet_reassembly_timer<I: Ip + TestIpExt>()
    where
        IpLayerTimerId: From<FragmentCacheKey<I::Addr>>,
    {
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(I::FAKE_CONFIG).build();
        let mut core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let fragment_id = 5;

        // Test to make sure that packets must arrive within the reassembly
        // timer.

        // Process fragment #0
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 0, 3);

        // Make sure a timer got added.
        bindings_ctx.timer_ctx().assert_timers_installed([(
            IpLayerTimerId::from(FragmentCacheKey::new(
                I::FAKE_CONFIG.remote_ip.get(),
                I::FAKE_CONFIG.local_ip.get(),
                fragment_id.into(),
            ))
            .into(),
            ..,
        )]);

        // Process fragment #1
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 1, 3);

        // Trigger the timer (simulate a timer for the fragmented packet)
        let key = FragmentCacheKey::new(
            I::FAKE_CONFIG.remote_ip.get(),
            I::FAKE_CONFIG.local_ip.get(),
            u32::from(fragment_id),
        );
        assert_eq!(
            bindings_ctx.trigger_next_timer(core_ctx, crate::handle_timer).unwrap(),
            IpLayerTimerId::from(key).into(),
        );

        // Make sure no other timers exist.
        bindings_ctx.timer_ctx().assert_no_timers_installed();

        // Process fragment #2
        process_ip_fragment::<I, _>(&mut core_ctx, &mut bindings_ctx, &device, fragment_id, 2, 3);

        // Make sure no packets got dispatched yet since even though we
        // technically received all the fragments, this fragment (#2) arrived
        // too late and the reassembly timer was triggered, causing the prior
        // fragment data to be discarded.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);
    }

    #[ip_test]
    fn test_ip_reassembly_only_at_destination_host<I: Ip + TestIpExt>() {
        // Create a new network with two parties (alice & bob) and enable IP
        // packet routing for alice.
        let a = "alice";
        let b = "bob";
        let fake_config = I::FAKE_CONFIG;
        let (mut alice, alice_device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.swap()).build();
        {
            let Ctx { core_ctx, bindings_ctx } = &mut alice;
            set_forwarding_enabled::<_, I>(
                core_ctx,
                bindings_ctx,
                &alice_device_ids[0].clone().into(),
                true,
            )
            .expect("qerror setting routing enabled");
        }
        let (bob, bob_device_ids) = FakeEventDispatcherBuilder::from_config(fake_config).build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            a,
            alice,
            alice_device_ids[0].downgrade(),
            b,
            bob,
            bob_device_ids[0].downgrade(),
        );
        // Make sure the (strongly referenced) device IDs are dropped before
        // `net`.
        let alice_device_id: DeviceId<_> = alice_device_ids[0].clone().into();
        core::mem::drop((alice_device_ids, bob_device_ids));

        let fragment_id = 5;

        // Test that packets only get reassembled and dispatched at the
        // destination. In this test, Alice is receiving packets from some
        // source that is actually destined for Bob. Alice should simply forward
        // the packets without attempting to process or reassemble the
        // fragments.

        // Process fragment #0
        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            process_ip_fragment::<I, _>(
                &mut &*core_ctx,
                bindings_ctx,
                &alice_device_id,
                fragment_id,
                0,
                3,
            );
        });
        // Make sure the packet got sent from alice to bob
        assert!(!net.step(device::testutil::receive_frame, handle_timer).is_idle());

        // Process fragment #1
        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            process_ip_fragment::<I, _>(
                &mut &*core_ctx,
                bindings_ctx,
                &alice_device_id,
                fragment_id,
                1,
                3,
            );
        });
        assert!(!net.step(device::testutil::receive_frame, handle_timer).is_idle());

        // Make sure no packets got dispatched yet.
        assert_eq!(
            net.context("alice").core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );
        assert_eq!(
            net.context("bob").core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );

        // Process fragment #2
        net.with_context("alice", |Ctx { core_ctx, bindings_ctx }| {
            process_ip_fragment::<I, _>(
                &mut &*core_ctx,
                bindings_ctx,
                &alice_device_id,
                fragment_id,
                2,
                3,
            );
        });
        assert!(!net.step(device::testutil::receive_frame, handle_timer).is_idle());

        // Make sure the packet finally got dispatched now that the final
        // fragment has been received by bob.
        assert_eq!(
            net.context("alice").core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            0
        );
        assert_eq!(
            net.context("bob").core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(),
            1
        );

        // Make sure there are no more events.
        assert!(net.step(device::testutil::receive_frame, handle_timer).is_idle());
    }

    #[test]
    fn test_ipv6_packet_too_big() {
        // Test sending an IPv6 Packet Too Big Error when receiving a packet
        // that is too big to be forwarded when it isn't destined for the node
        // it arrived at.

        let fake_config = Ipv6::FAKE_CONFIG;
        let mut dispatcher_builder = FakeEventDispatcherBuilder::from_config(fake_config.clone());
        let extra_ip = UnicastAddr::new(Ipv6Addr::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 168, 0, 100,
        ]))
        .unwrap();
        let extra_mac = UnicastAddr::new(Mac::new([12, 13, 14, 15, 16, 17])).unwrap();
        dispatcher_builder.add_ndp_table_entry(0, extra_ip, extra_mac);
        dispatcher_builder.add_ndp_table_entry(
            0,
            extra_mac.to_ipv6_link_local().addr().get(),
            extra_mac,
        );
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) = dispatcher_builder.build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        set_forwarding_enabled::<_, Ipv6>(core_ctx, &mut bindings_ctx, &device, true)
            .expect("error setting routing enabled");
        let frame_dst = FrameDestination::Individual { local: true };

        // Construct an IPv6 packet that is too big for our MTU (MTU = 1280;
        // body itself is 5000). Note, the final packet will be larger because
        // of IP header data.
        let mut rng = new_rng(70812476915813);
        let body: Vec<u8> = core::iter::repeat_with(|| rng.gen()).take(5000).collect();

        // Ip packet from some node destined to a remote on this network,
        // arriving locally.
        let mut ipv6_packet_buf = Buf::new(body, ..)
            .encapsulate(Ipv6PacketBuilder::new(
                extra_ip,
                fake_config.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();
        // Receive the IP packet.
        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            ipv6_packet_buf.clone(),
        );

        // Should not have dispatched the packet.
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);
        assert_eq!(core_ctx.state.icmp_tx_counters::<Ipv6>().packet_too_big.get(), 1);

        // Should have sent out one frame though.
        assert_eq!(bindings_ctx.frames_sent().len(), 1);

        // Received packet should be a Packet Too Big ICMP error message.
        let buf = &bindings_ctx.frames_sent()[0].1[..];
        // The original packet's TTL gets decremented so we decrement here
        // to validate the rest of the icmp message body.
        let ipv6_packet_buf_mut: &mut [u8] = ipv6_packet_buf.as_mut();
        ipv6_packet_buf_mut[7] -= 1;
        let (_, _, _, _, _, message, code) =
            parse_icmp_packet_in_ip_packet_in_ethernet_frame::<Ipv6, _, Icmpv6PacketTooBig, _>(
                buf,
                EthernetFrameLengthCheck::NoCheck,
                move |packet| {
                    // Size of the ICMP message body should be size of the
                    // MTU without IP and ICMP headers.
                    let expected_len = 1280 - 48;
                    let actual_body: &[u8] = ipv6_packet_buf.as_ref();
                    let actual_body = &actual_body[..expected_len];
                    assert_eq!(packet.body().len(), expected_len);
                    assert_eq!(packet.body().bytes(), actual_body);
                },
            )
            .unwrap();
        assert_eq!(code, IcmpUnusedCode);
        // MTU should match the MTU for the link.
        assert_eq!(message, Icmpv6PacketTooBig::new(1280));
    }

    fn create_packet_too_big_buf<A: IpAddress>(
        src_ip: A,
        dst_ip: A,
        mtu: u16,
        body: Option<Buf<Vec<u8>>>,
    ) -> Buf<Vec<u8>> {
        let body = body.unwrap_or_else(|| Buf::new(Vec::new(), ..));

        match [src_ip, dst_ip].into() {
            IpAddr::V4([src_ip, dst_ip]) => body
                .encapsulate(IcmpPacketBuilder::<Ipv4, IcmpDestUnreachable>::new(
                    dst_ip,
                    src_ip,
                    Icmpv4DestUnreachableCode::FragmentationRequired,
                    NonZeroU16::new(mtu)
                        .map(IcmpDestUnreachable::new_for_frag_req)
                        .unwrap_or_else(Default::default),
                ))
                .encapsulate(Ipv4PacketBuilder::new(src_ip, dst_ip, 64, Ipv4Proto::Icmp))
                .serialize_vec_outer()
                .unwrap(),
            IpAddr::V6([src_ip, dst_ip]) => body
                .encapsulate(IcmpPacketBuilder::<Ipv6, Icmpv6PacketTooBig>::new(
                    dst_ip,
                    src_ip,
                    IcmpUnusedCode,
                    Icmpv6PacketTooBig::new(u32::from(mtu)),
                ))
                .encapsulate(Ipv6PacketBuilder::new(src_ip, dst_ip, 64, Ipv6Proto::Icmpv6))
                .serialize_vec_outer()
                .unwrap(),
        }
        .into_inner()
    }

    trait GetPmtuIpExt: Ip {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Self::Addr,
            remote_ip: Self::Addr,
        ) -> Option<Mtu>;
    }

    impl GetPmtuIpExt for Ipv4 {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Ipv4Addr,
            remote_ip: Ipv4Addr,
        ) -> Option<Mtu> {
            state.ipv4.inner.pmtu_cache.lock().get_pmtu(local_ip, remote_ip)
        }
    }

    impl GetPmtuIpExt for Ipv6 {
        fn get_pmtu<BC: BindingsContext>(
            state: &StackState<BC>,
            local_ip: Ipv6Addr,
            remote_ip: Ipv6Addr,
        ) -> Option<Mtu> {
            state.ipv6.inner.pmtu_cache.lock().get_pmtu(local_ip, remote_ip)
        }
    }

    #[ip_test]
    fn test_ip_update_pmtu<I: Ip + TestIpExt + GetPmtuIpExt>() {
        // Test receiving a Packet Too Big (IPv6) or Dest Unreachable
        // Fragmentation Required (IPv4) which should update the PMTU if it is
        // less than the current value.

        let fake_config = I::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update PMTU from None.

        let new_mtu1 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 100);

        // Create ICMP IP buf
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu1)).unwrap(),
            None,
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, I>(&core_ctx, &mut bindings_ctx, &device, frame_dst, packet_buf);

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        assert_eq!(
            I::get_pmtu(&core_ctx.state, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu1
        );

        // Don't update PMTU when current PMTU is less than reported MTU.

        let new_mtu2 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 200);

        // Create IPv6 ICMPv6 packet too big packet with MTU larger than current
        // PMTU.
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu2)).unwrap(),
            None,
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, I>(&core_ctx, &mut bindings_ctx, &device, frame_dst, packet_buf);

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 2);

        // The PMTU should not have updated to `new_mtu2`
        assert_eq!(
            I::get_pmtu(&core_ctx.state, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu1
        );

        // Update PMTU when current PMTU is greater than the reported MTU.

        let new_mtu3 = Mtu::new(u32::from(I::MINIMUM_LINK_MTU) + 50);

        // Create IPv6 ICMPv6 packet too big packet with MTU smaller than
        // current PMTU.
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(u32::from(new_mtu3)).unwrap(),
            None,
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, I>(&core_ctx, &mut bindings_ctx, &device, frame_dst, packet_buf);

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 3);

        // The PMTU should have updated to 1900.
        assert_eq!(
            I::get_pmtu(&core_ctx.state, fake_config.local_ip.get(), fake_config.remote_ip.get())
                .unwrap(),
            new_mtu3
        );
    }

    #[ip_test]
    fn test_ip_update_pmtu_too_low<I: Ip + TestIpExt + GetPmtuIpExt>() {
        // Test receiving a Packet Too Big (IPv6) or Dest Unreachable
        // Fragmentation Required (IPv4) which should not update the PMTU if it
        // is less than the min MTU.

        let fake_config = I::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update PMTU from None but with an MTU too low.

        let new_mtu1 = u32::from(I::MINIMUM_LINK_MTU) - 1;

        // Create ICMP IP buf
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            u16::try_from(new_mtu1).unwrap(),
            None,
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, I>(&core_ctx, &mut bindings_ctx, &device, frame_dst, packet_buf);

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        assert_eq!(
            I::get_pmtu(&core_ctx.state, fake_config.local_ip.get(), fake_config.remote_ip.get()),
            None
        );
    }

    /// Create buffer to be used as the ICMPv4 message body
    /// where the original packet's body  length is `body_len`.
    fn create_orig_packet_buf(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, body_len: usize) -> Buf<Vec<u8>> {
        Buf::new(vec![0; body_len], ..)
            .encapsulate(Ipv4PacketBuilder::new(src_ip, dst_ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner()
    }

    #[test]
    fn test_ipv4_remote_no_rfc1191() {
        // Test receiving an IPv4 Dest Unreachable Fragmentation
        // Required from a node that does not implement RFC 1191.

        let fake_config = Ipv4::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(fake_config.clone()).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        // Update from None.

        // Create ICMP IP buf w/ orig packet body len = 500; orig packet len =
        // 520
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0, // A 0 value indicates that the source of the
            // ICMP message does not implement RFC 1191.
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 500)
                .into(),
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, Ipv4>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            packet_buf,
        );

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Should have decreased PMTU value to the next lower PMTU
        // plateau from `crate::ip::path_mtu::PMTU_PLATEAUS`.
        assert_eq!(
            Ipv4::get_pmtu(
                &core_ctx.state,
                fake_config.local_ip.get(),
                fake_config.remote_ip.get()
            )
            .unwrap(),
            Mtu::new(508),
        );

        // Don't Update when packet size is too small.

        // Create ICMP IP buf w/ orig packet body len = 1; orig packet len = 21
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0,
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 1)
                .into(),
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, Ipv4>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            packet_buf,
        );

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 2);

        // Should not have updated PMTU as there is no other valid
        // lower PMTU value.
        assert_eq!(
            Ipv4::get_pmtu(
                &core_ctx.state,
                fake_config.local_ip.get(),
                fake_config.remote_ip.get()
            )
            .unwrap(),
            Mtu::new(508),
        );

        // Update to lower PMTU estimate based on original packet size.

        // Create ICMP IP buf w/ orig packet body len = 60; orig packet len = 80
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0,
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 60)
                .into(),
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, Ipv4>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            packet_buf,
        );

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 3);

        // Should have decreased PMTU value to the next lower PMTU
        // plateau from `crate::ip::path_mtu::PMTU_PLATEAUS`.
        assert_eq!(
            Ipv4::get_pmtu(
                &core_ctx.state,
                fake_config.local_ip.get(),
                fake_config.remote_ip.get()
            )
            .unwrap(),
            Mtu::new(68),
        );

        // Should not update PMTU because the next low PMTU from this original
        // packet size is higher than current PMTU.

        // Create ICMP IP buf w/ orig packet body len = 290; orig packet len =
        // 310
        let packet_buf = create_packet_too_big_buf(
            fake_config.remote_ip.get(),
            fake_config.local_ip.get(),
            0, // A 0 value indicates that the source of the
            // ICMP message does not implement RFC 1191.
            create_orig_packet_buf(fake_config.local_ip.get(), fake_config.remote_ip.get(), 290)
                .into(),
        );

        // Receive the IP packet.
        receive_ip_packet::<_, _, Ipv4>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            packet_buf,
        );

        // Should have dispatched the packet.
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 4);

        // Should not have updated the PMTU as the current PMTU is lower.
        assert_eq!(
            Ipv4::get_pmtu(
                &core_ctx.state,
                fake_config.local_ip.get(),
                fake_config.remote_ip.get()
            )
            .unwrap(),
            Mtu::new(68),
        );
    }

    #[test]
    fn test_invalid_icmpv4_in_ipv6() {
        let ip_config = Ipv6::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(ip_config.clone()).build();
        let core_ctx = &core_ctx;
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        let ic_config = Ipv4::FAKE_CONFIG;
        let icmp_builder = IcmpPacketBuilder::<Ipv4, _>::new(
            ic_config.remote_ip,
            ic_config.local_ip,
            IcmpUnusedCode,
            IcmpEchoRequest::new(0, 0),
        );

        let ip_builder = Ipv6PacketBuilder::new(
            ip_config.remote_ip,
            ip_config.local_ip,
            64,
            Ipv6Proto::Other(Ipv4Proto::Icmp.into()),
        );

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(icmp_builder)
            .encapsulate(ip_builder)
            .serialize_vec_outer()
            .unwrap();

        crate::device::testutil::enable_device(&core_ctx, &mut bindings_ctx, &device);
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);

        // Should not have dispatched the packet.
        assert_eq!(core_ctx.state.ipv6.inner.counters.receive_ip_packet.get(), 1);
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);

        // In IPv6, the next header value (ICMP(v4)) would have been considered
        // unrecognized so an ICMP parameter problem response SHOULD be sent,
        // but the netstack chooses to just drop the packet since we are not
        // required to send the ICMP response.
        assert_empty(bindings_ctx.frames_sent().iter());
    }

    #[test]
    fn test_invalid_icmpv6_in_ipv4() {
        let ip_config = Ipv4::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(ip_config.clone()).build();
        let core_ctx = &core_ctx;
        // First possible device id.
        let device: DeviceId<_> = device_ids[0].clone().into();
        let frame_dst = FrameDestination::Individual { local: true };

        let ic_config = Ipv6::FAKE_CONFIG;
        let icmp_builder = IcmpPacketBuilder::<Ipv6, _>::new(
            ic_config.remote_ip,
            ic_config.local_ip,
            IcmpUnusedCode,
            IcmpEchoRequest::new(0, 0),
        );

        let ip_builder = Ipv4PacketBuilder::new(
            ip_config.remote_ip,
            ip_config.local_ip,
            64,
            Ipv4Proto::Other(Ipv6Proto::Icmpv6.into()),
        );

        let buf = Buf::new(Vec::new(), ..)
            .encapsulate(icmp_builder)
            .encapsulate(ip_builder)
            .serialize_vec_outer()
            .unwrap();

        receive_ip_packet::<_, _, Ipv4>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);

        // Should have dispatched the packet but resulted in an ICMP error.
        assert_eq!(core_ctx.state.ipv4.inner.counters.dispatch_receive_ip_packet.get(), 1);
        assert_eq!(core_ctx.state.icmp_tx_counters::<Ipv4>().dest_unreachable.get(), 1);
        assert_eq!(bindings_ctx.frames_sent().len(), 1);
        let buf = &bindings_ctx.frames_sent()[0].1[..];
        let (_, _, _, _, _, _, code) = parse_icmp_packet_in_ip_packet_in_ethernet_frame::<
            Ipv4,
            _,
            IcmpDestUnreachable,
            _,
        >(buf, EthernetFrameLengthCheck::NoCheck, |_| {})
        .unwrap();
        assert_eq!(code, Icmpv4DestUnreachableCode::DestProtocolUnreachable);
    }

    #[ip_test]
    fn test_joining_leaving_ip_multicast_group<I: Ip + TestIpExt + packet_formats::ip::IpExt>() {
        // Test receiving a packet destined to a multicast IP (and corresponding
        // multicast MAC).

        let config = I::FAKE_CONFIG;
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) =
            FakeEventDispatcherBuilder::from_config(config.clone()).build();
        let core_ctx = &core_ctx;
        let eth_device = &device_ids[0];
        let device: DeviceId<_> = eth_device.clone().into();
        let multi_addr = I::get_multicast_addr(3).get();
        let dst_mac = Mac::from(&MulticastAddr::new(multi_addr).unwrap());
        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(I::PacketBuilder::new(
                config.remote_ip.get(),
                multi_addr,
                64,
                IpProto::Udp.into(),
            ))
            .encapsulate(EthernetFrameBuilder::new(
                config.remote_mac.get(),
                dst_mac,
                I::ETHER_TYPE,
                ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .into_inner();

        let multi_addr = MulticastAddr::new(multi_addr).unwrap();
        // Should not have dispatched the packet since we are not in the
        // multicast group `multi_addr`.
        assert!(!is_in_ip_multicast(&core_ctx, &device, multi_addr));
        device::receive_frame(&core_ctx, &mut bindings_ctx, &eth_device, buf.clone());
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 0);

        // Join the multicast group and receive the packet, we should dispatch
        // it.
        match multi_addr.into() {
            IpAddr::V4(multicast_addr) => crate::ip::device::join_ip_multicast::<Ipv4, _, _>(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device,
                multicast_addr,
            ),
            IpAddr::V6(multicast_addr) => crate::ip::device::join_ip_multicast::<Ipv6, _, _>(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device,
                multicast_addr,
            ),
        }
        assert!(is_in_ip_multicast(&core_ctx, &device, multi_addr));
        device::receive_frame(&core_ctx, &mut bindings_ctx, &eth_device, buf.clone());
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);

        // Leave the multicast group and receive the packet, we should not
        // dispatch it.
        match multi_addr.into() {
            IpAddr::V4(multicast_addr) => crate::ip::device::leave_ip_multicast::<Ipv4, _, _>(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device,
                multicast_addr,
            ),
            IpAddr::V6(multicast_addr) => crate::ip::device::leave_ip_multicast::<Ipv6, _, _>(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device,
                multicast_addr,
            ),
        }
        assert!(!is_in_ip_multicast(&core_ctx, &device, multi_addr));
        device::receive_frame(&core_ctx, &mut bindings_ctx, &eth_device, buf);
        assert_eq!(core_ctx.state.ip_counters::<I>().dispatch_receive_ip_packet.get(), 1);
    }

    #[test]
    fn test_no_dispatch_non_ndp_packets_during_ndp_dad() {
        // Here we make sure we are not dispatching packets destined to a
        // tentative address (that is performing NDP's Duplicate Address
        // Detection (DAD)) -- IPv6 only.

        let config = Ipv6::FAKE_CONFIG;
        let Ctx { core_ctx, mut bindings_ctx } = crate::testutil::FakeCtx::default();
        let core_ctx = &core_ctx;
        let device = crate::device::add_ethernet_device(
            &core_ctx,
            config.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            core_ctx,
            &mut bindings_ctx,
            &device,
            Ipv6DeviceConfigurationUpdate {
                // Doesn't matter as long as DAD is enabled.
                dad_transmits: Some(NonZeroU8::new(1)),
                // Auto-generate a link-local address.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let frame_dst = FrameDestination::Individual { local: true };

        let ip: Ipv6Addr = config.local_mac.to_ipv6_link_local().addr().get();

        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(config.remote_ip, ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        // Received packet should not have been dispatched.
        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            buf.clone(),
        );
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 0);

        // Wait until DAD is complete. Arbitrarily choose a year in the future
        // as a time after which we're confident DAD will be complete. We can't
        // run until there are no timers because some timers will always exist
        // for background tasks.
        //
        // TODO(https://fxbug.dev/48578): Once this test is contextified, use a
        // more precise condition to ensure that DAD is complete.
        let now = bindings_ctx.now();
        let _: Vec<_> = bindings_ctx.trigger_timers_until_instant(
            now + Duration::from_secs(60 * 60 * 24 * 365),
            handle_timer_helper_with_sc_ref(core_ctx, crate::handle_timer),
        );

        // Received packet should have been dispatched.
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Set the new IP (this should trigger DAD).
        let ip = config.local_ip.get();
        crate::device::add_ip_addr_subnet(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            AddrSubnet::new(ip, 128).unwrap(),
        )
        .unwrap();

        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(config.remote_ip, ip, 64, IpProto::Udp.into()))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        // Received packet should not have been dispatched.
        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            frame_dst,
            buf.clone(),
        );
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 1);

        // Make sure all timers are done (DAD to complete on the interface due
        // to new IP).
        //
        // TODO(https://fxbug.dev/48578): Once this test is contextified, use a
        // more precise condition to ensure that DAD is complete.
        let _: Vec<_> = bindings_ctx.trigger_timers_until_instant(
            FakeInstant::LATEST,
            handle_timer_helper_with_sc_ref(core_ctx, crate::handle_timer),
        );

        // Received packet should have been dispatched.
        receive_ip_packet::<_, _, Ipv6>(&core_ctx, &mut bindings_ctx, &device, frame_dst, buf);
        assert_eq!(core_ctx.state.ipv6.inner.counters.dispatch_receive_ip_packet.get(), 2);
    }

    #[test]
    fn test_drop_non_unicast_ipv6_source() {
        // Test that an inbound IPv6 packet with a non-unicast source address is
        // dropped.
        let cfg = FAKE_CONFIG_V6;
        let (Ctx { core_ctx, mut bindings_ctx }, _device_ids) =
            FakeEventDispatcherBuilder::from_config(cfg.clone()).build();
        let core_ctx = &core_ctx;
        let device = crate::device::add_ethernet_device(
            &core_ctx,
            cfg.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        crate::device::testutil::enable_device(&core_ctx, &mut bindings_ctx, &device);

        let ip: Ipv6Addr = cfg.local_mac.to_ipv6_link_local().addr().get();
        let buf = Buf::new(vec![0; 10], ..)
            .encapsulate(Ipv6PacketBuilder::new(
                Ipv6::MULTICAST_SUBNET.network(),
                ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap()
            .into_inner();

        receive_ip_packet::<_, _, Ipv6>(
            &core_ctx,
            &mut bindings_ctx,
            &device,
            FrameDestination::Individual { local: true },
            buf,
        );
        assert_eq!(core_ctx.state.ipv6.counters.non_unicast_source.get(), 1);
    }

    #[test]
    fn test_receive_ip_packet_action() {
        let v4_config = Ipv4::FAKE_CONFIG;
        let v6_config = Ipv6::FAKE_CONFIG;

        let mut builder = FakeEventDispatcherBuilder::default();
        // Both devices have the same MAC address, which is a bit weird, but not
        // a problem for this test.
        let v4_subnet = AddrSubnet::from_witness(v4_config.local_ip, 16).unwrap().subnet();
        let dev_idx0 =
            builder.add_device_with_ip(v4_config.local_mac, v4_config.local_ip.get(), v4_subnet);
        let dev_idx1 = builder.add_device_with_ip_and_config(
            v6_config.local_mac,
            v6_config.local_ip.get(),
            AddrSubnet::from_witness(v6_config.local_ip, 64).unwrap().subnet(),
            Ipv4DeviceConfigurationUpdate::default(),
            Ipv6DeviceConfigurationUpdate {
                // Auto-generate a link-local address.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) = builder.clone().build();
        let core_ctx = &core_ctx;
        let v4_dev: DeviceId<_> = device_ids[dev_idx0].clone().into();
        let v6_dev: DeviceId<_> = device_ids[dev_idx1].clone().into();

        // Receive packet addressed to us.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                v4_config.local_ip
            ),
            ReceivePacketAction::Deliver
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                v6_config.local_ip
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the IPv4 subnet broadcast address.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                SpecifiedAddr::new(v4_subnet.broadcast()).unwrap()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the IPv4 limited broadcast address.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                Ipv4::LIMITED_BROADCAST_ADDRESS
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a multicast address we're subscribed to.
        crate::ip::device::join_ip_multicast::<Ipv4, _, _>(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &v4_dev,
            Ipv4::ALL_ROUTERS_MULTICAST_ADDRESS,
        );
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                Ipv4::ALL_ROUTERS_MULTICAST_ADDRESS.into_specified()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to the all-nodes multicast address.
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.into_specified()
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a multicast address we're subscribed to.
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                v6_config.local_ip.to_solicited_node_address().into_specified(),
            ),
            ReceivePacketAction::Deliver
        );

        // Receive packet addressed to a tentative address.
        {
            // Construct a one-off context that has DAD enabled. The context
            // built above has DAD disabled, and so addresses start off in the
            // assigned state rather than the tentative state.
            let Ctx { core_ctx, mut bindings_ctx } = FakeCtx::default();
            let core_ctx = &core_ctx;
            let local_mac = v6_config.local_mac;
            let device = crate::device::add_ethernet_device(
                &core_ctx,
                local_mac,
                IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
            let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                core_ctx,
                &mut bindings_ctx,
                &device,
                Ipv6DeviceConfigurationUpdate {
                    // Doesn't matter as long as DAD is enabled.
                    dad_transmits: Some(NonZeroU8::new(1)),
                    // Auto-generate a link-local address.
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            let tentative: UnicastAddr<Ipv6Addr> = local_mac.to_ipv6_link_local().addr().get();
            assert_eq!(
                receive_ipv6_packet_action(
                    &mut CoreCtx::new_deprecated(core_ctx),
                    &mut bindings_ctx,
                    &device,
                    tentative.into_specified()
                ),
                ReceivePacketAction::Drop { reason: DropReason::Tentative }
            );
        }

        // Receive packet destined to a remote address when forwarding is
        // disabled on the inbound interface.
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::Drop { reason: DropReason::ForwardingDisabledInboundIface }
        );

        // Receive packet destined to a remote address when forwarding is
        // enabled both globally and on the inbound device.
        set_forwarding_enabled::<_, Ipv4>(core_ctx, &mut bindings_ctx, &v4_dev, true)
            .expect("error setting routing enabled");
        set_forwarding_enabled::<_, Ipv6>(core_ctx, &mut bindings_ctx, &v6_dev, true)
            .expect("error setting routing enabled");
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::Forward {
                dst: Destination { next_hop: NextHop::RemoteAsNeighbor, device: v4_dev.clone() }
            }
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::Forward {
                dst: Destination { next_hop: NextHop::RemoteAsNeighbor, device: v6_dev.clone() }
            }
        );

        // Receive packet destined to a host with no route when forwarding is
        // enabled both globally and on the inbound device.
        *core_ctx.state.ipv4.inner.table.write() = Default::default();
        *core_ctx.state.ipv6.inner.table.write() = Default::default();
        assert_eq!(
            receive_ipv4_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v4_dev,
                v4_config.remote_ip
            ),
            ReceivePacketAction::SendNoRouteToDest
        );
        assert_eq!(
            receive_ipv6_packet_action(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &v6_dev,
                v6_config.remote_ip
            ),
            ReceivePacketAction::SendNoRouteToDest
        );
    }

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Device {
        First,
        Second,
        Loopback,
    }

    impl Device {
        fn index(self) -> usize {
            match self {
                Self::First => 0,
                Self::Second => 1,
                Self::Loopback => 2,
            }
        }

        fn from_index(index: usize) -> Self {
            match index {
                0 => Self::First,
                1 => Self::Second,
                2 => Self::Loopback,
                x => panic!("index out of bounds: {x}"),
            }
        }

        fn ip_address<A: IpAddress>(self) -> SpecifiedAddr<A>
        where
            A::Version: TestIpExt,
        {
            match self {
                Self::First | Self::Second => <A::Version as TestIpExt>::get_other_ip_address(
                    (self.index() + 1).try_into().unwrap(),
                ),
                Self::Loopback => <A::Version as Ip>::LOOPBACK_ADDRESS,
            }
        }

        fn mac(self) -> UnicastAddr<Mac> {
            UnicastAddr::new(Mac::new([0, 1, 2, 3, 4, self.index().try_into().unwrap()])).unwrap()
        }
    }

    fn remote_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(27)
    }

    fn make_test_ctx<I: Ip + TestIpExt>() -> (Ctx<FakeBindingsCtx>, Vec<DeviceId<FakeBindingsCtx>>)
    {
        let mut builder = FakeEventDispatcherBuilder::default();
        for device in [Device::First, Device::Second] {
            let ip: SpecifiedAddr<I::Addr> = device.ip_address();
            let subnet =
                AddrSubnet::from_witness(ip, <I::Addr as IpAddress>::BYTES * 8).unwrap().subnet();
            let index = builder.add_device_with_ip(device.mac(), ip.get(), subnet);
            assert_eq!(index, device.index());
        }
        let (mut ctx, device_ids) = builder.build();
        let mut device_ids = device_ids.into_iter().map(Into::into).collect::<Vec<_>>();
        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
        let loopback_id = crate::device::add_loopback_device(
            core_ctx,
            Ipv6::MINIMUM_LINK_MTU,
            DEFAULT_INTERFACE_METRIC,
        )
        .unwrap()
        .into();
        crate::device::testutil::enable_device(core_ctx, bindings_ctx, &loopback_id);
        crate::device::add_ip_addr_subnet(
            core_ctx,
            bindings_ctx,
            &loopback_id,
            AddrSubnetAndManualConfigEither::new::<I>(
                AddrSubnet::from_witness(I::LOOPBACK_ADDRESS, I::LOOPBACK_SUBNET.prefix()).unwrap(),
                Default::default(),
            ),
        )
        .unwrap();
        assert_eq!(device_ids.len(), Device::Loopback.index());
        device_ids.push(loopback_id);
        (ctx, device_ids)
    }

    fn do_route_lookup<I: Ip + TestIpExt + IpDeviceStateIpExt>(
        core_ctx: &SyncCtx<FakeBindingsCtx>,
        bindings_ctx: &mut FakeBindingsCtx,
        device_ids: Vec<DeviceId<FakeBindingsCtx>>,
        egress_device: Option<Device>,
        local_ip: Option<SpecifiedAddr<I::Addr>>,
        dest_ip: SpecifiedAddr<I::Addr>,
    ) -> Result<ResolvedRoute<I, Device>, ResolveRouteError>
    where
        for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>:
            IpSocketContext<I, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        let egress_device = egress_device.map(|d| &device_ids[d.index()]);

        IpSocketContext::<I, _>::lookup_route(
            &mut CoreCtx::new_deprecated(core_ctx),
            bindings_ctx,
            egress_device,
            local_ip,
            dest_ip,
        )
        // Convert device IDs in any route so it's easier to compare.
        .map(|ResolvedRoute { src_addr, device, next_hop }| {
            let device = Device::from_index(device_ids.iter().position(|d| d == &device).unwrap());
            ResolvedRoute { src_addr, device, next_hop }
        })
    }

    #[ip_test]
    #[test_case(None,
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    next_hop: NextHop::RemoteAsNeighbor }); "local delivery")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    next_hop: NextHop::RemoteAsNeighbor }); "local delivery specified local addr")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::First),
                Device::First.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::Loopback,
                    next_hop: NextHop::RemoteAsNeighbor });
                    "local delivery specified device and addr")]
    #[test_case(None,
                Some(Device::Loopback),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable);
                "local delivery specified loopback device no addr")]
    #[test_case(None,
                Some(Device::Loopback),
                Device::Loopback.ip_address(),
                Ok(ResolvedRoute { src_addr: Device::Loopback.ip_address(), device: Device::Loopback,
                    next_hop: NextHop::RemoteAsNeighbor,
                }); "local delivery to loopback addr via specified loopback device no addr")]
    #[test_case(None,
                Some(Device::Second),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable);
                "local delivery specified mismatched device no addr")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::Loopback),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable); "local delivery specified loopback device")]
    #[test_case(Some(Device::First.ip_address()),
                Some(Device::Second),
                Device::First.ip_address(),
                Err(ResolveRouteError::Unreachable); "local delivery specified mismatched device")]
    #[test_case(None,
                None,
                remote_ip::<I>(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "remote delivery")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                remote_ip::<I>(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "remote delivery specified addr")]
    #[test_case(Some(Device::Second.ip_address()), None, remote_ip::<I>(),
                Err(ResolveRouteError::NoSrcAddr); "remote delivery specified addr no route")]
    #[test_case(None,
                Some(Device::First),
                remote_ip::<I>(),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "remote delivery specified device")]
    #[test_case(None, Some(Device::Second), remote_ip::<I>(),
                Err(ResolveRouteError::Unreachable); "remote delivery specified device no route")]
    #[test_case(Some(Device::Second.ip_address()),
                None,
                Device::First.ip_address(),
                Ok(ResolvedRoute {src_addr: Device::Second.ip_address(), device: Device::Loopback,
                    next_hop: NextHop::RemoteAsNeighbor }); "local delivery cross device")]
    fn lookup_route<I: Ip + TestIpExt + IpDeviceIpExt + IpLayerIpExt>(
        local_ip: Option<SpecifiedAddr<I::Addr>>,
        egress_device: Option<Device>,
        dest_ip: SpecifiedAddr<I::Addr>,
        expected_result: Result<ResolvedRoute<I, Device>, ResolveRouteError>,
    ) where
        for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>:
            IpSocketContext<I, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        set_logger_for_test();

        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) = make_test_ctx::<I>();
        let core_ctx = &core_ctx;

        // Add a route to the remote address only for Device::First.
        crate::testutil::add_route(
            core_ctx,
            &mut bindings_ctx,
            AddableEntryEither::without_gateway(
                Subnet::new(*remote_ip::<I>(), <I::Addr as IpAddress>::BYTES * 8).unwrap().into(),
                device_ids[Device::First.index()].clone(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ),
        )
        .unwrap();

        let result = do_route_lookup(
            core_ctx,
            &mut bindings_ctx,
            device_ids,
            egress_device,
            local_ip,
            dest_ip,
        );
        assert_eq!(result, expected_result);
    }

    #[ip_test]
    #[test_case(None,
                None,
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "no constraints")]
    #[test_case(Some(Device::First.ip_address()),
                None,
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "constrain local addr")]
    #[test_case(Some(Device::Second.ip_address()), None,
                Ok(ResolvedRoute { src_addr: Device::Second.ip_address(), device: Device::Second,
                    next_hop: NextHop::RemoteAsNeighbor });
                    "constrain local addr to second device")]
    #[test_case(None,
                Some(Device::First),
                Ok(ResolvedRoute { src_addr: Device::First.ip_address(), device: Device::First,
                    next_hop: NextHop::RemoteAsNeighbor }); "constrain device")]
    #[test_case(None, Some(Device::Second),
                Ok(ResolvedRoute { src_addr: Device::Second.ip_address(), device: Device::Second,
                    next_hop: NextHop::RemoteAsNeighbor }); "constrain to second device")]
    fn lookup_route_multiple_devices_with_route<I: Ip + TestIpExt + IpDeviceIpExt + IpLayerIpExt>(
        local_ip: Option<SpecifiedAddr<I::Addr>>,
        egress_device: Option<Device>,
        expected_result: Result<ResolvedRoute<I, Device>, ResolveRouteError>,
    ) where
        for<'a> UnlockedCoreCtx<'a, FakeBindingsCtx>:
            IpSocketContext<I, FakeBindingsCtx, DeviceId = DeviceId<FakeBindingsCtx>>,
    {
        set_logger_for_test();

        let (Ctx { core_ctx, mut bindings_ctx }, device_ids) = make_test_ctx::<I>();
        let core_ctx = &core_ctx;

        // Add a route to the remote address for both devices, with preference
        // for the first.
        for device in [Device::First, Device::Second] {
            crate::testutil::add_route(
                core_ctx,
                &mut bindings_ctx,
                AddableEntryEither::without_gateway(
                    Subnet::new(*remote_ip::<I>(), <I::Addr as IpAddress>::BYTES * 8)
                        .unwrap()
                        .into(),
                    device_ids[device.index()].clone(),
                    AddableMetric::ExplicitMetric(RawMetric(device.index().try_into().unwrap())),
                ),
            )
            .unwrap();
        }

        let result = do_route_lookup(
            core_ctx,
            &mut bindings_ctx,
            device_ids,
            egress_device,
            local_ip,
            remote_ip::<I>(),
        );
        assert_eq!(result, expected_result);
    }
}
