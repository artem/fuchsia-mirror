// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the entry point of TCP packets, by directing them into the correct
//! state machine.

use alloc::collections::hash_map;
use assert_matches::assert_matches;
use core::{convert::TryFrom, fmt::Debug, num::NonZeroU16};
use tracing::{error, trace};

use net_types::{ip::IpAddress, SpecifiedAddr};
use packet::{BufferMut, BufferView as _, EmptyBuf, InnerPacketBuilder as _, Serializer};
use packet_formats::{
    ip::IpProto,
    tcp::{
        TcpFlowAndSeqNum, TcpOptionsTooLongError, TcpParseArgs, TcpSegment, TcpSegmentBuilder,
        TcpSegmentBuilderWithOptions,
    },
};
use thiserror::Error;

use crate::{
    device::{self, DeviceIdContext as _},
    error::NotFoundError,
    ip::{
        socket::{DefaultSendOptions, DeviceIpSocketHandler as _, IpSocketHandler as _, MmsError},
        EitherDeviceId, IpExt, IpLayerIpExt, IpTransportContext, TransportIpContext,
        TransportReceiveError,
    },
    socket::{
        address::{
            AddrIsMappedError, AddrVecIter, ConnAddr, ConnIpAddr, ListenerAddr, ListenerIpAddr,
            SocketIpAddr,
        },
        AddrVec, InsertError,
    },
    trace_duration,
    transport::tcp::{
        self,
        buffer::SendPayload,
        segment::{Options, Segment},
        seqnum::UnscaledWindowSize,
        socket::{
            do_send_inner, isn::IsnGenerator, BoundSocketState, Connection, DemuxState,
            DemuxSyncContext as _, HandshakeStatus, Listener, ListenerAddrState,
            ListenerSharingState, MaybeListener, NonSyncContext, PrimaryRc, SeqNum, SocketHandler,
            SyncContext, TcpBindingsTypes, TcpIpTransportContext, TcpPortSpec, TcpSocketId,
            TcpSocketSetEntry, TcpSocketState,
        },
        state::{BufferProvider, Closed, DataAcked, Initial, State, TimeWait},
        BufferSizes, ConnectionError, Control, IcmpErrorCode, Mss, SocketOptions,
    },
};

impl<BT: TcpBindingsTypes> BufferProvider<BT::ReceiveBuffer, BT::SendBuffer> for BT {
    type ActiveOpen = BT::ListenerNotifierOrProvidedBuffers;

    type PassiveOpen = BT::ReturnedBuffers;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (BT::ReceiveBuffer, BT::SendBuffer, Self::PassiveOpen) {
        BT::new_passive_open_buffers(buffer_sizes)
    }
}

impl<I, C, SC> IpTransportContext<I, C, SC> for TcpIpTransportContext
where
    I: IpLayerIpExt,
    I::ErrorCode: Into<IcmpErrorCode>,
    C: NonSyncContext<I, SC::WeakDeviceId>
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as TcpBindingsTypes>::ListenerNotifierOrProvidedBuffers,
            PassiveOpen = <C as TcpBindingsTypes>::ReturnedBuffers,
        >,
    SC: SyncContext<I, C>,
{
    fn receive_icmp_error(
        sync_ctx: &mut SC,
        ctx: &mut C,
        _device: &SC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        mut original_body: &[u8],
        err: I::ErrorCode,
    ) {
        let mut buffer = &mut original_body;
        let Some(flow_and_seqnum) = buffer.take_obj_front::<TcpFlowAndSeqNum>() else {
            error!("received an ICMP error but its body is less than 8 bytes");
            return;
        };

        let Some(original_src_ip) = original_src_ip else { return };
        let Some(original_src_port) = NonZeroU16::new(flow_and_seqnum.src_port()) else { return };
        let Some(original_dst_port) = NonZeroU16::new(flow_and_seqnum.dst_port()) else { return };
        let original_seqnum = SeqNum::new(flow_and_seqnum.sequence_num());

        SocketHandler::on_icmp_error(
            sync_ctx,
            ctx,
            original_src_ip,
            original_dst_ip,
            original_src_port,
            original_dst_port,
            original_seqnum,
            err.into(),
        );
    }

    fn receive_ip_packet<B: BufferMut>(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        remote_ip: I::RecvSrcAddr,
        local_ip: SpecifiedAddr<I::Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        let remote_ip = match SpecifiedAddr::new(remote_ip.into()) {
            None => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: source address unspecified, dropping the packet");
                return Ok(());
            }
            Some(src_ip) => src_ip,
        };
        let remote_ip: SocketIpAddr<_> = match remote_ip.try_into() {
            Ok(remote_ip) => remote_ip,
            Err(AddrIsMappedError {}) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: source address is mapped (ipv4-mapped-ipv6), dropping the packet");
                return Ok(());
            }
        };
        let local_ip: SocketIpAddr<_> = match local_ip.try_into() {
            Ok(local_ip) => local_ip,
            Err(AddrIsMappedError {}) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: local address is mapped (ipv4-mapped-ipv6), dropping the packet");
                return Ok(());
            }
        };
        let packet = match buffer
            .parse_with::<_, TcpSegment<_>>(TcpParseArgs::new(remote_ip.addr(), local_ip.addr()))
        {
            Ok(packet) => packet,
            Err(err) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: failed parsing incoming packet {:?}", err);
                return Ok(());
            }
        };
        let local_port = packet.dst_port();
        let remote_port = packet.src_port();
        let incoming = match Segment::try_from(packet) {
            Ok(segment) => segment,
            Err(err) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: malformed segment {:?}", err);
                return Ok(());
            }
        };
        let conn_addr =
            ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) };

        handle_incoming_packet::<_, B, _, _>(sync_ctx, ctx, conn_addr, device, incoming);
        Ok(())
    }
}

fn handle_incoming_packet<I, B, C, SC>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    conn_addr: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    incoming_device: &SC::DeviceId,
    incoming: Segment<&[u8]>,
) where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext<I, SC::WeakDeviceId>
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as TcpBindingsTypes>::ListenerNotifierOrProvidedBuffers,
            PassiveOpen = <C as TcpBindingsTypes>::ReturnedBuffers,
        >,
    SC: SyncContext<I, C>,
{
    trace_duration!(ctx, "tcp::handle_incoming_packet");
    let mut tw_reuse = None;

    let mut addrs_to_search = AddrVecIter::<I, SC::WeakDeviceId, TcpPortSpec>::with_device(
        conn_addr.into(),
        sync_ctx.downgrade_device_id(incoming_device),
    );

    let found_socket = loop {
        let sock =
            sync_ctx.with_demux(|demux| lookup_socket::<I, SC, C>(demux, &mut addrs_to_search));
        match sock {
            None => break false,
            Some(SocketLookupResult::Connection((conn_id, conn_addr))) => {
                // It is not possible to have two same connections that
                // share the same local and remote IPs and ports.
                assert_eq!(tw_reuse, None);

                match sync_ctx.with_socket_mut_transport_demux(
                    &conn_id,
                    |sync_ctx, socket_state| {
                        try_handle_incoming_for_connection::<I, SC, C, B>(
                            sync_ctx,
                            ctx,
                            conn_addr.clone(),
                            &conn_id,
                            socket_state,
                            incoming,
                        )
                    },
                ) {
                    ConnectionIncomingSegmentDisposition::FoundSocket => break true,
                    ConnectionIncomingSegmentDisposition::Destroy => {
                        tcp::socket::destroy_socket(sync_ctx, ctx, conn_id);
                        break true;
                    }
                    ConnectionIncomingSegmentDisposition::ReuseCandidateForListener => {
                        tw_reuse = Some((conn_id.clone(), conn_addr));
                    }
                }
            }
            Some(SocketLookupResult::Listener((id, _listener_addr))) => {
                match sync_ctx.with_socket_mut_isn_transport_demux(
                    &id,
                    |sync_ctx, socket_state, isn| {
                        try_handle_incoming_for_listener::<I, SC, C, B>(
                            sync_ctx,
                            ctx,
                            isn,
                            socket_state,
                            incoming,
                            conn_addr,
                            incoming_device,
                            &mut tw_reuse,
                        )
                    },
                ) {
                    ListenerIncomingSegmentDisposition::FoundSocket => break true,
                    ListenerIncomingSegmentDisposition::ConflictingConnection => {
                        // We're about to rewind the lookup. If we got a
                        // conflicting connection it means tw_reuse has been
                        // removed from the demux state and we need to destroy
                        // it.
                        if let Some((tw_reuse, _)) = tw_reuse.take() {
                            tcp::socket::destroy_socket(sync_ctx, ctx, tw_reuse);
                        }

                        // Reset the address vector iterator and go again, a
                        // conflicting connection was found.
                        addrs_to_search =
                            AddrVecIter::<I, SC::WeakDeviceId, TcpPortSpec>::with_device(
                                conn_addr.into(),
                                sync_ctx.downgrade_device_id(incoming_device),
                            );
                    }
                    ListenerIncomingSegmentDisposition::NoMatchingSocket => (),
                    ListenerIncomingSegmentDisposition::NewConnection(primary) => {
                        // If we have a new connection, we need to add it to the
                        // set of all sockets.

                        // First things first, if we got here then tw_reuse is
                        // gone so we need to destroy it.
                        if let Some((tw_reuse, _)) = tw_reuse.take() {
                            tcp::socket::destroy_socket(sync_ctx, ctx, tw_reuse);
                        }

                        // Now put the new connection into the socket map.
                        //
                        // Note that there's a possible subtle race here where
                        // another thread could have already operated further on
                        // this connection and marked it for destruction which
                        // puts the entry in the DOA state, if we see that we
                        // must immediately destroy the socket after having put
                        // it in the map.
                        let id = TcpSocketId(PrimaryRc::clone_strong(&primary));
                        let to_destroy = sync_ctx.with_all_sockets_mut(move |all_sockets| {
                            let insert_entry = TcpSocketSetEntry::Primary(primary);
                            match all_sockets.entry(id) {
                                hash_map::Entry::Vacant(v) => {
                                    let _: &mut _ = v.insert(insert_entry);
                                    None
                                }
                                hash_map::Entry::Occupied(mut o) => {
                                    // We're holding on to the primary ref, the
                                    // only possible state here should be a DOA
                                    // entry.
                                    assert_matches!(
                                        core::mem::replace(o.get_mut(), insert_entry),
                                        TcpSocketSetEntry::DeadOnArrival
                                    );
                                    Some(o.key().clone())
                                }
                            }
                        });
                        // NB: we're releasing and reaquiring the
                        // all_sockets_mut lock here for the convenience of not
                        // needing different versions of `destroy_socket`. This
                        // should be fine because the race this is solving
                        // should not be common. If we have correct thread
                        // attribution per flow it should effectively become
                        // impossible so we go for code simplicity here.
                        if let Some(to_destroy) = to_destroy {
                            tcp::socket::destroy_socket(sync_ctx, ctx, to_destroy);
                        }
                        break true;
                    }
                }
            }
        }
    };

    let ConnIpAddr { local: (local_ip, _), remote: (remote_ip, _) } = conn_addr;
    if !found_socket {
        // There is no existing TCP state, pretend it is closed
        // and generate a RST if needed.
        // Per RFC 793 (https://tools.ietf.org/html/rfc793#page-21):
        // CLOSED is fictional because it represents the state when
        // there is no TCB, and therefore, no connection.
        if let Some(seg) = (Closed { reason: None::<Option<ConnectionError>> }.on_segment(incoming))
        {
            match sync_ctx.send_oneshot_ip_packet(
                ctx,
                None,
                Some(local_ip),
                remote_ip,
                IpProto::Tcp.into(),
                DefaultSendOptions,
                |_addr| tcp_serialize_segment(seg, conn_addr),
                None,
            ) {
                Ok(()) => {}
                Err((err, DefaultSendOptions)) => {
                    // TODO(https://fxbug.dev/101993): Increment the counter.
                    trace!("cannot construct an ip socket to respond RST: {:?}, ignoring", err);
                }
            }
        }
    }
}

enum SocketLookupResult<I: IpExt, D: device::WeakId, BT: TcpBindingsTypes> {
    Connection((TcpSocketId<I, D, BT>, ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, D>)),
    Listener((TcpSocketId<I, D, BT>, ListenerAddr<ListenerIpAddr<I::Addr, NonZeroU16>, D>)),
}

fn lookup_socket<I, SC, C>(
    DemuxState { socketmap, .. }: &DemuxState<I, SC::WeakDeviceId, C>,
    addrs_to_search: &mut AddrVecIter<I, SC::WeakDeviceId, TcpPortSpec>,
) -> Option<SocketLookupResult<I, SC::WeakDeviceId, C>>
where
    I: IpLayerIpExt,
    C: NonSyncContext<I, SC::WeakDeviceId>,
    SC: SyncContext<I, C>,
{
    addrs_to_search.find_map(|addr| {
        match addr {
            // Connections are always searched before listeners because they
            // are more specific.
            AddrVec::Conn(conn_addr) => {
                socketmap.conns().get_by_addr(&conn_addr).map(|conn_addr_state| {
                    SocketLookupResult::Connection((conn_addr_state.id(), conn_addr))
                })
            }
            AddrVec::Listen(listener_addr) => {
                // If we have a listener and the incoming segment is a SYN, we
                // allocate a new connection entry in the demuxer.
                // TODO(https://fxbug.dev/101992): Support SYN cookies.

                socketmap
                    .listeners()
                    .get_by_addr(&listener_addr)
                    .and_then(|addr_state| match addr_state {
                        ListenerAddrState::ExclusiveListener(id) => Some(id.clone()),
                        ListenerAddrState::Shared { listener: Some(id), bound: _ } => {
                            Some(id.clone())
                        }
                        ListenerAddrState::ExclusiveBound(_)
                        | ListenerAddrState::Shared { listener: None, bound: _ } => None,
                    })
                    .map(|id| SocketLookupResult::Listener((id, listener_addr)))
            }
        }
    })
}

enum ConnectionIncomingSegmentDisposition {
    FoundSocket,
    ReuseCandidateForListener,
    Destroy,
}

enum ListenerIncomingSegmentDisposition<S> {
    FoundSocket,
    ConflictingConnection,
    NoMatchingSocket,
    NewConnection(S),
}

/// Tries to handle the incoming segment by providing it to a connected socket.
///
/// Returns `FoundSocket` if the segment was handled; Otherwise,
/// `ReuseCandidateForListener` will be returned if there is a defunct socket
/// that is currently in TIME_WAIT, which is ready to be reused if there is an
/// active listener listening on the port.
fn try_handle_incoming_for_connection<I, SC, C, B>(
    sync_ctx: &mut SC::IpTransportAndDemuxCtx<'_>,
    ctx: &mut C,
    conn_addr: ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, SC::WeakDeviceId>,
    conn_id: &TcpSocketId<I, SC::WeakDeviceId, C>,
    socket_state: &mut TcpSocketState<I, SC::WeakDeviceId, C>,
    incoming: Segment<&[u8]>,
) -> ConnectionIncomingSegmentDisposition
where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext<I, SC::WeakDeviceId>
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as TcpBindingsTypes>::ListenerNotifierOrProvidedBuffers,
            PassiveOpen = <C as TcpBindingsTypes>::ReturnedBuffers,
        >,
    SC: SyncContext<I, C>,
{
    let (conn, _, addr) = assert_matches!(
        socket_state,
        TcpSocketState::Bound(BoundSocketState::Connected(conn)) => conn,
        "invalid socket ID"
    );

    let Connection {
        accept_queue,
        state,
        ip_sock,
        defunct,
        socket_options,
        soft_error: _,
        handshake_status,
    } = conn;

    // Per RFC 9293 Section 3.6.1:
    //   When a connection is closed actively, it MUST linger in the TIME-WAIT
    //   state for a time 2xMSL (Maximum Segment Lifetime) (MUST-13). However,
    //   it MAY accept a new SYN from the remote TCP endpoint to reopen the
    //   connection directly from TIME-WAIT state (MAY-2), if it:
    //
    //   (1) assigns its initial sequence number for the new connection to be
    //       larger than the largest sequence number it used on the previous
    //       connection incarnation, and
    //   (2) returns to TIME-WAIT state if the SYN turns out to be an old
    //       duplicate.
    if *defunct && incoming.contents.control() == Some(Control::SYN) && incoming.ack.is_none() {
        if let State::TimeWait(TimeWait {
            last_seq: _,
            last_ack,
            last_wnd: _,
            last_wnd_scale: _,
            expiry: _,
        }) = state
        {
            if !incoming.seq.before(*last_ack) {
                return ConnectionIncomingSegmentDisposition::ReuseCandidateForListener;
            }
        }
    }

    let (reply, passive_open, data_acked) =
        state.on_segment::<_, C>(incoming, ctx.now(), socket_options, *defunct);

    let mut confirm_reachable = || {
        let remote_ip = *ip_sock.remote_ip();
        let device = ip_sock.device().and_then(|weak| sync_ctx.upgrade_weak_device_id(weak));
        sync_ctx.confirm_reachable_with_destination(ctx, remote_ip.into(), device.as_ref());
    };

    match data_acked {
        DataAcked::Yes => confirm_reachable(),
        DataAcked::No => {}
    }

    match state {
        State::Listen(_) => {
            unreachable!("has an invalid status: {:?}", conn.state)
        }
        State::SynSent(_) | State::SynRcvd(_) => {
            assert_eq!(*handshake_status, HandshakeStatus::Pending)
        }
        State::Established(_)
        | State::FinWait1(_)
        | State::FinWait2(_)
        | State::Closing(_)
        | State::CloseWait(_)
        | State::LastAck(_)
        | State::TimeWait(_) => {
            if handshake_status
                .update_if_pending(HandshakeStatus::Completed { reported: accept_queue.is_some() })
            {
                confirm_reachable();
            }
        }
        State::Closed(Closed { reason }) => {
            if *defunct {
                // If the incoming segment caused the state machine to
                // enter Closed state, and the user has already promised
                // not to use the connection again, we can remove the
                // connection from the socketmap.
                let _: Option<_> = ctx.cancel_timer(conn_id.downgrade());
                sync_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
                    assert_matches!(socketmap.conns_mut().remove(&conn_id, &addr), Ok(()))
                });
                return ConnectionIncomingSegmentDisposition::Destroy;
            }
            let _: bool = handshake_status.update_if_pending(match reason {
                None => HandshakeStatus::Completed { reported: accept_queue.is_some() },
                Some(_err) => HandshakeStatus::Aborted,
            });
        }
    }

    if let Some(seg) = reply {
        let body = tcp_serialize_segment(seg, conn_addr.ip);
        match sync_ctx.send_ip_packet(ctx, &ip_sock, body, None) {
            Ok(()) => {}
            Err((body, err)) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: failed to send ip packet {:?}: {:?}", body, err)
            }
        }
    }

    // Send any enqueued data, if there is any.
    do_send_inner(conn_id, conn, addr, sync_ctx, ctx);

    // Enqueue the connection to the associated listener
    // socket's accept queue.
    if let Some(passive_open) = passive_open {
        let accept_queue = conn.accept_queue.as_ref().expect("no accept queue but passive open");
        accept_queue.notify_ready(conn_id, passive_open);
    }

    // We found a valid connection for the segment.
    ConnectionIncomingSegmentDisposition::FoundSocket
}

/// Tries to handle an incoming segment by passing it to a listening socket.
///
/// Returns `FoundSocket` if the segment was handled, otherwise `NoMatchingSocket`.
fn try_handle_incoming_for_listener<I, SC, C, B>(
    sync_ctx: &mut SC::IpTransportAndDemuxCtx<'_>,
    ctx: &mut C,
    isn: &IsnGenerator<C::Instant>,
    socket_state: &mut TcpSocketState<I, SC::WeakDeviceId, C>,
    incoming: Segment<&[u8]>,
    incoming_addrs: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    incoming_device: &SC::DeviceId,
    tw_reuse: &mut Option<(
        TcpSocketId<I, SC::WeakDeviceId, C>,
        ConnAddr<ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>, SC::WeakDeviceId>,
    )>,
) -> ListenerIncomingSegmentDisposition<PrimaryRc<I, SC::WeakDeviceId, C>>
where
    I: IpLayerIpExt,
    B: BufferMut,
    C: NonSyncContext<I, SC::WeakDeviceId>
        + BufferProvider<
            C::ReceiveBuffer,
            C::SendBuffer,
            ActiveOpen = <C as TcpBindingsTypes>::ListenerNotifierOrProvidedBuffers,
            PassiveOpen = <C as TcpBindingsTypes>::ReturnedBuffers,
        >,
    SC: SyncContext<I, C>,
{
    let (maybe_listener, sharing, listener_addr) = assert_matches!(
        socket_state,
        TcpSocketState::Bound(BoundSocketState::Listener(l)) => l,
        "invalid socket ID"
    );

    let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } =
        incoming_addrs;

    let Listener { accept_queue, backlog, buffer_sizes, socket_options } = match maybe_listener {
        MaybeListener::Bound(_bound) => {
            // If the socket is only bound, but not listening.
            return ListenerIncomingSegmentDisposition::NoMatchingSocket;
        }
        MaybeListener::Listener(listener) => listener,
    };

    // Note that this checks happens at the very beginning, before we try to
    // reuse the connection in TIME-WAIT, this is because we need to store the
    // reused connection in the accept queue so we have to respect its limit.
    if accept_queue.len() == backlog.get() {
        // TODO(https://fxbug.dev/101993): Increment the counter.
        trace!("incoming SYN dropped because of the full backlog of the listener");
        return ListenerIncomingSegmentDisposition::FoundSocket;
    }

    let ListenerAddr { ip: _, device: bound_device } = listener_addr;
    // Ensure that if the remote address requires a zone, we propagate that to
    // the address for the connected socket.
    let bound_device = bound_device.as_ref();
    let bound_device = if crate::socket::must_have_zone(remote_ip.as_ref()) {
        Some(bound_device.map_or(EitherDeviceId::Strong(incoming_device), EitherDeviceId::Weak))
    } else {
        bound_device.map(EitherDeviceId::Weak)
    };

    let bound_device = bound_device.as_ref().map(|d| d.as_ref());
    let ip_sock = match sync_ctx.new_ip_socket(
        ctx,
        bound_device,
        Some(local_ip),
        remote_ip,
        IpProto::Tcp.into(),
        DefaultSendOptions,
    ) {
        Ok(ip_sock) => ip_sock,
        Err(err) => {
            // TODO(https://fxbug.dev/101993): Increment the counter.
            trace!("cannot construct an ip socket to the SYN originator: {:?}, ignoring", err);
            return ListenerIncomingSegmentDisposition::NoMatchingSocket;
        }
    };

    let isn = isn.generate(
        ctx.now(),
        (ip_sock.local_ip().clone(), local_port),
        (ip_sock.remote_ip().clone(), remote_port),
    );
    let device_mms = match sync_ctx.get_mms(ctx, &ip_sock) {
        Ok(mms) => mms,
        Err(err) => {
            // If we cannot find a device or the device's MTU is too small,
            // there isn't much we can do here since sending a RST back is
            // impossible, we just need to silent drop the segment.
            error!("Cannot find a device with large enough MTU for the connection");
            match err {
                MmsError::NoDevice(_) | MmsError::MTUTooSmall(_) => {
                    return ListenerIncomingSegmentDisposition::FoundSocket;
                }
            }
        }
    };
    let Some(device_mss) = Mss::from_mms::<I>(device_mms) else {
        return ListenerIncomingSegmentDisposition::FoundSocket;
    };

    let mut state = State::Listen(Closed::<Initial>::listen(
        isn,
        buffer_sizes.clone(),
        device_mss,
        Mss::default::<I>(),
        socket_options.user_timeout,
    ));

    // Prepare a reply to be sent out.
    //
    // We might end up discarding the reply in case we can't instantiate this
    // new connection.
    let reply = assert_matches!(
        state.on_segment::<_, C>(incoming, ctx.now(), &SocketOptions::default(), false /* defunct */),
        (reply, None, /* data_acked */ _) => reply
    );

    let result = if matches!(state, State::SynRcvd(_)) {
        let poll_send_at = state.poll_send_at().expect("no retrans timer");
        let socket_options = socket_options.clone();
        let ListenerSharingState { sharing, listening: _ } = *sharing;
        let bound_device = ip_sock.device().cloned();

        let addr = ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) },
            device: bound_device,
        };

        let new_socket = sync_ctx.with_demux_mut(|DemuxState { socketmap, .. }| {
            // If we're reusing an entry, remove it from the demux before
            // proceeding.
            //
            // We could just reuse the old allocation for the new connection but
            // because of the restrictions on the socket map data structure (for
            // good reasons), we can't update the sharing info unconditionally.
            // So here we just remove the old connection and create a new one.
            // Also this approach has the benefit of not accidentally persisting
            // the old state that we don't want.
            if let Some((tw_reuse, conn_addr)) = tw_reuse {
                match socketmap.conns_mut().remove(tw_reuse, &conn_addr) {
                    Ok(()) => {
                        assert_matches!(ctx.cancel_timer(tw_reuse.downgrade()), Some(_));
                    }
                    Err(NotFoundError) => {
                        // We could lose a race trying to reuse the tw_reuse
                        // socket, so we just accept the loss and be happy that
                        // the conn_addr we want to use is free.
                    }
                }
            }

            // Try to create and add the new socket to the demux.
            let accept_queue_clone = accept_queue.clone();
            let ip_sock = ip_sock.clone();
            match socketmap.conns_mut().try_insert_with(addr, sharing, move |addr, sharing| {
                TcpSocketId::new(TcpSocketState::Bound(BoundSocketState::Connected((
                    Connection {
                        accept_queue: Some(accept_queue_clone),
                        state,
                        ip_sock,
                        defunct: false,
                        socket_options,
                        soft_error: None,
                        handshake_status: HandshakeStatus::Pending,
                    },
                    sharing,
                    addr,
                ))))
            }) {
                Ok((entry, primary)) => {
                    let id = entry.id();
                    // Make sure the new socket is in the pending accept queue
                    // before we release the demux lock.
                    accept_queue.push_pending(id.clone());
                    let timer = id.downgrade();
                    assert_eq!(ctx.schedule_timer_instant(poll_send_at, timer), None);
                    Some(primary)
                }
                Err((e, _sharing_state)) => {
                    // The only error we accept here is if the entry exists
                    // fully, any indirect conflicts are unexpected because we
                    // know the listener is still alive and installed in the
                    // demux.
                    assert_matches!(e, InsertError::Exists);
                    // If we fail to insert it means we lost a race and this
                    // packet is destined to a connection that is already
                    // established. In that case we should tell the demux code
                    // to retry demuxing it all over again.
                    None
                }
            }
        });

        match new_socket {
            Some(new_socket) => ListenerIncomingSegmentDisposition::NewConnection(new_socket),
            None => {
                // We didn't create a new connection, short circuit early and
                // don't send out the pending segment.
                return ListenerIncomingSegmentDisposition::ConflictingConnection;
            }
        }
    } else {
        // We found a valid listener for the segment even if the connection
        // state is not a newly pending connection.
        ListenerIncomingSegmentDisposition::FoundSocket
    };

    // We can send a reply now if we got here.
    if let Some(seg) = reply {
        let body = tcp_serialize_segment(seg, incoming_addrs);
        match sync_ctx.send_ip_packet(ctx, &ip_sock, body, None) {
            Ok(()) => {}
            Err((body, err)) => {
                // TODO(https://fxbug.dev/101993): Increment the counter.
                trace!("tcp: failed to send ip packet {:?}: {:?}", body, err)
            }
        }
    }

    result
}

#[derive(Error, Debug)]
#[error("Multiple mutually exclusive flags are set: syn: {syn}, fin: {fin}, rst: {rst}")]
pub(crate) struct MalformedFlags {
    syn: bool,
    fin: bool,
    rst: bool,
}

impl<'a> TryFrom<TcpSegment<&'a [u8]>> for Segment<&'a [u8]> {
    type Error = MalformedFlags;

    fn try_from(from: TcpSegment<&'a [u8]>) -> Result<Self, Self::Error> {
        if usize::from(from.syn()) + usize::from(from.fin()) + usize::from(from.rst()) > 1 {
            return Err(MalformedFlags { syn: from.syn(), fin: from.fin(), rst: from.rst() });
        }
        let syn = from.syn().then(|| Control::SYN);
        let fin = from.fin().then(|| Control::FIN);
        let rst = from.rst().then(|| Control::RST);
        let control = syn.or(fin).or(rst);
        let options = Options::from_iter(from.iter_options());
        let (to, discarded) = Segment::with_data_options(
            from.seq_num().into(),
            from.ack_num().map(Into::into),
            control,
            UnscaledWindowSize::from(from.window_size()),
            from.into_body(),
            options,
        );
        debug_assert_eq!(discarded, 0);
        Ok(to)
    }
}

pub(super) fn tcp_serialize_segment<'a, S, A>(
    segment: S,
    conn_addr: ConnIpAddr<A, NonZeroU16, NonZeroU16>,
) -> impl Serializer<Buffer = EmptyBuf> + Debug + 'a
where
    S: Into<Segment<SendPayload<'a>>>,
    A: IpAddress,
{
    let Segment { seq, ack, wnd, contents, options } = segment.into();
    let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } = conn_addr;
    let mut builder = TcpSegmentBuilder::new(
        local_ip.addr(),
        remote_ip.addr(),
        local_port,
        remote_port,
        seq.into(),
        ack.map(Into::into),
        u16::from(wnd),
    );
    match contents.control() {
        None => {}
        Some(Control::SYN) => builder.syn(true),
        Some(Control::FIN) => builder.fin(true),
        Some(Control::RST) => builder.rst(true),
    }
    (*contents.data()).into_serializer().encapsulate(
        TcpSegmentBuilderWithOptions::new(builder, options.iter()).unwrap_or_else(
            |TcpOptionsTooLongError| {
                panic!("Too many TCP options");
            },
        ),
    )
}

#[cfg(test)]
mod test {
    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use packet::ParseBuffer as _;
    use test_case::test_case;

    use crate::{
        testutil::TestIpExt,
        transport::tcp::{seqnum::SeqNum, Mss},
    };

    use super::*;

    const SEQ: SeqNum = SeqNum::new(12345);
    const ACK: SeqNum = SeqNum::new(67890);

    impl Segment<SendPayload<'static>> {
        const FAKE_DATA: &'static [u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 0];
        fn with_fake_data(split: bool) -> Self {
            let (segment, discarded) = Self::with_data(
                SEQ,
                Some(ACK),
                None,
                UnscaledWindowSize::from(u16::MAX),
                if split {
                    let (first, second) = Self::FAKE_DATA.split_at(Self::FAKE_DATA.len() / 2);
                    SendPayload::Straddle(first, second)
                } else {
                    SendPayload::Contiguous(Self::FAKE_DATA)
                },
            );
            assert_eq!(discarded, 0);
            segment
        }
    }

    #[ip_test]
    #[test_case(Segment::syn(SEQ, UnscaledWindowSize::from(u16::MAX), Options { mss: None, window_scale: None }).into(), &[]; "syn")]
    #[test_case(Segment::syn(SEQ, UnscaledWindowSize::from(u16::MAX), Options { mss: Some(Mss(const_unwrap_option(NonZeroU16::new(1440 as u16)))), window_scale: None }).into(), &[]; "syn with mss")]
    #[test_case(Segment::ack(SEQ, ACK, UnscaledWindowSize::from(u16::MAX)).into(), &[]; "ack")]
    #[test_case(Segment::with_fake_data(false), Segment::FAKE_DATA; "contiguous data")]
    #[test_case(Segment::with_fake_data(true), Segment::FAKE_DATA; "split data")]
    fn tcp_serialize_segment<I: Ip + TestIpExt>(
        segment: Segment<SendPayload<'_>>,
        expected_body: &[u8],
    ) {
        const SOURCE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(1111));
        const DEST_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(2222));

        let options = segment.options;
        let serializer = super::tcp_serialize_segment(
            segment,
            ConnIpAddr {
                local: (
                    SocketIpAddr::new_from_specified_or_panic(I::FAKE_CONFIG.local_ip),
                    SOURCE_PORT,
                ),
                remote: (
                    SocketIpAddr::new_from_specified_or_panic(I::FAKE_CONFIG.remote_ip),
                    DEST_PORT,
                ),
            },
        );

        let mut serialized = serializer.serialize_vec_outer().unwrap().unwrap_b();
        let parsed_segment = serialized
            .parse_with::<_, TcpSegment<_>>(TcpParseArgs::new(
                *I::FAKE_CONFIG.remote_ip,
                *I::FAKE_CONFIG.local_ip,
            ))
            .expect("is valid segment");

        assert_eq!(parsed_segment.src_port(), SOURCE_PORT);
        assert_eq!(parsed_segment.dst_port(), DEST_PORT);
        assert_eq!(parsed_segment.seq_num(), u32::from(SEQ));
        assert_eq!(
            UnscaledWindowSize::from(parsed_segment.window_size()),
            UnscaledWindowSize::from(u16::MAX)
        );
        assert_eq!(options.iter().count(), parsed_segment.iter_options().count());
        for (orig, parsed) in options.iter().zip(parsed_segment.iter_options()) {
            assert_eq!(orig, parsed);
        }
        assert_eq!(parsed_segment.into_body(), expected_body);
    }
}
