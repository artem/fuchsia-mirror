// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_bluetooth_bredr::{ChannelMode, ChannelParameters},
    fuchsia_zircon as zx,
    futures::channel::oneshot,
    std::{
        cmp::PartialEq,
        collections::{hash_map::Iter, HashMap},
        fmt::Debug,
    },
    test_rfcomm_client::RfcommManager,
};

#[derive(Debug, PartialEq)]
pub struct IncrementedIdMap<T> {
    next_id: u32,
    map: HashMap<u32, T>,
}

impl<T: Debug> IncrementedIdMap<T> {
    pub fn new() -> IncrementedIdMap<T> {
        IncrementedIdMap { next_id: 0, map: HashMap::new() }
    }

    pub fn map(&self) -> &HashMap<u32, T> {
        &self.map
    }

    /// Returns id assigned.
    pub fn insert(&mut self, value: T) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        assert!(self.map.insert(id, value).is_none());
        id
    }

    pub fn remove(&mut self, id: &u32) -> Option<T> {
        self.map.remove(id)
    }
}

impl<'a, T> IntoIterator for &'a IncrementedIdMap<T> {
    type Item = (&'a u32, &'a T);
    type IntoIter = Iter<'a, u32, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

#[derive(Debug)]
pub struct L2capChannel {
    pub socket: zx::Socket,
    pub mode: ChannelMode,
    pub max_tx_sdu_size: u16,
}

#[derive(Debug)]
pub struct SdpService {
    pub advertisement_stopper: oneshot::Sender<()>,
    pub params: ChannelParameters,
}

/// Tracks all state local to the command line tool.
pub struct ProfileState {
    /// Currently connected L2CAP channels.
    pub l2cap_channels: IncrementedIdMap<L2capChannel>,
    /// Currently active service advertisements.
    pub services: IncrementedIdMap<SdpService>,
    /// The current RFCOMM state.
    pub rfcomm: RfcommManager,
}

impl ProfileState {
    pub fn new(rfcomm: RfcommManager) -> ProfileState {
        ProfileState {
            l2cap_channels: IncrementedIdMap::new(),
            services: IncrementedIdMap::new(),
            rfcomm,
        }
    }

    pub fn reset(&mut self) {
        // Dropping the services will stop the advertisements.
        self.services = IncrementedIdMap::new();

        // Dropping the L2CAP sockets will disconnect channels.
        self.l2cap_channels = IncrementedIdMap::new();

        // Resetting the RFCOMM state will cancel any active service advertisement &
        // search and disconnect the channels.
        self.rfcomm.clear_services();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremented_id_map() {
        let mut numbers = IncrementedIdMap::<i32>::new();
        assert_eq!(0, numbers.insert(0));
        assert_eq!(1, numbers.insert(1));

        assert_eq!(2, numbers.map().len());
        assert_eq!(Some(&0i32), numbers.map().get(&0u32));
        assert_eq!(Some(&1i32), numbers.map().get(&1u32));
    }
}
