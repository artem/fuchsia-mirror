// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 core UDP.
//!
//! This crate contains the UDP implementation for netstack3.

#![no_std]
#![deny(missing_docs, unreachable_patterns, clippy::useless_conversion, clippy::redundant_clone)]

extern crate fakealloc as alloc;
extern crate fakestd as std;

#[path = "."]
mod internal {
    pub(super) mod base;
}

pub use internal::base::{
    BoundSockets, BoundStateContext, DualStackBoundStateContext, NonDualStackBoundStateContext,
    SendError, SendToError, Sockets, StateContext, UdpApi, UdpBindingsContext, UdpBindingsTypes,
    UdpCounters, UdpCountersInner, UdpIpTransportContext, UdpReceiveBindingsContext, UdpRemotePort,
    UdpSocketId, UdpSocketSet, UdpSocketState, UdpState, UdpStateBuilder, UdpStateContext,
    UseUdpIpTransportContextBlanket,
};
