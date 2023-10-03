// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_overnet_protocol::TRANSFER_KEY_LENGTH;
use rand::Rng;

/// Labels a node with a mesh-unique address
#[derive(PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Hash, Debug)]
pub struct NodeId(pub u64);

/// Identifies whether an endpoint is a client or server
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Endpoint {
    /// Endpoint is a client
    Client,
    /// Endpoint is a server
    Server,
}

impl NodeId {
    /// Makes a string node ID for use with the circuit protocol.
    pub fn circuit_string(&self) -> String {
        format!("{:x}", self.0)
    }

    /// Turns a string node id from the circuit protocol into a `NodeId`
    pub fn from_circuit_string(id: &str) -> Result<Self, ()> {
        Ok(NodeId(u64::from_str_radix(id, 16).map_err(|_| ())?))
    }
}

impl From<u64> for NodeId {
    fn from(id: u64) -> Self {
        NodeId(id)
    }
}

impl From<NodeId> for fidl_fuchsia_overnet_protocol::NodeId {
    fn from(id: NodeId) -> Self {
        Self { id: id.0 }
    }
}

impl From<&NodeId> for fidl_fuchsia_overnet_protocol::NodeId {
    fn from(id: &NodeId) -> Self {
        Self { id: id.0 }
    }
}

impl From<fidl_fuchsia_overnet_protocol::NodeId> for NodeId {
    fn from(id: fidl_fuchsia_overnet_protocol::NodeId) -> Self {
        id.id.into()
    }
}

/// Labels a link with a node-unique identifier
#[derive(PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Debug, Hash)]
pub struct NodeLinkId(pub u64);

impl From<u64> for NodeLinkId {
    fn from(id: u64) -> Self {
        NodeLinkId(id)
    }
}

pub(crate) type TransferKey = [u8; TRANSFER_KEY_LENGTH as usize];

pub(crate) fn generate_transfer_key() -> TransferKey {
    rand::thread_rng().gen::<TransferKey>()
}
