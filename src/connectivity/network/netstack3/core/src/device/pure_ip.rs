// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A pure IP device, capable of directly sending/receiving IPv4 & IPv6 packets.

use alloc::vec::Vec;
use lock_order::lock::UnlockedAccess;
use net_types::ip::{Ip, IpVersion, Mtu};
use packet::{Buf, BufferMut, Serializer};
use tracing::warn;

use crate::{
    device::{
        queue::{
            tx::{BufVecU8Allocator, TransmitQueue, TransmitQueueHandler},
            TransmitQueueFrameError,
        },
        state::{DeviceStateSpec, IpLinkDeviceState},
        BaseDeviceId, BasePrimaryDeviceId, BaseWeakDeviceId, Device, DeviceLayerTypes,
        DeviceReceiveFrameSpec, DeviceSendFrameError, PureIpDeviceCounters,
    },
    BindingsContext,
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

/// Metadata for IP packets held in the TX queue.
pub struct PureIpDeviceTxQueueFrameMetadata {
    /// The IP version of the sent packet.
    ip_version: IpVersion,
}

/// State for a pure IP device.
pub struct PureIpDeviceState {
    /// The MTU of the device.
    pub(crate) mtu: Mtu,
    /// The device's transmit queue.
    tx_queue: TransmitQueue<PureIpDeviceTxQueueFrameMetadata, Buf<Vec<u8>>, BufVecU8Allocator>,
    /// Counters specific to pure IP devices.
    counters: PureIpDeviceCounters,
}

impl Device for PureIpDevice {}

impl DeviceStateSpec for PureIpDevice {
    type Link<BT: DeviceLayerTypes> = PureIpDeviceState;
    type External<BT: DeviceLayerTypes> = BT::PureIpDeviceState;
    type CreationProperties = PureIpDeviceCreationProperties;
    type Counters = PureIpDeviceCounters;
    const IS_LOOPBACK: bool = false;
    const DEBUG_TYPE: &'static str = "PureIP";

    fn new_link_state<BT: DeviceLayerTypes>(
        PureIpDeviceCreationProperties { mtu }: Self::CreationProperties,
    ) -> Self::Link<BT> {
        PureIpDeviceState {
            mtu,
            tx_queue: Default::default(),
            counters: PureIpDeviceCounters::default(),
        }
    }
}

/// Metadata for IP packets received on a pure IP device.
pub struct PureIpDeviceReceiveFrameMetadata<D> {
    /// The device a packet was received on.
    device_id: D,
    /// The IP version of the received packet.
    ip_version: IpVersion,
}

impl DeviceReceiveFrameSpec for PureIpDevice {
    type FrameMetadata<D> = PureIpDeviceReceiveFrameMetadata<D>;
}

/// Enqueues the given IP packet on the TX queue for the given [`PureIpDevice`].
pub(super) fn send_ip_frame<CC, BC, I, S>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    packet: S,
) -> Result<(), S>
where
    CC: TransmitQueueHandler<PureIpDevice, BC, Meta = PureIpDeviceTxQueueFrameMetadata>,
    I: Ip,
    S: Serializer,
    S::Buffer: BufferMut,
{
    // TODO(https://fxbug.dev/42051633): Update device counters.
    let result = TransmitQueueHandler::<PureIpDevice, _>::queue_tx_frame(
        core_ctx,
        bindings_ctx,
        device_id,
        PureIpDeviceTxQueueFrameMetadata { ip_version: I::VERSION },
        packet,
    );
    match result {
        Ok(()) => Ok(()),
        Err(TransmitQueueFrameError::NoQueue(DeviceSendFrameError::DeviceNotReady(()))) => {
            warn!("device {device_id:?} not ready to send frame.");
            Ok(())
        }
        Err(TransmitQueueFrameError::QueueFull(s))
        | Err(TransmitQueueFrameError::SerializeError(s)) => Err(s),
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::PureIpDeviceCounters>
    for IpLinkDeviceState<PureIpDevice, BC>
{
    type Data = PureIpDeviceCounters;
    type Guard<'l> = &'l PureIpDeviceCounters
        where
            Self: 'l ;
    fn access(&self) -> Self::Guard<'_> {
        &self.link.counters
    }
}
