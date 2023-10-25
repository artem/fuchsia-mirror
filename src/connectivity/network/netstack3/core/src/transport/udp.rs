// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The User Datagram Protocol (UDP).

use alloc::{collections::hash_map::DefaultHasher, vec::Vec};
use core::{
    convert::Infallible,
    fmt::Debug,
    hash::{Hash, Hasher},
    num::{NonZeroU16, NonZeroU8, NonZeroUsize},
    ops::RangeInclusive,
};
use lock_order::{lock::UnlockedAccess, Locked};

use dense_map::EntryKey;
use derivative::Derivative;
use either::Either;
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, IpInvariant, IpMarked, IpVersionMarker, Ipv4, Ipv4Addr, Ipv6,
        Ipv6Addr,
    },
    MulticastAddr, SpecifiedAddr, Witness,
};
use packet::{BufferMut, Nested, ParsablePacket, ParseBuffer, Serializer};
use packet_formats::{
    error::ParseError,
    ip::{IpProto, IpProtoExt},
    udp::{UdpPacket, UdpPacketBuilder, UdpPacketRaw, UdpParseArgs},
};
use thiserror::Error;
use tracing::trace;

pub(crate) use crate::socket::datagram::IpExt;
use crate::{
    algorithm::{PortAlloc, PortAllocImpl, ProtocolFlowId},
    context::{CounterContext, InstantContext, NonTestCtxMarker, RngContext, TracingContext},
    convert::BidirectionalConverter,
    counters::Counter,
    data_structures::socketmap::{IterShadows as _, SocketMap, Tagged},
    device::{AnyDevice, DeviceId, DeviceIdContext, Id, WeakDeviceId, WeakId},
    error::{LocalAddressError, SocketError, ZonedAddressError},
    ip::{
        icmp::IcmpIpExt,
        socket::{IpSockCreateAndSendError, IpSockCreationError, IpSockSendError},
        BufferIpTransportContext, BufferTransportIpContext, IpTransportContext,
        MulticastMembershipHandler, ResolveRouteError, TransportIpContext, TransportReceiveError,
    },
    socket::{
        address::{
            AddrIsMappedError, ConnAddr, ConnIpAddr, DualStackConnIpAddr, DualStackIpAddr,
            DualStackListenerIpAddr, IpPortSpec, ListenerAddr, ListenerIpAddr, SocketIpAddr,
            SocketZonedIpAddr,
        },
        datagram::{
            self, AddrEntry, BoundSocketState as DatagramBoundSocketState,
            BoundSockets as DatagramBoundSockets, ConnectError, DatagramBoundStateContext,
            DatagramFlowId, DatagramSocketMapSpec, DatagramSocketSpec, DatagramStateContext,
            DualStackConnState, DualStackDatagramBoundStateContext, DualStackIpExt,
            DualStackIpSocket, EitherIpSocket, ExpectedConnError, ExpectedUnboundError,
            FoundSockets, InUseError, IpOptions, LocalIdentifierAllocator, MaybeDualStack,
            MulticastMembershipInterfaceSelector, NonDualStackDatagramBoundStateContext,
            SendError as DatagramSendError, SetMulticastMembershipError, ShutdownType,
            SocketHopLimits, SocketInfo as DatagramSocketInfo, SocketState as DatagramSocketState,
            SocketsState as DatagramSocketsState,
        },
        AddrVec, Bound, IncompatibleError, InsertError, ListenerAddrInfo, RemoveResult,
        SocketAddrType, SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
    sync::RwLock,
    trace_duration, transport, SyncCtx,
};

/// A builder for UDP layer state.
#[derive(Clone)]
pub(crate) struct UdpStateBuilder {
    send_port_unreachable: bool,
}

impl Default for UdpStateBuilder {
    fn default() -> UdpStateBuilder {
        UdpStateBuilder { send_port_unreachable: false }
    }
}

impl UdpStateBuilder {
    /// Enable or disable sending ICMP Port Unreachable messages in response to
    /// inbound UDP packets for which a corresponding local socket does not
    /// exist (default: disabled).
    ///
    /// Responding with an ICMP Port Unreachable error is a vector for reflected
    /// Denial-of-Service (DoS) attacks. The attacker can send a UDP packet to a
    /// closed port with the source address set to the address of the victim,
    /// and ICMP response will be sent there.
    ///
    /// According to [RFC 1122 Section 4.1.3.1], "\[i\]f a datagram arrives
    /// addressed to a UDP port for which there is no pending LISTEN call, UDP
    /// SHOULD send an ICMP Port Unreachable message." Since an ICMP response is
    /// not mandatory, and due to the security risks described, responses are
    /// disabled by default.
    ///
    /// [RFC 1122 Section 4.1.3.1]: https://tools.ietf.org/html/rfc1122#section-4.1.3.1
    #[cfg(test)]
    pub(crate) fn send_port_unreachable(&mut self, send_port_unreachable: bool) -> &mut Self {
        self.send_port_unreachable = send_port_unreachable;
        self
    }

    pub(crate) fn build<I: IpExt, D: WeakId>(self) -> UdpState<I, D> {
        UdpState {
            sockets: Default::default(),
            send_port_unreachable: self.send_port_unreachable,
            counters: Default::default(),
        }
    }
}

/// Convenience alias to make names shorter.
pub(crate) type UdpBoundSocketMap<I, D> = DatagramBoundSockets<I, D, IpPortSpec, (Udp, I, D)>;

impl<I: IpExt, NewIp: IpExt, D: WeakId> GenericOverIp<NewIp> for UdpBoundSocketMap<I, D> {
    type Type = UdpBoundSocketMap<NewIp, D>;
}

#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""))]
pub(crate) struct BoundSockets<I: IpExt, D: WeakId> {
    bound_sockets: UdpBoundSocketMap<I, D>,
    /// lazy_port_alloc is lazy-initialized when it's used.
    lazy_port_alloc: Option<PortAlloc<UdpBoundSocketMap<I, D>>>,
}

pub(crate) type SocketsState<I, D> = DatagramSocketsState<I, D, Udp>;

/// A collection of UDP sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Sockets<I: Ip + IpExt, D: WeakId> {
    pub(crate) sockets_state: RwLock<SocketsState<I, D>>,
    pub(crate) bound: RwLock<BoundSockets<I, D>>,
}

/// The state associated with the UDP protocol.
///
/// `D` is the device ID type.
pub(crate) struct UdpState<I: IpExt, D: WeakId> {
    pub(crate) sockets: Sockets<I, D>,
    pub(crate) send_port_unreachable: bool,
    pub(crate) counters: UdpCounters<I>,
}

impl<I: IpExt, D: WeakId> Default for UdpState<I, D> {
    fn default() -> UdpState<I, D> {
        UdpStateBuilder::default().build()
    }
}

pub(crate) type UdpCounters<I> = IpMarked<I, UdpCountersInner>;

/// Counters for the UDP layer.
#[derive(Default)]
pub(crate) struct UdpCountersInner {
    /// Count of ICMP error messages received.
    pub(crate) rx_icmp_error: Counter,
}

impl<C: crate::NonSyncContext, I: Ip> UnlockedAccess<crate::lock_ordering::UdpCounters<I>>
    for SyncCtx<C>
{
    type Data = UdpCounters<I>;
    type Guard<'l> = &'l UdpCounters<I> where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.state.get_udp_counters()
    }
}

impl<NonSyncCtx: crate::NonSyncContext, I: Ip, L> CounterContext<UdpCounters<I>>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_counters<O, F: FnOnce(&UdpCounters<I>) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::UdpCounters<I>>())
    }
}

/// Uninstantiatable type for implementing [`DatagramSocketSpec`].
pub(crate) enum Udp {}

/// Produces an iterator over eligible receiving socket addresses.
#[cfg(test)]
fn iter_receiving_addrs<I: Ip + IpExt, D: WeakId>(
    addr: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    device: D,
) -> impl Iterator<Item = AddrVec<I, D, IpPortSpec>> {
    crate::socket::address::AddrVecIter::with_device(addr.into(), device)
}

fn check_posix_sharing<I: IpExt, D: WeakId>(
    new_sharing: Sharing,
    dest: AddrVec<I, D, IpPortSpec>,
    socketmap: &SocketMap<AddrVec<I, D, IpPortSpec>, Bound<(Udp, I, D)>>,
) -> Result<(), InsertError> {
    // Having a value present at a shadowed address is disqualifying, unless
    // both the new and existing sockets allow port sharing.
    if dest.iter_shadows().any(|a| {
        socketmap.get(&a).map_or(false, |bound| {
            !bound.tag(&a).to_sharing_options().is_shareable_with_new_state(new_sharing)
        })
    }) {
        return Err(InsertError::ShadowAddrExists);
    }

    // Likewise, the presence of a value that shadows the target address is
    // disqualifying unless both allow port sharing.
    match &dest {
        AddrVec::Conn(ConnAddr { ip: _, device: None }) | AddrVec::Listen(_) => {
            if socketmap.descendant_counts(&dest).any(|(tag, _): &(_, NonZeroUsize)| {
                !tag.to_sharing_options().is_shareable_with_new_state(new_sharing)
            }) {
                return Err(InsertError::ShadowerExists);
            }
        }
        AddrVec::Conn(ConnAddr { ip: _, device: Some(_) }) => {
            // No need to check shadows here because there are no addresses
            // that shadow a ConnAddr with a device.
            debug_assert_eq!(socketmap.descendant_counts(&dest).len(), 0)
        }
    }

    // There are a few combinations of addresses that can conflict with
    // each other even though there is not a direct shadowing relationship:
    // - listener address with device and connected address without.
    // - "any IP" listener with device and specific IP listener without.
    // - "any IP" listener with device and connected address without.
    //
    // The complication is that since these pairs of addresses don't have a
    // direct shadowing relationship, it's not possible to query for one
    // from the other in the socketmap without a linear scan. Instead. we
    // rely on the fact that the tag values in the socket map have different
    // values for entries with and without device IDs specified.
    fn conflict_exists<I: IpExt, D: WeakId>(
        new_sharing: Sharing,
        socketmap: &SocketMap<AddrVec<I, D, IpPortSpec>, Bound<(Udp, I, D)>>,
        addr: impl Into<AddrVec<I, D, IpPortSpec>>,
        mut is_conflicting: impl FnMut(&AddrVecTag) -> bool,
    ) -> bool {
        socketmap.descendant_counts(&addr.into()).any(|(tag, _): &(_, NonZeroUsize)| {
            is_conflicting(tag)
                && !tag.to_sharing_options().is_shareable_with_new_state(new_sharing)
        })
    }

    let found_indirect_conflict = match dest {
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier },
            device: Some(_device),
        }) => {
            // An address with a device will shadow an any-IP listener
            // `dest` with a device so we only need to check for addresses
            // without a device. Likewise, an any-IP listener will directly
            // shadow `dest`, so an indirect conflict can only come from a
            // specific listener or connected socket (without a device).
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: None, identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    !*has_device
                        && match addr_type {
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => true,
                            SocketAddrType::AnyListener => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: Some(ip), identifier },
            device: Some(_device),
        }) => {
            // A specific-IP listener `dest` with a device will be shadowed
            // by a connected socket with a device and will shadow
            // specific-IP addresses without a device and any-IP listeners
            // with and without devices. That means an indirect conflict can
            // only come from a connected socket without a device.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: Some(ip), identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    !*has_device
                        && match addr_type {
                            SocketAddrType::Connected => true,
                            SocketAddrType::AnyListener | SocketAddrType::SpecificListener => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: Some(_), identifier },
            device: None,
        }) => {
            // A specific-IP listener `dest` without a device will be
            // shadowed by a specific-IP listener with a device and by any
            // connected socket (with or without a device).  It will also
            // shadow an any-IP listener without a device, which means an
            // indirect conflict can only come from an any-IP listener with
            // a device.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: None, identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::AnyListener => true,
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => false,
                        }
                },
            )
        }
        AddrVec::Conn(ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_identifier), remote: _ },
            device: None,
        }) => {
            // A connected socket `dest` without a device shadows listeners
            // without devices, and is shadowed by a connected socket with
            // a device. It can indirectly conflict with listening sockets
            // with devices.

            // Check for specific-IP listeners with devices, which would
            // indirectly conflict.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr {
                    ip: ListenerIpAddr {
                        addr: Some(local_ip),
                        identifier: local_identifier.clone(),
                    },
                    device: None,
                },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::SpecificListener => true,
                            SocketAddrType::AnyListener | SocketAddrType::Connected => false,
                        }
                },
            ) ||
            // Check for any-IP listeners with devices since they conflict.
            // Note that this check cannot be combined with the one above
            // since they examine tag counts for different addresses. While
            // the counts of tags matched above *will* also be propagated to
            // the any-IP listener entry, they would be indistinguishable
            // from non-conflicting counts. For a connected address with
            // `Some(local_ip)`, the descendant counts at the listener
            // address with `addr = None` would include any
            // `SpecificListener` tags for both addresses with
            // `Some(local_ip)` and `Some(other_local_ip)`. The former
            // indirectly conflicts with `dest` but the latter does not,
            // hence this second distinct check.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr {
                    ip: ListenerIpAddr { addr: None, identifier: local_identifier },
                    device: None,
                },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::AnyListener => true,
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: _ },
            device: _,
        }) => false,
        AddrVec::Conn(ConnAddr { ip: _, device: Some(_device) }) => false,
    };
    if found_indirect_conflict {
        Err(InsertError::IndirectConflict)
    } else {
        Ok(())
    }
}

impl<I: IpExt, D: Id> SocketMapStateSpec for (Udp, I, D) {
    type ListenerId = I::DualStackReceivingId<Udp>;
    type ConnId = I::DualStackReceivingId<Udp>;

    type AddrVecTag = AddrVecTag;

    type ListenerSharingState = Sharing;
    type ConnSharingState = Sharing;

    type ListenerAddrState = AddrState<Self::ListenerId>;

    type ConnAddrState = AddrState<Self::ConnId>;
    fn listener_tag(
        ListenerAddrInfo { has_device, specified_addr }: ListenerAddrInfo,
        state: &Self::ListenerAddrState,
    ) -> Self::AddrVecTag {
        AddrVecTag {
            has_device,
            addr_type: specified_addr
                .then_some(SocketAddrType::SpecificListener)
                .unwrap_or(SocketAddrType::AnyListener),
            sharing: state.to_sharing_options(),
        }
    }
    fn connected_tag(has_device: bool, state: &Self::ConnAddrState) -> Self::AddrVecTag {
        AddrVecTag {
            has_device,
            addr_type: SocketAddrType::Connected,
            sharing: state.to_sharing_options(),
        }
    }
}

impl<AA, I: IpExt, D: WeakId> SocketMapConflictPolicy<AA, Sharing, I, D, IpPortSpec> for (Udp, I, D)
where
    AA: Into<AddrVec<I, D, IpPortSpec>> + Clone,
{
    fn check_insert_conflicts(
        new_sharing_state: &Sharing,
        addr: &AA,
        socketmap: &SocketMap<AddrVec<I, D, IpPortSpec>, Bound<Self>>,
    ) -> Result<(), InsertError> {
        check_posix_sharing(*new_sharing_state, addr.clone().into(), socketmap)
    }
}

/// State held for IPv6 sockets related to dual-stack operation.
#[derive(Clone, Debug, Derivative)]
#[derivative(Default)]
pub(crate) struct DualStackSocketState {
    // TODO(https://fxbug.dev/21198): Change this to true when dual-stack
    // sockets are enabled by default.
    #[derivative(Default(value = "false"))]
    dual_stack_enabled: bool,
}

impl DatagramSocketSpec for Udp {
    type AddrSpec = IpPortSpec;
    type SocketId<I: IpExt> = SocketId<I>;
    type OtherStackIpOptions<I: IpExt> = I::OtherStackIpOptions<DualStackSocketState>;
    type ListenerIpAddr<I: IpExt> = I::DualStackListenerIpAddr<NonZeroU16>;
    type ConnIpAddr<I: IpExt> = I::DualStackConnIpAddr<Self>;
    type ConnStateExtra = ();
    type ConnState<I: IpExt, D: Debug + Eq + Hash> = I::DualStackConnState<D, Self>;
    type UnboundSharingState<I: IpExt> = Sharing;
    type SocketMapSpec<I: IpExt, D: WeakId> = (Self, I, D);
    type ListenerSharingState = Sharing;

    type Serializer<I: IpExt, B: BufferMut> = Nested<B, UdpPacketBuilder<I::Addr>>;
    type SerializeError = Infallible;

    fn ip_proto<I: IpProtoExt>() -> I::Proto {
        IpProto::Udp.into()
    }

    fn make_receiving_map_id<I: IpExt, D: WeakId>(
        s: Self::SocketId<I>,
    ) -> I::DualStackReceivingId<Udp> {
        I::into_dual_stack_receiving_id(s)
    }

    fn make_packet<I: IpExt, B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    ) -> Result<Self::Serializer<I, B>, Infallible> {
        let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } = addr;
        Ok(body.encapsulate(UdpPacketBuilder::new(
            local_ip.addr(),
            remote_ip.addr(),
            Some(*local_port),
            *remote_port,
        )))
    }

    fn try_alloc_listen_identifier<I: IpExt, D: WeakId>(
        rng: &mut impl crate::RngContext,
        is_available: impl Fn(NonZeroU16) -> Result<(), InUseError>,
    ) -> Option<NonZeroU16> {
        try_alloc_listen_port::<I, D>(rng, is_available)
    }

    fn conn_addr_from_state<I: IpExt, D: Clone + Debug + Eq + Hash>(
        state: &Self::ConnState<I, D>,
    ) -> ConnAddr<Self::ConnIpAddr<I>, D> {
        I::conn_addr_from_state(state)
    }
}

impl<I: IpExt, D: WeakId> DatagramSocketMapSpec<I, D, IpPortSpec> for (Udp, I, D) {
    type ReceivingId = I::DualStackReceivingId<Udp>;
}

enum LookupResult<'a, I: IpExt, D: Id> {
    Conn(
        &'a I::DualStackReceivingId<Udp>,
        ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, D>,
    ),
    Listener(
        &'a I::DualStackReceivingId<Udp>,
        ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>,
    ),
}

#[derive(Hash, Copy, Clone)]
struct SocketSelectorParams<I: Ip, A: AsRef<I::Addr>> {
    src_ip: I::Addr,
    dst_ip: A,
    src_port: u16,
    dst_port: u16,
    _ip: IpVersionMarker<I>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum AddrState<T> {
    Exclusive(T),
    ReusePort(Vec<T>),
}

impl<'a, A: IpAddress, LI> From<&'a ListenerIpAddr<A, LI>> for SocketAddrType {
    fn from(ListenerIpAddr { addr, identifier: _ }: &'a ListenerIpAddr<A, LI>) -> Self {
        match addr {
            Some(_) => SocketAddrType::SpecificListener,
            None => SocketAddrType::AnyListener,
        }
    }
}

impl<'a, A: IpAddress, LI, RI> From<&'a ConnIpAddr<A, LI, RI>> for SocketAddrType {
    fn from(_: &'a ConnIpAddr<A, LI, RI>) -> Self {
        SocketAddrType::Connected
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Sharing {
    Exclusive,
    ReusePort,
}

impl Default for Sharing {
    fn default() -> Self {
        Self::Exclusive
    }
}

impl Sharing {
    pub(crate) fn is_shareable_with_new_state(&self, new_state: Sharing) -> bool {
        match (self, new_state) {
            (Sharing::Exclusive, Sharing::Exclusive) => false,
            (Sharing::Exclusive, Sharing::ReusePort) => false,
            (Sharing::ReusePort, Sharing::Exclusive) => false,
            (Sharing::ReusePort, Sharing::ReusePort) => true,
        }
    }

    pub(crate) fn is_reuse_port(&self) -> bool {
        match self {
            Sharing::Exclusive => false,
            Sharing::ReusePort => true,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AddrVecTag {
    pub(crate) has_device: bool,
    pub(crate) addr_type: SocketAddrType,
    pub(crate) sharing: Sharing,
}

pub(crate) trait ToSharingOptions {
    fn to_sharing_options(&self) -> Sharing;
}

impl ToSharingOptions for AddrVecTag {
    fn to_sharing_options(&self) -> Sharing {
        let AddrVecTag { has_device: _, addr_type: _, sharing } = self;
        *sharing
    }
}

impl<T> ToSharingOptions for AddrState<T> {
    fn to_sharing_options(&self) -> Sharing {
        match self {
            AddrState::Exclusive(_) => Sharing::Exclusive,
            AddrState::ReusePort(_) => Sharing::ReusePort,
        }
    }
}

impl<T> ToSharingOptions for (T, Sharing) {
    fn to_sharing_options(&self) -> Sharing {
        let (_state, sharing) = self;
        *sharing
    }
}

impl<I: Debug + Eq> SocketMapAddrStateSpec for AddrState<I> {
    type Id = I;
    type SharingState = Sharing;
    type Inserter<'a> = &'a mut Vec<I> where I: 'a;

    fn new(new_sharing_state: &Sharing, id: I) -> Self {
        match new_sharing_state {
            Sharing::Exclusive => Self::Exclusive(id),
            Sharing::ReusePort => Self::ReusePort(Vec::from([id])),
        }
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        match self {
            Self::Exclusive(x) => id == x,
            Self::ReusePort(ids) => ids.contains(id),
        }
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        new_sharing_state: &'a Sharing,
    ) -> Result<&'b mut Vec<I>, IncompatibleError> {
        match (self, new_sharing_state) {
            (AddrState::Exclusive(_), _) | (AddrState::ReusePort(_), Sharing::Exclusive) => {
                Err(IncompatibleError)
            }
            (AddrState::ReusePort(ids), Sharing::ReusePort) => Ok(ids),
        }
    }

    fn could_insert(
        &self,
        new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        match (self, new_sharing_state) {
            (AddrState::Exclusive(_), _) | (_, Sharing::Exclusive) => Err(IncompatibleError),
            (AddrState::ReusePort(_), Sharing::ReusePort) => Ok(()),
        }
    }

    fn remove_by_id(&mut self, id: I) -> RemoveResult {
        match self {
            AddrState::Exclusive(_) => RemoveResult::IsLast,
            AddrState::ReusePort(ids) => {
                let index = ids.iter().position(|i| i == &id).expect("couldn't find ID to remove");
                assert_eq!(ids.swap_remove(index), id);
                if ids.is_empty() {
                    RemoveResult::IsLast
                } else {
                    RemoveResult::Success
                }
            }
        }
    }
}

impl<T> AddrState<T> {
    fn select_receiver<I: Ip, A: AsRef<I::Addr> + Hash>(
        &self,
        selector: SocketSelectorParams<I, A>,
    ) -> &T {
        match self {
            AddrState::Exclusive(id) => id,
            AddrState::ReusePort(ids) => {
                let mut hasher = DefaultHasher::new();
                selector.hash(&mut hasher);
                let index: usize = hasher.finish() as usize % ids.len();
                &ids[index]
            }
        }
    }

    fn collect_all_ids(&self) -> impl Iterator<Item = &'_ T> {
        match self {
            AddrState::Exclusive(id) => Either::Left(core::iter::once(id)),
            AddrState::ReusePort(ids) => Either::Right(ids.iter()),
        }
    }
}

impl<'a, I: Ip + IpExt, D: WeakId + 'a> AddrEntry<'a, I, D, IpPortSpec, (Udp, I, D)> {
    /// Returns an iterator that yields a `LookupResult` for each contained ID.
    fn collect_all_ids(self) -> impl Iterator<Item = LookupResult<'a, I, D>> + 'a {
        match self {
            Self::Listen(state, l) => Either::Left(
                state.collect_all_ids().map(move |id| LookupResult::Listener(id, l.clone())),
            ),
            Self::Conn(state, c) => Either::Right(
                state.collect_all_ids().map(move |id| LookupResult::Conn(id, c.clone())),
            ),
        }
    }

    /// Returns a `LookupResult` for the contained ID that matches the selector.
    fn select_receiver<A: AsRef<I::Addr> + Hash>(
        self,
        selector: SocketSelectorParams<I, A>,
    ) -> LookupResult<'a, I, D> {
        match self {
            Self::Listen(state, l) => LookupResult::Listener(state.select_receiver(selector), l),
            Self::Conn(state, c) => LookupResult::Conn(state.select_receiver(selector), c),
        }
    }
}

/// Finds the socket(s) that should receive an incoming packet.
///
/// Uses the provided addresses and receiving device to look up sockets that
/// should receive a matching incoming packet. The returned iterator may
/// yield 0, 1, or multiple sockets.
fn lookup<'s, I: Ip + IpExt, D: WeakId>(
    bound: &'s DatagramBoundSockets<I, D, IpPortSpec, (Udp, I, D)>,
    (src_ip, src_port): (Option<SocketIpAddr<I::Addr>>, Option<NonZeroU16>),
    (dst_ip, dst_port): (SocketIpAddr<I::Addr>, NonZeroU16),
    device: D,
) -> impl Iterator<Item = LookupResult<'s, I, D>> + 's {
    let matching_entries = bound.iter_receivers((src_ip, src_port), (dst_ip, dst_port), device);
    match matching_entries {
        None => Either::Left(None),
        Some(FoundSockets::Single(entry)) => {
            let selector = SocketSelectorParams::<_, SpecifiedAddr<I::Addr>> {
                src_ip: src_ip.map_or(I::UNSPECIFIED_ADDRESS, SocketIpAddr::addr),
                dst_ip: dst_ip.into(),
                src_port: src_port.map_or(0, NonZeroU16::get),
                dst_port: dst_port.get(),
                _ip: IpVersionMarker::default(),
            };
            Either::Left(Some(entry.select_receiver(selector)))
        }

        Some(FoundSockets::Multicast(entries)) => {
            Either::Right(entries.into_iter().flat_map(AddrEntry::collect_all_ids))
        }
    }
    .into_iter()
}

/// Helper function to allocate a listen port.
///
/// Finds a random ephemeral port that is not in the provided `used_ports` set.
fn try_alloc_listen_port<I: IpExt, D: WeakId>(
    ctx: &mut impl RngContext,
    is_available: impl Fn(NonZeroU16) -> Result<(), InUseError>,
) -> Option<NonZeroU16> {
    let mut port = UdpBoundSocketMap::<I, D>::rand_ephemeral(&mut ctx.rng());
    for _ in UdpBoundSocketMap::<I, D>::EPHEMERAL_RANGE {
        // We can unwrap here because we know that the EPHEMERAL_RANGE doesn't
        // include 0.
        let tryport = NonZeroU16::new(port.get()).unwrap();
        match is_available(tryport) {
            Ok(()) => return Some(tryport),
            Err(InUseError {}) => port.next(),
        }
    }
    None
}

impl<I: IpExt, D: WeakId> PortAllocImpl for UdpBoundSocketMap<I, D> {
    const EPHEMERAL_RANGE: RangeInclusive<u16> = 49152..=65535;
    type Id = ProtocolFlowId<SocketIpAddr<I::Addr>>;

    fn is_port_available(&self, id: &Self::Id, port: u16) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();
        let conn = ConnAddr::from_protocol_flow_and_local_port(id, port);

        // A port is free if there are no sockets currently using it, and if
        // there are no sockets that are shadowing it.
        AddrVec::from(conn).iter_shadows().all(|a| match &a {
            AddrVec::Listen(l) => self.listeners().get_by_addr(&l).is_none(),
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).is_none(),
        } && self.get_shadower_counts(&a) == 0)
    }
}

/// Information associated with a UDP connection.
#[derive(Debug, GenericOverIp)]
#[cfg_attr(test, derive(PartialEq))]
#[generic_over_ip(A, IpAddress)]
pub struct ConnInfo<A: IpAddress, D> {
    /// The local address associated with a UDP connection.
    pub local_ip: SocketZonedIpAddr<A, D>,
    /// The local port associated with a UDP connection.
    pub local_port: NonZeroU16,
    /// The remote address associated with a UDP connection.
    pub remote_ip: SocketZonedIpAddr<A, D>,
    /// The remote port associated with a UDP connection.
    pub remote_port: NonZeroU16,
}

impl<A: IpAddress, D: Clone + Debug>
    From<ConnAddr<<A::Version as DualStackIpExt>::DualStackConnIpAddr<Udp>, D>> for ConnInfo<A, D>
where
    A::Version: DualStackIpExt,
{
    fn from(c: ConnAddr<<A::Version as DualStackIpExt>::DualStackConnIpAddr<Udp>, D>) -> Self {
        fn to_ipv6_mapped(a: SpecifiedAddr<Ipv4Addr>) -> SpecifiedAddr<Ipv6Addr> {
            // Safe to unwrap; ipv4-mapped-ipv6 addresses are always specified.
            SpecifiedAddr::new(a.get().to_ipv6_mapped()).unwrap()
        }

        let ConnAddr { ip, device } = c;
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt>(I::DualStackConnIpAddr<Udp>);
        let (local_ip, IpInvariant(local_port), remote_ip, IpInvariant(remote_port)) =
            A::Version::map_ip(
                Wrapper(ip),
                |Wrapper(ConnIpAddr {
                     local: (local_ip, local_id),
                     remote: (remote_ip, remote_id),
                 })| {
                    (
                        local_ip.into(),
                        IpInvariant(local_id),
                        remote_ip.into(),
                        IpInvariant(remote_id),
                    )
                },
                |Wrapper(dual_stack_conn_ip_addr)| match dual_stack_conn_ip_addr {
                    DualStackConnIpAddr::ThisStack(ConnIpAddr {
                        local: (local_ip, local_id),
                        remote: (remote_ip, remote_id),
                    }) => (
                        local_ip.into(),
                        IpInvariant(local_id),
                        remote_ip.into(),
                        IpInvariant(remote_id),
                    ),
                    DualStackConnIpAddr::OtherStack(ConnIpAddr {
                        local: (local_ip, local_id),
                        remote: (remote_ip, remote_id),
                    }) => (
                        to_ipv6_mapped(local_ip.into()),
                        IpInvariant(local_id),
                        to_ipv6_mapped(remote_ip.into()),
                        IpInvariant(remote_id),
                    ),
                },
            );

        Self {
            local_ip: SocketZonedIpAddr::new_with_zone(local_ip, || {
                device.clone().expect("device must be bound for addresses that require zones")
            }),
            local_port,
            remote_ip: SocketZonedIpAddr::new_with_zone(remote_ip, || {
                device.expect("device must be bound for addresses that require zones")
            }),
            remote_port,
        }
    }
}

/// Information associated with a UDP listener
#[derive(GenericOverIp)]
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
#[generic_over_ip(A, IpAddress)]
pub struct ListenerInfo<A: IpAddress, D> {
    /// The local address associated with a UDP listener, or `None` for any
    /// address.
    pub local_ip: Option<SocketZonedIpAddr<A, D>>,
    /// The local port associated with a UDP listener.
    pub local_port: NonZeroU16,
}

impl<A: IpAddress, D>
    From<ListenerAddr<<A::Version as DualStackIpExt>::DualStackListenerIpAddr<NonZeroU16>, D>>
    for ListenerInfo<A, D>
where
    A::Version: DualStackIpExt,
{
    fn from(
        ListenerAddr { ip, device }: ListenerAddr<
            <A::Version as DualStackIpExt>::DualStackListenerIpAddr<NonZeroU16>,
            D,
        >,
    ) -> Self {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt>(I::DualStackListenerIpAddr<NonZeroU16>);
        let (addr, IpInvariant(identifier)): (Option<SpecifiedAddr<A>>, _) = A::Version::map_ip(
            Wrapper(ip),
            |Wrapper(ListenerIpAddr { addr, identifier })| {
                (addr.map(SocketIpAddr::into), IpInvariant(identifier))
            },
            |Wrapper(DualStackListenerIpAddr { addr, identifier })| {
                let addr = match addr {
                    DualStackIpAddr::ThisStack(addr) => addr.map(SocketIpAddr::into),
                    DualStackIpAddr::OtherStack(addr) => SpecifiedAddr::new(
                        addr.map_or(Ipv4::UNSPECIFIED_ADDRESS, SocketIpAddr::addr).to_ipv6_mapped(),
                    ),
                };
                (addr, IpInvariant(identifier))
            },
        );

        let local_ip = addr.map(|addr| {
            SocketZonedIpAddr::new_with_zone(addr, || {
                device.expect("device must be bound for addresses that require zones")
            })
        });
        Self { local_ip, local_port: identifier }
    }
}

impl<A: IpAddress, D> From<NonZeroU16> for ListenerInfo<A, D> {
    fn from(local_port: NonZeroU16) -> Self {
        Self { local_ip: None, local_port }
    }
}

/// A unique identifier for a UDP socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
#[generic_over_ip(I, Ip)]

pub struct SocketId<I: Ip>(usize, IpVersionMarker<I>);

impl<I: Ip> From<usize> for SocketId<I> {
    fn from(value: usize) -> Self {
        Self(value, IpVersionMarker::default())
    }
}

impl<I: Ip> EntryKey for SocketId<I> {
    fn get_key_index(&self) -> usize {
        let Self(index, _marker) = self;
        *index
    }
}

/// An execution context for the UDP protocol.
pub trait NonSyncContext<I: IcmpIpExt> {
    /// Receives an ICMP error message related to a previously-sent UDP packet.
    ///
    /// `err` is the specific error identified by the incoming ICMP error
    /// message.
    ///
    /// Concretely, this method is called when an ICMP error message is received
    /// which contains an original packet which - based on its source and
    /// destination IPs and ports - most likely originated from the given
    /// socket. Note that the offending packet is not guaranteed to have
    /// originated from the given socket. For example, it may have originated
    /// from a previous socket with the same addresses, it may be the result of
    /// packet corruption on the network, it may have been injected by a
    /// malicious party, etc.
    fn receive_icmp_error(&mut self, id: SocketId<I>, err: I::ErrorCode);
}

/// The non-synchronized context for UDP.
pub(crate) trait StateNonSyncContext<I: IpExt>:
    InstantContext + RngContext + TracingContext + NonSyncContext<I>
{
}
impl<I: IpExt, C: InstantContext + RngContext + TracingContext + NonSyncContext<I>>
    StateNonSyncContext<I> for C
{
}

/// An execution context for the UDP protocol which also provides access to state.
pub(crate) trait BoundStateContext<I: IpExt, C: StateNonSyncContext<I>>:
    DeviceIdContext<AnyDevice> + UdpStateContext
{
    /// The synchronized context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + MulticastMembershipHandler<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    type DualStackContext: DualStackDatagramBoundStateContext<
        I,
        C,
        Udp,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
    type NonDualStackContext: NonDualStackDatagramBoundStateContext<
        I,
        C,
        Udp,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to UDP sockets.
    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &BoundSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to UDP sockets.
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Returns a context for dual- or non-dual-stack operation.
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;

    /// Calls the function without access to the UDP bound socket state.
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait StateContext<I: IpExt, C: StateNonSyncContext<I>>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback.
    type SocketStateCtx<'a>: BoundStateContext<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + UdpStateContext;

    /// Calls the function with an immutable reference to a socket's state.
    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to UDP sockets.
    fn with_sockets_state_mut<
        O,
        F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function without access to UDP socket state.
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Returns true if UDP may send a port unreachable ICMP error message.
    fn should_send_port_unreachable(&mut self) -> bool;
}

/// Empty trait to work around coherence issues.
///
/// This serves only to convince the coherence checker that a particular blanket
/// trait implementation could only possibly conflict with other blanket impls
/// in this crate. It can be safely implemented for any type.
/// TODO(https://github.com/rust-lang/rust/issues/97811): Remove this once the
/// coherence checker doesn't require it.
pub(crate) trait UdpStateContext {}

/// An execution context for UDP dual-stack operations.
pub(crate) trait DualStackBoundStateContext<I: IpExt, C: StateNonSyncContext<I>>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callbacks to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        // Allow creating IP sockets for the other IP version.
        + TransportIpContext<I::OtherVersion, C>;

    /// Calls the provided callback with mutable access to both the
    /// demultiplexing maps.
    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<I, Self::WeakDeviceId>,
            &mut BoundSockets<I::OtherVersion, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the provided callback with mutable access to the demultiplexing
    /// map for the other IP version.
    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<I::OtherVersion, Self::WeakDeviceId>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

/// An execution context for UDP non-dual-stack operations.
pub(crate) trait NonDualStackBoundStateContext<I: IpExt, C: StateNonSyncContext<I>>:
    DeviceIdContext<AnyDevice>
{
}

/// An execution context for the UDP protocol when a buffer is provided.
///
/// `BufferNonSyncContext` is like [`UdpContext`], except that it also requires that
/// the context be capable of receiving frames in buffers of type `B`. This is
/// used when a buffer of type `B` is provided to UDP (in particular, in
/// [`send_udp_conn`] and [`send_udp_listener`]), and allows any generated
/// link-layer frames to reuse that buffer rather than needing to always
/// allocate a new one.
pub trait BufferNonSyncContext<I: IcmpIpExt, B: BufferMut, D>: NonSyncContext<I> {
    /// Receives a UDP packet on a socket.
    fn receive_udp(
        &mut self,
        id: SocketId<I>,
        device_id: &D,
        dst_addr: (I::Addr, NonZeroU16),
        src_addr: (I::Addr, Option<NonZeroU16>),
        body: &B,
    );
}

/// The non-synchronized context for UDP with a buffer.
pub(crate) trait BufferStateNonSyncContext<I: IpExt, B: BufferMut, D>:
    StateNonSyncContext<I> + BufferNonSyncContext<I, B, D>
{
}
impl<I: IpExt, B: BufferMut, C: StateNonSyncContext<I> + BufferNonSyncContext<I, B, D>, D>
    BufferStateNonSyncContext<I, B, D> for C
{
}

// TODO(https://fxbug.dev/133966): This trait is used to express the requirement
// for the blanket impl [1] so that we don't need to mention the datagram traits
// in our BufferSocketHandler trait. We should simplify this by adding datagram
// trait bounds.
// [1]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/connectivity/network/netstack3/core/src/socket/datagram.rs;l=1073;drc=9c47dd3a602a603b1a803cf3abc7e708e8e17455
/// An execution context for the UDP protocol when a buffer is provided which
/// also provides access to state.
pub(crate) trait BufferStateContext<
    I: IpExt,
    C: BufferStateNonSyncContext<I, B, Self::DeviceId>,
    B: BufferMut,
>: StateContext<I, C> where
    for<'a> Self: StateContext<I, C, SocketStateCtx<'a> = Self::BufferSocketStateCtx<'a>>,
{
    type BufferSocketStateCtx<'a>: BufferBoundStateContext<
        I,
        C,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
}

// TODO(https://fxbug.dev/133966): This trait is used to express the requirement
// for the blanket impl [1] so that we don't need to mention the datagram traits
// in our BufferSocketHandler trait. We should simplify this by adding datagram
// trait bounds.
// [1]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/connectivity/network/netstack3/core/src/socket/datagram.rs;l=1042;drc=9c47dd3a602a603b1a803cf3abc7e708e8e17455
/// An execution context for the UDP protocol when a buffer is provided which
/// also provides access to state.
pub(crate) trait BufferBoundStateContext<
    I: IpExt,
    C: BufferStateNonSyncContext<I, B, Self::DeviceId>,
    B: BufferMut,
> where
    for<'a> Self: BoundStateContext<I, C, IpSocketsCtx<'a> = Self::BufferIpSocketsCtx<'a>>,
{
    type BufferIpSocketsCtx<'a>: BufferTransportIpContext<
        I,
        C,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
}

/// An implementation of [`IpTransportContext`] for UDP.
pub(crate) enum UdpIpTransportContext {}

impl<
        I: IpExt,
        C: StateNonSyncContext<I> + StateNonSyncContext<I::OtherVersion>,
        SC: StateContext<I, C> + CounterContext<UdpCounters<I>>,
    > IpTransportContext<I, C, SC> for UdpIpTransportContext
{
    fn receive_icmp_error(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: Option<SpecifiedAddr<I::Addr>>,
        dst_ip: SpecifiedAddr<I::Addr>,
        mut original_udp_packet: &[u8],
        err: I::ErrorCode,
    ) {
        sync_ctx.with_counters(|counters| {
            counters.rx_icmp_error.increment();
        });
        trace!("UdpIpTransportContext::receive_icmp_error({:?})", err);

        let udp_packet = match original_udp_packet
            .parse_with::<_, UdpPacketRaw<_>>(IpVersionMarker::<I>::default())
        {
            Ok(packet) => packet,
            Err(e) => {
                let _: ParseError = e;
                // TODO(joshlf): Do something with this error.
                return;
            }
        };

        if let (Some(src_ip), Some(src_port), Some(dst_port)) =
            (src_ip, udp_packet.src_port(), udp_packet.dst_port())
        {
            let src_ip = match src_ip.try_into() {
                Ok(addr) => addr,
                Err(AddrIsMappedError {}) => {
                    trace!("UdpIpTransportContext::receive_icmp_error: mapped source address");
                    return;
                }
            };
            let dst_ip = match dst_ip.try_into() {
                Ok(addr) => addr,
                Err(AddrIsMappedError {}) => {
                    trace!("UdpIpTransportContext::receive_icmp_error: mapped destination address");
                    return;
                }
            };
            sync_ctx.with_bound_state_context(|sync_ctx| {
                BoundStateContext::<I, _>::with_bound_sockets(
                    sync_ctx,
                    |sync_ctx, BoundSockets { bound_sockets, lazy_port_alloc: _ }| {
                        let receiver = lookup(
                            bound_sockets,
                            (Some(dst_ip), Some(dst_port)),
                            (src_ip, src_port),
                            sync_ctx.downgrade_device_id(device),
                        )
                        .next();

                        if let Some(id) = receiver {
                            let id = match id {
                                LookupResult::Listener(id, _) | LookupResult::Conn(id, _) => id,
                            };
                            match I::as_dual_stack_ip_socket(id) {
                                DualStackIpSocket::CurrentStack(id) => {
                                    ctx.receive_icmp_error(*id, err)
                                }
                                DualStackIpSocket::OtherStack(_id) => {
                                    // TODO(https://fxbug.dev/21198): Handle
                                    // ICMPv4 error codes on DualStack IPv6
                                    // sockets.
                                    trace!(
                                        "UdpIpTransportContext::receive_icmp_error: Support for
                                        ICMPv4 error codes on IPv6 Dual Stack sockets is
                                        unimplemented."
                                    );
                                }
                            }
                        } else {
                            trace!(
                                "UdpIpTransportContext::receive_icmp_error: Got ICMP error
                                message for nonexistent UDP socket; either the socket
                                responsible has since been removed, or the error message
                                was sent in error or corrupted"
                            );
                        }
                    },
                )
            });
        } else {
            trace!("UdpIpTransportContext::receive_icmp_error: Got ICMP error message for IP packet with an invalid source or destination IP or port");
        }
    }
}

fn receive_ip_packet<
    I: IpExt,
    B: BufferMut,
    C: BufferStateNonSyncContext<I, B, SC::DeviceId>
        + BufferStateNonSyncContext<I::OtherVersion, B, SC::DeviceId>,
    SC: BufferStateContext<I, C, B> + BufferStateContext<I::OtherVersion, C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    src_ip: I::RecvSrcAddr,
    dst_ip: SpecifiedAddr<I::Addr>,
    mut buffer: B,
) -> Result<(), (B, TransportReceiveError)> {
    trace_duration!(ctx, "udp::receive_ip_packet");
    trace!("received UDP packet: {:x?}", buffer.as_mut());
    let src_ip: I::Addr = src_ip.into();

    let packet = if let Ok(packet) =
        buffer.parse_with::<_, UdpPacket<_>>(UdpParseArgs::new(src_ip, dst_ip.get()))
    {
        packet
    } else {
        // There isn't much we can do if the UDP packet is
        // malformed.
        return Ok(());
    };

    let src_ip = if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
        match src_ip.try_into() {
            Ok(addr) => Some(addr),
            Err(AddrIsMappedError {}) => {
                trace!("UdpIpTransportContext::receive_icmp_error: mapped source address");
                return Ok(());
            }
        }
    } else {
        None
    };
    let dst_ip = match dst_ip.try_into() {
        Ok(addr) => addr,
        Err(AddrIsMappedError {}) => {
            trace!("UdpIpTransportContext::receive_icmp_error: mapped destination address");
            return Ok(());
        }
    };

    let src_port = packet.src_port();
    // Unfortunately, type inference isn't smart enough for us to just do
    // packet.parse_metadata().
    let meta = ParsablePacket::<_, UdpParseArgs<I::Addr>>::parse_metadata(&packet);

    /// The maximum number of socket IDs that are expected to receive a given
    /// packet. While it's possible for this number to be exceeded, it's
    /// unlikely.
    const MAX_EXPECTED_IDS: usize = 16;

    /// Collection of sockets that will receive a packet.
    ///
    /// Making this a [`smallvec::SmallVec`] lets us keep all the retrieved ids
    /// on the stack most of the time. If there are more than
    /// [`MAX_EXPECTED_IDS`], this will spill and allocate on the heap.
    type Recipients<Id> = smallvec::SmallVec<[Id; MAX_EXPECTED_IDS]>;

    let dst_port = packet.dst_port();
    let recipients = StateContext::<I, _>::with_bound_state_context(sync_ctx, |sync_ctx| {
        let device_weak = sync_ctx.downgrade_device_id(device);
        DatagramBoundStateContext::with_bound_sockets(sync_ctx, |_sync_ctx, bound_sockets| {
            lookup(bound_sockets, (src_ip, src_port), (dst_ip, dst_port), device_weak)
                .map(|result| match result {
                    // TODO(https://fxbug.dev/125489): Make these socket IDs
                    // strongly owned instead of just cloning them to prevent
                    // deletion before delivery is done.
                    LookupResult::Conn(id, _) | LookupResult::Listener(id, _) => id.clone(),
                })
                // Collect into an array on the stack that
                .collect::<Recipients<_>>()
        })
    });

    let was_delivered = recipients.into_iter().fold(false, |was_delivered, lookup_result| {
        let delivered = try_dual_stack_deliver::<I, B, C, SC>(
            sync_ctx,
            ctx,
            lookup_result,
            device,
            (dst_ip.addr(), dst_port),
            (src_ip.map_or(I::UNSPECIFIED_ADDRESS, SocketIpAddr::addr), src_port),
            &buffer,
        );
        was_delivered | delivered
    });

    if !was_delivered && StateContext::<I, _>::should_send_port_unreachable(sync_ctx) {
        buffer.undo_parse(meta);
        Err((buffer, TransportReceiveError::new_port_unreachable()))
    } else {
        Ok(())
    }
}

/// Tries to deliver the given UDP packet to the given UDP socket.
fn try_deliver<
    I: IpExt,
    SC: BufferStateContext<I, C, B>,
    C: BufferStateNonSyncContext<I, B, SC::DeviceId>,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: SocketId<I>,
    device: &SC::DeviceId,
    (dst_ip, dst_port): (I::Addr, NonZeroU16),
    (src_ip, src_port): (I::Addr, Option<NonZeroU16>),
    buffer: &B,
) -> bool {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let should_deliver = match state.get_socket_state(&id).expect("socket ID is valid") {
            DatagramSocketState::Bound(DatagramBoundSocketState::Connected {
                state,
                sharing: _,
            }) => {
                let state = match transport::udp::BoundStateContext::dual_stack_context(sync_ctx) {
                    MaybeDualStack::DualStack(dual_stack) => {
                        match dual_stack.converter().convert(state) {
                            DualStackConnState::ThisStack(state) => state,
                            DualStackConnState::OtherStack(state) => {
                                dual_stack.assert_dual_stack_enabled(state);
                                todo!(
                                    "https://fxbug.dev/21198: Support dual-stack udp try_deliver"
                                );
                            }
                        }
                    }
                    MaybeDualStack::NotDualStack(not_dual_stack) => {
                        not_dual_stack.converter().convert(state)
                    }
                };
                state.should_receive()
            }
            DatagramSocketState::Bound(DatagramBoundSocketState::Listener {
                state: _,
                sharing: _,
            })
            | DatagramSocketState::Unbound(_) => true,
        };
        if should_deliver {
            ctx.receive_udp(id, device, (dst_ip, dst_port), (src_ip, src_port), buffer);
        }
        should_deliver
    })
}

/// A wrapper for [`try_deliver`] that supports dual stack delivery.
fn try_dual_stack_deliver<
    I: IpExt,
    B: BufferMut,
    C: BufferStateNonSyncContext<I, B, SC::DeviceId>
        + BufferStateNonSyncContext<I::OtherVersion, B, SC::DeviceId>,
    SC: BufferStateContext<I, C, B> + BufferStateContext<I::OtherVersion, C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    socket: I::DualStackReceivingId<Udp>,
    device: &SC::DeviceId,
    (dst_ip, dst_port): (I::Addr, NonZeroU16),
    (src_ip, src_port): (I::Addr, Option<NonZeroU16>),
    buffer: &B,
) -> bool {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Inputs<I: IpExt> {
        src_ip: I::Addr,
        dst_ip: I::Addr,
        socket: I::DualStackReceivingId<Udp>,
    }

    struct Outputs<I: IpExt> {
        src_ip: I::Addr,
        dst_ip: I::Addr,
        socket: SocketId<I>,
    }

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    enum DualStackOutputs<I: DualStackIpExt> {
        CurrentStack(Outputs<I>),
        OtherStack(Outputs<I::OtherVersion>),
    }

    let dual_stack_outputs = I::map_ip(
        Inputs { src_ip, dst_ip, socket },
        |Inputs { src_ip, dst_ip, socket }| match socket {
            EitherIpSocket::V4(socket) => {
                DualStackOutputs::CurrentStack(Outputs { src_ip, dst_ip, socket })
            }
            EitherIpSocket::V6(socket) => DualStackOutputs::OtherStack(Outputs {
                src_ip: src_ip.to_ipv6_mapped(),
                dst_ip: dst_ip.to_ipv6_mapped(),
                socket,
            }),
        },
        |Inputs { src_ip, dst_ip, socket }| {
            DualStackOutputs::CurrentStack(Outputs { src_ip, dst_ip, socket })
        },
    );

    match dual_stack_outputs {
        DualStackOutputs::CurrentStack(Outputs { src_ip, dst_ip, socket }) => try_deliver(
            sync_ctx,
            ctx,
            socket,
            device,
            (dst_ip, dst_port),
            (src_ip, src_port),
            buffer,
        ),
        DualStackOutputs::OtherStack(Outputs { src_ip, dst_ip, socket }) => try_deliver(
            sync_ctx,
            ctx,
            socket,
            device,
            (dst_ip, dst_port),
            (src_ip, src_port),
            buffer,
        ),
    }
}

impl<
        I: IpExt,
        B: BufferMut,
        C: BufferStateNonSyncContext<I, B, SC::DeviceId>
            + BufferStateNonSyncContext<I::OtherVersion, B, SC::DeviceId>,
        SC: BufferStateContext<I, C, B>
            + BufferStateContext<I::OtherVersion, C, B>
            + NonTestCtxMarker
            + CounterContext<UdpCounters<I>>,
    > BufferIpTransportContext<I, C, SC, B> for UdpIpTransportContext
{
    fn receive_ip_packet(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        receive_ip_packet::<I, _, _, _>(sync_ctx, ctx, device, src_ip, dst_ip, buffer)
    }
}

/// An error encountered while sending a UDP packet to an alternate address.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum SendToError {
    /// The socket is not writeable.
    #[error("not writeable")]
    NotWriteable,
    /// An error was encountered while trying to create a temporary IP socket
    /// to use for the send operation.
    #[error("could not create a temporary connection socket: {}", _0)]
    CreateSock(IpSockCreationError),
    /// An MTU was exceeded.
    #[error("the maximum transmission unit (MTU) was exceeded")]
    Mtu,
    /// There was a problem with the remote address relating to its zone.
    #[error("zone error: {}", _0)]
    Zone(ZonedAddressError),
}

/// An error encountered while enabling or disabling dual-stack operation.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub struct NotDualStackCapableError;

pub(crate) trait SocketHandler<I: IpExt, C>: DeviceIdContext<AnyDevice> {
    fn create_udp(&mut self) -> SocketId<I>;

    fn connect(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
    ) -> Result<(), ConnectError>;

    fn set_device(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError>;

    fn get_udp_bound_device(&mut self, ctx: &C, id: SocketId<I>) -> Option<Self::WeakDeviceId>;

    fn set_dual_stack_enabled(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        enabled: bool,
    ) -> Result<(), NotDualStackCapableError>;

    fn get_dual_stack_enabled(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<bool, NotDualStackCapableError>;

    fn set_udp_posix_reuse_port(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        reuse_port: bool,
    ) -> Result<(), ExpectedUnboundError>;

    fn get_udp_posix_reuse_port(&mut self, ctx: &C, id: SocketId<I>) -> bool;

    fn set_udp_multicast_membership(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, Self::DeviceId>,
        want_membership: bool,
    ) -> Result<(), SetMulticastMembershipError>;

    fn set_udp_unicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        unicast_hop_limit: Option<NonZeroU8>,
    );

    fn set_udp_multicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_hop_limit: Option<NonZeroU8>,
    );

    fn get_udp_unicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8;

    fn get_udp_multicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8;

    fn get_udp_transparent(&mut self, id: SocketId<I>) -> bool;

    fn set_udp_transparent(&mut self, id: SocketId<I>, value: bool);

    fn disconnect_connected(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<(), ExpectedConnError>;

    fn shutdown(
        &mut self,
        ctx: &C,
        id: SocketId<I>,
        which: ShutdownType,
    ) -> Result<(), ExpectedConnError>;

    fn get_shutdown(&mut self, ctx: &C, id: SocketId<I>) -> Option<ShutdownType>;

    fn close(&mut self, ctx: &mut C, id: SocketId<I>) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn get_udp_info(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn listen_udp(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        addr: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>>;
}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: StateContext<I, C>> SocketHandler<I, C> for SC {
    fn create_udp(&mut self) -> SocketId<I> {
        datagram::create(self)
    }

    fn connect(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<<I>::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
    ) -> Result<(), ConnectError> {
        datagram::connect(self, ctx, id, remote_ip, remote_port, ())
    }

    fn set_device(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError> {
        datagram::set_device(self, ctx, id, device_id)
    }

    fn get_udp_bound_device(&mut self, ctx: &C, id: SocketId<I>) -> Option<Self::WeakDeviceId> {
        datagram::get_bound_device(self, ctx, id)
    }

    fn set_dual_stack_enabled(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        enabled: bool,
    ) -> Result<(), NotDualStackCapableError> {
        datagram::with_dual_stack_ip_options_mut(self, ctx, id, |other_stack| {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct WrapOtherStack<'a, I: IpExt>(
                &'a mut I::OtherStackIpOptions<DualStackSocketState>,
            );
            I::map_ip(
                (enabled, WrapOtherStack(other_stack)),
                |(_enabled, _v4)| Err(NotDualStackCapableError),
                |(enabled, WrapOtherStack(other_stack))| {
                    let DualStackSocketState { dual_stack_enabled } = other_stack;
                    *dual_stack_enabled = enabled;
                    Ok(())
                },
            )
        })
    }

    fn get_dual_stack_enabled(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<bool, NotDualStackCapableError> {
        datagram::with_dual_stack_ip_options(self, ctx, id, |other_stack| {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct WrapOtherStack<'a, I: IpExt>(&'a I::OtherStackIpOptions<DualStackSocketState>);
            I::map_ip(
                WrapOtherStack(other_stack),
                |_v4| Err(NotDualStackCapableError),
                |WrapOtherStack(other_stack)| {
                    let DualStackSocketState { dual_stack_enabled } = other_stack;
                    Ok(*dual_stack_enabled)
                },
            )
        })
    }

    fn set_udp_posix_reuse_port(
        &mut self,
        _ctx: &mut C,
        id: SocketId<I>,
        reuse_port: bool,
    ) -> Result<(), ExpectedUnboundError> {
        datagram::update_sharing(
            self,
            id,
            if reuse_port { Sharing::ReusePort } else { Sharing::Exclusive },
        )
    }

    fn get_udp_posix_reuse_port(&mut self, _ctx: &C, id: SocketId<I>) -> bool {
        datagram::get_sharing(self, id).is_reuse_port()
    }

    fn set_udp_multicast_membership(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, Self::DeviceId>,
        want_membership: bool,
    ) -> Result<(), SetMulticastMembershipError> {
        datagram::set_multicast_membership(
            self,
            ctx,
            id,
            multicast_group,
            interface,
            want_membership,
        )
        .map_err(Into::into)
    }

    fn set_udp_unicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        unicast_hop_limit: Option<NonZeroU8>,
    ) {
        crate::socket::datagram::update_ip_hop_limit(
            self,
            ctx,
            id,
            SocketHopLimits::set_unicast(unicast_hop_limit),
        )
    }

    fn set_udp_multicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_hop_limit: Option<NonZeroU8>,
    ) {
        crate::socket::datagram::update_ip_hop_limit(
            self,
            ctx,
            id,
            SocketHopLimits::set_multicast(multicast_hop_limit),
        )
    }

    fn get_udp_unicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8 {
        crate::socket::datagram::get_ip_hop_limits(self, ctx, id).unicast
    }

    fn get_udp_multicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8 {
        crate::socket::datagram::get_ip_hop_limits(self, ctx, id).multicast
    }

    fn get_udp_transparent(&mut self, id: SocketId<I>) -> bool {
        crate::socket::datagram::get_ip_transparent(self, id)
    }

    fn set_udp_transparent(&mut self, id: SocketId<I>, value: bool) {
        crate::socket::datagram::set_ip_transparent(self, id, value)
    }

    fn disconnect_connected(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<(), ExpectedConnError> {
        datagram::disconnect_connected(self, ctx, id)
    }

    fn shutdown(
        &mut self,
        ctx: &C,
        id: SocketId<I>,
        which: ShutdownType,
    ) -> Result<(), ExpectedConnError> {
        datagram::shutdown_connected(self, ctx, id, which)
    }

    fn get_shutdown(&mut self, ctx: &C, id: SocketId<I>) -> Option<ShutdownType> {
        datagram::get_shutdown_connected(self, ctx, id)
    }

    fn close(&mut self, ctx: &mut C, id: SocketId<I>) -> SocketInfo<I::Addr, Self::WeakDeviceId> {
        datagram::close(self, ctx, id).into()
    }

    fn get_udp_info(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId> {
        datagram::get_info(self, ctx, id).into()
    }

    fn listen_udp(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        addr: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
        datagram::listen(self, ctx, id, addr, port)
    }
}

/// Error when sending a packet on a socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip()]
pub enum SendError {
    /// The socket is not writeable.
    NotWriteable,
    /// The packet couldn't be sent.
    IpSock(IpSockSendError),
}

pub(crate) trait BufferSocketHandler<I: IpExt, C, B: BufferMut>:
    SocketHandler<I, C>
{
    fn send(
        &mut self,
        ctx: &mut C,
        conn: SocketId<I>,
        body: B,
    ) -> Result<(), Either<SendError, ExpectedConnError>>;

    fn send_to(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
        body: B,
    ) -> Result<(), Either<LocalAddressError, SendToError>>;
}

impl<
        I: IpExt,
        B: BufferMut,
        C: BufferStateNonSyncContext<I, B, SC::DeviceId>,
        SC: BufferStateContext<I, C, B>,
    > BufferSocketHandler<I, C, B> for SC
{
    fn send(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        body: B,
    ) -> Result<(), Either<SendError, ExpectedConnError>> {
        datagram::send_conn(self, ctx, id, body).map_err(|err| match err {
            DatagramSendError::NotConnected => Either::Right(ExpectedConnError),
            DatagramSendError::NotWriteable => Either::Left(SendError::NotWriteable),
            DatagramSendError::IpSock(err) => Either::Left(SendError::IpSock(err)),
            DatagramSendError::SerializeError(never) => match never {},
        })
    }

    fn send_to(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: Option<SocketZonedIpAddr<I::Addr, Self::DeviceId>>,
        remote_port: NonZeroU16,
        body: B,
    ) -> Result<(), Either<LocalAddressError, SendToError>> {
        datagram::send_to(self, ctx, id, remote_ip, remote_port, body).map_err(|e| match e {
            Either::Left(e) => Either::Left(e),
            Either::Right(e) => {
                let err = match e {
                    datagram::SendToError::SerializeError(never) => match never {},
                    datagram::SendToError::NotWriteable => SendToError::NotWriteable,
                    datagram::SendToError::Zone(e) => SendToError::Zone(e),
                    datagram::SendToError::CreateAndSend(e) => match e {
                        IpSockCreateAndSendError::Mtu => SendToError::Mtu,
                        IpSockCreateAndSendError::Create(e) => SendToError::CreateSock(e),
                    },
                    datagram::SendToError::RemoteUnexpectedlyMapped => SendToError::CreateSock(
                        IpSockCreationError::Route(ResolveRouteError::Unreachable),
                    ),
                };
                Either::Right(err)
            }
        })
    }
}

/// Sends a UDP packet on an existing socket.
///
/// # Errors
///
/// Returns an error if the socket is not connected or the packet cannot be sent.
/// On error, the original `body` is returned unmodified so that it can be
/// reused by the caller.
///
/// # Panics
///
/// Panics if `id` is not a valid UDP socket identifier.
pub fn send_udp<I: Ip, B: BufferMut, C: crate::BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    body: B,
) -> Result<(), Either<SendError, ExpectedConnError>> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, body)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, body)), id)| {
            BufferSocketHandler::<Ipv4, _, _>::send(sync_ctx, ctx, id, body).map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, body)), id)| {
            BufferSocketHandler::<Ipv6, _, _>::send(sync_ctx, ctx, id, body).map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sends a UDP packet to the provided destination address.
///
/// If this is called with an unbound socket, the socket will be implicitly
/// bound. If that succeeds, the ID for the new socket is returned.
///
/// # Errors
///
/// Returns an error if the socket is unbound and connecting fails, or if the
/// packet could not be sent. If the socket is unbound and connecting succeeds
/// but sending fails, the socket remains connected.
///
/// # Panics
///
/// Panics if `id` is not a valid UDP socket identifier.
pub fn send_udp_to<I: Ip, B: BufferMut, C: crate::BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<C>>>,
    remote_port: NonZeroU16,
    body: B,
) -> Result<(), Either<LocalAddressError, SendToError>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();

    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, remote_port, body)), id, remote_ip),
        |(IpInvariant((sync_ctx, ctx, remote_port, body)), id, remote_ip)| {
            BufferSocketHandler::<Ipv4, _, _>::send_to(
                sync_ctx,
                ctx,
                id,
                remote_ip,
                remote_port,
                body,
            )
            .map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, remote_port, body)), id, remote_ip)| {
            BufferSocketHandler::<Ipv6, _, _>::send_to(
                sync_ctx,
                ctx,
                id,
                remote_ip,
                remote_port,
                body,
            )
            .map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: StateContext<I, C>> DatagramStateContext<I, C, Udp>
    for SC
{
    type SocketsStateCtx<'a> = SC::SocketStateCtx<'a>;

    fn with_sockets_state<
        O,
        F: FnOnce(
            &mut Self::SocketsStateCtx<'_>,
            &DatagramSocketsState<I, Self::WeakDeviceId, Udp>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_sockets_state(|sync_ctx, sockets_state| cb(sync_ctx, sockets_state))
    }

    fn with_sockets_state_mut<
        O,
        F: FnOnce(
            &mut Self::SocketsStateCtx<'_>,
            &mut DatagramSocketsState<I, Self::WeakDeviceId, Udp>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_sockets_state_mut(|sync_ctx, sockets_state| cb(sync_ctx, sockets_state))
    }

    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        self.with_bound_state_context(cb)
    }
}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: BoundStateContext<I, C> + UdpStateContext>
    DatagramBoundStateContext<I, C, Udp> for SC
{
    type IpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;
    // TODO(https://fxbug.dev/133884): Remove the laziness by dropping `Option`.
    type LocalIdAllocator = Option<PortAlloc<UdpBoundSocketMap<I, SC::WeakDeviceId>>>;

    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &DatagramBoundSockets<I, Self::WeakDeviceId, IpPortSpec, (Udp, I, SC::WeakDeviceId)>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_bound_sockets(|sync_ctx, BoundSockets { bound_sockets, lazy_port_alloc: _ }| {
            cb(sync_ctx, bound_sockets)
        })
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut DatagramBoundSockets<I, Self::WeakDeviceId, IpPortSpec, (Udp, I, SC::WeakDeviceId)>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_bound_sockets_mut(|sync_ctx, BoundSockets { bound_sockets, lazy_port_alloc }| {
            cb(sync_ctx, bound_sockets, lazy_port_alloc)
        })
    }

    type DualStackContext = SC::DualStackContext;
    type NonDualStackContext = SC::NonDualStackContext;
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        BoundStateContext::dual_stack_context(self)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        self.with_transport_context(cb)
    }
}

impl<
        C: StateNonSyncContext<Ipv6> + StateNonSyncContext<Ipv4>,
        SC: DualStackBoundStateContext<Ipv6, C> + UdpStateContext,
    > DualStackDatagramBoundStateContext<Ipv6, C, Udp> for SC
{
    type IpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;
    fn dual_stack_enabled(
        &self,
        state: &impl AsRef<IpOptions<Ipv6, Self::WeakDeviceId, Udp>>,
    ) -> bool {
        let DualStackSocketState { dual_stack_enabled } = state.as_ref().other_stack();
        *dual_stack_enabled
    }

    type Converter = ();
    fn converter(&self) -> Self::Converter {
        ()
    }

    fn from_other_ip_addr(&self, addr: Ipv4Addr) -> Ipv6Addr {
        addr.to_ipv6_mapped()
    }

    fn to_other_receiving_id(&self, id: SocketId<Ipv6>) -> EitherIpSocket<Udp> {
        EitherIpSocket::V6(id)
    }

    type LocalIdAllocator = Option<PortAlloc<UdpBoundSocketMap<Ipv6, SC::WeakDeviceId>>>;
    type OtherLocalIdAllocator = Option<PortAlloc<UdpBoundSocketMap<Ipv4, SC::WeakDeviceId>>>;

    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut UdpBoundSocketMap<Ipv6, Self::WeakDeviceId>,
            &mut UdpBoundSocketMap<Ipv4, Self::WeakDeviceId>,
            &mut Self::LocalIdAllocator,
            &mut Self::OtherLocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_both_bound_sockets_mut(
            |sync_ctx,
             BoundSockets { bound_sockets: bound_first, lazy_port_alloc: alloc_first },
             BoundSockets { bound_sockets: bound_second, lazy_port_alloc: alloc_second }
            | {
                cb(sync_ctx, bound_first, bound_second, alloc_first, alloc_second)
            },
        )
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut UdpBoundSocketMap<Ipv4, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_other_bound_sockets_mut(
            |sync_ctx, BoundSockets { bound_sockets, lazy_port_alloc: _ }| {
                cb(sync_ctx, bound_sockets)
            },
        )
    }
}

impl<
        C: StateNonSyncContext<Ipv4>,
        SC: BoundStateContext<Ipv4, C> + NonDualStackBoundStateContext<Ipv4, C> + UdpStateContext,
    > NonDualStackDatagramBoundStateContext<Ipv4, C, Udp> for SC
{
    type Converter = ();
    fn converter(&self) -> Self::Converter {
        ()
    }
}

impl<I: IpExt, C: StateNonSyncContext<I>, D: WeakId>
    LocalIdentifierAllocator<I, D, IpPortSpec, C, (Udp, I, D)>
    for Option<PortAlloc<UdpBoundSocketMap<I, D>>>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &UdpBoundSocketMap<I, D>,
        ctx: &mut C,
        flow: datagram::DatagramFlowId<I::Addr, NonZeroU16>,
    ) -> Option<NonZeroU16> {
        let DatagramFlowId { local_ip, remote_ip, remote_id } = flow;
        let id = ProtocolFlowId::new(local_ip, remote_ip, remote_id);
        let mut rng = ctx.rng();
        // Lazily init port_alloc if it hasn't been inited yet.
        let port_alloc = self.get_or_insert_with(|| PortAlloc::new(&mut rng));
        port_alloc.try_alloc(&id, bound).and_then(NonZeroU16::new)
    }
}

/// Creates an unbound UDP socket.
///
/// `create_udp` creates a new UDP socket and returns an identifier for it.
pub fn create_udp<I: Ip, C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>) -> SocketId<I> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        IpInvariant(&mut sync_ctx),
        |IpInvariant(sync_ctx)| SocketHandler::<Ipv4, _>::create_udp(sync_ctx),
        |IpInvariant(sync_ctx)| SocketHandler::<Ipv6, _>::create_udp(sync_ctx),
    )
}

/// Connect a UDP socket
///
/// `connect` binds `id` as a connection to the remote address and port.
/// It is also bound to a local address and port, meaning that packets sent on
/// this connection will always come from that address and port. The local
/// address will be chosen based on the route to the remote address, and the
/// local port will be chosen from the available ones.
///
/// # Errors
///
/// `connect` will fail in the following cases:
/// - If both `local_ip` and `local_port` are specified but conflict with an
///   existing connection or listener
/// - If one or both are left unspecified but there is still no way to satisfy
///   the request (e.g., `local_ip` is specified but there are no available
///   local ports for that address)
/// - If there is no route to `remote_ip`
/// - If `id` belongs to an already-connected socket
///
/// # Panics
///
/// `connect` panics if `id` is not a valid [`UnboundId`].
pub fn connect<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, DeviceId<C>>>,
    remote_port: NonZeroU16,
) -> Result<(), ConnectError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, remote_port)), id, remote_ip),
        |(IpInvariant((sync_ctx, ctx, remote_port)), id, remote_ip)| {
            SocketHandler::<Ipv4, _>::connect(sync_ctx, ctx, id, remote_ip, remote_port)
                .map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, remote_port)), id, remote_ip)| {
            SocketHandler::<Ipv6, _>::connect(sync_ctx, ctx, id, remote_ip, remote_port)
                .map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sets the bound device for a socket.
///
/// Sets the device to be used for sending and receiving packets for a socket.
/// If the socket is not currently bound to a local address and port, the device
/// will be used when binding.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn set_udp_device<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    device_id: Option<&DeviceId<C>>,
) -> Result<(), SocketError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, device_id)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, device_id)), id)| {
            SocketHandler::<Ipv4, _>::set_device(sync_ctx, ctx, id, device_id).map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, device_id)), id)| {
            SocketHandler::<Ipv6, _>::set_device(sync_ctx, ctx, id, device_id).map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Gets the device the specified socket is bound to.
///
/// # Panics
///
/// Panics if `id` is not a valid socket ID.
pub fn get_udp_bound_device<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> Option<WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(device) = I::map_ip::<_, IpInvariant<Option<WeakDeviceId<C>>>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_bound_device(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_bound_device(sync_ctx, ctx, id))
        },
    );
    device
}

/// Sets the POSIX `SO_REUSEPORT` option for the specified socket.
///
/// # Errors
///
/// Returns an error if the socket is already bound.
///
/// # Panics
///
/// `set_udp_posix_reuse_port` panics if `id` is not a valid `SocketId`.
pub fn set_udp_posix_reuse_port<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    reuse_port: bool,
) -> Result<(), ExpectedUnboundError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, reuse_port)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, reuse_port)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_posix_reuse_port(sync_ctx, ctx, id, reuse_port)
        },
        |(IpInvariant((sync_ctx, ctx, reuse_port)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_posix_reuse_port(sync_ctx, ctx, id, reuse_port)
        },
    )
}

/// Gets the POSIX `SO_REUSEPORT` option for the specified socket.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn get_udp_posix_reuse_port<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> bool {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(reuse_port) = I::map_ip::<_, IpInvariant<bool>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_posix_reuse_port(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_posix_reuse_port(sync_ctx, ctx, id))
        },
    );
    reuse_port
}

/// Sets the specified socket's membership status for the given group.
///
/// If `id` is unbound, the membership state will take effect when it is bound.
/// An error is returned if the membership change request is invalid (e.g.
/// leaving a group that was not joined, or joining a group multiple times) or
/// if the device to use to join is unspecified or conflicts with the existing
/// socket state.
pub fn set_udp_multicast_membership<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, DeviceId<C>>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, want_membership)), id, multicast_group, interface),
        |(IpInvariant((sync_ctx, ctx, want_membership)), id, multicast_group, interface)| {
            SocketHandler::<Ipv4, _>::set_udp_multicast_membership(
                sync_ctx,
                ctx,
                id,
                multicast_group,
                interface,
                want_membership,
            )
            .map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, want_membership)), id, multicast_group, interface)| {
            SocketHandler::<Ipv6, _>::set_udp_multicast_membership(
                sync_ctx,
                ctx,
                id,
                multicast_group,
                interface,
                want_membership,
            )
            .map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sets the hop limit for packets sent by the socket to a unicast destination.
///
/// Sets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn set_udp_unicast_hop_limit<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    unicast_hop_limit: Option<NonZeroU8>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, unicast_hop_limit)), id),
        |(IpInvariant((sync_ctx, ctx, unicast_hop_limit)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_unicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                unicast_hop_limit,
            )
        },
        |(IpInvariant((sync_ctx, ctx, unicast_hop_limit)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_unicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                unicast_hop_limit,
            )
        },
    )
}

/// Sets the hop limit for packets sent by the socket to a multicast destination.
///
/// Sets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn set_udp_multicast_hop_limit<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    multicast_hop_limit: Option<NonZeroU8>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, multicast_hop_limit)), id),
        |(IpInvariant((sync_ctx, ctx, multicast_hop_limit)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_multicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                multicast_hop_limit,
            )
        },
        |(IpInvariant((sync_ctx, ctx, multicast_hop_limit)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_multicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                multicast_hop_limit,
            )
        },
    )
}

/// Gets the hop limit for packets sent by the socket to a unicast destination.
pub fn get_udp_unicast_hop_limit<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(hop_limit) = I::map_ip::<_, IpInvariant<NonZeroU8>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_unicast_hop_limit(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_unicast_hop_limit(sync_ctx, ctx, id))
        },
    );
    hop_limit
}

/// Gets the hop limit for packets sent by the socket to a multicast destination.
///
/// Gets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn get_udp_multicast_hop_limit<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(hop_limit) = I::map_ip::<_, IpInvariant<NonZeroU8>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_multicast_hop_limit(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_multicast_hop_limit(sync_ctx, ctx, id))
        },
    );
    hop_limit
}

/// Gets the transparent option.
pub fn get_udp_transparent<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &SocketId<I>,
) -> bool {
    I::map_ip(
        (IpInvariant(&mut Locked::new(sync_ctx)), id.clone()),
        |(IpInvariant(sync_ctx), id)| sync_ctx.get_udp_transparent(id.clone()),
        |(IpInvariant(sync_ctx), id)| sync_ctx.get_udp_transparent(id.clone()),
    )
}

/// Sets the transparent option.
pub fn set_udp_transparent<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &SocketId<I>,
    value: bool,
) {
    I::map_ip::<_, ()>(
        (IpInvariant(&mut Locked::new(sync_ctx)), id.clone(), value),
        |(IpInvariant(sync_ctx), id, value)| {
            SocketHandler::set_udp_transparent(sync_ctx, id, value)
        },
        |(IpInvariant(sync_ctx), id, value)| {
            SocketHandler::set_udp_transparent(sync_ctx, id, value)
        },
    );
}

/// Disconnects a connected UDP socket.
///
/// `disconnect_udp_connected` removes an existing connected socket and replaces
/// it with a listening socket bound to the same local address and port.
///
/// # Errors
///
/// Returns an error if the socket is not connected.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn disconnect_udp_connected<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
) -> Result<(), ExpectedConnError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::disconnect_connected(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::disconnect_connected(sync_ctx, ctx, id)
        },
    )
}

/// Shuts down a socket for reading and/or writing.
///
/// # Errors
///
/// Returns an error if the socket is not connected.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn shutdown<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, which)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, which)), id)| {
            SocketHandler::<Ipv4, _>::shutdown(sync_ctx, ctx, id, which)
        },
        |(IpInvariant((sync_ctx, ctx, which)), id)| {
            SocketHandler::<Ipv6, _>::shutdown(sync_ctx, ctx, id, which)
        },
    )
}

/// Get the shutdown state for a socket.
///
/// If the socket is not connected, or if `shutdown` was not called on it,
/// returns `None`.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn get_shutdown<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> Option<ShutdownType> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::get_shutdown(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::get_shutdown(sync_ctx, ctx, id)
        },
    )
}

/// Information about the addresses for a socket.
#[derive(GenericOverIp)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[generic_over_ip(A, IpAddress)]
pub enum SocketInfo<A: IpAddress, D> {
    /// The socket was not bound.
    Unbound,
    /// The socket was listening.
    Listener(ListenerInfo<A, D>),
    /// The socket was connected.
    Connected(ConnInfo<A, D>),
}

impl<I: IpExt, D: WeakId> From<DatagramSocketInfo<I, D, Udp>> for SocketInfo<I::Addr, D> {
    fn from(value: DatagramSocketInfo<I, D, Udp>) -> Self {
        match value {
            DatagramSocketInfo::Unbound => Self::Unbound,
            DatagramSocketInfo::Listener(addr) => Self::Listener(addr.into()),
            DatagramSocketInfo::Connected(addr) => Self::Connected(addr.into()),
        }
    }
}

/// Gets the [`SocketInfo`] associated with the UDP socket referenced by `id`.
///
/// # Panics
///
/// `get_udp_info` panics if `id` is not a valid `SocketId`.
pub fn get_udp_info<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
) -> SocketInfo<I::Addr, WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::get_udp_info(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::get_udp_info(sync_ctx, ctx, id)
        },
    )
}

/// Removes a socket that was previously created.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn close<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: SocketId<I>,
) -> SocketInfo<I::Addr, WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| SocketHandler::<Ipv4, _>::close(sync_ctx, ctx, id),
        |(IpInvariant((sync_ctx, ctx)), id)| SocketHandler::<Ipv6, _>::close(sync_ctx, ctx, id),
    )
}

/// Use an existing socket to listen for incoming UDP packets.
///
/// `listen_udp` converts `id` into a listening socket and registers the new
/// socket as a listener for incoming UDP packets on the given `port`. If `addr`
/// is `None`, the listener is a "wildcard listener", and is bound to all local
/// addresses. See the [`crate::transport`] module documentation for more
/// details.
///
/// If `addr` is `Some``, and `addr` is already bound on the given port (either
/// by a listener or a connection), `listen_udp` will fail. If `addr` is `None`,
/// and a wildcard listener is already bound to the given port, `listen_udp`
/// will fail.
///
/// # Errors
///
/// Returns an error if the socket is not currently unbound.
///
/// # Panics
///
/// `listen_udp` panics if `id` is not a valid [`SocketId`].
pub fn listen_udp<I: Ip, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    addr: Option<SocketZonedIpAddr<I::Addr, DeviceId<C>>>,
    port: Option<NonZeroU16>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, port)), id.clone(), addr),
        |(IpInvariant((sync_ctx, ctx, port)), id, addr)| {
            SocketHandler::<Ipv4, _>::listen_udp(sync_ctx, ctx, id, addr, port).map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, port)), id, addr)| {
            SocketHandler::<Ipv6, _>::listen_udp(sync_ctx, ctx, id, addr, port).map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

#[cfg(test)]
mod tests {
    use alloc::{
        borrow::ToOwned,
        collections::{HashMap, HashSet},
        vec,
        vec::Vec,
    };
    use const_unwrap::const_unwrap_option;
    use core::convert::TryInto as _;

    use assert_matches::assert_matches;

    use ip_test_macro::ip_test;
    use itertools::Itertools as _;
    use net_declare::net_ip_v4 as ip_v4;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{IpAddr, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr},
        AddrAndZone, LinkLocalAddr, MulticastAddr, Scope as _, ScopeableAddress as _, ZonedAddr,
    };
    use packet::{Buf, InnerPacketBuilder, ParsablePacket, Serializer};
    use packet_formats::{
        icmp::{Icmpv4DestUnreachableCode, Icmpv6DestUnreachableCode},
        ip::IpPacketBuilder,
        ipv4::{Ipv4Header, Ipv4PacketRaw},
        ipv6::{Ipv6Header, Ipv6PacketRaw},
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{
            FakeCtxWithSyncCtx, FakeFrameCtx, FakeNonSyncCtx, FakeSyncCtx, Wrapped,
            WrappedFakeSyncCtx,
        },
        device::testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
        error::RemoteAddressError,
        ip::{
            device::state::IpDeviceStateIpExt,
            icmp::{Icmpv4ErrorCode, Icmpv6ErrorCode},
            socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx},
            testutil::{DualStackSendIpPacketMeta, FakeIpDeviceIdCtx},
            ResolveRouteError, SendIpPacketMeta,
        },
        socket::{
            self,
            datagram::{MulticastInterfaceSelector, UninstantiableContext},
        },
        testutil::{set_logger_for_test, TestIpExt as _},
    };

    /// A packet received on a socket.
    #[derive(Debug, Derivative, PartialEq)]
    #[derivative(Default(bound = ""))]
    struct SocketReceived<I: IcmpIpExt> {
        packets: Vec<ReceivedPacket<I>>,
        errors: Vec<I::ErrorCode>,
    }

    #[derive(Debug, PartialEq)]
    struct ReceivedPacket<I: Ip> {
        addr: ReceivedPacketAddrs<I>,
        body: Vec<u8>,
    }

    #[derive(Debug, PartialEq)]
    struct ReceivedPacketAddrs<I: Ip> {
        src_ip: I::Addr,
        dst_ip: I::Addr,
        src_port: Option<NonZeroU16>,
    }

    impl FakeUdpSyncCtx<FakeDeviceId> {
        fn new_fake_device<I: TestIpExt>() -> Self {
            Self::with_local_remote_ip_addrs(vec![local_ip::<I>()], vec![remote_ip::<I>()])
        }
    }

    impl<Outer: Default> Wrapped<Outer, FakeUdpInnerSyncCtx<FakeDeviceId>> {
        fn with_local_remote_ip_addrs<A: Into<SpecifiedAddr<IpAddr>>>(
            local_ips: Vec<A>,
            remote_ips: Vec<A>,
        ) -> Self {
            Self::with_state(FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                device: FakeDeviceId,
                local_ips,
                remote_ips,
            }]))
        }
    }

    impl<Outer: Default, D: FakeStrongDeviceId> Wrapped<Outer, FakeUdpInnerSyncCtx<D>> {
        fn with_state(state: FakeDualStackIpSocketCtx<D>) -> Self {
            Wrapped {
                outer: Outer::default(),
                inner: WrappedFakeSyncCtx::with_inner_and_outer_state(state, Default::default()),
            }
        }
    }

    /// `FakeCtx` specialized for UDP.
    type FakeUdpCtx<D> = FakeCtxWithSyncCtx<FakeUdpSyncCtx<D>, (), (), FakeNonSyncCtxState>;

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeBoundSockets<D: WeakId> {
        v4: BoundSockets<Ipv4, D>,
        v6: BoundSockets<Ipv6, D>,
    }

    impl<D: WeakId, I: IpExt> AsRef<BoundSockets<I, D>> for FakeBoundSockets<D> {
        fn as_ref(&self) -> &BoundSockets<I, D> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(state)| &state.v4,
                |IpInvariant(state)| &state.v6,
            )
        }
    }

    impl<D: WeakId, I: IpExt> AsMut<BoundSockets<I, D>> for FakeBoundSockets<D> {
        fn as_mut(&mut self) -> &mut BoundSockets<I, D> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(state)| &mut state.v4,
                |IpInvariant(state)| &mut state.v6,
            )
        }
    }

    impl<I: IpExt, D: WeakId> AsRef<Self> for SocketsState<I, D> {
        fn as_ref(&self) -> &Self {
            self
        }
    }

    impl<I: IpExt, D: WeakId> AsMut<Self> for SocketsState<I, D> {
        fn as_mut(&mut self) -> &mut Self {
            self
        }
    }

    type FakeUdpInnerSyncCtx<D> =
        Wrapped<FakeBoundSockets<FakeWeakDeviceId<D>>, FakeBufferSyncCtx<D>>;

    /// `FakeNonSyncCtx` specialized for UDP.
    type FakeUdpNonSyncCtx = FakeNonSyncCtx<(), (), FakeNonSyncCtxState>;

    /// The FakeSyncCtx held as the inner state of the [`WrappedFakeSyncCtx`] that
    /// is [`FakeUdpSyncCtx`].
    type FakeBufferSyncCtx<D> =
        FakeSyncCtx<FakeDualStackIpSocketCtx<D>, DualStackSendIpPacketMeta<D>, D>;

    type UdpFakeDeviceCtx = FakeUdpCtx<FakeDeviceId>;
    type UdpFakeDeviceSyncCtx = FakeUdpSyncCtx<FakeDeviceId>;
    type UdpFakeDeviceNonSyncCtx = FakeUdpNonSyncCtx;

    #[derive(Default)]
    struct FakeNonSyncCtxState {
        received_v4: HashMap<SocketId<Ipv4>, SocketReceived<Ipv4>>,
        received_v6: HashMap<SocketId<Ipv6>, SocketReceived<Ipv6>>,
    }

    impl FakeNonSyncCtxState {
        fn received<I: TestIpExt>(&self) -> &HashMap<SocketId<I>, SocketReceived<I>> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: TestIpExt>(&'a HashMap<SocketId<I>, SocketReceived<I>>);
            let Wrap(map) = I::map_ip(
                IpInvariant(self),
                |IpInvariant(state)| Wrap(&state.received_v4),
                |IpInvariant(state)| Wrap(&state.received_v6),
            );
            map
        }

        fn received_mut<I: IcmpIpExt>(&mut self) -> &mut HashMap<SocketId<I>, SocketReceived<I>> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IcmpIpExt>(&'a mut HashMap<SocketId<I>, SocketReceived<I>>);
            let Wrap(map) = I::map_ip(
                IpInvariant(self),
                |IpInvariant(state)| Wrap(&mut state.received_v4),
                |IpInvariant(state)| Wrap(&mut state.received_v6),
            );
            map
        }

        fn socket_data<I: TestIpExt>(&self) -> HashMap<SocketId<I>, Vec<&'_ [u8]>> {
            self.received::<I>()
                .iter()
                .map(|(id, SocketReceived { errors: _, packets })| {
                    (
                        id.clone(),
                        packets.iter().map(|ReceivedPacket { addr: _, body }| &body[..]).collect(),
                    )
                })
                .collect()
        }

        fn errors<I: TestIpExt>(&self) -> HashMap<SocketId<I>, Vec<I::ErrorCode>> {
            self.received::<I>()
                .iter()
                .map(|(id, SocketReceived { errors, packets: _ })| (id.clone(), errors.clone()))
                .collect()
        }
    }

    impl<I: IcmpIpExt> NonSyncContext<I> for FakeUdpNonSyncCtx {
        fn receive_icmp_error(&mut self, id: SocketId<I>, err: I::ErrorCode) {
            self.state_mut().received_mut().entry(id).or_default().errors.push(err)
        }
    }

    impl<
            I: TestIpExt,
            D: FakeStrongDeviceId,
            Outer: AsRef<SocketsState<I, FakeWeakDeviceId<D>>>
                + AsMut<SocketsState<I, FakeWeakDeviceId<D>>>,
        > StateContext<I, FakeUdpNonSyncCtx> for Wrapped<Outer, FakeUdpInnerSyncCtx<D>>
    {
        type SocketStateCtx<'a> = FakeUdpInnerSyncCtx<D>;

        fn with_sockets_state<
            O,
            F: FnOnce(&mut Self::SocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_ref())
        }

        fn with_sockets_state_mut<
            O,
            F: FnOnce(&mut Self::SocketStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_mut())
        }

        fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketStateCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer: _, inner } = self;
            cb(inner)
        }

        fn should_send_port_unreachable(&mut self) -> bool {
            false
        }
    }

    impl<I: TestIpExt, D: FakeStrongDeviceId> BoundStateContext<I, FakeUdpNonSyncCtx>
        for FakeUdpInnerSyncCtx<D>
    {
        type IpSocketsCtx<'a> = FakeBufferSyncCtx<D>;
        type DualStackContext = I::UdpDualStackBoundStateContext<D>;
        type NonDualStackContext = I::UdpNonDualStackBoundStateContext<D>;

        fn with_bound_sockets<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &BoundSockets<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_ref())
        }

        fn with_bound_sockets_mut<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSockets<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_mut())
        }

        fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer: _, inner } = self;
            cb(inner)
        }

        fn dual_stack_context(
            &mut self,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            struct Wrap<'a, I: Ip + TestIpExt, D: FakeStrongDeviceId + 'static>(
                MaybeDualStack<
                    &'a mut I::UdpDualStackBoundStateContext<D>,
                    &'a mut I::UdpNonDualStackBoundStateContext<D>,
                >,
            );
            // TODO(https://fxbug.dev/131992): Replace this with a derived impl.
            impl<'a, I: TestIpExt, NewIp: TestIpExt, D: FakeStrongDeviceId + 'static>
                GenericOverIp<NewIp> for Wrap<'a, I, D>
            {
                type Type = Wrap<'a, NewIp, D>;
            }

            let Wrap(context) = I::map_ip(
                IpInvariant(self),
                |IpInvariant(this)| Wrap(MaybeDualStack::NotDualStack(this)),
                |IpInvariant(this)| Wrap(MaybeDualStack::DualStack(this)),
            );
            context
        }
    }

    impl<D: FakeStrongDeviceId + 'static> UdpStateContext for FakeUdpInnerSyncCtx<D> {}

    impl<D: FakeStrongDeviceId> NonDualStackBoundStateContext<Ipv4, FakeUdpNonSyncCtx>
        for FakeUdpInnerSyncCtx<D>
    {
    }

    impl<D: FakeStrongDeviceId> DualStackBoundStateContext<Ipv6, FakeUdpNonSyncCtx>
        for FakeUdpInnerSyncCtx<D>
    {
        type IpSocketsCtx<'a> = FakeBufferSyncCtx<D>;

        fn with_both_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<Ipv6, Self::WeakDeviceId>,
                &mut BoundSockets<Ipv4, Self::WeakDeviceId>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { inner, outer: FakeBoundSockets { v4, v6 } } = self;
            cb(inner, v6, v4)
        }

        fn with_other_bound_sockets_mut<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSockets<Ipv4, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            DualStackBoundStateContext::with_both_bound_sockets_mut(
                self,
                |sync_ctx, _bound, other_bound| cb(sync_ctx, other_bound),
            )
        }
    }

    impl<
            I: TestIpExt,
            D: FakeStrongDeviceId,
            B: BufferMut,
            Outer: AsRef<SocketsState<I, FakeWeakDeviceId<D>>>
                + AsMut<SocketsState<I, FakeWeakDeviceId<D>>>,
        > BufferStateContext<I, FakeUdpNonSyncCtx, B> for Wrapped<Outer, FakeUdpInnerSyncCtx<D>>
    {
        type BufferSocketStateCtx<'a> = Self::SocketStateCtx<'a>;
    }

    impl<I: TestIpExt, D: FakeStrongDeviceId, B: BufferMut>
        BufferBoundStateContext<I, FakeUdpNonSyncCtx, B> for FakeUdpInnerSyncCtx<D>
    {
        type BufferIpSocketsCtx<'a> = Self::IpSocketsCtx<'a>;
    }

    /// Ip packet delivery for the [`FakeUdpSyncCtx`].
    impl<I: IpExt + IpDeviceStateIpExt + TestIpExt, D: FakeStrongDeviceId, B: BufferMut>
        BufferIpTransportContext<I, FakeUdpNonSyncCtx, FakeUdpSyncCtx<D>, B>
        for UdpIpTransportContext
    {
        fn receive_ip_packet(
            sync_ctx: &mut FakeUdpSyncCtx<D>,
            ctx: &mut FakeUdpNonSyncCtx,
            device: &D,
            src_ip: I::RecvSrcAddr,
            dst_ip: SpecifiedAddr<I::Addr>,
            buffer: B,
        ) -> Result<(), (B, TransportReceiveError)> {
            // NB: The compiler can't deduce that `I::OtherVersion`` implements
            // `TestIpExt`, so use `map_ip` to transform the associated types
            // into concrete types (`Ipv4` & `Ipv6`).
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct SrcWrapper<I: IpExt>(I::RecvSrcAddr);
            let IpInvariant(result) = net_types::map_ip_twice!(
                I,
                (IpInvariant((sync_ctx, ctx, device, buffer)), SrcWrapper(src_ip), dst_ip),
                |(IpInvariant((sync_ctx, ctx, device, buffer)), SrcWrapper(src_ip), dst_ip)| {
                    IpInvariant(receive_ip_packet::<I, _, _, _>(
                        sync_ctx, ctx, device, src_ip, dst_ip, buffer,
                    ))
                }
            );
            result
        }
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct DualStackSocketsState<D: WeakId> {
        v4: SocketsState<Ipv4, D>,
        v6: SocketsState<Ipv6, D>,
        udpv4_counters: UdpCounters<Ipv4>,
        udpv6_counters: UdpCounters<Ipv6>,
    }

    impl<I: IpExt, D: WeakId> AsRef<SocketsState<I, D>> for DualStackSocketsState<D> {
        fn as_ref(&self) -> &SocketsState<I, D> {
            I::map_ip(IpInvariant(self), |IpInvariant(dual)| &dual.v4, |IpInvariant(dual)| &dual.v6)
        }
    }

    impl<D: WeakId> DualStackSocketsState<D> {
        fn get_udp_counters<I: Ip>(&self) -> &UdpCounters<I> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(dual)| &dual.udpv4_counters,
                |IpInvariant(dual)| &dual.udpv6_counters,
            )
        }
    }

    impl<I: IpExt, D: WeakId> AsMut<SocketsState<I, D>> for DualStackSocketsState<D> {
        fn as_mut(&mut self) -> &mut SocketsState<I, D> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(dual)| &mut dual.v4,
                |IpInvariant(dual)| &mut dual.v6,
            )
        }
    }

    type FakeUdpSyncCtx<D> =
        Wrapped<DualStackSocketsState<FakeWeakDeviceId<D>>, FakeUdpInnerSyncCtx<D>>;

    impl<I: Ip, D: FakeStrongDeviceId> CounterContext<UdpCounters<I>> for FakeUdpSyncCtx<D> {
        fn with_counters<O, F: FnOnce(&UdpCounters<I>) -> O>(&self, cb: F) -> O {
            cb(&self.outer.get_udp_counters())
        }
    }

    impl<I: TestIpExt, B: BufferMut, D> BufferNonSyncContext<I, B, D> for FakeUdpNonSyncCtx {
        fn receive_udp(
            &mut self,
            id: SocketId<I>,
            _device: &D,
            (dst_ip, _dst_port): (I::Addr, NonZeroU16),
            (src_ip, src_port): (I::Addr, Option<NonZeroU16>),
            body: &B,
        ) {
            self.state_mut().received_mut::<I>().entry(id).or_default().packets.push(
                ReceivedPacket {
                    addr: ReceivedPacketAddrs { src_ip, dst_ip, src_port },
                    body: body.as_ref().to_owned(),
                },
            )
        }
    }

    fn local_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(1)
    }

    fn remote_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(2)
    }

    trait TestIpExt: crate::testutil::TestIpExt + IpExt + IpDeviceStateIpExt {
        type UdpDualStackBoundStateContext<D: FakeStrongDeviceId + 'static>:
            DualStackDatagramBoundStateContext<Self, FakeUdpNonSyncCtx, Udp, DeviceId=D, WeakDeviceId=D::Weak>;
        type UdpNonDualStackBoundStateContext<D: FakeStrongDeviceId + 'static>:
            NonDualStackDatagramBoundStateContext<Self, FakeUdpNonSyncCtx, Udp, DeviceId=D, WeakDeviceId=D::Weak>;
        fn try_into_recv_src_addr(addr: Self::Addr) -> Option<Self::RecvSrcAddr>;
    }

    impl TestIpExt for Ipv4 {
        type UdpDualStackBoundStateContext<D: FakeStrongDeviceId + 'static> =
            UninstantiableContext<Self, Udp, FakeUdpInnerSyncCtx<D>>;

        type UdpNonDualStackBoundStateContext<D: FakeStrongDeviceId + 'static> =
            FakeUdpInnerSyncCtx<D>;

        fn try_into_recv_src_addr(addr: Ipv4Addr) -> Option<Ipv4Addr> {
            Some(addr)
        }
    }

    impl TestIpExt for Ipv6 {
        type UdpDualStackBoundStateContext<D: FakeStrongDeviceId + 'static> =
            FakeUdpInnerSyncCtx<D>;
        type UdpNonDualStackBoundStateContext<D: FakeStrongDeviceId + 'static> =
            UninstantiableContext<Self, Udp, FakeUdpInnerSyncCtx<D>>;

        fn try_into_recv_src_addr(addr: Ipv6Addr) -> Option<Ipv6SourceAddr> {
            Ipv6SourceAddr::new(addr)
        }
    }

    /// Helper function to inject an UDP packet with the provided parameters.
    fn receive_udp_packet<
        A: IpAddress,
        D: FakeStrongDeviceId,
        SC: DeviceIdContext<AnyDevice, DeviceId = D>,
    >(
        sync_ctx: &mut SC,
        ctx: &mut FakeUdpNonSyncCtx,
        device: D,
        src_ip: A,
        dst_ip: A,
        src_port: impl Into<u16>,
        dst_port: NonZeroU16,
        body: &[u8],
    ) where
        A::Version: TestIpExt,
        UdpIpTransportContext:
            BufferIpTransportContext<A::Version, FakeUdpNonSyncCtx, SC, Buf<Vec<u8>>>,
    {
        let builder =
            UdpPacketBuilder::new(src_ip, dst_ip, NonZeroU16::new(src_port.into()), dst_port);
        let buffer = Buf::new(body.to_owned(), ..)
            .encapsulate(builder)
            .serialize_vec_outer()
            .unwrap()
            .into_inner();
        <UdpIpTransportContext as BufferIpTransportContext<A::Version, _, _, _>>::receive_ip_packet(
            sync_ctx,
            ctx,
            &device,
            <A::Version as TestIpExt>::try_into_recv_src_addr(src_ip).unwrap(),
            SpecifiedAddr::new(dst_ip).unwrap(),
            buffer,
        )
        .expect("Receive IP packet succeeds");
    }

    const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(100));
    const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(200));

    fn conn_addr<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec>
    where
        I: Ip + TestIpExt,
    {
        let local_ip = SocketIpAddr::new_from_specified_or_panic(local_ip::<I>());
        let remote_ip = SocketIpAddr::new_from_specified_or_panic(remote_ip::<I>());
        ConnAddr {
            ip: ConnIpAddr { local: (local_ip, LOCAL_PORT), remote: (remote_ip, REMOTE_PORT) },
            device,
        }
        .into()
    }

    fn local_listener<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec>
    where
        I: Ip + TestIpExt,
    {
        let local_ip = SocketIpAddr::new_from_specified_or_panic(local_ip::<I>());
        ListenerAddr { ip: ListenerIpAddr { identifier: LOCAL_PORT, addr: Some(local_ip) }, device }
            .into()
    }

    fn wildcard_listener<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec>
    where
        I: Ip + TestIpExt,
    {
        ListenerAddr { ip: ListenerIpAddr { identifier: LOCAL_PORT, addr: None }, device }.into()
    }

    #[ip_test]
    #[test_case(conn_addr(Some(FakeWeakDeviceId(FakeDeviceId))), [
            conn_addr(None), local_listener(Some(FakeWeakDeviceId(FakeDeviceId))), local_listener(None),
            wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), wildcard_listener(None)
        ]; "conn with device")]
    #[test_case(local_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
        [local_listener(None), wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), wildcard_listener(None)];
        "local listener with device")]
    #[test_case(wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), [wildcard_listener(None)];
        "wildcard listener with device")]
    #[test_case(conn_addr(None), [local_listener(None), wildcard_listener(None)]; "conn no device")]
    #[test_case(local_listener(None), [wildcard_listener(None)]; "local listener no device")]
    #[test_case(wildcard_listener(None), []; "wildcard listener no device")]
    fn test_udp_addr_vec_iter_shadows_conn<I: Ip + IpExt, D: WeakId, const N: usize>(
        addr: AddrVec<I, D, IpPortSpec>,
        expected_shadows: [AddrVec<I, D, IpPortSpec>; N],
    ) {
        assert_eq!(addr.iter_shadows().collect::<HashSet<_>>(), HashSet::from(expected_shadows));
    }

    #[ip_test]
    fn test_iter_receiving_addrs<I: Ip + TestIpExt>() {
        let addr = ConnIpAddr {
            local: (SocketIpAddr::new_from_specified_or_panic(local_ip::<I>()), LOCAL_PORT),
            remote: (SocketIpAddr::new_from_specified_or_panic(remote_ip::<I>()), REMOTE_PORT),
        };
        assert_eq!(
            iter_receiving_addrs::<I, _>(addr, FakeWeakDeviceId(FakeDeviceId)).collect::<Vec<_>>(),
            vec![
                // A socket connected on exactly the receiving vector has precedence.
                conn_addr(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Connected takes precedence over listening with device match.
                conn_addr(None),
                local_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Specific IP takes precedence over device match.
                local_listener(None),
                wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Fallback to least specific
                wildcard_listener(None)
            ]
        );
    }

    /// Tests UDP listeners over different IP versions.
    ///
    /// Tests that a listener can be created, that the context receives packet
    /// notifications for that listener, and that we can send data using that
    /// listener.
    #[ip_test]
    fn test_listen_udp<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let socket = SocketHandler::create_udp(&mut sync_ctx);
        // Create a listener on local port 100, bound to the local IP:
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");

        // Inject a packet and check that the context receives it:
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );

        assert_eq!(
            non_sync_ctx.state().received::<I>(),
            &HashMap::from([(
                socket,
                SocketReceived {
                    errors: vec![],
                    packets: vec![ReceivedPacket {
                        body: body.into(),
                        addr: ReceivedPacketAddrs {
                            src_ip: remote_ip.get(),
                            dst_ip: local_ip.get(),
                            src_port: Some(const_unwrap_option(NonZeroU16::new(200))),
                        },
                    }]
                }
            )])
        );

        // Send a packet providing a local ip:
        BufferSocketHandler::send_to(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
            Buf::new(body.to_vec(), ..),
        )
        .expect("send_to suceeded");
        // And send a packet that doesn't:
        BufferSocketHandler::send_to(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
            Buf::new(body.to_vec(), ..),
        )
        .expect("send_to succeeded");
        let frames = sync_ctx.inner.inner.frames();
        assert_eq!(frames.len(), 2);
        let check_frame = |(meta, frame_body): &(
            DualStackSendIpPacketMeta<FakeDeviceId>,
            Vec<u8>,
        )| {
            let SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ } =
                meta.try_as::<I>().unwrap();
            assert_eq!(next_hop, &remote_ip);
            assert_eq!(src_ip, &local_ip);
            assert_eq!(dst_ip, &remote_ip);
            assert_eq!(proto, &IpProto::Udp.into());
            let mut buf = &frame_body[..];
            let udp_packet =
                UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
                    .expect("Parsed sent UDP packet");
            assert_eq!(udp_packet.src_port().unwrap().get(), 100);
            assert_eq!(udp_packet.dst_port().get(), 200);
            assert_eq!(udp_packet.body(), &body[..]);
        };
        check_frame(&frames[0]);
        check_frame(&frames[1]);
    }

    /// Tests that UDP packets without a connection are dropped.
    ///
    /// Tests that receiving a UDP packet on a port over which there isn't a
    /// listener causes the packet to be dropped correctly.
    #[ip_test]
    fn test_udp_drop<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();

        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );
        assert_eq!(&non_sync_ctx.state().socket_data::<I>(), &HashMap::new());
    }

    /// Tests that UDP connections can be created and data can be transmitted
    /// over it.
    ///
    /// Only tests with specified local port and address bounds.
    #[ip_test]
    fn test_udp_conn_basic<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        // Create a UDP connection with a specified local port and local IP.
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(NonZeroU16::new(100).unwrap()),
        )
        .expect("listen_udp failed");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        // Inject a UDP packet and see if we receive it on the context.
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );

        assert_eq!(non_sync_ctx.state().socket_data(), HashMap::from([(socket, vec![&body[..]])]));

        // Now try to send something over this new connection.
        BufferSocketHandler::send(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Buf::new(body.to_vec(), ..),
        )
        .expect("send_udp_conn returned an error");

        let (meta, frame_body) = assert_matches!(sync_ctx.inner.inner.frames(), [frame] => frame);
        // Check first frame.
        let SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ } =
            meta.try_as::<I>().unwrap();
        assert_eq!(next_hop, &remote_ip);
        assert_eq!(src_ip, &local_ip);
        assert_eq!(dst_ip, &remote_ip);
        assert_eq!(proto, &IpProto::Udp.into());
        let mut buf = &frame_body[..];
        let udp_packet = UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
            .expect("Parsed sent UDP packet");
        assert_eq!(udp_packet.src_port().unwrap().get(), 100);
        assert_eq!(udp_packet.dst_port().get(), 200);
        assert_eq!(udp_packet.body(), &body[..]);
    }

    /// Tests that UDP connections fail with an appropriate error for
    /// non-routable remote addresses.
    #[ip_test]
    fn test_udp_conn_unroutable<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        // Set fake context callback to treat all addresses as unroutable.
        let _local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(127);
        // Create a UDP connection with a specified local port and local IP.
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let conn_err = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .unwrap_err();

        assert_eq!(conn_err, ConnectError::Ip(ResolveRouteError::Unreachable.into()));
    }

    /// Tests that UDP listener creation fails with an appropriate error when
    /// local address is non-local.
    #[ip_test]
    fn test_udp_conn_cannot_bind<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        // Use remote address to trigger IpSockCreationError::LocalAddrNotAssigned.
        let remote_ip = remote_ip::<I>();
        // Create a UDP listener with a specified local port and local ip:
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200),
        );

        assert_eq!(result, Err(Either::Right(LocalAddressError::CannotBindToAddress)));
    }

    #[test]
    fn test_udp_conn_picks_link_local_source_address() {
        set_logger_for_test();
        // When the remote address has global scope but the source address
        // is link-local, make sure that the socket implicitly has its bound
        // device set.
        set_logger_for_test();
        let local_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let remote_ip = SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap();
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(100));
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![local_ip], vec![remote_ip]),
        );
        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            REMOTE_PORT,
        )
        .expect("can connect");

        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket);
        let (conn_local_ip, conn_remote_ip) = assert_matches!(
            info,
            SocketInfo::Connected(ConnInfo {
                local_ip: conn_local_ip,
                remote_ip: conn_remote_ip,
                local_port: _,
                remote_port: REMOTE_PORT
            }) => (conn_local_ip, conn_remote_ip)
        );
        assert_eq!(
            conn_local_ip.into_inner(),
            ZonedAddr::Zoned(AddrAndZone::new(local_ip, FakeWeakDeviceId(FakeDeviceId)).unwrap())
        );
        assert_eq!(conn_remote_ip.into_inner(), ZonedAddr::Unzoned(remote_ip));

        // Double-check that the bound device can't be changed after being set
        // implicitly.
        assert_eq!(
            SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, None),
            Err(SocketError::Local(
                LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch,)
            ))
        );
    }

    fn set_device_removed<I: TestIpExt>(sync_ctx: &mut UdpFakeDeviceSyncCtx, device_removed: bool) {
        let sync_ctx: &mut FakeSyncCtx<_, _, _> = sync_ctx.as_mut();
        let sync_ctx: &mut FakeIpDeviceIdCtx<_> = sync_ctx.get_mut().as_mut();
        sync_ctx.set_device_removed(FakeDeviceId, device_removed);
    }

    #[ip_test]
    #[test_case(
        true,
        Err(IpSockCreationError::Route(ResolveRouteError::Unreachable).into()); "remove device")]
    #[test_case(false, Ok(()); "dont remove device")]
    fn test_udp_conn_device_removed<I: Ip + TestIpExt>(
        remove_device: bool,
        expected: Result<(), ConnectError>,
    ) {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, unbound, Some(&FakeDeviceId))
            .unwrap();

        if remove_device {
            set_device_removed::<I>(&mut sync_ctx, remove_device);
        }

        let remote_ip = remote_ip::<I>();
        assert_eq!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                NonZeroU16::new(100).unwrap(),
            ),
            expected,
        );
    }

    /// Tests that UDP connections fail with an appropriate error when local
    /// ports are exhausted.
    #[ip_test]
    fn test_udp_conn_exhausted<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        let local_ip = local_ip::<I>();
        // Exhaust local ports to trigger FailedToAllocateLocalPort error.
        for port_num in UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE {
            let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(local_ip).into()),
                NonZeroU16::new(port_num),
            )
            .unwrap();
        }

        let remote_ip = remote_ip::<I>();
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let conn_err = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(100).unwrap(),
        )
        .unwrap_err();

        assert_eq!(conn_err, ConnectError::CouldNotAllocateLocalPort);
    }

    #[ip_test]
    fn test_connect_success<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let local_port = const_unwrap_option(NonZeroU16::new(100));
        let multicast_addr = I::get_multicast_addr(3);
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        // Set some properties on the socket that should be preserved.
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
            .expect("is unbound");
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            multicast_addr,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join multicast group should succeed");

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(local_port),
        )
        .expect("Initial call to listen_udp was expected to succeed");

        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            const_unwrap_option(NonZeroU16::new(200)),
        )
        .expect("connect should succeed");

        // Check that socket options set on the listener are propagated to the
        // connected socket.
        assert!(SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, socket));
        assert_eq!(
            sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(),
            HashMap::from([(
                (FakeDeviceId, multicast_addr),
                const_unwrap_option(NonZeroUsize::new(1))
            )])
        );
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                multicast_addr,
                MulticastInterfaceSelector::LocalAddress(local_ip).into(),
                true
            ),
            Err(SetMulticastMembershipError::GroupAlreadyJoined)
        );
    }

    #[ip_test]
    fn test_connect_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(127);
        let multicast_addr = I::get_multicast_addr(3);
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        // Set some properties on the socket that should be preserved.
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
            .expect("is unbound");
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            multicast_addr,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join multicast group should succeed");

        // Create a UDP connection with a specified local port and local IP.
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(const_unwrap_option(NonZeroU16::new(100))),
        )
        .expect("Initial call to listen_udp was expected to succeed");

        assert_matches!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                const_unwrap_option(NonZeroU16::new(1234))
            ),
            Err(ConnectError::Ip(IpSockCreationError::Route(ResolveRouteError::Unreachable)))
        );

        // Check that the listener was unchanged by the failed connection.
        assert!(SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, socket));
        assert_eq!(
            sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(),
            HashMap::from([(
                (FakeDeviceId, multicast_addr),
                const_unwrap_option(NonZeroUsize::new(1))
            )])
        );
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                multicast_addr,
                MulticastInterfaceSelector::LocalAddress(local_ip).into(),
                true
            ),
            Err(SetMulticastMembershipError::GroupAlreadyJoined)
        );
    }

    #[ip_test]
    fn test_reconnect_udp_conn_success<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let other_remote_ip = I::get_other_ip_address(3);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip, other_remote_ip],
            ));

        let local_port = NonZeroU16::new(100).unwrap();
        let local_ip = ZonedAddr::Unzoned(local_ip).into();
        let remote_ip = ZonedAddr::Unzoned(remote_ip).into();
        let other_remote_ip = ZonedAddr::Unzoned(other_remote_ip).into();

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(local_ip),
            Some(local_port),
        )
        .expect("listen should succeed");

        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect was expected to succeed");
        let other_remote_port = NonZeroU16::new(300).unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(other_remote_ip),
            other_remote_port,
        )
        .expect("connect should succeed");
        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Connected(ConnInfo {
                local_ip: local_ip.into_inner().map_zone(FakeWeakDeviceId).into(),
                local_port,
                remote_ip: other_remote_ip.into_inner().map_zone(FakeWeakDeviceId).into(),
                remote_port: other_remote_port
            })
        );
    }

    #[ip_test]
    fn test_reconnect_udp_conn_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>()).into();
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>()).into();
        let other_remote_ip = ZonedAddr::Unzoned(I::get_other_ip_address(3)).into();

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(local_ip),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");

        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_ip),
            REMOTE_PORT,
        )
        .expect("connect was expected to succeed");
        let other_remote_port = NonZeroU16::new(300).unwrap();
        let error = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(other_remote_ip),
            other_remote_port,
        )
        .expect_err("connect should fail");
        assert_matches!(
            error,
            ConnectError::Ip(IpSockCreationError::Route(ResolveRouteError::Unreachable))
        );

        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Connected(ConnInfo {
                local_ip: local_ip.into_inner().map_zone(FakeWeakDeviceId).into(),
                local_port: LOCAL_PORT,
                remote_ip: remote_ip.into_inner().map_zone(FakeWeakDeviceId).into(),
                remote_port: REMOTE_PORT
            })
        );
    }

    #[ip_test]
    fn test_send_to<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let other_remote_ip = I::get_other_ip_address(3);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip, other_remote_ip],
            ));

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        let body = [1, 2, 3, 4, 5];
        // Try to send something with send_to
        BufferSocketHandler::send_to(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(other_remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
            Buf::new(body.to_vec(), ..),
        )
        .expect("send_to failed");

        // The socket should not have been affected.
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        assert_eq!(info.local_ip, ZonedAddr::Unzoned(local_ip).into());
        assert_eq!(info.remote_ip, ZonedAddr::Unzoned(remote_ip).into());
        assert_eq!(info.remote_port, REMOTE_PORT);

        // Check first frame.
        let (meta, frame_body) = assert_matches!(sync_ctx.inner.inner.frames(), [frame] => frame);
        let SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ } =
            meta.try_as::<I>().unwrap();

        assert_eq!(next_hop, &other_remote_ip);
        assert_eq!(src_ip, &local_ip);
        assert_eq!(dst_ip, &other_remote_ip);
        assert_eq!(proto, &I::Proto::from(IpProto::Udp));
        let mut buf = &frame_body[..];
        let udp_packet = UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
            .expect("Parsed sent UDP packet");
        assert_eq!(udp_packet.src_port().unwrap(), LOCAL_PORT);
        assert_eq!(udp_packet.dst_port(), REMOTE_PORT);
        assert_eq!(udp_packet.body(), &body[..]);
    }

    /// Tests that UDP send failures are propagated as errors.
    ///
    /// Only tests with specified local port and address bounds.
    #[ip_test]
    fn test_send_udp_conn_failure<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let _local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        // Create a UDP connection with a specified local port and local IP.
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        // Instruct the fake frame context to throw errors.
        let frames: &mut FakeFrameCtx<_> = sync_ctx.as_mut();
        frames.set_should_error_for_frame(|_frame_meta| true);

        // Now try to send something over this new connection:
        let send_err = BufferSocketHandler::send(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Buf::new(Vec::new(), ..),
        )
        .unwrap_err();
        assert_eq!(send_err, Either::Left(SendError::IpSock(IpSockSendError::Mtu)));
    }

    #[ip_test]
    fn test_send_udp_conn_device_removed<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let remote_ip = remote_ip::<I>();
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, Some(&FakeDeviceId))
            .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        for (device_removed, expected_res) in [
            (false, Ok(())),
            (
                true,
                Err(Either::Left(SendError::IpSock(IpSockSendError::Unroutable(
                    ResolveRouteError::Unreachable,
                )))),
            ),
        ] {
            set_device_removed::<I>(&mut sync_ctx, device_removed);

            assert_eq!(
                BufferSocketHandler::send(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    socket,
                    Buf::new(Vec::new(), ..),
                ),
                expected_res,
            )
        }
    }

    #[ip_test]
    #[test_case(false, ShutdownType::Send; "shutdown send then send")]
    #[test_case(false, ShutdownType::SendAndReceive; "shutdown both then send")]
    #[test_case(true, ShutdownType::Send; "shutdown send then sendto")]
    #[test_case(true, ShutdownType::SendAndReceive; "shutdown both then sendto")]
    fn test_send_udp_after_shutdown<I: Ip + TestIpExt>(send_to: bool, shutdown: ShutdownType) {
        set_logger_for_test();

        #[derive(Debug)]
        struct NotWriteableError;

        fn send<I: Ip + TestIpExt, SC: BufferSocketHandler<I, FakeUdpNonSyncCtx, Buf<Vec<u8>>>>(
            remote_ip: Option<SocketZonedIpAddr<I::Addr, SC::DeviceId>>,
            sync_ctx: &mut SC,
            non_sync_ctx: &mut FakeUdpNonSyncCtx,
            id: SocketId<I>,
        ) -> Result<(), NotWriteableError> {
            match remote_ip {
                Some(remote_ip) => BufferSocketHandler::send_to(
                    sync_ctx,
                    non_sync_ctx,
                    id,
                    Some(remote_ip),
                    REMOTE_PORT,
                    Buf::new(Vec::new(), ..),
                )
                .map_err(
                    |e| assert_matches!(e, Either::Right(SendToError::NotWriteable) => NotWriteableError)
                ),
                None => BufferSocketHandler::send(
                    sync_ctx,
                    non_sync_ctx,
                    id,
                    Buf::new(Vec::new(), ..),
                )
                .map_err(|e| assert_matches!(e, Either::Left(SendError::NotWriteable) => NotWriteableError)),
            }
        }

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>()).into();
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(200));
        let send_to_ip = send_to.then_some(remote_ip);

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_ip),
            REMOTE_PORT,
        )
        .expect("connect failed");

        send(send_to_ip, &mut sync_ctx, &mut non_sync_ctx, socket).expect("can send");
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, socket, shutdown)
            .expect("is connected");

        assert_matches!(
            send(send_to_ip, &mut sync_ctx, &mut non_sync_ctx, socket),
            Err(NotWriteableError)
        );
    }

    #[ip_test]
    #[test_case(ShutdownType::Receive; "receive")]
    #[test_case(ShutdownType::SendAndReceive; "both")]
    fn test_marked_for_receive_shutdown<I: Ip + TestIpExt>(which: ShutdownType) {
        set_logger_for_test();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(200));

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
            Some(LOCAL_PORT),
        )
        .expect("can bind");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip::<I>()).into()),
            REMOTE_PORT,
        )
        .expect("can connect");

        // Receive once, then set the shutdown flag, then receive again and
        // check that it doesn't get to the socket.

        let packet = [1, 1, 1, 1];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );

        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([(socket, vec![&packet[..]])])
        );
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, socket, which).expect("is connected");
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );
        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([(socket, vec![&packet[..]])])
        );

        // Calling shutdown for the send direction doesn't change anything.
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, socket, ShutdownType::Send)
            .expect("is connected");
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );
        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([(socket, vec![&packet[..]])])
        );
    }

    /// Tests that if we have multiple listeners and connections, demuxing the
    /// flows is performed correctly.
    #[ip_test]
    fn test_udp_demux<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let local_ip = local_ip::<I>();
        let remote_ip_a = I::get_other_ip_address(70);
        let remote_ip_b = I::get_other_ip_address(72);
        let local_port_a = NonZeroU16::new(100).unwrap();
        let local_port_b = NonZeroU16::new(101).unwrap();
        let local_port_c = NonZeroU16::new(102).unwrap();
        let local_port_d = NonZeroU16::new(103).unwrap();
        let remote_port_a = NonZeroU16::new(200).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip_a, remote_ip_b],
            ));

        // Create some UDP connections and listeners:
        // conn2 has just a remote addr different than conn1, which requires
        // allowing them to share the local port.
        let [conn1, conn2] = [remote_ip_a, remote_ip_b].map(|remote_ip| {
            let socket = SocketHandler::create_udp(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
                .expect("is unbound");
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(local_ip).into()),
                Some(local_port_d),
            )
            .expect("listen_udp failed");
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                remote_port_a,
            )
            .expect("connect failed");
            socket
        });
        let list1 = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            list1,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(local_port_a),
        )
        .expect("listen_udp failed");
        let list2 = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            list2,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(local_port_b),
        )
        .expect("listen_udp failed");
        let wildcard_list = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            wildcard_list,
            None,
            Some(local_port_c),
        )
        .expect("listen_udp failed");

        let mut expectations = HashMap::<SocketId<I>, SocketReceived<I>>::new();
        // Now inject UDP packets that each of the created connections should
        // receive.
        let body_conn1 = [1, 1, 1, 1];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_d,
            &body_conn1[..],
        );
        expectations.entry(conn1).or_default().packets.push(ReceivedPacket {
            body: body_conn1.into(),
            addr: ReceivedPacketAddrs {
                src_ip: remote_ip_a.get(),
                dst_ip: local_ip.get(),
                src_port: Some(remote_port_a),
            },
        });
        assert_eq!(non_sync_ctx.state().received(), &expectations);

        let body_conn2 = [2, 2, 2, 2];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_b.get(),
            local_ip.get(),
            remote_port_a,
            local_port_d,
            &body_conn2[..],
        );
        expectations.entry(conn2).or_default().packets.push(ReceivedPacket {
            addr: ReceivedPacketAddrs {
                src_ip: remote_ip_b.get(),
                dst_ip: local_ip.get(),
                src_port: Some(remote_port_a),
            },
            body: body_conn2.into(),
        });
        assert_eq!(non_sync_ctx.state().received(), &expectations);

        let body_list1 = [3, 3, 3, 3];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_a,
            &body_list1[..],
        );
        expectations.entry(list1).or_default().packets.push(ReceivedPacket {
            addr: ReceivedPacketAddrs {
                src_ip: remote_ip_a.get(),
                dst_ip: local_ip.get(),
                src_port: Some(remote_port_a),
            },
            body: body_list1.into(),
        });
        assert_eq!(non_sync_ctx.state().received(), &expectations);

        let body_list2 = [4, 4, 4, 4];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_b,
            &body_list2[..],
        );
        expectations.entry(list2).or_default().packets.push(ReceivedPacket {
            body: body_list2.into(),
            addr: ReceivedPacketAddrs {
                src_ip: remote_ip_a.get(),
                dst_ip: local_ip.get(),
                src_port: Some(remote_port_a),
            },
        });
        assert_eq!(non_sync_ctx.state().received(), &expectations);

        let body_wildcard_list = [5, 5, 5, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_c,
            &body_wildcard_list[..],
        );
        expectations.entry(wildcard_list).or_default().packets.push(ReceivedPacket {
            addr: ReceivedPacketAddrs {
                src_ip: remote_ip_a.get(),
                dst_ip: local_ip.get(),
                src_port: Some(remote_port_a),
            },
            body: body_wildcard_list.into(),
        });
        assert_eq!(non_sync_ctx.state().received(), &expectations);
    }

    /// Tests UDP wildcard listeners for different IP versions.
    #[ip_test]
    fn test_wildcard_listeners<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip_a = I::get_other_ip_address(1);
        let local_ip_b = I::get_other_ip_address(2);
        let remote_ip_a = I::get_other_ip_address(70);
        let remote_ip_b = I::get_other_ip_address(72);
        let listener_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let listener = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            None,
            Some(listener_port),
        )
        .expect("listen_udp failed");

        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip_a.get(),
            remote_port,
            listener_port,
            &body[..],
        );
        // Receive into a different local IP.
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_b.get(),
            local_ip_b.get(),
            remote_port,
            listener_port,
            &body[..],
        );

        // Check that we received both packets for the listener.
        assert_eq!(
            non_sync_ctx.state().received::<I>(),
            &HashMap::from([(
                listener,
                SocketReceived {
                    errors: vec![],
                    packets: vec![
                        ReceivedPacket {
                            addr: ReceivedPacketAddrs {
                                src_ip: remote_ip_a.get(),
                                dst_ip: local_ip_a.get(),
                                src_port: Some(remote_port),
                            },
                            body: body.into(),
                        },
                        ReceivedPacket {
                            addr: ReceivedPacketAddrs {
                                src_ip: remote_ip_b.get(),
                                dst_ip: local_ip_b.get(),
                                src_port: Some(remote_port),
                            },
                            body: body.into()
                        }
                    ]
                }
            )])
        );
    }

    #[ip_test]
    fn test_receive_source_port_zero_on_listener<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let listener = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            None,
            Some(LOCAL_PORT),
        )
        .expect("listen_udp failed");

        let body = [];
        let (src_ip, src_port) = (I::FAKE_CONFIG.remote_ip.get(), 0u16);
        let (dst_ip, dst_port) = (I::FAKE_CONFIG.local_ip.get(), LOCAL_PORT);

        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            &body[..],
        );
        // Check that we received both packets for the listener.
        assert_eq!(
            non_sync_ctx.state().received(),
            &HashMap::from([(
                listener,
                SocketReceived {
                    packets: vec![ReceivedPacket {
                        body: vec![],
                        addr: ReceivedPacketAddrs { src_ip, dst_ip, src_port: None },
                    }],
                    errors: vec![]
                }
            )])
        );
    }

    #[ip_test]
    fn test_receive_source_addr_unspecified_on_listener<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let listener = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            None,
            Some(LOCAL_PORT),
        )
        .expect("listen_udp failed");

        let body = [];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            I::UNSPECIFIED_ADDRESS,
            I::FAKE_CONFIG.local_ip.get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );
        // Check that we received the packet on the listener.
        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([(listener, vec![&body[..]])])
        );
    }

    #[ip_test]
    #[test_case(const_unwrap_option(NonZeroU16::new(u16::MAX)), Ok(const_unwrap_option(NonZeroU16::new(u16::MAX))); "ephemeral available")]
    #[test_case(const_unwrap_option(NonZeroU16::new(100)), Err(LocalAddressError::FailedToAllocateLocalPort);
        "no ephemeral available")]
    fn test_bind_picked_port_all_others_taken<I: Ip + TestIpExt>(
        available_port: NonZeroU16,
        expected_result: Result<NonZeroU16, LocalAddressError>,
    ) {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        for port in 1..=u16::MAX {
            let port = NonZeroU16::new(port).unwrap();
            if port == available_port {
                continue;
            }
            let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
            let _listener = SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(port),
            )
            .expect("uncontested bind");
        }

        // Now that all but the LOCAL_PORT are occupied, ask the stack to
        // select a port.
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let result =
            SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, socket, None, None)
                .map(|()| {
                    let info =
                        SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket);
                    assert_matches!(info, SocketInfo::Listener(info) => info.local_port)
                })
                .map_err(Either::unwrap_right);
        assert_eq!(result, expected_result);
    }

    #[ip_test]
    fn test_receive_multicast_packet<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(70);
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let multicast_addr = I::get_multicast_addr(0);
        let multicast_addr_other = I::get_multicast_addr(1);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![local_ip], vec![remote_ip]),
        );

        // Create 3 sockets: one listener for all IPs, two listeners on the same
        // local address.
        let any_listener = {
            let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
                .expect("is unbound");
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed");
            socket
        };

        let specific_listeners = [(); 2].map(|()| {
            let socket = SocketHandler::create_udp(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
                .expect("is unbound");
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(multicast_addr.into_specified()).into()),
                Some(local_port),
            )
            .expect("listen_udp failed");
            socket
        });

        let mut receive_packet = |body, local_ip: MulticastAddr<I::Addr>| {
            let body = [body];
            receive_udp_packet(
                &mut sync_ctx,
                &mut non_sync_ctx,
                FakeDeviceId,
                remote_ip.get(),
                local_ip.get(),
                remote_port,
                local_port,
                &body,
            )
        };

        // These packets should be received by all listeners.
        receive_packet(1, multicast_addr);
        receive_packet(2, multicast_addr);

        // This packet should be received only by the all-IPs listener.
        receive_packet(3, multicast_addr_other);

        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([
                (specific_listeners[0], vec![[1].as_slice(), &[2]]),
                (specific_listeners[1], vec![&[1], &[2]]),
                (any_listener, vec![&[1], &[2], &[3]]),
            ]),
        );
    }

    type UdpMultipleDevicesCtx = FakeUdpCtx<MultipleDevicesId>;
    type UdpMultipleDevicesSyncCtx = FakeUdpSyncCtx<MultipleDevicesId>;
    type UdpMultipleDevicesNonSyncCtx = FakeUdpNonSyncCtx;

    impl FakeUdpSyncCtx<MultipleDevicesId> {
        fn new_multiple_devices<I: TestIpExt>() -> Self {
            let remote_ips = vec![I::get_other_remote_ip_address(1)];
            Self::with_state(FakeDualStackIpSocketCtx::new(
                MultipleDevicesId::all().into_iter().enumerate().map(|(i, device)| {
                    FakeDeviceConfig {
                        device,
                        local_ips: vec![I::get_other_ip_address((i + 1).try_into().unwrap())],
                        remote_ips: remote_ips.clone(),
                    }
                }),
            ))
        }
    }

    /// Tests that if sockets are bound to devices, they will only receive
    /// packets that are received on those devices.
    #[ip_test]
    fn test_bound_to_device_receive<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let sync_ctx = &mut sync_ctx;
        let bound_first_device = SocketHandler::<I, _>::create_udp(sync_ctx);
        SocketHandler::listen_udp(
            sync_ctx,
            &mut non_sync_ctx,
            bound_first_device,
            Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");
        SocketHandler::connect(
            sync_ctx,
            &mut non_sync_ctx,
            bound_first_device,
            Some(ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");
        SocketHandler::set_device(
            sync_ctx,
            &mut non_sync_ctx,
            bound_first_device,
            Some(&MultipleDevicesId::A),
        )
        .expect("bind should succeed");

        let bound_second_device = SocketHandler::create_udp(sync_ctx);
        SocketHandler::set_device(
            sync_ctx,
            &mut non_sync_ctx,
            bound_second_device,
            Some(&MultipleDevicesId::B),
        )
        .unwrap();
        SocketHandler::listen_udp(
            sync_ctx,
            &mut non_sync_ctx,
            bound_second_device,
            None,
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");

        // Inject a packet received on `MultipleDevicesId::A` from the specified
        // remote; this should go to the first socket.
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            sync_ctx,
            &mut non_sync_ctx,
            MultipleDevicesId::A,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );

        // A second packet received on `MultipleDevicesId::B` will go to the
        // second socket.
        receive_udp_packet(
            sync_ctx,
            &mut non_sync_ctx,
            MultipleDevicesId::B,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );
        assert_eq!(
            non_sync_ctx.state().socket_data(),
            HashMap::from([
                (bound_first_device, vec![&body[..]]),
                (bound_second_device, vec![&body[..]])
            ])
        );
    }

    /// Tests that if sockets are bound to devices, they will send packets out
    /// of those devices.
    #[ip_test]
    fn test_bound_to_device_send<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let sync_ctx = &mut sync_ctx;
        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let socket = SocketHandler::<I, _>::create_udp(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket, Some(&device)).unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, socket, None, Some(LOCAL_PORT))
                .expect("listen should succeed");
            socket
        });

        // Send a packet from each socket.
        let body = [1, 2, 3, 4, 5];
        for socket in bound_on_devices {
            BufferSocketHandler::send_to(
                sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)).into()),
                REMOTE_PORT,
                Buf::new(body.to_vec(), ..),
            )
            .expect("send should succeed");
        }

        let mut received_devices = sync_ctx
            .inner
            .inner
            .frames()
            .iter()
            .map(|(meta, _body)| {
                let SendIpPacketMeta {
                    device,
                    src_ip: _,
                    dst_ip,
                    next_hop: _,
                    proto,
                    ttl: _,
                    mtu: _,
                } = meta.try_as::<I>().unwrap();
                assert_eq!(proto, &IpProto::Udp.into());
                assert_eq!(dst_ip, &I::get_other_remote_ip_address(1),);
                *device
            })
            .collect::<Vec<_>>();
        received_devices.sort();
        assert_eq!(received_devices, &MultipleDevicesId::all());
    }

    fn receive_packet_on<I: Ip + TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx,
        ctx: &mut UdpMultipleDevicesNonSyncCtx,
        device: MultipleDevicesId,
    ) {
        const BODY: [u8; 5] = [1, 2, 3, 4, 5];
        receive_udp_packet(
            sync_ctx,
            ctx,
            device,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &BODY[..],
        )
    }

    /// Check that sockets can be bound to and unbound from devices.
    #[ip_test]
    fn test_bind_unbind_device<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let sync_ctx = &mut sync_ctx;

        // Start with `socket` bound to a device on all IPs.
        let socket = SocketHandler::create_udp(sync_ctx);
        SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket, Some(&MultipleDevicesId::A))
            .unwrap();
        SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, socket, None, Some(LOCAL_PORT))
            .expect("listen failed");

        // Since it is bound, it does not receive a packet from another device.
        receive_packet_on::<I>(sync_ctx, &mut non_sync_ctx, MultipleDevicesId::B);
        let received = &non_sync_ctx.state().socket_data::<I>();
        assert_eq!(received, &HashMap::new());

        // When unbound, the socket can receive packets on the other device.
        SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket, None)
            .expect("clearing bound device failed");
        receive_packet_on::<I>(sync_ctx, &mut non_sync_ctx, MultipleDevicesId::B);
        let received = non_sync_ctx.state().received::<I>().iter().collect::<Vec<_>>();
        let (rx_socket, socket_received) =
            assert_matches!(received[..], [(rx_socket, packets)] => (rx_socket, packets));
        assert_eq!(rx_socket, &socket);
        assert_matches!(socket_received.packets[..], [_]);
    }

    /// Check that bind fails as expected when it would cause illegal shadowing.
    #[ip_test]
    fn test_unbind_device_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let sync_ctx = &mut sync_ctx;

        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let socket = SocketHandler::<I, _>::create_udp(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket, Some(&device)).unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, socket, None, Some(LOCAL_PORT))
                .expect("listen should succeed");
            socket
        });

        // Clearing the bound device is not allowed for either socket since it
        // would then be shadowed by the other socket.
        for socket in bound_on_devices {
            assert_matches!(
                SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket, None),
                Err(SocketError::Local(LocalAddressError::AddressInUse))
            );
        }
    }

    /// Check that binding a device fails if it would make a connected socket
    /// unroutable.
    #[ip_test]
    fn test_bind_conn_socket_device_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let device_configs = HashMap::from(
            [(MultipleDevicesId::A, 1), (MultipleDevicesId::B, 2)].map(|(device, i)| {
                (
                    device,
                    FakeDeviceConfig {
                        device,
                        local_ips: vec![I::get_other_ip_address(i)],
                        remote_ips: vec![I::get_other_remote_ip_address(i)],
                    },
                )
            }),
        );
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(device_configs.iter().map(|(_, v)| v).cloned()),
            ));
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(device_configs[&MultipleDevicesId::A].remote_ips[0]).into()),
            LOCAL_PORT,
        )
        .expect("connect should succeed");

        // `socket` is not explicitly bound to device `A` but its route must
        // go through it because of the destination address. Therefore binding
        // to device `B` wil not work.
        assert_matches!(
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(&MultipleDevicesId::B)
            ),
            Err(SocketError::Remote(RemoteAddressError::NoRoute))
        );

        // Binding to device `A` should be fine.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(&MultipleDevicesId::A),
        )
        .expect("routing picked A already");
    }

    #[ip_test]
    fn test_bound_device_receive_multicast_packet<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let remote_ip = I::get_other_ip_address(1);
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let multicast_addr = I::get_multicast_addr(0);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );

        // Create 3 sockets: one listener bound on each device and one not bound
        // to a device.

        let sync_ctx = &mut sync_ctx;
        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let listener = SocketHandler::<I, _>::create_udp(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, listener, Some(&device))
                .unwrap();
            SocketHandler::set_udp_posix_reuse_port(sync_ctx, &mut non_sync_ctx, listener, true)
                .expect("is unbound");
            SocketHandler::listen_udp(
                sync_ctx,
                &mut non_sync_ctx,
                listener,
                None,
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed");

            (device, listener)
        });

        let listener = SocketHandler::create_udp(sync_ctx);
        SocketHandler::set_udp_posix_reuse_port(sync_ctx, &mut non_sync_ctx, listener, true)
            .expect("is unbound");
        SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, listener, None, Some(LOCAL_PORT))
            .expect("listen should succeed");

        fn index_for_device(id: MultipleDevicesId) -> u8 {
            match id {
                MultipleDevicesId::A => 0,
                MultipleDevicesId::B => 1,
                MultipleDevicesId::C => 2,
            }
        }

        let mut receive_packet = |remote_ip: SpecifiedAddr<I::Addr>, device: MultipleDevicesId| {
            let body = vec![index_for_device(device)];
            receive_udp_packet(
                sync_ctx,
                &mut non_sync_ctx,
                device,
                remote_ip.get(),
                multicast_addr.get(),
                remote_port,
                local_port,
                &body,
            )
        };

        // Receive packets from the remote IP on each device (2 packets total).
        // Listeners bound on devices should receive one, and the other listener
        // should receive both.
        for device in MultipleDevicesId::all() {
            receive_packet(remote_ip, device);
        }

        let per_socket_data = non_sync_ctx.state().socket_data();
        for (device, listener) in bound_on_devices {
            assert_eq!(per_socket_data[&listener], vec![&[index_for_device(device)]]);
        }
        let expected_listener_data = &MultipleDevicesId::all().map(|d| vec![index_for_device(d)]);
        assert_eq!(&per_socket_data[&listener], expected_listener_data);
    }

    /// Tests establishing a UDP connection without providing a local IP
    #[ip_test]
    fn test_conn_unspecified_local_ip<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, socket, None, Some(local_port))
            .expect("listen_udp failed");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip::<I>()).into()),
            remote_port,
        )
        .expect("connect failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket);
        assert_eq!(
            info,
            SocketInfo::Connected(ConnInfo {
                local_ip: ZonedAddr::Unzoned(local_ip::<I>()).into(),
                local_port,
                remote_ip: ZonedAddr::Unzoned(remote_ip::<I>()).into(),
                remote_port,
            })
        );
    }

    /// Tests local port allocation for [`connect`].
    ///
    /// Tests that calling [`connect`] causes a valid local port to be
    /// allocated.
    #[ip_test]
    fn test_udp_local_port_alloc<I: Ip + TestIpExt>() {
        let local_ip = local_ip::<I>();
        let ip_a = I::get_other_ip_address(100);
        let ip_b = I::get_other_ip_address(200);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![local_ip], vec![ip_a, ip_b]),
        );

        let conn_a = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn_a,
            Some(ZonedAddr::Unzoned(ip_a).into()),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let conn_b = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn_b,
            Some(ZonedAddr::Unzoned(ip_b).into()),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let conn_c = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn_c,
            Some(ZonedAddr::Unzoned(ip_a).into()),
            NonZeroU16::new(2020).unwrap(),
        )
        .expect("connect failed");
        let conn_d = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn_d,
            Some(ZonedAddr::Unzoned(ip_a).into()),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let valid_range = &UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE;
        let mut get_conn_port = |id| {
            let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, id);
            let info = assert_matches!(info, SocketInfo::Connected(info) => info);
            let ConnInfo { local_ip: _, local_port, remote_ip: _, remote_port: _ } = info;
            local_port
        };
        let port_a = get_conn_port(conn_a).get();
        let port_b = get_conn_port(conn_b).get();
        let port_c = get_conn_port(conn_c).get();
        let port_d = get_conn_port(conn_d).get();
        assert!(valid_range.contains(&port_a));
        assert!(valid_range.contains(&port_b));
        assert!(valid_range.contains(&port_c));
        assert!(valid_range.contains(&port_d));
        assert_ne!(port_a, port_b);
        assert_ne!(port_a, port_c);
        assert_ne!(port_a, port_d);
    }

    /// Tests that if `listen_udp` fails, it can be retried later.
    #[ip_test]
    fn test_udp_retry_listen_after_removing_conflict<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        fn listen_unbound<I: Ip + TestIpExt, C: StateNonSyncContext<I>>(
            sync_ctx: &mut impl StateContext<I, C>,
            non_sync_ctx: &mut C,
            socket: SocketId<I>,
        ) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
                Some(NonZeroU16::new(100).unwrap()),
            )
        }

        // Tie up the address so the second call to `connect` fails.
        let listener = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        listen_unbound(&mut sync_ctx, &mut non_sync_ctx, listener)
            .expect("Initial call to listen_udp was expected to succeed");

        // Trying to connect on the same address should fail.
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        assert_eq!(
            listen_unbound(&mut sync_ctx, &mut non_sync_ctx, unbound),
            Err(Either::Right(LocalAddressError::AddressInUse))
        );

        // Once the first listener is removed, the second socket can be
        // connected.
        let _: SocketInfo<I::Addr, _> =
            SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, listener);

        listen_unbound(&mut sync_ctx, &mut non_sync_ctx, unbound).expect("listen should succeed");
    }

    /// Tests local port allocation for [`listen_udp`].
    ///
    /// Tests that calling [`listen_udp`] causes a valid local port to be
    /// allocated when no local port is passed.
    #[ip_test]
    fn test_udp_listen_port_alloc<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();

        let wildcard_list = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, wildcard_list, None, None)
            .expect("listen_udp failed");
        let specified_list = SocketHandler::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            specified_list,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            None,
        )
        .expect("listen_udp failed");

        let mut get_listener_port = |id| {
            let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, id);
            let info = assert_matches!(info, SocketInfo::Listener(info) => info);
            let ListenerInfo { local_ip: _, local_port } = info;
            local_port
        };
        let wildcard_port = get_listener_port(wildcard_list);
        let specified_port = get_listener_port(specified_list);
        assert!(UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE
            .contains(&wildcard_port.get()));
        assert!(UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE
            .contains(&specified_port.get()));
        assert_ne!(wildcard_port, specified_port);
    }

    #[ip_test]
    fn test_bind_multiple_reuse_port<I: Ip + TestIpExt>() {
        let local_port = NonZeroU16::new(100).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let listeners = [(), ()].map(|()| {
            let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, socket, true)
                .expect("is unbound");
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed");
            socket
        });

        for listener in listeners {
            assert_eq!(
                SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, listener),
                SocketInfo::Listener(ListenerInfo { local_ip: None, local_port })
            );
        }
    }

    #[ip_test]
    fn test_set_unset_reuse_port_unbound<I: Ip + TestIpExt>() {
        let local_port = NonZeroU16::new(100).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let _listener = {
            let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            )
            .expect("is unbound");
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                false,
            )
            .expect("is unbound");
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed")
        };

        // Because there is already a listener bound without `SO_REUSEPORT` set,
        // the next bind to the same address should fail.
        assert_eq!(
            {
                let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
                SocketHandler::listen_udp(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    unbound,
                    None,
                    Some(local_port),
                )
            },
            Err(Either::Right(LocalAddressError::AddressInUse))
        );
    }

    #[ip_test]
    #[test_case(bind_as_listener)]
    #[test_case(bind_as_connected)]
    fn test_set_unset_reuse_port_bound<I: Ip + TestIpExt>(
        set_up_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx,
            &mut UdpFakeDeviceNonSyncCtx,
            SocketId<I>,
        ),
    ) {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let socket = SocketHandler::create_udp(&mut sync_ctx);
        set_up_socket(&mut sync_ctx, &mut non_sync_ctx, socket);

        // Per src/connectivity/network/netstack3/docs/POSIX_COMPATIBILITY.md,
        // Netstack3 only allows setting SO_REUSEPORT on unbound sockets.
        assert_matches!(
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                false,
            ),
            Err(ExpectedUnboundError)
        )
    }

    /// Tests [`remove_udp`]
    #[ip_test]
    fn test_remove_udp_conn<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>()).into();
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>()).into();
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(local_ip),
            Some(local_port),
        )
        .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_ip),
            remote_port,
        )
        .expect("connect failed");
        let info = SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, socket);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        // Assert that the info gotten back matches what was expected.
        assert_eq!(info.local_ip, local_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.local_port, local_port);
        assert_eq!(info.remote_ip, remote_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.remote_port, remote_port);

        // Assert that that connection id was removed from the connections
        // state.
        let Wrapped { outer: sockets_state, inner: _ } = &sync_ctx;
        assert_matches!(sockets_state.as_ref().get_socket_state(&socket), None);
    }

    /// Tests [`remove_udp`]
    #[ip_test]
    fn test_remove_udp_listener<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>()).into();
        let local_port = NonZeroU16::new(100).unwrap();

        // Test removing a specified listener.
        let specified = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            specified,
            Some(local_ip),
            Some(local_port),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, specified);
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip.unwrap(), local_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.local_port, local_port);
        let Wrapped { outer: sockets_state, inner: _ } = &sync_ctx;
        assert_matches!(sockets_state.as_ref().get_socket_state(&specified), None);

        // Test removing a wildcard listener.
        let wildcard = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            wildcard,
            None,
            Some(local_port),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, wildcard);
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip, None);
        assert_eq!(info.local_port, local_port);
        let Wrapped { outer: sockets_state, inner: _ } = &sync_ctx;
        assert_matches!(sockets_state.as_ref().get_socket_state(&wildcard), None);
    }

    fn try_join_leave_multicast<I: Ip + TestIpExt>(
        mcast_addr: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, MultipleDevicesId>,
        set_up_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx,
            &mut UdpFakeDeviceNonSyncCtx,
            SocketId<I>,
        ),
    ) -> (
        Result<(), SetMulticastMembershipError>,
        HashMap<(MultipleDevicesId, MulticastAddr<I::Addr>), NonZeroUsize>,
    ) {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );

        let socket = SocketHandler::create_udp(&mut sync_ctx);
        set_up_socket(&mut sync_ctx, &mut non_sync_ctx, socket);
        let result = SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            mcast_addr,
            interface,
            true,
        );

        let memberships_snapshot = sync_ctx.inner.inner.get_ref().multicast_memberships::<I>();
        if let Ok(()) = result {
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                mcast_addr,
                interface,
                false,
            )
            .expect("leaving group failed");
        }
        assert_eq!(sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(), HashMap::default());

        (result, memberships_snapshot)
    }

    fn leave_unbound<I: TestIpExt>(
        _sync_ctx: &mut UdpMultipleDevicesSyncCtx,
        _non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx,
        _unbound: SocketId<I>,
    ) {
    }

    fn bind_as_listener<I: TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx,
        non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx,
        unbound: SocketId<I>,
    ) {
        SocketHandler::listen_udp(
            sync_ctx,
            non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
            NonZeroU16::new(100),
        )
        .expect("listen should succeed")
    }

    fn bind_as_connected<I: TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx,
        non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx,
        unbound: SocketId<I>,
    ) {
        SocketHandler::connect(
            sync_ctx,
            non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect should succeed")
    }

    fn iface_id<A: IpAddress>(
        id: MultipleDevicesId,
    ) -> MulticastInterfaceSelector<A, MultipleDevicesId> {
        MulticastInterfaceSelector::Interface(id)
    }
    fn iface_addr<A: IpAddress>(
        addr: SpecifiedAddr<A>,
    ) -> MulticastInterfaceSelector<A, MultipleDevicesId> {
        MulticastInterfaceSelector::LocalAddress(addr)
    }

    #[ip_test]
    #[test_case(iface_id(MultipleDevicesId::A), leave_unbound::<I>; "device_no_addr_unbound")]
    #[test_case(iface_addr(local_ip::<I>()), leave_unbound::<I>; "addr_no_device_unbound")]
    #[test_case(iface_id(MultipleDevicesId::A), bind_as_listener::<I>; "device_no_addr_listener")]
    #[test_case(iface_addr(local_ip::<I>()), bind_as_listener::<I>; "addr_no_device_listener")]
    #[test_case(iface_id(MultipleDevicesId::A), bind_as_connected::<I>; "device_no_addr_connected")]
    #[test_case(iface_addr(local_ip::<I>()), bind_as_connected::<I>; "addr_no_device_connected")]
    fn test_join_leave_multicast_succeeds<I: Ip + TestIpExt>(
        interface: MulticastInterfaceSelector<I::Addr, MultipleDevicesId>,
        set_up_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx,
            &mut UdpFakeDeviceNonSyncCtx,
            SocketId<I>,
        ),
    ) {
        let mcast_addr = I::get_multicast_addr(3);

        let (result, ip_options) =
            try_join_leave_multicast(mcast_addr, interface.into(), set_up_socket);
        assert_eq!(result, Ok(()));
        assert_eq!(
            ip_options,
            HashMap::from([(
                (MultipleDevicesId::A, mcast_addr),
                const_unwrap_option(NonZeroUsize::new(1))
            )])
        );
    }

    #[ip_test]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), leave_unbound, Ok(());
        "with_ip_unbound")]
    #[test_case(MultipleDevicesId::A, None, leave_unbound, Ok(());
        "without_ip_unbound")]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), bind_as_listener, Ok(());
        "with_ip_listener")]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), bind_as_connected, Ok(());
        "with_ip_connected")]
    fn test_join_leave_multicast_interface_inferred_from_bound_device<I: Ip + TestIpExt>(
        bound_device: MultipleDevicesId,
        interface_addr: Option<SpecifiedAddr<I::Addr>>,
        set_up_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx,
            &mut UdpFakeDeviceNonSyncCtx,
            SocketId<I>,
        ),
        expected_result: Result<(), SetMulticastMembershipError>,
    ) {
        let mcast_addr = I::get_multicast_addr(3);
        let (result, ip_options) = try_join_leave_multicast(
            mcast_addr,
            interface_addr
                .map(MulticastInterfaceSelector::LocalAddress)
                .map(Into::into)
                .unwrap_or(MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute),
            |sync_ctx, non_sync_ctx, unbound| {
                SocketHandler::set_device(sync_ctx, non_sync_ctx, unbound, Some(&bound_device))
                    .unwrap();
                set_up_socket(sync_ctx, non_sync_ctx, unbound)
            },
        );
        assert_eq!(result, expected_result);
        assert_eq!(
            ip_options,
            expected_result.map_or(HashMap::default(), |()| HashMap::from([(
                (bound_device, mcast_addr),
                const_unwrap_option(NonZeroUsize::new(1))
            )]))
        );
    }

    #[ip_test]
    fn test_multicast_membership_with_removed_device<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, unbound, Some(&FakeDeviceId))
            .unwrap();

        set_device_removed::<I>(&mut sync_ctx, true);

        let group = I::get_multicast_addr(4);
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                group,
                // Will use the socket's bound device.
                MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute,
                true,
            ),
            Err(SetMulticastMembershipError::DeviceDoesNotExist),
        );

        // Should not have updated the device's multicast state.
        //
        // Note that even though we mock the device being removed above, its
        // state still exists in the fake IP socket context so we can inspect
        // it here.
        assert_eq!(sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(), HashMap::default(),);
    }

    #[ip_test]
    fn test_remove_udp_unbound_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            group,
            MulticastInterfaceSelector::LocalAddress(local_ip::<I>()).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(),
            HashMap::from([(
                (MultipleDevicesId::A, group),
                const_unwrap_option(NonZeroUsize::new(1))
            )])
        );

        let _: SocketInfo<I::Addr, _> =
            SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, unbound);
        assert_eq!(sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(), HashMap::default());
    }

    #[ip_test]
    fn test_remove_udp_listener_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let local_ip = local_ip::<I>();
        let local_port = NonZeroU16::new(100).unwrap();

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let first_group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            first_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(local_port),
        )
        .expect("listen_udp failed");
        let second_group = I::get_multicast_addr(5);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            second_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(),
            HashMap::from([
                ((MultipleDevicesId::A, first_group), const_unwrap_option(NonZeroUsize::new(1))),
                ((MultipleDevicesId::A, second_group), const_unwrap_option(NonZeroUsize::new(1)))
            ])
        );

        let _: SocketInfo<I::Addr, _> =
            SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, socket);
        assert_eq!(sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(), HashMap::default());
    }

    #[ip_test]
    fn test_remove_udp_connected_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(
                UdpMultipleDevicesSyncCtx::new_multiple_devices::<I>(),
            );
        let local_ip = local_ip::<I>();

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let first_group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            first_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        let second_group = I::get_multicast_addr(5);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            second_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(),
            HashMap::from([
                ((MultipleDevicesId::A, first_group), const_unwrap_option(NonZeroUsize::new(1))),
                ((MultipleDevicesId::A, second_group), const_unwrap_option(NonZeroUsize::new(1)))
            ])
        );

        let _: SocketInfo<I::Addr, _> =
            SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, socket);
        assert_eq!(sync_ctx.inner.inner.get_ref().multicast_memberships::<I>(), HashMap::default());
    }

    #[ip_test]
    #[should_panic(expected = "listen again failed")]
    fn test_listen_udp_removes_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = local_ip::<I>();
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");

        // Attempting to create a new listener from the same unbound ID should
        // panic since the unbound socket ID is now invalid.
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            NonZeroU16::new(200),
        )
        .expect("listen again failed");
    }

    #[ip_test]
    fn test_get_conn_info<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>()).into();
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>()).into();
        // Create a UDP connection with a specified local port and local IP.
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(local_ip),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        assert_eq!(info.local_ip, local_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.local_port.get(), 100);
        assert_eq!(info.remote_ip, remote_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.remote_port.get(), 200);
    }

    #[ip_test]
    fn test_get_listener_info<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>()).into();

        // Check getting info on specified listener.
        let specified = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            specified,
            Some(local_ip),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, specified);
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip.unwrap(), local_ip.into_inner().map_zone(FakeWeakDeviceId).into());
        assert_eq!(info.local_port.get(), 100);

        // Check getting info on wildcard listener.
        let wildcard = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            wildcard,
            None,
            NonZeroU16::new(200),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, wildcard);
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip, None);
        assert_eq!(info.local_port.get(), 200);
    }

    #[ip_test]
    fn test_get_reuse_port<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let first = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, first),
            false,
        );

        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, first, true)
            .expect("is unbound");

        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, first),
            true
        );

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            first,
            Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
            None,
        )
        .expect("listen failed");
        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, first),
            true
        );
        let _: SocketInfo<_, _> = SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, first);

        let second = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, second, true)
            .expect("is unbound");
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            second,
            Some(ZonedAddr::Unzoned(remote_ip::<I>()).into()),
            const_unwrap_option(NonZeroU16::new(569)),
        )
        .expect("connect failed");

        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, second),
            true
        );
    }

    #[ip_test]
    fn test_get_bound_device_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, unbound),
            None
        );

        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, unbound, Some(&FakeDeviceId))
            .unwrap();
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, unbound),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[ip_test]
    fn test_get_bound_device_listener<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, Some(&FakeDeviceId))
            .unwrap();
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
            Some(NonZeroU16::new(100).unwrap()),
        )
        .expect("failed to listen");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, None)
            .expect("failed to set device");
        assert_eq!(SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket), None);
    }

    #[ip_test]
    fn test_get_bound_device_connected<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, Some(&FakeDeviceId))
            .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip::<I>()).into()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("failed to connect");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, None)
            .expect("failed to set device");
        assert_eq!(SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket), None);
    }

    #[ip_test]
    fn test_listen_udp_forwards_errors<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let remote_ip = remote_ip::<I>();

        // Check listening to a non-local IP fails.
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let listen_err = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            NonZeroU16::new(100),
        )
        .expect_err("listen_udp unexpectedly succeeded");
        assert_eq!(listen_err, Either::Right(LocalAddressError::CannotBindToAddress));

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let _ = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            NonZeroU16::new(200),
        )
        .expect("listen_udp failed");
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let listen_err = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            NonZeroU16::new(200),
        )
        .expect_err("listen_udp unexpectedly succeeded");
        assert_eq!(listen_err, Either::Right(LocalAddressError::AddressInUse));
    }

    const IPV6_LINK_LOCAL_ADDR: Ipv6Addr = net_ip_v6!("fe80::1234");
    #[test_case(IPV6_LINK_LOCAL_ADDR, IPV6_LINK_LOCAL_ADDR; "unicast")]
    #[test_case(IPV6_LINK_LOCAL_ADDR, MulticastAddr::new(net_ip_v6!("ff02::1234")).unwrap().get(); "multicast")]
    fn test_listen_udp_ipv6_link_local_requires_zone(
        interface_addr: Ipv6Addr,
        bind_addr: Ipv6Addr,
    ) {
        type I = Ipv6;
        let interface_addr = LinkLocalAddr::new(interface_addr).unwrap().into_specified();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(
                vec![interface_addr],
                vec![remote_ip::<I>()],
            ));

        let bind_addr = LinkLocalAddr::new(bind_addr).unwrap().into_specified();
        assert!(bind_addr.scope().can_have_zone());

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(bind_addr).into()),
            NonZeroU16::new(200),
        );
        assert_eq!(
            result,
            Err(Either::Right(LocalAddressError::Zone(ZonedAddressError::RequiredZoneNotProvided)))
        );
    }

    #[test_case(MultipleDevicesId::A, Ok(()); "matching")]
    #[test_case(MultipleDevicesId::B, Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "not matching")]
    fn test_listen_udp_ipv6_link_local_with_bound_device_set(
        zone_id: MultipleDevicesId,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                ),
            ));

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, zone_id).unwrap()).into()),
            NonZeroU16::new(200),
        )
        .map_err(Either::unwrap_right);
        assert_eq!(result, expected_result);
    }

    #[test_case(MultipleDevicesId::A, Ok(()); "matching")]
    #[test_case(MultipleDevicesId::B, Err(LocalAddressError::AddressMismatch); "not matching")]
    fn test_listen_udp_ipv6_link_local_with_zone_requires_addr_assigned_to_device(
        zone_id: MultipleDevicesId,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                ),
            ));

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, zone_id).unwrap()).into()),
            NonZeroU16::new(200),
        )
        .map_err(Either::unwrap_right);
        assert_eq!(result, expected_result);
    }

    #[test_case(None, Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "clear device")]
    #[test_case(Some(MultipleDevicesId::A), Ok(()); "set same device")]
    #[test_case(Some(MultipleDevicesId::B),
                Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "change device")]
    fn test_listen_udp_ipv6_listen_link_local_update_bound_device(
        new_device: Option<MultipleDevicesId>,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                ),
            ));

        let socket = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, MultipleDevicesId::A).unwrap()).into()),
            NonZeroU16::new(200),
        )
        .expect("listen failed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(MultipleDevicesId::A))
        );

        assert_eq!(
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                new_device.as_ref()
            ),
            expected_result.map_err(SocketError::Local),
        );
    }

    #[test_case(None; "bind all IPs")]
    #[test_case(Some(ZonedAddr::Unzoned(local_ip::<Ipv6>())); "bind unzoned")]
    #[test_case(Some(ZonedAddr::Zoned(AddrAndZone::new(SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap(),
        MultipleDevicesId::A).unwrap())); "bind with same zone")]
    fn test_udp_ipv6_connect_with_unzoned(
        bound_addr: Option<ZonedAddr<SpecifiedAddr<Ipv6Addr>, MultipleDevicesId>>,
    ) {
        let remote_ips = vec![remote_ip::<Ipv6>()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new([
                    FakeDeviceConfig {
                        device: MultipleDevicesId::A,
                        local_ips: vec![
                            local_ip::<Ipv6>(),
                            SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap(),
                        ],
                        remote_ips: remote_ips.clone(),
                    },
                    FakeDeviceConfig {
                        device: MultipleDevicesId::B,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap()],
                        remote_ips: remote_ips,
                    },
                ]),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            bound_addr.map(SocketZonedIpAddr::from),
            Some(LOCAL_PORT),
        )
        .unwrap();

        assert_matches!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(remote_ip::<Ipv6>()).into()),
                REMOTE_PORT,
            ),
            Ok(())
        );
    }

    #[test]
    fn test_udp_ipv6_connect_zoned_get_info() {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let remote_ips = vec![remote_ip::<Ipv6>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<Ipv6>())]
                        .map(|(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        }),
                ),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let zoned_local_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr, MultipleDevicesId::A).unwrap()).into();
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(zoned_local_addr),
            Some(LOCAL_PORT),
        )
        .unwrap();

        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip::<Ipv6>()).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Connected(ConnInfo {
                local_ip: zoned_local_addr.into_inner().map_zone(FakeWeakDeviceId).into(),
                local_port: LOCAL_PORT,
                remote_ip: ZonedAddr::Unzoned(remote_ip::<Ipv6>()).into(),
                remote_port: REMOTE_PORT,
            })
        );
    }

    #[test_case(ZonedAddr::Zoned(AddrAndZone::new(SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap(),
        MultipleDevicesId::B).unwrap()),
        Err(ConnectError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "connect to different zone")]
    #[test_case(ZonedAddr::Unzoned(SpecifiedAddr::new(net_ip_v6!("fe80::3")).unwrap()),
        Ok(FakeWeakDeviceId(MultipleDevicesId::A)); "connect implicit zone")]
    fn test_udp_ipv6_bind_zoned(
        remote_addr: ZonedAddr<SpecifiedAddr<Ipv6Addr>, MultipleDevicesId>,
        expected: Result<FakeWeakDeviceId<MultipleDevicesId>, ConnectError>,
    ) {
        let remote_ips = vec![SpecifiedAddr::new(net_ip_v6!("fe80::3")).unwrap()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new([
                    FakeDeviceConfig {
                        device: MultipleDevicesId::A,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap()],
                        remote_ips: remote_ips.clone(),
                    },
                    FakeDeviceConfig {
                        device: MultipleDevicesId::B,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap()],
                        remote_ips: remote_ips,
                    },
                ]),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(
                ZonedAddr::Zoned(
                    AddrAndZone::new(
                        SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap(),
                        MultipleDevicesId::A,
                    )
                    .unwrap(),
                )
                .into(),
            ),
            Some(LOCAL_PORT),
        )
        .unwrap();

        let result = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(remote_addr.into()),
            REMOTE_PORT,
        )
        .map(|()| {
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket).unwrap()
        });
        assert_eq!(result, expected);
    }

    #[ip_test]
    fn test_listen_udp_loopback_no_zone_is_required<I: Ip + TestIpExt>() {
        let loopback_addr = I::LOOPBACK_ADDRESS;
        let remote_ips = vec![remote_ip::<I>()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, loopback_addr),
                        (MultipleDevicesId::B, local_ip::<I>()),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![local_ip],
                        remote_ips: remote_ips.clone(),
                    }),
                ),
            ));

        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(loopback_addr).into()),
            NonZeroU16::new(200),
        );
        assert_matches!(result, Ok(_));
    }

    #[test_case(None, true, Ok(()); "connected success")]
    #[test_case(None, false, Ok(()); "listening success")]
    #[test_case(Some(MultipleDevicesId::A), true, Ok(()); "conn bind same device")]
    #[test_case(Some(MultipleDevicesId::A), false, Ok(()); "listen bind same device")]
    #[test_case(
        Some(MultipleDevicesId::B),
        true,
        Err(SendToError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "conn bind different device")]
    #[test_case(
        Some(MultipleDevicesId::B),
        false,
        Err(SendToError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "listen bind different device")]
    fn test_udp_ipv6_send_to_zoned(
        bind_device: Option<MultipleDevicesId>,
        connect: bool,
        expected: Result<(), SendToError>,
    ) {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));
        let conn_remote_ip = Ipv6::get_other_remote_ip_address(1);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, Ipv6::get_other_ip_address(1)),
                        (MultipleDevicesId::B, Ipv6::get_other_ip_address(2)),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![local_ip],
                        remote_ips: vec![ll_addr, conn_remote_ip],
                    }),
                ),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);

        if let Some(device) = bind_device {
            SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, Some(&device))
                .unwrap();
        }

        let send_to_remote_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr, MultipleDevicesId::A).unwrap()).into();
        let result = if connect {
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(conn_remote_ip).into()),
                REMOTE_PORT,
            )
            .expect("connect should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(send_to_remote_addr),
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        } else {
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                None,
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(send_to_remote_addr),
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        };

        assert_eq!(result.map_err(|err| assert_matches!(err, Either::Right(e) => e)), expected);
    }

    #[test_case(true; "connected")]
    #[test_case(false; "listening")]
    fn test_udp_ipv6_bound_zoned_send_to_zoned(connect: bool) {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::5678")).unwrap().into_specified();
        let device_a_local_ip = net_ip_v6!("fe80::1111");
        let conn_remote_ip = Ipv6::get_other_remote_ip_address(1);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeDualStackIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, device_a_local_ip),
                        (MultipleDevicesId::B, net_ip_v6!("fe80::2222")),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![LinkLocalAddr::new(local_ip).unwrap().into_specified()],
                        remote_ips: vec![ll_addr, conn_remote_ip],
                    }),
                ),
            ));

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(
                ZonedAddr::Zoned(
                    AddrAndZone::new(
                        SpecifiedAddr::new(device_a_local_ip).unwrap(),
                        MultipleDevicesId::A,
                    )
                    .unwrap(),
                )
                .into(),
            ),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");

        // Use a remote address on device B, while the socket is listening on
        // device A. This should cause a failure when sending.
        let send_to_remote_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr, MultipleDevicesId::B).unwrap()).into();

        let result = if connect {
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(ZonedAddr::Unzoned(conn_remote_ip).into()),
                REMOTE_PORT,
            )
            .expect("connect should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(send_to_remote_addr),
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        } else {
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(send_to_remote_addr),
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        };

        assert_matches!(
            result,
            Err(Either::Right(SendToError::Zone(ZonedAddressError::DeviceZoneMismatch)))
        );
    }

    #[test_case(None; "removes implicit")]
    #[test_case(Some(FakeDeviceId); "preserves implicit")]
    fn test_connect_disconnect_affects_bound_device(bind_device: Option<FakeDeviceId>) {
        // If a socket is bound to an unzoned address, whether or not it has a
        // bound device should be restored after `connect` then `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let local_ip = local_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![local_ip], vec![ll_addr]),
        );

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, bind_device.as_ref())
            .unwrap();

        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(LOCAL_PORT),
        )
        .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, FakeDeviceId).unwrap()).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, socket)
            .expect("was connected");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            bind_device.map(FakeWeakDeviceId),
        );
    }

    #[test]
    fn test_bind_zoned_addr_connect_disconnect() {
        // If a socket is bound to a zoned address, the address's device should
        // be retained after `connect` then `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let remote_ip = remote_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![ll_addr], vec![remote_ip]),
        );

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, FakeDeviceId).unwrap()).into()),
            Some(LOCAL_PORT),
        )
        .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, socket)
            .expect("was connected");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[test]
    fn test_bind_device_after_connect_persists_after_disconnect() {
        // If a socket is bound to an unzoned address, connected to a zoned address, and then has
        // its device set, the device should be *retained* after `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let local_ip = local_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(vec![local_ip], vec![ll_addr]),
        );

        let socket = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Unzoned(local_ip).into()),
            Some(LOCAL_PORT),
        )
        .unwrap();
        SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr, FakeDeviceId).unwrap()).into()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        // This is a no-op functionally since the socket is already bound to the
        // device but it implies that we shouldn't unbind the device on
        // disconnect.
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, socket, Some(&FakeDeviceId))
            .expect("binding same device should succeed");

        SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, socket)
            .expect("was connected");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, socket),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[ip_test]
    fn test_remove_udp_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
        let unbound = SocketHandler::<I, _>::create_udp(&mut sync_ctx);
        let _: SocketInfo<_, _> = SocketHandler::close(&mut sync_ctx, &mut non_sync_ctx, unbound);

        let Wrapped { outer: sockets_state, inner: _ } = &sync_ctx;
        assert_matches!(sockets_state.as_ref().get_socket_state(&unbound), None)
    }

    #[ip_test]
    fn test_hop_limits_used_for_sending_packets<I: Ip + TestIpExt>() {
        let some_multicast_addr: MulticastAddr<I::Addr> = I::map_ip(
            (),
            |()| Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
            |()| MulticastAddr::new(net_ip_v6!("ff0e::1")).unwrap(),
        );

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::with_local_remote_ip_addrs(
                vec![local_ip::<I>()],
                vec![remote_ip::<I>(), some_multicast_addr.into_specified()],
            ));
        let listener = SocketHandler::<I, _>::create_udp(&mut sync_ctx);

        const UNICAST_HOPS: NonZeroU8 = const_unwrap_option(NonZeroU8::new(23));
        const MULTICAST_HOPS: NonZeroU8 = const_unwrap_option(NonZeroU8::new(98));
        SocketHandler::set_udp_unicast_hop_limit(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            Some(UNICAST_HOPS),
        );
        SocketHandler::set_udp_multicast_hop_limit(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            Some(MULTICAST_HOPS),
        );

        SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, listener, None, None)
            .expect("listen failed");

        let mut send_and_get_ttl = |remote_ip| {
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                const_unwrap_option(NonZeroU16::new(9090)),
                Buf::new(vec![], ..),
            )
            .expect("send failed");

            let (meta, _body) = sync_ctx.inner.inner.frames().last().unwrap();
            let SendIpPacketMeta {
                device: _,
                src_ip: _,
                dst_ip,
                next_hop: _,
                proto: _,
                ttl,
                mtu: _,
            } = meta.try_as::<I>().unwrap();
            assert_eq!(*dst_ip, remote_ip);
            *ttl
        };

        assert_eq!(send_and_get_ttl(some_multicast_addr.into_specified()), Some(MULTICAST_HOPS));
        assert_eq!(send_and_get_ttl(remote_ip::<I>()), Some(UNICAST_HOPS));
    }

    const DUAL_STACK_ANY_ADDR: Ipv6Addr = net_ip_v6!("::");
    const DUAL_STACK_V4_ANY_ADDR: Ipv6Addr = net_ip_v6!("::FFFF:0.0.0.0");

    #[derive(Copy, Clone, Debug)]
    enum DualStackBindAddr {
        Any,
        V4Any,
        V4Specific,
    }

    impl DualStackBindAddr {
        const fn v6_addr(&self) -> Option<Ipv6Addr> {
            match self {
                Self::Any => Some(DUAL_STACK_ANY_ADDR),
                Self::V4Any => Some(DUAL_STACK_V4_ANY_ADDR),
                Self::V4Specific => None,
            }
        }
    }
    const V4_LOCAL_IP: Ipv4Addr = ip_v4!("192.168.1.10");
    const V4_LOCAL_IP_MAPPED: Ipv6Addr = net_ip_v6!("::ffff:192.168.1.10");
    const V6_LOCAL_IP: Ipv6Addr = net_ip_v6!("2201::1");

    /// Enables dual stack operations on the given unbound socket.
    ///
    /// # Panics
    ///
    /// Panics if dual stack operations could not be enabled. Note that this
    /// operation will always fail for bound sockets.
    fn enable_dual_stack_for_test<C, SC: SocketHandler<Ipv6, C>>(
        sync_ctx: &mut SC,
        non_sync_ctx: &mut C,
        socket: SocketId<Ipv6>,
    ) {
        // TODO(https://fxbug.dev/21198): Remove this fn once IPv6 sockets are
        // dual-stack-enabled by default.
        SocketHandler::<Ipv6, _>::set_dual_stack_enabled(sync_ctx, non_sync_ctx, socket, true)
            .expect("can set dual-stack enabled");
        assert_eq!(
            SocketHandler::<Ipv6, _>::get_dual_stack_enabled(sync_ctx, non_sync_ctx, socket),
            Ok(true)
        );
    }

    #[test_case(DualStackBindAddr::Any; "dual-stack")]
    #[test_case(DualStackBindAddr::V4Any; "v4 any")]
    #[test_case(DualStackBindAddr::V4Specific; "v4 specific")]
    fn dual_stack_delivery(bind_addr: DualStackBindAddr) {
        const REMOTE_IP: Ipv4Addr = ip_v4!("8.8.8.8");
        const REMOTE_IP_MAPPED: Ipv6Addr = net_ip_v6!("::ffff:8.8.8.8");
        let bind_addr = bind_addr.v6_addr().unwrap_or(V4_LOCAL_IP_MAPPED);
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs(
                vec![SpecifiedAddr::new(V4_LOCAL_IP).unwrap()],
                vec![SpecifiedAddr::new(REMOTE_IP).unwrap()],
            ));

        let listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        enable_dual_stack_for_test(&mut sync_ctx, &mut non_sync_ctx, listener);
        SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            SpecifiedAddr::new(bind_addr).map(|a| ZonedAddr::Unzoned(a).into()),
            Some(LOCAL_PORT),
        )
        .expect("can bind");

        const BODY: &[u8] = b"abcde";
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            REMOTE_IP,
            V4_LOCAL_IP,
            REMOTE_PORT,
            LOCAL_PORT,
            BODY,
        );

        assert_eq!(
            non_sync_ctx.state().received::<Ipv6>(),
            &HashMap::from([(
                listener,
                SocketReceived {
                    errors: vec![],
                    packets: vec![ReceivedPacket {
                        body: BODY.into(),
                        addr: ReceivedPacketAddrs {
                            dst_ip: V4_LOCAL_IP_MAPPED,
                            src_ip: REMOTE_IP_MAPPED,
                            src_port: Some(REMOTE_PORT),
                        }
                    }],
                }
            )])
        );
    }

    #[test_case(DualStackBindAddr::Any, true; "dual-stack any bind v4 first")]
    #[test_case(DualStackBindAddr::V4Any, true; "v4 any bind v4 first")]
    #[test_case(DualStackBindAddr::V4Specific, true; "v4 specific bind v4 first")]
    #[test_case(DualStackBindAddr::Any, false; "dual-stack any bind v4 second")]
    #[test_case(DualStackBindAddr::V4Any, false; "v4 any bind v4 second")]
    #[test_case(DualStackBindAddr::V4Specific, false; "v4 specific bind v4 second")]
    fn dual_stack_bind_conflict(bind_addr: DualStackBindAddr, bind_v4_first: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs(
                vec![SpecifiedAddr::new(V4_LOCAL_IP).unwrap()],
                vec![],
            ));

        let v4_listener = SocketHandler::<Ipv4, _>::create_udp(&mut sync_ctx);
        let v6_listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);

        enable_dual_stack_for_test(&mut sync_ctx, &mut non_sync_ctx, v6_listener);

        let bind_v4 = |sync_ctx, non_sync_ctx| {
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                v4_listener,
                SpecifiedAddr::new(V4_LOCAL_IP).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            )
        };
        let bind_v6 = |sync_ctx, non_sync_ctx| {
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                v6_listener,
                SpecifiedAddr::new(bind_addr.v6_addr().unwrap_or(V4_LOCAL_IP_MAPPED))
                    .map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            )
        };

        let second_bind_error = if bind_v4_first {
            bind_v4(&mut sync_ctx, &mut non_sync_ctx).expect("no conflict");
            bind_v6(&mut sync_ctx, &mut non_sync_ctx).expect_err("should conflict")
        } else {
            bind_v6(&mut sync_ctx, &mut non_sync_ctx).expect("no conflict");
            bind_v4(&mut sync_ctx, &mut non_sync_ctx).expect_err("should conflict")
        };
        assert_eq!(second_bind_error, Either::Right(LocalAddressError::AddressInUse));
    }

    #[test_case(DualStackBindAddr::V4Any; "v4 any")]
    #[test_case(DualStackBindAddr::V4Specific; "v4 specific")]
    fn dual_stack_enable(bind_addr: DualStackBindAddr) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs(
                vec![SpecifiedAddr::new(V4_LOCAL_IP).unwrap()],
                vec![],
            ));

        let bind_addr = bind_addr.v6_addr().unwrap_or(V4_LOCAL_IP_MAPPED);
        let listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);

        // TODO(https://fxbug.dev/21198): Update this test this once IPv6
        // sockets are dual-stack-enabled by default.
        assert_eq!(
            SocketHandler::<Ipv6, _>::get_dual_stack_enabled(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener
            ),
            Ok(false)
        );
        // With dual-stack behavior disabled, the IPv6 socket can't bind to
        // an IPv4-mapped IPv6 address.
        assert_eq!(
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                SpecifiedAddr::new(bind_addr).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            ),
            Err(Either::Right(LocalAddressError::CannotBindToAddress))
        );
        enable_dual_stack_for_test(&mut sync_ctx, &mut non_sync_ctx, listener);
        // Try again now that dual-stack sockets are enabled.
        assert_eq!(
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                SpecifiedAddr::new(bind_addr).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            ),
            Ok(())
        );
    }

    #[test]
    fn dual_stack_bind_unassigned_v4_address() {
        const NOT_ASSIGNED_MAPPED: Ipv6Addr = net_ip_v6!("::ffff:8.8.8.8");
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs(
                vec![SpecifiedAddr::new(V4_LOCAL_IP).unwrap()],
                vec![],
            ));

        let listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        enable_dual_stack_for_test(&mut sync_ctx, &mut non_sync_ctx, listener);
        assert_eq!(
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                SpecifiedAddr::new(NOT_ASSIGNED_MAPPED).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            ),
            Err(Either::Right(LocalAddressError::CannotBindToAddress))
        );
    }

    #[test_case(net_ip_v6!("::"), true; "dual stack any")]
    #[test_case(net_ip_v6!("::"), false; "v6 any")]
    #[test_case(net_ip_v6!("::ffff:0.0.0.0"), true; "v4 unspecified")]
    #[test_case(V4_LOCAL_IP_MAPPED, true; "v4 specified")]
    #[test_case(V6_LOCAL_IP, true; "v6 specified dual stack enabled")]
    #[test_case(V6_LOCAL_IP, false; "v6 specified dual stack disabled")]
    fn dual_stack_get_info(bind_addr: Ipv6Addr, enable_dual_stack: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs::<
                SpecifiedAddr<IpAddr>,
            >(
                vec![
                    SpecifiedAddr::new(V4_LOCAL_IP).unwrap().into(),
                    SpecifiedAddr::new(V6_LOCAL_IP).unwrap().into(),
                ],
                vec![],
            ));

        let listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::<Ipv6, _>::set_dual_stack_enabled(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener,
            enable_dual_stack,
        )
        .expect("can set dual-stack enabled");
        let bind_addr = SpecifiedAddr::new(bind_addr);
        assert_eq!(
            SocketHandler::<Ipv6, _>::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                bind_addr.map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            ),
            Ok(())
        );

        assert_eq!(
            SocketHandler::<Ipv6, _>::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, listener),
            SocketInfo::Listener(ListenerInfo {
                local_ip: bind_addr.map(|a| ZonedAddr::Unzoned(a).into()),
                local_port: LOCAL_PORT,
            })
        );
    }

    #[test_case(net_ip_v6!("::"), true; "dual stack any")]
    #[test_case(net_ip_v6!("::"), false; "v6 any")]
    #[test_case(net_ip_v6!("::ffff:0.0.0.0"), true; "v4 unspecified")]
    #[test_case(V4_LOCAL_IP_MAPPED, true; "v4 specified")]
    #[test_case(V6_LOCAL_IP, true; "v6 specified dual stack enabled")]
    #[test_case(V6_LOCAL_IP, false; "v6 specified dual stack disabled")]
    fn dual_stack_remove_listener(bind_addr: Ipv6Addr, enable_dual_stack: bool) {
        // Ensure that when a socket is removed, it doesn't leave behind state
        // in the demultiplexing maps. Do this by binding a new socket at the
        // same address and asserting success.
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs::<
                SpecifiedAddr<IpAddr>,
            >(
                vec![
                    SpecifiedAddr::new(V4_LOCAL_IP).unwrap().into(),
                    SpecifiedAddr::new(V6_LOCAL_IP).unwrap().into(),
                ],
                vec![],
            ));

        let mut bind_listener = || {
            let listener = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
            SocketHandler::<Ipv6, _>::set_dual_stack_enabled(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener,
                enable_dual_stack,
            )
            .expect("can set dual-stack enabled");
            let bind_addr = SpecifiedAddr::new(bind_addr);
            assert_eq!(
                SocketHandler::<Ipv6, _>::listen_udp(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    listener,
                    bind_addr.map(|a| ZonedAddr::Unzoned(a).into()),
                    Some(LOCAL_PORT),
                ),
                Ok(())
            );

            assert_eq!(
                SocketHandler::<Ipv6, _>::close(&mut sync_ctx, &mut non_sync_ctx, listener),
                SocketInfo::Listener(ListenerInfo {
                    local_ip: bind_addr.map(|a| ZonedAddr::Unzoned(a).into()),
                    local_port: LOCAL_PORT,
                })
            );
        };

        // The first time should succeed because the state is empty.
        bind_listener();
        // The second time should succeed because the first removal didn't
        // leave any state behind.
        bind_listener();
    }

    // TODO(https://fxbug.dev/135204): Expand the test cases once dual-stack
    // connect is supported.
    #[test_case(true; "connect to this stack with dual stack enabled")]
    #[test_case(false; "connect to this stack with dual stack disabled")]
    fn dual_stack_connect(enable_dual_stack: bool) {
        let FakeCtxWithSyncCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtxWithSyncCtx::with_sync_ctx(FakeUdpSyncCtx::with_local_remote_ip_addrs(
                vec![Ipv6::FAKE_CONFIG.local_ip],
                vec![Ipv6::FAKE_CONFIG.remote_ip],
            ));

        let connected = SocketHandler::<Ipv6, _>::create_udp(&mut sync_ctx);
        SocketHandler::<Ipv6, _>::set_dual_stack_enabled(
            &mut sync_ctx,
            &mut non_sync_ctx,
            connected,
            enable_dual_stack,
        )
        .expect("can set dual-stack enabled");

        assert_eq!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                connected,
                Some(ZonedAddr::Unzoned(Ipv6::FAKE_CONFIG.remote_ip).into()),
                REMOTE_PORT,
            ),
            Ok(())
        );
    }

    #[test]
    fn test_icmp_error() {
        struct InitializedContext<I: TestIpExt> {
            ctx: UdpFakeDeviceCtx,
            wildcard_listener: SocketId<I>,
            specific_listener: SocketId<I>,
            connection: SocketId<I>,
        }
        // Create a context with:
        // - A wildcard listener on port 1
        // - A listener on the local IP and port 2
        // - A connection from the local IP to the remote IP on local port 2 and
        //   remote port 3
        fn initialize_context<I: TestIpExt>() -> InitializedContext<I> {
            let mut ctx =
                UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::new_fake_device::<I>());
            let UdpFakeDeviceCtx { sync_ctx, non_sync_ctx } = &mut ctx;
            let wildcard_listener = SocketHandler::create_udp(sync_ctx);
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                wildcard_listener,
                None,
                Some(NonZeroU16::new(1).unwrap()),
            )
            .unwrap();

            let specific_listener = SocketHandler::create_udp(sync_ctx);
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                specific_listener,
                Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
                Some(NonZeroU16::new(2).unwrap()),
            )
            .unwrap();

            let connection = SocketHandler::create_udp(sync_ctx);
            SocketHandler::listen_udp(
                sync_ctx,
                non_sync_ctx,
                connection,
                Some(ZonedAddr::Unzoned(local_ip::<I>()).into()),
                Some(NonZeroU16::new(3).unwrap()),
            )
            .unwrap();
            SocketHandler::connect(
                sync_ctx,
                non_sync_ctx,
                connection,
                Some(ZonedAddr::Unzoned(remote_ip::<I>()).into()),
                NonZeroU16::new(4).unwrap(),
            )
            .unwrap();
            InitializedContext { ctx, wildcard_listener, specific_listener, connection }
        }

        // Serialize a UDP-in-IP packet with the given values, and then receive
        // an ICMP error message with that packet as the original packet.
        fn receive_icmp_error<
            I: TestIpExt,
            F: Fn(&mut UdpFakeDeviceSyncCtx, &mut UdpFakeDeviceNonSyncCtx, &[u8], I::ErrorCode),
        >(
            sync_ctx: &mut UdpFakeDeviceSyncCtx,
            ctx: &mut UdpFakeDeviceNonSyncCtx,
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: u16,
            dst_port: u16,
            err: I::ErrorCode,
            f: F,
        ) where
            I::PacketBuilder: core::fmt::Debug,
        {
            let packet = (&[0u8][..])
                .into_serializer()
                .encapsulate(UdpPacketBuilder::new(
                    src_ip,
                    dst_ip,
                    NonZeroU16::new(src_port),
                    NonZeroU16::new(dst_port).unwrap(),
                ))
                .encapsulate(I::PacketBuilder::new(src_ip, dst_ip, 64, IpProto::Udp.into()))
                .serialize_vec_outer()
                .unwrap();
            f(sync_ctx, ctx, packet.as_ref(), err);
        }

        fn test<
            I: TestIpExt + PartialEq,
            F: Copy + Fn(&mut UdpFakeDeviceSyncCtx, &mut UdpFakeDeviceNonSyncCtx, &[u8], I::ErrorCode),
        >(
            err: I::ErrorCode,
            f: F,
            other_remote_ip: I::Addr,
        ) where
            I::PacketBuilder: core::fmt::Debug,
            I::ErrorCode: Copy + core::fmt::Debug + PartialEq,
        {
            let InitializedContext {
                ctx: UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx },
                wildcard_listener,
                specific_listener,
                connection,
            } = initialize_context::<I>();

            let src_ip = local_ip::<I>();
            let dst_ip = remote_ip::<I>();
            let mut expected_errors = HashMap::<SocketId<I>, Vec<_>>::new();

            // Test that we receive an error for the connection.
            receive_icmp_error::<I, _>(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                3,
                4,
                err,
                f,
            );
            expected_errors.entry(connection).or_default().push(err);
            assert_eq!(non_sync_ctx.state().errors(), expected_errors);

            // Test that we receive an error for the listener.
            receive_icmp_error::<I, _>(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                2,
                4,
                err,
                f,
            );
            expected_errors.entry(specific_listener).or_default().push(err);
            assert_eq!(non_sync_ctx.state().errors(), expected_errors);

            // Test that we receive an error for the wildcard listener.
            receive_icmp_error::<I, _>(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                1,
                4,
                err,
                f,
            );
            expected_errors.entry(wildcard_listener).or_default().push(err);
            assert_eq!(non_sync_ctx.state().errors(), expected_errors);

            // Test that we receive an error for the wildcard listener even if
            // the original packet was sent to a different remote IP/port.
            receive_icmp_error::<I, _>(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                other_remote_ip,
                1,
                5,
                err,
                f,
            );
            expected_errors.entry(wildcard_listener).or_default().push(err);
            assert_eq!(non_sync_ctx.state().errors(), expected_errors);

            // Test that an error that doesn't correspond to any connection or
            // listener isn't received.
            receive_icmp_error::<I, _>(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                3,
                5,
                err,
                f,
            );
            assert_eq!(non_sync_ctx.state().errors(), expected_errors);
        }

        test::<Ipv4, _>(
            Icmpv4ErrorCode::DestUnreachable(Icmpv4DestUnreachableCode::DestNetworkUnreachable),
            |sync_ctx: &mut UdpFakeDeviceSyncCtx,
             ctx: &mut UdpFakeDeviceNonSyncCtx,
             mut packet,
             error_code| {
                let packet = packet.parse::<Ipv4PacketRaw<_>>().unwrap();
                let device = FakeDeviceId;
                let src_ip = SpecifiedAddr::new(packet.src_ip());
                let dst_ip = SpecifiedAddr::new(packet.dst_ip()).unwrap();
                let body = packet.body().into_inner();
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    sync_ctx, ctx, &device, src_ip, dst_ip, body, error_code,
                )
            },
            Ipv4Addr::new([1, 2, 3, 4]),
        );

        test::<Ipv6, _>(
            Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute),
            |sync_ctx: &mut UdpFakeDeviceSyncCtx,
             ctx: &mut UdpFakeDeviceNonSyncCtx,
             mut packet,
             error_code| {
                let packet = packet.parse::<Ipv6PacketRaw<_>>().unwrap();
                let device = FakeDeviceId;
                let src_ip = SpecifiedAddr::new(packet.src_ip());
                let dst_ip = SpecifiedAddr::new(packet.dst_ip()).unwrap();
                let body = packet.body().unwrap().into_inner();
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    sync_ctx, ctx, &device, src_ip, dst_ip, body, error_code,
                )
            },
            Ipv6Addr::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8]),
        );
    }

    type FakeBoundSocketMap<I> = UdpBoundSocketMap<I, FakeWeakDeviceId<FakeDeviceId>>;

    fn listen<I: Ip + IpExt>(
        ip: I::Addr,
        port: u16,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec> {
        let addr = SpecifiedAddr::new(ip).map(SocketIpAddr::new_from_specified_or_panic);
        let port = NonZeroU16::new(port).expect("port must be nonzero");
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr, identifier: port },
            device: None,
        })
    }

    fn listen_device<I: Ip + IpExt>(
        ip: I::Addr,
        port: u16,
        device: FakeWeakDeviceId<FakeDeviceId>,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec> {
        let addr = SpecifiedAddr::new(ip).map(SocketIpAddr::new_from_specified_or_panic);
        let port = NonZeroU16::new(port).expect("port must be nonzero");
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr, identifier: port },
            device: Some(device),
        })
    }

    fn conn<I: Ip + IpExt>(
        local_ip: I::Addr,
        local_port: u16,
        remote_ip: I::Addr,
        remote_port: u16,
    ) -> AddrVec<I, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec> {
        let local_ip = SocketIpAddr::new(local_ip).expect("addr must be specified & non-mapped");
        let local_port = NonZeroU16::new(local_port).expect("port must be nonzero");
        let remote_ip = SocketIpAddr::new(remote_ip).expect("addr must be specified & non-mapped");
        let remote_port = NonZeroU16::new(remote_port).expect("port must be nonzero");
        AddrVec::Conn(ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) },
            device: None,
        })
    }

    #[test_case([
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive),
        (listen(ip_v4!("0.0.0.0"), 2), Sharing::Exclusive)],
            Ok(()); "listen_any_ip_different_port")]
    #[test_case([
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive),
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "any_ip_same_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "listen_same_specific_ip")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort)],
            Ok(()); "listen_same_specific_ip_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort)],
            Err(InsertError::Exists); "listen_same_specific_ip_exclusive_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "listen_same_specific_ip_reuse_port_exclusive")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::ReusePort)],
            Ok(()); "conn_shadows_listener_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_exclusive")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::ReusePort)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_exclusive_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_reuse_port_exclusive")]
    #[test_case([
        (listen_device(ip_v4!("1.1.1.1"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "conn_indirect_conflict_specific_listener")]
    #[test_case([
        (listen_device(ip_v4!("0.0.0.0"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "conn_indirect_conflict_any_listener")]
    #[test_case([
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        (listen_device(ip_v4!("1.1.1.1"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "specific_listener_indirect_conflict_conn")]
    #[test_case([
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        (listen_device(ip_v4!("0.0.0.0"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "any_listener_indirect_conflict_conn")]
    fn bind_sequence<
        C: IntoIterator<Item = (AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec>, Sharing)>,
    >(
        spec: C,
        expected: Result<(), InsertError>,
    ) {
        let mut map = FakeBoundSocketMap::<Ipv4>::default();
        let mut spec = spec.into_iter().enumerate().peekable();
        let mut try_insert = |(index, (addr, options))| match addr {
            AddrVec::Conn(c) => map
                .conns_mut()
                .try_insert(c, options, EitherIpSocket::V4(SocketId::from(index)))
                .map(|_| ())
                .map_err(|(e, _)| e),
            AddrVec::Listen(l) => map
                .listeners_mut()
                .try_insert(l, options, EitherIpSocket::V4(SocketId::from(index)))
                .map(|_| ())
                .map_err(|(e, _)| e),
        };
        let last = loop {
            let one_spec = spec.next().expect("empty list of test cases");
            if spec.peek().is_none() {
                break one_spec;
            } else {
                try_insert(one_spec).expect("intermediate bind failed")
            }
        };

        let result = try_insert(last);
        assert_eq!(result, expected);
    }

    #[test_case([
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
            (listen(ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        ]; "distinct")]
    #[test_case([
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        ]; "listen_reuse_port")]
    #[test_case([
            (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 3), Sharing::ReusePort),
            (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 3), Sharing::ReusePort),
        ]; "conn_reuse_port")]
    fn remove_sequence<I>(spec: I)
    where
        I: IntoIterator<
            Item = (AddrVec<Ipv4, FakeWeakDeviceId<FakeDeviceId>, IpPortSpec>, Sharing),
        >,
        I::IntoIter: ExactSizeIterator,
    {
        enum Socket<A: IpAddress, D, LI, RI> {
            Listener(SocketId<A::Version>, ListenerAddr<ListenerIpAddr<A, LI>, D>),
            Conn(SocketId<A::Version>, ConnAddr<ConnIpAddr<A, LI, RI>, D>),
        }
        let spec = spec.into_iter();
        let spec_len = spec.len();
        for spec in spec.permutations(spec_len) {
            let mut map = FakeBoundSocketMap::<Ipv4>::default();
            let sockets = spec
                .into_iter()
                .enumerate()
                .map(|(socket_index, (addr, options))| match addr {
                    AddrVec::Conn(c) => map
                        .conns_mut()
                        .try_insert(c, options, EitherIpSocket::V4(SocketId::from(socket_index)))
                        .map(|entry| {
                            Socket::Conn(
                                assert_matches!(entry.id(), EitherIpSocket::V4(id) => *id),
                                entry.get_addr().clone(),
                            )
                        })
                        .expect("insert_failed"),
                    AddrVec::Listen(l) => map
                        .listeners_mut()
                        .try_insert(l, options, EitherIpSocket::V4(SocketId::from(socket_index)))
                        .map(|entry| {
                            Socket::Listener(
                                assert_matches!(entry.id(), EitherIpSocket::V4(id) => *id),
                                entry.get_addr().clone(),
                            )
                        })
                        .expect("insert_failed"),
                })
                .collect::<Vec<_>>();

            for socket in sockets {
                match socket {
                    Socket::Listener(l, addr) => {
                        assert_matches!(
                            map.listeners_mut().remove(&EitherIpSocket::V4(l), &addr),
                            Ok(())
                        );
                    }
                    Socket::Conn(c, addr) => {
                        assert_matches!(
                            map.conns_mut().remove(&EitherIpSocket::V4(c), &addr),
                            Ok(())
                        );
                    }
                }
            }
        }
    }

    #[ip_test]
    #[test_case(true; "bind to device")]
    #[test_case(false; "no bind to device")]
    fn loopback_bind_to_device<I: Ip + IpExt + crate::testutil::TestIpExt>(bind_to_device: bool) {
        set_logger_for_test();
        const HELLO: &'static [u8] = b"Hello";
        let (mut ctx, local_device_ids) = I::FAKE_CONFIG.into_builder().build();
        let crate::testutil::Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let loopback_device_id: crate::DeviceId<crate::testutil::FakeNonSyncCtx> =
            crate::device::add_loopback_device(
                sync_ctx,
                net_types::ip::Mtu::new(u16::MAX as u32),
                crate::testutil::DEFAULT_INTERFACE_METRIC,
            )
            .expect("create the loopback interface")
            .into();
        crate::device::testutil::enable_device(sync_ctx, non_sync_ctx, &loopback_device_id);
        let socket = create_udp::<I, _>(sync_ctx);
        listen_udp(sync_ctx, non_sync_ctx, &socket, None, NonZeroU16::new(1234)).unwrap();
        if bind_to_device {
            set_udp_device(
                sync_ctx,
                non_sync_ctx,
                &socket,
                Some(&local_device_ids[0].clone().into()),
            )
            .unwrap();
        }
        send_udp_to(
            sync_ctx,
            non_sync_ctx,
            &socket,
            Some(SocketZonedIpAddr::from(ZonedAddr::Unzoned(I::FAKE_CONFIG.local_ip))),
            NonZeroU16::new(1234).unwrap(),
            Buf::new(HELLO.to_vec(), ..),
        )
        .unwrap();
        crate::testutil::handle_queued_rx_packets(sync_ctx, non_sync_ctx);

        // TODO(https://fxbug.dev/135041): They should both be non-empty. The
        // socket map should allow a looped back packet to be delivered despite
        // it being bound to a device other than loopback.
        if bind_to_device {
            assert_matches!(&non_sync_ctx.take_udp_received(socket)[..], []);
        } else {
            assert_matches!(
                &non_sync_ctx.take_udp_received(socket)[..],
                [packet] => assert_eq!(packet, HELLO)
            );
        }
    }
}
