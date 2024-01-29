// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A pure IP device, capable of directly sending/receiving IPv4 & IPv6 packets.

use alloc::vec::Vec;
use net_types::ip::{IpVersion, Mtu};
use packet::Buf;

use crate::device::{
    queue::tx::{BufVecU8Allocator, TransmitQueue},
    state::DeviceStateSpec,
    BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId, Device, DeviceLayerTypes,
    DeviceReceiveFrameSpec,
};

mod integration;

/// A weak device ID identifying a pure IP device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for pure IP
/// devices.
///
/// [`WeakDeviceId`]: crate::device::WeakDeviceId
pub type PureIpWeakDeviceId<BT> = BaseWeakDeviceId<PureIpDevice, BT>;

/// A strong device ID identifying a pure IP device.
///
/// This device ID is like [`DeviceId`] but specifically for pure IP devices.
///
/// [`DeviceId`]: crate::device::DeviceId
pub type PureIpDeviceId<BT> = BaseDeviceId<PureIpDevice, BT>;

/// The primary reference for a pure IP device.
pub(crate) type PureIpPrimaryDeviceId<BT> = BasePrimaryDeviceId<PureIpDevice, BT>;

/// A marker type identifying a pure IP device.
#[derive(Copy, Clone)]
pub enum PureIpDevice {}

/// The parameters required to create a pure IP device.
#[derive(Debug)]
pub struct PureIpDeviceCreationProperties {
    /// The MTU of the device.
    mtu: Mtu,
}

/// State for a pure IP device.
pub struct PureIpDeviceState {
    /// The MTU of the device.
    pub(crate) mtu: Mtu,
    /// The device's transmit queue.
    tx_queue: TransmitQueue<(), Buf<Vec<u8>>, BufVecU8Allocator>,
}

impl Device for PureIpDevice {}

impl DeviceStateSpec for PureIpDevice {
    type Link<BT: DeviceLayerTypes> = PureIpDeviceState;
    type External<BT: DeviceLayerTypes> = BT::PureIpDeviceState;
    type CreationProperties = PureIpDeviceCreationProperties;
    const IS_LOOPBACK: bool = false;
    const DEBUG_TYPE: &'static str = "PureIP";

    fn new_link_state<BT: DeviceLayerTypes>(
        PureIpDeviceCreationProperties { mtu }: Self::CreationProperties,
    ) -> Self::Link<BT> {
        PureIpDeviceState { mtu, tx_queue: Default::default() }
    }
}

/// Metadata for IP packets received on a pure IP device.
pub struct PureIpDeviceFrameMetadata<D> {
    /// The device a packet was received on.
    device_id: D,
    /// The IP version of the received packet.
    ip_version: IpVersion,
}

impl DeviceReceiveFrameSpec for PureIpDevice {
    type FrameMetadata<D> = PureIpDeviceFrameMetadata<D>;
}
