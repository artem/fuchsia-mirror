// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of traits defined in foreign modules for the types defined
//! in the loopback module.

use alloc::vec::Vec;

use lock_order::{
    lock::{LockLevelFor, UnlockedAccessMarkerFor},
    relation::LockBefore,
};
use netstack3_base::DeviceIdContext;
use packet::Buf;

use crate::{
    context::{prelude::*, WrapLockLevel},
    device::{
        self,
        ethernet::EthernetDeviceCounters,
        loopback::{
            LoopbackDevice, LoopbackDeviceId, LoopbackRxQueueMeta, LoopbackTxQueueMeta,
            LoopbackWeakDeviceId,
        },
        queue::{
            BufVecU8Allocator, DequeueState, ReceiveDequeContext, ReceiveQueueContext,
            ReceiveQueueFullError, ReceiveQueueHandler, ReceiveQueueState, ReceiveQueueTypes,
            TransmitDequeueContext, TransmitQueueCommon, TransmitQueueContext, TransmitQueueState,
        },
        socket::{ParseSentFrameError, SentFrame},
        DeviceLayerTypes, DeviceSendFrameError, IpLinkDeviceState,
    },
    BindingsContext, BindingsTypes, CoreCtx,
};

impl<BT: BindingsTypes, L> DeviceIdContext<LoopbackDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = LoopbackDeviceId<BT>;
    type WeakDeviceId = LoopbackWeakDeviceId<BT>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueTypes<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = LoopbackRxQueueMeta;
    type Buffer = Buf<Vec<u8>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxQueue>>
    ReceiveQueueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_receive_queue_mut<
        O,
        F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackRxQueue>();
            cb(&mut x)
        })
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackRxDequeue>>
    ReceiveDequeContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type ReceiveQueueCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::LoopbackRxDequeue>>;

    fn with_dequed_frames_and_rx_queue_ctx<
        O,
        F: FnOnce(
            &mut DequeueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>,
            &mut Self::ReceiveQueueCtx<'_>,
        ) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) =
                    core_ctx_and_resource
                        .lock_with_and::<crate::lock_ordering::LoopbackRxDequeue, _>(|c| c.right());
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueCommon<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = LoopbackTxQueueMeta;
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame<'a, 'b>(
        buf: &'a [u8],
        LoopbackTxQueueMeta: &'b Self::Meta,
    ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError> {
        SentFrame::try_parse_as_ethernet(buf)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxQueue>>
    TransmitQueueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::LoopbackTxQueue>();
            cb(&mut x)
        })
    }

    fn send_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
        // Never handle frames synchronously with the send path - always queue
        // the frame to be received by the loopback device into a queue which
        // a dedicated RX task will kick to handle the queued packet.
        //
        // This is done so that a socket lock may be held while sending a packet
        // which may need to be delivered to the sending socket itself. Without
        // this decoupling of RX/TX paths, sending a packet while holding onto
        // the socket lock will result in a deadlock.
        match ReceiveQueueHandler::queue_rx_frame(self, bindings_ctx, device_id, meta.into(), buf) {
            Ok(()) => {}
            Err(ReceiveQueueFullError((LoopbackRxQueueMeta, _frame))) => {
                // RX queue is full - there is nothing further we can do here.
                tracing::error!("dropped RX frame on loopback device due to full RX queue")
            }
        }

        Ok(())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::LoopbackTxDequeue>>
    TransmitDequeueContext<LoopbackDevice, BC> for CoreCtx<'_, BC, L>
{
    type TransmitQueueCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::LoopbackTxDequeue>>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) =
                    core_ctx_and_resource
                        .lock_with_and::<crate::lock_ordering::LoopbackTxDequeue, _>(|c| c.right());
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

impl<BT: DeviceLayerTypes> UnlockedAccessMarkerFor<IpLinkDeviceState<LoopbackDevice, BT>>
    for crate::lock_ordering::LoopbackDeviceCounters
{
    type Data = EthernetDeviceCounters;

    fn unlocked_access(t: &IpLinkDeviceState<LoopbackDevice, BT>) -> &Self::Data {
        &t.link.counters
    }
}

impl<BT: DeviceLayerTypes> LockLevelFor<IpLinkDeviceState<LoopbackDevice, BT>>
    for crate::lock_ordering::LoopbackRxQueue
{
    type Data = ReceiveQueueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>;
}

impl<BT: DeviceLayerTypes> LockLevelFor<IpLinkDeviceState<LoopbackDevice, BT>>
    for crate::lock_ordering::LoopbackRxDequeue
{
    type Data = DequeueState<LoopbackRxQueueMeta, Buf<Vec<u8>>>;
}

impl<BT: DeviceLayerTypes> LockLevelFor<IpLinkDeviceState<LoopbackDevice, BT>>
    for crate::lock_ordering::LoopbackTxQueue
{
    type Data = TransmitQueueState<LoopbackTxQueueMeta, Buf<Vec<u8>>, BufVecU8Allocator>;
}

impl<BT: DeviceLayerTypes> LockLevelFor<IpLinkDeviceState<LoopbackDevice, BT>>
    for crate::lock_ordering::LoopbackTxDequeue
{
    type Data = DequeueState<LoopbackTxQueueMeta, Buf<Vec<u8>>>;
}
