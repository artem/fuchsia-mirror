// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    bt_avctp::{AvcPeer, AvctpPeer},
    fidl_fuchsia_bluetooth_avrcp as fidl_avrcp, fidl_fuchsia_bluetooth_bredr as bredr,
    fuchsia_bluetooth::types::{Channel, PeerId},
    fuchsia_inspect_derive::{AttachError, Inspect},
    fuchsia_sync::RwLock,
    futures::{self, channel::oneshot},
    std::{collections::HashMap, sync::Arc},
};

mod inspect;
mod target_delegate;

use crate::{
    metrics::MetricsNode,
    peer::{Controller, RemotePeerHandle},
    profile::AvrcpService,
    types::PeerError as Error,
};
use inspect::PeerManagerInspect;
pub use target_delegate::TargetDelegate;

#[derive(Debug)]
pub enum ServiceRequest {
    /// Request for a `Controller` given a `peer_id`.
    GetController { peer_id: PeerId, reply: oneshot::Sender<Controller> },

    /// Request to set the current volume handler. Returns an error if one is already set.
    RegisterAbsoluteVolumeHandler {
        absolute_volume_handler: fidl_avrcp::AbsoluteVolumeHandlerProxy,
        reply: oneshot::Sender<Result<(), Error>>,
    },

    /// Request to set the current target handler. Returns an error if one is already set.
    RegisterTargetHandler {
        target_handler: fidl_avrcp::TargetHandlerProxy,
        reply: oneshot::Sender<Result<(), Error>>,
    },
}

impl ServiceRequest {
    pub fn new_controller_request(
        peer_id: PeerId,
    ) -> (oneshot::Receiver<Controller>, ServiceRequest) {
        let (sender, receiver) = oneshot::channel();
        (receiver, ServiceRequest::GetController { peer_id: peer_id.clone(), reply: sender })
    }

    pub fn new_register_target_handler_request(
        target_handler: fidl_avrcp::TargetHandlerProxy,
    ) -> (oneshot::Receiver<Result<(), Error>>, ServiceRequest) {
        let (sender, receiver) = oneshot::channel();
        (receiver, ServiceRequest::RegisterTargetHandler { target_handler, reply: sender })
    }

    pub fn new_register_absolute_volume_handler_request(
        absolute_volume_handler: fidl_avrcp::AbsoluteVolumeHandlerProxy,
    ) -> (oneshot::Receiver<Result<(), Error>>, ServiceRequest) {
        let (sender, receiver) = oneshot::channel();
        (
            receiver,
            ServiceRequest::RegisterAbsoluteVolumeHandler {
                absolute_volume_handler,
                reply: sender,
            },
        )
    }
}

/// Creates, manages, and holds reference to all known peers by the AVRCP service. Handles incoming
/// service requests from FIDL and profile events from the BREDR service and dispatches them
/// accordingly.
pub struct PeerManager {
    profile_proxy: bredr::ProfileProxy,
    /// Known peers, which may be connected or disconnected
    peers: RwLock<HashMap<PeerId, RemotePeerHandle>>,
    /// The delegate for the AVRCP target, where commands to this peer are sent.
    target_delegate: Arc<TargetDelegate>,
    /// The 'peers' node of this inspect tree. All known peers have a child node in this tree.
    inspect: PeerManagerInspect,
}

impl Inspect for &mut PeerManager {
    fn iattach(
        self,
        parent: &fuchsia_inspect::Node,
        name: impl AsRef<str>,
    ) -> Result<(), AttachError> {
        self.inspect.iattach(parent, name.as_ref())
    }
}

impl PeerManager {
    pub fn new(profile_proxy: bredr::ProfileProxy) -> Self {
        Self {
            profile_proxy,
            peers: RwLock::new(HashMap::new()),
            target_delegate: Arc::new(TargetDelegate::new()),
            inspect: PeerManagerInspect::default(),
        }
    }

    pub fn set_metrics_node(&mut self, node: MetricsNode) {
        self.inspect.set_metrics_node(node);
    }

    pub fn get_remote_peer(&self, peer_id: &PeerId) -> RemotePeerHandle {
        self.peers
            .write()
            .entry(peer_id.clone())
            .or_insert_with(|| {
                let mut handle = RemotePeerHandle::spawn_peer(
                    peer_id.clone(),
                    self.target_delegate.clone(),
                    self.profile_proxy.clone(),
                );
                // The inspect node for this peer with a shared metrics node.
                let _ = handle.iattach(self.inspect.node(), fuchsia_inspect::unique_name("peer_"));
                handle.set_metrics_node(self.inspect.metrics_node().clone());
                // Log the unique peer.
                self.inspect.metrics_node().new_peer(peer_id.clone());
                handle
            })
            .clone()
    }

    /// Handle a new incoming connection by a remote peer.
    pub fn new_control_connection(&self, peer_id: &PeerId, channel: Channel) {
        let peer_handle = self.get_remote_peer(peer_id);
        let peer = AvcPeer::new(channel);
        peer_handle.set_control_connection(peer);
    }

    /// Handle a new incoming browse channel connection by a remote peer.
    pub fn new_browse_connection(&mut self, peer_id: &PeerId, channel: Channel) {
        let peer_handle = self.get_remote_peer(peer_id);
        let peer = AvctpPeer::new(channel);
        peer_handle.set_browse_connection(peer);
    }

    pub fn services_found(&mut self, peer_id: &PeerId, services: Vec<AvrcpService>) {
        let peer_handle = self.get_remote_peer(&peer_id);
        for service in services {
            match service {
                AvrcpService::Target { .. } => {
                    peer_handle.set_target_descriptor(service);
                }
                AvrcpService::Controller { .. } => {
                    peer_handle.set_controller_descriptor(service);
                }
            }
        }
    }

    pub fn handle_service_request(&mut self, service_request: ServiceRequest) {
        match service_request {
            ServiceRequest::GetController { peer_id, reply } => {
                // ignoring error if we failed to reply.
                let _ = reply.send(self.get_remote_peer(&peer_id).get_controller());
            }
            ServiceRequest::RegisterTargetHandler { target_handler, reply } => {
                let _ = reply.send(self.target_delegate.set_target_handler(target_handler));
            }
            ServiceRequest::RegisterAbsoluteVolumeHandler { absolute_volume_handler, reply } => {
                let _ = reply.send(
                    self.target_delegate.set_absolute_volume_handler(absolute_volume_handler),
                );
            }
        }
    }
}
