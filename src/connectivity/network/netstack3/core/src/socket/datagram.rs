// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Shared code for implementing datagram sockets.

use alloc::collections::HashSet;
use core::{
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU16, NonZeroU8},
};
use packet::{BufferMut, Serializer};

use derivative::Derivative;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress},
    MulticastAddr, MulticastAddress as _, SpecifiedAddr, ZonedAddr,
};
use packet_formats::ip::{IpProto, IpProtoExt};
use thiserror::Error;

use crate::{
    algorithm::ProtocolFlowId,
    data_structures::{
        id_map::{Entry as IdMapEntry, EntryKey, IdMap},
        id_map_collection::{Entry as IdMapCollectionEntry, IdMapCollection},
        socketmap::Tagged,
    },
    device::{AnyDevice, DeviceIdContext, WeakId},
    error::{LocalAddressError, RemoteAddressError, SocketError, ZonedAddressError},
    ip::{
        socket::{
            BufferIpSocketHandler, IpSock, IpSockCreateAndSendError, IpSockCreationError,
            IpSockSendError, IpSocketHandler as _, SendOptions,
        },
        BufferTransportIpContext, EitherDeviceId, HopLimits, IpExt, MulticastMembershipHandler,
        TransportIpContext,
    },
    socket::{
        self,
        address::{AddrVecIter, ConnAddr, ConnIpAddr, ListenerIpAddr},
        AddrVec, Bound, BoundSocketMap, Connection, ConvertSocketTypeState, ExistsError,
        InsertError, Listener, ListenerAddr, SocketId, SocketMapAddrSpec, SocketMapAddrStateSpec,
        SocketMapConflictPolicy, SocketMapStateSpec, SocketState,
    },
};

/// Datagram socket storage.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct DatagramSockets<A: SocketMapAddrSpec, S: DatagramSocketStateSpec>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    pub(crate) bound: BoundSocketMap<A, S>,
    pub(crate) unbound:
        IdMap<UnboundSocketState<A::IpAddr, A::WeakDeviceId, S::UnboundSharingState>>,
    pub(crate) bound_state: IdMapCollection<SocketId<S>, SocketState<A, S>>,
}

impl<A: SocketMapAddrSpec, S: DatagramSocketStateSpec> DatagramSockets<A, S>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S: SocketMapConflictPolicy<
            ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
            A,
        > + SocketMapConflictPolicy<
            ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            A,
        >,
{
    pub(crate) fn iter_receivers(
        &self,
        (src_ip, src_port): (SpecifiedAddr<A::IpAddr>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SpecifiedAddr<A::IpAddr>, A::LocalIdentifier),
        device: A::WeakDeviceId,
    ) -> Option<FoundSockets<AddrEntry<'_, A, S>, impl Iterator<Item = AddrEntry<'_, A, S>> + '_>>
    {
        let Self { bound, unbound: _, bound_state: _ } = self;
        bound.lookup((src_ip, src_port), (dst_ip, dst_port), device)
    }
}

pub(crate) enum FoundSockets<A, It> {
    /// A single recipient was found for the address.
    Single(A),
    /// Indicates the looked-up address was multicast, and holds an iterator of
    /// the found receivers.
    Multicast(It),
}

impl<A: SocketMapAddrSpec, S: DatagramSocketStateSpec> BoundSocketMap<A, S>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S: SocketMapConflictPolicy<
            ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
            <S as SocketMapStateSpec>::ListenerSharingState,
            A,
        > + SocketMapConflictPolicy<
            ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
            <S as SocketMapStateSpec>::ConnSharingState,
            A,
        >,
{
    /// Finds the socket(s) that should receive an incoming packet.
    ///
    /// Uses the provided addresses and receiving device to look up sockets that
    /// should receive a matching incoming packet. Returns `None` if no sockets
    /// were found, or the results of the lookup.
    fn lookup(
        &self,
        (src_ip, src_port): (SpecifiedAddr<A::IpAddr>, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SpecifiedAddr<A::IpAddr>, A::LocalIdentifier),
        device: A::WeakDeviceId,
    ) -> Option<FoundSockets<AddrEntry<'_, A, S>, impl Iterator<Item = AddrEntry<'_, A, S>> + '_>>
    {
        let mut matching_entries = AddrVecIter::with_device(
            match src_port {
                Some(src_port) => {
                    ConnIpAddr { local: (dst_ip, dst_port), remote: (src_ip, src_port) }.into()
                }
                None => ListenerIpAddr { addr: Some(dst_ip), identifier: dst_port }.into(),
            },
            device,
        )
        .filter_map(move |addr: AddrVec<A>| match addr {
            AddrVec::Listen(l) => {
                self.listeners().get_by_addr(&l).map(|state| AddrEntry::Listen(state, l))
            }
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).map(|state| AddrEntry::Conn(state, c)),
        });

        if dst_ip.is_multicast() {
            Some(FoundSockets::Multicast(matching_entries))
        } else {
            let single_entry: Option<_> = matching_entries.next();
            single_entry.map(FoundSockets::Single)
        }
    }
}

pub(crate) enum AddrEntry<'a, A: SocketMapAddrSpec, S: DatagramSocketStateSpec> {
    Listen(&'a S::ListenerAddrState, ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>),
    Conn(
        &'a S::ConnAddrState,
        ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
    ),
}

#[derive(Debug, Derivative)]
#[derivative(Default(bound = "S: Default"))]
pub(crate) struct UnboundSocketState<A: IpAddress, D, S> {
    pub(crate) device: Option<D>,
    pub(crate) sharing: S,
    pub(crate) ip_options: IpOptions<A, D>,
}

#[derive(Debug)]
pub(crate) struct ListenerState<A: Eq + Hash, D: Hash + Eq> {
    pub(crate) ip_options: IpOptions<A, D>,
}

#[derive(Debug)]
pub(crate) struct ConnState<I: IpExt, D: Eq + Hash> {
    pub(crate) socket: IpSock<I, D, IpOptions<I::Addr, D>>,
    pub(crate) shutdown: Shutdown,
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
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Shutdown {
    /// True if the send path is shut down for the owning socket.
    ///
    /// If this is true, the socket should not be able to send packets.
    pub(crate) send: bool,
    /// True if the receive path is shut down for the owning socket.
    ///
    /// If this is true, the socket should not be able to receive packets.
    pub(crate) receive: bool,
}

#[derive(Clone, Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct IpOptions<A, D> {
    multicast_memberships: MulticastMemberships<A, D>,
    hop_limits: SocketHopLimits,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SocketHopLimits {
    unicast: Option<NonZeroU8>,
    // TODO(https://fxbug.dev/108323): Make this an Option<u8> to allow sending
    // multicast packets destined only for the local machine.
    multicast: Option<NonZeroU8>,
}

impl SocketHopLimits {
    pub(crate) fn set_unicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.unicast = value
    }

    pub(crate) fn set_multicast(value: Option<NonZeroU8>) -> impl FnOnce(&mut Self) {
        move |limits| limits.multicast = value
    }

    fn get_limits_with_defaults(&self, defaults: &HopLimits) -> HopLimits {
        let Self { unicast, multicast } = self;
        HopLimits {
            unicast: unicast.unwrap_or(defaults.unicast),
            multicast: multicast.unwrap_or(defaults.multicast),
        }
    }
}

impl<A: IpAddress, D> SendOptions<A::Version> for IpOptions<A, D> {
    fn hop_limit(&self, destination: &SpecifiedAddr<A>) -> Option<NonZeroU8> {
        if destination.is_multicast() {
            self.hop_limits.multicast
        } else {
            self.hop_limits.unicast
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

impl<A: IpAddress, D: crate::device::Id> ConnAddr<A, D, NonZeroU16, NonZeroU16> {
    pub(crate) fn from_protocol_flow_and_local_port(
        id: &ProtocolFlowId<A>,
        local_port: NonZeroU16,
    ) -> Self {
        Self {
            ip: ConnIpAddr {
                local: (*id.local_addr(), local_port),
                remote: (*id.remote_addr(), id.remote_port()),
            },
            device: None,
        }
    }
}

fn leave_all_joined_groups<I: Ip, C, SC: MulticastMembershipHandler<I, C>>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    memberships: MulticastMemberships<I::Addr, SC::WeakDeviceId>,
) {
    for (addr, device) in memberships {
        let Some(device) = sync_ctx.upgrade_weak_device_id(&device) else { continue; };
        sync_ctx.leave_multicast_group(ctx, &device, addr)
    }
}

pub(crate) trait LocalIdentifierAllocator<A: SocketMapAddrSpec, C, S: SocketMapStateSpec>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    fn try_alloc_local_id(
        &mut self,
        bound: &BoundSocketMap<A, S>,
        ctx: &mut C,
        flow: DatagramFlowId<A::IpAddr, A::RemoteIdentifier>,
    ) -> Option<A::LocalIdentifier>;
}

pub(crate) struct DatagramFlowId<A: IpAddress, RI> {
    pub(crate) local_ip: SpecifiedAddr<A>,
    pub(crate) remote_ip: SpecifiedAddr<A>,
    pub(crate) remote_id: RI,
}

pub(crate) trait DatagramStateContext<A: SocketMapAddrSpec, C, S: SocketMapStateSpec>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    /// The synchronized context passed to the callback provided to
    /// `with_sockets_mut`.
    type IpSocketsCtx<'a>: TransportIpContext<
            A::IpVersion,
            C,
            DeviceId = <A::WeakDeviceId as WeakId>::Strong,
            WeakDeviceId = A::WeakDeviceId,
        > + MulticastMembershipHandler<A::IpVersion, C>;

    /// The additional allocator passed to the callback provided to
    /// `with_sockets_mut`.
    type LocalIdAllocator: LocalIdentifierAllocator<A, C, S>;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>, &DatagramSockets<A, S>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut DatagramSockets<A, S>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait DatagramStateNonSyncContext<A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    /// Attempts to allocate an identifier for a listener.
    ///
    /// `is_available` checks whether the provided address could be used without
    /// conflicting with any existing entries in state context's socket map,
    /// returning an error otherwise.
    fn try_alloc_listen_identifier(
        &mut self,
        is_available: impl Fn(A::LocalIdentifier) -> Result<(), InUseError>,
    ) -> Option<A::LocalIdentifier>;
}

pub(crate) trait BufferDatagramStateContext<
    A: SocketMapAddrSpec,
    C,
    S: SocketMapStateSpec,
    B: BufferMut,
>: DatagramStateContext<A, C, S> where
    Bound<S>: Tagged<AddrVec<A>>,
{
    type BufferIpSocketsCtx<'a>: BufferTransportIpContext<
        A::IpVersion,
        C,
        B,
        DeviceId = <A::WeakDeviceId as WeakId>::Strong,
        WeakDeviceId = A::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_buf<
        O,
        F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>, &DatagramSockets<A, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_buf_mut<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &mut DatagramSockets<A, S>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

impl<
        A: SocketMapAddrSpec,
        C,
        S: SocketMapStateSpec,
        B: BufferMut,
        SC: DatagramStateContext<A, C, S>,
    > BufferDatagramStateContext<A, C, S, B> for SC
where
    Bound<S>: Tagged<AddrVec<A>>,
    for<'a> SC::IpSocketsCtx<'a>: BufferTransportIpContext<
        A::IpVersion,
        C,
        B,
        DeviceId = <A::WeakDeviceId as WeakId>::Strong,
        WeakDeviceId = A::WeakDeviceId,
    >,
{
    type BufferIpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;

    fn with_sockets_buf<
        O,
        F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>, &DatagramSockets<A, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets(self, cb)
    }

    fn with_sockets_buf_mut<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
            &mut DatagramSockets<A, S>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets_mut(self, cb)
    }
}

pub(crate) trait DatagramSocketStateSpec: SocketMapStateSpec {
    type UnboundId: Clone + From<usize> + EntryKey + Debug;
    type UnboundSharingState: Default;
}

pub(crate) trait DatagramSocketSpec<A: SocketMapAddrSpec>:
    DatagramSocketStateSpec<
        ListenerState = ListenerState<A::IpAddr, A::WeakDeviceId>,
        ConnState = ConnState<A::IpVersion, A::WeakDeviceId>,
    > + SocketMapConflictPolicy<
        ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>,
        <Self as SocketMapStateSpec>::ListenerSharingState,
        A,
    > + SocketMapConflictPolicy<
        ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>,
        <Self as SocketMapStateSpec>::ConnSharingState,
        A,
    >
{
    type Serializer<B: BufferMut>: Serializer<Buffer = B>;
    fn make_packet<B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<A::IpAddr, A::LocalIdentifier, A::RemoteIdentifier>,
    ) -> Self::Serializer<B>;
}

pub(crate) struct InUseError;

pub(crate) fn create_unbound<
    A: SocketMapAddrSpec,
    S: DatagramSocketStateSpec,
    C,
    SC: DatagramStateContext<A, C, S>,
>(
    sync_ctx: &mut SC,
) -> S::UnboundId
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets_mut(
        |_sync_ctx, DatagramSockets { bound_state: _, unbound, bound: _ }, _allocator| {
            unbound.push(UnboundSocketState::default()).into()
        },
    )
}

pub(crate) enum SocketInfo<A: SocketMapAddrSpec> {
    Unbound,
    Listener(ListenerAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier>),
    Connected(ConnAddr<A::IpAddr, A::WeakDeviceId, A::LocalIdentifier, A::RemoteIdentifier>),
}

pub(crate) fn remove<
    A: SocketMapAddrSpec,
    S: DatagramSocketSpec<A>,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: impl Into<DatagramSocketId<S>>,
) -> SocketInfo<A>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    A::IpVersion: IpExt,
{
    sync_ctx.with_sockets_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, unbound, bound } = state;
        let (ip_options, info) = match id.into() {
            DatagramSocketId::Unbound(id) => {
                let UnboundSocketState { device: _, sharing: _, ip_options } =
                    unbound.remove(id.get_key_index()).expect("invalid UDP unbound ID");

                (ip_options, SocketInfo::Unbound)
            }
            DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
                let (state, _sharing, addr) = Listener::from_socket_state(
                    bound_state
                        .remove(&Listener::to_socket_id(id.clone()))
                        .expect("invalid UDP listener ID"),
                );
                bound.listeners_mut().remove(&id, &addr).expect("Invalid UDP listener ID");

                let ListenerState { ip_options } = state;
                (ip_options, SocketInfo::Listener(addr))
            }
            DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
                let (state, _sharing, addr) = Connection::from_socket_state(
                    bound_state
                        .remove(&Connection::to_socket_id(id.clone()))
                        .expect("invalid UDP connection ID"),
                );
                bound.conns_mut().remove(&id, &addr).expect("UDP connection not found");
                let ConnState { socket, clear_device_on_disconnect: _, shutdown: _ } = state;
                (socket.into_options(), SocketInfo::Connected(addr))
            }
        };

        let IpOptions { multicast_memberships, hop_limits: _ } = ip_options;
        leave_all_joined_groups(sync_ctx, ctx, multicast_memberships);
        info
    })
}

pub(crate) fn get_info<
    A: SocketMapAddrSpec,
    S: DatagramSocketStateSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: impl Into<DatagramSocketId<S>>,
) -> SocketInfo<A>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets(|_sync_ctx, state| {
        let DatagramSockets { bound_state, unbound: _, bound: _ } = state;
        match id.into() {
            DatagramSocketId::Unbound(_) => SocketInfo::Unbound,
            DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
                let (_state, _sharing, addr) = Listener::from_socket_state_ref(
                    bound_state
                        .get(&Listener::to_socket_id(id.clone()))
                        .expect("invalid UDP listener ID"),
                );
                SocketInfo::Listener(addr.clone())
            }
            DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
                let (_state, _sharing, addr) = Connection::from_socket_state_ref(
                    bound_state
                        .get(&Connection::to_socket_id(id.clone()))
                        .expect("invalid UDP connection ID"),
                );
                SocketInfo::Connected(addr.clone())
            }
        }
    })
}

pub(crate) fn listen<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::UnboundId,
    addr: Option<
        ZonedAddr<A::IpAddr, <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
    >,
    local_id: Option<A::LocalIdentifier>,
) -> Result<S::ListenerId, LocalAddressError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::UnboundSharingState: Clone + Into<S::ListenerSharingState>,
    S::ListenerSharingState: Default,
{
    sync_ctx.with_sockets_mut(
        |sync_ctx, DatagramSockets { bound_state, bound, unbound }, _allocator| {
            let identifier = match local_id {
                Some(local_id) => Ok(local_id),
                None => {
                    let addr = addr.clone().map(|addr| addr.into_addr_zone().0);
                    let sharing_options = Default::default();
                    ctx.try_alloc_listen_identifier(|identifier| {
                        let check_addr =
                            ListenerAddr { device: None, ip: ListenerIpAddr { identifier, addr } };
                        bound.listeners().could_insert(&check_addr, &sharing_options).map_err(|e| {
                            match e {
                                InsertError::Exists
                                | InsertError::IndirectConflict
                                | InsertError::ShadowAddrExists
                                | InsertError::ShadowerExists => InUseError,
                            }
                        })
                    })
                }
                .ok_or(LocalAddressError::FailedToAllocateLocalPort),
            }?;

            let UnboundSocketState { device, sharing: _, ip_options: _ } = unbound
                .get(id.get_key_index())
                .unwrap_or_else(|| panic!("unbound ID {:?} is invalid", id));
            let (addr, device, identifier) = match addr {
                Some(addr) => {
                    // Extract the specified address and the device. The device
                    // is either the one from the address or the one to which
                    // the socket was previously bound.
                    let (addr, device) =
                        crate::transport::resolve_addr_with_device(addr, device.clone())?;

                    // Binding to multicast addresses is allowed regardless.
                    // Other addresses can only be bound to if they are assigned
                    // to the device.
                    if !addr.is_multicast() {
                        let mut assigned_to = sync_ctx.get_devices_with_assigned_addr(addr);
                        if let Some(device) = &device {
                            if !assigned_to.any(|d| device == &EitherDeviceId::Strong(d)) {
                                return Err(LocalAddressError::AddressMismatch);
                            }
                        } else {
                            if !assigned_to.any(
                            |_: <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId| {
                                true
                            },
                        ) {
                            return Err(LocalAddressError::CannotBindToAddress);
                        }
                        }
                    }
                    (Some(addr), device, identifier)
                }
                None => (None, device.clone().map(EitherDeviceId::Weak), identifier),
            };

            let unbound_entry = match unbound.entry(id.get_key_index()) {
                IdMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
                IdMapEntry::Occupied(o) => o,
            };

            let UnboundSocketState { device: _, sharing, ip_options: _ } = unbound_entry.get();
            let sharing = sharing.clone();
            match bound.listeners_mut().try_insert(
                ListenerAddr {
                    ip: ListenerIpAddr { addr, identifier },
                    device: device.map(|d| d.as_weak(sync_ctx).into_owned()),
                },
                sharing.into(),
                |addr, sharing| {
                    // Remove the unbound state only after we're sure the
                    // insertion is going to succeed.
                    let UnboundSocketState { device: _, sharing: _, ip_options } =
                        unbound_entry.remove();
                    let entry = bound_state.push_entry(
                        |index| Listener::to_socket_id(S::ListenerId::from(index)),
                        Listener::to_socket_state((
                            { ListenerState { ip_options } },
                            sharing,
                            addr,
                        )),
                    );
                    Listener::from_socket_id_ref(entry.key()).clone()
                },
            ) {
                Ok(entry) => Ok(entry.id()),
                Err((_e, _sharing)) => Err(LocalAddressError::AddressInUse),
            }
        },
    )
}

/// An error when attempting to create a datagram socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum SockCreationError {
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
}

pub(crate) fn connect<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::UnboundId,
    remote_ip: ZonedAddr<A::IpAddr, <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
    remote_id: A::RemoteIdentifier,
    proto: IpProto,
) -> Result<S::ConnId, SockCreationError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    S::UnboundSharingState: Clone + Into<S::ConnSharingState>,
{
    sync_ctx.with_sockets_mut(|sync_ctx, state, allocator| {
        let DatagramSockets { bound_state, bound, unbound } = state;
        let occupied = match unbound.entry(id.get_key_index()) {
            IdMapEntry::Vacant(_) => panic!("unbound socket {:?} not found", id),
            IdMapEntry::Occupied(o) => o,
        };
        let UnboundSocketState { device, sharing, ip_options: _ } = occupied.get();

        let (remote_ip, socket_device) =
            crate::transport::resolve_addr_with_device(remote_ip, device.clone())
                .map_err(SockCreationError::Zone)?;

        let mut ip_sock = sync_ctx
            .new_ip_socket(
                ctx,
                socket_device.as_ref().map(|d| d.as_ref()),
                None,
                remote_ip,
                proto.into(),
                Default::default(),
            )
            .map_err(|(e, _ip_options): (_, IpOptions<_, _>)| e)?;

        let local_ip = *ip_sock.local_ip();
        let remote_ip = *ip_sock.remote_ip();

        let local_id = match allocator.try_alloc_local_id(
            bound,
            ctx,
            DatagramFlowId { local_ip, remote_ip, remote_id: remote_id.clone() },
        ) {
            Some(x) => x,
            None => {
                return Err(SockCreationError::CouldNotAllocateLocalPort);
            }
        };

        let c = ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_id), remote: (remote_ip, remote_id) },
            device: ip_sock.device().cloned(),
        };
        match bound.conns_mut().try_insert(c, sharing.clone().into(), |addr, sharing| {
            let UnboundSocketState { device: _, sharing: _, ip_options } = occupied.remove();
            *ip_sock.options_mut() = ip_options;
            let entry = bound_state.push_entry(
                |index| Connection::to_socket_id(index.into()),
                Connection::to_socket_state((
                    ConnState {
                        socket: ip_sock,
                        clear_device_on_disconnect: false,
                        shutdown: Shutdown::default(),
                    },
                    sharing,
                    addr,
                )),
            );
            Connection::from_socket_id_ref(entry.key()).clone()
        }) {
            Ok(entry) => Ok(entry.id()),
            Err(e) => {
                let _: (InsertError, S::ConnSharingState) = e;
                Err(SockCreationError::SockAddrConflict)
            }
        }
    })
}

/// Error returned when [`connect_listener`] fails.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConnectListenerError {
    /// An error was encountered creating an IP socket.
    #[error("{}", _0)]
    Ip(#[from] IpSockCreationError),
    /// There was a problem with the provided address relating to its zone.
    #[error("{}", _0)]
    Zone(#[from] ZonedAddressError),
    /// The new socket conflicts with an existing one.
    #[error("The address is already occupied")]
    AddressConflict,
}

pub(crate) fn connect_listener<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ListenerId,
    remote_ip: ZonedAddr<A::IpAddr, <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
    remote_id: A::RemoteIdentifier,
    proto: IpProto,
) -> Result<S::ConnId, ConnectListenerError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    S::UnboundSharingState: Clone + Into<S::ListenerSharingState>,
    S::ListenerSharingState: Into<S::ConnSharingState>,
{
    sync_ctx.with_sockets_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound, unbound: _ } = state;
        let bound_state_listener =
            bound_state.get(&Listener::to_socket_id(id.clone())).expect("invalid listener ID");
        let (_, sharing, original_addr @ ListenerAddr { ip, device }): &(
            ListenerState<_, _>,
            _,
            _,
        ) = Listener::from_socket_state_ref(bound_state_listener);
        let original_addr = original_addr.clone();
        let sharing = sharing.clone();

        let (remote_ip, socket_device) =
            crate::transport::resolve_addr_with_device(remote_ip, device.clone())?;

        let ListenerIpAddr { addr: local_ip, identifier: local_port } = ip.clone();

        let mut ip_sock = sync_ctx
            .new_ip_socket(
                ctx,
                socket_device.as_ref().map(|d| d.as_ref()),
                local_ip,
                remote_ip,
                proto.into(),
                Default::default(),
            )
            .map_err(|(e, _ip_options)| e)?;

        let clear_device_on_disconnect = device.is_none() && socket_device.is_some();

        // First update the socket map, then, if that succeeds, update
        // bound_state.
        let bound_entry =
            bound.listeners_mut().entry(&id, &original_addr).expect("Invalid listener ID");
        bound_entry.remove();

        let local_ip = *ip_sock.local_ip();
        let remote_ip = *ip_sock.remote_ip();

        let c = ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_id) },
            device: ip_sock.device().cloned(),
        };
        match bound.conns_mut().try_insert(c, sharing.clone().into(), |addr, sharing| {
            let (ListenerState { ip_options }, _sharing, _addr) = Listener::from_socket_state(
                bound_state
                    .remove(&Listener::to_socket_id(id.clone()))
                    .expect("listener known to be present"),
            );
            *ip_sock.options_mut() = ip_options;
            let entry = bound_state.push_entry(
                |index| Connection::to_socket_id(index.into()),
                Connection::to_socket_state((
                    ConnState {
                        socket: ip_sock,
                        clear_device_on_disconnect,
                        shutdown: Shutdown::default(),
                    },
                    sharing,
                    addr,
                )),
            );
            Connection::from_socket_id_ref(entry.key()).clone()
        }) {
            Ok(entry) => {
                return Ok(entry.id());
            }
            Err(insert_error) => {
                let _: (InsertError, S::ConnSharingState) = insert_error;
                let _entry = bound
                    .listeners_mut()
                    .try_insert(original_addr, sharing, |_addr, _sharing| id)
                    .expect("reinserting just-removed listener failed");
                Err(ConnectListenerError::AddressConflict)
            }
        }
    })
}

pub(crate) fn reconnect<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ConnId,
    remote_ip: ZonedAddr<A::IpAddr, <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
    remote_id: A::RemoteIdentifier,
) -> Result<(), ConnectListenerError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    S::UnboundSharingState: Clone + Into<S::ListenerSharingState>,
{
    sync_ctx.with_sockets_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound, unbound: _ } = state;
        let mut bound_state_connection_entry =
            match bound_state.entry(Connection::to_socket_id(id.clone())) {
                IdMapCollectionEntry::Vacant(_) => unreachable!("invalid connection ID"),
                IdMapCollectionEntry::Occupied(o) => o,
            };
        let (
            ConnState { socket, clear_device_on_disconnect: _, shutdown: _ },
            sharing,
            original_addr @ ConnAddr { ip: ConnIpAddr { local, remote: _ }, device },
        ): &(ConnState<_, _>, _, _) =
            Connection::from_socket_state_ref(bound_state_connection_entry.get());
        let entry = bound.conns_mut().entry(&id, original_addr).expect("Invalid conn ID");
        let proto = socket.proto();
        let original_addr = original_addr.clone();

        let (remote_ip, socket_device) =
            crate::transport::resolve_addr_with_device(remote_ip, device.clone())?;

        let (local_ip, _) = local;
        let mut ip_sock = sync_ctx
            .new_ip_socket(
                ctx,
                socket_device.as_ref().map(|d| d.as_ref()),
                Some(*local_ip),
                remote_ip,
                proto,
                Default::default(),
            )
            .map_err(|(e, _ip_options)| e)?;

        let local = local.clone();

        let c = ConnAddr {
            ip: ConnIpAddr { local, remote: (remote_ip, remote_id) },
            device: ip_sock.device().cloned(),
        };

        // Remove the old connection from the socket map before inserting the
        // new one. This will be undone on failure.
        entry.remove();

        match bound.conns_mut().try_insert(c, sharing.clone(), |addr, _sharing| {
            // Use the new socket with the old socket's options.
            let (conn_state, _sharing, bound_state_addr) =
                Connection::from_socket_state_mut(bound_state_connection_entry.get_mut());
            *ip_sock.options_mut() = conn_state.socket.take_options();
            conn_state.socket = ip_sock;
            conn_state.shutdown = Shutdown::default();
            // Don't forget to update the socket state with the new address!
            *bound_state_addr = addr;
            // Keep the same ID.
            id.clone()
        }) {
            Ok(_entry) => Ok(()),
            Err(insert_error) => {
                let (_, sharing): (InsertError, _) = insert_error;
                // Restore the original socket if creation of the new socket fails.
                let _entry = bound
                    .conns_mut()
                    .try_insert(original_addr, sharing, |_addr, _sharing| id)
                    .unwrap_or_else(|(e, _sharing): (_, S::ConnSharingState)| {
                        unreachable!("reinserting just-removed connected socket failed: {:?}", e)
                    });

                Err(ConnectListenerError::AddressConflict)
            }
        }
    })
}

pub(crate) fn set_unbound_device<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::UnboundId,
    device_id: Option<&<SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
) where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state: _, unbound, bound: _ } = state;
        let UnboundSocketState { ref mut device, sharing: _, ip_options: _ } =
            unbound.get_mut(id.get_key_index()).expect("unbound UDP socket not found");
        *device = device_id.map(|d| sync_ctx.downgrade_device_id(d));
    })
}

pub(crate) fn disconnect_connected<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::ConnId,
) -> S::ListenerId
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    S::ConnSharingState: Into<S::ListenerSharingState>,
{
    sync_ctx.with_sockets_mut(|_sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound, unbound: _ } = state;
        let (state, sharing, addr): (_, S::ConnSharingState, _) = Connection::from_socket_state(
            bound_state
                .remove(&Connection::to_socket_id(id.clone()))
                .expect("no state for connection"),
        );
        let _bound_addr = bound.conns_mut().remove(&id, &addr).expect("connection not found");

        let ConnState { socket, clear_device_on_disconnect, shutdown: _ } = state;
        let ip_options = socket.into_options();

        let ConnAddr { ip: ConnIpAddr { local: (local_ip, identifier), remote: _ }, mut device } =
            addr;
        if clear_device_on_disconnect {
            device = None
        }

        let addr = ListenerAddr { ip: ListenerIpAddr { addr: Some(local_ip), identifier }, device };

        bound
            .listeners_mut()
            .try_insert(addr, sharing.into(), |addr, sharing| {
                let entry = bound_state.push_entry(
                    |index| Listener::to_socket_id(index.into()),
                    Listener::to_socket_state((ListenerState { ip_options }, sharing, addr)),
                );
                Listener::from_socket_id_ref(entry.key()).clone()
            })
            .expect("inserting listener for disconnected socket failed")
            .id()
    })
}

/// Which direction(s) to shut down for a socket.
#[derive(Copy, Clone, Debug, GenericOverIp)]
pub enum ShutdownType {
    /// Prevent sending packets on the socket.
    Send,
    /// Prevent receiving packets on the socket.
    Receive,
    /// Prevent sending and receiving packets on the socket.
    SendAndReceive,
}

pub(crate) fn shutdown_connected<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::ConnId,
    which: ShutdownType,
) where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets_mut(|_sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound: _, unbound: _ } = state;
        let (ConnState { socket: _, clear_device_on_disconnect: _, shutdown }, _sharing, _addr) =
            Connection::from_socket_state_mut(
                bound_state.get_mut(&Connection::to_socket_id(id)).expect("no such connection"),
            );
        let (shutdown_send, shutdown_receive) = match which {
            ShutdownType::Send => (true, false),
            ShutdownType::Receive => (false, true),
            ShutdownType::SendAndReceive => (true, true),
        };
        let Shutdown { send, receive } = shutdown;
        *send |= shutdown_send;
        *receive |= shutdown_receive;
    })
}

pub(crate) fn get_shutdown_connected<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::ConnId,
) -> Option<ShutdownType>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets(|_sync_ctx, state| {
        let DatagramSockets { bound_state, bound: _, unbound: _ } = state;
        let (ConnState { socket: _, clear_device_on_disconnect: _, shutdown }, _sharing, _addr) =
            Connection::from_socket_state_ref(
                bound_state.get(&Connection::to_socket_id(id)).expect("no such connection"),
            );
        let Shutdown { send, receive } = shutdown;
        Some(match (send, receive) {
            (false, false) => return None,
            (true, false) => ShutdownType::Send,
            (false, true) => ShutdownType::Receive,
            (true, true) => ShutdownType::SendAndReceive,
        })
    })
}

/// Error encountered when sending a datagram on a socket.
pub enum SendError<B, S> {
    /// The socket is not writeable.
    NotWriteable(B),
    /// There was a problem sending the IP packet.
    IpSock(S, IpSockSendError),
}

pub(crate) fn send_conn<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: BufferDatagramStateContext<A, C, S, B>,
    S: DatagramSocketSpec<A>,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ConnId,
    body: B,
) -> Result<(), SendError<B, S::Serializer<B>>>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets_buf_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound: _, unbound: _, bound_state } = state;
        let (
            ConnState {
                socket,
                clear_device_on_disconnect: _,
                shutdown: Shutdown { send: shutdown_send, receive: _ },
            },
            _sharing,
            addr,
        ) = Connection::from_socket_state_ref(
            bound_state.get(&Connection::to_socket_id(id)).expect("no such connection"),
        );
        if *shutdown_send {
            return Err(SendError::NotWriteable(body));
        }

        let ConnAddr { ip, device: _ } = addr;

        sync_ctx
            .send_ip_packet(ctx, &socket, S::make_packet(body, &ip), None)
            .map_err(|(serializer, send_error)| SendError::IpSock(serializer, send_error))
    })
}

pub(crate) enum SendToError<B, S> {
    NotWriteable(B),
    Zone(B, ZonedAddressError),
    CreateAndSend(S, IpSockCreateAndSendError),
}

pub(crate) fn send_conn_to<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: BufferDatagramStateContext<A, C, S, B>,
    S: DatagramSocketSpec<A>,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ConnId,
    remote_ip: ZonedAddr<
        A::IpAddr,
        <SC::BufferIpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId,
    >,
    remote_port: A::RemoteIdentifier,
    body: B,
) -> Result<(), SendToError<B, S::Serializer<B>>>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    sync_ctx.with_sockets_buf_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound: _, unbound: _ } = state;
        let state = bound_state.get(&Connection::to_socket_id(id)).expect("no such connection");
        let (
            ConnState { socket, clear_device_on_disconnect: _, shutdown },
            _,
            ConnAddr { ip: ConnIpAddr { local, remote: _ }, device },
        ): &(_, S::ConnSharingState, _) = Connection::from_socket_state_ref(state);

        let Shutdown { send: shutdown_write, receive: _ } = shutdown;
        if *shutdown_write {
            return Err(SendToError::NotWriteable(body));
        }
        let (local_ip, local_id) = local;

        send_oneshot::<A, S, _, _, _>(
            sync_ctx,
            ctx,
            (Some(*local_ip), local_id.clone()),
            remote_ip,
            remote_port,
            device,
            socket.options(),
            socket.proto(),
            body,
        )
    })
}

pub(crate) fn send_listener_to<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: BufferDatagramStateContext<A, C, S, B>,
    S: DatagramSocketSpec<A>,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ListenerId,
    proto: <A::IpVersion as IpProtoExt>::Proto,
    remote_ip: ZonedAddr<
        A::IpAddr,
        <SC::BufferIpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId,
    >,
    remote_port: A::RemoteIdentifier,
    body: B,
) -> Result<(), SendToError<B, S::Serializer<B>>>
where
    Bound<S>: Tagged<AddrVec<A>>,
{
    // TODO(https://fxbug.dev/92447) If `local_ip` is `None`, and so
    // `new_ip_socket` picks a local IP address for us, it may cause problems
    // when we don't match the bound listener addresses. We should revisit
    // whether that check is actually necessary.
    //
    // Also, if the local IP address is a multicast address this function should
    // probably fail and `send_udp_conn_to` must be used instead.
    sync_ctx.with_sockets_buf_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound: _, unbound: _ } = state;
        let state =
            bound_state.get(&Listener::to_socket_id(id)).expect("specified listener not found");
        let (
            ListenerState { ip_options },
            _,
            ListenerAddr { ip: ListenerIpAddr { addr: local_ip, identifier: local_port }, device },
        ): &(_, S::ListenerSharingState, _) = Listener::from_socket_state_ref(state);

        send_oneshot::<A, S, _, _, _>(
            sync_ctx,
            ctx,
            (*local_ip, local_port.clone()),
            remote_ip,
            remote_port,
            device,
            ip_options,
            proto,
            body,
        )
    })
}

fn send_oneshot<
    A: SocketMapAddrSpec,
    S: DatagramSocketSpec<A>,
    SC: BufferIpSocketHandler<
        A::IpVersion,
        C,
        B,
        DeviceId = <A::WeakDeviceId as WeakId>::Strong,
        WeakDeviceId = A::WeakDeviceId,
    >,
    C,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    (local_ip, local_id): (Option<SpecifiedAddr<A::IpAddr>>, A::LocalIdentifier),
    remote_ip: ZonedAddr<A::IpAddr, SC::DeviceId>,
    remote_id: A::RemoteIdentifier,
    device: &Option<A::WeakDeviceId>,
    ip_options: &IpOptions<A::IpAddr, A::WeakDeviceId>,
    proto: <A::IpVersion as IpProtoExt>::Proto,
    body: B,
) -> Result<(), SendToError<B, S::Serializer<B>>> {
    let (remote_ip, device) =
        match crate::transport::resolve_addr_with_device(remote_ip, device.clone()) {
            Ok(addr) => addr,
            Err(e) => return Err(SendToError::Zone(body, e)),
        };

    sync_ctx
        .send_oneshot_ip_packet(
            ctx,
            device.as_ref().map(|d| d.as_ref()),
            local_ip,
            remote_ip,
            proto,
            ip_options,
            |local_ip| {
                S::make_packet(
                    body,
                    &ConnIpAddr { local: (local_ip, local_id), remote: (remote_ip, remote_id) },
                )
            },
            None,
        )
        .map_err(|(body, err, _ip_options)| SendToError::CreateAndSend(body, err))
}

pub(crate) type DatagramBoundId<S> = SocketId<S>;

pub(crate) fn set_listener_device<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::ListenerId,
    new_device: Option<&<SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
) -> Result<(), LocalAddressError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    for<'a> A::WeakDeviceId:
        PartialEq<<SC::IpSocketsCtx<'a> as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    sync_ctx.with_sockets_mut(
        |sync_ctx, DatagramSockets { bound_state, unbound: _, bound }, _allocator| {
            // Don't allow changing the device if one of the IP addresses in the socket
            // address vector requires a zone (scope ID).
            let state = bound_state
                .get_mut(&SocketId::Listener(id.clone()))
                .unwrap_or_else(|| panic!("invalid listener ID {:?}", id));
            let (_, _, addr): &(S::ListenerState, S::ListenerSharingState, _) =
                Listener::from_socket_state_ref(state);
            let ListenerAddr {
                device: old_device,
                ip: ListenerIpAddr { addr: ip_addr, identifier: _ },
            } = addr;
            if !socket::can_device_change(
                ip_addr.as_ref(), /* local_ip */
                None,             /* remote_ip */
                old_device.as_ref(),
                new_device,
            ) {
                return Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch));
            }

            let entry = bound
                .listeners_mut()
                .entry(&id, addr)
                .unwrap_or_else(|| panic!("invalid listener ID {:?}", id));
            let new_addr = ListenerAddr {
                device: new_device.map(|d| sync_ctx.downgrade_device_id(d)),
                ..addr.clone()
            };
            let new_entry = entry
                .try_update_addr(new_addr)
                .map_err(|(ExistsError {}, _entry)| LocalAddressError::AddressInUse)?;

            let (_, _, addr): &mut (S::ListenerState, S::ListenerSharingState, _) =
                Listener::from_socket_state_mut(state);
            *addr = new_entry.get_addr().clone();
            Ok(())
        },
    )
}

pub(crate) fn set_connected_device<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::ConnId,
    new_device: Option<&<SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId>,
) -> Result<(), SocketError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    for<'a> A::WeakDeviceId:
        PartialEq<<SC::IpSocketsCtx<'a> as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    sync_ctx.with_sockets_mut(
        |sync_ctx, DatagramSockets { bound_state, bound, unbound: _ }, _allocator| {
            // Don't allow changing the device if one of the IP addresses in the socket
            // address vector requires a zone (scope ID).
            let bound_state = bound_state
                .get_mut(&Connection::to_socket_id(id.clone()))
                .unwrap_or_else(|| panic!("invalid conn ID {:?}", id));
            let (state, _, addr): &(_, S::ConnSharingState, _) =
                Connection::from_socket_state_ref(bound_state);
            let ConnAddr {
                device: old_device,
                ip: ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) },
            } = addr;
            if !socket::can_device_change(
                Some(local_ip),
                Some(remote_ip),
                old_device.as_ref(),
                new_device,
            ) {
                return Err(SocketError::Local(LocalAddressError::Zone(
                    ZonedAddressError::DeviceZoneMismatch,
                )));
            }

            let ConnState { socket, clear_device_on_disconnect: _, shutdown: _ } = state;
            let mut new_socket = sync_ctx
                .new_ip_socket(
                    ctx,
                    new_device.map(EitherDeviceId::Strong),
                    Some(*local_ip),
                    *remote_ip,
                    socket.proto(),
                    Default::default(),
                )
                .map_err(|_: (IpSockCreationError, IpOptions<_, _>)| {
                    SocketError::Remote(RemoteAddressError::NoRoute)
                })?;

            let entry = bound
                .conns_mut()
                .entry(&id, addr)
                .unwrap_or_else(|| panic!("invalid conn ID {:?}", id));
            let new_addr = ConnAddr { device: new_socket.device().cloned(), ..addr.clone() };

            let entry = match entry.try_update_addr(new_addr) {
                Err((ExistsError, _entry)) => {
                    return Err(SocketError::Local(LocalAddressError::AddressInUse))
                }
                Ok(entry) => entry,
            };
            // Since the move was successful, replace the old socket with
            // the new one but move the options over.
            let (state, _, addr): &mut (_, S::ConnSharingState, _) =
                Connection::from_socket_state_mut(bound_state);
            let ConnState { socket, clear_device_on_disconnect, shutdown: _ } = state;
            let _: IpOptions<_, _> = new_socket.replace_options(socket.take_options());
            *socket = new_socket;
            *addr = entry.get_addr().clone();

            // If this operation explicitly sets the device for the socket, it
            // should no longer be cleared on disconnect.
            if new_device.is_some() {
                *clear_device_on_disconnect = false;
            }
            Ok(())
        },
    )
}

pub(crate) fn get_bound_device<
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: impl Into<DatagramSocketId<S>>,
) -> Option<A::WeakDeviceId>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
{
    sync_ctx.with_sockets(|_sync_ctx, state| {
        let DatagramSockets { bound_state, bound: _, unbound } = state;
        match id.into() {
            DatagramSocketId::Unbound(id) => {
                let UnboundSocketState { device, sharing: _, ip_options: _ } =
                    unbound.get(id.get_key_index()).expect("unbound socket not found");
                device.clone()
            }
            DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
                let state = bound_state.get(&SocketId::Listener(id)).expect("listener not found");
                let (_, _, addr): &(S::ListenerState, S::ListenerSharingState, _) =
                    Listener::from_socket_state_ref(state);
                let ListenerAddr { device, ip: _ } = addr;
                device.clone()
            }
            DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
                let state =
                    bound_state.get(&SocketId::Connection(id)).expect("connected socket not found");
                let (_, _, addr): &(S::ConnState, S::ConnSharingState, _) =
                    Connection::from_socket_state_ref(state);
                let ConnAddr { device, ip: _ } = addr;
                device.clone()
            }
        }
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
    /// The requested membership change had no effect (tried to leave a group
    /// without joining, or to join a group again).
    NoMembershipChange,
    /// The socket is bound to a device that doesn't match the one specified.
    WrongDevice,
}

/// Selects the interface for the given remote address, optionally with a
/// constraint on the source address.
fn pick_interface_for_addr<
    A: SocketMapAddrSpec,
    S: SocketMapStateSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: TransportIpContext<A::IpVersion, C, WeakDeviceId = A::WeakDeviceId>,
>(
    sync_ctx: &mut SC,
    _remote_addr: MulticastAddr<A::IpAddr>,
    source_addr: Option<SpecifiedAddr<A::IpAddr>>,
) -> Result<SC::DeviceId, SetMulticastMembershipError> {
    if let Some(source_addr) = source_addr {
        let mut devices = sync_ctx.get_devices_with_assigned_addr(source_addr);
        if let Some(d) = devices.next() {
            if devices.next() == None {
                return Ok(d);
            }
        }
    }
    log_unimplemented!((), "https://fxbug.dev/39479: Implement this by looking up a route");
    Err(SetMulticastMembershipError::NoDeviceAvailable)
}

#[derive(Derivative)]
#[derivative(Clone(bound = "S::UnboundId: Clone, S::ListenerId: Clone, S::ConnId: Clone"))]
pub(crate) enum DatagramSocketId<S: DatagramSocketStateSpec> {
    Unbound(S::UnboundId),
    Bound(DatagramBoundId<S>),
}

/// Selector for the device to affect when changing multicast settings.
#[derive(Copy, Clone, Debug, Eq, GenericOverIp, PartialEq)]
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
    A: SocketMapAddrSpec,
    C: DatagramStateNonSyncContext<A, S>,
    SC: DatagramStateContext<A, C, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: impl Into<DatagramSocketId<S>>,
    multicast_group: MulticastAddr<A::IpAddr>,
    interface: MulticastMembershipInterfaceSelector<
        A::IpAddr,
        <SC::IpSocketsCtx<'_> as DeviceIdContext<AnyDevice>>::DeviceId,
    >,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    for<'a> A::WeakDeviceId:
        PartialEq<<SC::IpSocketsCtx<'a> as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    let id = id.into();

    sync_ctx.with_sockets_mut(|sync_ctx, state, _allocator| {
        let DatagramSockets { bound_state, bound: _, unbound } = state;
        let bound_device = match id.clone() {
            DatagramSocketId::Unbound(id) => {
                let UnboundSocketState { device, sharing: _, ip_options: _ } =
                    unbound.get(id.get_key_index()).expect("unbound UDP socket not found");
                device
            }
            DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
                let state =
                    bound_state.get(&SocketId::Listener(id)).expect("Listening socket not found");
                let (_, _, ListenerAddr { ip: _, device }): &(
                    ListenerState<_, _>,
                    S::ListenerSharingState,
                    _,
                ) = Listener::from_socket_state_ref(state);
                device
            }
            DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
                let state =
                    bound_state.get(&SocketId::Connection(id)).expect("Connected socket not found");
                let (_, _, ConnAddr { ip: _, device }): &(ConnState<_, _>, S::ConnSharingState, _) =
                    Connection::from_socket_state_ref(state);
                device
            }
        };

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
                    pick_interface_for_addr(sync_ctx, multicast_group, Some(addr))?,
                ),
            },
            MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute => {
                if let Some(bound_device) = bound_device.as_ref() {
                    EitherDeviceId::Weak(bound_device.clone())
                } else {
                    EitherDeviceId::Strong(pick_interface_for_addr(
                        sync_ctx,
                        multicast_group,
                        None,
                    )?)
                }
            }
        };

        let DatagramSockets { bound_state: _, bound: _, unbound } = state;
        let ip_options = match id {
            DatagramSocketId::Unbound(id) => {
                let UnboundSocketState { device: _, sharing: _, ip_options } =
                    unbound.get_mut(id.get_key_index()).expect("unbound UDP socket not found");
                ip_options
            }

            DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
                let state = bound_state
                    .get_mut(&SocketId::Listener(id))
                    .expect("Listening socket not found");
                let (ListenerState { ip_options }, _, _): &mut (
                    _,
                    S::ListenerSharingState,
                    ListenerAddr<_, _, _>,
                ) = Listener::from_socket_state_mut(state);
                ip_options
            }
            DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
                let state = bound_state
                    .get_mut(&SocketId::Connection(id))
                    .expect("Connected socket not found");
                let (ConnState { socket, clear_device_on_disconnect: _, shutdown: _ }, _, _): &mut (
                    _,
                    S::ConnSharingState,
                    ConnAddr<_, _, _, _>,
                ) = Connection::from_socket_state_mut(state);
                socket.options_mut()
            }
        };

        let Some(strong_interface) = interface.as_strong(sync_ctx) else {
            return Err(SetMulticastMembershipError::DeviceDoesNotExist);
        };

        let IpOptions { multicast_memberships, hop_limits: _ } = ip_options;
        match multicast_memberships
            .apply_membership_change(multicast_group, &interface.as_weak(sync_ctx), want_membership)
            .ok_or(SetMulticastMembershipError::NoMembershipChange)?
        {
            MulticastMembershipChange::Join => {
                sync_ctx.join_multicast_group(ctx, &strong_interface, multicast_group)
            }
            MulticastMembershipChange::Leave => {
                sync_ctx.leave_multicast_group(ctx, &strong_interface, multicast_group)
            }
        }

        Ok(())
    })
}

fn get_options_device<A: SocketMapAddrSpec, S: DatagramSocketSpec<A>>(
    DatagramSockets { bound_state, bound: _, unbound }: &DatagramSockets<A, S>,
    id: DatagramSocketId<S>,
) -> (&IpOptions<A::IpAddr, A::WeakDeviceId>, &Option<A::WeakDeviceId>)
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    A::IpVersion: IpExt,
{
    match id {
        DatagramSocketId::Unbound(id) => {
            let UnboundSocketState { ip_options, device, sharing: _ } =
                unbound.get(id.get_key_index()).expect("unbound UDP socket not found");
            (ip_options, device)
        }
        DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
            let state =
                bound_state.get(&SocketId::Listener(id)).expect("listening socket not found");
            let (ListenerState { ip_options }, _, ListenerAddr { device, ip: _ }): &(
                _,
                S::ListenerSharingState,
                _,
            ) = Listener::from_socket_state_ref(state);
            (ip_options, device)
        }
        DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
            let state =
                bound_state.get(&SocketId::Connection(id)).expect("connected socket not found");
            let (
                ConnState { socket, clear_device_on_disconnect: _, shutdown: _ },
                _,
                ConnAddr { device, ip: _ },
            ): &(_, S::ConnSharingState, _) = Connection::from_socket_state_ref(state);
            (socket.options(), device)
        }
    }
}

fn get_options_mut<A: SocketMapAddrSpec, S: DatagramSocketSpec<A>>(
    DatagramSockets { bound_state, bound: _, unbound }: &mut DatagramSockets<A, S>,
    id: DatagramSocketId<S>,
) -> &mut IpOptions<A::IpAddr, A::WeakDeviceId>
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    A::IpVersion: IpExt,
{
    match id {
        DatagramSocketId::Unbound(id) => {
            let UnboundSocketState { ip_options, device: _, sharing: _ } =
                unbound.get_mut(id.get_key_index()).expect("unbound UDP socket not found");
            ip_options
        }
        DatagramSocketId::Bound(DatagramBoundId::Listener(id)) => {
            let state =
                bound_state.get_mut(&SocketId::Listener(id)).expect("listening socket not found");
            let (ListenerState { ip_options }, _, _): &mut (
                _,
                S::ListenerSharingState,
                ListenerAddr<_, _, _>,
            ) = Listener::from_socket_state_mut(state);
            ip_options
        }
        DatagramSocketId::Bound(DatagramBoundId::Connection(id)) => {
            let state =
                bound_state.get_mut(&SocketId::Connection(id)).expect("connected socket not found");
            let (ConnState { socket, clear_device_on_disconnect: _, shutdown: _ }, _, _): &mut (
                _,
                S::ConnSharingState,
                ConnAddr<_, _, _, _>,
            ) = Connection::from_socket_state_mut(state);
            socket.options_mut()
        }
    }
}

pub(crate) fn update_ip_hop_limit<
    A: SocketMapAddrSpec,
    SC: DatagramStateContext<A, C, S>,
    C: DatagramStateNonSyncContext<A, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: impl Into<DatagramSocketId<S>>,
    update: impl FnOnce(&mut SocketHopLimits),
) where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    A::IpVersion: IpExt,
{
    sync_ctx.with_sockets_mut(|_sync_ctx, sockets, _allocator| {
        let options = get_options_mut(sockets, id.into());

        update(&mut options.hop_limits)
    })
}

pub(crate) fn get_ip_hop_limits<
    A: SocketMapAddrSpec,
    SC: DatagramStateContext<A, C, S>,
    C: DatagramStateNonSyncContext<A, S>,
    S: DatagramSocketSpec<A>,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: impl Into<DatagramSocketId<S>>,
) -> HopLimits
where
    Bound<S>: Tagged<AddrVec<A>>,
    S::ListenerAddrState:
        SocketMapAddrStateSpec<Id = S::ListenerId, SharingState = S::ListenerSharingState>,
    S::ConnAddrState: SocketMapAddrStateSpec<Id = S::ConnId, SharingState = S::ConnSharingState>,
    A::IpVersion: IpExt,
{
    sync_ctx.with_sockets(|sync_ctx, sockets| {
        let (options, device) = get_options_device(sockets, id.into());
        let IpOptions { hop_limits, multicast_memberships: _ } = options;
        let device = device.as_ref().and_then(|d| sync_ctx.upgrade_weak_device_id(d));
        hop_limits.get_limits_with_defaults(&sync_ctx.get_default_hop_limits(device.as_ref()))
    })
}

#[cfg(test)]
mod test {
    use core::{convert::Infallible as Never, marker::PhantomData, ops::DerefMut as _};

    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use nonzero_ext::nonzero;
    use test_case::test_case;

    use crate::{
        data_structures::socketmap::SocketMap,
        device::{
            testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
            WeakId,
        },
        ip::{
            device::state::IpDeviceStateIpExt,
            socket::testutil::{FakeDeviceConfig, FakeIpSocketCtx},
            testutil::FakeIpDeviceIdCtx,
            IpLayerIpExt, DEFAULT_HOP_LIMITS,
        },
        socket::{IncompatibleError, InsertError, RemoveResult},
        testutil::{FakeNonSyncCtx, TestIpExt},
    };

    use super::*;

    trait DatagramIpExt: Ip + IpExt + IpDeviceStateIpExt {}

    impl DatagramIpExt for Ipv4 {}
    impl DatagramIpExt for Ipv6 {}

    struct FakeAddrSpec<I, D>(Never, PhantomData<(I, D)>);

    impl<I: IpExt, D: WeakId> SocketMapAddrSpec for FakeAddrSpec<I, D> {
        type WeakDeviceId = D;
        type IpAddr = I::Addr;
        type IpVersion = I;
        type LocalIdentifier = u8;
        type RemoteIdentifier = char;
    }

    struct FakeStateSpec<I, D>(Never, PhantomData<(I, D)>);

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Tag;

    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    struct Sharing;

    #[derive(Copy, Clone, Debug, Derivative)]
    #[derivative(Eq(bound = ""), PartialEq(bound = ""))]
    struct Id<T>(usize, PhantomData<T>);

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Conn;
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Listen;
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Unbound;

    impl<S> From<usize> for Id<S> {
        fn from(u: usize) -> Self {
            Self(u, PhantomData)
        }
    }

    impl<S> EntryKey for Id<S> {
        fn get_key_index(&self) -> usize {
            let Self(u, PhantomData) = self;
            *u
        }
    }

    impl<I: DatagramIpExt, D: crate::device::Id> SocketMapStateSpec for FakeStateSpec<I, D> {
        type AddrVecTag = Tag;
        type ConnAddrState = Id<Conn>;
        type ConnId = Id<Conn>;
        type ConnSharingState = Sharing;
        type ConnState = ConnState<I, D>;
        type ListenerAddrState = Id<Listen>;
        type ListenerId = Id<Listen>;
        type ListenerSharingState = Sharing;
        type ListenerState = ListenerState<I::Addr, D>;
    }

    impl<A, S> Tagged<A> for Id<S> {
        type Tag = Tag;
        fn tag(&self, _address: &A) -> Self::Tag {
            Tag
        }
    }

    impl<I: DatagramIpExt, D: crate::device::Id> From<Id<Conn>>
        for DatagramSocketId<FakeStateSpec<I, FakeWeakDeviceId<D>>>
    {
        fn from(u: Id<Conn>) -> Self {
            DatagramSocketId::Bound(DatagramBoundId::Connection(u))
        }
    }

    impl<I: DatagramIpExt, D: crate::device::Id> From<Id<Listen>>
        for DatagramSocketId<FakeStateSpec<I, FakeWeakDeviceId<D>>>
    {
        fn from(u: Id<Listen>) -> Self {
            DatagramSocketId::Bound(DatagramBoundId::Listener(u))
        }
    }

    impl<I: DatagramIpExt, D: crate::device::Id> From<Id<Unbound>>
        for DatagramSocketId<FakeStateSpec<I, FakeWeakDeviceId<D>>>
    {
        fn from(u: Id<Unbound>) -> Self {
            DatagramSocketId::Unbound(u)
        }
    }

    impl<I: DatagramIpExt, D: crate::device::Id> DatagramSocketStateSpec
        for FakeStateSpec<I, FakeWeakDeviceId<D>>
    {
        type UnboundId = Id<Unbound>;
        type UnboundSharingState = Sharing;
    }

    impl<A, I: DatagramIpExt, D: FakeStrongDeviceId>
        SocketMapConflictPolicy<A, Sharing, FakeAddrSpec<I, FakeWeakDeviceId<D>>>
        for FakeStateSpec<I, FakeWeakDeviceId<D>>
    {
        fn check_insert_conflicts(
            _new_sharing_state: &Sharing,
            _addr: &A,
            _socketmap: &SocketMap<AddrVec<FakeAddrSpec<I, FakeWeakDeviceId<D>>>, Bound<Self>>,
        ) -> Result<(), InsertError>
        where
            Bound<Self>: Tagged<AddrVec<FakeAddrSpec<I, FakeWeakDeviceId<D>>>>,
        {
            // Addresses are completely independent and shadowing doesn't cause
            // conflicts.
            Ok(())
        }
    }

    impl<I: DatagramIpExt, D: FakeStrongDeviceId>
        DatagramSocketSpec<FakeAddrSpec<I, FakeWeakDeviceId<D>>>
        for FakeStateSpec<I, FakeWeakDeviceId<D>>
    {
        type Serializer<B: BufferMut> = B;
        fn make_packet<B: BufferMut>(
            body: B,
            _addr: &ConnIpAddr<
                <FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::IpAddr,
                <FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::LocalIdentifier,
                <FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Self::Serializer<B> {
            body
        }
    }

    impl<S> SocketMapAddrStateSpec for Id<S> {
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

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeDatagramState<I: DatagramIpExt, D: FakeStrongDeviceId> {
        sockets: DatagramSockets<
            FakeAddrSpec<I, FakeWeakDeviceId<D>>,
            FakeStateSpec<I, FakeWeakDeviceId<D>>,
        >,
        state: FakeIpSocketCtx<I, D>,
    }

    impl<I: DatagramIpExt + IpLayerIpExt, D: FakeStrongDeviceId + 'static>
        DatagramStateContext<
            FakeAddrSpec<I, FakeWeakDeviceId<D>>,
            FakeNonSyncCtx,
            FakeStateSpec<I, FakeWeakDeviceId<D>>,
        > for FakeDatagramState<I, D>
    {
        type IpSocketsCtx<'a> = FakeIpSocketCtx<I, D>;
        type LocalIdAllocator = ();

        fn with_sockets<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &DatagramSockets<
                    FakeAddrSpec<I, FakeWeakDeviceId<D>>,
                    FakeStateSpec<I, FakeWeakDeviceId<D>>,
                >,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { sockets, state } = self;
            cb(state, sockets)
        }

        fn with_sockets_mut<
            O,
            F: FnOnce(
                &mut Self::IpSocketsCtx<'_>,
                &mut DatagramSockets<
                    FakeAddrSpec<I, FakeWeakDeviceId<D>>,
                    FakeStateSpec<I, FakeWeakDeviceId<D>>,
                >,
                &mut Self::LocalIdAllocator,
            ) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { sockets, state } = self;
            cb(state, sockets, &mut ())
        }
    }

    impl<I: DatagramIpExt + IpLayerIpExt, D: FakeStrongDeviceId + 'static>
        DatagramStateNonSyncContext<
            FakeAddrSpec<I, FakeWeakDeviceId<FakeDeviceId>>,
            FakeStateSpec<I, FakeWeakDeviceId<D>>,
        > for FakeNonSyncCtx
    {
        fn try_alloc_listen_identifier(
            &mut self,
            _is_available: impl Fn(u8) -> Result<(), InUseError>,
        ) -> Option<
            <FakeAddrSpec<I, FakeWeakDeviceId<FakeDeviceId>> as SocketMapAddrSpec>::LocalIdentifier,
        > {
            unimplemented!("not required for any existing tests")
        }
    }

    impl<I: DatagramIpExt, D: FakeStrongDeviceId + 'static>
        LocalIdentifierAllocator<
            FakeAddrSpec<I, FakeWeakDeviceId<D>>,
            FakeNonSyncCtx,
            FakeStateSpec<I, FakeWeakDeviceId<D>>,
        > for ()
    {
        fn try_alloc_local_id(
            &mut self,
            bound: &BoundSocketMap<
                FakeAddrSpec<I, FakeWeakDeviceId<D>>,
                FakeStateSpec<I, FakeWeakDeviceId<D>>,
            >,
            _ctx: &mut FakeNonSyncCtx,
            _flow: DatagramFlowId<
                <FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::IpAddr,
                <FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Option<<FakeAddrSpec<I, FakeWeakDeviceId<D>> as SocketMapAddrSpec>::LocalIdentifier>
        {
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
    fn set_get_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeDatagramState::<I, FakeDeviceId>::default();
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create_unbound(&mut sync_ctx);
        const EXPECTED_HOP_LIMITS: HopLimits =
            HopLimits { unicast: nonzero!(45u8), multicast: nonzero!(23u8) };

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound, |limits| {
            *limits = SocketHopLimits {
                unicast: Some(EXPECTED_HOP_LIMITS.unicast),
                multicast: Some(EXPECTED_HOP_LIMITS.multicast),
            }
        });

        assert_eq!(get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound), EXPECTED_HOP_LIMITS);
    }

    #[ip_test]
    fn set_get_device_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeDatagramState::<I, FakeDeviceId> {
            sockets: Default::default(),
            state: FakeIpSocketCtx::new([FakeDeviceConfig {
                device: FakeDeviceId,
                local_ips: Default::default(),
                remote_ips: Default::default(),
            }]),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create_unbound(&mut sync_ctx);
        set_unbound_device(&mut sync_ctx, &mut non_sync_ctx, unbound, Some(&FakeDeviceId));

        let HopLimits { mut unicast, multicast } = DEFAULT_HOP_LIMITS;
        unicast = unicast.checked_add(1).unwrap();
        {
            let mut default_hop_limit =
                sync_ctx.state.get_device_state(&FakeDeviceId).default_hop_limit.write();
            let default_hop_limit = default_hop_limit.deref_mut();
            assert_ne!(*default_hop_limit, unicast);
            *default_hop_limit = unicast;
        }
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound),
            HopLimits { unicast, multicast }
        );

        // If the device is removed, use default hop limits.
        AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut sync_ctx.state)
            .set_device_removed(FakeDeviceId, true);
        assert_eq!(get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound), DEFAULT_HOP_LIMITS);
    }

    #[ip_test]
    fn default_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeDatagramState::<I, FakeDeviceId>::default();
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create_unbound(&mut sync_ctx);
        assert_eq!(get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound), DEFAULT_HOP_LIMITS);

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound, |limits| {
            *limits =
                SocketHopLimits { unicast: Some(nonzero!(1u8)), multicast: Some(nonzero!(1u8)) }
        });

        // The limits no longer match the default.
        assert_ne!(get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound), DEFAULT_HOP_LIMITS);

        // Clear the hop limits set on the socket.
        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound, |limits| {
            *limits = Default::default()
        });

        // The values should be back at the defaults.
        assert_eq!(get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound), DEFAULT_HOP_LIMITS);
    }

    #[ip_test]
    fn bind_device_unbound<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeDatagramState::<I, FakeDeviceId>::default();
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create_unbound(&mut sync_ctx);

        set_unbound_device(&mut sync_ctx, &mut non_sync_ctx, unbound, Some(&FakeDeviceId));
        assert_eq!(
            get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        set_unbound_device(&mut sync_ctx, &mut non_sync_ctx, unbound, None);
        assert_eq!(get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound), None);
    }

    #[ip_test]
    #[test_case(true; "remove device b")]
    #[test_case(false; "dont remove device b")]
    fn multicast_membership_changes<I: Ip + DatagramIpExt + TestIpExt>(remove_device_b: bool) {
        let mut sync_ctx = FakeIpSocketCtx::<I, MultipleDevicesId>::new(
            MultipleDevicesId::all().into_iter().map(|device| FakeDeviceConfig {
                device,
                local_ips: Default::default(),
                remote_ips: Default::default(),
            }),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::default();

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
        sync_ctx.join_multicast_group(&mut non_sync_ctx, &MultipleDevicesId::A, multicast_addr1);

        let multicast_addr2 = I::get_multicast_addr(2);
        assert_eq!(
            memberships.apply_membership_change(
                multicast_addr2,
                &FakeWeakDeviceId(MultipleDevicesId::B),
                true /* want_membership */
            ),
            Some(MulticastMembershipChange::Join),
        );
        sync_ctx.join_multicast_group(&mut non_sync_ctx, &MultipleDevicesId::B, multicast_addr2);

        for (device, addr, expected) in [
            (MultipleDevicesId::A, multicast_addr1, true),
            (MultipleDevicesId::A, multicast_addr2, false),
            (MultipleDevicesId::B, multicast_addr1, false),
            (MultipleDevicesId::B, multicast_addr2, true),
        ] {
            assert_eq!(
                sync_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={}, addr={}",
                device,
                addr,
            );
        }

        if remove_device_b {
            AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut sync_ctx)
                .set_device_removed(MultipleDevicesId::B, true);
        }

        leave_all_joined_groups(&mut sync_ctx, &mut non_sync_ctx, memberships);
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
                sync_ctx.get_device_state(&device).multicast_groups.read().contains(&addr),
                expected,
                "device={}, addr={}",
                device,
                addr,
            );
        }
    }
}
