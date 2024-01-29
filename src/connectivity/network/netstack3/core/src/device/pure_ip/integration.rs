// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of traits defined in foreign modules for the types defined
//! in the pure_ip module.

use alloc::vec::Vec;
use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    wrap::LockedWrapperApi,
};
use net_types::ip::{Ip, IpVersion, Ipv4, Ipv6};
use packet::{Buf, BufferMut, Serializer};

use crate::{
    context::{RecvFrameContext, SendFrameContext},
    device::{
        config::DeviceConfigurationContext,
        pure_ip::{
            PureIpDevice, PureIpDeviceFrameMetadata, PureIpDeviceId, PureIpPrimaryDeviceId,
            PureIpWeakDeviceId,
        },
        queue::{
            tx::{
                BufVecU8Allocator, TransmitDequeueContext, TransmitQueueBindingsContext,
                TransmitQueueCommon, TransmitQueueContext, TransmitQueueState,
            },
            DequeueState,
        },
        socket::{DeviceSocketMetadata, HeldDeviceSockets, ParseSentFrameError},
        state::IpLinkDeviceState,
        DeviceCollectionContext, DeviceIdContext, DeviceLayerEventDispatcher, DeviceSendFrameError,
        RecvIpFrameMeta,
    },
    device_socket::SentFrame,
    neighbor::NudUserConfig,
    BindingsContext, BindingsTypes, CoreCtx,
};

impl<BT: BindingsTypes, L> DeviceIdContext<PureIpDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = PureIpDeviceId<BT>;
    type WeakDeviceId = PureIpWeakDeviceId<BT>;
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        device_id.downgrade()
    }
    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        weak_device_id.upgrade()
    }
}

impl<'a, BT, L> DeviceCollectionContext<PureIpDevice, BT> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
{
    fn insert(&mut self, device: PureIpPrimaryDeviceId<BT>) {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        let strong = device.clone_strong();
        assert!(devices.pure_ip.insert(strong, device).is_none());
    }

    fn remove(&mut self, device: &PureIpDeviceId<BT>) -> Option<PureIpPrimaryDeviceId<BT>> {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.pure_ip.remove(device)
    }
}

impl<'a, BT, L> DeviceConfigurationContext<PureIpDevice> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    fn with_nud_config<I: Ip, O, F: FnOnce(Option<&NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // PureIp doesn't support NUD.
        f(None)
    }

    fn with_nud_config_mut<I: Ip, O, F: FnOnce(Option<&mut NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // PureIp doesn't support NUD.
        f(None)
    }
}

impl<CC, BC> RecvFrameContext<BC, PureIpDeviceFrameMetadata<CC::DeviceId>> for CC
where
    CC: DeviceIdContext<PureIpDevice>
        + RecvFrameContext<BC, RecvIpFrameMeta<CC::DeviceId, Ipv4>>
        + RecvFrameContext<BC, RecvIpFrameMeta<CC::DeviceId, Ipv6>>,
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: PureIpDeviceFrameMetadata<CC::DeviceId>,
        buffer: B,
    ) {
        // NB: A link layer device would need to dispatch the frame to the
        // device socket handler; however pure IP devices operate above the link
        // layer, and are not expected to deliver their packets to device
        // sockets. This conforms to the behavior on Linux.
        let PureIpDeviceFrameMetadata { device_id, ip_version } = metadata;
        match ip_version {
            IpVersion::V4 => self.receive_frame(
                bindings_ctx,
                RecvIpFrameMeta::<_, Ipv4>::new(device_id, None),
                buffer,
            ),
            IpVersion::V6 => self.receive_frame(
                bindings_ctx,
                RecvIpFrameMeta::<_, Ipv6>::new(device_id, None),
                buffer,
            ),
        }
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceRxDequeue>>
    RecvFrameContext<BC, RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv4>> for CoreCtx<'_, BC, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv4>,
        frame: B,
    ) {
        crate::ip::receive_ipv4_packet(
            self,
            bindings_ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceRxDequeue>>
    RecvFrameContext<BC, RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv6>> for CoreCtx<'_, BC, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: RecvIpFrameMeta<PureIpDeviceId<BC>, Ipv6>,
        frame: B,
    ) {
        crate::ip::receive_ipv6_packet(
            self,
            bindings_ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

impl<BC: BindingsContext, L> SendFrameContext<BC, DeviceSocketMetadata<PureIpDeviceId<BC>>>
    for CoreCtx<'_, BC, L>
{
    fn send_frame<S>(
        &mut self,
        _bindings_ctx: &mut BC,
        _metadata: DeviceSocketMetadata<PureIpDeviceId<BC>>,
        _body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        // TODO(https://fxbug.dev/42051633): Handle sending IP packets.
        Ok(())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>>
    TransmitQueueCommon<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    type Meta = ();
    type Allocator = BufVecU8Allocator;
    type Buffer = Buf<Vec<u8>>;

    fn parse_outgoing_frame(_buf: &[u8]) -> Result<SentFrame<&[u8]>, ParseSentFrameError> {
        // TODO(https://fxbug.dev/42051633): Handle parsing outgoing frames
        Err(ParseSentFrameError)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>>
    TransmitQueueContext<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state(self, device_id, |mut state| {
            let mut x = state.lock::<crate::lock_ordering::PureIpDeviceTxQueue>();
            cb(&mut x)
        })
    }

    fn send_frame(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _meta: Self::Meta,
        _buf: Self::Buffer,
    ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
        // TODO(https://fxbug.dev/42051633): Handle sending IP packets.
        Ok(())
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::PureIpDeviceTxDequeue>>
    TransmitDequeueContext<PureIpDevice, BC> for CoreCtx<'_, BC, L>
{
    type TransmitQueueCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::PureIpDeviceTxDequeue>;

    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::integration::with_device_state_and_core_ctx(
            self,
            device_id,
            |mut core_ctx_and_resource| {
                let (mut x, mut locked) = core_ctx_and_resource
                    .lock_with_and::<crate::lock_ordering::PureIpDeviceTxDequeue, _>(
                    |c| c.right(),
                );
                cb(&mut x, &mut locked.cast_core_ctx())
            },
        )
    }
}

impl<BC: BindingsContext> TransmitQueueBindingsContext<PureIpDevice, PureIpDeviceId<BC>> for BC {
    fn wake_tx_task(&mut self, device_id: &PureIpDeviceId<BC>) {
        DeviceLayerEventDispatcher::wake_tx_task(self, &device_id.clone().into())
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::PureIpDeviceTxQueue>
    for IpLinkDeviceState<PureIpDevice, BC>
{
    type Data = TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>;
    type Guard<'l> = crate::sync::LockGuard<'l, TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.tx_queue.queue.lock()
    }
}

impl<BC: BindingsContext> LockFor<crate::lock_ordering::PureIpDeviceTxDequeue>
    for IpLinkDeviceState<PureIpDevice, BC>
{
    type Data = DequeueState<(), Buf<Vec<u8>>>;
    type Guard<'l> = crate::sync::LockGuard<'l, DequeueState<(), Buf<Vec<u8>>>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.link.tx_queue.deque.lock()
    }
}

impl<BC: BindingsContext> RwLockFor<crate::lock_ordering::DeviceSockets>
    for IpLinkDeviceState<PureIpDevice, BC>
{
    type Data = HeldDeviceSockets<BC>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, HeldDeviceSockets<BC>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, HeldDeviceSockets<BC>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.sockets.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.sockets.write()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_types::{ip::Mtu, Witness};
    use packet_formats::ip::{IpPacketBuilder, IpProto};

    use crate::{
        device::{
            pure_ip::PureIpDeviceCreationProperties, RemoveDeviceResult, TransmitQueueConfiguration,
        },
        testutil::{TestIpExt, DEFAULT_INTERFACE_METRIC},
    };

    const MTU: Mtu = Mtu::new(1234);
    const TTL: u8 = 64;

    #[test]
    // Smoke test verifying [`PureIpDevice`] implements the traits required to
    // satisfy the [`DeviceApi`].
    fn add_remove_pure_ip_device() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let mut device_api = ctx.core_api().device::<PureIpDevice>();
        let device = device_api.add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        assert_matches!(device_api.remove_device(device), RemoveDeviceResult::Removed(_));
    }

    #[test]
    // Smoke test verifying [`PureIpDevice`] implements the traits required to
    // satisfy the [`TransmitQueueApi`].
    fn update_tx_queue_config() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let mut device_api = ctx.core_api().device::<PureIpDevice>();
        let device = device_api.add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        let mut tx_queue_api = ctx.core_api().transmit_queue::<PureIpDevice>();
        tx_queue_api.set_configuration(&device, TransmitQueueConfiguration::Fifo);
    }

    #[ip_test]
    fn receive_frame<I: Ip + TestIpExt>() {
        let mut ctx = crate::testutil::FakeCtx::default();
        let device = ctx.core_api().device::<PureIpDevice>().add_device_with_default_state(
            PureIpDeviceCreationProperties { mtu: MTU },
            DEFAULT_INTERFACE_METRIC,
        );
        crate::device::testutil::enable_device(&mut ctx, &device.clone().into());

        let packet = Buf::new(Vec::new(), ..)
            .encapsulate(I::PacketBuilder::new(
                I::FAKE_CONFIG.remote_ip.get(),
                I::FAKE_CONFIG.local_ip.get(),
                TTL,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .ok()
            .unwrap()
            .unwrap_b();

        // Receive a frame from the network and verify delivery to the IP layer.
        assert_eq!(ctx.core_ctx.state.ip_counters::<I>().receive_ip_packet.get(), 0);
        ctx.core_api().device::<PureIpDevice>().receive_frame(
            PureIpDeviceFrameMetadata { device_id: device, ip_version: I::VERSION },
            packet,
        );
        assert_eq!(ctx.core_ctx.state.ip_counters::<I>().receive_ip_packet.get(), 1);
    }
}
