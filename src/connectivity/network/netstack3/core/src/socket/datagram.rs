// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Shared code for implementing datagram sockets.

use alloc::collections::HashSet;
use core::{
    convert::Infallible as Never,
    fmt::Debug,
    hash::Hash,
    marker::PhantomData,
    num::NonZeroU8,
    ops::{Deref, DerefMut},
};

use dense_map::{DenseMap, Entry as DenseMapEntry, EntryKey};
use derivative::Derivative;
use either::Either;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpVersionMarker, Ipv4, Ipv6},
    MulticastAddr, MulticastAddress as _, SpecifiedAddr, ZonedAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::ip::IpProtoExt;
use thiserror::Error;

use crate::{
    algorithm::ProtocolFlowId,
    context::RngContext,
    convert::{BidirectionalConverter, OwnedOrRefsBidirectionalConverter},
    device::{self, AnyDevice, DeviceIdContext, Id, WeakId},
    error::{LocalAddressError, NotFoundError, RemoteAddressError, SocketError, ZonedAddressError},
    ip::{
        socket::{
            IpSock, IpSockCreateAndSendError, IpSockCreationError, IpSockSendError,
            IpSocketHandler, SendOneShotIpPacketError, SendOptions,
        },
        EitherDeviceId, HopLimits, MulticastMembershipHandler, TransportIpContext,
    },
    socket::{
        self,
        address::{
            dual_stack_remote_ip, try_unmap, AddrVecIter, ConnAddr, ConnIpAddr,
            DualStackConnIpAddr, DualStackListenerIpAddr, DualStackRemoteIp, ListenerIpAddr,
            SocketIpAddr, SocketZonedIpAddr, TryUnmapResult,
        },
        AddrVec, BoundSocketMap, EitherStack, ExistsError, InsertError, ListenerAddr,
        MaybeDualStack, NotDualStackCapableError, Shutdown, SocketMapAddrSpec,
        SocketMapConflictPolicy, SocketMapStateSpec,
    },
};

/// Datagram demultiplexing map.
pub(crate) type BoundSockets<I, D, A, S> = BoundSocketMap<I, D, A, S>;

/// Storage of state for all datagram sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct SocketsState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec>(
    DenseMap<SocketState<I, D, S>>,
);

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> Deref for SocketsState<I, D, S> {
    type Target = DenseMap<SocketState<I, D, S>>;

    fn deref(&self) -> &Self::Target {
        let Self(idmap) = self;
        idmap
    }
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> DerefMut for SocketsState<I, D, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let Self(idmap) = self;
        idmap
    }
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> SocketsState<I, D, S> {
    pub(crate) fn get_socket_state(&self, id: &S::SocketId<I>) -> Option<&SocketState<I, D, S>> {
        self.get(id.get_key_index())
    }
}

impl<I: IpExt, NewIp: IpExt, D: device::WeakId, S: DatagramSocketSpec> GenericOverIp<NewIp>
    for SocketsState<I, D, S>
{
    type Type = SocketsState<NewIp, D, S>;
}

pub(crate) trait IpExt:
    crate::ip::IpExt + DualStackIpExt + crate::ip::icmp::IcmpIpExt
{
}
impl<I: crate::ip::IpExt + DualStackIpExt + crate::ip::icmp::IcmpIpExt> IpExt for I {}

#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum SocketState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    Unbound(UnboundSocketState<I, D, S>),
    Bound(BoundSocketState<I, D, S>),
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for SocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Unbound(unbound) => unbound.as_ref(),
            Self::Bound(bound) => bound.as_ref(),
        }
    }
}

/// State associated with a Bound Socket.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) struct BoundSocketState<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    // The type of bound socket (e.g. Listener vs. Connected), and any
    // type-specific state.
    pub(crate) socket_type: BoundSocketStateType<I, D, S>,
    // The original bound address of the socket, as requested by the caller.
    // `None` if:
    //   * the socket was connected from unbound, or
    //   * listen was called without providing a local port.
    pub(crate) original_bound_addr: Option<S::ListenerIpAddr<I>>,
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for BoundSocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        let BoundSocketState { socket_type, original_bound_addr: _ } = self;
        match socket_type {
            BoundSocketStateType::Listener { state, sharing: _ } => state.as_ref(),
            BoundSocketStateType::Connected { state, sharing: _ } => state.as_ref(),
        }
    }
}

// State for the sub-types of bound socket (e.g. Listener or Connected).
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum BoundSocketStateType<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    Listener { state: ListenerState<I, D, S>, sharing: S::SharingState },
    Connected { state: S::ConnState<I, D>, sharing: S::SharingState },
}

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for BoundSocketStateType<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Listener { state, sharing: _ } => state.as_ref(),
            Self::Connected { state, sharing: _ } => state.as_ref(),
        }
    }
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: DatagramSocketMapSpec<I, D, A>>
    BoundSockets<I, D, A, S>
{
    pub(crate) fn iter_receivers(
        &self,
        (src_ip, src_port): (Option<SocketIpAddr<I::Addr>>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SocketIpAddr<I::Addr>, A::LocalIdentifier),
        device: D,
    ) -> Option<
        FoundSockets<
            AddrEntry<'_, I, D, A, S>,
            impl Iterator<Item = AddrEntry<'_, I, D, A, S>> + '_,
        >,
    > {
        self.lookup((src_ip, src_port), (dst_ip, dst_port), device)
    }
}

pub(crate) enum FoundSockets<A, It> {
    /// A single recipient was found for the address.
    Single(A),
    /// Indicates the looked-up address was multicast, and holds an iterator of
    /// the found receivers.
    Multicast(It),
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: DatagramSocketMapSpec<I, D, A>>
    BoundSocketMap<I, D, A, S>
{
    /// Finds the socket(s) that should receive an incoming packet.
    ///
    /// Uses the provided addresses and receiving device to look up sockets that
    /// should receive a matching incoming packet. Returns `None` if no sockets
    /// were found, or the results of the lookup.
    fn lookup(
        &self,
        (src_ip, src_port): (Option<SocketIpAddr<I::Addr>>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SocketIpAddr<I::Addr>, A::LocalIdentifier),
        device: D,
    ) -> Option<
        FoundSockets<
            AddrEntry<'_, I, D, A, S>,
            impl Iterator<Item = AddrEntry<'_, I, D, A, S>> + '_,
        >,
    > {
        let mut matching_entries = AddrVecIter::with_device(
            match (src_ip, src_port) {
                (Some(specified_src_ip), Some(src_port)) => {
                    ConnIpAddr { local: (dst_ip, dst_port), remote: (specified_src_ip, src_port) }
                        .into()
                }
                _ => ListenerIpAddr { addr: Some(dst_ip), identifier: dst_port }.into(),
            },
            device,
        )
        .filter_map(move |addr: AddrVec<I, D, A>| match addr {
            AddrVec::Listen(l) => {
                self.listeners().get_by_addr(&l).map(|state| AddrEntry::Listen(state, l))
            }
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).map(|state| AddrEntry::Conn(state, c)),
        });

        if dst_ip.addr().is_multicast() {
            Some(FoundSockets::Multicast(matching_entries))
        } else {
            let single_entry: Option<_> = matching_entries.next();
            single_entry.map(FoundSockets::Single)
        }
    }
}

pub(crate) enum AddrEntry<'a, I: Ip, D, A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    Listen(&'a S::ListenerAddrState, ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>),
    Conn(
        &'a S::ConnAddrState,
        ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>,
    ),
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"), Default(bound = ""))]
pub(crate) struct UnboundSocketState<I: IpExt, D, S: DatagramSocketSpec> {
    device: Option<D>,
    sharing: S::SharingState,
    ip_options: IpOptions<I, D, S>,
}

impl<I: IpExt, D, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>> for UnboundSocketState<I, D, S> {
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct ListenerState<I: IpExt, D: Hash + Eq + Debug, S: DatagramSocketSpec + ?Sized> {
    pub(crate) ip_options: IpOptions<I, D, S>,
    pub(crate) addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
}

impl<I: IpExt, D: Debug + Hash + Eq, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for ListenerState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) struct ConnState<
    WireI: IpExt,
    SocketI: IpExt,
    D: Eq + Hash,
    S: DatagramSocketSpec + ?Sized,
> {
    pub(crate) socket: IpSock<WireI, D, SocketHopLimits<WireI>>,
    pub(crate) ip_options: IpOptions<SocketI, D, S>,
    pub(crate) shutdown: Shutdown,
    pub(crate) addr: ConnAddr<
        ConnIpAddr<
            WireI::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
        D,
    >,
    /// Determines whether a call to disconnect this socket should also clear
    /// the device on the socket address.
    ///
    /// This will only be `true` if
    ///   1) the corresponding address has a bound device
    ///   2) the local address does not require a zone
    ///   3) the remote address does require a zone
    ///   4) the device was not set via [`set_unbound_device`]
    ///
    /// In that case, when the socket is disconnected, the device should be
    /// cleared since it was set as part of a `connect` call, not explicitly.
    ///
    /// TODO(http://fxbug.dev/110370): Implement this by changing socket
    /// addresses.
    pub(crate) clear_device_on_disconnect: bool,

    /// The extra state for the connection.
    ///
    /// For UDP it should be [`()`], for ICMP it should be [`NonZeroU16`] to
    /// remember the remote ID set by connect.
    pub(crate) extra: S::ConnStateExtra,
}

impl<WireI: IpExt, SocketI: IpExt, D: Hash + Eq, S: DatagramSocketSpec>
    AsRef<IpOptions<SocketI, D, S>> for ConnState<WireI, SocketI, D, S>
{
    fn as_ref(&self) -> &IpOptions<SocketI, D, S> {
        &self.ip_options
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsRef<Shutdown>
    for ConnState<WireI, SocketI, D, S>
{
    fn as_ref(&self) -> &Shutdown {
        &self.shutdown
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: Hash + Eq, S: DatagramSocketSpec>
    AsMut<IpOptions<SocketI, D, S>> for ConnState<WireI, SocketI, D, S>
{
    fn as_mut(&mut self) -> &mut IpOptions<SocketI, D, S> {
        &mut self.ip_options
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsMut<Shutdown>
    for ConnState<WireI, SocketI, D, S>
{
    fn as_mut(&mut self) -> &mut Shutdown {
        &mut self.shutdown
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: Eq + Hash, S: DatagramSocketSpec>
    ConnState<WireI, SocketI, D, S>
{
    pub(crate) fn should_receive(&self) -> bool {
        let Self {
            shutdown,
            socket: _,
            ip_options: _,
            clear_device_on_disconnect: _,
            addr: _,
            extra: _,
        } = self;
        let Shutdown { receive, send: _ } = shutdown;
        !*receive
    }
}

/// Connection state belong to either this-stack or the other-stack.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum DualStackConnState<
    I: IpExt + DualStackIpExt,
    D: Eq + Hash,
    S: DatagramSocketSpec + ?Sized,
> {
    /// The [`ConnState`] for a socked connected with [`I::Version`].
    ThisStack(ConnState<I, I, D, S>),
    /// The [`ConnState`] for a socked connected with [`I::OtherVersion`].
    OtherStack(ConnState<I::OtherVersion, I, D, S>),
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_ref(),
            DualStackConnState::OtherStack(state) => state.as_ref(),
        }
    }
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsMut<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_mut(&mut self) -> &mut IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_mut(),
            DualStackConnState::OtherStack(state) => state.as_mut(),
        }
    }
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsRef<Shutdown>
    for DualStackConnState<I, D, S>
{
    fn as_ref(&self) -> &Shutdown {
        match self {
            DualStackConnState::ThisStack(state) => state.as_ref(),
            DualStackConnState::OtherStack(state) => state.as_ref(),
        }
    }
}

impl<I: IpExt, D: Hash + Eq, S: DatagramSocketSpec> AsMut<Shutdown>
    for DualStackConnState<I, D, S>
{
    fn as_mut(&mut self) -> &mut Shutdown {
        match self {
            DualStackConnState::ThisStack(state) => state.as_mut(),
            DualStackConnState::OtherStack(state) => state.as_mut(),
        }
    }
}

#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Clone(bound = "D: Clone"), Debug(bound = "D: Debug"), Default(bound = ""))]
pub(crate) struct IpOptions<I: IpExt, D, S: DatagramSocketSpec + ?Sized> {
    multicast_memberships: MulticastMemberships<I::Addr, D>,
    hop_limits: SocketHopLimits<I>,
    other_stack: S::OtherStackIpOptions<I>,
    transparent: bool,
}

impl<I: IpExt, D, S: DatagramSocketSpec> AsRef<Self> for IpOptions<I, D, S> {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<I: IpExt, D, S: DatagramSocketSpec> IpOptions<I, D, S> {
    pub(crate) fn other_stack(&self) -> &S::OtherStackIpOptions<I> {
        &self.other_stack
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SocketHopLimits<I: Ip> {
    pub(crate) unicast: Option<NonZeroU8>,
    // TODO(https://fxbug.dev/108323): Make this an Option<u8> to allow sending
    // multicast packets destined only for the local machine.
    pub(crate) multicast: Option<NonZeroU8>,
    // An unused marker type signifying the IP version for which these hop
    // limits are valid. Including this helps prevent using the wrong hop limits
    // when operating on dualstack sockets.
    pub(crate) version: IpVersionMarker<I>,
}

impl<I: Ip> SocketHopLimits<I> {
    pub(crate) fn set_unicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.unicast = value
    }

    pub(crate) fn set_multicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.multicast = value
    }

    fn get_limits_with_defaults(&self, defaults: &HopLimits) -> HopLimits {
        let Self { unicast, multicast, version: _ } = self;
        HopLimits {
            unicast: unicast.unwrap_or(defaults.unicast),
            multicast: multicast.unwrap_or(defaults.multicast),
        }
    }
}

impl<I: Ip> SendOptions<I> for SocketHopLimits<I> {
    fn hop_limit(&self, destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8> {
        let SocketHopLimits { unicast, multicast, version: _ } = self;
        if destination.is_multicast() {
            *multicast
        } else {
            *unicast
        }
    }
}

#[derive(Clone, Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct MulticastMemberships<A, D>(HashSet<(MulticastAddr<A>, D)>);

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) enum MulticastMembershipChange {
    Join,
    Leave,
}

impl<A: Eq + Hash, D: Eq + Hash + Clone> MulticastMemberships<A, D> {
    pub(crate) fn apply_membership_change(
        &mut self,
        address: MulticastAddr<A>,
        device: &D,
        want_membership: bool,
    ) -> Option<MulticastMembershipChange> {
        let device = device.clone();

        let Self(map) = self;
        if want_membership {
            map.insert((address, device)).then(|| MulticastMembershipChange::Join)
        } else {
            map.remove(&(address, device)).then(|| MulticastMembershipChange::Leave)
        }
    }
}

impl<A: Eq + Hash, D: Eq + Hash> IntoIterator for MulticastMemberships<A, D> {
    type Item = (MulticastAddr<A>, D);
    type IntoIter = <HashSet<(MulticastAddr<A>, D)> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        let Self(memberships) = self;
        memberships.into_iter()
    }
}

impl<A: IpAddress, D: crate::device::Id, LI, RI: Copy> ConnAddr<ConnIpAddr<A, LI, RI>, D> {
    pub(crate) fn from_protocol_flow_and_local_port(
        id: &ProtocolFlowId<SocketIpAddr<A>, RI>,
        local_port: LI,
    ) -> Self {
        Self {
            ip: ConnIpAddr {
                local: (*id.local_addr(), local_port),
                remote: (*id.remote_addr(), *id.remote_port()),
            },
            device: None,
        }
    }
}

fn leave_all_joined_groups<A: IpAddress, BC, CC: MulticastMembershipHandler<A::Version, BC>>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    memberships: MulticastMemberships<A, CC::WeakDeviceId>,
) {
    for (addr, device) in memberships {
        let Some(device) = core_ctx.upgrade_weak_device_id(&device) else {
            continue;
        };
        core_ctx.leave_multicast_group(bindings_ctx, &device, addr)
    }
}

pub(crate) trait LocalIdentifierAllocator<
    I: Ip,
    D: Id,
    A: SocketMapAddrSpec,
    C,
    S: SocketMapStateSpec,
>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &BoundSocketMap<I, D, A, S>,
        ctx: &mut C,
        flow: DatagramFlowId<I::Addr, A::RemoteIdentifier>,
    ) -> Option<A::LocalIdentifier>;
}

#[derive(Hash)]
pub(crate) struct DatagramFlowId<A: IpAddress, RI> {
    pub(crate) local_ip: SocketIpAddr<A>,
    pub(crate) remote_ip: SocketIpAddr<A>,
    pub(crate) remote_id: RI,
}

pub(crate) trait DatagramStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The core context passed to the callback provided to methods.
    type SocketsStateCtx<'a>: DatagramBoundStateContext<I, BC, S>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_state<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_state_mut<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &mut SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with access to a [`DatagramBoundStateContext`].
    fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait DatagramBoundStateContext<I: IpExt + DualStackIpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The core context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, BC>
        + MulticastMembershipHandler<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    /// Context for dual-stack socket state access.
    ///
    /// This type type provides access, via an implementation of the
    /// [`DualStackDatagramBoundStateContext`] trait, to state necessary for
    /// implementing dual-stack socket operations. While a type must always be
    /// provided, implementations of [`DatagramBoundStateContext`] for socket
    /// types that don't support dual-stack operation (like ICMP and raw IP
    /// sockets, and UDPv4) can use the [`UninstantiableDualStackContext`] type,
    /// which is uninstantiable.
    type DualStackContext: DualStackDatagramBoundStateContext<
        I,
        BC,
        S,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Context for single-stack socket access.
    ///
    /// This type provides access, via an implementation of the
    /// [`NonDualStackDatagramBoundStateContext`] trait, to functionality
    /// necessary to implement sockets that do not support dual-stack operation.
    type NonDualStackContext: NonDualStackDatagramBoundStateContext<
        I,
        BC,
        S,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// The additional allocator passed to the callback provided to
    /// `with_sockets_mut`.
    type LocalIdAllocator: LocalIdentifierAllocator<
        I,
        Self::WeakDeviceId,
        S::AddrSpec,
        BC,
        S::SocketMapSpec<I, Self::WeakDeviceId>,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_bound_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Provides access to either the dual-stack or non-dual-stack context.
    ///
    /// For socket types that don't support dual-stack operation (like ICMP,
    /// raw IP sockets, and UDPv4), this method should always return a reference
    /// to the non-dual-stack context to allow the caller to access
    /// non-dual-stack state. Otherwise it should provide an instance of the
    /// `DualStackContext`, which can be used by the caller to access dual-stack
    /// state.
    fn dual_stack_context(
        &mut self,
    ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;

    /// Calls the function with only the inner context.
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

// Implement `GenericOverIp` for a `MaybeDualStack` whose `DS` and `NDS` also
// implement `GenericOverIp`.
impl<I: DualStackIpExt, DS: GenericOverIp<I>, NDS: GenericOverIp<I>> GenericOverIp<I>
    for MaybeDualStack<DS, NDS>
{
    type Type = MaybeDualStack<<DS as GenericOverIp<I>>::Type, <NDS as GenericOverIp<I>>::Type>;
}

impl<'a, DS, NDS> MaybeDualStack<&'a mut DS, &'a mut NDS> {
    fn to_converter<I: IpExt, BC, S: DatagramSocketSpec>(
        self,
    ) -> MaybeDualStack<DS::Converter, NDS::Converter>
    where
        DS: DualStackDatagramBoundStateContext<I, BC, S>,
        NDS: NonDualStackDatagramBoundStateContext<I, BC, S>,
    {
        match self {
            MaybeDualStack::DualStack(ds) => MaybeDualStack::DualStack(ds.converter()),
            MaybeDualStack::NotDualStack(nds) => MaybeDualStack::NotDualStack(nds.converter()),
        }
    }
}

/// Provides access to dual-stack socket state.
pub(crate) trait DualStackDatagramBoundStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The core context passed to the callbacks to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        // Allow creating IP sockets for the other IP version.
        + TransportIpContext<I::OtherVersion, BC>;

    /// Returns if the socket state indicates dual-stack operation is enabled.
    fn dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) -> bool;

    /// Returns the [`SendOptions`] to use for packets in the other stack.
    fn to_other_send_options<'a>(
        &self,
        state: &'a IpOptions<I, Self::WeakDeviceId, S>,
    ) -> &'a SocketHopLimits<I::OtherVersion>;

    /// Asserts that the socket state indicates dual-stack operation is enabled.
    ///
    /// Provided trait function.
    fn assert_dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) {
        debug_assert!(self.dual_stack_enabled(state), "socket must be dual-stack enabled")
    }

    /// A type for converting between address types.
    ///
    /// This allows converting between the possibly-dual-stack
    /// `S::ListenerIpAddr<I>` and the concrete dual-stack
    /// [`DualStackListenerIpAddr`].
    type Converter: OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            DualStackListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            DualStackConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnState<I, Self::WeakDeviceId>,
            DualStackConnState<I, Self::WeakDeviceId, S>,
        >;

    /// Returns an instance of a type that implements [`BidirectionalConverter`]
    /// for addresses.
    ///
    /// The returned object can be used to convert between the
    /// `S::ListenerIpAddr<I>` and the appropriate [`DualStackListenerIpAddr`].
    fn converter(&self) -> Self::Converter;

    /// Converts a socket ID to a bound socket ID.
    ///
    /// Converts a socket ID for IP version `I` into a bound socket ID that can
    /// be inserted into the demultiplexing map for IP version `I::OtherVersion`.
    fn to_other_bound_socket_id(
        &self,
        id: S::SocketId<I>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId;

    /// Converts an other-IP-version address to an address for IP version `I`.
    fn from_other_ip_addr(&self, addr: <I::OtherVersion as Ip>::Addr) -> I::Addr;

    /// The local id allocator for sockets in the `I` stack.
    type LocalIdAllocator: LocalIdentifierAllocator<
        I,
        Self::WeakDeviceId,
        S::AddrSpec,
        BC,
        S::SocketMapSpec<I, Self::WeakDeviceId>,
    >;

    /// The local id allocator for sockets in the `I::OtherVersion` stack
    type OtherLocalIdAllocator: LocalIdentifierAllocator<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
        BC,
        S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
    >;

    /// Calls the provided callback with mutable access to both the
    /// demultiplexing maps.
    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I, Self::WeakDeviceId>,
            >,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
            &mut Self::LocalIdAllocator,
            &mut Self::OtherLocalIdAllocator,
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
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the provided callback with access to the `IpSocketsCtx`.
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provides access to socket state for a single IP version.
pub(crate) trait NonDualStackDatagramBoundStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// A type for converting between address types.
    ///
    /// This allows converting between the possibly-dual-stack
    /// `S::ListenerIpAddr<I>` and the concrete not-dual-stack
    /// [``ListenerIpAddr`].
    type Converter: OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        > + OwnedOrRefsBidirectionalConverter<
            S::ConnState<I, Self::WeakDeviceId>,
            ConnState<I, I, Self::WeakDeviceId, S>,
        >;

    /// Returns an instance of a type that implements [`BidirectionalConverter`]
    /// for addresses.
    ///
    /// The returned object can be used to convert between the
    /// `S::ListenerIpAddr<I>` and the appropriate `ListenerIpAddr`.
    fn converter(&self) -> Self::Converter;
}

pub(crate) trait DatagramStateBindingsContext<I: Ip, S>: RngContext {}
impl<BC: RngContext, I: Ip, S> DatagramStateBindingsContext<I, S> for BC {}

/// Types and behavior for datagram socket demultiplexing map.
///
/// `I: Ip` describes the type of packets that can be received by sockets in
/// the map.
pub(crate) trait DatagramSocketMapSpec<I: Ip, D: Id, A: SocketMapAddrSpec>:
    SocketMapStateSpec<ListenerId = Self::BoundSocketId, ConnId = Self::BoundSocketId>
    + SocketMapConflictPolicy<
        ListenerAddr<ListenerIpAddr<I::Addr, A::LocalIdentifier>, D>,
        <Self as SocketMapStateSpec>::ListenerSharingState,
        I,
        D,
        A,
    > + SocketMapConflictPolicy<
        ConnAddr<ConnIpAddr<I::Addr, A::LocalIdentifier, A::RemoteIdentifier>, D>,
        <Self as SocketMapStateSpec>::ConnSharingState,
        I,
        D,
        A,
    >
{
    /// The type of IDs stored in a [`BoundSocketMap`] for which this is the
    /// specification.
    ///
    /// This can be the same as [`DatagramSocketSpec::SocketId`] but doesn't
    /// have to be. In the case of
    /// dual-stack sockets, for example, an IPv4 socket will have type
    /// `DatagramSocketSpec::SocketId<Ipv4>` but the IPv4 demultiplexing map
    /// might have `BoundSocketId=Either<DatagramSocketSpec::SocketId<Ipv4>,
    /// DatagramSocketSpec::SocketId<Ipv6>>` to allow looking up IPv6 sockets
    /// when receiving IPv4 packets.
    type BoundSocketId: Clone + Debug;
}

/// Common features of dual-stack sockets that vary by IP version.
///
/// This trait exists to provide per-IP-version associated types that are
/// useful for implementing dual-stack sockets. The types are intentionally
/// asymmetric - `DualStackIpExt::Xxx` has a different shape for the [`Ipv4`]
/// and [`Ipv6`] impls.
pub(crate) trait DualStackIpExt: super::DualStackIpExt {
    /// The type of socket that can receive an IP packet.
    ///
    /// For `Ipv4`, this is [`EitherIpSocket<S>`], and for `Ipv6` it is just
    /// `S::SocketId<Ipv6>`.
    ///
    /// [`EitherIpSocket<S>]`: [EitherIpSocket]
    type DualStackBoundSocketId<S: DatagramSocketSpec>: Clone + Debug + Eq;

    /// The IP options type for the other stack that will be held for a socket.
    ///
    /// For [`Ipv4`], this is `()`, and for [`Ipv6`] it is `State`. For a
    /// protocol like UDP or TCP where the IPv6 socket is dual-stack capable,
    /// the generic state struct can have a field with type
    /// `I::OtherStackIpOptions<Ipv4InIpv6Options>`.
    type OtherStackIpOptions<State: Clone + Debug + Default>: Clone + Debug + Default;

    /// A listener address for dual-stack operation.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug>: Clone + Debug;

    /// A connected address for dual-stack operation.
    type DualStackConnIpAddr<S: DatagramSocketSpec>: Clone + Debug;

    /// Connection state for a dual-stack socket.
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec>: Debug
        + AsRef<IpOptions<Self, D, S>>
        + AsMut<IpOptions<Self, D, S>>;

    /// Convert a socket ID into a `Self::DualStackBoundSocketId`.
    ///
    /// For coherency reasons this can't be a `From` bound on
    /// `DualStackBoundSocketId`. If more methods are added, consider moving
    /// this to its own dedicated trait that bounds `DualStackBoundSocketId`.
    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S>
    where
        Self: IpExt;

    /// Retrieves the associated connection address from the connection state.
    fn conn_addr_from_state<D: Eq + Hash + Debug + Clone, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D>;
}

/// An IP Socket ID that is either `Ipv4` or `Ipv6`.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Debug(bound = ""),
    Eq(bound = "S::SocketId<Ipv4>: Eq, S::SocketId<Ipv6>: Eq"),
    PartialEq(bound = "S::SocketId<Ipv4>: PartialEq, S::SocketId<Ipv6>: PartialEq")
)]
pub(crate) enum EitherIpSocket<S: DatagramSocketSpec> {
    V4(S::SocketId<Ipv4>),
    V6(S::SocketId<Ipv6>),
}

impl DualStackIpExt for Ipv4 {
    /// Incoming IPv4 packets may be received by either IPv4 or IPv6 sockets.
    type DualStackBoundSocketId<S: DatagramSocketSpec> = EitherIpSocket<S>;
    type OtherStackIpOptions<State: Clone + Debug + Default> = ();
    /// IPv4 sockets can't listen on dual-stack addresses.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug> =
        ListenerIpAddr<Self::Addr, LocalIdentifier>;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnIpAddr<S: DatagramSocketSpec> = ConnIpAddr<
        Self::Addr,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec> =
        ConnState<Self, Self, D, S>;

    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S> {
        EitherIpSocket::V4(id)
    }

    fn conn_addr_from_state<D: Clone + Debug + Eq + Hash, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D> {
        let ConnState {
            socket: _,
            ip_options: _,
            shutdown: _,
            addr,
            clear_device_on_disconnect: _,
            extra: _,
        } = state;
        addr.clone()
    }
}

impl DualStackIpExt for Ipv6 {
    /// Incoming IPv6 packets may only be received by IPv6 sockets.
    type DualStackBoundSocketId<S: DatagramSocketSpec> = S::SocketId<Self>;
    type OtherStackIpOptions<State: Clone + Debug + Default> = State;
    /// IPv6 listeners can listen on dual-stack addresses (if the protocol
    /// and socket are dual-stack-enabled).
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug> =
        DualStackListenerIpAddr<Self::Addr, LocalIdentifier>;
    /// IPv6 sockets can connect on dual-stack addresses (if the protocol and
    /// socket are dual-stack-enabled).
    type DualStackConnIpAddr<S: DatagramSocketSpec> = DualStackConnIpAddr<
        Self::Addr,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;
    /// IPv6 sockets can connect on dual-stack addresses (if the protocol and
    /// socket are dual-stack-enabled).
    type DualStackConnState<D: Eq + Hash + Debug, S: DatagramSocketSpec> =
        DualStackConnState<Self, D, S>;

    fn into_dual_stack_bound_socket_id<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackBoundSocketId<S> {
        id
    }

    fn conn_addr_from_state<D: Clone + Debug + Eq + Hash, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D> {
        match state {
            DualStackConnState::ThisStack(state) => {
                let ConnState { addr, .. } = state;
                let ConnAddr { ip, device } = addr.clone();
                ConnAddr { ip: DualStackConnIpAddr::ThisStack(ip), device }
            }
            DualStackConnState::OtherStack(state) => {
                let ConnState {
                    socket: _,
                    ip_options: _,
                    shutdown: _,
                    addr,
                    clear_device_on_disconnect: _,
                    extra: _,
                } = state;
                let ConnAddr { ip, device } = addr.clone();
                ConnAddr { ip: DualStackConnIpAddr::OtherStack(ip), device }
            }
        }
    }
}

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
/// A wrapper to make [`DualStackIpExt::OtherStackIpOptions`] [`GenericOverIp`].
pub(crate) struct WrapOtherStackIpOptions<'a, I: DualStackIpExt, S: 'a + Clone + Debug + Default>(
    pub(crate) &'a I::OtherStackIpOptions<S>,
);

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
/// A wrapper to make [`DualStackIpExt::OtherStackIpOptions`] [`GenericOverIp`].
pub(crate) struct WrapOtherStackIpOptionsMut<'a, I: DualStackIpExt, S: 'a + Clone + Debug + Default>(
    pub(crate) &'a mut I::OtherStackIpOptions<S>,
);

/// Types and behavior for datagram sockets.
///
/// These sockets may or may not support dual-stack operation.
pub(crate) trait DatagramSocketSpec {
    /// The socket address spec for the datagram socket type.
    ///
    /// This describes the types of identifiers the socket uses, e.g.
    /// local/remote port for UDP.
    type AddrSpec: SocketMapAddrSpec;

    /// Identifier for an individual socket for a given IP version.
    ///
    /// Corresponds uniquely to a socket resource. This is the type that will
    /// be returned by [`create`] and used to identify which socket is being
    /// acted on by calls like [`listen`], [`connect`], [`remove`], etc.
    type SocketId<I: IpExt>: Clone + Debug + EntryKey + From<usize> + Eq;

    /// IP-level options for sending `I::OtherVersion` IP packets.
    type OtherStackIpOptions<I: IpExt>: Clone + Debug + Default;

    /// The type of a listener IP address.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ListenerIpAddr`], which will be one of
    /// [`ListenerIpAddr`] or [`DualStackListenerIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ListenerIpAddr`].
    type ListenerIpAddr<I: IpExt>: Clone + Debug;

    /// The sharing state for a socket.
    ///
    /// NB: The underlying [`BoundSocketMap`]` uses separate types for the
    /// sharing state of connected vs listening sockets. At the moment, datagram
    /// sockets have no need for differentiated sharing states, so consolidate
    /// them under one type.
    type SharingState: Clone + Debug + Default;

    /// The type of an IP address for a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnIpAddr`], which will be one of
    /// [`ConnIpAddr`] or [`DualStackConnIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ConnIpAddr`].
    type ConnIpAddr<I: IpExt>: Clone + Debug;

    /// The type of a state held by a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnState`], which will be one of [`ConnState`] or
    /// [`DualStackConnState`]. Non-dual-stack-capable protocols (like ICMP and
    /// raw IP sockets) should just use [`ConnState`].
    type ConnState<I: IpExt, D: Debug + Eq + Hash>: Debug
        + AsRef<IpOptions<I, D, Self>>
        + AsMut<IpOptions<I, D, Self>>;

    /// The extra state that a connection state want to remember.
    ///
    /// For example: UDP sockets does not have any extra state to remember, so
    /// it should just be `()`; ICMP sockets need to remember the remote ID the
    /// socket is 'connected' to, the remote ID is not used when sending nor
    /// participating in the demuxing decisions. So it will be stored in the
    /// extra state so that it can be retrieved later, i.e, it should be
    /// `NonZeroU16` for ICMP sockets.
    type ConnStateExtra: Debug;

    /// The specification for the [`BoundSocketMap`] for a given IP version.
    ///
    /// Describes the per-address and per-socket values held in the
    /// demultiplexing map for a given IP version.
    type SocketMapSpec<I: IpExt + DualStackIpExt, D: device::WeakId>: DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
        ListenerSharingState = Self::SharingState,
        ConnSharingState = Self::SharingState,
    >;

    /// Returns the IP protocol of this datagram specification.
    fn ip_proto<I: IpProtoExt>() -> I::Proto;

    /// Converts [`Self::SocketId`] to [`DatagramSocketMapSpec::BoundSocketId`].
    ///
    /// Constructs a socket identifier to its in-demultiplexing map form. For
    /// protocols with dual-stack sockets, like UDP, implementations should
    /// perform a transformation. Otherwise it should be the identity function.
    fn make_bound_socket_map_id<I: IpExt, D: device::WeakId>(
        s: Self::SocketId<I>,
    ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId;

    /// The type of serializer returned by [`DatagramSocketSpec::make_packet`]
    /// for a given IP version and buffer type.
    type Serializer<I: IpExt, B: BufferMut>: Serializer<Buffer = B>;
    /// The potential error for serializing a packet. For example, in UDP, this
    /// should be infallible but for ICMP, there will be an error if the input
    /// is not an echo request.
    type SerializeError;
    fn make_packet<I: IpExt, B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<
            I::Addr,
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    ) -> Result<Self::Serializer<I, B>, Self::SerializeError>;

    /// Attempts to allocate a local identifier for a listening socket.
    ///
    /// Returns the identifier on success, or `None` on failure.
    fn try_alloc_listen_identifier<I: IpExt, D: device::WeakId>(
        rng: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>;

    /// Retrieves the associated connection ip addr from the connection state.
    fn conn_addr_from_state<I: IpExt, D: Clone + Debug + Eq + Hash>(
        state: &Self::ConnState<I, D>,
    ) -> ConnAddr<Self::ConnIpAddr<I>, D>;
}

pub(crate) struct InUseError;

pub(crate) fn create<I: IpExt, S: DatagramSocketSpec, BC, CC: DatagramStateContext<I, BC, S>>(
    core_ctx: &mut CC,
) -> S::SocketId<I> {
    core_ctx.with_sockets_state_mut(|_core_ctx, state| {
        state.push(SocketState::Unbound(UnboundSocketState::default())).into()
    })
}

#[derive(Debug)]
pub(crate) enum SocketInfo<I: Ip + DualStackIpExt, D, S: DatagramSocketSpec> {
    Unbound,
    Listener(ListenerAddr<S::ListenerIpAddr<I>, D>),
    Connected(ConnAddr<S::ConnIpAddr<I>, D>),
}

pub(crate) fn close<
    I: IpExt,
    S: DatagramSocketSpec,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
) -> SocketInfo<I, CC::WeakDeviceId, S> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let (ip_options, info) = match state.remove(id.get_key_index()).expect("invalid socket ID")
        {
            SocketState::Unbound(UnboundSocketState { device: _, sharing: _, ip_options }) => {
                (ip_options, SocketInfo::Unbound)
            }
            SocketState::Bound(state) => match core_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    let op = DualStackRemoveOperation::new_from_state(dual_stack, id, &state);
                    dual_stack
                        .with_both_bound_sockets_mut(
                            |_core_ctx, sockets, other_sockets, _alloc, _other_alloc| {
                                op.apply(sockets, other_sockets)
                            },
                        )
                        .into_options_and_info()
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    let op = SingleStackRemoveOperation::new_from_state(not_dual_stack, id, &state);
                    core_ctx
                        .with_bound_sockets_mut(|_core_ctx, sockets, _allocator| op.apply(sockets))
                        .into_options_and_info()
                }
            },
        };
        DatagramBoundStateContext::<I, _, _>::with_transport_context(core_ctx, |core_ctx| {
            leave_all_joined_groups(core_ctx, bindings_ctx, ip_options.multicast_memberships)
        });
        info
    })
}

/// State associated with removing a socket.
///
/// Note that this type is generic over two `IpExt` parameters: `WireI` and
/// `SocketI`. This allows it to be used for both single-stack remove operations
/// (where `WireI` and `SocketI` are the same), as well as dual-stack remove
/// operations (where `WireI`, and `SocketI` may be different).
enum Remove<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec> {
    Listener {
        // The socket's address, stored as a concrete `ListenerIpAddr`.
        concrete_addr: ListenerAddr<
            ListenerIpAddr<WireI::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
            D,
        >,
        // The socket's address, stored as an associated `S::ListenerIpAddr`.
        associated_addr: ListenerAddr<S::ListenerIpAddr<SocketI>, D>,
        ip_options: IpOptions<SocketI, D, S>,
        sharing: S::SharingState,
        socket_id:
            <S::SocketMapSpec<WireI, D> as DatagramSocketMapSpec<WireI, D, S::AddrSpec>>::BoundSocketId,
    },
    Connected {
        // The socket's address, stored as a concrete `ConnIpAddr`.
        concrete_addr: ConnAddr<
            ConnIpAddr<
                WireI::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
            D,
        >,
        // The socket's address, stored as an associated `S:ConnIpAddr`.
        associated_addr: ConnAddr<S::ConnIpAddr<SocketI>, D>,
        ip_options: IpOptions<SocketI, D, S>,
        sharing: S::SharingState,
        socket_id:
            <S::SocketMapSpec<WireI, D> as DatagramSocketMapSpec<WireI, D, S::AddrSpec>>::BoundSocketId,
    },
}

/// The yet-to-be-performed removal of a socket.
///
/// Like [`Remove`], this type takes two generic `IpExt` parameters so that
/// it can be used from single-stack and dual-stack remove operations.
struct RemoveOperation<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec>(
    Remove<WireI, SocketI, D, S>,
);

impl<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec>
    RemoveOperation<WireI, SocketI, D, S>
{
    /// Apply this remove operation to the given `BoundSocketMap`.
    ///
    /// # Panics
    ///
    /// Panics if the given socket map does not contain the socket specified by
    /// this removal operation.
    fn apply(
        self,
        sockets: &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
    ) -> RemoveInfo<WireI, SocketI, D, S> {
        let RemoveOperation(remove) = self;
        match &remove {
            Remove::Listener {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing: _,
                socket_id,
            } => {
                let ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device } =
                    concrete_addr;
                BoundStateHandler::<_, S, _>::remove_listener(
                    sockets,
                    addr,
                    *identifier,
                    device,
                    socket_id,
                );
            }
            Remove::Connected {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing: _,
                socket_id,
            } => {
                sockets
                    .conns_mut()
                    .remove(socket_id, concrete_addr)
                    .expect("UDP connection not found");
            }
        }
        RemoveInfo(remove)
    }
}

// A single stack implementation of `RemoveOperation` (e.g. where `WireI` and
// `SocketI` are the same type).
impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> RemoveOperation<I, I, D, S> {
    /// Constructs the remove operation from existing socket state.
    fn new_from_state<BC, CC: NonDualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D>>(
        core_ctx: &mut CC,
        socket_id: S::SocketId<I>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        let BoundSocketState { socket_type: state, original_bound_addr: _ } = state;
        RemoveOperation(match state {
            BoundSocketStateType::Listener { state, sharing } => {
                let ListenerState { addr: associated_addr, ip_options } = state;
                let ListenerAddr { ip, device } = associated_addr.clone();
                let concrete_addr = ListenerAddr { ip: core_ctx.converter().convert(ip), device };
                Remove::Listener {
                    concrete_addr,
                    associated_addr: associated_addr.clone(),
                    ip_options: ip_options.clone(),
                    sharing: sharing.clone(),
                    socket_id: S::make_bound_socket_map_id(socket_id),
                }
            }
            BoundSocketStateType::Connected { state, sharing } => {
                let ConnState {
                    addr,
                    socket: _,
                    ip_options,
                    clear_device_on_disconnect: _,
                    shutdown: _,
                    extra: _,
                } = core_ctx.converter().convert(state);
                let ConnAddr { ip, device } = addr.clone();
                let associated_addr =
                    ConnAddr { ip: core_ctx.converter().convert_back(ip), device };
                Remove::Connected {
                    concrete_addr: addr.clone(),
                    associated_addr,
                    ip_options: ip_options.clone(),
                    sharing: sharing.clone(),
                    socket_id: S::make_bound_socket_map_id(socket_id),
                }
            }
        })
    }
}

/// Information for a recently-removed single-stack socket.
///
/// Like [`Remove`], this type takes two generic `IpExt` parameters so that
/// it can be used from single-stack and dual-stack remove operations.
struct RemoveInfo<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec>(
    Remove<WireI, SocketI, D, S>,
);

impl<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec>
    RemoveInfo<WireI, SocketI, D, S>
{
    fn into_options_and_info(self) -> (IpOptions<SocketI, D, S>, SocketInfo<SocketI, D, S>) {
        let RemoveInfo(remove) = self;
        match remove {
            Remove::Listener {
                concrete_addr: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
            } => (ip_options, SocketInfo::Listener(associated_addr)),
            Remove::Connected {
                concrete_addr: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_id: _,
            } => (ip_options, SocketInfo::Connected(associated_addr)),
        }
    }

    fn into_options_sharing_and_device(
        self,
    ) -> (IpOptions<SocketI, D, S>, S::SharingState, Option<D>) {
        let RemoveInfo(remove) = self;
        match remove {
            Remove::Listener {
                concrete_addr: ListenerAddr { ip: _, device },
                associated_addr: _,
                ip_options,
                sharing,
                socket_id: _,
            } => (ip_options, sharing, device),
            Remove::Connected {
                concrete_addr: ConnAddr { ip: _, device },
                associated_addr: _,
                ip_options,
                sharing,
                socket_id: _,
            } => (ip_options, sharing, device),
        }
    }

    /// Undo this removal by reinserting the socket into the [`BoundSocketMap`].
    ///
    /// # Panics
    ///
    /// Panics if the socket can not be inserted into the given map (i.e. if it
    /// already exists). This is not expected to happen, provided the
    /// [`BoundSocketMap`] lock has been held across removal and reinsertion.
    fn reinsert(
        self,
        sockets: &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
    ) {
        let RemoveInfo(remove) = self;
        match remove {
            Remove::Listener {
                concrete_addr: ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device },
                associated_addr: _,
                ip_options: _,
                sharing,
                socket_id,
            } => {
                BoundStateHandler::<_, S, _>::try_insert_listener(
                    sockets, addr, identifier, device, sharing, socket_id,
                )
                .expect("reinserting just-removed listener failed");
            }
            Remove::Connected {
                concrete_addr,
                associated_addr: _,
                ip_options: _,
                sharing,
                socket_id,
            } => {
                let _entry = sockets
                    .conns_mut()
                    .try_insert(concrete_addr, sharing, socket_id)
                    .expect("reinserting just-removed connected failed");
            }
        }
    }
}

/// The yet-to-be-performed removal of a single-stack socket.
type SingleStackRemoveOperation<I, D, S> = RemoveOperation<I, I, D, S>;

/// Information for a recently-removed single-stack socket.
type SingleStackRemoveInfo<I, D, S> = RemoveInfo<I, I, D, S>;

/// State associated with removing a dual-stack socket.
enum DualStackRemove<I: IpExt, D: WeakId, S: DatagramSocketSpec> {
    CurrentStack(Remove<I, I, D, S>),
    OtherStack(Remove<I::OtherVersion, I, D, S>),
    ListenerBothStacks {
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        // The socket's address, stored as an associated `S::ListenerIpAddr`.
        // NB: The addr must be the dual-stack any address, hence there's no
        // need to store it's concrete form.
        associated_addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_ids: PairedBoundSocketIds<I, D, S>,
    },
}

/// The yet-to-be-performed removal of a single-stack socket.
struct DualStackRemoveOperation<I: IpExt, D: WeakId, S: DatagramSocketSpec>(
    DualStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> DualStackRemoveOperation<I, D, S> {
    /// Constructs the removal operation from existing socket state.
    fn new_from_state<BC, CC: DualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D>>(
        core_ctx: &mut CC,
        socket_id: S::SocketId<I>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        let BoundSocketState { socket_type: state, original_bound_addr: _ } = state;
        DualStackRemoveOperation(match state {
            BoundSocketStateType::Listener { state, sharing } => {
                let ListenerState { addr: associated_addr, ip_options } = state;
                let ListenerAddr { ip, device } = associated_addr.clone();
                match (core_ctx.converter().convert(ip), core_ctx.dual_stack_enabled(&ip_options)) {
                    // Dual-stack enabled, bound in both stacks.
                    (DualStackListenerIpAddr::BothStacks(identifier), true) => {
                        DualStackRemove::ListenerBothStacks {
                            identifier: identifier.clone(),
                            device: device,
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_ids: PairedBoundSocketIds {
                                this: S::make_bound_socket_map_id(socket_id.clone()),
                                other: core_ctx.to_other_bound_socket_id(socket_id),
                            },
                        }
                    }
                    // Bound in this stack, with/without dual-stack enabled.
                    (DualStackListenerIpAddr::ThisStack(addr), true | false) => {
                        DualStackRemove::CurrentStack(Remove::Listener {
                            concrete_addr: ListenerAddr { ip: addr, device },
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id),
                        })
                    }
                    // Dual-stack enabled, bound only in the other stack.
                    (DualStackListenerIpAddr::OtherStack(addr), true) => {
                        DualStackRemove::OtherStack(Remove::Listener {
                            concrete_addr: ListenerAddr { ip: addr, device },
                            associated_addr: associated_addr.clone(),
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                        })
                    }
                    (DualStackListenerIpAddr::OtherStack(_), false)
                    | (DualStackListenerIpAddr::BothStacks(_), false) => {
                        unreachable!("dual-stack disabled socket cannot use the other stack")
                    }
                }
            }
            BoundSocketStateType::Connected { state, sharing } => {
                match core_ctx.converter().convert(state) {
                    DualStackConnState::ThisStack(state) => {
                        let ConnState {
                            addr,
                            socket: _,
                            ip_options,
                            clear_device_on_disconnect: _,
                            shutdown: _,
                            extra: _,
                        } = state;
                        let ConnAddr { ip, device } = addr.clone();
                        let ip = DualStackConnIpAddr::ThisStack(ip);
                        let associated_addr =
                            ConnAddr { ip: core_ctx.converter().convert_back(ip), device };
                        DualStackRemove::CurrentStack(Remove::Connected {
                            concrete_addr: addr.clone(),
                            associated_addr,
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id),
                        })
                    }
                    DualStackConnState::OtherStack(state) => {
                        core_ctx.assert_dual_stack_enabled(&state);
                        let ConnState {
                            addr,
                            socket: _,
                            ip_options,
                            clear_device_on_disconnect: _,
                            shutdown: _,
                            extra: _,
                        } = state;
                        let ConnAddr { ip, device } = addr.clone();
                        let ip = DualStackConnIpAddr::OtherStack(ip);
                        let associated_addr =
                            ConnAddr { ip: core_ctx.converter().convert_back(ip), device };
                        DualStackRemove::OtherStack(Remove::Connected {
                            concrete_addr: addr.clone(),
                            associated_addr,
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                        })
                    }
                }
            }
        })
    }

    /// Apply this remove operation to the given `BoundSocketMap`s.
    ///
    /// # Panics
    ///
    /// Panics if the given socket maps does not contain the socket specified by
    /// this removal operation.
    fn apply(
        self,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        other_sockets: &mut BoundSocketMap<
            I::OtherVersion,
            D,
            S::AddrSpec,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
    ) -> DualStackRemoveInfo<I, D, S> {
        let DualStackRemoveOperation(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(remove) => {
                let RemoveInfo(remove) = RemoveOperation(remove).apply(sockets);
                DualStackRemoveInfo(DualStackRemove::CurrentStack(remove))
            }
            DualStackRemove::OtherStack(remove) => {
                let RemoveInfo(remove) = RemoveOperation(remove).apply(other_sockets);
                DualStackRemoveInfo(DualStackRemove::OtherStack(remove))
            }
            DualStackRemove::ListenerBothStacks {
                identifier,
                device,
                associated_addr,
                ip_options,
                sharing,
                socket_ids,
            } => {
                PairedSocketMapMut::<_, _, S> { bound: sockets, other_bound: other_sockets }
                    .remove_listener(&DualStackUnspecifiedAddr, identifier, &device, &socket_ids);
                DualStackRemoveInfo(DualStackRemove::ListenerBothStacks {
                    identifier,
                    device,
                    associated_addr,
                    ip_options,
                    sharing,
                    socket_ids,
                })
            }
        }
    }
}

/// Information for a recently-removed single-stack socket.
struct DualStackRemoveInfo<I: IpExt, D: WeakId, S: DatagramSocketSpec>(DualStackRemove<I, D, S>);

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> DualStackRemoveInfo<I, D, S> {
    fn into_options_and_info(self) -> (IpOptions<I, D, S>, SocketInfo<I, D, S>) {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(remove) => RemoveInfo(remove).into_options_and_info(),
            DualStackRemove::OtherStack(remove) => RemoveInfo(remove).into_options_and_info(),
            DualStackRemove::ListenerBothStacks {
                identifier: _,
                device: _,
                associated_addr,
                ip_options,
                sharing: _,
                socket_ids: _,
            } => (ip_options, SocketInfo::Listener(associated_addr)),
        }
    }

    fn into_options_sharing_and_device(self) -> (IpOptions<I, D, S>, S::SharingState, Option<D>) {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(remove) => {
                RemoveInfo(remove).into_options_sharing_and_device()
            }
            DualStackRemove::OtherStack(remove) => {
                RemoveInfo(remove).into_options_sharing_and_device()
            }
            DualStackRemove::ListenerBothStacks {
                identifier: _,
                device,
                associated_addr: _,
                ip_options,
                sharing,
                socket_ids: _,
            } => (ip_options, sharing, device),
        }
    }

    /// Undo this removal by reinserting the socket into the [`BoundSocketMap`].
    ///
    /// # Panics
    ///
    /// Panics if the socket can not be inserted into the given maps (i.e. if it
    /// already exists). This is not expected to happen, provided the
    /// [`BoundSocketMap`] lock has been held across removal and reinsertion.
    fn reinsert(
        self,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        other_sockets: &mut BoundSocketMap<
            I::OtherVersion,
            D,
            S::AddrSpec,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
    ) {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(remove) => {
                RemoveInfo(remove).reinsert(sockets);
            }
            DualStackRemove::OtherStack(remove) => {
                RemoveInfo(remove).reinsert(other_sockets);
            }
            DualStackRemove::ListenerBothStacks {
                identifier,
                device,
                associated_addr: _,
                ip_options: _,
                sharing,
                socket_ids,
            } => {
                let mut socket_maps_pair =
                    PairedSocketMapMut { bound: sockets, other_bound: other_sockets };
                BoundStateHandler::<_, S, _>::try_insert_listener(
                    &mut socket_maps_pair,
                    DualStackUnspecifiedAddr,
                    identifier,
                    device,
                    sharing,
                    socket_ids,
                )
                .expect("reinserting just-removed listener failed")
            }
        }
    }
}

pub(crate) fn get_info<
    I: IpExt,
    S: DatagramSocketSpec,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: S::SocketId<I>,
) -> SocketInfo<I, CC::WeakDeviceId, S> {
    core_ctx.with_sockets_state(|_core_ctx, state| {
        match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => SocketInfo::Unbound,
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state, sharing: _ } => {
                        let ListenerState { addr, ip_options: _ } = state;
                        SocketInfo::Listener(addr.clone())
                    }
                    BoundSocketStateType::Connected { state, sharing: _ } => {
                        SocketInfo::Connected(S::conn_addr_from_state(state))
                    }
                }
            }
        }
    })
}

pub(crate) fn listen<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    addr: Option<SocketZonedIpAddr<I::Addr, CC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        listen_inner::<_, BC, _, S>(core_ctx, bindings_ctx, state, id, addr, local_id)
    })
}

/// Abstraction for operations over one or two demultiplexing maps.
trait BoundStateHandler<I: IpExt, S: DatagramSocketSpec, D: device::WeakId> {
    /// The type of address that can be inserted or removed for listeners.
    type ListenerAddr: Clone;
    /// The type of ID that can be inserted or removed.
    type BoundSocketId;

    /// Checks whether an entry could be inserted for the specified address and
    /// identifier.
    ///
    /// Returns `true` if a value could be inserted at the specified address and
    /// local ID, with the provided sharing state; otherwise returns `false`.
    fn is_listener_entry_available(
        &self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing_state: &S::SharingState,
    ) -> bool;

    /// Inserts `id` at a listener address or returns an error.
    ///
    /// Inserts the identifier `id` at the listener address for `addr` and
    /// local `identifier` with device `device` and the given sharing state. If
    /// the insertion conflicts with an existing socket, a `LocalAddressError`
    /// is returned.
    fn try_insert_listener(
        &mut self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError>;

    /// Removes `id` at listener address, assuming it exists.
    ///
    /// Panics if `id` does not exit.
    fn remove_listener(
        &mut self,
        addr: &Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &Self::BoundSocketId,
    );
}

/// A sentinel type for the unspecified address in a dual-stack context.
///
/// This is kind of like [`Ipv6::UNSPECIFIED_ADDRESS`], but makes it clear that
/// the value is being used in a dual-stack context.
#[derive(Copy, Clone, Debug)]
struct DualStackUnspecifiedAddr;

/// Implementation of BoundStateHandler for a single demultiplexing map.
impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
    for BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>
{
    type ListenerAddr = Option<SocketIpAddr<I::Addr>>;
    type BoundSocketId =
        <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId;
    fn is_listener_entry_available(
        &self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &S::SharingState,
    ) -> bool {
        let check_addr = ListenerAddr { device: None, ip: ListenerIpAddr { identifier, addr } };
        match self.listeners().could_insert(&check_addr, sharing) {
            Ok(()) => true,
            Err(
                InsertError::Exists
                | InsertError::IndirectConflict
                | InsertError::ShadowAddrExists
                | InsertError::ShadowerExists,
            ) => false,
        }
    }

    fn try_insert_listener(
        &mut self,
        addr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError> {
        try_insert_single_listener(self, addr, identifier, device, sharing, id).map(|_entry| ())
    }

    fn remove_listener(
        &mut self,
        addr: &Self::ListenerAddr,
        identifier: <<S as DatagramSocketSpec>::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &Self::BoundSocketId,
    ) {
        remove_single_listener(self, addr, identifier, device, id)
    }
}

struct PairedSocketMapMut<'a, I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    bound: &'a mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
    other_bound: &'a mut BoundSocketMap<
        I::OtherVersion,
        D,
        S::AddrSpec,
        S::SocketMapSpec<I::OtherVersion, D>,
    >,
}

struct PairedBoundSocketIds<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    this: <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
    other: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
        I::OtherVersion,
        D,
        S::AddrSpec,
    >>::BoundSocketId,
}

/// Implementation for a pair of demultiplexing maps for different IP versions.
impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
    for PairedSocketMapMut<'_, I, D, S>
{
    type ListenerAddr = DualStackUnspecifiedAddr;
    type BoundSocketId = PairedBoundSocketIds<I, D, S>;

    fn is_listener_entry_available(
        &self,
        DualStackUnspecifiedAddr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &S::SharingState,
    ) -> bool {
        let PairedSocketMapMut { bound, other_bound } = self;
        BoundStateHandler::<I, S, D>::is_listener_entry_available(*bound, None, identifier, sharing)
            && BoundStateHandler::<I::OtherVersion, S, D>::is_listener_entry_available(
                *other_bound,
                None,
                identifier,
                sharing,
            )
    }

    fn try_insert_listener(
        &mut self,
        DualStackUnspecifiedAddr: Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: S::SharingState,
        id: Self::BoundSocketId,
    ) -> Result<(), LocalAddressError> {
        let PairedSocketMapMut { bound: this, other_bound: other } = self;
        let PairedBoundSocketIds { this: this_id, other: other_id } = id;
        try_insert_single_listener(this, None, identifier, device.clone(), sharing.clone(), this_id)
            .and_then(|first_entry| {
                match try_insert_single_listener(other, None, identifier, device, sharing, other_id)
                {
                    Ok(_second_entry) => Ok(()),
                    Err(e) => {
                        first_entry.remove();
                        Err(e)
                    }
                }
            })
    }

    fn remove_listener(
        &mut self,
        DualStackUnspecifiedAddr: &Self::ListenerAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: &Option<D>,
        id: &PairedBoundSocketIds<I, D, S>,
    ) {
        let PairedSocketMapMut { bound: this, other_bound: other } = self;
        let PairedBoundSocketIds { this: this_id, other: other_id } = id;
        remove_single_listener(this, &None, identifier, device, this_id);
        remove_single_listener(other, &None, identifier, device, other_id);
    }
}

fn try_insert_single_listener<
    I: IpExt,
    D: device::WeakId,
    A: SocketMapAddrSpec,
    S: DatagramSocketMapSpec<I, D, A>,
>(
    bound: &mut BoundSocketMap<I, D, A, S>,
    addr: Option<SocketIpAddr<I::Addr>>,
    identifier: A::LocalIdentifier,
    device: Option<D>,
    sharing: S::ListenerSharingState,
    id: S::ListenerId,
) -> Result<socket::SocketStateEntry<'_, I, D, A, S, socket::Listener>, LocalAddressError> {
    bound
        .listeners_mut()
        .try_insert(ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }, sharing, id)
        .map_err(|e| match e {
            (
                InsertError::ShadowAddrExists
                | InsertError::Exists
                | InsertError::IndirectConflict
                | InsertError::ShadowerExists,
                sharing,
            ) => {
                let _: S::ListenerSharingState = sharing;
                LocalAddressError::AddressInUse
            }
        })
}

fn remove_single_listener<
    I: IpExt,
    D: device::WeakId,
    A: SocketMapAddrSpec,
    S: DatagramSocketMapSpec<I, D, A>,
>(
    bound: &mut BoundSocketMap<I, D, A, S>,
    addr: &Option<SocketIpAddr<I::Addr>>,
    identifier: A::LocalIdentifier,
    device: &Option<D>,
    id: &S::ListenerId,
) {
    let addr =
        ListenerAddr { ip: ListenerIpAddr { addr: *addr, identifier }, device: device.clone() };
    bound
        .listeners_mut()
        .remove(id, &addr)
        .unwrap_or_else(|NotFoundError| panic!("socket ID {:?} not found for {:?}", id, addr))
}

fn try_pick_identifier<
    I: IpExt,
    S: DatagramSocketSpec,
    D: device::WeakId,
    BS: BoundStateHandler<I, S, D>,
    BC: RngContext,
>(
    addr: BS::ListenerAddr,
    bound: &BS,
    bindings_ctx: &mut BC,
    sharing: &S::SharingState,
) -> Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
    S::try_alloc_listen_identifier::<I, D>(bindings_ctx, move |identifier| {
        bound
            .is_listener_entry_available(addr.clone(), identifier, sharing)
            .then_some(())
            .ok_or(InUseError)
    })
}

fn try_pick_bound_address<I: IpExt, CC: TransportIpContext<I, BC>, BC, LI>(
    addr: Option<ZonedAddr<SocketIpAddr<I::Addr>, CC::DeviceId>>,
    device: &Option<CC::WeakDeviceId>,
    core_ctx: &mut CC,
    identifier: LI,
) -> Result<
    (Option<SocketIpAddr<I::Addr>>, Option<EitherDeviceId<CC::DeviceId, CC::WeakDeviceId>>, LI),
    LocalAddressError,
> {
    let (addr, device, identifier) = match addr {
        Some(addr) => {
            // Extract the specified address and the device. The device
            // is either the one from the address or the one to which
            // the socket was previously bound.
            let (addr, device) = crate::transport::resolve_addr_with_device(addr, device.clone())?;

            // Binding to multicast addresses is allowed regardless.
            // Other addresses can only be bound to if they are assigned
            // to the device.
            if !addr.addr().is_multicast() {
                let mut assigned_to = TransportIpContext::<I, _>::get_devices_with_assigned_addr(
                    core_ctx,
                    addr.into(),
                );
                if let Some(device) = &device {
                    if !assigned_to.any(|d| device == &EitherDeviceId::Strong(d)) {
                        return Err(LocalAddressError::AddressMismatch);
                    }
                } else {
                    if !assigned_to.any(|_: CC::DeviceId| true) {
                        return Err(LocalAddressError::CannotBindToAddress);
                    }
                }
            }
            (Some(addr), device, identifier)
        }
        None => (None, device.clone().map(EitherDeviceId::Weak), identifier),
    };
    Ok((addr, device, identifier))
}

fn listen_inner<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramBoundStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    state: &mut SocketsState<I, CC::WeakDeviceId, S>,
    id: S::SocketId<I>,
    addr: Option<SocketZonedIpAddr<I::Addr, CC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    /// Possible operations that might be performed, depending on whether the
    /// socket state spec supports dual-stack operation and what the address
    /// looks like.
    #[derive(Debug, GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    enum BoundOperation<'a, I: IpExt, DS: DeviceIdContext<AnyDevice>, NDS> {
        /// Bind to the "any" address on both stacks.
        DualStackAnyAddr(&'a mut DS),
        /// Bind to a non-dual-stack address only on the current stack.
        OnlyCurrentStack(
            MaybeDualStack<&'a mut DS, &'a mut NDS>,
            Option<ZonedAddr<SocketIpAddr<I::Addr>, DS::DeviceId>>,
        ),
        /// Bind to an address only on the other stack.
        OnlyOtherStack(
            &'a mut DS,
            Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, DS::DeviceId>>,
        ),
    }

    let mut entry = match state.entry(id.get_key_index()) {
        DenseMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
        DenseMapEntry::Occupied(o) => o,
    };
    let UnboundSocketState { device, sharing, ip_options } = match entry.get() {
        SocketState::Unbound(state) => state,
        SocketState::Bound(_) => return Err(Either::Left(ExpectedUnboundError)),
    };

    let dual_stack = core_ctx.dual_stack_context();
    let bound_operation: BoundOperation<'_, I, _, _> = match (dual_stack, addr) {
        // Dual-stack support and unspecified address.
        (MaybeDualStack::DualStack(dual_stack), None) => {
            match dual_stack.dual_stack_enabled(ip_options) {
                // Socket is dual-stack enabled, bind in both stacks.
                true => BoundOperation::DualStackAnyAddr(dual_stack),
                // Dual-stack support but not enabled, so bind unspecified in the
                // current stack.
                false => {
                    BoundOperation::OnlyCurrentStack(MaybeDualStack::DualStack(dual_stack), None)
                }
            }
        }
        // There is dual-stack support and the address is not unspecified so how
        // to proceed is going to depend on the value of `addr`.
        (MaybeDualStack::DualStack(dual_stack), Some(addr)) => match try_unmap(addr) {
            // `addr` can't be represented in the other stack.
            TryUnmapResult::CannotBeUnmapped(addr) => {
                BoundOperation::OnlyCurrentStack(MaybeDualStack::DualStack(dual_stack), Some(addr))
            }
            // There's a representation in the other stack, so use that if possible.
            TryUnmapResult::Mapped(addr) => match dual_stack.dual_stack_enabled(ip_options) {
                true => BoundOperation::OnlyOtherStack(dual_stack, addr),
                false => return Err(Either::Right(LocalAddressError::CannotBindToAddress)),
            },
        },
        // No dual-stack support, so only bind on the current stack.
        (MaybeDualStack::NotDualStack(single_stack), None) => {
            BoundOperation::OnlyCurrentStack(MaybeDualStack::NotDualStack(single_stack), None)
        }
        // No dual-stack support, so check the address is allowed in the current
        // stack.
        (MaybeDualStack::NotDualStack(single_stack), Some(addr)) => match try_unmap(addr) {
            // The address is only representable in the current stack.
            TryUnmapResult::CannotBeUnmapped(addr) => BoundOperation::OnlyCurrentStack(
                MaybeDualStack::NotDualStack(single_stack),
                Some(addr),
            ),
            // The address has a representation in the other stack but there's
            // no dual-stack support!
            TryUnmapResult::Mapped(_addr) => {
                let _: Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, _>> = _addr;
                return Err(Either::Right(LocalAddressError::CannotBindToAddress));
            }
        },
    };

    fn try_bind_single_stack<
        I: IpExt,
        S: DatagramSocketSpec,
        CC: TransportIpContext<I, BC>,
        BC: RngContext,
    >(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        bound: &mut BoundSocketMap<
            I,
            CC::WeakDeviceId,
            S::AddrSpec,
            S::SocketMapSpec<I, CC::WeakDeviceId>,
        >,
        addr: Option<ZonedAddr<SocketIpAddr<I::Addr>, CC::DeviceId>>,
        device: &Option<CC::WeakDeviceId>,
        local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        id: <S::SocketMapSpec<I, CC::WeakDeviceId> as SocketMapStateSpec>::ListenerId,
        sharing: S::SharingState,
    ) -> Result<
        ListenerAddr<
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
            CC::WeakDeviceId,
        >,
        LocalAddressError,
    > {
        let identifier = match local_id {
            Some(id) => Some(id),
            None => try_pick_identifier::<I, S, _, _, _>(
                addr.as_ref().map(ZonedAddr::addr),
                bound,
                bindings_ctx,
                &sharing,
            ),
        }
        .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
        let (addr, device, identifier) =
            try_pick_bound_address::<I, _, _, _>(addr, device, core_ctx, identifier)?;
        let weak_device = device.map(|d| d.as_weak(core_ctx).into_owned());

        // TODO(https://fxbug.dev/132092): Delete conversion once `ZonedAddr`
        // holds `NonMappedAddr`.
        let addr =
            addr.map(SocketIpAddr::try_from).transpose().expect("ZonedAddr should be non-mapped");
        BoundStateHandler::<_, S, _>::try_insert_listener(
            bound,
            addr,
            identifier,
            weak_device.clone(),
            sharing,
            id,
        )
        .map(|()| ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device: weak_device })
    }

    let bound_addr: ListenerAddr<S::ListenerIpAddr<I>, CC::WeakDeviceId> = match bound_operation {
        BoundOperation::OnlyCurrentStack(either_dual_stack, addr) => {
            let converter = either_dual_stack.to_converter();
            core_ctx
                .with_bound_sockets_mut(|core_ctx, bound, _allocator| {
                    let id = S::make_bound_socket_map_id(id);

                    try_bind_single_stack::<I, S, _, _>(
                        core_ctx,
                        bindings_ctx,
                        bound,
                        addr,
                        device,
                        local_id,
                        id,
                        sharing.clone(),
                    )
                })
                .map(|ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }| {
                    let ip = match converter {
                        MaybeDualStack::DualStack(converter) => converter.convert_back(
                            DualStackListenerIpAddr::ThisStack(ListenerIpAddr { addr, identifier }),
                        ),
                        MaybeDualStack::NotDualStack(converter) => {
                            converter.convert_back(ListenerIpAddr { addr, identifier })
                        }
                    };
                    ListenerAddr { ip, device }
                })
        }
        BoundOperation::OnlyOtherStack(core_ctx, addr) => {
            let id = core_ctx.to_other_bound_socket_id(id);
            core_ctx
                .with_other_bound_sockets_mut(|core_ctx, other_bound| {
                    try_bind_single_stack::<_, S, _, _>(
                        core_ctx,
                        bindings_ctx,
                        other_bound,
                        addr,
                        device,
                        local_id,
                        id,
                        sharing.clone(),
                    )
                })
                .map(|ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }| {
                    ListenerAddr {
                        ip: core_ctx.converter().convert_back(DualStackListenerIpAddr::OtherStack(
                            ListenerIpAddr { addr, identifier },
                        )),
                        device,
                    }
                })
        }
        BoundOperation::DualStackAnyAddr(core_ctx) => {
            let ids = PairedBoundSocketIds {
                this: S::make_bound_socket_map_id(id.clone()),
                other: core_ctx.to_other_bound_socket_id(id),
            };
            core_ctx
                .with_both_bound_sockets_mut(
                    |core_ctx, bound, other_bound, _alloc, _other_alloc| {
                        let mut bound_pair = PairedSocketMapMut { bound, other_bound };
                        let sharing = sharing.clone();

                        let identifier = match local_id {
                            Some(id) => Some(id),
                            None => try_pick_identifier::<I, S, _, _, _>(
                                DualStackUnspecifiedAddr,
                                &bound_pair,
                                bindings_ctx,
                                &sharing,
                            ),
                        }
                        .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
                        let (_addr, device, identifier) = try_pick_bound_address::<I, _, _, _>(
                            None, device, core_ctx, identifier,
                        )?;
                        let weak_device = device.map(|d| d.as_weak(core_ctx).into_owned());

                        BoundStateHandler::<_, S, _>::try_insert_listener(
                            &mut bound_pair,
                            DualStackUnspecifiedAddr,
                            identifier,
                            weak_device.clone(),
                            sharing,
                            ids,
                        )
                        .map(|()| (identifier, weak_device))
                    },
                )
                .map(|(identifier, device)| ListenerAddr {
                    ip: core_ctx
                        .converter()
                        .convert_back(DualStackListenerIpAddr::BothStacks(identifier)),
                    device,
                })
        }
    }
    .map_err(Either::Right)?;
    // Match Linux behavior by only storing the original bound addr when the
    // local_id was provided by the caller.
    let original_bound_addr = local_id.map(|_id| {
        let ListenerAddr { ip, device: _ } = &bound_addr;
        ip.clone()
    });

    // Replace the unbound state only after we're sure the
    // insertion has succeeded.
    *entry.get_mut() = SocketState::Bound(BoundSocketState {
        socket_type: BoundSocketStateType::Listener {
            state: ListenerState {
                // TODO(https://fxbug.dev/131970): Remove this clone().
                ip_options: ip_options.clone(),
                addr: bound_addr,
            },
            sharing: sharing.clone(),
        },
        original_bound_addr,
    });
    Ok(())
}

/// An error when attempting to create a datagram socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConnectError {
    /// An error was encountered creating an IP socket.
    #[error("{}", _0)]
    Ip(#[from] IpSockCreationError),
    /// No local port was specified, and none could be automatically allocated.
    #[error("a local port could not be allocated")]
    CouldNotAllocateLocalPort,
    /// The specified socket addresses (IP addresses and ports) conflict with an
    /// existing socket.
    #[error("the socket's IP address and port conflict with an existing socket")]
    SockAddrConflict,
    /// There was a problem with the provided address relating to its zone.
    #[error("{}", _0)]
    Zone(#[from] ZonedAddressError),
    /// The remote address is mapped (i.e. an ipv4-mapped-ipv6 address), but the
    /// socket is not dual-stack enabled.
    #[error("IPv4-mapped-IPv6 addresses are not supported by this socket")]
    RemoteUnexpectedlyMapped,
    /// The remote address is non-mapped (i.e not an ipv4-mapped-ipv6 address),
    /// but the socket is dual stack enabled and bound to a mapped address.
    #[error("non IPv4-mapped-Ipv6 addresses are not supported by this socket")]
    RemoteUnexpectedlyNonMapped,
}

/// Parameters required to connect a socket.
struct ConnectParameters<WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec> {
    local_ip: Option<SocketIpAddr<WireI::Addr>>,
    local_port: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
    remote_ip: ZonedAddr<SocketIpAddr<WireI::Addr>, D::Strong>,
    remote_port: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: Option<D>,
    sharing: S::SharingState,
    ip_options: IpOptions<SocketI, D, S>,
    send_options: SocketHopLimits<WireI>,
    socket_id:
        <S::SocketMapSpec<WireI, D> as DatagramSocketMapSpec<WireI, D, S::AddrSpec>>::BoundSocketId,
    original_shutdown: Option<Shutdown>,
    extra: S::ConnStateExtra,
}

/// Inserts a connected socket into the bound socket map.
///
/// It accepts two closures that capture the logic required to remove and
/// reinsert the original state from/into the bound_socket_map. The original
/// state will only be reinserted if an error is encountered during connect.
/// The output of `remove_original` is fed into `reinsert_original`.
fn connect_inner<
    WireI: IpExt,
    SocketI: IpExt,
    D: WeakId,
    S: DatagramSocketSpec,
    R,
    BC,
    CC: IpSocketHandler<WireI, BC, WeakDeviceId = D, DeviceId = D::Strong>,
    A: LocalIdentifierAllocator<WireI, D, S::AddrSpec, BC, S::SocketMapSpec<WireI, D>>,
>(
    connect_params: ConnectParameters<WireI, SocketI, D, S>,
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    sockets: &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
    allocator: &mut A,
    remove_original: impl FnOnce(
        &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
    ) -> R,
    reinsert_original: impl FnOnce(
        &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
        R,
    ),
) -> Result<ConnState<WireI, SocketI, D, S>, ConnectError> {
    let ConnectParameters {
        local_ip,
        local_port,
        remote_ip,
        remote_port,
        device,
        sharing,
        ip_options,
        send_options,
        socket_id,
        original_shutdown,
        extra,
    } = connect_params;

    let (remote_ip, socket_device) =
        crate::transport::resolve_addr_with_device::<WireI::Addr, _, _, _>(
            remote_ip,
            device.clone(),
        )?;

    let clear_device_on_disconnect = device.is_none() && socket_device.is_some();

    let ip_sock = IpSocketHandler::<WireI, _>::new_ip_socket(
        core_ctx,
        bindings_ctx,
        socket_device.as_ref().map(|d| d.as_ref()),
        local_ip.map(SocketIpAddr::into),
        remote_ip,
        S::ip_proto::<WireI>(),
        send_options,
    )
    .map_err(|(e, _ip_options)| e)?;

    let local_port = match local_port {
        Some(id) => id.clone(),
        None => allocator
            .try_alloc_local_id(
                sockets,
                bindings_ctx,
                DatagramFlowId {
                    local_ip: *ip_sock.local_ip(),
                    remote_ip: *ip_sock.remote_ip(),
                    remote_id: remote_port.clone(),
                },
            )
            .ok_or(ConnectError::CouldNotAllocateLocalPort)?,
    };
    let conn_addr = ConnAddr {
        ip: ConnIpAddr {
            local: (*ip_sock.local_ip(), local_port),
            remote: (*ip_sock.remote_ip(), remote_port),
        },
        device: ip_sock.device().cloned(),
    };
    // Now that all the other checks have been done, actually remove the
    // original state from the socket map.
    let reinsert_op = remove_original(sockets);
    // Try to insert the new connection, restoring the original state on
    // failure.
    let bound_addr = match sockets.conns_mut().try_insert(conn_addr, sharing, socket_id) {
        Ok(bound_entry) => bound_entry.get_addr().clone(),
        Err((
            InsertError::Exists
            | InsertError::IndirectConflict
            | InsertError::ShadowerExists
            | InsertError::ShadowAddrExists,
            _sharing,
        )) => {
            reinsert_original(sockets, reinsert_op);
            return Err(ConnectError::SockAddrConflict);
        }
    };
    Ok(ConnState {
        socket: ip_sock,
        ip_options,
        clear_device_on_disconnect,
        shutdown: original_shutdown.unwrap_or(Shutdown::default()),
        addr: bound_addr,
        extra,
    })
}

/// State required to perform single-stack connection of a socket.
struct SingleStackConnectOperation<I: IpExt, D: WeakId, S: DatagramSocketSpec> {
    params: ConnectParameters<I, I, D, S>,
    remove_op: Option<SingleStackRemoveOperation<I, D, S>>,
    sharing: S::SharingState,
}

impl<I: IpExt, D: WeakId, S: DatagramSocketSpec> SingleStackConnectOperation<I, D, S> {
    /// Constructs the connect operation from existing socket state.
    fn new_from_state<
        BC,
        CC: NonDualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D, DeviceId = D::Strong>,
    >(
        core_ctx: &mut CC,
        socket_id: S::SocketId<I>,
        state: &SocketState<I, D, S>,
        remote_ip: ZonedAddr<SocketIpAddr<I::Addr>, D::Strong>,
        remote_port: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        extra: S::ConnStateExtra,
    ) -> Self {
        match state {
            SocketState::Unbound(UnboundSocketState { device, sharing, ip_options }) => {
                SingleStackConnectOperation {
                    params: ConnectParameters {
                        local_ip: None,
                        local_port: None,
                        remote_ip,
                        remote_port,
                        device: device.clone(),
                        sharing: sharing.clone(),
                        ip_options: ip_options.clone(),
                        send_options: ip_options.hop_limits,
                        socket_id: S::make_bound_socket_map_id(socket_id),
                        original_shutdown: None,
                        extra,
                    },
                    sharing: sharing.clone(),
                    remove_op: None,
                }
            }
            SocketState::Bound(state) => {
                let remove_op =
                    SingleStackRemoveOperation::new_from_state(core_ctx, socket_id.clone(), state);
                let BoundSocketState { socket_type, original_bound_addr: _ } = state;
                match socket_type {
                    BoundSocketStateType::Listener {
                        state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                        sharing,
                    } => {
                        let ListenerIpAddr { addr, identifier } = core_ctx.converter().convert(ip);
                        SingleStackConnectOperation {
                            params: ConnectParameters {
                                local_ip: addr.clone(),
                                local_port: Some(*identifier),
                                remote_ip,
                                remote_port,
                                device: device.clone(),
                                sharing: sharing.clone(),
                                ip_options: ip_options.clone(),
                                send_options: ip_options.hop_limits,
                                socket_id: S::make_bound_socket_map_id(socket_id),
                                original_shutdown: None,
                                extra,
                            },
                            sharing: sharing.clone(),
                            remove_op: Some(remove_op),
                        }
                    }
                    BoundSocketStateType::Connected { state, sharing } => {
                        let ConnState {
                            socket: _,
                            ip_options,
                            shutdown,
                            addr:
                                ConnAddr {
                                    ip: ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                    device,
                                },
                            clear_device_on_disconnect: _,
                            extra: _,
                        } = core_ctx.converter().convert(state);
                        SingleStackConnectOperation {
                            params: ConnectParameters {
                                local_ip: Some(local_ip.clone()),
                                local_port: Some(*local_id),
                                remote_ip,
                                remote_port,
                                device: device.clone(),
                                sharing: sharing.clone(),
                                ip_options: ip_options.clone(),
                                send_options: ip_options.hop_limits,
                                socket_id: S::make_bound_socket_map_id(socket_id),
                                original_shutdown: Some(shutdown.clone()),
                                extra,
                            },
                            sharing: sharing.clone(),
                            remove_op: Some(remove_op),
                        }
                    }
                }
            }
        }
    }

    /// Performs this operation and connects the socket.
    ///
    /// This is primarily a wrapper around `connect_inner` that establishes the
    /// remove/reinsert closures for single stack removal.
    ///
    /// Returns a tuple containing the state, and sharing state for the new
    /// connection.
    fn apply<
        BC,
        CC: IpSocketHandler<I, BC, WeakDeviceId = D, DeviceId = D::Strong>,
        A: LocalIdentifierAllocator<I, D, S::AddrSpec, BC, S::SocketMapSpec<I, D>>,
    >(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        socket_map: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        allocator: &mut A,
    ) -> Result<(ConnState<I, I, D, S>, S::SharingState), ConnectError> {
        let SingleStackConnectOperation { params, remove_op, sharing } = self;
        let remove_fn =
            |sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>| {
                remove_op.map(|remove_op| remove_op.apply(sockets))
            };
        let reinsert_fn =
            |sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
             remove_info: Option<SingleStackRemoveInfo<I, D, S>>| {
                if let Some(remove_info) = remove_info {
                    remove_info.reinsert(sockets)
                }
            };
        let conn_state = connect_inner(
            params,
            core_ctx,
            bindings_ctx,
            socket_map,
            allocator,
            remove_fn,
            reinsert_fn,
        )?;
        Ok((conn_state, sharing))
    }
}

/// State required to perform dual-stack connection of a socket.
struct DualStackConnectOperation<I: DualStackIpExt, D: WeakId, S: DatagramSocketSpec> {
    params: EitherStack<ConnectParameters<I, I, D, S>, ConnectParameters<I::OtherVersion, I, D, S>>,
    remove_op: Option<DualStackRemoveOperation<I, D, S>>,
    sharing: S::SharingState,
}

impl<I: DualStackIpExt, D: WeakId, S: DatagramSocketSpec> DualStackConnectOperation<I, D, S> {
    /// Constructs the connect operation from existing socket state.
    fn new_from_state<
        BC,
        CC: DualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D, DeviceId = D::Strong>,
    >(
        core_ctx: &mut CC,
        socket_id: S::SocketId<I>,
        state: &SocketState<I, D, S>,
        remote_ip: DualStackRemoteIp<I, D::Strong>,
        remote_port: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        extra: S::ConnStateExtra,
    ) -> Result<Self, ConnectError> {
        match state {
            SocketState::Unbound(UnboundSocketState { device, sharing, ip_options }) => {
                // Unbound sockets don't have a predisposition of which stack to
                // connect in. Instead, it's dictated entirely by the remote.
                let params = match remote_ip {
                    DualStackRemoteIp::ThisStack(remote_ip) => {
                        EitherStack::ThisStack(ConnectParameters {
                            local_ip: None,
                            local_port: None,
                            remote_ip,
                            remote_port,
                            device: device.clone(),
                            sharing: sharing.clone(),
                            ip_options: ip_options.clone(),
                            send_options: ip_options.hop_limits,
                            socket_id: S::make_bound_socket_map_id(socket_id),
                            original_shutdown: None,
                            extra,
                        })
                    }
                    DualStackRemoteIp::OtherStack(remote_ip) => {
                        if !core_ctx.dual_stack_enabled(ip_options) {
                            return Err(ConnectError::RemoteUnexpectedlyMapped);
                        }
                        EitherStack::OtherStack(ConnectParameters {
                            local_ip: None,
                            local_port: None,
                            remote_ip,
                            remote_port,
                            device: device.clone(),
                            sharing: sharing.clone(),
                            ip_options: ip_options.clone(),
                            send_options: core_ctx.to_other_send_options(ip_options).clone(),
                            socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                            original_shutdown: None,
                            extra,
                        })
                    }
                };
                Ok(DualStackConnectOperation { params, remove_op: None, sharing: sharing.clone() })
            }
            SocketState::Bound(state) => {
                let remove_op =
                    DualStackRemoveOperation::new_from_state(core_ctx, socket_id.clone(), state);

                let BoundSocketState { socket_type, original_bound_addr: _ } = state;
                match socket_type {
                    BoundSocketStateType::Listener {
                        state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                        sharing,
                    } => {
                        match (remote_ip, core_ctx.converter().convert(ip)) {
                            // Disallow connecting to the other stack because the
                            // existing socket state is in this stack.
                            (
                                DualStackRemoteIp::OtherStack(_),
                                DualStackListenerIpAddr::ThisStack(_),
                            ) => Err(ConnectError::RemoteUnexpectedlyMapped),
                            // Disallow connecting to this stack because the existing
                            // socket state is in the other stack.
                            (
                                DualStackRemoteIp::ThisStack(_),
                                DualStackListenerIpAddr::OtherStack(_),
                            ) => Err(ConnectError::RemoteUnexpectedlyNonMapped),
                            // Connect in this stack.
                            (
                                DualStackRemoteIp::ThisStack(remote_ip),
                                DualStackListenerIpAddr::ThisStack(ListenerIpAddr {
                                    addr,
                                    identifier,
                                }),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::ThisStack(ConnectParameters {
                                    local_ip: addr.clone(),
                                    local_port: Some(*identifier),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: ip_options.hop_limits,
                                    socket_id: S::make_bound_socket_map_id(socket_id),
                                    original_shutdown: None,
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                            // Listeners in "both stacks" can connect to either
                            // stack. Connect in this stack as specified by the
                            // remote.
                            (
                                DualStackRemoteIp::ThisStack(remote_ip),
                                DualStackListenerIpAddr::BothStacks(identifier),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::ThisStack(ConnectParameters {
                                    local_ip: None,
                                    local_port: Some(*identifier),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: ip_options.hop_limits,
                                    socket_id: S::make_bound_socket_map_id(socket_id),
                                    original_shutdown: None,
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                            // Connect in the other stack.
                            (
                                DualStackRemoteIp::OtherStack(remote_ip),
                                DualStackListenerIpAddr::OtherStack(ListenerIpAddr {
                                    addr,
                                    identifier,
                                }),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::OtherStack(ConnectParameters {
                                    local_ip: addr.clone(),
                                    local_port: Some(*identifier),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: core_ctx
                                        .to_other_send_options(ip_options)
                                        .clone(),
                                    socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                                    original_shutdown: None,
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                            // Listeners in "both stacks" can connect to either
                            // stack. Connect in the other stack as specified by
                            // the remote.
                            (
                                DualStackRemoteIp::OtherStack(remote_ip),
                                DualStackListenerIpAddr::BothStacks(identifier),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::OtherStack(ConnectParameters {
                                    local_ip: None,
                                    local_port: Some(*identifier),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: core_ctx
                                        .to_other_send_options(ip_options)
                                        .clone(),
                                    socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                                    original_shutdown: None,
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                        }
                    }
                    BoundSocketStateType::Connected { state, sharing } => {
                        match (remote_ip, core_ctx.converter().convert(state)) {
                            // Disallow connecting to the other stack because the
                            // existing socket state is in this stack.
                            (
                                DualStackRemoteIp::OtherStack(_),
                                DualStackConnState::ThisStack(_),
                            ) => Err(ConnectError::RemoteUnexpectedlyMapped),
                            // Disallow connecting to this stack because the existing
                            // socket state is in the other stack.
                            (
                                DualStackRemoteIp::ThisStack(_),
                                DualStackConnState::OtherStack(_),
                            ) => Err(ConnectError::RemoteUnexpectedlyNonMapped),
                            // Connect in this stack.
                            (
                                DualStackRemoteIp::ThisStack(remote_ip),
                                DualStackConnState::ThisStack(ConnState {
                                    socket: _,
                                    ip_options,
                                    shutdown,
                                    addr:
                                        ConnAddr {
                                            ip:
                                                ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                            device,
                                        },
                                    clear_device_on_disconnect: _,
                                    extra: _,
                                }),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::ThisStack(ConnectParameters {
                                    local_ip: Some(local_ip.clone()),
                                    local_port: Some(*local_id),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: ip_options.hop_limits,
                                    socket_id: S::make_bound_socket_map_id(socket_id),
                                    original_shutdown: Some(shutdown.clone()),
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                            // Connect in the other stack.
                            (
                                DualStackRemoteIp::OtherStack(remote_ip),
                                DualStackConnState::OtherStack(ConnState {
                                    socket: _,
                                    ip_options,
                                    shutdown,
                                    addr:
                                        ConnAddr {
                                            ip:
                                                ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                            device,
                                        },
                                    clear_device_on_disconnect: _,
                                    extra: _,
                                }),
                            ) => Ok(DualStackConnectOperation {
                                params: EitherStack::OtherStack(ConnectParameters {
                                    local_ip: Some(local_ip.clone()),
                                    local_port: Some(*local_id),
                                    remote_ip,
                                    remote_port,
                                    device: device.clone(),
                                    sharing: sharing.clone(),
                                    ip_options: ip_options.clone(),
                                    send_options: core_ctx
                                        .to_other_send_options(ip_options)
                                        .clone(),
                                    socket_id: core_ctx.to_other_bound_socket_id(socket_id),
                                    original_shutdown: Some(shutdown.clone()),
                                    extra,
                                }),
                                sharing: sharing.clone(),
                                remove_op: Some(remove_op),
                            }),
                        }
                    }
                }
            }
        }
    }

    /// Performs this operation and connects the socket.
    ///
    /// This is primarily a wrapper around [`connect_inner`] that establishes the
    /// remove/reinsert closures for dual stack removal.
    ///
    /// Returns a tuple containing the state, and sharing state for the new
    /// connection.
    fn apply<
        BC,
        CC: IpSocketHandler<I, BC, WeakDeviceId = D, DeviceId = D::Strong>
            + IpSocketHandler<I::OtherVersion, BC, WeakDeviceId = D, DeviceId = D::Strong>,
        A: LocalIdentifierAllocator<I, D, S::AddrSpec, BC, S::SocketMapSpec<I, D>>,
        OA: LocalIdentifierAllocator<
            I::OtherVersion,
            D,
            S::AddrSpec,
            BC,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
    >(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        socket_map: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        other_socket_map: &mut BoundSocketMap<
            I::OtherVersion,
            D,
            S::AddrSpec,
            S::SocketMapSpec<I::OtherVersion, D>,
        >,
        allocator: &mut A,
        other_allocator: &mut OA,
    ) -> Result<(DualStackConnState<I, D, S>, S::SharingState), ConnectError> {
        let DualStackConnectOperation { params, remove_op, sharing } = self;
        let conn_state = match params {
            EitherStack::ThisStack(params) => {
                // NB: Because we're connecting in this stack, we receive this
                // stack's sockets as an argument to `remove_fn` and
                // `reinsert_fn`. Thus we need to capture + pass through the
                // other stack's sockets.
                let remove_fn =
                    |sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>| {
                        remove_op.map(|remove_op| {
                            let remove_info = remove_op.apply(sockets, other_socket_map);
                            (remove_info, other_socket_map)
                        })
                    };
                let reinsert_fn =
                    |sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
                     remove_info: Option<(
                        DualStackRemoveInfo<I, D, S>,
                        &mut BoundSocketMap<
                            I::OtherVersion,
                            D,
                            S::AddrSpec,
                            S::SocketMapSpec<I::OtherVersion, D>,
                        >,
                    )>| {
                        if let Some((remove_info, other_sockets)) = remove_info {
                            remove_info.reinsert(sockets, other_sockets)
                        }
                    };
                connect_inner(
                    params,
                    core_ctx,
                    bindings_ctx,
                    socket_map,
                    allocator,
                    remove_fn,
                    reinsert_fn,
                )
                .map(DualStackConnState::ThisStack)
            }
            EitherStack::OtherStack(params) => {
                // NB: Because we're connecting in the other stack, we receive
                // the other stack's sockets as an argument to `remove_fn` and
                // `reinsert_fn`. Thus we need to capture + pass through this
                // stack's sockets.
                let remove_fn = |other_sockets: &mut BoundSocketMap<
                    I::OtherVersion,
                    D,
                    S::AddrSpec,
                    S::SocketMapSpec<I::OtherVersion, D>,
                >| {
                    remove_op.map(|remove_op| {
                        let remove_info = remove_op.apply(socket_map, other_sockets);
                        (remove_info, socket_map)
                    })
                };
                let reinsert_fn = |other_sockets: &mut BoundSocketMap<
                    I::OtherVersion,
                    D,
                    S::AddrSpec,
                    S::SocketMapSpec<I::OtherVersion, D>,
                >,
                                   remove_info: Option<(
                    DualStackRemoveInfo<I, D, S>,
                    &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
                )>| {
                    if let Some((remove_info, sockets)) = remove_info {
                        remove_info.reinsert(sockets, other_sockets)
                    }
                };
                connect_inner(
                    params,
                    core_ctx,
                    bindings_ctx,
                    other_socket_map,
                    other_allocator,
                    remove_fn,
                    reinsert_fn,
                )
                .map(DualStackConnState::OtherStack)
            }
        }?;
        Ok((conn_state, sharing))
    }
}

pub(crate) fn connect<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, CC::DeviceId>>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    extra: S::ConnStateExtra,
) -> Result<(), ConnectError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let mut entry = match state.entry(id.get_key_index()) {
            DenseMapEntry::Vacant(_) => panic!("socket ID {:?} is invalid", id),
            DenseMapEntry::Occupied(o) => o,
        };

        let (conn_state, sharing) = match (
            core_ctx.dual_stack_context(),
            dual_stack_remote_ip::<I, _>(remote_ip.clone()),
        ) {
            (MaybeDualStack::DualStack(ds), remote_ip) => {
                let connect_op = DualStackConnectOperation::new_from_state(
                    ds,
                    id,
                    entry.get(),
                    remote_ip,
                    remote_id,
                    extra,
                )?;
                let converter = ds.converter();
                let (conn_state, sharing) = ds.with_both_bound_sockets_mut(
                    |core_ctx, bound, other_bound, alloc, other_alloc| {
                        connect_op.apply(
                            core_ctx,
                            bindings_ctx,
                            bound,
                            other_bound,
                            alloc,
                            other_alloc,
                        )
                    },
                )?;
                Ok((converter.convert_back(conn_state), sharing))
            }
            (MaybeDualStack::NotDualStack(nds), DualStackRemoteIp::ThisStack(remote_ip)) => {
                let connect_op = SingleStackConnectOperation::new_from_state(
                    nds,
                    id,
                    entry.get(),
                    remote_ip,
                    remote_id,
                    extra,
                );
                let converter = nds.converter();
                let (conn_state, sharing) =
                    core_ctx.with_bound_sockets_mut(|core_ctx, bound, alloc| {
                        connect_op.apply(core_ctx, bindings_ctx, bound, alloc)
                    })?;
                Ok((converter.convert_back(conn_state), sharing))
            }
            (MaybeDualStack::NotDualStack(_), DualStackRemoteIp::OtherStack(_)) => {
                Err(ConnectError::RemoteUnexpectedlyMapped)
            }
        }?;
        let original_bound_addr = match entry.get() {
            SocketState::Unbound(_) => None,
            SocketState::Bound(BoundSocketState { socket_type: _, original_bound_addr }) => {
                original_bound_addr.clone()
            }
        };
        *entry.get_mut() = SocketState::Bound(BoundSocketState {
            socket_type: BoundSocketStateType::Connected { state: conn_state, sharing },
            original_bound_addr,
        });
        Ok(())
    })
}

/// A connected socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub struct ExpectedConnError;

/// An unbound socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub struct ExpectedUnboundError;

pub(crate) fn disconnect_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: S::SocketId<I>,
) -> Result<(), ExpectedConnError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let mut entry = match state.entry(id.get_key_index()) {
            DenseMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
            DenseMapEntry::Occupied(o) => o,
        };
        let state = match entry.get() {
            SocketState::Unbound(_) => return Err(ExpectedConnError),
            SocketState::Bound(state) => state,
        };
        let BoundSocketState { socket_type, original_bound_addr } = state;
        let conn_state = match socket_type {
            BoundSocketStateType::Listener { state: _, sharing: _ } => {
                return Err(ExpectedConnError)
            }
            BoundSocketStateType::Connected { state, sharing: _ } => state,
        };

        let clear_device_on_disconnect = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(dual_stack) => match dual_stack
                .converter()
                .convert(conn_state)
            {
                DualStackConnState::ThisStack(conn_state) => conn_state.clear_device_on_disconnect,
                DualStackConnState::OtherStack(conn_state) => conn_state.clear_device_on_disconnect,
            },
            MaybeDualStack::NotDualStack(not_dual_stack) => {
                not_dual_stack.converter().convert(conn_state).clear_device_on_disconnect
            }
        };

        *entry.get_mut() = match original_bound_addr {
            None => SocketState::Unbound(disconnect_to_unbound(
                core_ctx,
                id,
                clear_device_on_disconnect,
                state,
            )),
            Some(original_bound_addr) => SocketState::Bound(disconnect_to_listener(
                core_ctx,
                id,
                original_bound_addr.clone(),
                clear_device_on_disconnect,
                state,
            )),
        };
        Ok(())
    })
}

/// Converts a connected socket to an unbound socket.
///
/// Removes the connection's entry from the [`BoundSocketMap`], and returns the
/// socket's new state.
fn disconnect_to_unbound<
    I: IpExt,
    BC,
    CC: DatagramBoundStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
    clear_device_on_disconnect: bool,
    socket_state: &BoundSocketState<I, CC::WeakDeviceId, S>,
) -> UnboundSocketState<I, CC::WeakDeviceId, S> {
    let (ip_options, sharing, mut device) = match core_ctx.dual_stack_context() {
        MaybeDualStack::NotDualStack(nds) => {
            let remove_op = SingleStackRemoveOperation::new_from_state(nds, id, socket_state);
            let info = core_ctx
                .with_bound_sockets_mut(|_core_ctx, bound, _allocator| remove_op.apply(bound));
            info.into_options_sharing_and_device()
        }
        MaybeDualStack::DualStack(ds) => {
            let remove_op = DualStackRemoveOperation::new_from_state(ds, id, socket_state);
            let info = ds.with_both_bound_sockets_mut(
                |_core_ctx, bound, other_bound, _allocator, _other_allocator| {
                    remove_op.apply(bound, other_bound)
                },
            );
            info.into_options_sharing_and_device()
        }
    };
    if clear_device_on_disconnect {
        device = None
    }
    UnboundSocketState { device, sharing, ip_options }
}

/// Converts a connected socket to a listener socket.
///
/// Removes the connection's entry from the [`BoundSocketMap`] and returns the
/// socket's new state.
fn disconnect_to_listener<
    I: IpExt,
    BC,
    CC: DatagramBoundStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
    listener_ip: S::ListenerIpAddr<I>,
    clear_device_on_disconnect: bool,
    socket_state: &BoundSocketState<I, CC::WeakDeviceId, S>,
) -> BoundSocketState<I, CC::WeakDeviceId, S> {
    let (ip_options, sharing, device) = match core_ctx.dual_stack_context() {
        MaybeDualStack::NotDualStack(nds) => {
            let ListenerIpAddr { addr, identifier } = nds.converter().convert(listener_ip.clone());
            let remove_op =
                SingleStackRemoveOperation::new_from_state(nds, id.clone(), socket_state);
            core_ctx.with_bound_sockets_mut(|_core_ctx, bound, _allocator| {
                let (ip_options, sharing, mut device) =
                    remove_op.apply(bound).into_options_sharing_and_device();
                if clear_device_on_disconnect {
                    device = None;
                }
                BoundStateHandler::<_, S, _>::try_insert_listener(
                    bound,
                    addr,
                    identifier,
                    device.clone(),
                    sharing.clone(),
                    S::make_bound_socket_map_id(id),
                )
                .expect("inserting listener for disconnected socket should succeed");
                (ip_options, sharing, device)
            })
        }
        MaybeDualStack::DualStack(ds) => {
            let remove_op = DualStackRemoveOperation::new_from_state(ds, id.clone(), socket_state);
            let other_id = ds.to_other_bound_socket_id(id.clone());
            let id = S::make_bound_socket_map_id(id);
            let converter = ds.converter();
            ds.with_both_bound_sockets_mut(
                |_core_ctx, bound, other_bound, _allocator, _other_allocator| {
                    let (ip_options, sharing, mut device) =
                        remove_op.apply(bound, other_bound).into_options_sharing_and_device();
                    if clear_device_on_disconnect {
                        device = None;
                    }

                    match converter.convert(listener_ip.clone()) {
                        DualStackListenerIpAddr::ThisStack(ListenerIpAddr { addr, identifier }) => {
                            BoundStateHandler::<_, S, _>::try_insert_listener(
                                bound,
                                addr,
                                identifier,
                                device.clone(),
                                sharing.clone(),
                                id,
                            )
                        }
                        DualStackListenerIpAddr::OtherStack(ListenerIpAddr {
                            addr,
                            identifier,
                        }) => BoundStateHandler::<_, S, _>::try_insert_listener(
                            other_bound,
                            addr,
                            identifier,
                            device.clone(),
                            sharing.clone(),
                            other_id,
                        ),
                        DualStackListenerIpAddr::BothStacks(identifier) => {
                            let ids = PairedBoundSocketIds { this: id, other: other_id };
                            let mut bound_pair = PairedSocketMapMut { bound, other_bound };
                            BoundStateHandler::<_, S, _>::try_insert_listener(
                                &mut bound_pair,
                                DualStackUnspecifiedAddr,
                                identifier,
                                device.clone(),
                                sharing.clone(),
                                ids,
                            )
                        }
                    }
                    .expect("inserting listener for disconnected socket should succeed");
                    (ip_options, sharing, device)
                },
            )
        }
    };
    BoundSocketState {
        original_bound_addr: Some(listener_ip.clone()),
        socket_type: BoundSocketStateType::Listener {
            state: ListenerState { ip_options, addr: ListenerAddr { ip: listener_ip, device } },
            sharing,
        },
    }
}

/// Which direction(s) to shut down for a socket.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip()]
pub enum ShutdownType {
    /// Prevent sending packets on the socket.
    Send,
    /// Prevent receiving packets on the socket.
    Receive,
    /// Prevent sending and receiving packets on the socket.
    SendAndReceive,
}

pub(crate) fn shutdown_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let state = match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => return Err(ExpectedConnError),
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state: _, sharing: _ } => {
                        return Err(ExpectedConnError)
                    }
                    BoundSocketStateType::Connected { state, sharing: _ } => state,
                }
            }
        };
        let (shutdown_send, shutdown_receive) = match which {
            ShutdownType::Send => (true, false),
            ShutdownType::Receive => (false, true),
            ShutdownType::SendAndReceive => (true, true),
        };
        let Shutdown { send, receive } = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(ds) => ds.converter().convert(state).as_mut(),
            MaybeDualStack::NotDualStack(nds) => nds.converter().convert(state).as_mut(),
        };
        *send |= shutdown_send;
        *receive |= shutdown_receive;
        Ok(())
    })
}

pub(crate) fn get_shutdown_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
) -> Option<ShutdownType> {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => return None,
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state: _, sharing: _ } => return None,
                    BoundSocketStateType::Connected { state, sharing: _ } => state,
                }
            }
        };
        let Shutdown { send, receive } = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(ds) => ds.converter().convert(state).as_ref(),
            MaybeDualStack::NotDualStack(nds) => nds.converter().convert(state).as_ref(),
        };
        Some(match (send, receive) {
            (false, false) => return None,
            (true, false) => ShutdownType::Send,
            (false, true) => ShutdownType::Receive,
            (true, true) => ShutdownType::SendAndReceive,
        })
    })
}

/// Error encountered when sending a datagram on a socket.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip()]
pub enum SendError<SE> {
    /// The socket is not connected,
    NotConnected,
    /// The socket is not writeable.
    NotWriteable,
    /// There was a problem sending the IP packet.
    IpSock(IpSockSendError),
    /// There was a problem when serializing the packet.
    SerializeError(SE),
}

pub(crate) fn send_conn<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    body: B,
) -> Result<(), SendError<S::SerializeError>> {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => return Err(SendError::NotConnected),
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state: _, sharing: _ } => {
                        return Err(SendError::NotConnected)
                    }
                    BoundSocketStateType::Connected { state, sharing: _ } => state,
                }
            }
        };

        struct SendParams<'a, I: IpExt, S: DatagramSocketSpec, D: WeakId> {
            socket: &'a IpSock<I, D, SocketHopLimits<I>>,
            ip: &'a ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        }

        enum Operation<
            'a,
            I: DualStackIpExt,
            S: DatagramSocketSpec,
            D: WeakId,
            BC: DatagramStateBindingsContext<I, S>,
            DualStackSC: DualStackDatagramBoundStateContext<I, BC, S>,
            CC: DatagramBoundStateContext<I, BC, S>,
        > {
            SendToThisStack((SendParams<'a, I, S, D>, &'a mut CC)),
            SendToOtherStack((SendParams<'a, I::OtherVersion, S, D>, &'a mut DualStackSC)),
            // Allow `Operation` to be generic over `B` and `C` so that they can
            // be used in trait bounds for `DualStackSC` and `SC`.
            _Phantom((Never, PhantomData<BC>)),
        }

        let (shutdown, operation) = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(dual_stack) => match dual_stack.converter().convert(state) {
                DualStackConnState::ThisStack(ConnState {
                    socket,
                    ip_options: _,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                }) => (shutdown, Operation::SendToThisStack((SendParams { socket, ip }, core_ctx))),
                DualStackConnState::OtherStack(ConnState {
                    socket,
                    ip_options: _,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                }) => {
                    (shutdown, Operation::SendToOtherStack((SendParams { socket, ip }, dual_stack)))
                }
            },
            MaybeDualStack::NotDualStack(not_dual_stack) => {
                let ConnState {
                    socket,
                    ip_options: _,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                } = not_dual_stack.converter().convert(state);
                (shutdown, Operation::SendToThisStack((SendParams { socket, ip }, core_ctx)))
            }
        };

        let Shutdown { send: shutdown_send, receive: _ } = shutdown;
        if *shutdown_send {
            return Err(SendError::NotWriteable);
        }

        match operation {
            Operation::SendToThisStack((SendParams { socket, ip }, core_ctx)) => {
                let packet =
                    S::make_packet::<I, _>(body, &ip).map_err(SendError::SerializeError)?;
                core_ctx.with_transport_context(|core_ctx| {
                    core_ctx
                        .send_ip_packet(bindings_ctx, &socket, packet, None)
                        .map_err(|(_serializer, send_error)| SendError::IpSock(send_error))
                })
            }
            Operation::SendToOtherStack((SendParams { socket, ip }, dual_stack)) => {
                let packet = S::make_packet::<I::OtherVersion, _>(body, &ip)
                    .map_err(SendError::SerializeError)?;
                dual_stack.with_transport_context::<_, _>(|core_ctx| {
                    core_ctx
                        .send_ip_packet(bindings_ctx, &socket, packet, None)
                        .map_err(|(_serializer, send_error)| SendError::IpSock(send_error))
                })
            }
            Operation::_Phantom((never, _)) => match never {},
        }
    })
}

/// An error encountered while sending a datagram packet to an alternate address.
#[derive(Debug)]
pub enum SendToError<SE> {
    /// The socket is not writeable.
    NotWriteable,
    /// There was a problem with the remote address relating to its zone.
    Zone(ZonedAddressError),
    /// An error was encountered while trying to create a temporary IP socket
    /// to use for the send operation.
    CreateAndSend(IpSockCreateAndSendError),
    /// The remote address is mapped (i.e. an ipv4-mapped-ipv6 address), but the
    /// socket is not dual-stack enabled.
    RemoteUnexpectedlyMapped,
    /// The remote address is non-mapped (i.e not an ipv4-mapped-ipv6 address),
    /// but the socket is dual stack enabled and bound to a mapped address.
    RemoteUnexpectedlyNonMapped,
    /// The provided buffer is not vailid.
    SerializeError(SE),
}

pub(crate) fn send_to<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    remote_ip: Option<SocketZonedIpAddr<I::Addr, CC::DeviceId>>,
    remote_identifier: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    body: B,
) -> Result<(), Either<LocalAddressError, SendToError<S::SerializeError>>> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        match listen_inner(core_ctx, bindings_ctx, state, id.clone(), None, None) {
            Ok(()) | Err(Either::Left(ExpectedUnboundError)) => (),
            Err(Either::Right(e)) => return Err(Either::Left(e)),
        };
        let state = match state.get(id.get_key_index()).expect("no such socket") {
            SocketState::Unbound(_) => panic!("expected bound socket"),
            SocketState::Bound(BoundSocketState { socket_type: state, original_bound_addr: _ }) => {
                state
            }
        };

        enum Operation<
            'a,
            I: DualStackIpExt,
            S: DatagramSocketSpec,
            D: WeakId,
            BC: DatagramStateBindingsContext<I, S>,
            DualStackSC: DualStackDatagramBoundStateContext<I, BC, S>,
            CC: DatagramBoundStateContext<I, BC, S>,
            O: SendOptions<I>,
            OtherO: SendOptions<I::OtherVersion>,
        > {
            SendToThisStack((SendOneshotParameters<'a, I, S, D, O>, &'a mut CC)),

            SendToOtherStack(
                (SendOneshotParameters<'a, I::OtherVersion, S, D, OtherO>, &'a mut DualStackSC),
            ),
            // Allow `Operation` to be generic over `B` and `C` so that they can
            // be used in trait bounds for `DualStackSC` and `SC`.
            _Phantom((Never, PhantomData<BC>)),
        }

        let (operation, shutdown) = match (
            core_ctx.dual_stack_context(),
            dual_stack_remote_ip::<I, _>(remote_ip.clone()),
        ) {
            (MaybeDualStack::NotDualStack(_), DualStackRemoteIp::OtherStack(_)) => {
                return Err(Either::Right(SendToError::RemoteUnexpectedlyMapped))
            }
            (MaybeDualStack::NotDualStack(nds), DualStackRemoteIp::ThisStack(remote_ip)) => {
                match state {
                    BoundSocketStateType::Listener {
                        state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                        sharing: _,
                    } => {
                        let ListenerIpAddr { addr, identifier } =
                            nds.converter().convert(ip.clone());
                        (
                            Operation::SendToThisStack((
                                SendOneshotParameters {
                                    local_ip: addr,
                                    local_id: identifier,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device: &device,
                                    send_options: &ip_options.hop_limits,
                                },
                                core_ctx,
                            )),
                            None,
                        )
                    }
                    BoundSocketStateType::Connected { state, sharing: _ } => {
                        let ConnState {
                            socket: _,
                            ip_options,
                            clear_device_on_disconnect: _,
                            shutdown,
                            addr:
                                ConnAddr {
                                    ip: ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                    device,
                                },
                            extra: _,
                        } = nds.converter().convert(state);
                        (
                            Operation::SendToThisStack((
                                SendOneshotParameters {
                                    local_ip: Some(*local_ip),
                                    local_id: *local_id,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device: &device,
                                    send_options: &ip_options.hop_limits,
                                },
                                core_ctx,
                            )),
                            Some(shutdown),
                        )
                    }
                }
            }
            (MaybeDualStack::DualStack(ds), remote_ip) => match state {
                BoundSocketStateType::Listener {
                    state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                    sharing: _,
                } => match (ds.converter().convert(ip), remote_ip) {
                    (DualStackListenerIpAddr::ThisStack(_), DualStackRemoteIp::OtherStack(_)) => {
                        return Err(Either::Right(SendToError::RemoteUnexpectedlyMapped))
                    }
                    (DualStackListenerIpAddr::OtherStack(_), DualStackRemoteIp::ThisStack(_)) => {
                        return Err(Either::Right(SendToError::RemoteUnexpectedlyNonMapped))
                    }
                    (
                        DualStackListenerIpAddr::ThisStack(ListenerIpAddr { addr, identifier }),
                        DualStackRemoteIp::ThisStack(remote_ip),
                    ) => (
                        Operation::SendToThisStack((
                            SendOneshotParameters {
                                local_ip: *addr,
                                local_id: *identifier,
                                remote_ip,
                                remote_id: remote_identifier,
                                device: &device,
                                send_options: &ip_options.hop_limits,
                            },
                            core_ctx,
                        )),
                        None,
                    ),
                    (
                        DualStackListenerIpAddr::BothStacks(identifier),
                        DualStackRemoteIp::ThisStack(remote_ip),
                    ) => (
                        Operation::SendToThisStack((
                            SendOneshotParameters {
                                local_ip: None,
                                local_id: *identifier,
                                remote_ip,
                                remote_id: remote_identifier,
                                device: &device,
                                send_options: &ip_options.hop_limits,
                            },
                            core_ctx,
                        )),
                        None,
                    ),
                    (
                        DualStackListenerIpAddr::OtherStack(ListenerIpAddr { addr, identifier }),
                        DualStackRemoteIp::OtherStack(remote_ip),
                    ) => (
                        Operation::SendToOtherStack((
                            SendOneshotParameters {
                                local_ip: *addr,
                                local_id: *identifier,
                                remote_ip,
                                remote_id: remote_identifier,
                                device: &device,
                                send_options: ds.to_other_send_options(&ip_options),
                            },
                            ds,
                        )),
                        None,
                    ),
                    (
                        DualStackListenerIpAddr::BothStacks(identifier),
                        DualStackRemoteIp::OtherStack(remote_ip),
                    ) => (
                        Operation::SendToOtherStack((
                            SendOneshotParameters {
                                local_ip: None,
                                local_id: *identifier,
                                remote_ip,
                                remote_id: remote_identifier,
                                device: &device,
                                send_options: ds.to_other_send_options(&ip_options),
                            },
                            ds,
                        )),
                        None,
                    ),
                },
                BoundSocketStateType::Connected { state, sharing: _ } => {
                    match (ds.converter().convert(state), remote_ip) {
                        (DualStackConnState::ThisStack(_), DualStackRemoteIp::OtherStack(_)) => {
                            return Err(Either::Right(SendToError::RemoteUnexpectedlyMapped))
                        }
                        (DualStackConnState::OtherStack(_), DualStackRemoteIp::ThisStack(_)) => {
                            return Err(Either::Right(SendToError::RemoteUnexpectedlyNonMapped))
                        }
                        (
                            DualStackConnState::ThisStack(ConnState {
                                socket: _,
                                ip_options,
                                clear_device_on_disconnect: _,
                                shutdown,
                                addr:
                                    ConnAddr {
                                        ip: ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                        device,
                                    },
                                extra: _,
                            }),
                            DualStackRemoteIp::ThisStack(remote_ip),
                        ) => (
                            Operation::SendToThisStack((
                                SendOneshotParameters {
                                    local_ip: Some(*local_ip),
                                    local_id: *local_id,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device: &device,
                                    send_options: &ip_options.hop_limits,
                                },
                                core_ctx,
                            )),
                            Some(shutdown),
                        ),
                        (
                            DualStackConnState::OtherStack(ConnState {
                                socket: _,
                                ip_options,
                                clear_device_on_disconnect: _,
                                shutdown,
                                addr:
                                    ConnAddr {
                                        ip: ConnIpAddr { local: (local_ip, local_id), remote: _ },
                                        device,
                                    },
                                extra: _,
                            }),
                            DualStackRemoteIp::OtherStack(remote_ip),
                        ) => (
                            Operation::SendToOtherStack((
                                SendOneshotParameters {
                                    local_ip: Some(*local_ip),
                                    local_id: *local_id,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device: &device,
                                    send_options: ds.to_other_send_options(&ip_options),
                                },
                                ds,
                            )),
                            Some(shutdown),
                        ),
                    }
                }
            },
        };

        if let Some(Shutdown { send: shutdown_write, receive: _ }) = shutdown {
            if *shutdown_write {
                return Err(Either::Right(SendToError::NotWriteable));
            }
        }

        match operation {
            Operation::SendToThisStack((params, core_ctx)) => {
                DatagramBoundStateContext::with_transport_context(core_ctx, |core_ctx| {
                    send_oneshot::<_, S, _, _, _, _>(core_ctx, bindings_ctx, params, body)
                })
            }
            Operation::SendToOtherStack((params, core_ctx)) => {
                DualStackDatagramBoundStateContext::with_transport_context::<_, _>(
                    core_ctx,
                    |core_ctx| {
                        send_oneshot::<_, S, _, _, _, _>(core_ctx, bindings_ctx, params, body)
                    },
                )
            }
            Operation::_Phantom((never, _)) => match never {},
        }
        .map_err(Either::Right)
    })
}

struct SendOneshotParameters<'a, I: IpExt, S: DatagramSocketSpec, D: WeakId, O: SendOptions<I>> {
    local_ip: Option<SocketIpAddr<I::Addr>>,
    local_id: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    remote_ip: ZonedAddr<SocketIpAddr<I::Addr>, D::Strong>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: &'a Option<D>,
    send_options: &'a O,
}

fn send_oneshot<
    I: IpExt,
    S: DatagramSocketSpec,
    CC: IpSocketHandler<I, BC>,
    BC,
    B: BufferMut,
    O: SendOptions<I>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    SendOneshotParameters {
        local_ip,
        local_id,
        remote_ip,
        remote_id,
        device,
        send_options,
    }: SendOneshotParameters<'_, I, S, CC::WeakDeviceId, O>,
    body: B,
) -> Result<(), SendToError<S::SerializeError>> {
    let (remote_ip, device) = match crate::transport::resolve_addr_with_device::<I::Addr, _, _, _>(
        remote_ip,
        device.clone(),
    ) {
        Ok(addr) => addr,
        Err(e) => return Err(SendToError::Zone(e)),
    };

    core_ctx
        .send_oneshot_ip_packet_with_fallible_serializer(
            bindings_ctx,
            device.as_ref().map(|d| d.as_ref()),
            local_ip,
            remote_ip,
            S::ip_proto::<I>(),
            send_options,
            |local_ip| {
                S::make_packet::<I, _>(
                    body,
                    &ConnIpAddr { local: (local_ip, local_id), remote: (remote_ip, remote_id) },
                )
            },
            None,
        )
        .map_err(|err| match err {
            SendOneShotIpPacketError::CreateAndSendError { err, options: _ } => {
                SendToError::CreateAndSend(err)
            }
            SendOneShotIpPacketError::SerializeError(err) => SendToError::SerializeError(err),
        })
}

/// Mutably holds the original state of a bound socket required to update the
/// bound device.
enum SetBoundDeviceParameters<'a, WireI: IpExt, SocketI: IpExt, D: WeakId, S: DatagramSocketSpec> {
    Listener {
        ip: &'a ListenerIpAddr<WireI::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        device: &'a mut Option<D>,
    },
    Connected(&'a mut ConnState<WireI, SocketI, D, S>),
}

/// Update the device for a bound socket.
///
/// The update is applied both to the socket's entry in the given
/// [`BoundSocketMap`], and the mutable socket state in the given
/// [`SetBoundDeviceParameters`].
///
/// # Panics
///
/// Panics if the given `socket_id` is not present in the given `sockets` map.
fn set_bound_device_single_stack<
    'a,
    WireI: IpExt,
    SocketI: IpExt,
    D: WeakId,
    S: DatagramSocketSpec,
    BC,
    CC: IpSocketHandler<WireI, BC, WeakDeviceId = D, DeviceId = D::Strong>,
>(
    bindings_ctx: &mut BC,
    core_ctx: &mut CC,
    params: SetBoundDeviceParameters<'a, WireI, SocketI, D, S>,
    sockets: &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
    socket_id: &<
            S::SocketMapSpec<WireI, D> as DatagramSocketMapSpec<WireI, D, S::AddrSpec>
        >::BoundSocketId,
    new_device: Option<&D::Strong>,
) -> Result<(), SocketError> {
    let (local_ip, remote_ip, old_device) = match &params {
        SetBoundDeviceParameters::Listener {
            ip: ListenerIpAddr { addr, identifier: _ },
            device,
        } => (addr.as_ref(), None, device.as_ref()),
        SetBoundDeviceParameters::Connected(ConnState {
            socket: _,
            ip_options: _,
            addr:
                ConnAddr {
                    ip: ConnIpAddr { local: (local_ip, _local_id), remote: (remote_ip, _remote_id) },
                    device,
                },
            shutdown: _,
            clear_device_on_disconnect: _,
            extra: _,
        }) => (Some(local_ip), Some(remote_ip), device.as_ref()),
    };
    // Don't allow changing the device if one of the IP addresses in the
    // socket address vector requires a zone (scope ID).
    if !socket::can_device_change(
        local_ip.map(AsRef::<SpecifiedAddr<WireI::Addr>>::as_ref),
        remote_ip.map(AsRef::<SpecifiedAddr<WireI::Addr>>::as_ref),
        old_device,
        new_device,
    ) {
        return Err(SocketError::Local(LocalAddressError::Zone(
            ZonedAddressError::DeviceZoneMismatch,
        )));
    }

    match params {
        SetBoundDeviceParameters::Listener { ip, device } => {
            let new_device = new_device.map(|d| core_ctx.downgrade_device_id(d));
            let old_addr = ListenerAddr { ip: ip.clone(), device: device.clone() };
            let new_addr = ListenerAddr { ip: ip.clone(), device: new_device.clone() };
            let entry = sockets
                .listeners_mut()
                .entry(socket_id, &old_addr)
                .unwrap_or_else(|| panic!("invalid listener ID {:?}", socket_id));
            let _entry = entry
                .try_update_addr(new_addr)
                .map_err(|(ExistsError {}, _entry)| LocalAddressError::AddressInUse)?;
            *device = new_device
        }
        SetBoundDeviceParameters::Connected(ConnState {
            socket,
            ip_options: _,
            addr,
            shutdown: _,
            clear_device_on_disconnect,
            extra: _,
        }) => {
            let ConnAddr { ip, device } = addr;
            let ConnIpAddr { local: (local_ip, _local_id), remote: (remote_ip, _remote_id) } = ip;
            let mut new_socket = core_ctx
                .new_ip_socket(
                    bindings_ctx,
                    new_device.map(EitherDeviceId::Strong),
                    Some(local_ip.clone()),
                    remote_ip.clone(),
                    socket.proto(),
                    // NB: Use default options; They'll be replaced later if
                    // the insertion is successful.
                    Default::default(),
                )
                .map_err(|_: (IpSockCreationError, _)| {
                    SocketError::Remote(RemoteAddressError::NoRoute)
                })?;
            let new_device = new_socket.device().cloned();
            let old_addr = ConnAddr { ip: ip.clone(), device: device.clone() };
            let entry = sockets
                .conns_mut()
                .entry(socket_id, &old_addr)
                .unwrap_or_else(|| panic!("invalid conn id {:?}", socket_id));
            let new_addr = ConnAddr { ip: ip.clone(), device: new_device.clone() };
            let entry = entry
                .try_update_addr(new_addr)
                .map_err(|(ExistsError {}, _entry)| LocalAddressError::AddressInUse)?;
            // Since the move was successful, replace the old socket with
            // the new one but move the options over.
            let _: SocketHopLimits<WireI> = new_socket.replace_options(socket.take_options());
            *socket = new_socket;
            // If this operation explicitly sets the device for the socket, it
            // should no longer be cleared on disconnect.
            if new_device.is_some() {
                *clear_device_on_disconnect = false;
            }
            *addr = entry.get_addr().clone()
        }
    }
    Ok(())
}

/// Update the device for a listener socket in both stacks.
///
/// Either the update is applied successfully to both stacks, or (in the case of
/// an error) both stacks are left in their original state.
///
/// # Panics
///
/// Panics if the given socket IDs are not present in the given socket maps.
fn set_bound_device_listener_both_stacks<'a, I: IpExt, D: WeakId, S: DatagramSocketSpec>(
    old_device: &mut Option<D>,
    local_id: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    PairedSocketMapMut { bound: sockets, other_bound: other_sockets }: PairedSocketMapMut<
        'a,
        I,
        D,
        S,
    >,
    PairedBoundSocketIds { this: socket_id, other: other_socket_id }: PairedBoundSocketIds<I, D, S>,
    new_device: Option<D>,
) -> Result<(), SocketError> {
    fn try_update_entry<I: IpExt, D: WeakId, S: DatagramSocketSpec>(
        old_device: Option<D>,
        new_device: Option<D>,
        sockets: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
        socket_id: &<
                S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>
            >::BoundSocketId,
        local_id: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    ) -> Result<(), SocketError> {
        let old_addr = ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: local_id },
            device: old_device,
        };
        let entry = sockets
            .listeners_mut()
            .entry(socket_id, &old_addr)
            .unwrap_or_else(|| panic!("invalid listener ID {:?}", socket_id));
        let new_addr = ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: local_id },
            device: new_device,
        };
        let _entry = entry
            .try_update_addr(new_addr)
            .map_err(|(ExistsError {}, _entry)| LocalAddressError::AddressInUse)?;
        return Ok(());
    }

    // Try to update the entry in this stack.
    try_update_entry::<_, _, S>(
        old_device.clone(),
        new_device.clone(),
        sockets,
        &socket_id,
        local_id,
    )?;

    // Try to update the entry in the other stack.
    let result = try_update_entry::<_, _, S>(
        old_device.clone(),
        new_device.clone(),
        other_sockets,
        &other_socket_id,
        local_id,
    );

    if let Err(e) = result {
        // This stack was successfully updated, but the other stack failed to
        // update; rollback this stack to the original device. This shouldn't be
        // fallible, because both socket maps are locked.
        try_update_entry::<_, _, S>(new_device, old_device.clone(), sockets, &socket_id, local_id)
            .expect("failed to rollback listener in this stack to it's original device");
        return Err(e);
    }
    *old_device = new_device;
    return Ok(());
}

pub(crate) fn set_device<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    new_device: Option<&CC::DeviceId>,
) -> Result<(), SocketError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { ref mut device, sharing: _, ip_options: _ } = state;
                *device = new_device.map(|d| core_ctx.downgrade_device_id(d));
                Ok(())
            }
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                // Information about the set-device operation for the given
                // socket.
                enum Operation<'a, I: IpExt, D: WeakId, S: DatagramSocketSpec, CC, DualStackSC> {
                    ThisStack {
                        params: SetBoundDeviceParameters<'a, I, I, D, S>,
                        core_ctx: CC,
                    },
                    OtherStack {
                        params: SetBoundDeviceParameters<'a, I::OtherVersion, I, D, S>,
                        core_ctx: DualStackSC,
                    },
                    ListenerBothStacks {
                        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                        device: &'a mut Option<D>,
                        core_ctx: DualStackSC,
                    },
                }

                // Determine which operation needs to be applied.
                let op = match core_ctx.dual_stack_context() {
                    MaybeDualStack::DualStack(ds) => match socket_type {
                        BoundSocketStateType::Listener {
                            state:
                                ListenerState { ip_options: _, addr: ListenerAddr { ip, device } },
                            sharing: _,
                        } => match ds.converter().convert(ip) {
                            DualStackListenerIpAddr::ThisStack(ip) => Operation::ThisStack {
                                params: SetBoundDeviceParameters::Listener { ip, device },
                                core_ctx,
                            },
                            DualStackListenerIpAddr::OtherStack(ip) => Operation::OtherStack {
                                params: SetBoundDeviceParameters::Listener { ip, device },
                                core_ctx: ds,
                            },
                            DualStackListenerIpAddr::BothStacks(identifier) => {
                                Operation::ListenerBothStacks {
                                    identifier: *identifier,
                                    device,
                                    core_ctx: ds,
                                }
                            }
                        },
                        BoundSocketStateType::Connected { state, sharing: _ } => {
                            match ds.converter().convert(state) {
                                DualStackConnState::ThisStack(state) => Operation::ThisStack {
                                    params: SetBoundDeviceParameters::Connected(state),
                                    core_ctx,
                                },
                                DualStackConnState::OtherStack(state) => Operation::OtherStack {
                                    params: SetBoundDeviceParameters::Connected(state),
                                    core_ctx: ds,
                                },
                            }
                        }
                    },
                    MaybeDualStack::NotDualStack(nds) => match socket_type {
                        BoundSocketStateType::Listener {
                            state:
                                ListenerState { ip_options: _, addr: ListenerAddr { ip, device } },
                            sharing: _,
                        } => Operation::ThisStack {
                            params: SetBoundDeviceParameters::Listener {
                                ip: nds.converter().convert(ip),
                                device,
                            },
                            core_ctx,
                        },
                        BoundSocketStateType::Connected { state, sharing: _ } => {
                            Operation::ThisStack {
                                params: SetBoundDeviceParameters::Connected(
                                    nds.converter().convert(state),
                                ),
                                core_ctx,
                            }
                        }
                    },
                };

                // Apply the operation
                match op {
                    Operation::ThisStack { params, core_ctx } => {
                        let socket_id = S::make_bound_socket_map_id(id);
                        DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                            core_ctx,
                            |core_ctx, bound, _allocator| {
                                set_bound_device_single_stack(
                                    bindings_ctx,
                                    core_ctx,
                                    params,
                                    bound,
                                    &socket_id,
                                    new_device,
                                )
                            },
                        )
                    }
                    Operation::OtherStack { params, core_ctx } => {
                        let socket_id = core_ctx.to_other_bound_socket_id(id);
                        core_ctx.with_other_bound_sockets_mut(|core_ctx, bound| {
                            set_bound_device_single_stack(
                                bindings_ctx,
                                core_ctx,
                                params,
                                bound,
                                &socket_id,
                                new_device,
                            )
                        })
                    }
                    Operation::ListenerBothStacks { identifier, device, core_ctx } => {
                        let socket_id = PairedBoundSocketIds::<_, _, S> {
                            this: S::make_bound_socket_map_id(id.clone()),
                            other: core_ctx.to_other_bound_socket_id(id),
                        };
                        core_ctx.with_both_bound_sockets_mut(
                            |core_ctx, bound, other_bound, _allocator, _other_allocator| {
                                set_bound_device_listener_both_stacks(
                                    device,
                                    identifier,
                                    PairedSocketMapMut { bound, other_bound },
                                    socket_id,
                                    new_device.map(|d| core_ctx.downgrade_device_id(d)),
                                )
                            },
                        )
                    }
                }
            }
        }
    })
}

pub(crate) fn get_bound_device<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
) -> Option<CC::WeakDeviceId> {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let (_, device): (&IpOptions<_, _, _>, _) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("missing socket"));
        device.clone()
    })
}

/// Error resulting from attempting to change multicast membership settings for
/// a socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SetMulticastMembershipError {
    /// The provided address does not match the provided device.
    AddressNotAvailable,
    /// The device does not exist.
    DeviceDoesNotExist,
    /// The provided address does not match any address on the host.
    NoDeviceWithAddress,
    /// No device or address was specified and there is no device with a route
    /// to the multicast address.
    NoDeviceAvailable,
    /// Tried to join a group again.
    GroupAlreadyJoined,
    /// Tried to leave an unjoined group.
    GroupNotJoined,
    /// The socket is bound to a device that doesn't match the one specified.
    WrongDevice,
}

/// Selects the interface for the given remote address, optionally with a
/// constraint on the source address.
fn pick_interface_for_addr<
    A: IpAddress,
    S: DatagramSocketSpec,
    BC: DatagramStateBindingsContext<A::Version, S>,
    CC: DatagramBoundStateContext<A::Version, BC, S>,
>(
    core_ctx: &mut CC,
    _remote_addr: MulticastAddr<A>,
    source_addr: Option<SpecifiedAddr<A>>,
) -> Result<CC::DeviceId, SetMulticastMembershipError>
where
    A::Version: IpExt,
{
    core_ctx.with_transport_context(|core_ctx| {
        if let Some(source_addr) = source_addr {
            let mut devices = TransportIpContext::<A::Version, _>::get_devices_with_assigned_addr(
                core_ctx,
                source_addr,
            );
            if let Some(d) = devices.next() {
                if devices.next() == None {
                    return Ok(d);
                }
            }
        }
        log_unimplemented!((), "https://fxbug.dev/39479: Implement this by looking up a route");
        Err(SetMulticastMembershipError::NoDeviceAvailable)
    })
}

/// Selector for the device to affect when changing multicast settings.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub enum MulticastInterfaceSelector<A: IpAddress, D> {
    /// Use the device with the assigned address.
    LocalAddress(SpecifiedAddr<A>),
    /// Use the device with the specified identifier.
    Interface(D),
}

/// Selector for the device to use when changing multicast membership settings.
///
/// This is like `Option<MulticastInterfaceSelector` except it specifies the
/// semantics of the `None` value as "pick any device".
#[derive(Copy, Clone, Debug, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip(A, IpAddress)]
pub enum MulticastMembershipInterfaceSelector<A: IpAddress, D> {
    /// Use the specified interface.
    Specified(MulticastInterfaceSelector<A, D>),
    /// Pick any device with a route to the multicast target address.
    AnyInterfaceWithRoute,
}

impl<A: IpAddress, D> From<MulticastInterfaceSelector<A, D>>
    for MulticastMembershipInterfaceSelector<A, D>
{
    fn from(selector: MulticastInterfaceSelector<A, D>) -> Self {
        Self::Specified(selector)
    }
}

/// Sets the specified socket's membership status for the given group.
///
/// If `id` is unbound, the membership state will take effect when it is bound.
/// An error is returned if the membership change request is invalid (e.g.
/// leaving a group that was not joined, or joining a group multiple times) or
/// if the device to use to join is unspecified or conflicts with the existing
/// socket state.
pub(crate) fn set_multicast_membership<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, CC::DeviceId>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let (_, bound_device): (&IpOptions<_, _, _>, _) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("socket not found"));

        let interface = match interface {
            MulticastMembershipInterfaceSelector::Specified(selector) => match selector {
                MulticastInterfaceSelector::Interface(device) => {
                    if bound_device.as_ref().map_or(false, |d| d != &device) {
                        return Err(SetMulticastMembershipError::WrongDevice);
                    } else {
                        EitherDeviceId::Strong(device)
                    }
                }
                MulticastInterfaceSelector::LocalAddress(addr) => EitherDeviceId::Strong(
                    pick_interface_for_addr(core_ctx, multicast_group, Some(addr))?,
                ),
            },
            MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute => {
                if let Some(bound_device) = bound_device.as_ref() {
                    EitherDeviceId::Weak(bound_device.clone())
                } else {
                    EitherDeviceId::Strong(pick_interface_for_addr(
                        core_ctx,
                        multicast_group,
                        None,
                    )?)
                }
            }
        };

        let ip_options = get_options_mut(core_ctx, state, id);

        let Some(strong_interface) = interface.as_strong(core_ctx) else {
            return Err(SetMulticastMembershipError::DeviceDoesNotExist);
        };

        let change = ip_options
            .multicast_memberships
            .apply_membership_change(multicast_group, &interface.as_weak(core_ctx), want_membership)
            .ok_or(if want_membership {
                SetMulticastMembershipError::GroupAlreadyJoined
            } else {
                SetMulticastMembershipError::GroupNotJoined
            })?;

        DatagramBoundStateContext::<I, _, _>::with_transport_context(core_ctx, |core_ctx| {
            match change {
                MulticastMembershipChange::Join => {
                    MulticastMembershipHandler::<I, _>::join_multicast_group(
                        core_ctx,
                        bindings_ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
                MulticastMembershipChange::Leave => {
                    MulticastMembershipHandler::<I, _>::leave_multicast_group(
                        core_ctx,
                        bindings_ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
            }
        });

        Ok(())
    })
}

fn get_options_device_from_conn_state<
    WireI: IpExt,
    SocketI: IpExt,
    D: Eq + Hash,
    S: DatagramSocketSpec,
>(
    ConnState {
        socket: _,
        ip_options,
        clear_device_on_disconnect: _,
        shutdown: _,
        addr: ConnAddr { device, ip: _ },
        extra: _,
    }: &ConnState<WireI, SocketI, D, S>,
) -> (&IpOptions<SocketI, D, S>, &Option<D>) {
    (ip_options, device)
}

pub(crate) fn get_options_device<
    'a,
    I: IpExt,
    S: DatagramSocketSpec,
    BC,
    CC: DatagramBoundStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    state: &'a SocketState<I, CC::WeakDeviceId, S>,
) -> (&'a IpOptions<I, CC::WeakDeviceId, S>, &'a Option<CC::WeakDeviceId>) {
    match state {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device, sharing: _ } = state;
            (ip_options, device)
        }
        SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
            match socket_type {
                BoundSocketStateType::Listener { state, sharing: _ } => {
                    let ListenerState { ip_options, addr: ListenerAddr { device, ip: _ } } = state;
                    (ip_options, device)
                }
                BoundSocketStateType::Connected { state, sharing: _ } => {
                    match core_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(state) {
                                DualStackConnState::ThisStack(state) => {
                                    get_options_device_from_conn_state(state)
                                }
                                DualStackConnState::OtherStack(state) => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    get_options_device_from_conn_state(state)
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(not_dual_stack) => {
                            get_options_device_from_conn_state(
                                not_dual_stack.converter().convert(state),
                            )
                        }
                    }
                }
            }
        }
    }
}

fn get_options_mut<
    'a,
    I: IpExt,
    S: DatagramSocketSpec,
    BC,
    CC: DatagramBoundStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    state: &'a mut SocketsState<I, CC::WeakDeviceId, S>,
    id: S::SocketId<I>,
) -> &'a mut IpOptions<I, CC::WeakDeviceId, S>
where
    S::SocketId<I>: EntryKey,
{
    match state.get_mut(id.get_key_index()).expect("socket not found") {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device: _, sharing: _ } = state;
            ip_options
        }
        SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
            match socket_type {
                BoundSocketStateType::Listener { state, sharing: _ } => {
                    let ListenerState { ip_options, addr: _ } = state;
                    ip_options
                }
                BoundSocketStateType::Connected { state, sharing: _ } => {
                    match core_ctx.dual_stack_context() {
                        MaybeDualStack::DualStack(dual_stack) => {
                            match dual_stack.converter().convert(state) {
                                DualStackConnState::ThisStack(state) => state.as_mut(),
                                DualStackConnState::OtherStack(state) => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    state.as_mut()
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(not_dual_stack) => {
                            not_dual_stack.converter().convert(state).as_mut()
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn update_ip_hop_limit<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    update: impl FnOnce(&mut SocketHopLimits<I>),
) {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let options = get_options_mut(core_ctx, state, id);

        update(&mut options.hop_limits)
    })
}

pub(crate) fn get_ip_hop_limits<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
) -> HopLimits {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let (options, device) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("socket not found"));
        let device = device.as_ref().and_then(|d| core_ctx.upgrade_weak_device_id(d));
        DatagramBoundStateContext::<I, _, _>::with_transport_context(core_ctx, |core_ctx| {
            options.hop_limits.get_limits_with_defaults(
                &TransportIpContext::<I, _>::get_default_hop_limits(core_ctx, device.as_ref()),
            )
        })
    })
}

/// Calls the callback with mutable access to [`S::OtherStackIpOptions<I>`].
///
/// If the socket is bound, the callback is not called, and instead an
/// `ExpectedUnboundError` is returned.
pub(crate) fn with_other_stack_ip_options_mut_if_unbound<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    cb: impl FnOnce(&mut S::OtherStackIpOptions<I>) -> R,
) -> Result<R, ExpectedUnboundError> {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let is_unbound = match state.get(id.get_key_index()).expect("socket not found") {
            SocketState::Unbound(_) => true,
            SocketState::Bound(_) => false,
        };
        if is_unbound {
            let options = get_options_mut(core_ctx, state, id);
            Ok(cb(&mut options.other_stack))
        } else {
            Err(ExpectedUnboundError)
        }
    })
}

/// Calls the callback with mutable access to [`S::OtherStackIpOptions<I>`].
pub(crate) fn with_other_stack_ip_options_mut<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: S::SocketId<I>,
    cb: impl FnOnce(&mut S::OtherStackIpOptions<I>) -> R,
) -> R {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        let options = get_options_mut(core_ctx, state, id);
        cb(&mut options.other_stack)
    })
}

/// Calls the callback with access to [`S::OtherStackIpOptions<I>`].
pub(crate) fn with_other_stack_ip_options<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
    cb: impl FnOnce(&S::OtherStackIpOptions<I>) -> R,
) -> R {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let (options, _device) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("not found"));

        cb(&options.other_stack)
    })
}

/// Calls the callback with access to [`S::OtherStackIpOptions<I>`], and the
/// default [`HopLimits`] for `I::OtherVersion`.
///
/// If dualstack operations are not supported, the callback is not called, and
/// instead `NotDualStackCapableError` is returned.
pub(crate) fn with_other_stack_ip_options_and_default_hop_limits<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: S::SocketId<I>,
    cb: impl FnOnce(&S::OtherStackIpOptions<I>, HopLimits) -> R,
) -> Result<R, NotDualStackCapableError> {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let (options, device) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("not found"));
        let device = device.as_ref().and_then(|d| core_ctx.upgrade_weak_device_id(d));
        match DatagramBoundStateContext::<I, _, _>::dual_stack_context(core_ctx) {
            MaybeDualStack::NotDualStack(_) => Err(NotDualStackCapableError),
            MaybeDualStack::DualStack(ds) => {
                let default_hop_limits =
                    DualStackDatagramBoundStateContext::<I, _, _>::with_transport_context(
                        ds,
                        |sync_ctx| {
                            TransportIpContext::<I, _>::get_default_hop_limits(
                                sync_ctx,
                                device.as_ref(),
                            )
                        },
                    );
                Ok(cb(&options.other_stack, default_hop_limits))
            }
        }
    })
}

pub(crate) fn update_sharing<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec<SharingState = Sharing>,
    Sharing: Clone,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
    new_sharing: Sharing,
) -> Result<(), ExpectedUnboundError> {
    core_ctx.with_sockets_state_mut(|_core_ctx, state| {
        let state = match state.get_mut(id.get_key_index()).expect("socket not found") {
            SocketState::Bound(_) => return Err(ExpectedUnboundError),
            SocketState::Unbound(state) => state,
        };

        let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
        *sharing = new_sharing;
        Ok(())
    })
}

pub(crate) fn get_sharing<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec<SharingState = Sharing>,
    Sharing: Clone,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
) -> Sharing {
    core_ctx.with_sockets_state(|_core_ctx, state| {
        match state.get(id.get_key_index()).expect("socket not found") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
                sharing
            }
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state: _, sharing } => sharing,
                    BoundSocketStateType::Connected { state: _, sharing } => sharing,
                }
            }
        }
        .clone()
    })
}

pub(crate) fn set_ip_transparent<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
    value: bool,
) {
    core_ctx.with_sockets_state_mut(|core_ctx, state| {
        get_options_mut(core_ctx, state, id).transparent = value;
    })
}

pub(crate) fn get_ip_transparent<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: S::SocketId<I>,
) -> bool {
    core_ctx.with_sockets_state(|core_ctx, state| {
        let (options, _device) =
            get_options_device(core_ctx, state.get(id.get_key_index()).expect("missing socket"));
        options.transparent
    })
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use alloc::{vec, vec::Vec};
    use net_types::{
        ip::{IpAddr, Ipv4, Ipv6},
        SpecifiedAddr, Witness,
    };

    use crate::{
        context::testutil::FakeCtxWithCoreCtx, device::testutil::FakeStrongDeviceId,
        ip::socket::testutil::FakeDeviceConfig, testutil::TestIpExt,
    };

    // Helper function to ensure the Fake CoreCtx and BindingsCtx are setup with
    // [`FakeDeviceConfig`] (one per provided device), with remote/local IPs
    // that support a connection to the given remote_ip.
    pub(crate) fn setup_fake_ctx_with_dualstack_conn_addrs<
        TimerId,
        Event: Debug,
        BindingsCtxState: Default,
        CC,
        D: FakeStrongDeviceId,
    >(
        local_ip: IpAddr,
        remote_ip: SpecifiedAddr<IpAddr>,
        devices: impl IntoIterator<Item = D>,
        core_ctx_builder: impl FnOnce(Vec<FakeDeviceConfig<D, SpecifiedAddr<IpAddr>>>) -> CC,
    ) -> FakeCtxWithCoreCtx<CC, TimerId, Event, BindingsCtxState> {
        // A conversion helper to unmap ipv4-mapped-ipv6 addresses.
        fn unmap_ip(addr: IpAddr) -> IpAddr {
            match addr {
                IpAddr::V4(v4) => IpAddr::V4(v4),
                IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                    Some(v4) => IpAddr::V4(v4),
                    None => IpAddr::V6(v6),
                },
            }
        }

        // Convert the local/remote IPs into `IpAddr` in their non-mapped form.
        let local_ip = unmap_ip(local_ip);
        let remote_ip = unmap_ip(remote_ip.get());
        // If the given local_ip is unspecified, use the default from
        // `FAKE_CONFIG`. This ensures we always instantiate the
        // FakeDeviceConfig below with at least one local_ip, which is
        // required for connect operations to succeed.
        let local_ip = SpecifiedAddr::new(local_ip).unwrap_or_else(|| match remote_ip {
            IpAddr::V4(_) => Ipv4::FAKE_CONFIG.local_ip.into(),
            IpAddr::V6(_) => Ipv6::FAKE_CONFIG.local_ip.into(),
        });
        // If the given remote_ip is unspecified, we won't be able to
        // connect; abort the test.
        let remote_ip = SpecifiedAddr::new(remote_ip).expect("remote-ip should be specified");
        FakeCtxWithCoreCtx::with_core_ctx(core_ctx_builder(
            devices
                .into_iter()
                .map(|device| FakeDeviceConfig {
                    device,
                    local_ips: vec![local_ip],
                    remote_ips: vec![remote_ip],
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod test {
    use core::{convert::Infallible as Never, ops::DerefMut as _};

    use alloc::{vec, vec::Vec};
    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::{
        ip::{Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
        Witness,
    };
    use packet::Buf;
    use packet_formats::ip::{Ipv4Proto, Ipv6Proto};
    use test_case::test_case;

    use crate::{
        context::testutil::{FakeBindingsCtx, FakeCtxWithCoreCtx, Wrapped, WrappedFakeCoreCtx},
        data_structures::socketmap::SocketMap,
        device::testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
        ip::{
            device::state::{IpDeviceState, IpDeviceStateIpExt},
            socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx, FakeIpSocketCtx},
            testutil::{DualStackSendIpPacketMeta, FakeIpDeviceIdCtx},
            IpLayerIpExt, DEFAULT_HOP_LIMITS,
        },
        socket::{
            Bound, IncompatibleError, InsertError, ListenerAddrInfo, RemoveResult,
            SocketMapAddrStateSpec,
        },
        testutil::TestIpExt,
        uninstantiable::UninstantiableWrapper,
    };

    use super::*;

    trait DatagramIpExt<D: FakeStrongDeviceId>:
        Ip + IpExt + IpDeviceStateIpExt + TestIpExt + DualStackIpExt + DualStackContextsIpExt<D>
    {
    }
    impl<
            D: FakeStrongDeviceId,
            I: Ip
                + IpExt
                + IpDeviceStateIpExt
                + TestIpExt
                + DualStackIpExt
                + DualStackContextsIpExt<D>,
        > DatagramIpExt<D> for I
    {
    }

    #[derive(Debug)]
    enum FakeAddrSpec {}

    impl SocketMapAddrSpec for FakeAddrSpec {
        type LocalIdentifier = u8;
        type RemoteIdentifier = char;
    }

    #[derive(Debug)]
    enum FakeStateSpec {}

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Tag;

    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]

    enum Sharing {
        #[default]
        NoConflicts,
        // Any attempt to insert a connection with the following remote port
        // will conflict.
        ConnectionConflicts {
            remote_port: char,
        },
    }

    #[derive(Copy, Clone, Debug, Derivative)]
    #[derivative(Eq(bound = ""), PartialEq(bound = ""))]
    struct Id(usize);

    impl From<usize> for Id {
        fn from(u: usize) -> Self {
            Self(u)
        }
    }

    impl EntryKey for Id {
        fn get_key_index(&self) -> usize {
            let Self(u) = self;
            *u
        }
    }

    impl<I: Ip, D: crate::device::Id> SocketMapStateSpec for (FakeStateSpec, I, D) {
        type AddrVecTag = Tag;
        type ConnAddrState = Id;
        type ConnId = Id;
        type ConnSharingState = Sharing;
        type ListenerAddrState = Id;
        type ListenerId = Id;
        type ListenerSharingState = Sharing;
        fn listener_tag(_: ListenerAddrInfo, _state: &Self::ListenerAddrState) -> Self::AddrVecTag {
            Tag
        }
        fn connected_tag(_has_device: bool, _state: &Self::ConnAddrState) -> Self::AddrVecTag {
            Tag
        }
    }

    const FAKE_DATAGRAM_IPV4_PROTOCOL: Ipv4Proto = Ipv4Proto::Other(253);
    const FAKE_DATAGRAM_IPV6_PROTOCOL: Ipv6Proto = Ipv6Proto::Other(254);

    impl DatagramSocketSpec for FakeStateSpec {
        type AddrSpec = FakeAddrSpec;
        type SocketId<I: IpExt> = Id;
        type OtherStackIpOptions<I: IpExt> = SocketHopLimits<I::OtherVersion>;
        type SocketMapSpec<I: IpExt, D: device::WeakId> = (Self, I, D);
        type SharingState = Sharing;
        type ListenerIpAddr<I: IpExt> = I::DualStackListenerIpAddr<u8>;
        type ConnIpAddr<I: IpExt> = I::DualStackConnIpAddr<Self>;
        type ConnStateExtra = ();
        type ConnState<I: IpExt, D: Debug + Eq + Hash> = I::DualStackConnState<D, Self>;

        fn ip_proto<I: IpProtoExt>() -> I::Proto {
            I::map_ip((), |()| FAKE_DATAGRAM_IPV4_PROTOCOL, |()| FAKE_DATAGRAM_IPV6_PROTOCOL)
        }

        fn make_bound_socket_map_id<I: IpExt, D: device::WeakId>(
            s: Self::SocketId<I>,
        ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId
        {
            s
        }

        type Serializer<I: IpExt, B: BufferMut> = B;
        type SerializeError = Never;
        fn make_packet<I: IpExt, B: BufferMut>(
            body: B,
            _addr: &ConnIpAddr<
                I::Addr,
                <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Result<Self::Serializer<I, B>, Never> {
            Ok(body)
        }
        fn try_alloc_listen_identifier<I: Ip, D: device::WeakId>(
            _bindings_ctx: &mut impl RngContext,
            is_available: impl Fn(u8) -> Result<(), InUseError>,
        ) -> Option<u8> {
            (0..=u8::MAX).find(|i| is_available(*i).is_ok())
        }

        fn conn_addr_from_state<I: IpExt, D: Clone + Debug + Eq + Hash>(
            state: &Self::ConnState<I, D>,
        ) -> ConnAddr<Self::ConnIpAddr<I>, D> {
            I::conn_addr_from_state(state)
        }
    }

    impl<I: IpExt, D: device::WeakId> DatagramSocketMapSpec<I, D, FakeAddrSpec>
        for (FakeStateSpec, I, D)
    {
        type BoundSocketId = Id;
    }

    impl<I: IpExt, D: device::WeakId>
        SocketMapConflictPolicy<
            ConnAddr<ConnIpAddr<I::Addr, u8, char>, D>,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for (FakeStateSpec, I, D)
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            addr: &ConnAddr<ConnIpAddr<I::Addr, u8, char>, D>,
            _socketmap: &SocketMap<AddrVec<I, D, FakeAddrSpec>, Bound<Self>>,
        ) -> Result<(), InsertError> {
            let ConnAddr { ip: ConnIpAddr { local: _, remote: (_remote_ip, port) }, device: _ } =
                addr;
            match sharing {
                Sharing::NoConflicts => Ok(()),
                Sharing::ConnectionConflicts { remote_port } => {
                    if remote_port == port {
                        Err(InsertError::Exists)
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }

    impl<I: IpExt, D: device::WeakId>
        SocketMapConflictPolicy<
            ListenerAddr<ListenerIpAddr<I::Addr, u8>, D>,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for (FakeStateSpec, I, D)
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            _addr: &ListenerAddr<ListenerIpAddr<I::Addr, u8>, D>,
            _socketmap: &SocketMap<AddrVec<I, D, FakeAddrSpec>, Bound<Self>>,
        ) -> Result<(), InsertError> {
            match sharing {
                Sharing::NoConflicts => Ok(()),
                // Since this implementation is strictly for ListenerAddr,
                // ignore connection conflicts.
                Sharing::ConnectionConflicts { remote_port: _ } => Ok(()),
            }
        }
    }

    impl SocketMapAddrStateSpec for Id {
        type Id = Self;
        type SharingState = Sharing;
        type Inserter<'a> = Never where Self: 'a;

        fn new(_sharing: &Self::SharingState, id: Self) -> Self {
            id
        }
        fn contains_id(&self, id: &Self::Id) -> bool {
            self == id
        }
        fn try_get_inserter<'a, 'b>(
            &'b mut self,
            _new_sharing_state: &'a Self::SharingState,
        ) -> Result<Self::Inserter<'b>, IncompatibleError> {
            Err(IncompatibleError)
        }
        fn could_insert(
            &self,
            _new_sharing_state: &Self::SharingState,
        ) -> Result<(), IncompatibleError> {
            Err(IncompatibleError)
        }
        fn remove_by_id(&mut self, _id: Self::Id) -> RemoveResult {
            RemoveResult::IsLast
        }
    }

    #[derive(Derivative, GenericOverIp)]
    #[derivative(Default(bound = ""))]
    #[generic_over_ip()]
    struct FakeBoundSockets<D: FakeStrongDeviceId> {
        v4: BoundSockets<
            Ipv4,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, Ipv4, FakeWeakDeviceId<D>),
        >,
        v6: BoundSockets<
            Ipv6,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, Ipv6, FakeWeakDeviceId<D>),
        >,
    }

    impl<D: FakeStrongDeviceId, I: IpExt>
        AsRef<
            BoundSockets<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
        > for FakeBoundSockets<D>
    {
        fn as_ref(
            &self,
        ) -> &BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    (FakeStateSpec, I, FakeWeakDeviceId<D>),
                >,
            );
            let Wrap(state) = I::map_ip(self, |state| Wrap(&state.v4), |state| Wrap(&state.v6));
            state
        }
    }

    impl<D: FakeStrongDeviceId, I: IpExt>
        AsMut<
            BoundSockets<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
        > for FakeBoundSockets<D>
    {
        fn as_mut(
            &mut self,
        ) -> &mut BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a mut BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    (FakeStateSpec, I, FakeWeakDeviceId<D>),
                >,
            );
            let Wrap(state) =
                I::map_ip(self, |state| Wrap(&mut state.v4), |state| Wrap(&mut state.v6));
            state
        }
    }

    type FakeSocketsState<I, D> = SocketsState<I, FakeWeakDeviceId<D>, FakeStateSpec>;

    type FakeInnerCoreCtx<D> = crate::context::testutil::FakeCoreCtx<
        FakeDualStackIpSocketCtx<D>,
        DualStackSendIpPacketMeta<D>,
        D,
    >;

    type FakeCoreCtx<I, D> =
        Wrapped<FakeSocketsState<I, D>, Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>>;

    impl<I: IpExt, D: FakeStrongDeviceId> FakeCoreCtx<I, D> {
        fn new_with_sockets(state: FakeSocketsState<I, D>, bound: FakeBoundSockets<D>) -> Self {
            Self {
                outer: state,
                inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                    FakeDualStackIpSocketCtx::default(),
                    bound,
                ),
            }
        }
    }

    impl<D: FakeStrongDeviceId, I: DatagramIpExt<D> + IpLayerIpExt>
        DatagramStateContext<I, FakeBindingsCtx<(), (), ()>, FakeStateSpec> for FakeCoreCtx<I, D>
    {
        type SocketsStateCtx<'a> =
            Wrapped<FakeBoundSockets<Self::DeviceId>, FakeInnerCoreCtx<Self::DeviceId>>;

        fn with_sockets_state<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &SocketsState<I, Self::WeakDeviceId, FakeStateSpec>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_sockets_state_mut<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &mut SocketsState<I, Self::WeakDeviceId, FakeStateSpec>,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer)
        }

        fn with_bound_state_context<O, F: FnOnce(&mut Self::SocketsStateCtx<'_>) -> O>(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer: _, inner } = self;
            cb(inner)
        }
    }

    /// A test-only IpExt trait to specialize the `DualStackContext` and
    /// `NonDualStackContext` associated types on the
    /// `DatagramBoundStateContext`.
    ///
    /// This allows us to implement `DatagramBoundStateContext` for all `I`
    /// while also assigning its associated types different values for `Ipv4`
    /// and `Ipv6`.
    trait DualStackContextsIpExt<D: FakeStrongDeviceId>: Ip + DualStackIpExt {
        type DualStackContext: DualStackDatagramBoundStateContext<
            Self,
            FakeBindingsCtx<(), (), ()>,
            FakeStateSpec,
            DeviceId = D,
            WeakDeviceId = FakeWeakDeviceId<D>,
        >;
        type NonDualStackContext: NonDualStackDatagramBoundStateContext<
            Self,
            FakeBindingsCtx<(), (), ()>,
            FakeStateSpec,
            DeviceId = D,
            WeakDeviceId = FakeWeakDeviceId<D>,
        >;
        fn dual_stack_context(
            core_ctx: &mut Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;
    }

    impl<D: FakeStrongDeviceId> DualStackContextsIpExt<D> for Ipv4 {
        type DualStackContext =
            UninstantiableWrapper<Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>>;
        type NonDualStackContext = Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>;
        fn dual_stack_context(
            core_ctx: &mut Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::NotDualStack(core_ctx)
        }
    }

    impl<D: FakeStrongDeviceId> DualStackContextsIpExt<D> for Ipv6 {
        type DualStackContext = Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>;
        type NonDualStackContext =
            UninstantiableWrapper<Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>>;
        fn dual_stack_context(
            core_ctx: &mut Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::DualStack(core_ctx)
        }
    }

    impl<D: FakeStrongDeviceId, I: Ip + IpExt + IpDeviceStateIpExt + DualStackContextsIpExt<D>>
        DatagramBoundStateContext<I, FakeBindingsCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>
    {
        type IpSocketsCtx<'a> = FakeInnerCoreCtx<D>;
        type LocalIdAllocator = ();
        type DualStackContext = I::DualStackContext;
        type NonDualStackContext = I::NonDualStackContext;

        fn with_bound_sockets<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &BoundSockets<
                    I,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, I, Self::WeakDeviceId),
                >,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_ref())
        }
        fn with_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    I,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, I, Self::WeakDeviceId),
                >,
                &mut Self::LocalIdAllocator,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            cb(inner, outer.as_mut(), &mut ())
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
            I::dual_stack_context(self)
        }
    }

    impl<D: FakeStrongDeviceId>
        NonDualStackDatagramBoundStateContext<Ipv4, FakeBindingsCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>
    {
        type Converter = ();
        fn converter(&self) -> Self::Converter {
            ()
        }
    }

    impl<D: FakeStrongDeviceId>
        DualStackDatagramBoundStateContext<Ipv6, FakeBindingsCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerCoreCtx<D>>
    {
        type IpSocketsCtx<'a> = FakeInnerCoreCtx<D>;
        fn dual_stack_enabled(
            &self,
            _state: &impl AsRef<IpOptions<Ipv6, Self::WeakDeviceId, FakeStateSpec>>,
        ) -> bool {
            // For now, it's simplest to have dual-stack unconditionally enabled
            // for datagram tests. However, in the future this could be stateful
            // and follow an implementation similar to UDP's test fixture.
            true
        }

        fn to_other_send_options<'a>(
            &self,
            state: &'a IpOptions<Ipv6, Self::WeakDeviceId, FakeStateSpec>,
        ) -> &'a SocketHopLimits<Ipv4> {
            let IpOptions { multicast_memberships: _, hop_limits: _, other_stack, transparent: _ } =
                state;
            other_stack
        }

        fn assert_dual_stack_enabled(
            &self,
            state: &impl AsRef<IpOptions<Ipv6, Self::WeakDeviceId, FakeStateSpec>>,
        ) {
            assert!(self.dual_stack_enabled(state))
        }
        type Converter = ();
        fn converter(&self) -> Self::Converter {
            ()
        }

        fn from_other_ip_addr(&self, addr: Ipv4Addr) -> Ipv6Addr {
            addr.to_ipv6_mapped().get()
        }

        fn to_other_bound_socket_id(&self, id: Id) -> Id {
            id
        }

        type LocalIdAllocator = ();
        type OtherLocalIdAllocator = ();

        fn with_both_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    Ipv6,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv6, Self::WeakDeviceId),
                >,
                &mut BoundSockets<
                    Ipv4,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv4, Self::WeakDeviceId),
                >,
                &mut Self::LocalIdAllocator,
                &mut Self::OtherLocalIdAllocator,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { ref mut outer, inner } = self;
            let FakeBoundSockets { v4, v6 } = outer;
            cb(inner, v6, v4, &mut (), &mut ())
        }

        fn with_other_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    Ipv4,
                    Self::WeakDeviceId,
                    FakeAddrSpec,
                    (FakeStateSpec, Ipv4, Self::WeakDeviceId),
                >,
            ) -> O,
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
    }

    impl<I: Ip + IpExt + IpDeviceStateIpExt, D: FakeStrongDeviceId>
        LocalIdentifierAllocator<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeBindingsCtx<(), (), ()>,
            (FakeStateSpec, I, FakeWeakDeviceId<D>),
        > for ()
    {
        fn try_alloc_local_id(
            &mut self,
            bound: &BoundSocketMap<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                (FakeStateSpec, I, FakeWeakDeviceId<D>),
            >,
            _bindings_ctx: &mut FakeBindingsCtx<(), (), ()>,
            _flow: DatagramFlowId<I::Addr, <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier>,
        ) -> Option<<FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
            (0..u8::MAX).find_map(|identifier| {
                bound
                    .listeners()
                    .could_insert(
                        &ListenerAddr {
                            device: None,
                            ip: ListenerIpAddr { addr: None, identifier },
                        },
                        &Default::default(),
                    )
                    .is_ok()
                    .then_some(identifier)
            })
        }
    }

    #[ip_test]
    fn set_get_hop_limits<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create(&mut core_ctx);
        const EXPECTED_HOP_LIMITS: HopLimits = HopLimits {
            unicast: const_unwrap_option(NonZeroU8::new(45)),
            multicast: const_unwrap_option(NonZeroU8::new(23)),
        };

        update_ip_hop_limit(&mut core_ctx, &mut bindings_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(EXPECTED_HOP_LIMITS.unicast),
                multicast: Some(EXPECTED_HOP_LIMITS.multicast),
                version: IpVersionMarker::default(),
            }
        });

        assert_eq!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            EXPECTED_HOP_LIMITS
        );
    }

    #[ip_test]
    fn set_get_device_hop_limits<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                }]),
                Default::default(),
            ),
        };
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create(&mut core_ctx);
        set_device(&mut core_ctx, &mut bindings_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();

        let HopLimits { mut unicast, multicast } = DEFAULT_HOP_LIMITS;
        unicast = unicast.checked_add(1).unwrap();
        {
            let ip_socket_ctx = core_ctx.inner.inner.get_ref();
            let device_state: &IpDeviceState<_, I> =
                ip_socket_ctx.get_device_state(&FakeDeviceId).as_ref();
            let mut default_hop_limit = device_state.default_hop_limit.write();
            let default_hop_limit = default_hop_limit.deref_mut();
            assert_ne!(*default_hop_limit, unicast);
            *default_hop_limit = unicast;
        }
        assert_eq!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            HopLimits { unicast, multicast }
        );

        // If the device is removed, use default hop limits.
        AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut core_ctx.inner.inner.get_mut())
            .set_device_removed(FakeDeviceId, true);
        assert_eq!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn default_hop_limits<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create(&mut core_ctx);
        assert_eq!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        update_ip_hop_limit(&mut core_ctx, &mut bindings_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                multicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                version: IpVersionMarker::default(),
            }
        });

        // The limits no longer match the default.
        assert_ne!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        // Clear the hop limits set on the socket.
        update_ip_hop_limit(&mut core_ctx, &mut bindings_ctx, unbound.clone(), |limits| {
            *limits = Default::default()
        });

        // The values should be back at the defaults.
        assert_eq!(
            get_ip_hop_limits(&mut core_ctx, &bindings_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn bind_device_unbound<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx =
            FakeCoreCtx::<I, _>::new_with_sockets(Default::default(), Default::default());
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create(&mut core_ctx);

        set_device(&mut core_ctx, &mut bindings_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();
        assert_eq!(
            get_bound_device(&mut core_ctx, &bindings_ctx, unbound.clone()),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        set_device(&mut core_ctx, &mut bindings_ctx, unbound.clone(), None).unwrap();
        assert_eq!(get_bound_device(&mut core_ctx, &bindings_ctx, unbound), None);
    }

    #[ip_test]
    fn send_to_binds_unbound<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId> {
            outer: Default::default(),
            inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![I::FAKE_CONFIG.remote_ip],
                }]),
                Default::default(),
            ),
        };
        let mut bindings_ctx = FakeBindingsCtx::default();

        let socket = create(&mut core_ctx);
        let body = Buf::new(Vec::new(), ..);

        send_to(
            &mut core_ctx,
            &mut bindings_ctx,
            socket,
            Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
            'a',
            body,
        )
        .expect("succeeds");
        assert_matches!(
            get_info(&mut core_ctx, &mut bindings_ctx, socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    fn send_to_no_route_still_binds<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, _> {
            outer: Default::default(),
            inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![],
                }]),
                Default::default(),
            ),
        };
        let mut bindings_ctx = FakeBindingsCtx::default();

        let socket = create(&mut core_ctx);
        let body = Buf::new(Vec::new(), ..);

        assert_matches!(
            send_to(
                &mut core_ctx,
                &mut bindings_ctx,
                socket,
                Some(ZonedAddr::Unzoned(I::FAKE_CONFIG.remote_ip).into()),
                'a',
                body,
            ),
            Err(Either::Right(SendToError::CreateAndSend(_)))
        );
        assert_matches!(
            get_info(&mut core_ctx, &mut bindings_ctx, socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    #[test_case(true; "remove device b")]
    #[test_case(false; "dont remove device b")]
    fn multicast_membership_changes<I: Ip + DatagramIpExt<MultipleDevicesId> + TestIpExt>(
        remove_device_b: bool,
    ) {
        let mut core_ctx = FakeIpSocketCtx::<I, MultipleDevicesId>::new(
            MultipleDevicesId::all().into_iter().map(|device| FakeDeviceConfig {
                device,
                local_ips: Default::default(),
                remote_ips: Default::default(),
            }),
        );
        let mut bindings_ctx = FakeBindingsCtx::<(), (), ()>::default();

        let multicast_addr1 = I::get_multicast_addr(1);
        let mut memberships = MulticastMemberships::default();
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr1,
                &FakeWeakDeviceId(MultipleDevicesId::A),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        core_ctx.join_multicast_group(&mut bindings_ctx, &MultipleDevicesId::A, multicast_addr1);

        let multicast_addr2 = I::get_multicast_addr(2);
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr2,
                &FakeWeakDeviceId(MultipleDevicesId::B),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        core_ctx.join_multicast_group(&mut bindings_ctx, &MultipleDevicesId::B, multicast_addr2);

        for (device, addr, expected) in [
            (MultipleDevicesId::A, multicast_addr1, true),
            (MultipleDevicesId::A, multicast_addr2, false),
            (MultipleDevicesId::B, multicast_addr1, false),
            (MultipleDevicesId::B, multicast_addr2, true),
        ] {
            assert_eq!(
                core_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }

        if remove_device_b {
            AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut core_ctx)
                .set_device_removed(MultipleDevicesId::B, true);
        }

        leave_all_joined_groups(&mut core_ctx, &mut bindings_ctx, memberships);
        for (device, addr, expected) in [
            (MultipleDevicesId::A, multicast_addr1, false),
            (MultipleDevicesId::A, multicast_addr2, false),
            (MultipleDevicesId::B, multicast_addr1, false),
            // Should not attempt to leave the multicast group on the device if
            // the device looks like it was removed. Note that although we mark
            // the device as removed, we do not destroy its state so we can
            // inspect it here.
            (MultipleDevicesId::B, multicast_addr2, remove_device_b),
        ] {
            assert_eq!(
                core_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }
    }

    #[ip_test]
    fn set_get_transparent<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>() {
        let mut core_ctx = FakeCoreCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                }]),
                Default::default(),
            ),
        };

        let unbound = create(&mut core_ctx);

        assert!(!get_ip_transparent(&mut core_ctx, unbound.clone()));

        set_ip_transparent(&mut core_ctx, unbound.clone(), true);

        assert!(get_ip_transparent(&mut core_ctx, unbound.clone()));

        set_ip_transparent(&mut core_ctx, unbound.clone(), false);

        assert!(!get_ip_transparent(&mut core_ctx, unbound.clone()));
    }

    // Translates a listener addr from the associated type to a concrete type.
    fn convert_listener_addr<I: DualStackIpExt, LI: Clone + Debug>(
        addr: I::DualStackListenerIpAddr<LI>,
    ) -> MaybeDualStack<DualStackListenerIpAddr<I::Addr, LI>, ListenerIpAddr<I::Addr, LI>> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt, LI: Clone + Debug>(I::DualStackListenerIpAddr<LI>);
        I::map_ip(
            Wrapper(addr),
            |Wrapper(addr)| MaybeDualStack::NotDualStack(addr),
            |Wrapper(addr)| MaybeDualStack::DualStack(addr),
        )
    }

    // Translates a conn addr from the associated type to a concrete type.
    fn convert_conn_addr<I: DualStackIpExt, S: DatagramSocketSpec>(
        addr: I::DualStackConnIpAddr<S>,
    ) -> MaybeDualStack<
        DualStackConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
        ConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    > {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<I: DualStackIpExt, S: DatagramSocketSpec>(I::DualStackConnIpAddr<S>);
        I::map_ip(
            Wrapper(addr),
            |Wrapper(addr)| MaybeDualStack::NotDualStack(addr),
            |Wrapper(addr)| MaybeDualStack::DualStack(addr),
        )
    }

    #[derive(Eq, PartialEq)]
    enum OriginalSocketState {
        Unbound,
        Listener,
        Connected,
    }

    #[ip_test]
    #[test_case(OriginalSocketState::Unbound; "reinsert_unbound")]
    #[test_case(OriginalSocketState::Listener; "reinsert_listener")]
    #[test_case(OriginalSocketState::Connected; "reinsert_connected")]
    fn connect_reinserts_on_failure_single_stack<
        I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt,
    >(
        original: OriginalSocketState,
    ) {
        connect_reinserts_on_failure_inner::<I>(
            original,
            I::FAKE_CONFIG.local_ip.get(),
            I::FAKE_CONFIG.remote_ip,
        );
    }

    #[test_case(OriginalSocketState::Listener, net_ip_v6!("::FFFF:192.0.2.1"),
        net_ip_v4!("192.0.2.2"); "reinsert_listener_other_stack")]
    #[test_case(OriginalSocketState::Listener, net_ip_v6!("::"),
        net_ip_v4!("192.0.2.2"); "reinsert_listener_both_stacks")]
    #[test_case(OriginalSocketState::Connected, net_ip_v6!("::FFFF:192.0.2.1"),
        net_ip_v4!("192.0.2.2"); "reinsert_connected_other_stack")]
    fn connect_reinserts_on_failure_dual_stack(
        original: OriginalSocketState,
        local_ip: Ipv6Addr,
        remote_ip: Ipv4Addr,
    ) {
        let remote_ip = remote_ip.to_ipv6_mapped();
        connect_reinserts_on_failure_inner::<Ipv6>(original, local_ip, remote_ip);
    }

    fn connect_reinserts_on_failure_inner<I: Ip + DatagramIpExt<FakeDeviceId> + IpLayerIpExt>(
        original: OriginalSocketState,
        local_ip: I::Addr,
        remote_ip: SpecifiedAddr<I::Addr>,
    ) {
        let FakeCtxWithCoreCtx { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs(
                local_ip.to_ip_addr(),
                remote_ip.into(),
                [FakeDeviceId {}],
                |device_configs| FakeCoreCtx::<I, _> {
                    outer: FakeSocketsState::default(),
                    inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                        FakeDualStackIpSocketCtx::new(device_configs),
                        Default::default(),
                    ),
                },
            );

        let socket = create(&mut core_ctx);
        const LOCAL_PORT: u8 = 10;
        const ORIGINAL_REMOTE_PORT: char = 'a';
        const NEW_REMOTE_PORT: char = 'b';

        // Setup the original socket state.
        match original {
            OriginalSocketState::Unbound => {}
            OriginalSocketState::Listener => listen(
                &mut core_ctx,
                &mut bindings_ctx,
                socket,
                SpecifiedAddr::new(local_ip).map(|a| ZonedAddr::Unzoned(a).into()),
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed"),
            OriginalSocketState::Connected => connect(
                &mut core_ctx,
                &mut bindings_ctx,
                socket,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                ORIGINAL_REMOTE_PORT,
                Default::default(),
            )
            .expect("connect should succeed"),
        }

        // Update the sharing state to generate conflicts during the call to `connect`.
        core_ctx.with_sockets_state_mut(|_core_ctx, state| {
            let sharing = match state.get_mut(socket.get_key_index()).expect("socket not found") {
                SocketState::Unbound(UnboundSocketState { device: _, sharing, ip_options: _ }) => {
                    sharing
                }
                SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                    match socket_type {
                        BoundSocketStateType::Connected { state: _, sharing } => sharing,
                        BoundSocketStateType::Listener { state: _, sharing } => sharing,
                    }
                }
            };
            *sharing = Sharing::ConnectionConflicts { remote_port: NEW_REMOTE_PORT };
        });

        // Try to connect and observe a conflict error.
        assert_matches!(
            connect(
                &mut core_ctx,
                &mut bindings_ctx,
                socket,
                Some(ZonedAddr::Unzoned(remote_ip).into()),
                NEW_REMOTE_PORT,
                Default::default(),
            ),
            Err(ConnectError::SockAddrConflict)
        );

        // Verify the original socket state is intact.
        let info = get_info(&mut core_ctx, &mut bindings_ctx, socket);
        match original {
            OriginalSocketState::Unbound => assert_matches!(info, SocketInfo::Unbound),
            OriginalSocketState::Listener => {
                let ip =
                    assert_matches!(info, SocketInfo::Listener(ListenerAddr{ip, device: _}) => ip);
                let identifier = match convert_listener_addr::<I, _>(ip) {
                    MaybeDualStack::DualStack(DualStackListenerIpAddr::ThisStack(
                        ListenerIpAddr { addr: _, identifier },
                    )) => identifier,
                    MaybeDualStack::DualStack(DualStackListenerIpAddr::OtherStack(
                        ListenerIpAddr { addr: _, identifier },
                    ))
                    | MaybeDualStack::DualStack(DualStackListenerIpAddr::BothStacks(identifier)) => {
                        identifier
                    }
                    MaybeDualStack::NotDualStack(ListenerIpAddr { addr: _, identifier }) => {
                        identifier
                    }
                };
                assert_eq!(LOCAL_PORT, identifier);
            }
            OriginalSocketState::Connected => {
                let ip =
                    assert_matches!(info, SocketInfo::Connected(ConnAddr{ip, device: _}) => ip);
                let identifier = match convert_conn_addr::<I, _>(ip) {
                    MaybeDualStack::DualStack(addr) => match addr {
                        DualStackConnIpAddr::ThisStack(ConnIpAddr {
                            local: _,
                            remote: (_addr, identifier),
                        }) => identifier,
                        DualStackConnIpAddr::OtherStack(ConnIpAddr {
                            local: _,
                            remote: (_addr, identifier),
                        }) => identifier,
                    },
                    MaybeDualStack::NotDualStack(ConnIpAddr {
                        local: _,
                        remote: (_addr, identifier),
                    }) => identifier,
                };
                assert_eq!(ORIGINAL_REMOTE_PORT, identifier)
            }
        }
    }

    #[test_case(net_ip_v6!("::a:b:c:d"), ShutdownType::Send; "this_stack_send")]
    #[test_case(net_ip_v6!("::a:b:c:d"), ShutdownType::Receive; "this_stack_receive")]
    #[test_case(net_ip_v6!("::a:b:c:d"), ShutdownType::SendAndReceive; "this_stack_send_and_receive")]
    #[test_case(net_ip_v6!("::FFFF:192.0.2.1"), ShutdownType::Send; "other_stack_send")]
    #[test_case(net_ip_v6!("::FFFF:192.0.2.1"), ShutdownType::Receive; "other_stack_receive")]
    #[test_case(net_ip_v6!("::FFFF:192.0.2.1"), ShutdownType::SendAndReceive; "other_stack_send_and_receive")]
    fn set_get_shutdown_dualstack(remote_ip: Ipv6Addr, shutdown: ShutdownType) {
        let remote_ip = SpecifiedAddr::new(remote_ip).expect("remote_ip should be specified");
        let FakeCtxWithCoreCtx { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs(
                Ipv6::UNSPECIFIED_ADDRESS.into(),
                remote_ip.into(),
                [FakeDeviceId {}],
                |device_configs| FakeCoreCtx::<Ipv6, _> {
                    outer: FakeSocketsState::default(),
                    inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                        FakeDualStackIpSocketCtx::new(device_configs),
                        Default::default(),
                    ),
                },
            );

        const REMOTE_PORT: char = 'a';
        let socket = create(&mut core_ctx);
        connect(
            &mut core_ctx,
            &mut bindings_ctx,
            socket,
            Some(ZonedAddr::Unzoned(remote_ip).into()),
            REMOTE_PORT,
            Default::default(),
        )
        .expect("connect should succeed");
        assert_eq!(get_shutdown_connected(&mut core_ctx, &bindings_ctx, socket), None);

        shutdown_connected(&mut core_ctx, &bindings_ctx, socket, shutdown)
            .expect("shutdown should succeed");
        assert_eq!(get_shutdown_connected(&mut core_ctx, &bindings_ctx, socket), Some(shutdown));
    }

    #[ip_test]
    #[test_case(OriginalSocketState::Unbound; "unbound")]
    #[test_case(OriginalSocketState::Listener; "listener")]
    #[test_case(OriginalSocketState::Connected; "connected")]
    fn set_get_device_single_stack<I: Ip + DatagramIpExt<MultipleDevicesId> + IpLayerIpExt>(
        original: OriginalSocketState,
    ) {
        set_get_device_inner::<I>(
            original,
            I::FAKE_CONFIG.local_ip.get(),
            I::FAKE_CONFIG.remote_ip,
        );
    }

    #[test_case(OriginalSocketState::Listener, net_ip_v6!("::FFFF:192.0.2.1"),
        net_ip_v4!("192.0.2.2"); "listener_other_stack")]
    #[test_case(OriginalSocketState::Listener, net_ip_v6!("::"),
        net_ip_v4!("192.0.2.2"); "listener_both_stacks")]
    #[test_case(OriginalSocketState::Connected, net_ip_v6!("::FFFF:192.0.2.1"),
        net_ip_v4!("192.0.2.2"); "connected_other_stack")]
    fn set_get_device_dual_stack(
        original: OriginalSocketState,
        local_ip: Ipv6Addr,
        remote_ip: Ipv4Addr,
    ) {
        let remote_ip = remote_ip.to_ipv6_mapped();
        set_get_device_inner::<Ipv6>(original, local_ip, remote_ip);
    }

    fn set_get_device_inner<I: Ip + DatagramIpExt<MultipleDevicesId> + IpLayerIpExt>(
        original: OriginalSocketState,
        local_ip: I::Addr,
        remote_ip: SpecifiedAddr<I::Addr>,
    ) {
        const DEVICE_ID1: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE_ID2: MultipleDevicesId = MultipleDevicesId::B;

        let FakeCtxWithCoreCtx { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs(
                local_ip.to_ip_addr(),
                remote_ip.into(),
                [DEVICE_ID1, DEVICE_ID2],
                |device_configs| FakeCoreCtx::<I, _> {
                    outer: FakeSocketsState::default(),
                    inner: WrappedFakeCoreCtx::with_inner_and_outer_state(
                        FakeDualStackIpSocketCtx::new(device_configs),
                        Default::default(),
                    ),
                },
            );

        const LOCAL_PORT: u8 = 10;
        const REMOTE_PORT: char = 'a';

        let socket1 = create(&mut core_ctx);
        let socket2 = create(&mut core_ctx);

        // Initialize each socket to the `original` state, and verify that their
        // device can be set.
        for (socket, device_id) in [(socket1, DEVICE_ID1), (socket2, DEVICE_ID2)] {
            match original {
                OriginalSocketState::Unbound => {}
                OriginalSocketState::Listener => listen(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    socket,
                    SpecifiedAddr::new(local_ip).map(|a| ZonedAddr::Unzoned(a).into()),
                    Some(LOCAL_PORT),
                )
                .expect("listen should succeed"),
                OriginalSocketState::Connected => connect(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    socket,
                    Some(ZonedAddr::Unzoned(remote_ip).into()),
                    REMOTE_PORT,
                    Default::default(),
                )
                .expect("connect should succeed"),
            }

            assert_eq!(get_bound_device(&mut core_ctx, &bindings_ctx, socket), None);
            set_device(&mut core_ctx, &mut bindings_ctx, socket, Some(&device_id))
                .expect("set device should succeed");
            assert_eq!(
                get_bound_device(&mut core_ctx, &bindings_ctx, socket),
                Some(FakeWeakDeviceId(device_id))
            );
        }

        // For bound sockets, try to bind socket 2 to device 1, and expect it
        // it to conflict with socket 1 (They now have identical address keys in
        // the bound socket map)
        if original != OriginalSocketState::Unbound {
            assert_eq!(
                set_device(&mut core_ctx, &mut bindings_ctx, socket2, Some(&DEVICE_ID1)),
                Err(SocketError::Local(LocalAddressError::AddressInUse))
            );
            // Verify both sockets still have their original device.
            assert_eq!(
                get_bound_device(&mut core_ctx, &bindings_ctx, socket1),
                Some(FakeWeakDeviceId(DEVICE_ID1))
            );
            assert_eq!(
                get_bound_device(&mut core_ctx, &bindings_ctx, socket2),
                Some(FakeWeakDeviceId(DEVICE_ID2))
            );
        }

        // Verify the device can be unset.
        // NB: Close socket2 first, otherwise socket 1 will conflict with it.
        let _: SocketInfo<_, _, _> = close(&mut core_ctx, &mut bindings_ctx, socket2);
        set_device(&mut core_ctx, &mut bindings_ctx, socket1, None)
            .expect("set device should succeed");
        assert_eq!(get_bound_device(&mut core_ctx, &bindings_ctx, socket1), None,);
    }
}
