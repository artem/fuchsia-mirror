// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::quic_link::{new_quic_link, QuicReceiver};
use anyhow::{format_err, Error};
use fuchsia_async::net::UdpSocket;
use futures::{channel::mpsc, lock::Mutex, prelude::*};
use overnet_core::{ConnectionId, Endpoint, LinkIntroductionFacts, Router, MAX_FRAME_LENGTH};
use std::collections::HashMap;
use std::convert::TryInto;
use std::net::{SocketAddr, SocketAddrV6};
use std::sync::{Arc, Weak};

struct Connections(Mutex<HashMap<ConnectionId, Arc<QuicReceiver>>>);

impl Connections {
    async fn register_and_run<R>(
        &self,
        conn_id: ConnectionId,
        link_receiver: QuicReceiver,
        body: impl Future<Output = R>,
    ) -> R {
        self.0.lock().await.insert(conn_id, Arc::new(link_receiver));
        let r = body.await;
        self.0.lock().await.remove(&conn_id);
        r
    }

    async fn lookup<'a>(&'a self, dcid: &[u8]) -> Option<Arc<QuicReceiver>> {
        let conn_id = dcid.try_into().ok()?;
        self.0.lock().await.get(&conn_id).cloned()
    }
}

async fn run_link(
    node: &Weak<Router>,
    connections: &Connections,
    addr: SocketAddrV6,
    endpoint: Endpoint,
    first_packet: Option<Vec<u8>>,
    udp_socket: &UdpSocket,
) -> Result<(), Error> {
    let (link_sender, link_receiver) =
        Weak::upgrade(node).ok_or_else(|| format_err!("router gone"))?.new_link(
            LinkIntroductionFacts { you_are: Some(SocketAddr::V6(addr)) },
            Box::new(move || {
                Some(fidl_fuchsia_overnet_protocol::LinkConfig::Udp(
                    fidl_fuchsia_net::Ipv6SocketAddress {
                        address: fidl_fuchsia_net::Ipv6Address { addr: addr.ip().octets() },
                        port: addr.port(),
                        zone_index: addr.scope_id() as u64,
                    },
                ))
            }),
        );
    let (link_sender, link_receiver, conn_id) =
        new_quic_link(link_sender, link_receiver, endpoint).await?;
    tracing::info!(?addr, ?endpoint, ?first_packet, ?conn_id, "NEW LINK");
    if let Some(mut packet) = first_packet {
        link_receiver.received_frame(&mut packet).await;
    }
    connections
        .register_and_run(conn_id, link_receiver, async move {
            let mut frame = [0u8; MAX_FRAME_LENGTH];
            while let Ok(Some(len)) = link_sender.next_send(&mut frame).await {
                udp_socket.send_to(&frame[..len], addr.into()).await?;
            }
            Ok(())
        })
        .await
}

async fn recv_packet(
    connections: &Connections,
    sender: SocketAddrV6,
    buf: &mut [u8],
    tx_new_link: &mut mpsc::Sender<(SocketAddrV6, Endpoint, Option<Vec<u8>>)>,
) -> Result<(), Error> {
    let hdr = quiche::Header::from_slice(buf, quiche::MAX_CONN_ID_LEN)?;
    if let Some(connection) = connections.lookup(&hdr.dcid).await {
        connection.received_frame(buf).await;
    } else if hdr.ty == quiche::Type::Initial {
        tx_new_link.send((sender, Endpoint::Server, Some(buf.to_vec()))).await?;
    }
    Ok(())
}

async fn run_links(
    node: &Weak<Router>,
    discovered_peers: mpsc::Receiver<SocketAddrV6>,
    rx_new_link: mpsc::Receiver<(SocketAddrV6, Endpoint, Option<Vec<u8>>)>,
    connections: &Connections,
    udp_socket: &UdpSocket,
) -> Result<(), Error> {
    futures::stream::select(
        discovered_peers.map(|addr| (addr, Endpoint::Client, None)),
        rx_new_link,
    )
    .for_each_concurrent(None, |(addr, endpoint, first_packet)| async move {
        if let Err(e) = run_link(node, connections, addr, endpoint, first_packet, udp_socket).await
        {
            tracing::info!("link failed: {:?}", e)
        }
    })
    .await;
    Err(format_err!("No more incoming links"))
}

async fn route_incoming_frames(
    connections: &Connections,
    udp_socket: &UdpSocket,
    mut tx_new_link: mpsc::Sender<(SocketAddrV6, Endpoint, Option<Vec<u8>>)>,
) -> Result<(), Error> {
    let mut buf = [0u8; MAX_FRAME_LENGTH];
    loop {
        let (length, sender) = udp_socket.recv_from(&mut buf).await?;
        tracing::info!(length_bytes = length, from = ?sender, "got packet");
        if let Err(e) =
            recv_packet(connections, normalize_addr(sender), &mut buf[..length], &mut tx_new_link)
                .await
        {
            tracing::info!("error reading packet: {}", e);
        }
    }
}

pub async fn run_udp(
    node: Weak<Router>,
    discovered_peers: mpsc::Receiver<SocketAddrV6>,
    mut publish_addr: mpsc::Sender<SocketAddrV6>,
) -> Result<(), Error> {
    let udp_socket = &UdpSocket::bind(&"[::]:0".parse().unwrap())?;

    publish_addr.send(normalize_addr(udp_socket.local_addr()?)).await?;

    let connections = Connections(Mutex::new(HashMap::new()));
    let (tx_new_link, rx_new_link) = mpsc::channel(1);

    futures::future::try_join(
        run_links(&node, discovered_peers, rx_new_link, &connections, &udp_socket),
        route_incoming_frames(&connections, &udp_socket, tx_new_link),
    )
    .map_ok(drop)
    .await
}

fn normalize_addr(addr: SocketAddr) -> SocketAddrV6 {
    match addr {
        SocketAddr::V6(a) => SocketAddrV6::new(*a.ip(), a.port(), 0, 0),
        SocketAddr::V4(a) => SocketAddrV6::new(a.ip().to_ipv6_mapped(), a.port(), 0, 0),
    }
}
