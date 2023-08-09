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
    num::{NonZeroU16, NonZeroU8},
};

use derivative::Derivative;
use either::Either;
use explicit::UnreachableExt as _;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, Ipv4, Ipv6},
    MulticastAddr, MulticastAddress as _, SpecifiedAddr, Witness as _, ZonedAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::ip::{IpProto, IpProtoExt};
use thiserror::Error;

use crate::{
    algorithm::ProtocolFlowId,
    context::RngContext,
    data_structures::id_map::{Entry as IdMapEntry, EntryKey, IdMap},
    device::{self, AnyDevice, DeviceIdContext, Id},
    error::{LocalAddressError, RemoteAddressError, SocketError, ZonedAddressError},
    ip::{
        device::state::IpDeviceStateIpExt,
        socket::{
            BufferIpSocketHandler, IpSock, IpSockCreateAndSendError, IpSockCreationError,
            IpSockSendError, IpSocketHandler, SendOptions,
        },
        BufferTransportIpContext, EitherDeviceId, HopLimits, MulticastMembershipHandler,
        TransportIpContext,
    },
    socket::{
        self,
        address::{AddrVecIter, ConnAddr, ConnIpAddr, ListenerIpAddr},
        AddrVec, BoundSocketMap, ExistsError, InsertError, ListenerAddr, Shutdown,
        SocketMapAddrSpec, SocketMapConflictPolicy, SocketMapStateSpec,
        SocketState as BoundSocketState, SocketStateSpec,
    },
};

/// Datagram demultiplexing map.
pub(crate) type BoundSockets<I, D, A, S> = BoundSocketMap<I, D, A, S>;

/// Storage of state for all datagram sockets.
pub(crate) type SocketsState<I, D, S> = IdMap<SocketState<I, D, S>>;

impl<I: IpExt, NewIp: IpExt, D: device::WeakId, S: DatagramSocketSpec> GenericOverIp<NewIp>
    for SocketsState<I, D, S>
{
    type Type = SocketsState<NewIp, D, S>;
}

pub(crate) trait IpExt: crate::ip::IpExt + DualStackIpExt {}
impl<I: crate::ip::IpExt + DualStackIpExt> IpExt for I {}

#[derive(Derivative, GenericOverIp)]
#[derivative(Debug(bound = "D: Debug"))]
pub(crate) enum SocketState<I: Ip + IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    Unbound(UnboundSocketState<I::Addr, D, S::UnboundSharingState<I>>),
    Bound(BoundSocketState<I, D, S::AddrSpec, S::SocketMapSpec<I, D>>),
}

impl<I: Ip, D: Id, A: SocketMapAddrSpec, S: DatagramSocketMapSpec<I, D, A>>
    BoundSockets<I, D, A, S>
{
    pub(crate) fn iter_receivers(
        &self,
        (src_ip, src_port): (I::Addr, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SpecifiedAddr<I::Addr>, A::LocalIdentifier),
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

impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> SocketsState<I, D, S> {
    pub(crate) fn get_socket_state(&self, id: &S::SocketId<I>) -> Option<&SocketState<I, D, S>> {
        self.get(id.get_key_index())
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
        (src_ip, src_port): (I::Addr, Option<A::RemoteIdentifier>),
        (dst_ip, dst_port): (SpecifiedAddr<I::Addr>, A::LocalIdentifier),
        device: D,
    ) -> Option<
        FoundSockets<
            AddrEntry<'_, I, D, A, S>,
            impl Iterator<Item = AddrEntry<'_, I, D, A, S>> + '_,
        >,
    > {
        let mut matching_entries = AddrVecIter::with_device(
            match (SpecifiedAddr::new(src_ip), src_port) {
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

        if dst_ip.is_multicast() {
            Some(FoundSockets::Multicast(matching_entries))
        } else {
            let single_entry: Option<_> = matching_entries.next();
            single_entry.map(FoundSockets::Single)
        }
    }
}

pub(crate) enum AddrEntry<'a, I: Ip, D, A: SocketMapAddrSpec, S: SocketMapStateSpec> {
    Listen(&'a S::ListenerAddrState, ListenerAddr<I::Addr, D, A::LocalIdentifier>),
    Conn(&'a S::ConnAddrState, ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>),
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

fn leave_all_joined_groups<A: IpAddress, C, SC: MulticastMembershipHandler<A::Version, C>>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    memberships: MulticastMemberships<A, SC::WeakDeviceId>,
) {
    for (addr, device) in memberships {
        let Some(device) = sync_ctx.upgrade_weak_device_id(&device) else {
            continue;
        };
        sync_ctx.leave_multicast_group(ctx, &device, addr)
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

pub(crate) struct DatagramFlowId<A: IpAddress, RI> {
    pub(crate) local_ip: SpecifiedAddr<A>,
    pub(crate) remote_ip: SpecifiedAddr<A>,
    pub(crate) remote_id: RI,
}

pub(crate) trait DatagramStateContext<I: IpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to methods.
    type SocketsStateCtx<'a>: DatagramBoundStateContext<I, C, S>
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

pub(crate) trait DatagramBoundStateContext<I: IpExt + DualStackIpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + MulticastMembershipHandler<I, C>
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
        C,
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
        C,
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

    /// Provides access to the dual-stack context, if one is available.
    ///
    /// For socket types that don't support dual-stack operation (like ICMP,
    /// raw IP sockets, and UDPv4), this method should always return `None`.
    /// Otherwise it should provide an instance of the `DualStackContext`, which
    /// can be used by the caller to access dual-stack state.
    fn dual_stack_context(&mut self) -> Option<&mut Self::DualStackContext>;

    /// Calls the function with only the inner context.
    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// Provides access to dual-stack socket state.
pub(crate) trait DualStackDatagramBoundStateContext<I: IpExt, C, S: DatagramSocketSpec>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callbacks to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        // Allow creating IP sockets for the other IP version.
        + TransportIpContext<I::OtherVersion, C>;

    /// Returns if the socket state indicates dual-stack operation is enabled.
    fn dual_stack_enabled(&self, socket: &SocketState<I, Self::WeakDeviceId, S>) -> bool;

    /// Converts a socket ID to a receiving ID.
    ///
    /// Converts a socket ID for IP version `I` into a receiving ID that can be
    /// inserted into the demultiplexing map for IP version `I::OtherVersion`.
    fn to_other_receiving_id(
        &self,
        id: S::SocketId<I>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::ReceivingId;

    /// Converts an other-IP-version address to an address for IP version `I`.
    fn from_other_ip_addr(&self, addr: <I::OtherVersion as Ip>::Addr) -> I::Addr;

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
}

/// An uninstantiable type that implements  [`LocalIdentifierAllocator`].
pub(crate) struct UninstantiableAllocator(Never);

impl AsRef<Never> for UninstantiableAllocator {
    fn as_ref(&self) -> &Never {
        let Self(never) = self;
        &never
    }
}

impl<I: Ip, D: device::Id, A: SocketMapAddrSpec, C, S: SocketMapStateSpec>
    LocalIdentifierAllocator<I, D, A, C, S> for UninstantiableAllocator
{
    fn try_alloc_local_id(
        &mut self,
        _bound: &BoundSocketMap<I, D, A, S>,
        _ctx: &mut C,
        _flow: DatagramFlowId<I::Addr, A::RemoteIdentifier>,
    ) -> Option<A::LocalIdentifier> {
        self.uninstantiable_unreachable()
    }
}

/// An uninstantiable type that ipmlements
/// [`DualStackDatagramBoundStateContext`].
pub(crate) struct UninstantiableDualStackContext<I, S, P>(Never, PhantomData<(I, S, P)>);

impl<I, S, P> AsRef<Never> for UninstantiableDualStackContext<I, S, P> {
    fn as_ref(&self) -> &Never {
        let Self(never, _marker) = self;
        &never
    }
}

impl<I, S, P: DeviceIdContext<AnyDevice>> DeviceIdContext<AnyDevice>
    for UninstantiableDualStackContext<I, S, P>
{
    type DeviceId = P::DeviceId;
    type WeakDeviceId = P::WeakDeviceId;
    fn downgrade_device_id(&self, _device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        self.uninstantiable_unreachable()
    }
    fn is_device_installed(&self, _device_id: &Self::DeviceId) -> bool {
        self.uninstantiable_unreachable()
    }
    fn upgrade_weak_device_id(
        &self,
        _weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        self.uninstantiable_unreachable()
    }
}

impl<I: IpExt, S: DatagramSocketSpec, P: DatagramBoundStateContext<I, C, S>, C>
    DualStackDatagramBoundStateContext<I, C, S> for UninstantiableDualStackContext<I, S, P>
where
    for<'a> P::IpSocketsCtx<'a>: TransportIpContext<I::OtherVersion, C>,
{
    type IpSocketsCtx<'a> = P::IpSocketsCtx<'a>;

    fn dual_stack_enabled(&self, _socket: &SocketState<I, Self::WeakDeviceId, S>) -> bool {
        self.uninstantiable_unreachable()
    }

    fn to_other_receiving_id(
        &self,
        _id: S::SocketId<I>,
    ) -> <S::SocketMapSpec<I::OtherVersion, Self::WeakDeviceId> as DatagramSocketMapSpec<
        I::OtherVersion,
        Self::WeakDeviceId,
        S::AddrSpec,
    >>::ReceivingId {
        self.uninstantiable_unreachable()
    }

    fn from_other_ip_addr(&self, _addr: <I::OtherVersion as Ip>::Addr) -> I::Addr {
        self.uninstantiable_unreachable()
    }

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
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }

    fn with_other_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut BoundSockets<
                I::OtherVersion,
                Self::WeakDeviceId,
                S::AddrSpec,
                S::SocketMapSpec<<I>::OtherVersion, Self::WeakDeviceId>,
            >,
        ) -> O,
    >(
        &mut self,
        _cb: F,
    ) -> O {
        self.uninstantiable_unreachable()
    }
}

pub(crate) trait DatagramStateNonSyncContext<I: Ip, S>: RngContext {}
impl<C: RngContext, I: Ip, S> DatagramStateNonSyncContext<I, S> for C {}

pub(crate) trait BufferDatagramStateContext<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut>:
    DatagramStateContext<I, C, S>
{
    type BufferSocketStateCtx<'a>: BufferDatagramBoundStateContext<
        I,
        C,
        S,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_sockets_state_buf<
        O,
        F: FnOnce(&mut Self::BufferSocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the datagram sockets.
    fn with_sockets_state_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferSocketStateCtx<'_>,
            &mut SocketsState<I, Self::WeakDeviceId, S>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
}

pub(crate) trait BufferDatagramBoundStateContext<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut>:
    DatagramBoundStateContext<I, C, S>
{
    type BufferIpSocketsCtx<'a>: BufferTransportIpContext<
        I,
        C,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the function with an immutable reference to the datagram sockets.
    fn with_bound_sockets_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
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
    fn with_bound_sockets_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
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

    /// Calls the function with only the inner context.
    fn with_transport_context_buf<O, F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

impl<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut, SC: DatagramStateContext<I, C, S>>
    BufferDatagramStateContext<I, C, S, B> for SC
where
    for<'a> SC::SocketsStateCtx<'a>: BufferDatagramBoundStateContext<I, C, S, B>,
{
    type BufferSocketStateCtx<'a> = SC::SocketsStateCtx<'a>;

    fn with_sockets_state_buf<
        O,
        F: FnOnce(&mut Self::BufferSocketStateCtx<'_>, &SocketsState<I, Self::WeakDeviceId, S>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets_state(self, cb)
    }

    fn with_sockets_state_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferSocketStateCtx<'_>,
            &mut SocketsState<I, Self::WeakDeviceId, S>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_sockets_state_mut(self, cb)
    }
}

impl<I: IpExt, C, S: DatagramSocketSpec, B: BufferMut, SC: DatagramBoundStateContext<I, C, S>>
    BufferDatagramBoundStateContext<I, C, S, B> for SC
where
    for<'a> SC::IpSocketsCtx<'a>: BufferTransportIpContext<I, C, B>,
{
    type BufferIpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;

    fn with_bound_sockets_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
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
    ) -> O {
        Self::with_bound_sockets(self, cb)
    }

    fn with_bound_sockets_mut_buf<
        O,
        F: FnOnce(
            &mut Self::BufferIpSocketsCtx<'_>,
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
    ) -> O {
        Self::with_bound_sockets_mut(self, cb)
    }

    fn with_transport_context_buf<O, F: FnOnce(&mut Self::BufferIpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        Self::with_transport_context(self, cb)
    }
}

/// Types and behavior for datagram socket demultiplexing map.
///
/// `I: Ip` describes the type of packets that can be received by sockets in
/// the map.
pub(crate) trait DatagramSocketMapSpec<I: Ip, D: Id, A: SocketMapAddrSpec>:
    SocketMapStateSpec<ListenerId = Self::ReceivingId, ConnId = Self::ReceivingId>
    + SocketMapConflictPolicy<
        ListenerAddr<I::Addr, D, A::LocalIdentifier>,
        <Self as SocketMapStateSpec>::ListenerSharingState,
        I,
        D,
        A,
    > + SocketMapConflictPolicy<
        ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>,
        <Self as SocketMapStateSpec>::ConnSharingState,
        I,
        D,
        A,
    >
{
    /// The type of IDs stored in a [`BoundSocketMap`] for which this is the
    /// specification.
    ///
    /// When the bound socket map is used to demultiplex an incoming packet,
    /// this is the type of the identifier(s) that will be selected as
    /// candidates for receiving. This can be the same as
    /// [`DatagramSocketSpec::SocketId`] but doesn't have to be. In the case of
    /// dual-stack sockets, for example, an IPv4 socket will have type
    /// `DatagramSocketSpec::SocketId<Ipv4>` but the IPv4 demultiplexing map
    /// might have `ReceivingId=Either<DatagramSocketSpec::SocketId<Ipv4>,
    /// DatagramSocketSpec::SocketId<Ipv6>>` to allow looking up IPv6 sockets
    /// when receiving IPv4 packets.
    type ReceivingId: Clone + Debug;
}

/// Common features of dual-stack sockets that vary by IP version.
///
/// This trait exists to provide per-IP-version associated types that are
/// useful for implementing dual-stack sockets. The types are intentionally
/// asymmetric - `DualStackIpExt::Xxx` has a different shape for the [`Ipv4`]
/// and [`Ipv6`] impls.
// TODO(https://fxbug.dev/21198): Move this somewhere more general once the
// approach is proven to work for datagram sockets. When implementing dual-stack
// TCP, extract a supertrait from `DatagramSocketSpec` as described in the TODO
// below and put that together with this trait.
pub(crate) trait DualStackIpExt: Ip {
    /// The type of socket that can receive an IP packet.
    ///
    /// For `Ipv4`, this is [`EitherIpSocket<S>`], and for `Ipv6` it is just
    /// `S::SocketId<Ipv6>`.
    ///
    /// [`EitherIpSocket<S>]`: [EitherIpSocket]
    // TODO(https://fxbug.dev/21198): Extract the necessary GAT from
    // DatagramSocketSpec into its own trait and use that as the bound here.
    type DualStackReceivingId<S: DatagramSocketSpec>: Clone + Debug + Eq;

    /// The "other" IP version, e.g. [`Ipv4`] for [`Ipv6`] and vice-versa.
    type OtherVersion: Ip
        + IpDeviceStateIpExt
        + DualStackIpExt<OtherVersion = Self>
        + crate::ip::IpExt;

    /// Convert a socket ID into a `Self::DualStackReceivingId`.
    ///
    /// For coherency reasons this can't be a `From` bound on
    /// `DualStackReceivingId`. If more methods are added, consider moving this
    /// to its own dedicated trait that bounds `DualStackReceivingId`.
    fn dual_stack_receiver<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackReceivingId<S>
    where
        Self: IpExt;
}

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
    type DualStackReceivingId<S: DatagramSocketSpec> = EitherIpSocket<S>;

    type OtherVersion = Ipv6;

    fn dual_stack_receiver<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackReceivingId<S> {
        EitherIpSocket::V4(id)
    }
}

impl DualStackIpExt for Ipv6 {
    /// Incoming IPv6 packets may only be received by IPv6 sockets.
    type DualStackReceivingId<S: DatagramSocketSpec> = S::SocketId<Self>;

    type OtherVersion = Ipv4;

    fn dual_stack_receiver<S: DatagramSocketSpec>(
        id: S::SocketId<Self>,
    ) -> Self::DualStackReceivingId<S> {
        id
    }
}

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

    /// The sharing state for a listening socket in a [`BoundSocketMap`].
    ///
    /// This is provided here as an associated type to allow constraining the
    /// associated type on [`DatagramSocketSpec::SocketMapSpec`]. Having a
    /// single type that's not parameterized over the IP version requires that
    /// the `SocketMapSpec::ListenerSharingState` is IP-invariant.
    type ListenerSharingState: Clone + Debug;

    /// The specification for the [`BoundSocketMap`] for a given IP version.
    ///
    /// Describes the per-address and per-socket values held in the
    /// demultiplexing map for a given IP version.
    type SocketMapSpec<I: IpExt + DualStackIpExt, D: device::WeakId>: SocketStateSpec<ListenerState = ListenerState<I::Addr, D>, ConnState = ConnState<I, D>>
        + DatagramSocketMapSpec<
            I,
            D,
            Self::AddrSpec,
            ListenerSharingState = Self::ListenerSharingState,
        >;

    /// The sharing state for a socket that hasn't yet been bound.
    type UnboundSharingState<I: IpExt>: Clone + Debug + Default;

    /// Converts [`Self::SocketId`] to [`DatagramSocketMapSpec::ReceivingId`].
    ///
    /// Constructs a socket identifier to its in-demultiplexing map form. For
    /// protocols with dual-stack sockets, like UDP, implementations should
    /// perform a transformation. Otherwise it should be the identity function.
    fn make_receiving_map_id<I: IpExt, D: device::WeakId>(
        s: Self::SocketId<I>,
    ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::ReceivingId;

    /// The type of serializer returned by [`DatagramSocketSpec::make_packet`]
    /// for a given IP version and buffer type.
    type Serializer<I: Ip, B: BufferMut>: Serializer<Buffer = B>;
    fn make_packet<I: Ip, B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<
            I::Addr,
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    ) -> Self::Serializer<I, B>;

    /// Attempts to allocate a local identifier for a listening socket.
    ///
    /// Returns the identifier on success, or `None` on failure.
    fn try_alloc_listen_identifier<I: IpExt, D: device::WeakId>(
        rng: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>;
}

pub(crate) struct InUseError;

pub(crate) fn create<I: IpExt, S: DatagramSocketSpec, C, SC: DatagramStateContext<I, C, S>>(
    sync_ctx: &mut SC,
) -> S::SocketId<I>
where
{
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
        state.push(SocketState::Unbound(UnboundSocketState::default())).into()
    })
}

#[derive(Debug)]
pub(crate) enum SocketInfo<I: Ip, D, A: SocketMapAddrSpec> {
    Unbound,
    Listener(ListenerAddr<I::Addr, D, A::LocalIdentifier>),
    Connected(ConnAddr<I::Addr, D, A::LocalIdentifier, A::RemoteIdentifier>),
}

pub(crate) fn remove<
    I: IpExt,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
) -> SocketInfo<I, SC::WeakDeviceId, S::AddrSpec>
where
{
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let (ip_options, info) = match state.remove(id.get_key_index()).expect("invalid socket ID")
        {
            SocketState::Unbound(UnboundSocketState { device: _, sharing: _, ip_options }) => {
                (ip_options, SocketInfo::Unbound)
            }
            SocketState::Bound(state) => {
                DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                    sync_ctx,
                    |_sync_ctx, bound, _allocator| match state {
                        BoundSocketState::Listener(state) => {
                            let (state, _sharing, addr) = state;
                            bound
                                .listeners_mut()
                                .remove(&S::make_receiving_map_id(id), &addr)
                                .expect("Invalid UDP listener ID");

                            let ListenerState { ip_options } = state;
                            (ip_options, SocketInfo::Listener(addr))
                        }
                        BoundSocketState::Connected(state) => {
                            let (state, _sharing, addr) = state;
                            bound
                                .conns_mut()
                                .remove(&S::make_receiving_map_id(id), &addr)
                                .expect("UDP connection not found");
                            let ConnState { socket, clear_device_on_disconnect: _, shutdown: _ } =
                                state;
                            (socket.into_options(), SocketInfo::Connected(addr))
                        }
                    },
                )
            }
        };

        let IpOptions { multicast_memberships, hop_limits: _ } = ip_options;
        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            leave_all_joined_groups(sync_ctx, ctx, multicast_memberships)
        });
        info
    })
}

pub(crate) fn get_info<
    I: IpExt,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
) -> SocketInfo<I, SC::WeakDeviceId, S::AddrSpec>
where
{
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) => SocketInfo::Unbound,
            SocketState::Bound(BoundSocketState::Listener(state)) => {
                let (_state, _sharing, addr) = state;
                SocketInfo::Listener(addr.clone())
            }
            SocketState::Bound(BoundSocketState::Connected(state)) => {
                let (_state, _sharing, addr) = state;
                SocketInfo::Connected(addr.clone())
            }
        }
    })
}

pub(crate) fn listen<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    addr: Option<ZonedAddr<I::Addr, SC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>>
where
    S::UnboundSharingState<I>: Clone
        + Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState>,
    <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState: Default,
{
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        listen_inner::<_, C, _, S>(sync_ctx, ctx, state, id, addr, local_id)
    })
}

/// Abstraction for operations over one or two demultiplexing maps.
trait BoundStateHandler<I: IpExt, S: DatagramSocketSpec, D: device::WeakId> {
    /// The type of address that can be checked for insertion.
    type InsertAddr: Clone;
    /// The type of ID that can be inserted.
    type InsertId;
    /// The type of sharing state that can be inserted for listeners.
    type InsertListenerSharingState;

    /// Checks whether an entry could be inserted for the specified address and
    /// identifier.
    ///
    /// Returns `true` if a value could be inserted at the specified address and
    /// local ID, with the provided sharing state; otherwise returns `false`.
    fn is_listener_entry_available(
        &self,
        addr: Self::InsertAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing_state: &Self::InsertListenerSharingState,
    ) -> bool;

    /// Inserts `id` at a listener address or returns an error.
    ///
    /// Inserts the identifier `id` at the listener address for `addr` and
    /// local `identifier` with device `device` and the given sharing state. If
    /// the insertion conflicts with an existing socket, a `LocalAddressError`
    /// is returned.
    fn try_insert_listener(
        &mut self,
        addr: Self::InsertAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: Self::InsertListenerSharingState,
        id: Self::InsertId,
    ) -> Result<(), LocalAddressError>;
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
    type InsertAddr = Option<SpecifiedAddr<I::Addr>>;
    type InsertId =
        <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::ReceivingId;
    type InsertListenerSharingState =
        <S::SocketMapSpec<I, D> as SocketMapStateSpec>::ListenerSharingState;
    fn is_listener_entry_available(
        &self,
        addr: Option<SpecifiedAddr<I::Addr>>,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &Self::InsertListenerSharingState,
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
        addr: Self::InsertAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: Self::InsertListenerSharingState,
        id: Self::InsertId,
    ) -> Result<(), LocalAddressError> {
        try_insert_single_listener(self, addr, identifier, device, sharing, id).map(|_entry| ())
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

struct PairedReceivingIds<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> {
    this: <S::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, S::AddrSpec>>::ReceivingId,
    other: <S::SocketMapSpec<I::OtherVersion, D> as DatagramSocketMapSpec<
        I::OtherVersion,
        D,
        S::AddrSpec,
    >>::ReceivingId,
}

/// Implementation for a pair of demultiplexing maps for different IP versions.
impl<I: IpExt, D: device::WeakId, S: DatagramSocketSpec> BoundStateHandler<I, S, D>
    for PairedSocketMapMut<'_, I, D, S>
{
    type InsertAddr = DualStackUnspecifiedAddr;
    type InsertId = PairedReceivingIds<I, D, S>;
    type InsertListenerSharingState = S::ListenerSharingState;

    fn is_listener_entry_available(
        &self,
        DualStackUnspecifiedAddr: Self::InsertAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        sharing: &Self::InsertListenerSharingState,
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
        DualStackUnspecifiedAddr: Self::InsertAddr,
        identifier: <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        device: Option<D>,
        sharing: Self::InsertListenerSharingState,
        id: Self::InsertId,
    ) -> Result<(), LocalAddressError> {
        let PairedSocketMapMut { bound: this, other_bound: other } = self;
        let PairedReceivingIds { this: this_id, other: other_id } = id;
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
}

fn try_insert_single_listener<
    I: IpExt,
    D: device::WeakId,
    A: SocketMapAddrSpec,
    S: DatagramSocketMapSpec<I, D, A>,
>(
    bound: &mut BoundSocketMap<I, D, A, S>,
    addr: Option<SpecifiedAddr<I::Addr>>,
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

fn try_pick_identifier<
    I: IpExt,
    S: DatagramSocketSpec,
    D: device::WeakId,
    BS: BoundStateHandler<I, S, D>,
    C: RngContext,
>(
    addr: BS::InsertAddr,
    bound: &BS,
    ctx: &mut C,
    sharing: &BS::InsertListenerSharingState,
) -> Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
    S::try_alloc_listen_identifier::<I, D>(ctx, move |identifier| {
        bound
            .is_listener_entry_available(addr.clone(), identifier, sharing)
            .then_some(())
            .ok_or(InUseError)
    })
}

fn try_pick_bound_address<I: IpExt, SC: TransportIpContext<I, C>, C, LI>(
    addr: Option<ZonedAddr<I::Addr, SC::DeviceId>>,
    device: &Option<SC::WeakDeviceId>,
    sync_ctx: &mut SC,
    identifier: LI,
) -> Result<
    (Option<SpecifiedAddr<I::Addr>>, Option<EitherDeviceId<SC::DeviceId, SC::WeakDeviceId>>, LI),
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
            if !addr.is_multicast() {
                let mut assigned_to =
                    TransportIpContext::<I, _>::get_devices_with_assigned_addr(sync_ctx, addr);
                if let Some(device) = &device {
                    if !assigned_to.any(|d| device == &EitherDeviceId::Strong(d)) {
                        return Err(LocalAddressError::AddressMismatch);
                    }
                } else {
                    if !assigned_to.any(|_: SC::DeviceId| true) {
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

#[derive(GenericOverIp)]
enum TryUnmapResult<I: Ip + DualStackIpExt, D> {
    /// The address does not have an un-mapped representation.
    ///
    /// This spits back the input address unmodified.
    CannotBeUnmapped(ZonedAddr<I::Addr, D>),
    /// The address in the other stack that corresponds to the input.
    ///
    /// Since [`ZonedAddr`] is guaranteed to hold a specified address, this must
    /// hold an `Option<ZonedAddr>`. Since `::FFFF:0.0.0.0` is a legal
    /// IPv4-mapped IPv6 address, this allows us to represent it as the
    /// unspecified IPv4 address.
    Mapped(Option<ZonedAddr<<I::OtherVersion as Ip>::Addr, D>>),
}

/// Try to convert a specified address into the address that maps to it from
/// the other stack.
///
/// This is an IP-generic function that tries to invert the
/// IPv4-to-IPv4-mapped-IPv6 conversion that is performed by
/// [`Ipv4Addr::to_ipv6_mapped`].
///
/// The only inputs that will produce [`TryUnmapResult::Mapped`] are
/// IPv4-mapped IPv6 addresses. All other inputs will produce
/// [`TryUnmapResult::CannotBeUnmapped`].
fn try_unmap<A: IpAddress, D>(addr: ZonedAddr<A, D>) -> TryUnmapResult<A::Version, D>
where
    A::Version: DualStackIpExt,
{
    <A::Version as Ip>::map_ip(
        addr,
        |v4| TryUnmapResult::CannotBeUnmapped(v4),
        |v6| match v6.addr().to_ipv4_mapped() {
            Some(v4) => TryUnmapResult::Mapped(SpecifiedAddr::new(v4).map(ZonedAddr::Unzoned)),
            None => TryUnmapResult::CannotBeUnmapped(v6),
        },
    )
}

fn listen_inner<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramBoundStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    state: &mut SocketsState<I, SC::WeakDeviceId, S>,
    id: S::SocketId<I>,
    addr: Option<ZonedAddr<I::Addr, SC::DeviceId>>,
    local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
) -> Result<(), Either<ExpectedUnboundError, LocalAddressError>>
where
    S::UnboundSharingState<I>: Clone
        + Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState>,
{
    /// Possible operations that might be performed, depending on whether the
    /// socket state spec supports dual-stack operation and what the address
    /// looks like.
    #[derive(Debug, GenericOverIp)]
    enum BoundOperation<'a, I: Ip + IpExt, DS: DeviceIdContext<AnyDevice>> {
        /// Bind to the "any" address on both stacks.
        DualStackAnyAddr(&'a mut DS),
        /// Bind to a non-dual-stack address only on the current stack.
        OnlyCurrentStack(Option<ZonedAddr<I::Addr, DS::DeviceId>>),
        /// Bind to an address only on the other stack.
        OnlyOtherStack(&'a mut DS, Option<ZonedAddr<<I::OtherVersion as Ip>::Addr, DS::DeviceId>>),
    }

    let mut entry = match state.entry(id.get_key_index()) {
        IdMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
        IdMapEntry::Occupied(o) => o,
    };
    let UnboundSocketState { device, sharing, ip_options } = match entry.get() {
        SocketState::Unbound(state) => state,
        SocketState::Bound(_) => return Err(Either::Left(ExpectedUnboundError)),
    };

    let dual_stack = sync_ctx
        .dual_stack_context()
        .and_then(|ds| ds.dual_stack_enabled(entry.get()).then_some(ds));

    let bound_operation: BoundOperation<'_, I, _> = match (dual_stack, addr) {
        // No dual-stack support, so only bind on the current stack.
        (None, None) => BoundOperation::OnlyCurrentStack(None),
        // Dual-stack support and unspecified address means bind as dual-stack.
        (Some(dual_stack), None) => BoundOperation::DualStackAnyAddr(dual_stack),
        // There is dual-stack support and the address is not unspecified so how
        // to proceed is going to depend on the value of `addr`.
        (Some(dual_stack), Some(addr)) => match try_unmap(addr) {
            // `addr` can't be represented in the other stack.
            TryUnmapResult::CannotBeUnmapped(addr) => BoundOperation::OnlyCurrentStack(Some(addr)),
            // There's a representation in the other stack, so use that.
            TryUnmapResult::Mapped(addr) => BoundOperation::OnlyOtherStack(dual_stack, addr),
        },
        // No dual-stack support, so check the address is allowed in the current
        // stack.
        (None, Some(addr)) => match try_unmap(addr) {
            // The address is only representable in the current stack.
            TryUnmapResult::CannotBeUnmapped(addr) => BoundOperation::OnlyCurrentStack(Some(addr)),
            // The address has a representation in the other stack but there's
            // no dual-stack support!
            TryUnmapResult::Mapped(_addr) => {
                let _: Option<ZonedAddr<<I::OtherVersion as Ip>::Addr, _>> = _addr;
                return Err(Either::Right(LocalAddressError::CannotBindToAddress));
            }
        },
    };

    fn try_bind_single_stack<
        I: IpExt,
        S: DatagramSocketSpec,
        SC: TransportIpContext<I, C>,
        C: RngContext,
    >(
        sync_ctx: &mut SC,
        ctx: &mut C,
        bound: &mut BoundSocketMap<
            I,
            SC::WeakDeviceId,
            S::AddrSpec,
            S::SocketMapSpec<I, SC::WeakDeviceId>,
        >,
        addr: Option<ZonedAddr<I::Addr, SC::DeviceId>>,
        device: &Option<SC::WeakDeviceId>,
        local_id: Option<<S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier>,
        id: <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerId,
        sharing: S::ListenerSharingState,
    ) -> Result<
        ListenerAddr<
            I::Addr,
            SC::WeakDeviceId,
            <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        >,
        LocalAddressError,
    > {
        let identifier = match local_id {
            Some(id) => Some(id),
            None => try_pick_identifier::<I, S, _, _, _>(
                addr.as_ref().map(ZonedAddr::addr),
                bound,
                ctx,
                &sharing,
            ),
        }
        .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
        let (addr, device, identifier) =
            try_pick_bound_address::<I, _, _, _>(addr, device, sync_ctx, identifier)?;
        let weak_device = device.map(|d| d.as_weak(sync_ctx).into_owned());

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

    let bound_addr = match bound_operation {
        BoundOperation::OnlyCurrentStack(addr) => {
            sync_ctx.with_bound_sockets_mut(|sync_ctx, bound, _allocator| {
                let id = S::make_receiving_map_id(id);

                try_bind_single_stack::<I, S, _, _>(
                    sync_ctx,
                    ctx,
                    bound,
                    addr,
                    device,
                    local_id,
                    id,
                    sharing.clone().into(),
                )
            })
        }
        BoundOperation::OnlyOtherStack(sync_ctx, addr) => {
            let id = sync_ctx.to_other_receiving_id(id);
            sync_ctx
                .with_other_bound_sockets_mut(|sync_ctx, other_bound| {
                    try_bind_single_stack::<_, S, _, _>(
                        sync_ctx,
                        ctx,
                        other_bound,
                        addr,
                        device,
                        local_id,
                        id,
                        sharing.clone().into(),
                    )
                })
                .map(
                    |ListenerAddr {
                         ip: ListenerIpAddr { addr, identifier },
                         device: weak_device,
                     }| {
                        let addr =
                            addr.map_or(<I::OtherVersion as Ip>::UNSPECIFIED_ADDRESS, |a| a.get());
                        let addr = SpecifiedAddr::new(sync_ctx.from_other_ip_addr(addr));
                        ListenerAddr {
                            ip: ListenerIpAddr { addr, identifier },
                            device: weak_device,
                        }
                    },
                )
        }
        BoundOperation::DualStackAnyAddr(sync_ctx) => {
            let ids = PairedReceivingIds {
                this: S::make_receiving_map_id(id.clone()),
                other: sync_ctx.to_other_receiving_id(id),
            };
            sync_ctx.with_both_bound_sockets_mut(|sync_ctx, bound, other_bound| {
                let mut bound_pair = PairedSocketMapMut { bound, other_bound };
                let sharing = sharing.clone().into();

                let identifier = match local_id {
                    Some(id) => Some(id),
                    None => try_pick_identifier::<I, S, _, _, _>(
                        DualStackUnspecifiedAddr,
                        &bound_pair,
                        ctx,
                        &sharing,
                    ),
                }
                .ok_or(LocalAddressError::FailedToAllocateLocalPort)?;
                let (addr, device, identifier) =
                    try_pick_bound_address::<I, _, _, _>(None, device, sync_ctx, identifier)?;
                let weak_device = device.map(|d| d.as_weak(sync_ctx).into_owned());

                BoundStateHandler::<_, S, _>::try_insert_listener(
                    &mut bound_pair,
                    DualStackUnspecifiedAddr,
                    identifier,
                    weak_device.clone(),
                    sharing,
                    ids,
                )
                .map(|()| ListenerAddr {
                    ip: ListenerIpAddr { addr, identifier },
                    device: weak_device,
                })
            })
        }
    }
    .map_err(Either::Right)?;

    // Replace the unbound state only after we're sure the
    // insertion has succeeded.
    *entry.get_mut() = SocketState::Bound(BoundSocketState::Listener((
        // TODO(https://fxbug.dev/131970): Remove this clone().
        { ListenerState { ip_options: ip_options.clone() } },
        sharing.clone().into(),
        bound_addr,
    )));
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
}

pub(crate) fn connect<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    remote_ip: ZonedAddr<I::Addr, SC::DeviceId>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    proto: IpProto,
) -> Result<(), ConnectError>
where
    <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState:
        Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ConnSharingState>,
    S::UnboundSharingState<I>:
        Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ConnSharingState>,
{
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        enum BoundMapSocketState<LA, LS, LI, CA, CS, CI> {
            Listener { addr: LA, sharing: LS, id: LI },
            Connected { addr: CA, sharing: CS, id: CI },
        }
        let mut entry = match state.entry(id.get_key_index()) {
            IdMapEntry::Vacant(_) => panic!("socket ID {:?} is invalid", id),
            IdMapEntry::Occupied(o) => o,
        };

        let (local_ip, local_id, sharing, device, bound_map_socket_state, ip_options) = match entry
            .get()
        {
            SocketState::Unbound(state) => {
                let UnboundSocketState { device, sharing: unbound_sharing, ip_options } = state;
                (None, None, unbound_sharing.clone().into(), device.as_ref(), None, ip_options)
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener(state) => {
                    let (
                        ListenerState { ip_options },
                        listener_sharing,
                        listener_addr @ ListenerAddr {
                            ip: ListenerIpAddr { addr, identifier },
                            device,
                        },
                    ): &(ListenerState<_, _>, _, _) = state;
                    (
                        addr.as_ref(),
                        Some(identifier),
                        listener_sharing.clone().into(),
                        device.as_ref(),
                        Some(BoundMapSocketState::Listener {
                            addr: listener_addr.clone(),
                            sharing: listener_sharing.clone(),
                            id: S::make_receiving_map_id(id.clone()),
                        }),
                        ip_options,
                    )
                }
                BoundSocketState::Connected(state) => {
                    let (
                        ConnState { socket, clear_device_on_disconnect, shutdown: _ },
                        conn_sharing,
                        original_addr @ ConnAddr {
                            ip: ConnIpAddr { local: (ip, identifier), remote: _ },
                            device,
                        },
                    ): &(ConnState<_, _>, _, _) = state;
                    (
                        Some(ip),
                        Some(identifier),
                        conn_sharing.clone(),
                        device.as_ref().and_then(|d| (!clear_device_on_disconnect).then_some(d)),
                        Some(BoundMapSocketState::Connected {
                            addr: original_addr.clone(),
                            sharing: conn_sharing.clone(),
                            id: S::make_receiving_map_id(id.clone()),
                        }),
                        socket.options(),
                    )
                }
            },
        };
        let (remote_ip, socket_device) =
            crate::transport::resolve_addr_with_device(remote_ip, device.cloned())?;
        let clear_device_on_disconnect = device.is_none() && socket_device.is_some();

        let (mut ip_sock, bound_addr) =
            DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                sync_ctx,
                |sync_ctx, bound, allocator| {
                    let ip_sock = IpSocketHandler::<I, _>::new_ip_socket(
                        sync_ctx,
                        ctx,
                        socket_device.as_ref().map(|d| d.as_ref()),
                        local_ip.cloned(),
                        remote_ip,
                        proto.into(),
                        Default::default(),
                    )
                    .map_err(|(e, _ip_options)| e)?;
                    let local_ip = *ip_sock.local_ip();
                    let remote_ip = *ip_sock.remote_ip();
                    let local_id = match local_id {
                        Some(id) => id.clone(),
                        None => allocator
                            .try_alloc_local_id(
                                bound,
                                ctx,
                                DatagramFlowId {
                                    local_ip,
                                    remote_ip,
                                    remote_id: remote_id.clone(),
                                },
                            )
                            .ok_or(ConnectError::CouldNotAllocateLocalPort)?,
                    };

                    let c = ConnAddr {
                        ip: ConnIpAddr {
                            local: (local_ip, local_id),
                            remote: (remote_ip, remote_id),
                        },
                        device: ip_sock.device().cloned(),
                    };
                    // Now that all the other checks have been done, actually remove
                    // the ID from the socket map. It will be restored on failure.
                    if let Some(addr_and_id) = &bound_map_socket_state {
                        match addr_and_id {
                            BoundMapSocketState::Listener { addr, sharing: _, id } => {
                                bound.listeners_mut().remove(id, addr)
                            }
                            BoundMapSocketState::Connected { addr, sharing: _, id } => {
                                bound.conns_mut().remove(id, addr)
                            }
                        }
                        .expect("presence verified earlier")
                    };

                    match bound.conns_mut().try_insert(
                        c,
                        sharing.clone(),
                        S::make_receiving_map_id(id),
                    ) {
                        Ok(bound_entry) => Ok((ip_sock, bound_entry.get_addr().clone())),
                        Err((
                            InsertError::Exists
                            | InsertError::IndirectConflict
                            | InsertError::ShadowerExists
                            | InsertError::ShadowAddrExists,
                            _sharing,
                        )) => {
                            match bound_map_socket_state {
                                None => (),
                                Some(BoundMapSocketState::Listener { addr, sharing, id }) => {
                                    let _entry = bound
                                        .listeners_mut()
                                        .try_insert(addr, sharing, id)
                                        .expect("reinserting just-removed listener failed");
                                }
                                Some(BoundMapSocketState::Connected { addr, sharing, id }) => {
                                    let _entry = bound
                                        .conns_mut()
                                        .try_insert(addr, sharing, id)
                                        .expect("reinserting just-removed connection failed");
                                }
                            };
                            Err(ConnectError::SockAddrConflict)
                        }
                    }
                },
            )?;
        // Update the bound state after there's no further path for errors.
        // TODO(https://fxbug.dev/131970): Remove this clone.
        *ip_sock.options_mut() = ip_options.clone();
        *entry.get_mut() = SocketState::Bound(BoundSocketState::Connected((
            ConnState {
                socket: ip_sock,
                clear_device_on_disconnect,
                shutdown: Shutdown::default(),
            },
            sharing,
            bound_addr,
        )));
        Ok(())
    })
}

/// A connected socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
pub struct ExpectedConnError;

/// An unbound socket was expected.
#[derive(Copy, Clone, Debug, Default, Eq, GenericOverIp, PartialEq)]
pub struct ExpectedUnboundError;

pub(crate) fn disconnect_connected<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
) -> Result<(), ExpectedConnError>
where
    <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ConnSharingState:
        Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState>,
{
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let mut entry = match state.entry(id.get_key_index()) {
            IdMapEntry::Vacant(_) => panic!("unbound ID {:?} is invalid", id),
            IdMapEntry::Occupied(o) => o,
        };
        let (conn_state, sharing, addr): &mut (
            _,
            <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ConnSharingState,
            _,
        ) = match entry.get_mut() {
            SocketState::Unbound(_) | SocketState::Bound(BoundSocketState::Listener(_)) => {
                return Err(ExpectedConnError)
            }
            SocketState::Bound(BoundSocketState::Connected(state)) => state,
        };
        let ConnState { socket, clear_device_on_disconnect, shutdown: _ } = conn_state;

        let id = S::make_receiving_map_id(id);
        let bound_addr = DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
            sync_ctx,
            |_sync_ctx, bound, _allocator| {
                let _bound_addr =
                    bound.conns_mut().remove(&id, &addr).expect("connection not found");

                let ConnAddr {
                    ip: ConnIpAddr { local: (local_ip, identifier), remote: _ },
                    mut device,
                } = addr.clone();
                if *clear_device_on_disconnect {
                    device = None
                }

                let addr = ListenerAddr {
                    ip: ListenerIpAddr { addr: Some(local_ip), identifier },
                    device,
                };

                let bound_entry = bound
                    .listeners_mut()
                    .try_insert(addr, sharing.clone().into(), id)
                    .expect("inserting listener for disconnected socket failed");
                let bound_addr = bound_entry.get_addr().clone();
                bound_addr
            },
        );
        // TODO(https://fxbug.dev/131970): Remove this clone.
        let ip_options = socket.options().clone();
        *entry.get_mut() = SocketState::Bound(BoundSocketState::Listener((
            ListenerState { ip_options },
            sharing.clone().into(),
            bound_addr,
        )));
        Ok(())
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
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
        let state = match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) | SocketState::Bound(BoundSocketState::Listener(_)) => {
                return Err(ExpectedConnError)
            }
            SocketState::Bound(BoundSocketState::Connected(state)) => state,
        };

        let (ConnState { socket: _, clear_device_on_disconnect: _, shutdown }, _sharing, _addr) =
            state;
        let (shutdown_send, shutdown_receive) = match which {
            ShutdownType::Send => (true, false),
            ShutdownType::Receive => (false, true),
            ShutdownType::SendAndReceive => (true, true),
        };
        let Shutdown { send, receive } = shutdown;
        *send |= shutdown_send;
        *receive |= shutdown_receive;
        Ok(())
    })
}

pub(crate) fn get_shutdown_connected<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> Option<ShutdownType> {
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) | SocketState::Bound(BoundSocketState::Listener(_)) => {
                return None
            }
            SocketState::Bound(BoundSocketState::Connected(state)) => state,
        };

        let (ConnState { socket: _, clear_device_on_disconnect: _, shutdown }, _sharing, _addr) =
            state;
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
    /// The socket is not connected,
    NotConnected(B),
    /// The socket is not writeable.
    NotWriteable(B),
    /// There was a problem sending the IP packet.
    IpSock(S, IpSockSendError),
}

pub(crate) fn send_conn<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: BufferDatagramStateContext<I, C, S, B>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    body: B,
) -> Result<(), SendError<B, S::Serializer<I, B>>> {
    sync_ctx.with_sockets_state_buf(|sync_ctx, state| {
        let state = match state.get(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(_) | SocketState::Bound(BoundSocketState::Listener(_)) => {
                return Err(SendError::NotConnected(body))
            }
            SocketState::Bound(BoundSocketState::Connected(state)) => state,
        };

        let (
            ConnState {
                socket,
                clear_device_on_disconnect: _,
                shutdown: Shutdown { send: shutdown_send, receive: _ },
            },
            _sharing,
            addr,
        ) = state;
        if *shutdown_send {
            return Err(SendError::NotWriteable(body));
        }

        let ConnAddr { ip, device: _ } = addr;

        sync_ctx.with_transport_context_buf(|sync_ctx| {
            sync_ctx
                .send_ip_packet(ctx, &socket, S::make_packet(body, &ip), None)
                .map_err(|(serializer, send_error)| SendError::IpSock(serializer, send_error))
        })
    })
}

#[derive(Debug)]
pub(crate) enum SendToError<B, S> {
    NotWriteable(B),
    Zone(B, ZonedAddressError),
    CreateAndSend(S, IpSockCreateAndSendError),
}

pub(crate) fn send_to<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: BufferDatagramStateContext<I, C, S, B>,
    S: DatagramSocketSpec,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    remote_ip: ZonedAddr<I::Addr, SC::DeviceId>,
    remote_identifier: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    proto: <I as IpProtoExt>::Proto,
    body: B,
) -> Result<(), Either<(B, LocalAddressError), SendToError<B, S::Serializer<I, B>>>>
where
    S::UnboundSharingState<I>: Clone
        + Into<<S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState>,
    <S::SocketMapSpec<I, SC::WeakDeviceId> as SocketMapStateSpec>::ListenerSharingState: Default,
{
    sync_ctx.with_sockets_state_mut_buf(|sync_ctx, state| {
        match listen_inner(sync_ctx, ctx, state, id.clone(), None, None) {
            Ok(()) | Err(Either::Left(ExpectedUnboundError)) => (),
            Err(Either::Right(e)) => return Err(Either::Left((body, e))),
        };
        let state = match state.get(id.get_key_index()).expect("no such socket") {
            SocketState::Unbound(_) => panic!("expected bound socket"),
            SocketState::Bound(state) => state,
        };
        let (local, device, ip_options, proto) = match state {
            BoundSocketState::Connected(state) => {
                let (
                    ConnState { socket, clear_device_on_disconnect: _, shutdown },
                    _,
                    ConnAddr { ip: ConnIpAddr { local, remote: _ }, device },
                ): &(
                    _,
                    <S::SocketMapSpec<I, _> as SocketMapStateSpec>::ConnSharingState,
                    _,
                ) = state;

                let Shutdown { send: shutdown_write, receive: _ } = shutdown;
                if *shutdown_write {
                    return Err(Either::Right(SendToError::NotWriteable(body)));
                }
                let (local_ip, local_id) = local;

                ((Some(*local_ip), local_id.clone()), device, socket.options(), socket.proto())
            }
            BoundSocketState::Listener(state) => {
                // TODO(https://fxbug.dev/92447) If `local_ip` is `None`, and so
                // `new_ip_socket` picks a local IP address for us, it may cause problems
                // when we don't match the bound listener addresses. We should revisit
                // whether that check is actually necessary.
                //
                // Also, if the local IP address is a multicast address this function should
                // probably fail and `send_udp_conn_to` must be used instead.
                let (
                    ListenerState { ip_options },
                    _,
                    ListenerAddr {
                        ip: ListenerIpAddr { addr: local_ip, identifier: local_port },
                        device,
                    },
                ): &(_, S::ListenerSharingState, _) = state;
                ((*local_ip, local_port.clone()), device, ip_options, proto)
            }
        };

        sync_ctx
            .with_transport_context_buf(|sync_ctx| {
                send_oneshot::<_, S, _, _, _>(
                    sync_ctx,
                    ctx,
                    local,
                    remote_ip,
                    remote_identifier,
                    device,
                    ip_options,
                    proto,
                    body,
                )
            })
            .map_err(Either::Right)
    })
}

fn send_oneshot<
    I: IpExt,
    S: DatagramSocketSpec,
    SC: BufferIpSocketHandler<I, C, B>,
    C,
    B: BufferMut,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    (local_ip, local_id): (
        Option<SpecifiedAddr<I::Addr>>,
        <S::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
    ),
    remote_ip: ZonedAddr<I::Addr, SC::DeviceId>,
    remote_id: <S::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    device: &Option<SC::WeakDeviceId>,
    ip_options: &IpOptions<I::Addr, SC::WeakDeviceId>,
    proto: <I as IpProtoExt>::Proto,
    body: B,
) -> Result<(), SendToError<B, S::Serializer<I, B>>> {
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

pub(crate) fn set_device<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    new_device: Option<&SC::DeviceId>,
) -> Result<(), SocketError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        match state.get_mut(id.get_key_index()).expect("invalid socket ID") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { ref mut device, sharing: _, ip_options: _ } = state;
                *device = new_device.map(|d| sync_ctx.downgrade_device_id(d));
                Ok(())
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener(state) => {
                    // Don't allow changing the device if one of the IP addresses in the socket
                    // address vector requires a zone (scope ID).
                    let (_, _, addr): &(
                        <S::SocketMapSpec<I, _> as SocketStateSpec>::ListenerState,
                        S::ListenerSharingState,
                        _,
                    ) = state;
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
                        return Err(SocketError::Local(LocalAddressError::Zone(
                            ZonedAddressError::DeviceZoneMismatch,
                        )));
                    }

                    let new_addr = DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                        sync_ctx,
                        |sync_ctx, bound, _allocator| {
                            let entry = bound
                                .listeners_mut()
                                .entry(&S::make_receiving_map_id(id.clone()), addr)
                                .unwrap_or_else(|| panic!("invalid listener ID {:?}", id));
                            let new_addr = ListenerAddr {
                                device: new_device.map(|d| sync_ctx.downgrade_device_id(d)),
                                ..addr.clone()
                            };
                            let new_entry = entry.try_update_addr(new_addr).map_err(
                                |(ExistsError {}, _entry)| LocalAddressError::AddressInUse,
                            )?;
                            Ok::<_, LocalAddressError>(new_entry.get_addr().clone())
                        },
                    )?;

                    let (_, _, addr): &mut (
                        <S::SocketMapSpec<I, _> as SocketStateSpec>::ListenerState,
                        S::ListenerSharingState,
                        _,
                    ) = state;
                    *addr = new_addr;
                    Ok(())
                }
                BoundSocketState::Connected(bound_state) => {
                    let (state, _, addr): &(
                        _,
                        <S::SocketMapSpec<I, _> as SocketMapStateSpec>::ConnSharingState,
                        _,
                    ) = bound_state;
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
                    let (new_addr, mut new_socket) =
                        DatagramBoundStateContext::<I, _, _>::with_bound_sockets_mut(
                            sync_ctx,
                            |sync_ctx, bound, _allocator| {
                                let new_socket = sync_ctx
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
                                    .entry(&S::make_receiving_map_id(id.clone()), addr)
                                    .unwrap_or_else(|| panic!("invalid conn ID {:?}", id));
                                let new_addr = ConnAddr {
                                    device: new_socket.device().cloned(),
                                    ..addr.clone()
                                };

                                let entry = match entry.try_update_addr(new_addr) {
                                    Err((ExistsError, _entry)) => {
                                        return Err(SocketError::Local(
                                            LocalAddressError::AddressInUse,
                                        ))
                                    }
                                    Ok(entry) => entry,
                                };
                                Ok((entry.get_addr().clone(), new_socket))
                            },
                        )?;
                    // Since the move was successful, replace the old socket with
                    // the new one but move the options over.
                    let (state, _, addr): &mut (
                        _,
                        <S::SocketMapSpec<I, _> as SocketMapStateSpec>::ConnSharingState,
                        _,
                    ) = bound_state;
                    let ConnState { socket, clear_device_on_disconnect, shutdown: _ } = state;
                    let _: IpOptions<_, _> = new_socket.replace_options(socket.take_options());
                    *socket = new_socket;
                    *addr = new_addr;

                    // If this operation explicitly sets the device for the socket, it
                    // should no longer be cleared on disconnect.
                    if new_device.is_some() {
                        *clear_device_on_disconnect = false;
                    }
                    Ok(())
                }
            },
        }
    })
}

pub(crate) fn get_bound_device<
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> Option<SC::WeakDeviceId> {
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        let (_, device): (&IpOptions<_, _>, _) = get_options_device(state, id);
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
    /// The requested membership change had no effect (tried to leave a group
    /// without joining, or to join a group again).
    NoMembershipChange,
    /// The socket is bound to a device that doesn't match the one specified.
    WrongDevice,
}

/// Selects the interface for the given remote address, optionally with a
/// constraint on the source address.
fn pick_interface_for_addr<
    A: IpAddress,
    S: DatagramSocketSpec,
    C: DatagramStateNonSyncContext<A::Version, S>,
    SC: DatagramBoundStateContext<A::Version, C, S>,
>(
    sync_ctx: &mut SC,
    _remote_addr: MulticastAddr<A>,
    source_addr: Option<SpecifiedAddr<A>>,
) -> Result<SC::DeviceId, SetMulticastMembershipError>
where
    A::Version: IpExt,
{
    sync_ctx.with_transport_context(|sync_ctx| {
        if let Some(source_addr) = source_addr {
            let mut devices = TransportIpContext::<A::Version, _>::get_devices_with_assigned_addr(
                sync_ctx,
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
    I: IpExt,
    C: DatagramStateNonSyncContext<I, S>,
    SC: DatagramStateContext<I, C, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: S::SocketId<I>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, SC::DeviceId>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    sync_ctx.with_sockets_state_mut(|sync_ctx, state| {
        let (_, bound_device): (&IpOptions<_, _>, _) = get_options_device(state, id.clone());

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

        let ip_options = get_options_mut(state, id);

        let Some(strong_interface) = interface.as_strong(sync_ctx) else {
            return Err(SetMulticastMembershipError::DeviceDoesNotExist);
        };

        let IpOptions { multicast_memberships, hop_limits: _ } = ip_options;
        let change = multicast_memberships
            .apply_membership_change(multicast_group, &interface.as_weak(sync_ctx), want_membership)
            .ok_or(SetMulticastMembershipError::NoMembershipChange)?;

        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            match change {
                MulticastMembershipChange::Join => {
                    MulticastMembershipHandler::<I, _>::join_multicast_group(
                        sync_ctx,
                        ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
                MulticastMembershipChange::Leave => {
                    MulticastMembershipHandler::<I, _>::leave_multicast_group(
                        sync_ctx,
                        ctx,
                        &strong_interface,
                        multicast_group,
                    )
                }
            }
        });

        Ok(())
    })
}

fn get_options_device<I: IpExt, D: device::WeakId, S: DatagramSocketSpec>(
    state: &SocketsState<I, D, S>,
    id: S::SocketId<I>,
) -> (&IpOptions<I::Addr, D>, &Option<D>) {
    match state.get(id.get_key_index()).expect("socket not found") {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device, sharing: _ } = state;
            (ip_options, device)
        }
        SocketState::Bound(BoundSocketState::Listener(state)) => {
            let (ListenerState { ip_options }, _, ListenerAddr { device, ip: _ }): &(
                _,
                S::ListenerSharingState,
                _,
            ) = state;
            (ip_options, device)
        }
        SocketState::Bound(BoundSocketState::Connected(state)) => {
            let (
                ConnState { socket, clear_device_on_disconnect: _, shutdown: _ },
                _,
                ConnAddr { device, ip: _ },
            ): &(
                _,
                <S::SocketMapSpec<I, _> as SocketMapStateSpec>::ConnSharingState,
                _,
            ) = state;
            (socket.options(), device)
        }
    }
}

fn get_options_mut<I: IpExt, D: device::WeakId, S: DatagramSocketSpec>(
    state: &mut SocketsState<I, D, S>,
    id: S::SocketId<I>,
) -> &mut IpOptions<I::Addr, D>
where
    S::SocketId<I>: EntryKey,
{
    match state.get_mut(id.get_key_index()).expect("socket not found") {
        SocketState::Unbound(state) => {
            let UnboundSocketState { ip_options, device: _, sharing: _ } = state;
            ip_options
        }
        SocketState::Bound(BoundSocketState::Listener(state)) => {
            let (ListenerState { ip_options }, _, _): &mut (
                _,
                S::ListenerSharingState,
                ListenerAddr<_, _, _>,
            ) = state;
            ip_options
        }
        SocketState::Bound(BoundSocketState::Connected(state)) => {
            let (ConnState { socket, clear_device_on_disconnect: _, shutdown: _ }, _, _): &mut (
                _,
                <S::SocketMapSpec<I, _> as SocketMapStateSpec>::ConnSharingState,
                ConnAddr<_, _, _, _>,
            ) = state;
            socket.options_mut()
        }
    }
}

pub(crate) fn update_ip_hop_limit<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &mut C,
    id: S::SocketId<I>,
    update: impl FnOnce(&mut SocketHopLimits),
) {
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
        let options = get_options_mut(state, id);

        update(&mut options.hop_limits)
    })
}

pub(crate) fn get_ip_hop_limits<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec,
>(
    sync_ctx: &mut SC,
    _ctx: &C,
    id: S::SocketId<I>,
) -> HopLimits {
    sync_ctx.with_sockets_state(|sync_ctx, state| {
        let (options, device) = get_options_device(state, id);
        let IpOptions { hop_limits, multicast_memberships: _ } = options;
        let device = device.as_ref().and_then(|d| sync_ctx.upgrade_weak_device_id(d));
        DatagramBoundStateContext::<I, _, _>::with_transport_context(sync_ctx, |sync_ctx| {
            hop_limits.get_limits_with_defaults(
                &TransportIpContext::<I, _>::get_default_hop_limits(sync_ctx, device.as_ref()),
            )
        })
    })
}

pub(crate) fn update_sharing<
    I: IpExt,
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec<UnboundSharingState<I> = Sharing>,
    Sharing: Clone,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
    new_sharing: Sharing,
) -> Result<(), ExpectedUnboundError> {
    sync_ctx.with_sockets_state_mut(|_sync_ctx, state| {
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
    SC: DatagramStateContext<I, C, S>,
    C: DatagramStateNonSyncContext<I, S>,
    S: DatagramSocketSpec<UnboundSharingState<I> = Sharing>,
    Sharing: Clone,
>(
    sync_ctx: &mut SC,
    id: S::SocketId<I>,
) -> Sharing
where
    S::SocketMapSpec<I, SC::WeakDeviceId>: DatagramSocketMapSpec<
        I,
        SC::WeakDeviceId,
        S::AddrSpec,
        ListenerSharingState = Sharing,
        ConnSharingState = Sharing,
    >,
{
    sync_ctx.with_sockets_state(|_sync_ctx, state| {
        match state.get(id.get_key_index()).expect("socket not found") {
            SocketState::Unbound(state) => {
                let UnboundSocketState { device: _, sharing, ip_options: _ } = state;
                sharing
            }
            SocketState::Bound(state) => match state {
                BoundSocketState::Listener(state) => {
                    let (_, sharing, _): &(ListenerState<_, _>, _, ListenerAddr<_, _, _>) = state;
                    sharing
                }
                BoundSocketState::Connected(state) => {
                    let (_, sharing, _): &(ConnState<_, _>, _, ConnAddr<_, _, _, _>) = state;
                    sharing
                }
            },
        }
        .clone()
    })
}

#[cfg(test)]
mod test {
    use core::{convert::Infallible as Never, ops::DerefMut as _};

    use alloc::{vec, vec::Vec};
    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use derivative::Derivative;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use packet::Buf;
    use test_case::test_case;

    use crate::{
        context::testutil::{FakeNonSyncCtx, Wrapped, WrappedFakeSyncCtx},
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
    };

    use super::*;

    trait DatagramIpExt: Ip + IpExt + IpDeviceStateIpExt + TestIpExt {}
    impl<I: Ip + IpExt + IpDeviceStateIpExt + TestIpExt> DatagramIpExt for I {}

    #[derive(Debug)]
    enum FakeAddrSpec {}

    impl SocketMapAddrSpec for FakeAddrSpec {
        type LocalIdentifier = u8;
        type RemoteIdentifier = char;
    }

    enum FakeStateSpec {}

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Tag;

    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    struct Sharing;

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

    impl<I: IpExt, D: crate::device::Id> SocketStateSpec for (FakeStateSpec, I, D) {
        type ConnState = ConnState<I, D>;
        type ListenerState = ListenerState<I::Addr, D>;
    }

    impl DatagramSocketSpec for FakeStateSpec {
        type AddrSpec = FakeAddrSpec;
        type SocketId<I: IpExt> = Id;
        type UnboundSharingState<I: IpExt> = Sharing;
        type SocketMapSpec<I: IpExt, D: device::WeakId> = (Self, I, D);
        type ListenerSharingState = Sharing;

        fn make_receiving_map_id<I: IpExt, D: device::WeakId>(
            s: Self::SocketId<I>,
        ) -> <Self::SocketMapSpec<I, D> as DatagramSocketMapSpec<I, D, Self::AddrSpec>>::ReceivingId
        {
            s
        }

        type Serializer<I: Ip, B: BufferMut> = B;
        fn make_packet<I: Ip, B: BufferMut>(
            body: B,
            _addr: &ConnIpAddr<
                I::Addr,
                <FakeAddrSpec as SocketMapAddrSpec>::LocalIdentifier,
                <FakeAddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
            >,
        ) -> Self::Serializer<I, B> {
            body
        }
        fn try_alloc_listen_identifier<I: Ip, D: device::WeakId>(
            _ctx: &mut impl RngContext,
            is_available: impl Fn(u8) -> Result<(), InUseError>,
        ) -> Option<u8> {
            (0..=u8::MAX).find(|i| is_available(*i).is_ok())
        }
    }

    impl<I: IpExt, D: device::WeakId> DatagramSocketMapSpec<I, D, FakeAddrSpec>
        for (FakeStateSpec, I, D)
    {
        type ReceivingId = Id;
    }

    impl<A, I: IpExt, D: device::WeakId> SocketMapConflictPolicy<A, Sharing, I, D, FakeAddrSpec>
        for (FakeStateSpec, I, D)
    {
        fn check_insert_conflicts(
            _new_sharing_state: &Sharing,
            _addr: &A,
            _socketmap: &SocketMap<AddrVec<I, D, FakeAddrSpec>, Bound<Self>>,
        ) -> Result<(), InsertError> {
            // Addresses are completely independent and shadowing doesn't cause
            // conflicts.
            Ok(())
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
            struct Wrap<'a, I: Ip + IpExt, D: FakeStrongDeviceId>(
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
            struct Wrap<'a, I: Ip + IpExt, D: FakeStrongDeviceId>(
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

    type FakeInnerSyncCtx<D> = crate::context::testutil::FakeSyncCtx<
        FakeDualStackIpSocketCtx<D>,
        DualStackSendIpPacketMeta<D>,
        D,
    >;

    type FakeSyncCtx<I, D> =
        Wrapped<FakeSocketsState<I, D>, Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>>;

    impl<I: IpExt, D: FakeStrongDeviceId + 'static> FakeSyncCtx<I, D> {
        fn new_with_sockets(state: FakeSocketsState<I, D>, bound: FakeBoundSockets<D>) -> Self {
            Self {
                outer: state,
                inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                    FakeDualStackIpSocketCtx::default(),
                    bound,
                ),
            }
        }
    }

    impl<I: DatagramIpExt + IpLayerIpExt, D: FakeStrongDeviceId + 'static>
        DatagramStateContext<I, FakeNonSyncCtx<(), (), ()>, FakeStateSpec> for FakeSyncCtx<I, D>
    {
        type SocketsStateCtx<'a> = Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>;

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

    impl<I: Ip + IpExt + IpDeviceStateIpExt, D: FakeStrongDeviceId + 'static>
        DatagramBoundStateContext<I, FakeNonSyncCtx<(), (), ()>, FakeStateSpec>
        for Wrapped<FakeBoundSockets<D>, FakeInnerSyncCtx<D>>
    {
        type IpSocketsCtx<'a> = FakeInnerSyncCtx<D>;
        type LocalIdAllocator = ();
        type DualStackContext = UninstantiableDualStackContext<I, FakeStateSpec, Self>;

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

        fn dual_stack_context(&mut self) -> Option<&mut Self::DualStackContext> {
            // Dual-stack operation will be tested through UDP since that's the
            // only datagram socket type that will use it.
            None
        }
    }

    impl<I: Ip + IpExt + IpDeviceStateIpExt, D: FakeStrongDeviceId + 'static>
        LocalIdentifierAllocator<
            I,
            FakeWeakDeviceId<D>,
            FakeAddrSpec,
            FakeNonSyncCtx<(), (), ()>,
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
            _ctx: &mut FakeNonSyncCtx<(), (), ()>,
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
    fn set_get_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        const EXPECTED_HOP_LIMITS: HopLimits = HopLimits {
            unicast: const_unwrap_option(NonZeroU8::new(45)),
            multicast: const_unwrap_option(NonZeroU8::new(23)),
        };

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(EXPECTED_HOP_LIMITS.unicast),
                multicast: Some(EXPECTED_HOP_LIMITS.multicast),
            }
        });

        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            EXPECTED_HOP_LIMITS
        );
    }

    #[ip_test]
    fn set_get_device_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: FakeSocketsState::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig::<_, SpecifiedAddr<I::Addr>> {
                    device: FakeDeviceId,
                    local_ips: Default::default(),
                    remote_ips: Default::default(),
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();

        let HopLimits { mut unicast, multicast } = DEFAULT_HOP_LIMITS;
        unicast = unicast.checked_add(1).unwrap();
        {
            let ip_socket_ctx = sync_ctx.inner.inner.get_ref();
            let device_state: &IpDeviceState<_, I> =
                ip_socket_ctx.get_device_state(&FakeDeviceId).as_ref();
            let mut default_hop_limit = device_state.default_hop_limit.write();
            let default_hop_limit = default_hop_limit.deref_mut();
            assert_ne!(*default_hop_limit, unicast);
            *default_hop_limit = unicast;
        }
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            HopLimits { unicast, multicast }
        );

        // If the device is removed, use default hop limits.
        AsMut::<FakeIpDeviceIdCtx<_>>::as_mut(&mut sync_ctx.inner.inner.get_mut())
            .set_device_removed(FakeDeviceId, true);
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn default_hop_limits<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId>::new_with_sockets(
            Default::default(),
            Default::default(),
        );
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = SocketHopLimits {
                unicast: Some(const_unwrap_option(NonZeroU8::new(1))),
                multicast: Some(const_unwrap_option(NonZeroU8::new(1))),
            }
        });

        // The limits no longer match the default.
        assert_ne!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );

        // Clear the hop limits set on the socket.
        update_ip_hop_limit(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), |limits| {
            *limits = Default::default()
        });

        // The values should be back at the defaults.
        assert_eq!(
            get_ip_hop_limits(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            DEFAULT_HOP_LIMITS
        );
    }

    #[ip_test]
    fn bind_device_unbound<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx =
            FakeSyncCtx::<I, _>::new_with_sockets(Default::default(), Default::default());
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let unbound = create(&mut sync_ctx);

        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), Some(&FakeDeviceId)).unwrap();
        assert_eq!(
            get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound.clone()),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        set_device(&mut sync_ctx, &mut non_sync_ctx, unbound.clone(), None).unwrap();
        assert_eq!(get_bound_device(&mut sync_ctx, &non_sync_ctx, unbound), None);
    }

    #[ip_test]
    fn send_to_binds_unbound<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, FakeDeviceId> {
            outer: Default::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![I::FAKE_CONFIG.remote_ip],
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let socket = create(&mut sync_ctx);
        let body = Buf::new(Vec::new(), ..);

        send_to(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            I::FAKE_CONFIG.remote_ip.into(),
            'a',
            IpProto::Udp.into(),
            body,
        )
        .expect("succeeds");
        assert_matches!(
            get_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Listener(_)
        );
    }

    #[ip_test]
    fn send_to_no_route_still_binds<I: Ip + DatagramIpExt + IpLayerIpExt>() {
        let mut sync_ctx = FakeSyncCtx::<I, _> {
            outer: Default::default(),
            inner: WrappedFakeSyncCtx::with_inner_and_outer_state(
                FakeDualStackIpSocketCtx::new([FakeDeviceConfig {
                    device: FakeDeviceId,
                    local_ips: vec![I::FAKE_CONFIG.local_ip],
                    remote_ips: vec![],
                }]),
                Default::default(),
            ),
        };
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let socket = create(&mut sync_ctx);
        let body = Buf::new(Vec::new(), ..);

        assert_matches!(
            send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                I::FAKE_CONFIG.remote_ip.into(),
                'a',
                IpProto::Udp.into(),
                body,
            ),
            Err(Either::Right(SendToError::CreateAndSend(_, _)))
        );
        assert_matches!(
            get_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Listener(_)
        );
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
        let mut non_sync_ctx = FakeNonSyncCtx::<(), (), ()>::default();

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
