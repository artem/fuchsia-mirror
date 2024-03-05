// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use bt_rfcomm::profile as rfcomm;
use fidl::endpoints::create_proxy_and_stream;
use fidl_fuchsia_bluetooth as fidl_bt;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_async as fasync;
use fuchsia_bluetooth::profile::ProtocolDescriptor;
use fuchsia_bluetooth::types::{Channel, PeerId};
use tracing::{info, warn};

use peer_task::PeerTask;

use crate::config::HandsFreeFeatureSupport;

mod ag_indicators;
mod at_connection;
mod hf_indicators;
mod peer_task;
mod procedure;
mod procedure_manager;
mod procedure_manipulated_state;

/// Represents a Bluetooth peer that supports the AG role. Manages the Service
/// Level Connection, Audio Connection, and FIDL APIs.
pub struct Peer {
    peer_id: PeerId,
    config: HandsFreeFeatureSupport,
    profile_proxy: bredr::ProfileProxy,
    /// The processing task for data received from the remote peer over RFCOMM
    /// or FIDL APIs.
    /// This value is None if there is no RFCOMM channel present.
    /// If set, there is no guarantee that the RFCOMM channel is open.
    task: Option<fasync::Task<()>>,
}

impl Peer {
    pub fn new(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        profile_proxy: bredr::ProfileProxy,
    ) -> Self {
        Self { peer_id, config, profile_proxy, task: None }
    }

    /// Handle an PeerConnected ProfileEvent.  This creates a new peer task, so return the
    /// PeerHandlerProxy appropriate to it.
    pub fn handle_peer_connected(&mut self, channel: Channel) -> fidl_hfp::PeerHandlerProxy {
        if self.task.take().is_some() {
            info!(peer = %self.peer_id, "Shutting down existing task on incoming RFCOMM channel");
        }

        let (peer_handler_proxy, peer_handler_request_stream) =
            create_proxy_and_stream::<fidl_hfp::PeerHandlerMarker>()
                .expect("Unable to create proxy and stream for PeerHandler protocol");

        let task = PeerTask::spawn(self.peer_id, self.config, peer_handler_request_stream, channel);
        self.task = Some(task);

        peer_handler_proxy
    }

    /// Handle a SearchResult ProfileEvent.  If a new peer task is created, return the
    /// PeerHandlerProxy appropriate to it.
    pub async fn handle_search_result(
        &mut self,
        protocol: Option<Vec<ProtocolDescriptor>>,
    ) -> Result<Option<fidl_hfp::PeerHandlerProxy>> {
        if self.task.is_some() {
            info!(peer=%self.peer_id, "Already connected, ignoring search result");
            return Ok(None);
        }
        // If we haven't started the task, connect to the peer and do so.
        info!(peer=%self.peer_id, "Connecting RFCOMM.");

        let channel_result = self.connect_from_protocol(protocol).await;
        let channel = match channel_result {
            Ok(channel) => channel,
            Err(err) => {
                warn!(peer=%self.peer_id, ?err, "Unable to connect RFCOMM to peer.");
                return Err(err);
            }
        };

        let (peer_handler_proxy, peer_handler_request_stream) =
            create_proxy_and_stream::<fidl_hfp::PeerHandlerMarker>()
                .expect("Unable to create proxy for PeerHandler");

        let task = PeerTask::spawn(self.peer_id, self.config, peer_handler_request_stream, channel);
        self.task = Some(task);

        Ok(Some(peer_handler_proxy))
    }

    async fn connect_from_protocol(
        &self,
        protocol_option: Option<Vec<ProtocolDescriptor>>,
    ) -> Result<Channel> {
        let protocol = protocol_option.ok_or_else(|| {
            format_err!("Got no protocols for peer {:} in search result.", self.peer_id)
        })?;

        let server_channel_option = rfcomm::server_channel_from_protocol(&protocol);
        let server_channel = server_channel_option.ok_or_else(|| {
            format_err!(
                "Search result received for non-RFCOMM protocol {:?} from peer {:}.",
                protocol,
                self.peer_id
            )
        })?;

        let params = bredr::ConnectParameters::Rfcomm(bredr::RfcommParameters {
            channel: Some(server_channel.into()),
            ..bredr::RfcommParameters::default()
        });
        let peer_id: fidl_bt::PeerId = self.peer_id.into();

        let fidl_channel_result_result = self.profile_proxy.connect(&peer_id, &params).await;
        let fidl_channel = fidl_channel_result_result
            .map_err(|e| {
                format_err!(
                    "Unable to connect RFCOMM to peer {:} with FIDL error {:?}",
                    self.peer_id,
                    e
                )
            })?
            .map_err(|e| {
                format_err!(
                    "Unable to connect RFCOMM to peer {:} with Bluetooth error {:?}",
                    self.peer_id,
                    e
                )
            })?;

        // Convert a bredr::Channel into a fuchsia_bluetooth::types::Channel
        let channel = fidl_channel.try_into()?;

        Ok(channel)
    }

    pub fn task_exists(&self) -> bool {
        self.task.is_some()
    }
}
