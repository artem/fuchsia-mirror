// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types and functionality related to the state of raw IP sockets.

use lock_order::lock::RwLockFor;
use net_types::ip::{Ip, IpVersionMarker};
use packet_formats::ip::IpProtoExt;

use crate::{
    ip::raw::RawIpSocketsBindingsTypes,
    lock_ordering,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

/// State for a raw IP socket that can be modified, and is lock protected.
#[derive(Default)]
pub(super) struct RawIpSocketLockedState<I: Ip> {
    // TODO(https://fxbug.dev/42175797): Remove once IP fields are held.
    _delete_me: IpVersionMarker<I>,
}

/// State held by a raw IP socket.
pub(super) struct RawIpSocketState<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> {
    /// The bindings state associated with this socket.
    external_state: BT::RawIpSocketState<I>,
    /// The IANA Internet Protocol of this socket.
    ///
    /// This field is specified at creation time and never changes.
    protocol: I::Proto,
    // The locked socket state, accessible via the [`RawIpSocketStateContext`].
    locked_state: RwLock<RawIpSocketLockedState<I>>,
}

impl<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> RawIpSocketState<I, BT> {
    pub(super) fn new(
        protocol: I::Proto,
        external_state: BT::RawIpSocketState<I>,
    ) -> RawIpSocketState<I, BT> {
        RawIpSocketState { external_state, protocol, locked_state: Default::default() }
    }
    // TODO(https://fxbug.dev/42175797): Use this method during rx.
    #[allow(dead_code)]
    pub(super) fn protocol(&self) -> &I::Proto {
        &self.protocol
    }
    pub(super) fn into_external_state(self) -> BT::RawIpSocketState<I> {
        let RawIpSocketState { protocol: _, locked_state: _, external_state } = self;
        external_state
    }
}

impl<I: IpProtoExt, BT: RawIpSocketsBindingsTypes> RwLockFor<lock_ordering::RawIpSocketState<I>>
    for RawIpSocketState<I, BT>
{
    type Data = RawIpSocketLockedState<I>;
    type ReadGuard<'l> = RwLockReadGuard<'l, Self::Data>
        where Self: 'l;
    type WriteGuard<'l> = RwLockWriteGuard<'l, Self::Data>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.locked_state.read()
    }

    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.locked_state.write()
    }
}
