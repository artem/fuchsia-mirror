// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::uapi;

bitflags::bitflags! {
    pub struct FdEvents: u32 {
        const POLLIN = uapi::POLLIN;
        const POLLPRI = uapi::POLLPRI;
        const POLLOUT = uapi::POLLOUT;
        const POLLERR = uapi::POLLERR;
        const POLLHUP = uapi::POLLHUP;
        const POLLNVAL = uapi::POLLNVAL;
        const POLLRDNORM = uapi::POLLRDNORM;
        const POLLRDBAND = uapi::POLLRDBAND;
        const POLLWRNORM = uapi::POLLWRNORM;
        const POLLWRBAND = uapi::POLLWRBAND;
        const POLLMSG = uapi::POLLMSG;
        const POLLREMOVE = uapi::POLLREMOVE;
        const POLLRDHUP = uapi::POLLRDHUP;
        const EPOLLET = uapi::EPOLLET;
        const EPOLLONESHOT = uapi::EPOLLONESHOT;
    }
}

impl FdEvents {
    /// Build events from the given mask, truncating any bits that do not correspond to an event.
    pub fn from_waiter_mask(mask: u64) -> Self {
        Self::from_bits_truncate((mask & (u32::MAX as u64)) as u32)
    }

    /// Return the mask appropriate for waiting to these events in a Waiter.
    pub fn as_waiter_mask(&self) -> u64 {
        self.bits().into()
    }
}
