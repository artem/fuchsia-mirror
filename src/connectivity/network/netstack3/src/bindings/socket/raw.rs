// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::ControlFlow;

use fidl::endpoints::{ProtocolMarker, RequestStream};
use fidl_fuchsia_posix_socket as fposix_socket;
use fidl_fuchsia_posix_socket_raw as fpraw;
use fuchsia_zircon as zx;
use futures::StreamExt as _;
use netstack3_core::SyncCtx;
use tracing::error;
use zx::{HandleBased, Peered};

use crate::bindings::{BindingsNonSyncCtxImpl, Ctx};

use super::{
    worker::{self, CloseResponder, SocketWorker, SocketWorkerHandler, TaskSpawnerCollection},
    SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING,
};

#[derive(Debug)]
struct BindingData {
    /// The event to hand off for [`fpraw::SocketRequest::Describe`].
    peer_event: zx::EventPair,
}

impl BindingData {
    fn new(
        _sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        _domain: fposix_socket::Domain,
        _proto: fpraw::ProtocolAssociation,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        };

        BindingData { peer_event }
    }
}

impl CloseResponder for fpraw::SocketCloseResponder {
    fn send(self, response: Result<(), i32>) -> Result<(), fidl::Error> {
        fpraw::SocketCloseResponder::send(self, response)
    }
}

impl SocketWorkerHandler for BindingData {
    type Request = fpraw::SocketRequest;

    type RequestStream = fpraw::SocketRequestStream;

    type CloseResponder = fpraw::SocketCloseResponder;

    type SetupArgs = ();

    type Spawner = ();

    fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
        _spawner: &TaskSpawnerCollection<Self::Spawner>,
    ) -> std::ops::ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request)
    }

    fn close(
        self,
        _sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
    }
}

struct RequestHandler<'a> {
    ctx: &'a Ctx,
    data: &'a mut BindingData,
}

impl<'a> RequestHandler<'a> {
    fn describe(self) -> fpraw::SocketDescribeResponse {
        let Self { ctx: _ctx, data: BindingData { peer_event } } = self;
        let peer = peer_event
            .duplicate_handle(
                // The client only needs to be able to receive signals so don't
                // allow it to set signals.
                zx::Rights::BASIC,
            )
            .expect("failed to duplicate handle");
        fpraw::SocketDescribeResponse { event: Some(peer), ..Default::default() }
    }

    fn handle_request(
        self,
        request: fpraw::SocketRequest,
    ) -> std::ops::ControlFlow<fpraw::SocketCloseResponder, Option<fpraw::SocketRequestStream>>
    {
        match request {
            fpraw::SocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel())
                    .expect("failed to create async channel");
                let stream = fpraw::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fpraw::SocketRequest::Describe { responder } => {
                responder
                    .send(self.describe())
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fpraw::SocketRequest::Close { responder } => return ControlFlow::Break(responder),
            fpraw::SocketRequest::Query { responder } => responder
                .send(fpraw::SOCKET_PROTOCOL_NAME.as_bytes())
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetReuseAddress { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetReuseAddress { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetError { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetBroadcast { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetBroadcast { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetSendBuffer { value_bytes: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetSendBuffer { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetReceiveBuffer { value_bytes: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetReceiveBuffer { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetKeepAlive { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetKeepAlive { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetOutOfBandInline { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetOutOfBandInline { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetNoCheck { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetNoCheck { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetLinger { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetReusePort { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetReusePort { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetAcceptConn { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetBindToDevice { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetBindToDevice { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetTimestamp { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetTimestamp { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::Bind { addr: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::Connect { addr: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::Disconnect { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::GetSockName { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::GetPeerName { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::Shutdown { mode: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpTypeOfService { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpTypeOfService { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpTtl { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpTtl { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetIpPacketInfo { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpPacketInfo { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpReceiveTypeOfService { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpReceiveTypeOfService { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpReceiveTtl { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpReceiveTtl { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpMulticastInterface { iface: _, address: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpMulticastInterface { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpMulticastTtl { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpMulticastTtl { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpMulticastLoopback { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpMulticastLoopback { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::AddIpMembership { membership: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::DropIpMembership { membership: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpTransparent { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpTransparent { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpReceiveOriginalDestinationAddress {
                value: _,
                responder,
            } => respond_not_supported!(responder),
            fpraw::SocketRequest::GetIpReceiveOriginalDestinationAddress { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::AddIpv6Membership { membership: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::DropIpv6Membership { membership: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastInterface { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastInterface { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6UnicastHops { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6UnicastHops { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6ReceiveHopLimit { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastHops { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastHops { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastLoopback { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastLoopback { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6Only { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6Only { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetIpv6ReceiveTrafficClass { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6ReceiveTrafficClass { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6TrafficClass { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6TrafficClass { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6ReceivePacketInfo { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6ReceivePacketInfo { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetOriginalDestination { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::RecvMsg {
                want_addr: _,
                data_len: _,
                want_control: _,
                flags: _,
                responder,
            } => respond_not_supported!(responder),
            fpraw::SocketRequest::SendMsg { addr: _, data: _, control: _, flags: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetInfo { responder } => respond_not_supported!(responder),
            fpraw::SocketRequest::SetIpHeaderIncluded { value: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpHeaderIncluded { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIcmpv6Filter { filter: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIcmpv6Filter { responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::SetIpv6Checksum { config: _, responder } => {
                respond_not_supported!(responder)
            }
            fpraw::SocketRequest::GetIpv6Checksum { responder } => {
                respond_not_supported!(responder)
            }
        }
        ControlFlow::Continue(None)
    }
}

pub(crate) async fn serve(
    ctx: Ctx,
    stream: fpraw::ProviderRequestStream,
) -> crate::bindings::util::TaskWaitGroup {
    let ctx = &ctx;
    let (wait_group, spawner) = crate::bindings::util::TaskWaitGroup::new();
    let spawner: worker::ProviderScopedSpawner<_> = spawner.into();
    stream
        .map(|req| {
            let req = match req {
                Ok(req) => req,
                Err(e) => {
                    if !e.is_closed() {
                        tracing::error!(
                            "{} request error {e:?}",
                            fpraw::ProviderMarker::DEBUG_NAME
                        );
                    }
                    return;
                }
            };
            match req {
                fpraw::ProviderRequest::Socket { responder, domain, proto } => {
                    let (client, request_stream) = fidl::endpoints::create_request_stream()
                        .expect("failed to create a new request stream");

                    spawner.spawn(SocketWorker::serve_stream_with(
                        ctx.clone(),
                        move |sync_ctx, non_sync_ctx, properties| {
                            BindingData::new(sync_ctx, non_sync_ctx, domain, proto, properties)
                        },
                        SocketWorkerProperties {},
                        request_stream,
                        (),
                        spawner.clone(),
                    ));
                    responder
                        .send(Ok(client))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
            }
        })
        .collect::<()>()
        .await;
    wait_group
}
