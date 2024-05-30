// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Shared code for implementing datagram sockets.

use alloc::{
    collections::{HashMap, HashSet},
    vec::Vec,
};
use core::{
    borrow::Borrow,
    convert::Infallible as Never,
    fmt::Debug,
    hash::Hash,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU8},
    ops::{Deref, DerefMut},
};
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};

use derivative::Derivative;
use either::Either;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpVersionMarker, Ipv4, Ipv6},
    MulticastAddr, MulticastAddress as _, SpecifiedAddr, ZonedAddr,
};
use netstack3_base::{
    socket::{
        self, BoundSocketMap, ConnAddr, ConnInfoAddr, ConnIpAddr, DualStackConnIpAddr,
        DualStackListenerIpAddr, DualStackLocalIp, DualStackRemoteIp, EitherStack, InsertError,
        ListenerAddr, ListenerIpAddr, MaybeDualStack, NotDualStackCapableError, Shutdown,
        ShutdownType, SocketDeviceUpdate, SocketDeviceUpdateNotAllowedError, SocketIpAddr,
        SocketIpExt, SocketMapAddrSpec, SocketMapConflictPolicy, SocketMapStateSpec,
        SocketZonedAddrExt as _, StrictlyZonedAddr,
    },
    sync::{self, RwLock},
    AnyDevice, BidirectionalConverter, DeviceIdContext, DeviceIdentifier, EitherDeviceId,
    ExistsError, Inspector, InspectorDeviceExt, LocalAddressError, NotFoundError,
    OwnedOrRefsBidirectionalConverter, ReferenceNotifiers, ReferenceNotifiersExt as _,
    RemoteAddressError, RemoveResourceResultWithContext, RngContext, SocketError,
    StrongDeviceIdentifier as _, WeakDeviceIdentifier, ZonedAddressError,
};
use netstack3_filter::TransportPacketSerializer;
use packet::BufferMut;
use packet_formats::ip::IpProtoExt;
use thiserror::Error;

use crate::internal::{
    base::{
        self as ip, HopLimits, MulticastMembershipHandler, ResolveRouteError, TransportIpContext,
    },
    icmp,
    socket::{
        IpSock, IpSockCreateAndSendError, IpSockCreationError, IpSockSendError, IpSocketHandler,
        SendOneShotIpPacketError, SendOptions,
    },
};

pub(crate) mod spec_context;
mod uninstantiable;

/// Datagram demultiplexing map.
pub type BoundSockets<I, D, A, S> = BoundSocketMap<I, D, A, S>;

/// Top-level struct kept in datagram socket references.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct ReferenceState<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    pub(crate) state: RwLock<SocketState<I, D, S>>,
    pub(crate) external_data: S::ExternalData<I>,
}

// Local aliases for brevity.
type PrimaryRc<I, D, S> = sync::PrimaryRc<ReferenceState<I, D, S>>;
/// A convenient alias for a strong reference to a datagram socket.
pub type StrongRc<I, D, S> = sync::StrongRc<ReferenceState<I, D, S>>;
/// A convenient alias for a weak reference to a datagram socket.
pub type WeakRc<I, D, S> = sync::WeakRc<ReferenceState<I, D, S>>;

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    OrderedLockAccess<SocketState<I, D, S>> for ReferenceState<I, D, S>
{
    type Lock = RwLock<SocketState<I, D, S>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.state)
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> ReferenceState<I, D, S> {
    /// Returns the external data associated with the socket.
    pub fn external_data(&self) -> &S::ExternalData<I> {
        &self.external_data
    }

    /// Provides access to the socket state sidestepping lock ordering.
    #[cfg(any(test, feature = "testutils"))]
    pub fn state(&self) -> &RwLock<SocketState<I, D, S>> {
        &self.state
    }
}

/// A set containing all datagram sockets for a given implementation.
#[derive(Derivative, GenericOverIp)]
#[derivative(Default(bound = ""))]
#[generic_over_ip(I, Ip)]
pub struct DatagramSocketSet<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    HashMap<StrongRc<I, D, S>, PrimaryRc<I, D, S>>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> Debug
    for DatagramSocketSet<I, D, S>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_list().entries(rc.keys().map(StrongRc::debug_id)).finish()
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> Deref
    for DatagramSocketSet<I, D, S>
{
    type Target = HashMap<StrongRc<I, D, S>, PrimaryRc<I, D, S>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> DerefMut
    for DatagramSocketSet<I, D, S>
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Marker trait for datagram IP extensions.
pub trait IpExt: ip::IpExt + DualStackIpExt + icmp::IcmpIpExt {}
impl<I: ip::IpExt + DualStackIpExt + icmp::IcmpIpExt> IpExt for I {}

/// A datagram socket's state.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Debug(bound = ""))]
#[allow(missing_docs)]
pub enum SocketState<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    Unbound(UnboundSocketState<I, D, S>),
    Bound(BoundSocketState<I, D, S>),
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for SocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Unbound(unbound) => unbound.as_ref(),
            Self::Bound(bound) => bound.as_ref(),
        }
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> SocketState<I, D, S> {
    fn to_socket_info(&self) -> SocketInfo<I::Addr, D> {
        match self {
            Self::Unbound(_) => SocketInfo::Unbound,
            Self::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state, sharing: _ } => {
                        let ListenerState { addr, ip_options: _ } = state;
                        SocketInfo::Listener(addr.clone().into())
                    }
                    BoundSocketStateType::Connected { state, sharing: _ } => {
                        SocketInfo::Connected(S::conn_info_from_state(&state))
                    }
                }
            }
        }
    }

    /// Record inspect information generic to each datagram protocol.
    pub fn record_common_info<N>(&self, inspector: &mut N, socket_id: &S::SocketId<I, D>)
    where
        N: Inspector + InspectorDeviceExt<D>,
    {
        inspector.record_debug_child(socket_id, |node| {
            node.record_str("TransportProtocol", S::NAME);
            node.record_str("NetworkProtocol", I::NAME);

            let socket_info = self.to_socket_info();
            let (local, remote) = match socket_info {
                SocketInfo::Unbound => (None, None),
                SocketInfo::Listener(ListenerInfo { local_ip, local_identifier }) => (
                    Some((
                        local_ip.map_or_else(
                            || ZonedAddr::Unzoned(I::UNSPECIFIED_ADDRESS),
                            |addr| addr.into_inner_without_witness(),
                        ),
                        local_identifier,
                    )),
                    None,
                ),
                SocketInfo::Connected(ConnInfo {
                    local_ip,
                    local_identifier,
                    remote_ip,
                    remote_identifier,
                }) => (
                    Some((local_ip.into_inner_without_witness(), local_identifier)),
                    Some((remote_ip.into_inner_without_witness(), remote_identifier)),
                ),
            };
            node.record_local_socket_addr::<N, _, _, _>(local);
            node.record_remote_socket_addr::<N, _, _, _>(remote);
        });
    }
}

/// State associated with a Bound Socket.
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub struct BoundSocketState<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    /// The type of bound socket (e.g. Listener vs. Connected), and any
    /// type-specific state.
    pub socket_type: BoundSocketStateType<I, D, S>,
    /// The original bound address of the socket, as requested by the caller.
    /// `None` if:
    ///   * the socket was connected from unbound, or
    ///   * listen was called without providing a local port.
    pub original_bound_addr: Option<S::ListenerIpAddr<I>>,
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
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

/// State for the sub-types of bound socket (e.g. Listener or Connected).
#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
#[allow(missing_docs)]
pub enum BoundSocketStateType<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    Listener { state: ListenerState<I, D, S>, sharing: S::SharingState },
    Connected { state: S::ConnState<I, D>, sharing: S::SharingState },
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for BoundSocketStateType<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            Self::Listener { state, sharing: _ } => state.as_ref(),
            Self::Connected { state, sharing: _ } => state.as_ref(),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""), Default(bound = ""))]
pub struct UnboundSocketState<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    device: Option<D>,
    sharing: S::SharingState,
    ip_options: IpOptions<I, D, S>,
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for UnboundSocketState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

/// State associated with a listening socket.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct ListenerState<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec + ?Sized> {
    pub(crate) ip_options: IpOptions<I, D, S>,
    pub(crate) addr: ListenerAddr<S::ListenerIpAddr<I>, D>,
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for ListenerState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        &self.ip_options
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = "D: Debug"))]
pub struct ConnState<
    WireI: IpExt,
    SocketI: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec + ?Sized,
> {
    pub(crate) socket: IpSock<WireI, D>,
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
    /// TODO(https://fxbug.dev/42061727): Implement this by changing socket
    /// addresses.
    pub(crate) clear_device_on_disconnect: bool,

    /// The extra state for the connection.
    ///
    /// For UDP it should be [`()`], for ICMP it should be [`NonZeroU16`] to
    /// remember the remote ID set by connect.
    pub(crate) extra: S::ConnStateExtra,
}

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    AsRef<IpOptions<SocketI, D, S>> for ConnState<WireI, SocketI, D, S>
{
    fn as_ref(&self) -> &IpOptions<SocketI, D, S> {
        &self.ip_options
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<Shutdown>
    for ConnState<WireI, SocketI, D, S>
{
    fn as_ref(&self) -> &Shutdown {
        &self.shutdown
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    AsMut<IpOptions<SocketI, D, S>> for ConnState<WireI, SocketI, D, S>
{
    fn as_mut(&mut self) -> &mut IpOptions<SocketI, D, S> {
        &mut self.ip_options
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsMut<Shutdown>
    for ConnState<WireI, SocketI, D, S>
{
    fn as_mut(&mut self) -> &mut Shutdown {
        &mut self.shutdown
    }
}

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    ConnState<WireI, SocketI, D, S>
{
    /// Returns true if the connection can receive traffic.
    pub fn should_receive(&self) -> bool {
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
#[derivative(Debug(bound = ""))]
pub enum DualStackConnState<
    I: IpExt + DualStackIpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec + ?Sized,
> {
    /// The [`ConnState`] for a socked connected with [`I::Version`].
    ThisStack(ConnState<I, I, D, S>),
    /// The [`ConnState`] for a socked connected with [`I::OtherVersion`].
    OtherStack(ConnState<I::OtherVersion, I, D, S>),
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_ref(&self) -> &IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_ref(),
            DualStackConnState::OtherStack(state) => state.as_ref(),
        }
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsMut<IpOptions<I, D, S>>
    for DualStackConnState<I, D, S>
{
    fn as_mut(&mut self) -> &mut IpOptions<I, D, S> {
        match self {
            DualStackConnState::ThisStack(state) => state.as_mut(),
            DualStackConnState::OtherStack(state) => state.as_mut(),
        }
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<Shutdown>
    for DualStackConnState<I, D, S>
{
    fn as_ref(&self) -> &Shutdown {
        match self {
            DualStackConnState::ThisStack(state) => state.as_ref(),
            DualStackConnState::OtherStack(state) => state.as_ref(),
        }
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsMut<Shutdown>
    for DualStackConnState<I, D, S>
{
    fn as_mut(&mut self) -> &mut Shutdown {
        match self {
            DualStackConnState::ThisStack(state) => state.as_mut(),
            DualStackConnState::OtherStack(state) => state.as_mut(),
        }
    }
}

/// A datagram socket's options.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Clone(bound = ""), Debug, Default(bound = ""))]
pub struct DatagramSocketOptions<I: IpExt, D: WeakDeviceIdentifier> {
    /// The configured hop limits.
    pub hop_limits: SocketHopLimits<I>,
    /// The selected multicast interface.
    pub multicast_interface: Option<D>,

    /// Whether multicast packet loopback is enabled or not (see
    /// IP_MULTICAST_LOOP flag). Enabled by default.
    #[derivative(Default(value = "true"))]
    pub multicast_loop: bool,
}

impl<I: IpExt, D: WeakDeviceIdentifier> SendOptions<I> for DatagramSocketOptions<I, D> {
    fn hop_limit(&self, destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8> {
        let Self { hop_limits: SocketHopLimits { unicast, multicast, version: _ }, .. } = self;
        if destination.is_multicast() {
            *multicast
        } else {
            *unicast
        }
    }

    fn multicast_loop(&self) -> bool {
        self.multicast_loop
    }
}

/// A datagram socket's IP options.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Clone(bound = ""), Debug, Default(bound = ""))]
pub struct IpOptions<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec + ?Sized> {
    multicast_memberships: MulticastMemberships<I::Addr, D>,
    socket_options: DatagramSocketOptions<I, D>,
    other_stack: S::OtherStackIpOptions<I, D>,
    transparent: bool,
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> AsRef<Self> for IpOptions<I, D, S> {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> IpOptions<I, D, S> {
    /// Returns the IP options for the other stack.
    pub fn other_stack(&self) -> &S::OtherStackIpOptions<I, D> {
        &self.other_stack
    }

    /// Returns the transparent option.
    pub fn transparent(&self) -> bool {
        self.transparent
    }
}

/// The configurable hop limits for a datagram socket.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SocketHopLimits<I: Ip> {
    /// Unicast hop limit.
    pub unicast: Option<NonZeroU8>,
    /// Multicast hop limit.
    // TODO(https://fxbug.dev/42059735): Make this an Option<u8> to allow sending
    // multicast packets destined only for the local machine.
    pub multicast: Option<NonZeroU8>,
    /// An unused marker type signifying the IP version for which these hop
    /// limits are valid. Including this helps prevent using the wrong hop limits
    /// when operating on dualstack sockets.
    pub version: IpVersionMarker<I>,
}

impl<I: Ip> SocketHopLimits<I> {
    /// Returns a function that updates the unicast hop limit.
    pub fn set_unicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.unicast = value
    }

    /// Returns a function that updates the multicast hop limit.
    pub fn set_multicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
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

#[derive(Clone, Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct MulticastMemberships<A, D>(HashSet<(MulticastAddr<A>, D)>);

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) enum MulticastMembershipChange {
    Join,
    Leave,
}

impl<A: Eq + Hash, D: WeakDeviceIdentifier> MulticastMemberships<A, D> {
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

fn leave_all_joined_groups<A: IpAddress, BC, CC: MulticastMembershipHandler<A::Version, BC>>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    memberships: MulticastMemberships<A, CC::WeakDeviceId>,
) {
    for (addr, device) in memberships {
        let Some(device) = device.upgrade() else {
            continue;
        };
        core_ctx.leave_multicast_group(bindings_ctx, &device, addr)
    }
}

/// Identifies a flow for a datagram socket.
#[derive(Hash)]
pub struct DatagramFlowId<A: IpAddress, RI> {
    /// Socket's local address.
    pub local_ip: SocketIpAddr<A>,
    /// Socket's remote address.
    pub remote_ip: SocketIpAddr<A>,
    /// Socket's remote identifier (port).
    pub remote_id: RI,
}

/// The core context providing access to datagram socket state.
pub trait DatagramStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The core context passed to the callback provided to methods.
    type SocketsStateCtx<'a>: DatagramBoundStateContext<I, BC, S>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;

    /// Calls the function with mutable access to the set with all datagram
    /// sockets.
    fn with_all_sockets_mut<O, F: FnOnce(&mut DatagramSocketSet<I, Self::WeakDeviceId, S>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with immutable access to the set with all datagram
    /// sockets.
    fn with_all_sockets<O, F: FnOnce(&DatagramSocketSet<I, Self::WeakDeviceId, S>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with an immutable reference to the given socket's
    /// state.
    fn with_socket_state<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &SocketState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        id: &S::SocketId<I, Self::WeakDeviceId>,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the given socket's state.
    fn with_socket_state_mut<
        O,
        F: FnOnce(&mut Self::SocketsStateCtx<'_>, &mut SocketState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        id: &S::SocketId<I, Self::WeakDeviceId>,
        cb: F,
    ) -> O;

    /// Call `f` with each socket's state.
    fn for_each_socket<
        F: FnMut(
            &mut Self::SocketsStateCtx<'_>,
            &S::SocketId<I, Self::WeakDeviceId>,
            &SocketState<I, Self::WeakDeviceId, S>,
        ),
    >(
        &mut self,
        cb: F,
    );
}

/// A convenient alias for the BoundSockets type to shorten type signatures.
type BoundSocketsFromSpec<I, CC, S> = BoundSockets<
    I,
    <CC as DeviceIdContext<AnyDevice>>::WeakDeviceId,
    <S as DatagramSocketSpec>::AddrSpec,
    <S as DatagramSocketSpec>::SocketMapSpec<I, <CC as DeviceIdContext<AnyDevice>>::WeakDeviceId>,
>;

/// The core context providing access to bound datagram sockets.
pub trait DatagramBoundStateContext<I: IpExt + DualStackIpExt, BC, S: DatagramSocketSpec>:
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

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &BoundSocketsFromSpec<I, Self, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_bound_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut BoundSocketsFromSpec<I, Self, S>) -> O,
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

/// A marker trait for the requirements of
/// [`DualStackDatagramBoundStateContext::ds_converter`].
pub trait DualStackConverter<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>:
    'static
    + OwnedOrRefsBidirectionalConverter<
        S::ListenerIpAddr<I>,
        DualStackListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
    >
    + OwnedOrRefsBidirectionalConverter<
        S::ConnIpAddr<I>,
        DualStackConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    >
    + OwnedOrRefsBidirectionalConverter<S::ConnState<I, D>, DualStackConnState<I, D, S>>
{
}

impl<I, D, S, O> DualStackConverter<I, D, S> for O
where
    I: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
    O: 'static
        + OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            DualStackListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        >
        + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            DualStackConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        >
        + OwnedOrRefsBidirectionalConverter<S::ConnState<I, D>, DualStackConnState<I, D, S>>,
{
}

/// Provides access to dual-stack socket state.
pub trait DualStackDatagramBoundStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The core context passed to the callbacks to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, BC>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        // Allow creating IP sockets for the other IP version.
        + TransportIpContext<I::OtherVersion, BC>;

    /// Returns if the socket state indicates dual-stack operation is enabled.
    fn dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) -> bool;

    /// Returns the [`DatagramSocketOptions`] to use for packets in the other stack.
    fn to_other_socket_options<'a>(
        &self,
        state: &'a IpOptions<I, Self::WeakDeviceId, S>,
    ) -> &'a DatagramSocketOptions<I::OtherVersion, Self::WeakDeviceId>;

    /// Asserts that the socket state indicates dual-stack operation is enabled.
    ///
    /// Provided trait function.
    fn assert_dual_stack_enabled(&self, state: &impl AsRef<IpOptions<I, Self::WeakDeviceId, S>>) {
        debug_assert!(self.dual_stack_enabled(state), "socket must be dual-stack enabled")
    }

    /// Returns an instance of a type that implements [`DualStackConverter`]
    /// for addresses.
    fn ds_converter(&self) -> impl DualStackConverter<I, Self::WeakDeviceId, S>;

    /// Converts a socket ID to a bound socket ID.
    ///
    /// Converts a socket ID for IP version `I` into a bound socket ID that can
    /// be inserted into the demultiplexing map for IP version `I::OtherVersion`.
    fn to_other_bound_socket_id(
        &self,
        id: &S::SocketId<I, Self::WeakDeviceId>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::BoundSocketId;

    /// demultiplexing maps.
    fn with_both_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSocketsFromSpec<I, Self, S>,
            &mut BoundSocketsFromSpec<I::OtherVersion, Self, S>,
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
            &mut BoundSocketsFromSpec<I::OtherVersion, Self, S>,
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

/// A marker trait for the requirements of
/// [`NonDualStackDatagramBoundStateContext::nds_converter`].
pub trait NonDualStackConverter<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>:
    'static
    + OwnedOrRefsBidirectionalConverter<
        S::ListenerIpAddr<I>,
        ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
    >
    + OwnedOrRefsBidirectionalConverter<
        S::ConnIpAddr<I>,
        ConnIpAddr<
            I::Addr,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    >
    + OwnedOrRefsBidirectionalConverter<S::ConnState<I, D>, ConnState<I, I, D, S>>
{
}

impl<I, D, S, O> NonDualStackConverter<I, D, S> for O
where
    I: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
    O: 'static
        + OwnedOrRefsBidirectionalConverter<
            S::ListenerIpAddr<I>,
            ListenerIpAddr<I::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        >
        + OwnedOrRefsBidirectionalConverter<
            S::ConnIpAddr<I>,
            ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        >
        + OwnedOrRefsBidirectionalConverter<S::ConnState<I, D>, ConnState<I, I, D, S>>,
{
}

/// Provides access to socket state for a single IP version.
pub trait NonDualStackDatagramBoundStateContext<I: IpExt, BC, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// Returns an instance of a type that implements [`NonDualStackConverter`]
    /// for addresses.
    fn nds_converter(&self) -> impl NonDualStackConverter<I, Self::WeakDeviceId, S>;
}

pub trait DatagramStateBindingsContext<I: Ip, S>: RngContext + ReferenceNotifiers {}
impl<BC: RngContext + ReferenceNotifiers, I: Ip, S> DatagramStateBindingsContext<I, S> for BC {}

/// Types and behavior for datagram socket demultiplexing map.
///
/// `I: Ip` describes the type of packets that can be received by sockets in
/// the map.
pub trait DatagramSocketMapSpec<I: Ip, D: DeviceIdentifier, A: SocketMapAddrSpec>:
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

/// A marker trait for dual-stack socket features.
///
/// This trait acts as a marker for [`DualStackBaseIpExt`] for both `Self` and
/// `Self::OtherVersion`.
pub trait DualStackIpExt:
    DualStackBaseIpExt + socket::DualStackIpExt<OtherVersion: DualStackBaseIpExt>
{
}

impl<I> DualStackIpExt for I where
    I: DualStackBaseIpExt + socket::DualStackIpExt<OtherVersion: DualStackBaseIpExt>
{
}

/// Common features of dual-stack sockets that vary by IP version.
///
/// This trait exists to provide per-IP-version associated types that are
/// useful for implementing dual-stack sockets. The types are intentionally
/// asymmetric - `DualStackIpExt::Xxx` has a different shape for the [`Ipv4`]
/// and [`Ipv6`] impls.
pub trait DualStackBaseIpExt: socket::DualStackIpExt + SocketIpExt + ip::IpExt {
    /// The type of socket that can receive an IP packet.
    ///
    /// For `Ipv4`, this is [`EitherIpSocket<S>`], and for `Ipv6` it is just
    /// `S::SocketId<Ipv6>`.
    ///
    /// [`EitherIpSocket<S>]`: [EitherIpSocket]
    type DualStackBoundSocketId<D: WeakDeviceIdentifier, S: DatagramSocketSpec>: Clone + Debug + Eq;

    /// The IP options type for the other stack that will be held for a socket.
    ///
    /// For [`Ipv4`], this is `()`, and for [`Ipv6`] it is `State`. For a
    /// protocol like UDP or TCP where the IPv6 socket is dual-stack capable,
    /// the generic state struct can have a field with type
    /// `I::OtherStackIpOptions<Ipv4InIpv6Options>`.
    type OtherStackIpOptions<State: Clone + Debug + Default + Send + Sync>: Clone
        + Debug
        + Default
        + Send
        + Sync;

    /// A listener address for dual-stack operation.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug + Send + Sync + Into<NonZeroU16>>: Clone
        + Debug
        + Send
        + Sync
        + Into<(Option<SpecifiedAddr<Self::Addr>>, NonZeroU16)>;

    /// A connected address for dual-stack operation.
    type DualStackConnIpAddr<S: DatagramSocketSpec>: Clone
        + Debug
        + Into<ConnInfoAddr<Self::Addr, <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier>>;

    /// Connection state for a dual-stack socket.
    type DualStackConnState<D: WeakDeviceIdentifier, S: DatagramSocketSpec>: Debug
        + AsRef<IpOptions<Self, D, S>>
        + AsMut<IpOptions<Self, D, S>>
        + Send
        + Sync
    where
        Self::OtherVersion: DualStackBaseIpExt;

    /// Convert a socket ID into a `Self::DualStackBoundSocketId`.
    ///
    /// For coherency reasons this can't be a `From` bound on
    /// `DualStackBoundSocketId`. If more methods are added, consider moving
    /// this to its own dedicated trait that bounds `DualStackBoundSocketId`.
    fn into_dual_stack_bound_socket_id<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
        id: S::SocketId<Self, D>,
    ) -> Self::DualStackBoundSocketId<D, S>
    where
        Self: IpExt;

    /// Retrieves the associated connection address from the connection state.
    fn conn_addr_from_state<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
        state: &Self::DualStackConnState<D, S>,
    ) -> ConnAddr<Self::DualStackConnIpAddr<S>, D>
    where
        Self::OtherVersion: DualStackBaseIpExt;
}

/// An IP Socket ID that is either `Ipv4` or `Ipv6`.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Debug(bound = ""),
    Eq(bound = "S::SocketId<Ipv4, D>: Eq, S::SocketId<Ipv6, D>: Eq"),
    PartialEq(bound = "S::SocketId<Ipv4, D>: PartialEq, S::SocketId<Ipv6, D>: PartialEq")
)]
#[allow(missing_docs)]
pub enum EitherIpSocket<D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    V4(S::SocketId<Ipv4, D>),
    V6(S::SocketId<Ipv6, D>),
}

impl DualStackBaseIpExt for Ipv4 {
    /// Incoming IPv4 packets may be received by either IPv4 or IPv6 sockets.
    type DualStackBoundSocketId<D: WeakDeviceIdentifier, S: DatagramSocketSpec> =
        EitherIpSocket<D, S>;
    type OtherStackIpOptions<State: Clone + Debug + Default + Send + Sync> = ();
    /// IPv4 sockets can't listen on dual-stack addresses.
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug + Send + Sync + Into<NonZeroU16>> =
        ListenerIpAddr<Self::Addr, LocalIdentifier>;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnIpAddr<S: DatagramSocketSpec> = ConnIpAddr<
        Self::Addr,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;
    /// IPv4 sockets cannot connect on dual-stack addresses.
    type DualStackConnState<D: WeakDeviceIdentifier, S: DatagramSocketSpec> =
        ConnState<Self, Self, D, S>;

    fn into_dual_stack_bound_socket_id<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
        id: S::SocketId<Self, D>,
    ) -> Self::DualStackBoundSocketId<D, S> {
        EitherIpSocket::V4(id)
    }

    fn conn_addr_from_state<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
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

impl DualStackBaseIpExt for Ipv6 {
    /// Incoming IPv6 packets may only be received by IPv6 sockets.
    type DualStackBoundSocketId<D: WeakDeviceIdentifier, S: DatagramSocketSpec> =
        S::SocketId<Self, D>;
    type OtherStackIpOptions<State: Clone + Debug + Default + Send + Sync> = State;
    /// IPv6 listeners can listen on dual-stack addresses (if the protocol
    /// and socket are dual-stack-enabled).
    type DualStackListenerIpAddr<LocalIdentifier: Clone + Debug + Send + Sync + Into<NonZeroU16>> =
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
    type DualStackConnState<D: WeakDeviceIdentifier, S: DatagramSocketSpec> =
        DualStackConnState<Self, D, S>;

    fn into_dual_stack_bound_socket_id<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
        id: S::SocketId<Self, D>,
    ) -> Self::DualStackBoundSocketId<D, S> {
        id
    }

    fn conn_addr_from_state<D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
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
pub struct WrapOtherStackIpOptions<
    'a,
    I: DualStackIpExt,
    S: 'a + Clone + Debug + Default + Send + Sync,
>(pub &'a I::OtherStackIpOptions<S>);

#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
/// A wrapper to make [`DualStackIpExt::OtherStackIpOptions`] [`GenericOverIp`].
pub struct WrapOtherStackIpOptionsMut<
    'a,
    I: DualStackIpExt,
    S: 'a + Clone + Debug + Default + Send + Sync,
>(pub &'a mut I::OtherStackIpOptions<S>);

/// Types and behavior for datagram sockets.
///
/// These sockets may or may not support dual-stack operation.
pub trait DatagramSocketSpec: Sized + 'static {
    /// Name of this datagram protocol.
    const NAME: &'static str;

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
    type SocketId<I: IpExt, D: WeakDeviceIdentifier>: Clone
        + Debug
        + Eq
        + Send
        + Borrow<StrongRc<I, D, Self>>
        + From<StrongRc<I, D, Self>>;

    /// IP-level options for sending `I::OtherVersion` IP packets.
    type OtherStackIpOptions<I: IpExt, D: WeakDeviceIdentifier>: Clone
        + Debug
        + Default
        + Send
        + Sync;

    /// The type of a listener IP address.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ListenerIpAddr`], which will be one of
    /// [`ListenerIpAddr`] or [`DualStackListenerIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ListenerIpAddr`].
    type ListenerIpAddr<I: IpExt>: Clone
        + Debug
        + Into<(Option<SpecifiedAddr<I::Addr>>, NonZeroU16)>
        + Send
        + Sync
        + 'static;

    /// The sharing state for a socket.
    ///
    /// NB: The underlying [`BoundSocketMap`]` uses separate types for the
    /// sharing state of connected vs listening sockets. At the moment, datagram
    /// sockets have no need for differentiated sharing states, so consolidate
    /// them under one type.
    type SharingState: Clone + Debug + Default + Send + Sync + 'static;

    /// The type of an IP address for a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnIpAddr`], which will be one of
    /// [`ConnIpAddr`] or [`DualStackConnIpAddr`].
    /// Non-dual-stack-capable protocols (like ICMP and raw IP sockets) should
    /// just use [`ConnIpAddr`].
    type ConnIpAddr<I: IpExt>: Clone
        + Debug
        + Into<ConnInfoAddr<I::Addr, <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier>>;

    /// The type of a state held by a connected socket.
    ///
    /// For dual-stack-capable datagram protocols like UDP, this should use
    /// [`DualStackIpExt::ConnState`], which will be one of [`ConnState`] or
    /// [`DualStackConnState`]. Non-dual-stack-capable protocols (like ICMP and
    /// raw IP sockets) should just use [`ConnState`].
    type ConnState<I: IpExt, D: WeakDeviceIdentifier>: Debug
        + AsRef<IpOptions<I, D, Self>>
        + AsMut<IpOptions<I, D, Self>>
        + Send
        + Sync;

    /// The extra state that a connection state want to remember.
    ///
    /// For example: UDP sockets does not have any extra state to remember, so
    /// it should just be `()`; ICMP sockets need to remember the remote ID the
    /// socket is 'connected' to, the remote ID is not used when sending nor
    /// participating in the demuxing decisions. So it will be stored in the
    /// extra state so that it can be retrieved later, i.e, it should be
    /// `NonZeroU16` for ICMP sockets.
    type ConnStateExtra: Debug + Send + Sync;

    /// The specification for the [`BoundSocketMap`] for a given IP version.
    ///
    /// Describes the per-address and per-socket values held in the
    /// demultiplexing map for a given IP version.
    type SocketMapSpec<I: IpExt + DualStackIpExt, D: WeakDeviceIdentifier>: DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
        ListenerSharingState = Self::SharingState,
        ConnSharingState = Self::SharingState,
    >;

    /// External data kept by datagram sockets.
    ///
    /// This is used to store opaque bindings data alongside the core data
    /// inside the socket references.
    type ExternalData<I: Ip>: Debug + Send + Sync + 'static;

    /// Returns the IP protocol of this datagram specification.
    fn ip_proto<I: IpProtoExt>() -> I::Proto;

    /// Converts [`Self::SocketId`] to [`DatagramSocketMapSpec::BoundSocketId`].
    ///
    /// Constructs a socket identifier to its in-demultiplexing map form. For
    /// protocols with dual-stack sockets, like UDP, implementations should
    /// perform a transformation. Otherwise it should be the identity function.
    fn make_bound_socket_map_id<I: IpExt, D: WeakDeviceIdentifier>(
        s: &Self::SocketId<I, D>,
    ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId;

    /// The type of serializer returned by [`DatagramSocketSpec::make_packet`]
    /// for a given IP version and buffer type.
    type Serializer<I: IpExt, B: BufferMut>: TransportPacketSerializer<I, Buffer = B>;
    /// The potential error for serializing a packet. For example, in UDP, this
    /// should be infallible but for ICMP, there will be an error if the input
    /// is not an echo request.
    type SerializeError;

    /// Constructs a packet serializer with `addr` and `body`.
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
    fn try_alloc_listen_identifier<I: IpExt, D: WeakDeviceIdentifier>(
        rng: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>;

    /// Retrieves the associated connection info from the connection state.
    fn conn_info_from_state<I: IpExt, D: WeakDeviceIdentifier>(
        state: &Self::ConnState<I, D>,
    ) -> ConnInfo<I::Addr, D>;

    /// Tries to allocate a local identifier.
    fn try_alloc_local_id<I: IpExt, D: WeakDeviceIdentifier, BC: RngContext>(
        bound: &BoundSocketMap<I, D, Self::AddrSpec, Self::SocketMapSpec<I, D>>,
        bindings_ctx: &mut BC,
        flow: DatagramFlowId<I::Addr, <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>;
}

/// The error returned when an identifier (i.e.) port is already in use.
pub struct InUseError;

/// Creates a primary ID without inserting it into the all socket map.
pub fn create_primary_id<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    external_data: S::ExternalData<I>,
) -> PrimaryRc<I, D, S> {
    PrimaryRc::new(ReferenceState {
        state: RwLock::new(SocketState::Unbound(UnboundSocketState::default())),
        external_data,
    })
}

/// Creates a new datagram socket and inserts it into the list of all open
/// datagram sockets for the provided spec `S`.
///
/// The caller is responsible for calling  [`close`] when it's done with the
/// resource.
pub fn create<I: IpExt, S: DatagramSocketSpec, BC, CC: DatagramStateContext<I, BC, S>>(
    core_ctx: &mut CC,
    external_data: S::ExternalData<I>,
) -> S::SocketId<I, CC::WeakDeviceId> {
    let primary = create_primary_id(external_data);
    let strong = PrimaryRc::clone_strong(&primary);
    core_ctx.with_all_sockets_mut(move |socket_set| {
        let strong = PrimaryRc::clone_strong(&primary);
        assert_matches::assert_matches!(socket_set.insert(strong, primary), None);
    });
    strong.into()
}

/// Collects all currently opened sockets.
pub fn collect_all_sockets<
    I: IpExt,
    S: DatagramSocketSpec,
    BC,
    CC: DatagramStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
) -> Vec<S::SocketId<I, CC::WeakDeviceId>> {
    core_ctx.with_all_sockets(|socket_set| socket_set.keys().map(|s| s.clone().into()).collect())
}

/// Information associated with a datagram listener.
#[derive(GenericOverIp, Debug, Eq, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct ListenerInfo<A: IpAddress, D> {
    /// The local address associated with a datagram listener, or `None` for any
    /// address.
    pub local_ip: Option<StrictlyZonedAddr<A, SpecifiedAddr<A>, D>>,
    /// The local port associated with a datagram listener.
    pub local_identifier: NonZeroU16,
}

impl<A: IpAddress, LA: Into<(Option<SpecifiedAddr<A>>, NonZeroU16)>, D> From<ListenerAddr<LA, D>>
    for ListenerInfo<A, D>
{
    fn from(ListenerAddr { ip, device }: ListenerAddr<LA, D>) -> Self {
        let (addr, local_identifier) = ip.into();
        Self {
            local_ip: addr.map(|addr| {
                StrictlyZonedAddr::new_with_zone(addr, || {
                    // The invariant that a zone is present if needed is upheld by
                    // set_bindtodevice and bind.
                    device.expect("device must be bound for addresses that require zones")
                })
            }),
            local_identifier,
        }
    }
}

impl<A: IpAddress, D> From<NonZeroU16> for ListenerInfo<A, D> {
    fn from(local_identifier: NonZeroU16) -> Self {
        Self { local_ip: None, local_identifier }
    }
}

/// Information associated with a datagram connection.
#[derive(Debug, GenericOverIp, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub struct ConnInfo<A: IpAddress, D> {
    /// The local address associated with a datagram connection.
    pub local_ip: StrictlyZonedAddr<A, SpecifiedAddr<A>, D>,
    /// The local identifier associated with a datagram connection.
    pub local_identifier: NonZeroU16,
    /// The remote address associated with a datagram connection.
    pub remote_ip: StrictlyZonedAddr<A, SpecifiedAddr<A>, D>,
    /// The remote identifier associated with a datagram connection.
    pub remote_identifier: u16,
}

impl<A: IpAddress, D> ConnInfo<A, D> {
    /// Construct a new `ConnInfo`.
    pub fn new(
        local_ip: SpecifiedAddr<A>,
        local_identifier: NonZeroU16,
        remote_ip: SpecifiedAddr<A>,
        remote_identifier: u16,
        mut get_zone: impl FnMut() -> D,
    ) -> Self {
        Self {
            local_ip: StrictlyZonedAddr::new_with_zone(local_ip, &mut get_zone),
            local_identifier,
            remote_ip: StrictlyZonedAddr::new_with_zone(remote_ip, &mut get_zone),
            remote_identifier,
        }
    }
}

/// Information about the addresses for a socket.
#[derive(GenericOverIp, Debug, PartialEq)]
#[generic_over_ip(A, IpAddress)]
pub enum SocketInfo<A: IpAddress, D> {
    /// The socket is not bound.
    Unbound,
    /// The socket is listening.
    Listener(ListenerInfo<A, D>),
    /// The socket is connected.
    Connected(ConnInfo<A, D>),
}

/// Closes the socket.
pub fn close<
    I: IpExt,
    S: DatagramSocketSpec,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: S::SocketId<I, CC::WeakDeviceId>,
) -> RemoveResourceResultWithContext<S::ExternalData<I>, BC> {
    // Remove the socket from the list first to prevent double close.
    let primary = core_ctx.with_all_sockets_mut(|all_sockets| {
        all_sockets.remove(id.borrow()).expect("socket already closed")
    });
    core_ctx.with_socket_state(&id, |core_ctx, state| {
        let ip_options = match state {
            SocketState::Unbound(UnboundSocketState { device: _, sharing: _, ip_options }) => {
                ip_options.clone()
            }
            SocketState::Bound(state) => match core_ctx.dual_stack_context() {
                MaybeDualStack::DualStack(dual_stack) => {
                    let op = DualStackRemoveOperation::new_from_state(dual_stack, &id, state);
                    dual_stack
                        .with_both_bound_sockets_mut(|_core_ctx, sockets, other_sockets| {
                            op.apply(sockets, other_sockets)
                        })
                        .into_options()
                }
                MaybeDualStack::NotDualStack(not_dual_stack) => {
                    let op = SingleStackRemoveOperation::new_from_state(not_dual_stack, &id, state);
                    core_ctx
                        .with_bound_sockets_mut(|_core_ctx, sockets| op.apply(sockets))
                        .into_options()
                }
            },
        };
        DatagramBoundStateContext::<I, _, _>::with_transport_context(core_ctx, |core_ctx| {
            leave_all_joined_groups(core_ctx, bindings_ctx, ip_options.multicast_memberships)
        });
    });
    // Drop the (hopefully last) strong ID before unwrapping the primary
    // reference.
    core::mem::drop(id);
    BC::unwrap_or_notify_with_new_reference_notifier(
        primary,
        |ReferenceState { state: _, external_data }: ReferenceState<I, CC::WeakDeviceId, S>| {
            external_data
        },
    )
}

/// State associated with removing a socket.
///
/// Note that this type is generic over two `IpExt` parameters: `WireI` and
/// `SocketI`. This allows it to be used for both single-stack remove operations
/// (where `WireI` and `SocketI` are the same), as well as dual-stack remove
/// operations (where `WireI`, and `SocketI` may be different).
enum Remove<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    Listener {
        // The socket's address, stored as a concrete `ListenerIpAddr`.
        concrete_addr: ListenerAddr<
            ListenerIpAddr<WireI::Addr, <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
            D,
        >,
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
struct RemoveOperation<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    Remove<WireI, SocketI, D, S>,
);

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
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
            Remove::Listener { concrete_addr, ip_options: _, sharing: _, socket_id } => {
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
            Remove::Connected { concrete_addr, ip_options: _, sharing: _, socket_id } => {
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
impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> RemoveOperation<I, I, D, S> {
    /// Constructs the remove operation from existing socket state.
    fn new_from_state<BC, CC: NonDualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D>>(
        core_ctx: &mut CC,
        socket_id: &S::SocketId<I, D>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        let BoundSocketState { socket_type: state, original_bound_addr: _ } = state;
        RemoveOperation(match state {
            BoundSocketStateType::Listener {
                state: ListenerState { addr: ListenerAddr { ip, device }, ip_options },
                sharing,
            } => Remove::Listener {
                concrete_addr: ListenerAddr {
                    ip: core_ctx.nds_converter().convert(ip.clone()),
                    device: device.clone(),
                },
                ip_options: ip_options.clone(),
                sharing: sharing.clone(),
                socket_id: S::make_bound_socket_map_id(socket_id),
            },
            BoundSocketStateType::Connected { state, sharing } => {
                let ConnState {
                    addr,
                    socket: _,
                    ip_options,
                    clear_device_on_disconnect: _,
                    shutdown: _,
                    extra: _,
                } = core_ctx.nds_converter().convert(state);
                Remove::Connected {
                    concrete_addr: addr.clone(),
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
struct RemoveInfo<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    Remove<WireI, SocketI, D, S>,
);

impl<WireI: IpExt, SocketI: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    RemoveInfo<WireI, SocketI, D, S>
{
    fn into_options(self) -> IpOptions<SocketI, D, S> {
        let RemoveInfo(remove) = self;
        match remove {
            Remove::Listener { concrete_addr: _, ip_options, sharing: _, socket_id: _ } => {
                ip_options
            }
            Remove::Connected { concrete_addr: _, ip_options, sharing: _, socket_id: _ } => {
                ip_options
            }
        }
    }

    fn into_options_sharing_and_device(
        self,
    ) -> (IpOptions<SocketI, D, S>, S::SharingState, Option<D>) {
        let RemoveInfo(remove) = self;
        match remove {
            Remove::Listener {
                concrete_addr: ListenerAddr { ip: _, device },
                ip_options,
                sharing,
                socket_id: _,
            } => (ip_options, sharing, device),
            Remove::Connected {
                concrete_addr: ConnAddr { ip: _, device },
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
                ip_options: _,
                sharing,
                socket_id,
            } => {
                BoundStateHandler::<_, S, _>::try_insert_listener(
                    sockets, addr, identifier, device, sharing, socket_id,
                )
                .expect("reinserting just-removed listener failed");
            }
            Remove::Connected { concrete_addr, ip_options: _, sharing, socket_id } => {
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
enum DualStackRemove<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    CurrentStack(Remove<I, I, D, S>),
    OtherStack(Remove<I::OtherVersion, I, D, S>),
    ListenerBothStacks {
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        ip_options: IpOptions<I, D, S>,
        sharing: S::SharingState,
        socket_ids: PairedBoundSocketIds<I, D, S>,
    },
}

/// The yet-to-be-performed removal of a single-stack socket.
struct DualStackRemoveOperation<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    DualStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> DualStackRemoveOperation<I, D, S> {
    /// Constructs the removal operation from existing socket state.
    fn new_from_state<BC, CC: DualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D>>(
        core_ctx: &mut CC,
        socket_id: &S::SocketId<I, D>,
        state: &BoundSocketState<I, D, S>,
    ) -> Self {
        let BoundSocketState { socket_type: state, original_bound_addr: _ } = state;
        DualStackRemoveOperation(match state {
            BoundSocketStateType::Listener {
                state: ListenerState { addr, ip_options },
                sharing,
            } => {
                let ListenerAddr { ip, device } = addr.clone();
                match (
                    core_ctx.ds_converter().convert(ip),
                    core_ctx.dual_stack_enabled(&ip_options),
                ) {
                    // Dual-stack enabled, bound in both stacks.
                    (DualStackListenerIpAddr::BothStacks(identifier), true) => {
                        DualStackRemove::ListenerBothStacks {
                            identifier: identifier.clone(),
                            device,
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_ids: PairedBoundSocketIds {
                                this: S::make_bound_socket_map_id(socket_id),
                                other: core_ctx.to_other_bound_socket_id(socket_id),
                            },
                        }
                    }
                    // Bound in this stack, with/without dual-stack enabled.
                    (DualStackListenerIpAddr::ThisStack(addr), true | false) => {
                        DualStackRemove::CurrentStack(Remove::Listener {
                            concrete_addr: ListenerAddr { ip: addr, device },
                            ip_options: ip_options.clone(),
                            sharing: sharing.clone(),
                            socket_id: S::make_bound_socket_map_id(socket_id),
                        })
                    }
                    // Dual-stack enabled, bound only in the other stack.
                    (DualStackListenerIpAddr::OtherStack(addr), true) => {
                        DualStackRemove::OtherStack(Remove::Listener {
                            concrete_addr: ListenerAddr { ip: addr, device },
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
                match core_ctx.ds_converter().convert(state) {
                    DualStackConnState::ThisStack(state) => {
                        let ConnState {
                            addr,
                            socket: _,
                            ip_options,
                            clear_device_on_disconnect: _,
                            shutdown: _,
                            extra: _,
                        } = state;
                        DualStackRemove::CurrentStack(Remove::Connected {
                            concrete_addr: addr.clone(),
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
                        DualStackRemove::OtherStack(Remove::Connected {
                            concrete_addr: addr.clone(),
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
                ip_options,
                sharing,
                socket_ids,
            } => {
                PairedSocketMapMut::<_, _, S> { bound: sockets, other_bound: other_sockets }
                    .remove_listener(&DualStackUnspecifiedAddr, identifier, &device, &socket_ids);
                DualStackRemoveInfo(DualStackRemove::ListenerBothStacks {
                    identifier,
                    device,
                    ip_options,
                    sharing,
                    socket_ids,
                })
            }
        }
    }
}

/// Information for a recently-removed single-stack socket.
struct DualStackRemoveInfo<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
    DualStackRemove<I, D, S>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> DualStackRemoveInfo<I, D, S> {
    fn into_options(self) -> IpOptions<I, D, S> {
        let DualStackRemoveInfo(dual_stack_remove) = self;
        match dual_stack_remove {
            DualStackRemove::CurrentStack(remove) => RemoveInfo(remove).into_options(),
            DualStackRemove::OtherStack(remove) => RemoveInfo(remove).into_options(),
            DualStackRemove::ListenerBothStacks {
                identifier: _,
                device: _,
                ip_options,
                sharing: _,
                socket_ids: _,
            } => ip_options,
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

/// Returns the socket's bound/connection state information.
pub fn get_info<
    I: IpExt,
    S: DatagramSocketSpec,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> SocketInfo<I::Addr, CC::WeakDeviceId> {
    core_ctx.with_socket_state(id, |_core_ctx, state| state.to_socket_info())
}

/// Binds the socket to a local address and port.
pub fn listen<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    addr: Option<ZonedAddr<SpecifiedAddr<I::Addr>, CC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        listen_inner::<_, BC, _, S>(core_ctx, bindings_ctx, state, id, addr, local_id)
    })
}

/// Abstraction for operations over one or two demultiplexing maps.
trait BoundStateHandler<I: IpExt, S: DatagramSocketSpec, D: WeakDeviceIdentifier> {
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
impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
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

struct PairedSocketMapMut<'a, I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    bound: &'a mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
    other_bound: &'a mut BoundSocketMap<
        I::OtherVersion,
        D,
        S::AddrSpec,
        S::SocketMapSpec<I::OtherVersion, D>,
    >,
}

struct PairedBoundSocketIds<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    this: <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::BoundSocketId,
    other: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
        I::OtherVersion,
        D,
        S::AddrSpec,
    >>::BoundSocketId,
}

/// Implementation for a pair of demultiplexing maps for different IP versions.
impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
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
    D: WeakDeviceIdentifier,
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
    D: WeakDeviceIdentifier,
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
    D: WeakDeviceIdentifier,
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
            let (addr, device) = addr.resolve_addr_with_device(device.clone())?;

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
    state: &mut SocketState<I, CC::WeakDeviceId, S>,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    addr: Option<ZonedAddr<SpecifiedAddr<I::Addr>, CC::DeviceId>>,
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

    let UnboundSocketState { device, sharing, ip_options } = match state {
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
        (MaybeDualStack::DualStack(dual_stack), Some(addr)) => {
            match DualStackLocalIp::<I, _>::new(addr) {
                // `addr` can't be represented in the other stack.
                DualStackLocalIp::ThisStack(addr) => BoundOperation::OnlyCurrentStack(
                    MaybeDualStack::DualStack(dual_stack),
                    Some(addr),
                ),
                // There's a representation in the other stack, so use that if possible.
                DualStackLocalIp::OtherStack(addr) => {
                    match dual_stack.dual_stack_enabled(ip_options) {
                        true => BoundOperation::OnlyOtherStack(dual_stack, addr),
                        false => return Err(Either::Right(LocalAddressError::CannotBindToAddress)),
                    }
                }
            }
        }
        // No dual-stack support, so only bind on the current stack.
        (MaybeDualStack::NotDualStack(single_stack), None) => {
            BoundOperation::OnlyCurrentStack(MaybeDualStack::NotDualStack(single_stack), None)
        }
        // No dual-stack support, so check the address is allowed in the current
        // stack.
        (MaybeDualStack::NotDualStack(single_stack), Some(addr)) => {
            match DualStackLocalIp::<I, _>::new(addr) {
                // The address is only representable in the current stack.
                DualStackLocalIp::ThisStack(addr) => BoundOperation::OnlyCurrentStack(
                    MaybeDualStack::NotDualStack(single_stack),
                    Some(addr),
                ),
                // The address has a representation in the other stack but there's
                // no dual-stack support!
                DualStackLocalIp::OtherStack(_addr) => {
                    let _: Option<ZonedAddr<SocketIpAddr<<I::OtherVersion as Ip>::Addr>, _>> =
                        _addr;
                    return Err(Either::Right(LocalAddressError::CannotBindToAddress));
                }
            }
        }
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
        let weak_device = device.map(|d| d.as_weak().into_owned());

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
            let converter = match either_dual_stack {
                MaybeDualStack::DualStack(ds) => MaybeDualStack::DualStack(ds.ds_converter()),
                MaybeDualStack::NotDualStack(nds) => {
                    MaybeDualStack::NotDualStack(nds.nds_converter())
                }
            };
            core_ctx
                .with_bound_sockets_mut(|core_ctx, bound| {
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
                        ip: core_ctx.ds_converter().convert_back(
                            DualStackListenerIpAddr::OtherStack(ListenerIpAddr {
                                addr,
                                identifier,
                            }),
                        ),
                        device,
                    }
                })
        }
        BoundOperation::DualStackAnyAddr(core_ctx) => {
            let ids = PairedBoundSocketIds {
                this: S::make_bound_socket_map_id(id),
                other: core_ctx.to_other_bound_socket_id(id),
            };
            core_ctx
                .with_both_bound_sockets_mut(|core_ctx, bound, other_bound| {
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
                    let (_addr, device, identifier) =
                        try_pick_bound_address::<I, _, _, _>(None, device, core_ctx, identifier)?;
                    let weak_device = device.map(|d| d.as_weak().into_owned());

                    BoundStateHandler::<_, S, _>::try_insert_listener(
                        &mut bound_pair,
                        DualStackUnspecifiedAddr,
                        identifier,
                        weak_device.clone(),
                        sharing,
                        ids,
                    )
                    .map(|()| (identifier, weak_device))
                })
                .map(|(identifier, device)| ListenerAddr {
                    ip: core_ctx
                        .ds_converter()
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
    *state = SocketState::Bound(BoundSocketState {
        socket_type: BoundSocketStateType::Listener {
            state: ListenerState {
                // TODO(https://fxbug.dev/42082099): Remove this clone().
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
struct ConnectParameters<
    WireI: IpExt,
    SocketI: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
> {
    local_ip: Option<SocketIpAddr<WireI::Addr>>,
    local_port: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
    remote_ip: ZonedAddr<SocketIpAddr<WireI::Addr>, D::Strong>,
    remote_port: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: Option<D>,
    sharing: S::SharingState,
    ip_options: IpOptions<SocketI, D, S>,
    socket_options: DatagramSocketOptions<WireI, D>,
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
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
    R,
    BC: RngContext,
    CC: IpSocketHandler<WireI, BC, WeakDeviceId = D, DeviceId = D::Strong>,
>(
    connect_params: ConnectParameters<WireI, SocketI, D, S>,
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    sockets: &mut BoundSocketMap<WireI, D, S::AddrSpec, S::SocketMapSpec<WireI, D>>,
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
        socket_options,
        socket_id,
        original_shutdown,
        extra,
    } = connect_params;

    // Select multicast device if we are connecting to a multicast address.
    let device = device.or_else(|| {
        remote_ip
            .addr()
            .addr()
            .is_multicast()
            .then(|| socket_options.multicast_interface.clone())
            .flatten()
    });

    let (remote_ip, socket_device) = remote_ip.resolve_addr_with_device(device.clone())?;

    let clear_device_on_disconnect = device.is_none() && socket_device.is_some();

    let ip_sock = IpSocketHandler::<WireI, _>::new_ip_socket(
        core_ctx,
        bindings_ctx,
        socket_device.as_ref().map(|d| d.as_ref()),
        local_ip.map(SocketIpAddr::into),
        remote_ip,
        S::ip_proto::<WireI>(),
    )?;

    let local_port = match local_port {
        Some(id) => id.clone(),
        None => S::try_alloc_local_id(
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
struct SingleStackConnectOperation<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec> {
    params: ConnectParameters<I, I, D, S>,
    remove_op: Option<SingleStackRemoveOperation<I, D, S>>,
    sharing: S::SharingState,
}

impl<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    SingleStackConnectOperation<I, D, S>
{
    /// Constructs the connect operation from existing socket state.
    fn new_from_state<
        BC,
        CC: NonDualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D, DeviceId = D::Strong>,
    >(
        core_ctx: &mut CC,
        socket_id: &S::SocketId<I, D>,
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
                        socket_options: ip_options.socket_options.clone(),
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
                    SingleStackRemoveOperation::new_from_state(core_ctx, socket_id, state);
                let BoundSocketState { socket_type, original_bound_addr: _ } = state;
                match socket_type {
                    BoundSocketStateType::Listener {
                        state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                        sharing,
                    } => {
                        let ListenerIpAddr { addr, identifier } =
                            core_ctx.nds_converter().convert(ip);
                        SingleStackConnectOperation {
                            params: ConnectParameters {
                                local_ip: addr.clone(),
                                local_port: Some(*identifier),
                                remote_ip,
                                remote_port,
                                device: device.clone(),
                                sharing: sharing.clone(),
                                ip_options: ip_options.clone(),
                                socket_options: ip_options.socket_options.clone(),
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
                        } = core_ctx.nds_converter().convert(state);
                        SingleStackConnectOperation {
                            params: ConnectParameters {
                                local_ip: Some(local_ip.clone()),
                                local_port: Some(*local_id),
                                remote_ip,
                                remote_port,
                                device: device.clone(),
                                sharing: sharing.clone(),
                                ip_options: ip_options.clone(),
                                socket_options: ip_options.socket_options.clone(),
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
    fn apply<BC: RngContext, CC: IpSocketHandler<I, BC, WeakDeviceId = D, DeviceId = D::Strong>>(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        socket_map: &mut BoundSocketMap<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>,
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
        let conn_state =
            connect_inner(params, core_ctx, bindings_ctx, socket_map, remove_fn, reinsert_fn)?;
        Ok((conn_state, sharing))
    }
}

/// State required to perform dual-stack connection of a socket.
struct DualStackConnectOperation<I: DualStackIpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
{
    params: EitherStack<ConnectParameters<I, I, D, S>, ConnectParameters<I::OtherVersion, I, D, S>>,
    remove_op: Option<DualStackRemoveOperation<I, D, S>>,
    sharing: S::SharingState,
}

impl<I: DualStackIpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>
    DualStackConnectOperation<I, D, S>
{
    /// Constructs the connect operation from existing socket state.
    fn new_from_state<
        BC,
        CC: DualStackDatagramBoundStateContext<I, BC, S, WeakDeviceId = D, DeviceId = D::Strong>,
    >(
        core_ctx: &mut CC,
        socket_id: &S::SocketId<I, D>,
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
                            socket_options: ip_options.socket_options.clone(),
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
                            socket_options: core_ctx.to_other_socket_options(ip_options).clone(),
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
                    DualStackRemoveOperation::new_from_state(core_ctx, socket_id, state);

                let BoundSocketState { socket_type, original_bound_addr: _ } = state;
                match socket_type {
                    BoundSocketStateType::Listener {
                        state: ListenerState { ip_options, addr: ListenerAddr { ip, device } },
                        sharing,
                    } => {
                        match (remote_ip, core_ctx.ds_converter().convert(ip)) {
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
                                    socket_options: ip_options.socket_options.clone(),
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
                                    socket_options: ip_options.socket_options.clone(),
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
                                    socket_options: core_ctx
                                        .to_other_socket_options(ip_options)
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
                                    socket_options: core_ctx
                                        .to_other_socket_options(ip_options)
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
                        match (remote_ip, core_ctx.ds_converter().convert(state)) {
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
                                    socket_options: ip_options.socket_options.clone(),
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
                                    socket_options: core_ctx
                                        .to_other_socket_options(ip_options)
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
        BC: RngContext,
        CC: IpSocketHandler<I, BC, WeakDeviceId = D, DeviceId = D::Strong>
            + IpSocketHandler<I::OtherVersion, BC, WeakDeviceId = D, DeviceId = D::Strong>,
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
                connect_inner(params, core_ctx, bindings_ctx, socket_map, remove_fn, reinsert_fn)
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
                    remove_fn,
                    reinsert_fn,
                )
                .map(DualStackConnState::OtherStack)
            }
        }?;
        Ok((conn_state, sharing))
    }
}

/// Connects the datagram socket.
pub fn connect<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    remote_ip: Option<ZonedAddr<SpecifiedAddr<I::Addr>, CC::DeviceId>>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    extra: S::ConnStateExtra,
) -> Result<(), ConnectError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let (conn_state, sharing) = match (
            core_ctx.dual_stack_context(),
            DualStackRemoteIp::<I, _>::new(remote_ip.clone()),
        ) {
            (MaybeDualStack::DualStack(ds), remote_ip) => {
                let connect_op = DualStackConnectOperation::new_from_state(
                    ds, id, state, remote_ip, remote_id, extra,
                )?;
                let converter = ds.ds_converter();
                let (conn_state, sharing) =
                    ds.with_both_bound_sockets_mut(|core_ctx, bound, other_bound| {
                        connect_op.apply(core_ctx, bindings_ctx, bound, other_bound)
                    })?;
                Ok((converter.convert_back(conn_state), sharing))
            }
            (MaybeDualStack::NotDualStack(nds), DualStackRemoteIp::ThisStack(remote_ip)) => {
                let connect_op = SingleStackConnectOperation::new_from_state(
                    nds, id, state, remote_ip, remote_id, extra,
                );
                let converter = nds.nds_converter();
                let (conn_state, sharing) =
                    core_ctx.with_bound_sockets_mut(|core_ctx, bound| {
                        connect_op.apply(core_ctx, bindings_ctx, bound)
                    })?;
                Ok((converter.convert_back(conn_state), sharing))
            }
            (MaybeDualStack::NotDualStack(_), DualStackRemoteIp::OtherStack(_)) => {
                Err(ConnectError::RemoteUnexpectedlyMapped)
            }
        }?;
        let original_bound_addr = match state {
            SocketState::Unbound(_) => None,
            SocketState::Bound(BoundSocketState { socket_type: _, original_bound_addr }) => {
                original_bound_addr.clone()
            }
        };
        *state = SocketState::Bound(BoundSocketState {
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

/// Disconnects a connected socket.
pub fn disconnect_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> Result<(), ExpectedConnError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let inner_state = match state {
            SocketState::Unbound(_) => return Err(ExpectedConnError),
            SocketState::Bound(state) => state,
        };
        let BoundSocketState { socket_type, original_bound_addr } = inner_state;
        let conn_state = match socket_type {
            BoundSocketStateType::Listener { state: _, sharing: _ } => {
                return Err(ExpectedConnError)
            }
            BoundSocketStateType::Connected { state, sharing: _ } => state,
        };

        let clear_device_on_disconnect = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(dual_stack) => match dual_stack
                .ds_converter()
                .convert(conn_state)
            {
                DualStackConnState::ThisStack(conn_state) => conn_state.clear_device_on_disconnect,
                DualStackConnState::OtherStack(conn_state) => conn_state.clear_device_on_disconnect,
            },
            MaybeDualStack::NotDualStack(not_dual_stack) => {
                not_dual_stack.nds_converter().convert(conn_state).clear_device_on_disconnect
            }
        };

        *state = match original_bound_addr {
            None => SocketState::Unbound(disconnect_to_unbound(
                core_ctx,
                id,
                clear_device_on_disconnect,
                inner_state,
            )),
            Some(original_bound_addr) => SocketState::Bound(disconnect_to_listener(
                core_ctx,
                id,
                original_bound_addr.clone(),
                clear_device_on_disconnect,
                inner_state,
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
    id: &S::SocketId<I, CC::WeakDeviceId>,
    clear_device_on_disconnect: bool,
    socket_state: &BoundSocketState<I, CC::WeakDeviceId, S>,
) -> UnboundSocketState<I, CC::WeakDeviceId, S> {
    let (ip_options, sharing, mut device) = match core_ctx.dual_stack_context() {
        MaybeDualStack::NotDualStack(nds) => {
            let remove_op = SingleStackRemoveOperation::new_from_state(nds, id, socket_state);
            let info = core_ctx.with_bound_sockets_mut(|_core_ctx, bound| remove_op.apply(bound));
            info.into_options_sharing_and_device()
        }
        MaybeDualStack::DualStack(ds) => {
            let remove_op = DualStackRemoveOperation::new_from_state(ds, id, socket_state);
            let info = ds.with_both_bound_sockets_mut(|_core_ctx, bound, other_bound| {
                remove_op.apply(bound, other_bound)
            });
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
    id: &S::SocketId<I, CC::WeakDeviceId>,
    listener_ip: S::ListenerIpAddr<I>,
    clear_device_on_disconnect: bool,
    socket_state: &BoundSocketState<I, CC::WeakDeviceId, S>,
) -> BoundSocketState<I, CC::WeakDeviceId, S> {
    let (ip_options, sharing, device) = match core_ctx.dual_stack_context() {
        MaybeDualStack::NotDualStack(nds) => {
            let ListenerIpAddr { addr, identifier } =
                nds.nds_converter().convert(listener_ip.clone());
            let remove_op = SingleStackRemoveOperation::new_from_state(nds, id, socket_state);
            core_ctx.with_bound_sockets_mut(|_core_ctx, bound| {
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
            let remove_op = DualStackRemoveOperation::new_from_state(ds, id, socket_state);
            let other_id = ds.to_other_bound_socket_id(id);
            let id = S::make_bound_socket_map_id(id);
            let converter = ds.ds_converter();
            ds.with_both_bound_sockets_mut(|_core_ctx, bound, other_bound| {
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
                    DualStackListenerIpAddr::OtherStack(ListenerIpAddr { addr, identifier }) => {
                        BoundStateHandler::<_, S, _>::try_insert_listener(
                            other_bound,
                            addr,
                            identifier,
                            device.clone(),
                            sharing.clone(),
                            other_id,
                        )
                    }
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
            })
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

/// Shuts down the socket.
///
/// `which` determines the shutdown type.
pub fn shutdown_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let state = match state {
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
        let (shutdown_send, shutdown_receive) = which.to_send_receive();
        let Shutdown { send, receive } = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(ds) => ds.ds_converter().convert(state).as_mut(),
            MaybeDualStack::NotDualStack(nds) => nds.nds_converter().convert(state).as_mut(),
        };
        *send |= shutdown_send;
        *receive |= shutdown_receive;
        Ok(())
    })
}

/// Returns the socket's shutdown state.
pub fn get_shutdown_connected<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> Option<ShutdownType> {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let state = match state {
            SocketState::Unbound(_) => return None,
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                match socket_type {
                    BoundSocketStateType::Listener { state: _, sharing: _ } => return None,
                    BoundSocketStateType::Connected { state, sharing: _ } => state,
                }
            }
        };
        let Shutdown { send, receive } = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(ds) => ds.ds_converter().convert(state).as_ref(),
            MaybeDualStack::NotDualStack(nds) => nds.nds_converter().convert(state).as_ref(),
        };
        ShutdownType::from_send_receive(*send, *receive)
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

/// Sends data over a connected datagram socket.
pub fn send_conn<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    body: B,
) -> Result<(), SendError<S::SerializeError>> {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let state = match state {
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

        struct SendParams<
            'a,
            I: IpExt,
            S: DatagramSocketSpec,
            D: WeakDeviceIdentifier,
            O: SendOptions<I>,
        > {
            socket: &'a IpSock<I, D>,
            ip: &'a ConnIpAddr<
                I::Addr,
                <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
            options: &'a O,
        }

        enum Operation<
            'a,
            I: DualStackIpExt,
            S: DatagramSocketSpec,
            D: WeakDeviceIdentifier,
            BC: DatagramStateBindingsContext<I, S>,
            DualStackSC: DualStackDatagramBoundStateContext<I, BC, S>,
            CC: DatagramBoundStateContext<I, BC, S>,
            O: SendOptions<I>,
            OtherO: SendOptions<I::OtherVersion>,
        > {
            SendToThisStack((SendParams<'a, I, S, D, O>, &'a mut CC)),
            SendToOtherStack((SendParams<'a, I::OtherVersion, S, D, OtherO>, &'a mut DualStackSC)),
            // Allow `Operation` to be generic over `B` and `C` so that they can
            // be used in trait bounds for `DualStackSC` and `SC`.
            _Phantom((Never, PhantomData<BC>)),
        }

        let (shutdown, operation) = match core_ctx.dual_stack_context() {
            MaybeDualStack::DualStack(dual_stack) => match dual_stack.ds_converter().convert(state)
            {
                DualStackConnState::ThisStack(ConnState {
                    socket,
                    ip_options,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                }) => (
                    shutdown,
                    Operation::SendToThisStack((
                        SendParams { socket, ip, options: &ip_options.socket_options },
                        core_ctx,
                    )),
                ),
                DualStackConnState::OtherStack(ConnState {
                    socket,
                    ip_options,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                }) => (
                    shutdown,
                    Operation::SendToOtherStack((
                        SendParams {
                            socket,
                            ip,
                            options: dual_stack.to_other_socket_options(ip_options),
                        },
                        dual_stack,
                    )),
                ),
            },
            MaybeDualStack::NotDualStack(not_dual_stack) => {
                let ConnState {
                    socket,
                    ip_options,
                    clear_device_on_disconnect: _,
                    shutdown,
                    addr: ConnAddr { ip, device: _ },
                    extra: _,
                } = not_dual_stack.nds_converter().convert(state);
                (
                    shutdown,
                    Operation::SendToThisStack((
                        SendParams { socket, ip, options: &ip_options.socket_options },
                        core_ctx,
                    )),
                )
            }
        };

        let Shutdown { send: shutdown_send, receive: _ } = shutdown;
        if *shutdown_send {
            return Err(SendError::NotWriteable);
        }

        match operation {
            Operation::SendToThisStack((SendParams { socket, ip, options }, core_ctx)) => {
                let packet =
                    S::make_packet::<I, _>(body, &ip).map_err(SendError::SerializeError)?;
                DatagramBoundStateContext::with_transport_context(core_ctx, |core_ctx| {
                    core_ctx
                        .send_ip_packet(bindings_ctx, &socket, packet, None, options)
                        .map_err(|send_error| SendError::IpSock(send_error))
                })
            }
            Operation::SendToOtherStack((SendParams { socket, ip, options }, dual_stack)) => {
                let packet = S::make_packet::<I::OtherVersion, _>(body, &ip)
                    .map_err(SendError::SerializeError)?;
                DualStackDatagramBoundStateContext::with_transport_context::<_, _>(
                    dual_stack,
                    |core_ctx| {
                        core_ctx
                            .send_ip_packet(bindings_ctx, &socket, packet, None, options)
                            .map_err(|send_error| SendError::IpSock(send_error))
                    },
                )
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

/// Sends a datagram to the provided remote node.
pub fn send_to<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    remote_ip: Option<ZonedAddr<SpecifiedAddr<I::Addr>, CC::DeviceId>>,
    remote_identifier: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    body: B,
) -> Result<(), Either<LocalAddressError, SendToError<S::SerializeError>>> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        match listen_inner(core_ctx, bindings_ctx, state, id, None, None) {
            Ok(()) | Err(Either::Left(ExpectedUnboundError)) => (),
            Err(Either::Right(e)) => return Err(Either::Left(e)),
        };
        let state = match state {
            SocketState::Unbound(_) => panic!("expected bound socket"),
            SocketState::Bound(BoundSocketState { socket_type: state, original_bound_addr: _ }) => {
                state
            }
        };

        enum Operation<
            'a,
            I: DualStackIpExt,
            S: DatagramSocketSpec,
            D: WeakDeviceIdentifier,
            BC: DatagramStateBindingsContext<I, S>,
            DualStackSC: DualStackDatagramBoundStateContext<I, BC, S>,
            CC: DatagramBoundStateContext<I, BC, S>,
        > {
            SendToThisStack((SendOneshotParameters<'a, I, S, D>, &'a mut CC)),

            SendToOtherStack(
                (SendOneshotParameters<'a, I::OtherVersion, S, D>, &'a mut DualStackSC),
            ),
            // Allow `Operation` to be generic over `B` and `C` so that they can
            // be used in trait bounds for `DualStackSC` and `SC`.
            _Phantom((Never, PhantomData<BC>)),
        }

        let (operation, shutdown) = match (
            core_ctx.dual_stack_context(),
            DualStackRemoteIp::<I, _>::new(remote_ip.clone()),
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
                            nds.nds_converter().convert(ip.clone());
                        (
                            Operation::SendToThisStack((
                                SendOneshotParameters {
                                    local_ip: addr,
                                    local_id: identifier,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device,
                                    socket_options: &ip_options.socket_options,
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
                        } = nds.nds_converter().convert(state);
                        (
                            Operation::SendToThisStack((
                                SendOneshotParameters {
                                    local_ip: Some(*local_ip),
                                    local_id: *local_id,
                                    remote_ip,
                                    remote_id: remote_identifier,
                                    device,
                                    socket_options: &ip_options.socket_options,
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
                } => match (ds.ds_converter().convert(ip), remote_ip) {
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
                                device,
                                socket_options: &ip_options.socket_options,
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
                                device,
                                socket_options: &ip_options.socket_options,
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
                                device,
                                socket_options: ds.to_other_socket_options(ip_options),
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
                                device,
                                socket_options: ds.to_other_socket_options(ip_options),
                            },
                            ds,
                        )),
                        None,
                    ),
                },
                BoundSocketStateType::Connected { state, sharing: _ } => {
                    match (ds.ds_converter().convert(state), remote_ip) {
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
                                    device,
                                    socket_options: &ip_options.socket_options,
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
                                    device,
                                    socket_options: ds.to_other_socket_options(ip_options),
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
                    send_oneshot::<_, S, _, _, _>(core_ctx, bindings_ctx, params, body)
                })
            }
            Operation::SendToOtherStack((params, core_ctx)) => {
                DualStackDatagramBoundStateContext::with_transport_context::<_, _>(
                    core_ctx,
                    |core_ctx| send_oneshot::<_, S, _, _, _>(core_ctx, bindings_ctx, params, body),
                )
            }
            Operation::_Phantom((never, _)) => match never {},
        }
        .map_err(Either::Right)
    })
}

struct SendOneshotParameters<'a, I: IpExt, S: DatagramSocketSpec, D: WeakDeviceIdentifier> {
    local_ip: Option<SocketIpAddr<I::Addr>>,
    local_id: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    remote_ip: ZonedAddr<SocketIpAddr<I::Addr>, D::Strong>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: &'a Option<D>,
    socket_options: &'a DatagramSocketOptions<I, D>,
}

fn send_oneshot<I: IpExt, S: DatagramSocketSpec, CC: IpSocketHandler<I, BC>, BC, B: BufferMut>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    SendOneshotParameters {
        local_ip,
        local_id,
        remote_ip,
        remote_id,
        device,
        socket_options,
    }: SendOneshotParameters<'_, I, S, CC::WeakDeviceId>,
    body: B,
) -> Result<(), SendToError<S::SerializeError>> {
    let device = device.clone().or_else(|| {
        remote_ip
            .addr()
            .addr()
            .is_multicast()
            .then(|| socket_options.multicast_interface.clone())
            .flatten()
    });
    let (remote_ip, device) = match remote_ip.resolve_addr_with_device(device) {
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
            socket_options,
            |local_ip| {
                S::make_packet::<I, _>(
                    body,
                    &ConnIpAddr { local: (local_ip, local_id), remote: (remote_ip, remote_id) },
                )
            },
            None,
        )
        .map_err(|err| match err {
            SendOneShotIpPacketError::CreateAndSendError { err } => SendToError::CreateAndSend(err),
            SendOneShotIpPacketError::SerializeError(err) => SendToError::SerializeError(err),
        })
}

/// Mutably holds the original state of a bound socket required to update the
/// bound device.
enum SetBoundDeviceParameters<
    'a,
    WireI: IpExt,
    SocketI: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
> {
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
    D: WeakDeviceIdentifier,
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
    let device_update = SocketDeviceUpdate {
        local_ip: local_ip.map(AsRef::<SpecifiedAddr<WireI::Addr>>::as_ref),
        remote_ip: remote_ip.map(AsRef::<SpecifiedAddr<WireI::Addr>>::as_ref),
        old_device,
    };
    match device_update.check_update(new_device) {
        Ok(()) => (),
        Err(SocketDeviceUpdateNotAllowedError) => {
            return Err(SocketError::Local(LocalAddressError::Zone(
                ZonedAddressError::DeviceZoneMismatch,
            )));
        }
    };

    match params {
        SetBoundDeviceParameters::Listener { ip, device } => {
            let new_device = new_device.map(|d| d.downgrade());
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
            let new_socket = core_ctx
                .new_ip_socket(
                    bindings_ctx,
                    new_device.map(EitherDeviceId::Strong),
                    Some(local_ip.clone()),
                    remote_ip.clone(),
                    socket.proto(),
                )
                .map_err(|_: IpSockCreationError| {
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
fn set_bound_device_listener_both_stacks<
    'a,
    I: IpExt,
    D: WeakDeviceIdentifier,
    S: DatagramSocketSpec,
>(
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
    fn try_update_entry<I: IpExt, D: WeakDeviceIdentifier, S: DatagramSocketSpec>(
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

/// Sets the socket's bound device to `new_device`.
pub fn set_device<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    new_device: Option<&CC::DeviceId>,
) -> Result<(), SocketError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        match state {
            SocketState::Unbound(state) => {
                let UnboundSocketState { ref mut device, sharing: _, ip_options: _ } = state;
                *device = new_device.map(|d| d.downgrade());
                Ok(())
            }
            SocketState::Bound(BoundSocketState { socket_type, original_bound_addr: _ }) => {
                // Information about the set-device operation for the given
                // socket.
                enum Operation<
                    'a,
                    I: IpExt,
                    D: WeakDeviceIdentifier,
                    S: DatagramSocketSpec,
                    CC,
                    DualStackSC,
                > {
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
                        } => match ds.ds_converter().convert(ip) {
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
                            match ds.ds_converter().convert(state) {
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
                                ip: nds.nds_converter().convert(ip),
                                device,
                            },
                            core_ctx,
                        },
                        BoundSocketStateType::Connected { state, sharing: _ } => {
                            Operation::ThisStack {
                                params: SetBoundDeviceParameters::Connected(
                                    nds.nds_converter().convert(state),
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
                            |core_ctx, bound| {
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
                            this: S::make_bound_socket_map_id(id),
                            other: core_ctx.to_other_bound_socket_id(id),
                        };
                        core_ctx.with_both_bound_sockets_mut(|_core_ctx, bound, other_bound| {
                            set_bound_device_listener_both_stacks(
                                device,
                                identifier,
                                PairedSocketMapMut { bound, other_bound },
                                socket_id,
                                new_device.map(|d| d.downgrade()),
                            )
                        })
                    }
                }
            }
        }
    })
}

/// Returns the bound device for the socket.
pub fn get_bound_device<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> Option<CC::WeakDeviceId> {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (_, device): (&IpOptions<_, _, _>, _) = get_options_device(core_ctx, state);
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
    remote_addr: MulticastAddr<A>,
    source_addr: Option<SpecifiedAddr<A>>,
) -> Result<CC::DeviceId, SetMulticastMembershipError>
where
    A::Version: IpExt,
{
    core_ctx.with_transport_context(|core_ctx| match source_addr {
        Some(source_addr) => {
            let mut devices = TransportIpContext::<A::Version, _>::get_devices_with_assigned_addr(
                core_ctx,
                source_addr,
            );
            if let Some(d) = devices.next() {
                if devices.next() == None {
                    return Ok(d);
                }
            }
            Err(SetMulticastMembershipError::NoDeviceAvailable)
        }
        None => {
            let device = MulticastMembershipHandler::select_device_for_multicast_group(
                core_ctx,
                remote_addr,
            )
            .map_err(|e| match e {
                ResolveRouteError::NoSrcAddr | ResolveRouteError::Unreachable => {
                    SetMulticastMembershipError::NoDeviceAvailable
                }
            })?;
            Ok(device)
        }
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
pub fn set_multicast_membership<
    I: IpExt,
    BC: DatagramStateBindingsContext<I, S>,
    CC: DatagramStateContext<I, BC, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, CC::DeviceId>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let (_, bound_device): (&IpOptions<_, _, _>, _) = get_options_device(core_ctx, state);

        let interface = match interface {
            MulticastMembershipInterfaceSelector::Specified(selector) => match selector {
                MulticastInterfaceSelector::Interface(device) => {
                    if bound_device.as_ref().is_some_and(|d| d != &device) {
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

        let ip_options = get_options_mut(core_ctx, state);

        let Some(strong_interface) = interface.as_strong() else {
            return Err(SetMulticastMembershipError::DeviceDoesNotExist);
        };

        let change = ip_options
            .multicast_memberships
            .apply_membership_change(multicast_group, &interface.as_weak(), want_membership)
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
    D: WeakDeviceIdentifier,
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

/// Gets the [`IpOptions`] and bound device for the socket.
pub fn get_options_device<
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
                            match dual_stack.ds_converter().convert(state) {
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
                                not_dual_stack.nds_converter().convert(state),
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
    state: &'a mut SocketState<I, CC::WeakDeviceId, S>,
) -> &'a mut IpOptions<I, CC::WeakDeviceId, S> {
    match state {
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
                            match dual_stack.ds_converter().convert(state) {
                                DualStackConnState::ThisStack(state) => state.as_mut(),
                                DualStackConnState::OtherStack(state) => {
                                    dual_stack.assert_dual_stack_enabled(state);
                                    state.as_mut()
                                }
                            }
                        }
                        MaybeDualStack::NotDualStack(not_dual_stack) => {
                            not_dual_stack.nds_converter().convert(state).as_mut()
                        }
                    }
                }
            }
        }
    }
}

/// Updates the socket's IP hop limits.
pub fn update_ip_hop_limit<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    update: impl FnOnce(&mut SocketHopLimits<I>),
) {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let options = get_options_mut(core_ctx, state);

        update(&mut options.socket_options.hop_limits)
    })
}

/// Returns the socket's IP hop limits.
pub fn get_ip_hop_limits<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> HopLimits {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, device) = get_options_device(core_ctx, state);
        let device = device.as_ref().and_then(|d| d.upgrade());
        DatagramBoundStateContext::<I, _, _>::with_transport_context(core_ctx, |core_ctx| {
            options.socket_options.hop_limits.get_limits_with_defaults(
                &TransportIpContext::<I, _>::get_default_hop_limits(core_ctx, device.as_ref()),
            )
        })
    })
}

/// Calls the callback with mutable access to [`S::OtherStackIpOptions<I, D>`].
///
/// If the socket is bound, the callback is not called, and instead an
/// `ExpectedUnboundError` is returned.
pub fn with_other_stack_ip_options_mut_if_unbound<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    cb: impl FnOnce(&mut S::OtherStackIpOptions<I, CC::WeakDeviceId>) -> R,
) -> Result<R, ExpectedUnboundError> {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let is_unbound = match state {
            SocketState::Unbound(_) => true,
            SocketState::Bound(_) => false,
        };
        if is_unbound {
            let options = get_options_mut(core_ctx, state);
            Ok(cb(&mut options.other_stack))
        } else {
            Err(ExpectedUnboundError)
        }
    })
}

/// Calls the callback with mutable access to [`S::OtherStackIpOptions<I, D>`].
pub fn with_other_stack_ip_options_mut<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    cb: impl FnOnce(&mut S::OtherStackIpOptions<I, CC::WeakDeviceId>) -> R,
) -> R {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        let options = get_options_mut(core_ctx, state);
        cb(&mut options.other_stack)
    })
}

/// Calls the callback with access to [`S::OtherStackIpOptions<I, D>`].
pub fn with_other_stack_ip_options<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    cb: impl FnOnce(&S::OtherStackIpOptions<I, CC::WeakDeviceId>) -> R,
) -> R {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, _device) = get_options_device(core_ctx, state);

        cb(&options.other_stack)
    })
}

/// Calls the callback with access to [`S::OtherStackIpOptions<I, D>`], and the
/// default [`HopLimits`] for `I::OtherVersion`.
///
/// If dualstack operations are not supported, the callback is not called, and
/// instead `NotDualStackCapableError` is returned.
pub fn with_other_stack_ip_options_and_default_hop_limits<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
    R,
>(
    core_ctx: &mut CC,
    _bindings_ctx: &BC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    cb: impl FnOnce(&S::OtherStackIpOptions<I, CC::WeakDeviceId>, HopLimits) -> R,
) -> Result<R, NotDualStackCapableError> {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, device) = get_options_device(core_ctx, state);
        let device = device.as_ref().and_then(|d| d.upgrade());
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

/// Updates the socket's sharing state to `new_sharing`.
pub fn update_sharing<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec<SharingState: Clone>,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    new_sharing: S::SharingState,
) -> Result<(), ExpectedUnboundError> {
    core_ctx.with_socket_state_mut(id, |_core_ctx, state| {
        let state = match state {
            SocketState::Bound(_) => return Err(ExpectedUnboundError),
            SocketState::Unbound(state) => state,
        };

        let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
        *sharing = new_sharing;
        Ok(())
    })
}

/// Returns the socket's sharing state.
pub fn get_sharing<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec<SharingState: Clone>,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> S::SharingState {
    core_ctx.with_socket_state(id, |_core_ctx, state| {
        match state {
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

/// Sets the IP transparent option.
pub fn set_ip_transparent<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    value: bool,
) {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        get_options_mut(core_ctx, state).transparent = value;
    })
}

/// Returns the IP transparent option.
pub fn get_ip_transparent<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> bool {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, _device) = get_options_device(core_ctx, state);
        options.transparent
    })
}

/// Sets the multicast interface for outgoing multicast interface.
pub fn set_multicast_interface<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    value: Option<&CC::DeviceId>,
) {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        get_options_mut(core_ctx, state).socket_options.multicast_interface =
            value.map(|v| v.downgrade());
    })
}

/// Returns the configured multicast interface.
pub fn get_multicast_interface<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> Option<CC::WeakDeviceId> {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, _device) = get_options_device(core_ctx, state);
        options.socket_options.multicast_interface.clone()
    })
}

/// Sets the multicast loopback flag.
pub fn set_multicast_loop<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
    value: bool,
) {
    core_ctx.with_socket_state_mut(id, |core_ctx, state| {
        get_options_mut(core_ctx, state).socket_options.multicast_loop = value;
    })
}

/// Returns the multicast loopback flag.
pub fn get_multicast_loop<
    I: IpExt,
    CC: DatagramStateContext<I, BC, S>,
    BC: DatagramStateBindingsContext<I, S>,
    S: DatagramSocketSpec,
>(
    core_ctx: &mut CC,
    id: &S::SocketId<I, CC::WeakDeviceId>,
) -> bool {
    core_ctx.with_socket_state(id, |core_ctx, state| {
        let (options, _device) = get_options_device(core_ctx, state);
        options.socket_options.multicast_loop
    })
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use alloc::vec;
    use net_types::{ip::IpAddr, Witness};
    use netstack3_base::{
        testutil::{FakeStrongDeviceId, TestIpExt},
        CtxPair,
    };

    use crate::internal::socket::testutil::FakeDeviceConfig;

    /// Helper function to ensure the Fake CoreCtx and BindingsCtx are setup
    /// with [`FakeDeviceConfig`] (one per provided device), with remote/local
    /// IPs that support a connection to the given remote_ip.
    pub fn setup_fake_ctx_with_dualstack_conn_addrs<CC, BC: Default, D: FakeStrongDeviceId>(
        local_ip: IpAddr,
        remote_ip: SpecifiedAddr<IpAddr>,
        devices: impl IntoIterator<Item = D>,
        core_ctx_builder: impl FnOnce(Vec<FakeDeviceConfig<D, SpecifiedAddr<IpAddr>>>) -> CC,
    ) -> CtxPair<CC, BC> {
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
        // `TEST_ADDRS`. This ensures we always instantiate the
        // FakeDeviceConfig below with at least one local_ip, which is
        // required for connect operations to succeed.
        let local_ip = SpecifiedAddr::new(local_ip).unwrap_or_else(|| match remote_ip {
            IpAddr::V4(_) => Ipv4::TEST_ADDRS.local_ip.into(),
            IpAddr::V6(_) => Ipv6::TEST_ADDRS.local_ip.into(),
        });
        // If the given remote_ip is unspecified, we won't be able to
        // connect; abort the test.
        let remote_ip = SpecifiedAddr::new(remote_ip).expect("remote-ip should be specified");
        CtxPair::with_core_ctx(core_ctx_builder(
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
    use core::convert::Infallible as Never;

    use alloc::vec;
    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::{
        ip::{Ipv4Addr, Ipv6Addr},
        Witness,
    };
    use netstack3_base::{
        socket::{
            AddrVec, Bound, IncompatibleError, ListenerAddrInfo, RemoveResult,
            SocketMapAddrStateSpec,
        },
        socketmap::SocketMap,
        testutil::{
            FakeDeviceId, FakeReferencyDeviceId, FakeStrongDeviceId, FakeWeakDeviceId,
            MultipleDevicesId, TestIpExt,
        },
        CtxPair, UninstantiableWrapper,
    };
    use packet::{Buf, Serializer as _};
    use packet_formats::ip::{Ipv4Proto, Ipv6Proto};
    use test_case::test_case;

    use crate::internal::{
        base::{testutil::DualStackSendIpPacketMeta, DEFAULT_HOP_LIMITS},
        device::state::IpDeviceStateIpExt,
        socket::testutil::{FakeDeviceConfig, FakeDualStackIpSocketCtx, FakeIpSocketCtx},
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
        type LocalIdentifier = NonZeroU16;
        type RemoteIdentifier = u16;
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
            remote_port: u16,
        },
    }

    #[derive(Clone, Debug, Derivative)]
    #[derivative(Eq(bound = ""), PartialEq(bound = ""))]
    struct Id<I: IpExt, D: WeakDeviceIdentifier>(StrongRc<I, D, FakeStateSpec>);

    /// Utilities for accessing locked internal state in tests.
    impl<I: IpExt, D: WeakDeviceIdentifier> Id<I, D> {
        fn get(&self) -> impl Deref<Target = SocketState<I, D, FakeStateSpec>> + '_ {
            let Self(rc) = self;
            rc.state.read()
        }

        fn get_mut(&self) -> impl DerefMut<Target = SocketState<I, D, FakeStateSpec>> + '_ {
            let Self(rc) = self;
            rc.state.write()
        }
    }

    impl<I: IpExt, D: WeakDeviceIdentifier> From<StrongRc<I, D, FakeStateSpec>> for Id<I, D> {
        fn from(value: StrongRc<I, D, FakeStateSpec>) -> Self {
            Self(value)
        }
    }

    impl<I: IpExt, D: WeakDeviceIdentifier> Borrow<StrongRc<I, D, FakeStateSpec>> for Id<I, D> {
        fn borrow(&self) -> &StrongRc<I, D, FakeStateSpec> {
            let Self(rc) = self;
            rc
        }
    }

    #[derive(Debug)]
    struct AddrState<T>(T);

    struct FakeSocketMapStateSpec<I, D>(PhantomData<(I, D)>, Never);

    impl<I: IpExt, D: WeakDeviceIdentifier> SocketMapStateSpec for FakeSocketMapStateSpec<I, D> {
        type AddrVecTag = Tag;
        type ConnAddrState = AddrState<Self::ConnId>;
        type ConnId = I::DualStackBoundSocketId<D, FakeStateSpec>;
        type ConnSharingState = Sharing;
        type ListenerAddrState = AddrState<Self::ListenerId>;
        type ListenerId = I::DualStackBoundSocketId<D, FakeStateSpec>;
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
        const NAME: &'static str = "FAKE";
        type AddrSpec = FakeAddrSpec;
        type SocketId<I: IpExt, D: WeakDeviceIdentifier> = Id<I, D>;
        type OtherStackIpOptions<I: IpExt, D: WeakDeviceIdentifier> =
            DatagramSocketOptions<I::OtherVersion, D>;
        type SocketMapSpec<I: IpExt, D: WeakDeviceIdentifier> = FakeSocketMapStateSpec<I, D>;
        type SharingState = Sharing;
        type ListenerIpAddr<I: IpExt> =
            I::DualStackListenerIpAddr<<FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier>;
        type ConnIpAddr<I: IpExt> = I::DualStackConnIpAddr<Self>;
        type ConnStateExtra = ();
        type ConnState<I: IpExt, D: WeakDeviceIdentifier> = I::DualStackConnState<D, Self>;
        type ExternalData<I: Ip> = ();

        fn ip_proto<I: IpProtoExt>() -> I::Proto {
            I::map_ip((), |()| FAKE_DATAGRAM_IPV4_PROTOCOL, |()| FAKE_DATAGRAM_IPV6_PROTOCOL)
        }

        fn make_bound_socket_map_id<I: IpExt, D: WeakDeviceIdentifier>(
            s: &Self::SocketId<I, D>,
        ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::BoundSocketId
        {
            I::into_dual_stack_bound_socket_id(s.clone())
        }

        type Serializer<I: IpExt, B: BufferMut> = packet::Nested<B, ()>;
        type SerializeError = Never;
        fn make_packet<I: IpExt, B: BufferMut>(
            body: B,
            _addr: &ConnIpAddr<
                I::Addr,
                <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Result<Self::Serializer<I, B>, Never> {
            Ok(body.encapsulate(()))
        }
        fn try_alloc_listen_identifier<I: Ip, D: WeakDeviceIdentifier>(
            _bindings_ctx: &mut impl RngContext,
            is_available: impl Fn(
                <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            ) -> Result<(), InUseError>,
        ) -> Option<<FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
            (1..=u16::MAX).map(|i| NonZeroU16::new(i).unwrap()).find(|i| is_available(*i).is_ok())
        }

        fn conn_info_from_state<I: IpExt, D: WeakDeviceIdentifier>(
            state: &Self::ConnState<I, D>,
        ) -> ConnInfo<I::Addr, D> {
            let ConnAddr { ip, device } = I::conn_addr_from_state(state);
            let ConnInfoAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } =
                ip.into();
            ConnInfo::new(local_ip, local_port, remote_ip, remote_port, || {
                device.clone().expect("device must be bound for addresses that require zones")
            })
        }

        fn try_alloc_local_id<I: IpExt, D: WeakDeviceIdentifier, BC: RngContext>(
            bound: &BoundSocketMap<I, D, FakeAddrSpec, FakeSocketMapStateSpec<I, D>>,
            _bindings_ctx: &mut BC,
            _flow: DatagramFlowId<I::Addr, <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier>,
        ) -> Option<<FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
            (1..u16::MAX).find_map(|identifier| {
                let identifier = NonZeroU16::new(identifier).unwrap();
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

    impl<I: IpExt, D: WeakDeviceIdentifier> DatagramSocketMapSpec<I, D, FakeAddrSpec>
        for FakeSocketMapStateSpec<I, D>
    {
        type BoundSocketId = I::DualStackBoundSocketId<D, FakeStateSpec>;
    }

    impl<I: IpExt, D: WeakDeviceIdentifier>
        SocketMapConflictPolicy<
            ConnAddr<
                ConnIpAddr<
                    I::Addr,
                    <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                    <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
                >,
                D,
            >,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for FakeSocketMapStateSpec<I, D>
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            addr: &ConnAddr<
                ConnIpAddr<
                    I::Addr,
                    <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                    <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
                >,
                D,
            >,
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

    impl<I: IpExt, D: WeakDeviceIdentifier>
        SocketMapConflictPolicy<
            ListenerAddr<
                ListenerIpAddr<I::Addr, <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
                D,
            >,
            Sharing,
            I,
            D,
            FakeAddrSpec,
        > for FakeSocketMapStateSpec<I, D>
    {
        fn check_insert_conflicts(
            sharing: &Sharing,
            _addr: &ListenerAddr<
                ListenerIpAddr<I::Addr, <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
                D,
            >,
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

    impl<T: Eq> SocketMapAddrStateSpec for AddrState<T> {
        type Id = T;
        type SharingState = Sharing;
        type Inserter<'a> = Never where Self: 'a;

        fn new(_sharing: &Self::SharingState, id: Self::Id) -> Self {
            AddrState(id)
        }
        fn contains_id(&self, id: &Self::Id) -> bool {
            let Self(inner) = self;
            inner == id
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
            FakeSocketMapStateSpec<Ipv4, FakeWeakDeviceId<D>>,
        >,
        v6: BoundSockets<
            Ipv6,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeSocketMapStateSpec<Ipv6, FakeWeakDeviceId<D>>,
        >,
    }

    impl<D: FakeStrongDeviceId, I: IpExt>
        AsRef<
            BoundSockets<
                I,
                FakeWeakDeviceId<D>,
                FakeAddrSpec,
                FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
            >,
        > for FakeBoundSockets<D>
    {
        fn as_ref(
            &self,
        ) -> &BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
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
                FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
            >,
        > for FakeBoundSockets<D>
    {
        fn as_mut(
            &mut self,
        ) -> &mut BoundSockets<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
        > {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpExt, D: FakeStrongDeviceId>(
                &'a mut BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
                >,
            );
            let Wrap(state) =
                I::map_ip(self, |state| Wrap(&mut state.v4), |state| Wrap(&mut state.v6));
            state
        }
    }

    type FakeBindingsCtx = netstack3_base::testutil::FakeBindingsCtx<(), (), (), ()>;

    type FakeSocketSet<I, D> = DatagramSocketSet<I, FakeWeakDeviceId<D>, FakeStateSpec>;

    type InnerIpSocketCtx<D> = netstack3_base::testutil::FakeCoreCtx<
        FakeDualStackIpSocketCtx<D>,
        DualStackSendIpPacketMeta<D>,
        D,
    >;

    struct FakeDualStackCoreCtx<D: FakeStrongDeviceId> {
        bound_sockets: FakeBoundSockets<D>,
        ip_socket_ctx: InnerIpSocketCtx<D>,
    }

    struct FakeCoreCtx<I: IpExt, D: FakeStrongDeviceId> {
        dual_stack: FakeDualStackCoreCtx<D>,
        // NB: socket set is last in the struct so all the strong refs are
        // dropped before the primary refs contained herein.
        socket_set: FakeSocketSet<I, D>,
    }

    impl<I: IpExt, D: FakeStrongDeviceId> FakeCoreCtx<I, D> {
        fn new_with_sockets(
            socket_set: FakeSocketSet<I, D>,
            bound_sockets: FakeBoundSockets<D>,
        ) -> Self {
            Self {
                socket_set,
                dual_stack: FakeDualStackCoreCtx {
                    bound_sockets,
                    ip_socket_ctx: Default::default(),
                },
            }
        }

        fn new_with_ip_socket_ctx(ip_socket_ctx: FakeDualStackIpSocketCtx<D>) -> Self {
            Self {
                socket_set: Default::default(),
                dual_stack: FakeDualStackCoreCtx {
                    bound_sockets: Default::default(),
                    ip_socket_ctx: InnerIpSocketCtx::with_state(ip_socket_ctx),
                },
            }
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeCoreCtx<I, D> {
        type DeviceId = D;
        type WeakDeviceId = FakeWeakDeviceId<D>;
    }

    impl<D: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeDualStackCoreCtx<D> {
        type DeviceId = D;
        type WeakDeviceId = FakeWeakDeviceId<D>;
    }

    impl<D: FakeStrongDeviceId, I: DatagramIpExt<D>>
        spec_context::DatagramSpecStateContext<I, FakeCoreCtx<I, D>, FakeBindingsCtx>
        for FakeStateSpec
    {
        type SocketsStateCtx<'a> = FakeDualStackCoreCtx<D>;

        fn with_all_sockets_mut<
            O,
            F: FnOnce(&mut DatagramSocketSet<I, FakeWeakDeviceId<D>, FakeStateSpec>) -> O,
        >(
            core_ctx: &mut FakeCoreCtx<I, D>,
            cb: F,
        ) -> O {
            cb(&mut core_ctx.socket_set)
        }

        fn with_all_sockets<
            O,
            F: FnOnce(&DatagramSocketSet<I, FakeWeakDeviceId<D>, FakeStateSpec>) -> O,
        >(
            core_ctx: &mut FakeCoreCtx<I, D>,
            cb: F,
        ) -> O {
            cb(&core_ctx.socket_set)
        }

        fn with_socket_state<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &SocketState<I, FakeWeakDeviceId<D>, FakeStateSpec>,
            ) -> O,
        >(
            core_ctx: &mut FakeCoreCtx<I, D>,
            id: &Id<I, FakeWeakDeviceId<D>>,
            cb: F,
        ) -> O {
            cb(&mut core_ctx.dual_stack, &id.get())
        }

        fn with_socket_state_mut<
            O,
            F: FnOnce(
                &mut Self::SocketsStateCtx<'_>,
                &mut SocketState<I, FakeWeakDeviceId<D>, FakeStateSpec>,
            ) -> O,
        >(
            core_ctx: &mut FakeCoreCtx<I, D>,
            id: &Id<I, FakeWeakDeviceId<D>>,
            cb: F,
        ) -> O {
            cb(&mut core_ctx.dual_stack, &mut id.get_mut())
        }

        fn for_each_socket<
            F: FnMut(
                &mut Self::SocketsStateCtx<'_>,
                &Id<I, FakeWeakDeviceId<D>>,
                &SocketState<I, FakeWeakDeviceId<D>, FakeStateSpec>,
            ),
        >(
            core_ctx: &mut FakeCoreCtx<I, D>,
            mut cb: F,
        ) {
            core_ctx.socket_set.keys().for_each(|id| {
                let id = Id::from(id.clone());
                cb(&mut core_ctx.dual_stack, &id, &id.get());
            })
        }
    }

    /// A test-only IpExt trait to specialize the `DualStackContext` and
    /// `NonDualStackContext` associated types on the
    /// `DatagramBoundStateContext`.
    ///
    /// This allows us to implement `DatagramBoundStateContext` for all `I`
    /// while also assigning its associated types different values for `Ipv4`
    /// and `Ipv6`.
    trait DualStackContextsIpExt<D: FakeStrongDeviceId>: IpExt {
        type DualStackContext: DualStackDatagramBoundStateContext<
            Self,
            FakeBindingsCtx,
            FakeStateSpec,
            DeviceId = D,
            WeakDeviceId = FakeWeakDeviceId<D>,
        >;
        type NonDualStackContext: NonDualStackDatagramBoundStateContext<
            Self,
            FakeBindingsCtx,
            FakeStateSpec,
            DeviceId = D,
            WeakDeviceId = FakeWeakDeviceId<D>,
        >;
        fn dual_stack_context(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext>;
    }

    impl<D: FakeStrongDeviceId> DualStackContextsIpExt<D> for Ipv4 {
        type DualStackContext = UninstantiableWrapper<FakeDualStackCoreCtx<D>>;
        type NonDualStackContext = FakeDualStackCoreCtx<D>;
        fn dual_stack_context(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::NotDualStack(core_ctx)
        }
    }

    impl<D: FakeStrongDeviceId> DualStackContextsIpExt<D> for Ipv6 {
        type DualStackContext = FakeDualStackCoreCtx<D>;
        type NonDualStackContext = UninstantiableWrapper<FakeDualStackCoreCtx<D>>;
        fn dual_stack_context(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            MaybeDualStack::DualStack(core_ctx)
        }
    }

    impl<D: FakeStrongDeviceId, I: Ip + DualStackContextsIpExt<D>>
        spec_context::DatagramSpecBoundStateContext<I, FakeDualStackCoreCtx<D>, FakeBindingsCtx>
        for FakeStateSpec
    {
        type IpSocketsCtx<'a> = InnerIpSocketCtx<D>;
        type DualStackContext = I::DualStackContext;
        type NonDualStackContext = I::NonDualStackContext;

        fn with_bound_sockets<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
                >,
            ) -> O,
        >(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            let FakeDualStackCoreCtx { bound_sockets, ip_socket_ctx } = core_ctx;
            cb(ip_socket_ctx, bound_sockets.as_ref())
        }
        fn with_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSockets<
                    I,
                    FakeWeakDeviceId<D>,
                    FakeAddrSpec,
                    FakeSocketMapStateSpec<I, FakeWeakDeviceId<D>>,
                >,
            ) -> O,
        >(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            let FakeDualStackCoreCtx { bound_sockets, ip_socket_ctx } = core_ctx;
            cb(ip_socket_ctx, bound_sockets.as_mut())
        }

        fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            cb(&mut core_ctx.ip_socket_ctx)
        }

        fn dual_stack_context(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
        ) -> MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
            I::dual_stack_context(core_ctx)
        }
    }

    impl<D: FakeStrongDeviceId>
        spec_context::NonDualStackDatagramSpecBoundStateContext<
            Ipv4,
            FakeDualStackCoreCtx<D>,
            FakeBindingsCtx,
        > for FakeStateSpec
    {
        fn nds_converter(
            _core_ctx: &FakeDualStackCoreCtx<D>,
        ) -> impl NonDualStackConverter<Ipv4, FakeWeakDeviceId<D>, Self> {
            ()
        }
    }

    impl<D: FakeStrongDeviceId>
        spec_context::DualStackDatagramSpecBoundStateContext<
            Ipv6,
            FakeDualStackCoreCtx<D>,
            FakeBindingsCtx,
        > for FakeStateSpec
    {
        type IpSocketsCtx<'a> = InnerIpSocketCtx<D>;
        fn dual_stack_enabled(
            _core_ctx: &FakeDualStackCoreCtx<D>,
            _state: &impl AsRef<IpOptions<Ipv6, FakeWeakDeviceId<D>, FakeStateSpec>>,
        ) -> bool {
            // For now, it's simplest to have dual-stack unconditionally enabled
            // for datagram tests. However, in the future this could be stateful
            // and follow an implementation similar to UDP's test fixture.
            true
        }

        fn to_other_socket_options<'a>(
            _core_ctx: &FakeDualStackCoreCtx<D>,
            state: &'a IpOptions<Ipv6, FakeWeakDeviceId<D>, FakeStateSpec>,
        ) -> &'a DatagramSocketOptions<Ipv4, FakeWeakDeviceId<D>> {
            let IpOptions { other_stack, .. } = state;
            other_stack
        }

        fn ds_converter(
            _core_ctx: &FakeDualStackCoreCtx<D>,
        ) -> impl DualStackConverter<Ipv6, FakeWeakDeviceId<D>, Self> {
            ()
        }

        fn to_other_bound_socket_id(
            _core_ctx: &FakeDualStackCoreCtx<D>,
            id: &Id<Ipv6, D::Weak>,
        ) -> EitherIpSocket<D::Weak, FakeStateSpec> {
            EitherIpSocket::V6(id.clone())
        }

        fn with_both_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSocketsFromSpec<Ipv6, FakeDualStackCoreCtx<D>, FakeStateSpec>,
                &mut BoundSocketsFromSpec<Ipv4, FakeDualStackCoreCtx<D>, FakeStateSpec>,
            ) -> O,
        >(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            let FakeDualStackCoreCtx { bound_sockets: FakeBoundSockets { v4, v6 }, ip_socket_ctx } =
                core_ctx;
            cb(ip_socket_ctx, v6, v4)
        }

        fn with_other_bound_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut BoundSocketsFromSpec<Ipv4, FakeDualStackCoreCtx<D>, FakeStateSpec>,
            ) -> O,
        >(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            let FakeDualStackCoreCtx { bound_sockets, ip_socket_ctx } = core_ctx;
            cb(ip_socket_ctx, bound_sockets.as_mut())
        }

        fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
            core_ctx: &mut FakeDualStackCoreCtx<D>,
            cb: F,
        ) -> O {
            cb(&mut core_ctx.ip_socket_ctx)
        }
    }

    #[ip_test]
    fn set_get_hop_limits<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        const EXPECTED_HOP_LIMITS: HopLimits = HopLimits {
            unicast: const_unwrap_option(NonZeroU8::new(45)),
            multicast: const_unwrap_option(NonZeroU8::new(23)),
        };

        update_ip_hop_limit::<_, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &unbound,
            |limits| {
                *limits = SocketHopLimits {
                    unicast: Some(EXPECTED_HOP_LIMITS.unicast),
                    multicast: Some(EXPECTED_HOP_LIMITS.multicast),
                    version: IpVersionMarker::default(),
                }
            },
        );

        assert_eq!(
            get_ip_hop_limits::<_, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            EXPECTED_HOP_LIMITS
        );
    }

    #[ip_test]
    fn set_get_device_hop_limits<I: Ip + DatagramIpExt<FakeReferencyDeviceId>>() {
        let device = FakeReferencyDeviceId::default();
        let mut core_ctx =
            FakeCoreCtx::<I, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new([
                FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: device.clone(),
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                },
            ]));
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        set_device::<_, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &unbound,
            Some(&device),
        )
        .unwrap();

        let HopLimits { mut unicast, multicast } = DEFAULT_HOP_LIMITS;
        unicast = unicast.checked_add(1).unwrap();
        {
            let device_state =
                core_ctx.dual_stack.ip_socket_ctx.state.get_device_state_mut::<I>(&device);
            assert_ne!(device_state.default_hop_limit, unicast);
            device_state.default_hop_limit = unicast;
        }
        assert_eq!(
            get_ip_hop_limits::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            HopLimits { unicast, multicast }
        );

        // If the device is removed, use default hop limits.
        device.mark_removed();
        assert_eq!(
            get_ip_hop_limits::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn default_hop_limits<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        assert_eq!(
            get_ip_hop_limits::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            DEFAULT_HOP_LIMITS
        );

        update_ip_hop_limit::<I, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &unbound,
            |limits| {
                *limits = SocketHopLimits {
                    unicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                    multicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                    version: IpVersionMarker::default(),
                }
            },
        );

        // The limits no longer match the default.
        assert_ne!(
            get_ip_hop_limits::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            DEFAULT_HOP_LIMITS
        );

        // Clear the hop limits set on the socket.
        update_ip_hop_limit::<I, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &unbound,
            |limits| *limits = Default::default(),
        );

        // The values should be back at the defaults.
        assert_eq!(
            get_ip_hop_limits::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn bind_device_unbound<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx =
            FakeCoreCtx::<I, _>::new_with_sockets(Default::default(), Default::default());
        let mut bindings_ctx = FakeBindingsCtx::default();

        let unbound = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());

        set_device::<I, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &unbound,
            Some(&FakeDeviceId),
        )
        .unwrap();
        assert_eq!(
            get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        set_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &mut bindings_ctx, &unbound, None)
            .unwrap();
        assert_eq!(
            get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &unbound),
            None
        );
    }

    #[ip_test]
    fn send_to_binds_unbound<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx = FakeCoreCtx::<I, FakeDeviceId>::new_with_ip_socket_ctx(
            FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                device: FakeDeviceId,
                local_ips: vec![I::TEST_ADDRS.local_ip],
                remote_ips: vec![I::TEST_ADDRS.remote_ip],
            }]),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let socket = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        let body = Buf::new(Vec::new(), ..);

        send_to::<I, _, _, FakeStateSpec, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)),
            1234,
            body,
        )
        .expect("succeeds");
        assert_matches!(
            get_info::<I, FakeStateSpec, _, _>(&mut core_ctx, &mut bindings_ctx, &socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    fn send_to_no_route_still_binds<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx =
            FakeCoreCtx::<I, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new([
                FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::TEST_ADDRS.local_ip],
                    remote_ips: vec![],
                },
            ]));
        let mut bindings_ctx = FakeBindingsCtx::default();

        let socket = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        let body = Buf::new(Vec::new(), ..);

        assert_matches!(
            send_to::<I, _, _, FakeStateSpec, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip)),
                1234,
                body,
            ),
            Err(Either::Right(SendToError::CreateAndSend(_)))
        );
        assert_matches!(
            get_info::<I, FakeStateSpec, _, _>(&mut core_ctx, &mut bindings_ctx, &socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    #[test_case(true; "remove device b")]
    #[test_case(false; "dont remove device b")]
    fn multicast_membership_changes<I: Ip + DatagramIpExt<FakeReferencyDeviceId> + TestIpExt>(
        remove_device_b: bool,
    ) {
        let device_a = FakeReferencyDeviceId::default();
        let device_b = FakeReferencyDeviceId::default();
        let mut core_ctx = FakeIpSocketCtx::<I, FakeReferencyDeviceId>::new(
            [device_a.clone(), device_b.clone()].into_iter().map(|device| FakeDeviceConfig {
                device,
                local_ips: Default::default(),
                remote_ips: Default::default(),
            }),
        );
        let mut bindings_ctx = FakeBindingsCtx::default();

        let multicast_addr1 = I::get_multicast_addr(1);
        let mut memberships = MulticastMemberships::default();
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr1,
                &FakeWeakDeviceId(device_a.clone()),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        core_ctx.join_multicast_group(&mut bindings_ctx, &device_a, multicast_addr1);

        let multicast_addr2 = I::get_multicast_addr(2);
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr2,
                &FakeWeakDeviceId(device_b.clone()),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        core_ctx.join_multicast_group(&mut bindings_ctx, &device_b, multicast_addr2);

        for (device, addr, expected) in [
            (&device_a, multicast_addr1, true),
            (&device_a, multicast_addr2, false),
            (&device_b, multicast_addr1, false),
            (&device_b, multicast_addr2, true),
        ] {
            assert_eq!(
                core_ctx.get_device_state(device).is_in_multicast_group(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }

        if remove_device_b {
            device_b.mark_removed();
        }

        leave_all_joined_groups(&mut core_ctx, &mut bindings_ctx, memberships);
        for (device, addr, expected) in [
            (&device_a, multicast_addr1, false),
            (&device_a, multicast_addr2, false),
            (&device_b, multicast_addr1, false),
            // Should not attempt to leave the multicast group on the device if
            // the device looks like it was removed. Note that although we mark
            // the device as removed, we do not destroy its state so we can
            // inspect it here.
            (&device_b, multicast_addr2, remove_device_b),
        ] {
            assert_eq!(
                core_ctx.get_device_state(device).is_in_multicast_group(&addr),
                expected,
                "device={:?}, addr={}",
                device,
                addr,
            );
        }
    }

    #[ip_test]
    fn set_get_transparent<I: Ip + DatagramIpExt<FakeDeviceId>>() {
        let mut core_ctx =
            FakeCoreCtx::<I, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new([
                FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                },
            ]));

        let unbound = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());

        assert!(!get_ip_transparent::<I, _, _, FakeStateSpec>(&mut core_ctx, &unbound));

        set_ip_transparent::<I, _, _, FakeStateSpec>(&mut core_ctx, &unbound, true);

        assert!(get_ip_transparent::<I, _, _, FakeStateSpec>(&mut core_ctx, &unbound));

        set_ip_transparent::<I, _, _, FakeStateSpec>(&mut core_ctx, &unbound, false);

        assert!(!get_ip_transparent::<I, _, _, FakeStateSpec>(&mut core_ctx, &unbound));
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
    fn connect_reinserts_on_failure_single_stack<I: Ip + DatagramIpExt<FakeDeviceId>>(
        original: OriginalSocketState,
    ) {
        connect_reinserts_on_failure_inner::<I>(
            original,
            I::TEST_ADDRS.local_ip.get(),
            I::TEST_ADDRS.remote_ip,
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

    fn connect_reinserts_on_failure_inner<I: Ip + DatagramIpExt<FakeDeviceId>>(
        original: OriginalSocketState,
        local_ip: I::Addr,
        remote_ip: SpecifiedAddr<I::Addr>,
    ) {
        let CtxPair { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs::<_, FakeBindingsCtx, _>(
                local_ip.to_ip_addr(),
                remote_ip.into(),
                [FakeDeviceId {}],
                |device_configs| {
                    FakeCoreCtx::<I, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new(
                        device_configs,
                    ))
                },
            );

        let socket = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(10));
        const ORIGINAL_REMOTE_PORT: u16 = 1234;
        const NEW_REMOTE_PORT: u16 = 5678;

        // Setup the original socket state.
        match original {
            OriginalSocketState::Unbound => {}
            OriginalSocketState::Listener => listen::<I, _, _, FakeStateSpec>(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                SpecifiedAddr::new(local_ip).map(ZonedAddr::Unzoned),
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed"),
            OriginalSocketState::Connected => connect::<I, _, _, FakeStateSpec>(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(remote_ip)),
                ORIGINAL_REMOTE_PORT,
                Default::default(),
            )
            .expect("connect should succeed"),
        }

        // Update the sharing state to generate conflicts during the call to `connect`.
        core_ctx.with_socket_state_mut(
            &socket,
            |_core_ctx, state: &mut SocketState<I, _, FakeStateSpec>| {
                let sharing = match state {
                    SocketState::Unbound(UnboundSocketState {
                        device: _,
                        sharing,
                        ip_options: _,
                    }) => sharing,
                    SocketState::Bound(BoundSocketState {
                        socket_type,
                        original_bound_addr: _,
                    }) => match socket_type {
                        BoundSocketStateType::Connected { state: _, sharing } => sharing,
                        BoundSocketStateType::Listener { state: _, sharing } => sharing,
                    },
                };
                *sharing = Sharing::ConnectionConflicts { remote_port: NEW_REMOTE_PORT };
            },
        );

        // Try to connect and observe a conflict error.
        assert_matches!(
            connect::<I, _, _, FakeStateSpec>(
                &mut core_ctx,
                &mut bindings_ctx,
                &socket,
                Some(ZonedAddr::Unzoned(remote_ip)),
                NEW_REMOTE_PORT,
                Default::default(),
            ),
            Err(ConnectError::SockAddrConflict)
        );

        // Verify the original socket state is intact.
        let info = get_info::<I, FakeStateSpec, _, _>(&mut core_ctx, &mut bindings_ctx, &socket);
        match original {
            OriginalSocketState::Unbound => assert_matches!(info, SocketInfo::Unbound),
            OriginalSocketState::Listener => {
                let local_port = assert_matches!(
                    info,
                    SocketInfo::Listener(ListenerInfo {
                        local_ip: _,
                        local_identifier,
                    }) => local_identifier
                );
                assert_eq!(LOCAL_PORT, local_port);
            }
            OriginalSocketState::Connected => {
                let remote_port = assert_matches!(
                    info,
                    SocketInfo::Connected(ConnInfo {
                        local_ip: _,
                        local_identifier: _,
                        remote_ip: _,
                        remote_identifier,
                    }) => remote_identifier
                );
                assert_eq!(ORIGINAL_REMOTE_PORT, remote_port);
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
        let CtxPair { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs::<_, FakeBindingsCtx, _>(
                Ipv6::UNSPECIFIED_ADDRESS.into(),
                remote_ip.into(),
                [FakeDeviceId {}],
                |device_configs| {
                    FakeCoreCtx::<Ipv6, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new(
                        device_configs,
                    ))
                },
            );

        const REMOTE_PORT: u16 = 1234;
        let socket = create::<Ipv6, FakeStateSpec, _, _>(&mut core_ctx, ());
        connect::<Ipv6, _, _, FakeStateSpec>(
            &mut core_ctx,
            &mut bindings_ctx,
            &socket,
            Some(ZonedAddr::Unzoned(remote_ip)),
            REMOTE_PORT,
            Default::default(),
        )
        .expect("connect should succeed");
        assert_eq!(
            get_shutdown_connected::<Ipv6, _, _, FakeStateSpec>(
                &mut core_ctx,
                &bindings_ctx,
                &socket
            ),
            None
        );

        shutdown_connected::<Ipv6, _, _, FakeStateSpec>(
            &mut core_ctx,
            &bindings_ctx,
            &socket,
            shutdown,
        )
        .expect("shutdown should succeed");
        assert_eq!(
            get_shutdown_connected::<Ipv6, _, _, FakeStateSpec>(
                &mut core_ctx,
                &bindings_ctx,
                &socket
            ),
            Some(shutdown)
        );
    }

    #[ip_test]
    #[test_case(OriginalSocketState::Unbound; "unbound")]
    #[test_case(OriginalSocketState::Listener; "listener")]
    #[test_case(OriginalSocketState::Connected; "connected")]
    fn set_get_device_single_stack<I: Ip + DatagramIpExt<MultipleDevicesId>>(
        original: OriginalSocketState,
    ) {
        set_get_device_inner::<I>(original, I::TEST_ADDRS.local_ip.get(), I::TEST_ADDRS.remote_ip);
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

    fn set_get_device_inner<I: Ip + DatagramIpExt<MultipleDevicesId>>(
        original: OriginalSocketState,
        local_ip: I::Addr,
        remote_ip: SpecifiedAddr<I::Addr>,
    ) {
        const DEVICE_ID1: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE_ID2: MultipleDevicesId = MultipleDevicesId::B;

        let CtxPair { mut core_ctx, mut bindings_ctx } =
            testutil::setup_fake_ctx_with_dualstack_conn_addrs::<_, FakeBindingsCtx, _>(
                local_ip.to_ip_addr(),
                remote_ip.into(),
                [DEVICE_ID1, DEVICE_ID2],
                |device_configs| {
                    FakeCoreCtx::<I, _>::new_with_ip_socket_ctx(FakeDualStackIpSocketCtx::new(
                        device_configs,
                    ))
                },
            );

        const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(10));
        const REMOTE_PORT: u16 = 1234;

        let socket1 = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());
        let socket2 = create::<I, FakeStateSpec, _, _>(&mut core_ctx, ());

        // Initialize each socket to the `original` state, and verify that their
        // device can be set.
        for (socket, device_id) in [(&socket1, DEVICE_ID1), (&socket2, DEVICE_ID2)] {
            match original {
                OriginalSocketState::Unbound => {}
                OriginalSocketState::Listener => listen::<I, _, _, FakeStateSpec>(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    &socket,
                    SpecifiedAddr::new(local_ip).map(ZonedAddr::Unzoned),
                    Some(LOCAL_PORT),
                )
                .expect("listen should succeed"),
                OriginalSocketState::Connected => connect::<I, _, _, FakeStateSpec>(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    &socket,
                    Some(ZonedAddr::Unzoned(remote_ip)),
                    REMOTE_PORT,
                    Default::default(),
                )
                .expect("connect should succeed"),
            }

            assert_eq!(
                get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, socket),
                None
            );
            set_device::<I, _, _, FakeStateSpec>(
                &mut core_ctx,
                &mut bindings_ctx,
                socket,
                Some(&device_id),
            )
            .expect("set device should succeed");
            assert_eq!(
                get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, socket),
                Some(FakeWeakDeviceId(device_id))
            );
        }

        // For bound sockets, try to bind socket 2 to device 1, and expect it
        // it to conflict with socket 1 (They now have identical address keys in
        // the bound socket map)
        if original != OriginalSocketState::Unbound {
            assert_eq!(
                set_device::<I, _, _, FakeStateSpec>(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    &socket2,
                    Some(&DEVICE_ID1)
                ),
                Err(SocketError::Local(LocalAddressError::AddressInUse))
            );
            // Verify both sockets still have their original device.
            assert_eq!(
                get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &socket1),
                Some(FakeWeakDeviceId(DEVICE_ID1))
            );
            assert_eq!(
                get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &socket2),
                Some(FakeWeakDeviceId(DEVICE_ID2))
            );
        }

        // Verify the device can be unset.
        // NB: Close socket2 first, otherwise socket 1 will conflict with it.
        close::<I, FakeStateSpec, _, _>(&mut core_ctx, &mut bindings_ctx, socket2).into_removed();
        set_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &mut bindings_ctx, &socket1, None)
            .expect("set device should succeed");
        assert_eq!(
            get_bound_device::<I, _, _, FakeStateSpec>(&mut core_ctx, &bindings_ctx, &socket1),
            None,
        );
    }
}
