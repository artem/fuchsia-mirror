// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The transport layer.
//!
//! # Listeners and connections
//!
//! Some transport layer protocols (notably TCP and UDP) follow a common pattern
//! with respect to registering listeners and connections. There are some
//! subtleties here that are worth pointing out.
//!
//! ## Connections
//!
//! A connection has simpler semantics than a listener. It is bound to a single
//! local address and port and a single remote address and port. By virtue of
//! being bound to a local address, it is also bound to a local interface. This
//! means that, regardless of the entries in the forwarding table, all traffic
//! on that connection will always egress over the same interface. [^1] This
//! also means that, if the interface's address changes, any connections bound
//! to it are severed.
//!
//! ## Listeners
//!
//! A listener, on the other hand, can be bound to any number of local addresses
//! (although it is still always bound to a particular port). From the
//! perspective of this crate, there are two ways of registering a listener:
//! - By specifying one or more local addresses, the listener will be bound to
//!   each of those local addresses.
//! - By specifying zero local addresses, the listener will be bound to all
//!   addresses. These are referred to in our documentation as "wildcard
//!   listeners".
//!
//! The algorithm for figuring out what listener to deliver a packet to is as
//! follows: If there is any listener bound to the specific local address and
//! port addressed in the packet, deliver the packet to that listener.
//! Otherwise, if there is a wildcard listener bound the port addressed in the
//! packet, deliver the packet to that listener. This implies that if a listener
//! is removed which was bound to a particular local address, it can "uncover" a
//! wildcard listener bound to the same port, allowing traffic which would
//! previously have been delivered to the normal listener to now be delivered to
//! the wildcard listener.
//!
//! If desired, clients of this crate can implement a different mechanism for
//! registering listeners on all local addresses - enumerate every local
//! address, and then specify all of the local addresses when registering the
//! listener. This approach will not support shadowing, as a different listener
//! binding to the same port will explicitly conflict with the existing
//! listener, and will thus be rejected. In other words, from the perspective of
//! this crate's API, such listeners will appear like normal listeners that just
//! happen to bind all of the addresses, rather than appearing like wildcard
//! listeners.
//!
//! [^1]: It is an open design question as to whether incoming traffic on the
//!       connection will be accepted from a different interface. This is part
//!       of the "weak host model" vs "strong host model" discussion.

mod integration;
pub mod tcp;
pub mod udp;

use derivative::Derivative;
use lock_order::{lock::RwLockFor, Locked};
use net_types::{
    ip::{IpAddress, Ipv4, Ipv6},
    ScopeableAddress, SpecifiedAddr, ZonedAddr,
};

use crate::{
    convert::OwnedOrCloned,
    device::WeakDeviceId,
    error::ZonedAddressError,
    ip::{EitherDeviceId, IpExt},
    socket::address::SocketZonedIpAddr,
    sync::{RwLockReadGuard, RwLockWriteGuard},
    transport::{
        tcp::TcpState,
        udp::{UdpState, UdpStateBuilder},
    },
    BindingsTypes, NonSyncContext, SyncCtx,
};

/// A builder for transport layer state.
#[derive(Default, Clone)]
pub struct TransportStateBuilder {
    udp: UdpStateBuilder,
}

impl TransportStateBuilder {
    /// Get the builder for the UDP state.
    #[cfg(test)]
    pub(crate) fn udp_builder(&mut self) -> &mut UdpStateBuilder {
        &mut self.udp
    }

    pub(crate) fn build_with_ctx<C: NonSyncContext>(self, ctx: &mut C) -> TransportLayerState<C> {
        let now = ctx.now();
        let mut rng = ctx.rng();
        TransportLayerState {
            udpv4: self.udp.clone().build(),
            udpv6: self.udp.build(),
            tcpv4: TcpState::new(now, &mut rng),
            tcpv6: TcpState::new(now, &mut rng),
        }
    }
}

/// The state associated with the transport layer.
pub(crate) struct TransportLayerState<BT: BindingsTypes> {
    udpv4: UdpState<Ipv4, WeakDeviceId<BT>>,
    udpv6: UdpState<Ipv6, WeakDeviceId<BT>>,
    tcpv4: TcpState<Ipv4, WeakDeviceId<BT>, BT>,
    tcpv6: TcpState<Ipv6, WeakDeviceId<BT>, BT>,
}

impl<BT: BindingsTypes> TransportLayerState<BT> {
    fn tcp_state<I: IpExt>(&self) -> &TcpState<I, WeakDeviceId<BT>, BT> {
        I::map_ip((), |()| &self.tcpv4, |()| &self.tcpv6)
    }
}

impl<C: NonSyncContext> RwLockFor<crate::lock_ordering::UdpBoundMap<Ipv4>> for SyncCtx<C> {
    type Data = udp::BoundSockets<Ipv4, WeakDeviceId<C>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.udpv4.sockets.bound.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.udpv4.sockets.bound.write()
    }
}

impl<C: NonSyncContext> RwLockFor<crate::lock_ordering::UdpBoundMap<Ipv6>> for SyncCtx<C> {
    type Data = udp::BoundSockets<Ipv6, WeakDeviceId<C>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.udpv6.sockets.bound.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.udpv6.sockets.bound.write()
    }
}

impl<C: NonSyncContext> RwLockFor<crate::lock_ordering::UdpSocketsTable<Ipv4>> for SyncCtx<C> {
    type Data = udp::SocketsState<Ipv4, WeakDeviceId<C>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.udpv4.sockets.sockets_state.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.udpv4.sockets.sockets_state.write()
    }
}

impl<C: NonSyncContext> RwLockFor<crate::lock_ordering::UdpSocketsTable<Ipv6>> for SyncCtx<C> {
    type Data = udp::SocketsState<Ipv6, WeakDeviceId<C>>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data> where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data> where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.transport.udpv6.sockets.sockets_state.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.transport.udpv6.sockets.sockets_state.write()
    }
}

/// The identifier for timer events in the transport layer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) enum TransportLayerTimerId<C: crate::NonSyncContext> {
    Tcp(tcp::socket::TimerId<WeakDeviceId<C>, C>),
}

/// Handle a timer event firing in the transport layer.
pub(crate) fn handle_timer<NonSyncCtx: NonSyncContext>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, crate::lock_ordering::Unlocked>,
    ctx: &mut NonSyncCtx,
    id: TransportLayerTimerId<NonSyncCtx>,
) {
    match id {
        TransportLayerTimerId::Tcp(id) => tcp::socket::handle_timer(sync_ctx, ctx, id),
    }
}

impl<C: crate::NonSyncContext> From<tcp::socket::TimerId<WeakDeviceId<C>, C>>
    for TransportLayerTimerId<C>
{
    fn from(id: tcp::socket::TimerId<WeakDeviceId<C>, C>) -> Self {
        TransportLayerTimerId::Tcp(id)
    }
}

impl_timer_context!(
    C: NonSyncContext,
    TransportLayerTimerId<C>,
    tcp::socket::TimerId<WeakDeviceId<C>, C>,
    TransportLayerTimerId::Tcp(id),
    id
);

fn maybe_with_zone<A: IpAddress, D>(
    addr: SpecifiedAddr<A>,
    device: impl OwnedOrCloned<Option<D>>,
) -> SocketZonedIpAddr<A, D> {
    // Invariant guaranteed by bind/connect/reconnect: if a socket has an
    // address that must have a zone, it has a bound device.
    if let Some(addr_and_zone) = crate::socket::try_into_null_zoned(&addr) {
        let device = device.into_owned().unwrap_or_else(|| {
            unreachable!("connected address has zoned address {:?} but no device", addr)
        });
        ZonedAddr::Zoned(addr_and_zone.map_zone(|()| device)).into()
    } else {
        ZonedAddr::Unzoned(addr).into()
    }
}

/// Returns the address and device that should be used for a socket.
///
/// Given an address for a socket and an optional device that the socket is
/// already bound on, returns the address and device that should be used
/// for the socket. If `addr` and `device` require inconsistent devices,
/// or if `addr` requires a zone but there is none specified (by `addr` or
/// `device`), an error is returned.
pub(crate) fn resolve_addr_with_device<
    IA: IpAddress,
    A: ScopeableAddress + AsRef<SpecifiedAddr<IA>>,
    S: PartialEq,
    W: PartialEq + PartialEq<S>,
>(
    addr: ZonedAddr<A, S>,
    device: Option<W>,
) -> Result<(A, Option<EitherDeviceId<S, W>>), ZonedAddressError> {
    let (addr, zone) = addr.into_addr_zone();
    let device = match (zone, device) {
        (Some(zone), Some(device)) => {
            if device != zone {
                return Err(ZonedAddressError::DeviceZoneMismatch);
            }
            Some(EitherDeviceId::Strong(zone))
        }
        (Some(zone), None) => Some(EitherDeviceId::Strong(zone)),
        (None, Some(device)) => Some(EitherDeviceId::Weak(device)),
        (None, None) => {
            if crate::socket::must_have_zone(addr.as_ref()) {
                return Err(ZonedAddressError::RequiredZoneNotProvided);
            } else {
                None
            }
        }
    };
    Ok((addr, device))
}
