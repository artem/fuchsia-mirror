// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Testing-related utilities.

#![cfg(any(test, feature = "testutils"))]

#[cfg(test)]
use alloc::vec;
use alloc::{borrow::ToOwned, collections::HashMap, sync::Arc, vec::Vec};
use assert_matches::assert_matches;

use core::{
    borrow::Borrow,
    convert::Infallible as Never,
    ffi::CStr,
    fmt::Debug,
    hash::Hash,
    num::NonZeroU64,
    ops::{Deref, DerefMut},
    time::Duration,
};

use derivative::Derivative;
use lock_order::wrap::prelude::*;
#[cfg(test)]
use net_types::MulticastAddr;
use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnet, AddrSubnetEither, GenericOverIp, Ip, IpAddr, IpAddress, IpInvariant, IpVersion,
        Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu, Subnet, SubnetEither,
    },
    SpecifiedAddr, UnicastAddr, Witness as _,
};
use netstack3_filter::FilterTimerId;
use packet::{Buf, BufferMut};
use zerocopy::ByteSlice;

#[cfg(test)]
use crate::context::testutil::{FakeNetwork, FakeNetworkLinks, FakeNetworkSpec};
use crate::{
    context::{
        testutil::{
            FakeFrameCtx, FakeInstant, FakeTimerCtx, FakeTimerCtxExt, WithFakeFrameContext,
            WithFakeTimerContext,
        },
        CtxPair, DeferredResourceRemovalContext, EventContext, InstantBindingsTypes,
        InstantContext, ReferenceNotifiers, RngContext, TimerBindingsTypes, TimerContext,
        TimerHandler, TracingContext, UnlockedCoreCtx,
    },
    device::{
        ethernet::MaxEthernetFrameSize,
        ethernet::{EthernetCreationProperties, EthernetLinkDevice},
        link::LinkDevice,
        loopback::LoopbackDeviceId,
        DeviceClassMatcher, DeviceId, DeviceIdAndNameMatcher, DeviceLayerEventDispatcher,
        DeviceLayerStateTypes, DeviceLayerTypes, DeviceSendFrameError, EthernetDeviceId,
        EthernetWeakDeviceId, LoopbackCreationProperties, LoopbackDevice, PureIpDeviceId,
        PureIpWeakDeviceId, ReceiveQueueBindingsContext, TransmitQueueBindingsContext,
        WeakDeviceId,
    },
    filter::FilterBindingsTypes,
    ip::{
        self,
        device::{
            IpDeviceConfigurationUpdate, IpDeviceEvent, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate,
        },
        icmp::{IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpSocketId},
        nud::{self, LinkResolutionContext, LinkResolutionNotifier},
        raw::{RawIpSocketId, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes},
        AddRouteError, AddableEntryEither, AddableMetric, IpDeviceConfiguration, IpLayerEvent,
        IpLayerTimerId, RawMetric,
    },
    state::{StackState, StackStateBuilder},
    sync::{DynDebugReferences, Mutex},
    time::{TimerId, TimerIdInner},
    transport::{
        tcp::{
            buffer::{
                testutil::{ClientBuffers, ProvidedBuffers, TestSendBuffer},
                RingBuffer,
            },
            socket::TcpBindingsTypes,
            BufferSizes,
        },
        udp::{UdpBindingsTypes, UdpReceiveBindingsContext, UdpSocketId},
    },
    types::WorkQueueReport,
    BindingsContext, BindingsTypes,
};

pub use netstack3_base::testutil::{
    assert_empty, new_rng, run_with_many_seeds, set_logger_for_test, FakeCryptoRng,
    MonotonicIdentifier, TestAddrs, TestIpExt, TEST_ADDRS_V4, TEST_ADDRS_V6,
};

/// NDP test utilities.
pub mod ndp {
    pub use crate::ip::icmp::testutil::{
        neighbor_advertisement_ip_packet, neighbor_solicitation_ip_packet,
    };
}
/// Context test utilities.
pub mod context {
    pub use crate::context::testutil::*;
}

/// The default interface routing metric for test interfaces.
pub(crate) const DEFAULT_INTERFACE_METRIC: RawMetric = RawMetric(100);

/// Context available during the execution of the netstack.
pub type Ctx<BT> = CtxPair<StackState<BT>, BT>;

/// Extensions to [`CtxPair`] when it holds a full stack state.
pub trait CtxPairExt<BC: BindingsContext> {
    /// Retrieves the core and bindings context, respectively.
    ///
    /// This function can be used to call into non-api core functions that want
    /// a core context.
    fn contexts(&mut self) -> (UnlockedCoreCtx<'_, BC>, &mut BC);

    /// Retrieves a [`crate::api::CoreApi`] from this context pair.
    fn core_api(&mut self) -> crate::api::CoreApi<'_, &mut BC> {
        let (core_ctx, bindings_ctx) = self.contexts();
        crate::api::CoreApi::new(CtxPair { core_ctx, bindings_ctx })
    }

    /// Like [`CtxPairExt::contexts`], but retrieves only the core context.
    fn core_ctx(&self) -> UnlockedCoreCtx<'_, BC>;

    /// Retrieves a [`TestApi`] from this context pair.
    fn test_api(&mut self) -> TestApi<'_, BC> {
        let (core_ctx, bindings_ctx) = self.contexts();
        TestApi(core_ctx, bindings_ctx)
    }

    /// Shortcut for [`FakeTimerCtxExt::trigger_next_timer`].
    fn trigger_next_timer<Id>(&mut self) -> Option<Id>
    where
        BC: FakeTimerCtxExt<Id>,
        for<'a> UnlockedCoreCtx<'a, BC>: TimerHandler<BC, Id>,
    {
        let (mut core_ctx, bindings_ctx) = self.contexts();
        bindings_ctx.trigger_next_timer(&mut core_ctx)
    }

    /// Shortcut for [`FakeTimerCtxExt::trigger_timers_for`].
    fn trigger_timers_for<Id>(&mut self, duration: Duration) -> Vec<Id>
    where
        BC: FakeTimerCtxExt<Id>,
        for<'a> UnlockedCoreCtx<'a, BC>: TimerHandler<BC, Id>,
    {
        let (mut core_ctx, bindings_ctx) = self.contexts();
        bindings_ctx.trigger_timers_for(duration, &mut core_ctx)
    }

    /// Shortcut for [`FaketimerCtx::trigger_timers_until_instant`].
    fn trigger_timers_until_instant<Id>(&mut self, instant: FakeInstant) -> Vec<Id>
    where
        BC: FakeTimerCtxExt<Id>,
        for<'a> UnlockedCoreCtx<'a, BC>: TimerHandler<BC, Id>,
    {
        let (mut core_ctx, bindings_ctx) = self.contexts();
        bindings_ctx.trigger_timers_until_instant(instant, &mut core_ctx)
    }

    /// Shortcut for [`FakeTimerCtxExt::trigger_timers_until_and_expect_unordered`].
    fn trigger_timers_until_and_expect_unordered<Id, I: IntoIterator<Item = Id>>(
        &mut self,
        instant: FakeInstant,
        timers: I,
    ) where
        Id: Debug + Hash + Eq,
        BC: FakeTimerCtxExt<Id>,
        for<'a> UnlockedCoreCtx<'a, BC>: TimerHandler<BC, Id>,
    {
        let (mut core_ctx, bindings_ctx) = self.contexts();
        bindings_ctx.trigger_timers_until_and_expect_unordered(instant, timers, &mut core_ctx)
    }
}

impl<CC, BC> CtxPairExt<BC> for CtxPair<CC, BC>
where
    CC: Borrow<StackState<BC>>,
    BC: BindingsContext,
{
    fn contexts(&mut self) -> (UnlockedCoreCtx<'_, BC>, &mut BC) {
        let Self { core_ctx, bindings_ctx } = self;
        (UnlockedCoreCtx::new(CC::borrow(core_ctx)), bindings_ctx)
    }

    fn core_ctx(&self) -> UnlockedCoreCtx<'_, BC> {
        UnlockedCoreCtx::new(CC::borrow(&self.core_ctx))
    }
}

/// An API struct for test utilities.
pub struct TestApi<'a, BT: BindingsTypes>(UnlockedCoreCtx<'a, BT>, &'a mut BT);

impl<'l, BC> TestApi<'l, BC>
where
    BC: crate::BindingsContext,
{
    fn contexts(&mut self) -> (&mut UnlockedCoreCtx<'l, BC>, &mut BC) {
        let Self(core_ctx, bindings_ctx) = self;
        (core_ctx, bindings_ctx)
    }

    fn core_api(&mut self) -> crate::api::CoreApi<'_, &mut BC> {
        let (core_ctx, bindings_ctx) = self.contexts();
        let core_ctx = core_ctx.as_owned();
        crate::api::CoreApi::new(crate::context::CtxPair { core_ctx, bindings_ctx })
    }

    /// Joins the multicast group `multicast_addr` for `device`.
    #[netstack3_macros::context_ip_bounds(A::Version, BC, crate)]
    #[cfg(test)]
    pub fn join_ip_multicast<A: IpAddress>(
        &mut self,
        device: &DeviceId<BC>,
        multicast_addr: MulticastAddr<A>,
    ) where
        A::Version: crate::IpExt,
    {
        let (core_ctx, bindings_ctx) = self.contexts();
        ip::device::join_ip_multicast::<A::Version, _, _>(
            core_ctx,
            bindings_ctx,
            device,
            multicast_addr,
        );
    }

    /// Leaves the multicast group `multicast_addr` for `device`.
    #[cfg(test)]
    #[netstack3_macros::context_ip_bounds(A::Version, BC, crate)]
    pub fn leave_ip_multicast<A: IpAddress>(
        &mut self,
        device: &DeviceId<BC>,
        multicast_addr: MulticastAddr<A>,
    ) where
        A::Version: crate::IpExt,
    {
        let (core_ctx, bindings_ctx) = self.contexts();
        ip::device::leave_ip_multicast::<A::Version, _, _>(
            core_ctx,
            bindings_ctx,
            device,
            multicast_addr,
        );
    }

    /// Returns whether `device` is in the multicast group `addr`.
    #[cfg(test)]
    #[netstack3_macros::context_ip_bounds(A::Version, BC, crate)]
    pub fn is_in_ip_multicast<A: IpAddress>(
        &mut self,
        device: &DeviceId<BC>,
        addr: MulticastAddr<A>,
    ) -> bool
    where
        A::Version: crate::IpExt,
    {
        use ip::{
            AddressStatus, IpDeviceStateContext, IpLayerIpExt, Ipv4PresentAddressStatus,
            Ipv6PresentAddressStatus,
        };

        let (core_ctx, _) = self.contexts();
        let addr_status = IpDeviceStateContext::<A::Version, _>::address_status_for_device(
            core_ctx,
            addr.into_specified(),
            device,
        );
        let status = match addr_status {
            AddressStatus::Present(p) => p,
            AddressStatus::Unassigned => return false,
        };
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<I: IpLayerIpExt>(I::AddressStatus);
        A::Version::map_ip(
            Wrap(status),
            |Wrap(v4)| match v4 {
                Ipv4PresentAddressStatus::Multicast => true,
                Ipv4PresentAddressStatus::LimitedBroadcast
                | Ipv4PresentAddressStatus::SubnetBroadcast
                | Ipv4PresentAddressStatus::LoopbackSubnet
                | Ipv4PresentAddressStatus::Unicast => false,
            },
            |Wrap(v6)| match v6 {
                Ipv6PresentAddressStatus::Multicast => true,
                Ipv6PresentAddressStatus::UnicastAssigned
                | Ipv6PresentAddressStatus::UnicastTentative => false,
            },
        )
    }

    /// Receive an IP packet from a device.
    ///
    /// `receive_ip_packet` injects a packet directly at the IP layer for this
    /// context.
    #[cfg(test)]
    pub fn receive_ip_packet<I: Ip, B: BufferMut>(
        &mut self,
        device: &DeviceId<BC>,
        frame_dst: Option<crate::device::FrameDestination>,
        buffer: B,
    ) {
        let (core_ctx, bindings_ctx) = self.contexts();
        match I::VERSION {
            IpVersion::V4 => {
                ip::receive_ipv4_packet(core_ctx, bindings_ctx, device, frame_dst, buffer)
            }
            IpVersion::V6 => {
                ip::receive_ipv6_packet(core_ctx, bindings_ctx, device, frame_dst, buffer)
            }
        }
    }

    /// Add a route directly to the forwarding table.
    pub fn add_route(
        &mut self,
        entry: AddableEntryEither<crate::device::DeviceId<BC>>,
    ) -> Result<(), AddRouteError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        match entry {
            AddableEntryEither::V4(entry) => {
                ip::testutil::add_route::<Ipv4, _, _>(core_ctx, bindings_ctx, entry)
            }
            AddableEntryEither::V6(entry) => {
                ip::testutil::add_route::<Ipv6, _, _>(core_ctx, bindings_ctx, entry)
            }
        }
    }

    /// Delete a route from the forwarding table, returning `Err` if no route
    /// was found to be deleted.
    pub fn del_routes_to_subnet(
        &mut self,
        subnet: net_types::ip::SubnetEither,
    ) -> Result<(), crate::error::NotFoundError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        match subnet {
            SubnetEither::V4(subnet) => {
                ip::testutil::del_routes_to_subnet::<Ipv4, _, _>(core_ctx, bindings_ctx, subnet)
            }
            SubnetEither::V6(subnet) => {
                ip::testutil::del_routes_to_subnet::<Ipv6, _, _>(core_ctx, bindings_ctx, subnet)
            }
        }
    }

    /// Deletes all routes targeting `device`.
    pub(crate) fn del_device_routes(&mut self, device: &DeviceId<BC>) {
        let (core_ctx, bindings_ctx) = self.contexts();
        ip::testutil::del_device_routes::<Ipv4, _, _>(core_ctx, bindings_ctx, device);
        ip::testutil::del_device_routes::<Ipv6, _, _>(core_ctx, bindings_ctx, device);
    }

    /// Removes all of the routes through the device, then removes the device.
    pub fn clear_routes_and_remove_ethernet_device(
        &mut self,
        ethernet_device: crate::device::EthernetDeviceId<BC>,
    ) {
        let device_id = ethernet_device.into();
        self.del_device_routes(&device_id);
        let ethernet_device =
            assert_matches!(device_id, crate::device::DeviceId::Ethernet(id) => id);
        match self.core_api().device().remove_device(ethernet_device) {
            crate::sync::RemoveResourceResult::Removed(_external_state) => {}
            crate::sync::RemoveResourceResult::Deferred(_reference_receiver) => {
                panic!("failed to remove ethernet device")
            }
        }
    }

    /// Enables or disables the device for IP version `I` and returns whether it
    /// was enabled before.
    #[netstack3_macros::context_ip_bounds(I, BC, crate)]
    pub fn set_ip_device_enabled<I: crate::IpExt>(
        &mut self,
        device: &DeviceId<BC>,
        enabled: bool,
    ) -> bool {
        let update =
            IpDeviceConfigurationUpdate { ip_enabled: Some(enabled), ..Default::default() };
        let prev =
            self.core_api().device_ip::<I>().update_configuration(device, update.into()).unwrap();
        prev.as_ref().ip_enabled.unwrap()
    }

    /// Enables `device`.
    pub fn enable_device(&mut self, device: &DeviceId<BC>) {
        let _was_enabled: bool = self.set_ip_device_enabled::<Ipv4>(device, true);
        let _was_enabled: bool = self.set_ip_device_enabled::<Ipv6>(device, true);
    }

    /// Enables or disables IP packet routing on `device`.
    #[netstack3_macros::context_ip_bounds(I, BC, crate)]
    pub fn set_forwarding_enabled<I: crate::IpExt>(
        &mut self,
        device: &DeviceId<BC>,
        enabled: bool,
    ) {
        let _config = self
            .core_api()
            .device_ip::<I>()
            .update_configuration(
                device,
                IpDeviceConfigurationUpdate {
                    forwarding_enabled: Some(enabled),
                    ..Default::default()
                }
                .into(),
            )
            .unwrap();
    }

    /// Returns whether IP packet routing is enabled on `device`.
    #[netstack3_macros::context_ip_bounds(I, BC, crate)]
    pub fn is_forwarding_enabled<I: crate::IpExt>(&mut self, device: &DeviceId<BC>) -> bool {
        let configuration = self.core_api().device_ip::<I>().get_configuration(device);
        let IpDeviceConfiguration { forwarding_enabled, .. } = configuration.as_ref();
        *forwarding_enabled
    }

    /// Adds a loopback device with the IPv4/IPv6 loopback addresses assigned.
    pub fn add_loopback(&mut self) -> LoopbackDeviceId<BC>
    where
        <BC as DeviceLayerStateTypes>::DeviceIdentifier: Default,
        <BC as DeviceLayerStateTypes>::LoopbackDeviceState: Default,
    {
        let loopback_id = self.core_api().device::<LoopbackDevice>().add_device_with_default_state(
            LoopbackCreationProperties { mtu: Mtu::new(u32::MAX) },
            DEFAULT_INTERFACE_METRIC,
        );
        let device_id: DeviceId<_> = loopback_id.clone().into();
        self.enable_device(&device_id);

        self.core_api()
            .device_ip::<Ipv4>()
            .add_ip_addr_subnet(
                &device_id,
                AddrSubnet::from_witness(Ipv4::LOOPBACK_ADDRESS, Ipv4::LOOPBACK_SUBNET.prefix())
                    .unwrap(),
            )
            .unwrap();

        self.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(
                &device_id,
                AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                    .unwrap(),
            )
            .unwrap();
        loopback_id
    }
}

impl<'a> TestApi<'a, FakeBindingsCtx> {
    /// Handles any pending frames and returns true if any frames that were in
    /// the RX queue were processed.
    pub fn handle_queued_rx_packets(&mut self) -> bool {
        let mut handled = false;
        loop {
            let (_, bindings_ctx) = self.contexts();
            let rx_available = core::mem::take(&mut bindings_ctx.state_mut().rx_available);
            if rx_available.len() == 0 {
                break handled;
            }
            handled = true;
            for id in rx_available.into_iter() {
                loop {
                    match self.core_api().receive_queue().handle_queued_frames(&id) {
                        WorkQueueReport::AllDone => break,
                        WorkQueueReport::Pending => (),
                    }
                }
            }
        }
    }
}

#[derive(Default)]
/// Bindings context state held by [`FakeBindingsCtx`].
pub struct FakeBindingsCtxState {
    icmpv4_replies:
        HashMap<IcmpSocketId<Ipv4, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>, Vec<Vec<u8>>>,
    icmpv6_replies:
        HashMap<IcmpSocketId<Ipv6, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>, Vec<Vec<u8>>>,
    udpv4_received:
        HashMap<UdpSocketId<Ipv4, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>, Vec<Vec<u8>>>,
    udpv6_received:
        HashMap<UdpSocketId<Ipv6, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>, Vec<Vec<u8>>>,
    pub(crate) rx_available: Vec<LoopbackDeviceId<FakeBindingsCtx>>,
    pub(crate) tx_available: Vec<DeviceId<FakeBindingsCtx>>,
}

impl FakeBindingsCtxState {
    pub(crate) fn udp_state_mut<I: crate::IpExt>(
        &mut self,
    ) -> &mut HashMap<UdpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>, Vec<Vec<u8>>>
    {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrapper<'a, I: crate::IpExt>(
            &'a mut HashMap<
                UdpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>,
                Vec<Vec<u8>>,
            >,
        );
        let Wrapper(map) = I::map_ip::<_, Wrapper<'_, I>>(
            IpInvariant(self),
            |IpInvariant(this)| Wrapper(&mut this.udpv4_received),
            |IpInvariant(this)| Wrapper(&mut this.udpv6_received),
        );
        map
    }
}

/// Shorthand for [`Ctx`] with a [`FakeBindingsCtx`].
pub type FakeCtx = Ctx<FakeBindingsCtx>;
/// Shorthand for [`StackState`] that uses a [`FakeBindingsCtx`].
pub type FakeCoreCtx = StackState<FakeBindingsCtx>;

type InnerFakeBindingsCtx = crate::context::testutil::FakeBindingsCtx<
    TimerId<FakeBindingsCtx>,
    DispatchedEvent,
    FakeBindingsCtxState,
    DispatchedFrame,
>;

/// Test-only implementation of [`crate::BindingsContext`].
#[derive(Default, Clone)]
pub struct FakeBindingsCtx(Arc<Mutex<InnerFakeBindingsCtx>>);

/// A wrapper type that makes it easier to implement `Deref` (and optionally
/// `DerefMut`) for a value that is protected by a lock.
///
/// The first field is the type that provides access to the inner value,
/// probably a lock guard. The second and third fields are functions that, given
/// the first field, provide shared and mutable access (respectively) to the
/// inner value.
// TODO(https://github.com/rust-lang/rust/issues/117108): Replace this with
// mapped mutex guards once stable.
struct Wrapper<S, Callback, CallbackMut>(S, Callback, CallbackMut);

impl<T: ?Sized, S: Deref, Callback: for<'a> Fn(&'a <S as Deref>::Target) -> &'a T, CallbackMut>
    Deref for Wrapper<S, Callback, CallbackMut>
{
    type Target = T;

    fn deref(&self) -> &T {
        let Self(guard, f, _) = self;
        let target = guard.deref();
        f(target)
    }
}

impl<
        T: ?Sized,
        S: DerefMut,
        Callback: for<'a> Fn(&'a <S as Deref>::Target) -> &'a T,
        CallbackMut: for<'a> Fn(&'a mut <S as Deref>::Target) -> &'a mut T,
    > DerefMut for Wrapper<S, Callback, CallbackMut>
{
    fn deref_mut(&mut self) -> &mut T {
        let Self(guard, _, f) = self;
        let target = guard.deref_mut();
        f(target)
    }
}

impl FakeBindingsCtx {
    fn with_inner<F: FnOnce(&InnerFakeBindingsCtx) -> O, O>(&self, f: F) -> O {
        let Self(this) = self;
        let locked = this.lock();
        f(&*locked)
    }

    fn with_inner_mut<F: FnOnce(&mut InnerFakeBindingsCtx) -> O, O>(&self, f: F) -> O {
        let Self(this) = self;
        let mut locked = this.lock();
        f(&mut *locked)
    }

    #[cfg(test)]
    pub(crate) fn timer_ctx(&self) -> impl Deref<Target = FakeTimerCtx<TimerId<Self>>> + '_ {
        // NB: Helper function is required to satisfy lifetime requirements of
        // borrow.
        fn get_timers<'a>(
            i: &'a InnerFakeBindingsCtx,
        ) -> &'a FakeTimerCtx<TimerId<FakeBindingsCtx>> {
            &i.timers
        }
        Wrapper(self.0.lock(), get_timers, ())
    }

    pub(crate) fn state_mut(&mut self) -> impl DerefMut<Target = FakeBindingsCtxState> + '_ {
        // NB: Helper functions are required to satisfy lifetime requirements of
        // borrow.
        fn get_state<'a>(i: &'a InnerFakeBindingsCtx) -> &'a FakeBindingsCtxState {
            &i.state
        }
        fn get_state_mut<'a>(i: &'a mut InnerFakeBindingsCtx) -> &'a mut FakeBindingsCtxState {
            &mut i.state
        }
        Wrapper(self.0.lock(), get_state, get_state_mut)
    }

    /// Copy all ethernet frames sent so far.
    ///
    /// # Panics
    ///
    /// Panics if the there are non-Ethernet frames stored.
    #[cfg(test)]
    pub fn copy_ethernet_frames(
        &mut self,
    ) -> Vec<(EthernetWeakDeviceId<FakeBindingsCtx>, Vec<u8>)> {
        self.with_inner_mut(|ctx| {
            ctx.frames
                .frames()
                .into_iter()
                .map(|(meta, frame)| match meta {
                    DispatchedFrame::Ethernet(eth) => (eth.clone(), frame.clone()),
                    DispatchedFrame::PureIp(ip) => panic!("unexpected IP packet {ip:?}: {frame:?}"),
                })
                .collect()
        })
    }

    /// Take all ethernet frames sent so far.
    ///
    /// # Panics
    ///
    /// Panics if the there are non-Ethernet frames stored.
    pub fn take_ethernet_frames(
        &mut self,
    ) -> Vec<(EthernetWeakDeviceId<FakeBindingsCtx>, Vec<u8>)> {
        self.with_inner_mut(|ctx| {
            ctx.frames
                .take_frames()
                .into_iter()
                .map(|(meta, frame)| match meta {
                    DispatchedFrame::Ethernet(eth) => (eth, frame),
                    DispatchedFrame::PureIp(ip) => panic!("unexpected IP packet {ip:?}: {frame:?}"),
                })
                .collect()
        })
    }

    /// Take all IP frames sent so far.
    ///
    /// # Panics
    ///
    /// Panics if the there are non-IP frames stored.
    pub fn take_ip_frames(&mut self) -> Vec<(PureIpDeviceAndIpVersion<FakeBindingsCtx>, Vec<u8>)> {
        self.with_inner_mut(|ctx| {
            ctx.frames
                .take_frames()
                .into_iter()
                .map(|(meta, frame)| match meta {
                    DispatchedFrame::Ethernet(eth) => {
                        panic!("unexpected Ethernet frame {eth:?}: {frame:?}")
                    }
                    DispatchedFrame::PureIp(ip) => (ip, frame),
                })
                .collect()
        })
    }

    #[cfg(test)]
    pub(crate) fn take_events(&mut self) -> Vec<DispatchedEvent> {
        self.with_inner_mut(|ctx| ctx.events.take())
    }

    /// Takes all the received ICMP replies for a given `conn`.
    #[cfg(test)]
    pub(crate) fn take_icmp_replies<I: crate::IpExt>(
        &mut self,
        conn: &IcmpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>,
    ) -> Vec<Vec<u8>> {
        I::map_ip::<_, IpInvariant<Option<Vec<_>>>>(
            (IpInvariant(self), conn),
            |(IpInvariant(this), conn)| IpInvariant(this.state_mut().icmpv4_replies.remove(conn)),
            |(IpInvariant(this), conn)| IpInvariant(this.state_mut().icmpv6_replies.remove(conn)),
        )
        .into_inner()
        .unwrap_or_else(Vec::default)
    }

    #[cfg(test)]
    pub(crate) fn take_udp_received<I: crate::IpExt>(
        &mut self,
        conn: &UdpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>,
    ) -> Vec<Vec<u8>> {
        self.state_mut().udp_state_mut::<I>().remove(conn).unwrap_or_else(Vec::default)
    }
}

impl FilterBindingsTypes for FakeBindingsCtx {
    type DeviceClass = ();
}

impl DeviceClassMatcher<()> for () {
    fn device_class_matches(&self, (): &()) -> bool {
        unimplemented!()
    }
}

impl WithFakeTimerContext<TimerId<FakeBindingsCtx>> for FakeBindingsCtx {
    fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &self,
        f: F,
    ) -> O {
        self.with_inner(|ctx| f(&ctx.timers))
    }

    fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId<FakeBindingsCtx>>) -> O>(
        &mut self,
        f: F,
    ) -> O {
        self.with_inner_mut(|ctx| f(&mut ctx.timers))
    }
}

impl WithFakeFrameContext<DispatchedFrame> for FakeBindingsCtx {
    fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<DispatchedFrame>) -> O>(
        &mut self,
        f: F,
    ) -> O {
        self.with_inner_mut(|ctx| f(&mut ctx.frames))
    }
}

impl InstantBindingsTypes for FakeBindingsCtx {
    type Instant = FakeInstant;
}

impl InstantContext for FakeBindingsCtx {
    fn now(&self) -> FakeInstant {
        self.with_inner(|ctx| ctx.now())
    }
}

impl TimerBindingsTypes for FakeBindingsCtx {
    type Timer =
        <crate::context::testutil::FakeTimerCtx<TimerId<Self>> as TimerBindingsTypes>::Timer;
    type DispatchId = TimerId<Self>;
}

impl TimerContext for FakeBindingsCtx {
    fn new_timer(&mut self, id: Self::DispatchId) -> Self::Timer {
        self.with_inner_mut(|ctx| ctx.new_timer(id))
    }

    fn schedule_timer_instant(
        &mut self,
        time: Self::Instant,
        timer: &mut Self::Timer,
    ) -> Option<Self::Instant> {
        // Filter out conntrack GC timers. We don't need conntrack GC in most
        // tests, and this causes issues with tests that are expecting the
        // netstack to quiesce.
        match timer.dispatch_id.0 {
            TimerIdInner::IpLayer(IpLayerTimerId::FilterTimerv4(FilterTimerId::ConntrackGc(_)))
            | TimerIdInner::IpLayer(IpLayerTimerId::FilterTimerv6(FilterTimerId::ConntrackGc(_))) => {
                return None
            }
            _ => {}
        }
        self.with_inner_mut(|ctx| ctx.schedule_timer_instant(time, timer))
    }

    fn cancel_timer(&mut self, timer: &mut Self::Timer) -> Option<Self::Instant> {
        self.with_inner_mut(|ctx| ctx.cancel_timer(timer))
    }

    fn scheduled_instant(&self, timer: &mut Self::Timer) -> Option<Self::Instant> {
        self.with_inner_mut(|ctx| ctx.scheduled_instant(timer))
    }
}

impl RngContext for FakeBindingsCtx {
    type Rng<'a> = FakeCryptoRng;

    fn rng(&mut self) -> Self::Rng<'_> {
        let Self(this) = self;
        this.lock().rng()
    }
}

impl<T: Into<DispatchedEvent>> EventContext<T> for FakeBindingsCtx {
    fn on_event(&mut self, event: T) {
        self.with_inner_mut(|ctx| ctx.events.on_event(event.into()))
    }
}

impl TracingContext for FakeBindingsCtx {
    type DurationScope = ();

    fn duration(&self, _: &'static CStr) {}
}

impl TcpBindingsTypes for FakeBindingsCtx {
    type ReceiveBuffer = Arc<Mutex<RingBuffer>>;

    type SendBuffer = TestSendBuffer;

    type ReturnedBuffers = ClientBuffers;

    type ListenerNotifierOrProvidedBuffers = ProvidedBuffers;

    fn new_passive_open_buffers(
        buffer_sizes: BufferSizes,
    ) -> (Self::ReceiveBuffer, Self::SendBuffer, Self::ReturnedBuffers) {
        let client = ClientBuffers::new(buffer_sizes);
        (
            Arc::clone(&client.receive),
            TestSendBuffer::new(Arc::clone(&client.send), RingBuffer::default()),
            client,
        )
    }

    fn default_buffer_sizes() -> BufferSizes {
        // Use the test-only default impl.
        BufferSizes::default()
    }
}

impl ReferenceNotifiers for FakeBindingsCtx {
    type ReferenceReceiver<T: 'static> = Never;

    type ReferenceNotifier<T: Send + 'static> = Never;

    fn new_reference_notifier<T: Send + 'static>(
        debug_references: DynDebugReferences,
    ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
        // NB: We don't want deferred destruction in core tests. These are
        // always single-threaded and single-task, and we want to encourage
        // explicit cleanup.
        panic!(
            "FakeBindingsCtx can't create deferred reference notifiers for type {}: \
            debug_references={debug_references:?}",
            core::any::type_name::<T>()
        );
    }
}

impl DeferredResourceRemovalContext for FakeBindingsCtx {
    fn defer_removal<T: Send + 'static>(&mut self, receiver: Self::ReferenceReceiver<T>) {
        match receiver {}
    }
}

/// A link resolution notifier that ignores all notifications.
#[derive(Debug)]
pub struct NoOpLinkResolutionNotifier;

impl<D: LinkDevice> LinkResolutionContext<D> for FakeBindingsCtx {
    type Notifier = NoOpLinkResolutionNotifier;
}

impl<D: LinkDevice> LinkResolutionNotifier<D> for NoOpLinkResolutionNotifier {
    type Observer = ();

    fn new() -> (Self, Self::Observer) {
        (NoOpLinkResolutionNotifier, ())
    }

    fn notify(self, _result: Result<D::Address, crate::error::AddressResolutionFailed>) {}
}

#[derive(Clone)]
struct DeviceConfig {
    mac: UnicastAddr<Mac>,
    addr_subnet: Option<AddrSubnetEither>,
    ipv4_config: Option<Ipv4DeviceConfigurationUpdate>,
    ipv6_config: Option<Ipv6DeviceConfigurationUpdate>,
}

/// A builder for `FakeCtx`s.
///
/// A `FakeCtxBuilder` is capable of storing the configuration of a network
/// stack including forwarding table entries, devices and their assigned
/// addresses and configurations, ARP table entries, etc. It can be built using
/// `build`, producing a `FakeCtx` with all of the appropriate state configured.
#[derive(Clone, Default)]
pub struct FakeCtxBuilder {
    devices: Vec<DeviceConfig>,
    // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
    arp_table_entries: Vec<(usize, SpecifiedAddr<Ipv4Addr>, UnicastAddr<Mac>)>,
    ndp_table_entries: Vec<(usize, UnicastAddr<Ipv6Addr>, UnicastAddr<Mac>)>,
    // usize refers to index into devices Vec.
    device_routes: Vec<(SubnetEither, usize)>,
}

impl FakeCtxBuilder {
    /// Construct a `FakeCtxBuilder` from a `TestAddrs`.
    pub fn with_addrs<A: IpAddress>(addrs: TestAddrs<A>) -> FakeCtxBuilder {
        assert!(addrs.subnet.contains(&addrs.local_ip));
        assert!(addrs.subnet.contains(&addrs.remote_ip));

        let mut builder = FakeCtxBuilder::default();
        builder.devices.push(DeviceConfig {
            mac: addrs.local_mac,
            addr_subnet: Some(
                AddrSubnetEither::new(addrs.local_ip.get().into(), addrs.subnet.prefix()).unwrap(),
            ),
            ipv4_config: None,
            ipv6_config: None,
        });

        match addrs.remote_ip.into() {
            IpAddr::V4(ip) => builder.arp_table_entries.push((0, ip, addrs.remote_mac)),
            IpAddr::V6(ip) => builder.ndp_table_entries.push((
                0,
                UnicastAddr::new(ip.get()).unwrap(),
                addrs.remote_mac,
            )),
        };

        // Even with fixed ipv4 address we can have IPv6 link local addresses
        // pre-cached.
        builder.ndp_table_entries.push((
            0,
            addrs.remote_mac.to_ipv6_link_local().addr().get(),
            addrs.remote_mac,
        ));

        builder.device_routes.push((addrs.subnet.into(), 0));
        builder
    }

    /// Add a device.
    ///
    /// `add_device` returns a key which can be used to refer to the device in
    /// future calls to `add_arp_table_entry` and `add_device_route`.
    pub fn add_device(&mut self, mac: UnicastAddr<Mac>) -> usize {
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: None,
            ipv4_config: None,
            ipv6_config: None,
        });
        idx
    }

    /// Add a device with an IPv4 and IPv6 configuration.
    ///
    /// `add_device_with_config` is like `add_device`, except that it takes an
    /// IPv4 and IPv6 configuration to apply to the device when it is enabled.
    #[cfg(test)]
    pub(crate) fn add_device_with_config(
        &mut self,
        mac: UnicastAddr<Mac>,
        ipv4_config: Ipv4DeviceConfigurationUpdate,
        ipv6_config: Ipv6DeviceConfigurationUpdate,
    ) -> usize {
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: None,
            ipv4_config: Some(ipv4_config),
            ipv6_config: Some(ipv6_config),
        });
        idx
    }

    /// Add a device with an associated IP address.
    ///
    /// `add_device_with_ip` is like `add_device`, except that it takes an
    /// associated IP address and subnet to assign to the device.
    pub fn add_device_with_ip<A: IpAddress>(
        &mut self,
        mac: UnicastAddr<Mac>,
        ip: A,
        subnet: Subnet<A>,
    ) -> usize {
        assert!(subnet.contains(&ip));
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: Some(AddrSubnetEither::new(ip.into(), subnet.prefix()).unwrap()),
            ipv4_config: None,
            ipv6_config: None,
        });
        self.device_routes.push((subnet.into(), idx));
        idx
    }

    /// Add a device with an associated IP address and a particular IPv4 and
    /// IPv6 configuration.
    ///
    /// `add_device_with_ip_and_config` is like `add_device`, except that it
    /// takes an associated IP address and subnet to assign to the device, as
    /// well as IPv4 and IPv6 configurations to apply to the device when it is
    /// enabled.
    #[cfg(test)]
    pub(crate) fn add_device_with_ip_and_config<A: IpAddress>(
        &mut self,
        mac: UnicastAddr<Mac>,
        ip: A,
        subnet: Subnet<A>,
        ipv4_config: Ipv4DeviceConfigurationUpdate,
        ipv6_config: Ipv6DeviceConfigurationUpdate,
    ) -> usize {
        assert!(subnet.contains(&ip));
        let idx = self.devices.len();
        self.devices.push(DeviceConfig {
            mac,
            addr_subnet: Some(AddrSubnetEither::new(ip.into(), subnet.prefix()).unwrap()),
            ipv4_config: Some(ipv4_config),
            ipv6_config: Some(ipv6_config),
        });
        self.device_routes.push((subnet.into(), idx));
        idx
    }

    /// Add an ARP table entry for a device's ARP table.
    pub fn add_arp_table_entry(
        &mut self,
        device: usize,
        // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        ip: SpecifiedAddr<Ipv4Addr>,
        mac: UnicastAddr<Mac>,
    ) {
        self.arp_table_entries.push((device, ip, mac));
    }

    /// Add an NDP table entry for a device's NDP table.
    pub fn add_ndp_table_entry(
        &mut self,
        device: usize,
        // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        ip: UnicastAddr<Ipv6Addr>,
        mac: UnicastAddr<Mac>,
    ) {
        self.ndp_table_entries.push((device, ip, mac));
    }

    /// Add either an NDP entry (if IPv6) or ARP entry (if IPv4) to a
    /// `FakeCtxBuilder`.
    pub fn add_arp_or_ndp_table_entry<A: IpAddress>(
        &mut self,
        device: usize,
        // TODO(https://fxbug.dev/42083952): Use NeighborAddr when available.
        ip: SpecifiedAddr<A>,
        mac: UnicastAddr<Mac>,
    ) {
        match ip.into() {
            IpAddr::V4(ip) => self.add_arp_table_entry(device, ip, mac),
            IpAddr::V6(ip) => {
                self.add_ndp_table_entry(device, UnicastAddr::new(ip.get()).unwrap(), mac)
            }
        }
    }

    /// Builds a `Ctx` from the present configuration with a default dispatcher.
    pub fn build(self) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        self.build_with_modifications(|_| {})
    }

    /// `build_with_modifications` is equivalent to `build`, except that after
    /// the `StackStateBuilder` is initialized, it is passed to `f` for further
    /// modification before the `Ctx` is constructed.
    pub(crate) fn build_with_modifications<F: FnOnce(&mut StackStateBuilder)>(
        self,
        f: F,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        let mut stack_builder = StackStateBuilder::default();
        f(&mut stack_builder);
        self.build_with(stack_builder)
    }

    /// Build a `Ctx` from the present configuration with a caller-provided
    /// dispatcher and `StackStateBuilder`.
    pub(crate) fn build_with(
        self,
        state_builder: StackStateBuilder,
    ) -> (FakeCtx, Vec<EthernetDeviceId<FakeBindingsCtx>>) {
        let mut ctx = Ctx::new_with_builder(state_builder);

        let FakeCtxBuilder { devices, arp_table_entries, ndp_table_entries, device_routes } = self;
        let idx_to_device_id: Vec<_> = devices
            .into_iter()
            .map(|DeviceConfig { mac, addr_subnet: ip_and_subnet, ipv4_config, ipv6_config }| {
                let eth_id =
                    ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                        EthernetCreationProperties {
                            mac: mac,
                            max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                        },
                        DEFAULT_INTERFACE_METRIC,
                    );
                let id = eth_id.clone().into();
                if let Some(ipv4_config) = ipv4_config {
                    let _previous = ctx
                        .core_api()
                        .device_ip::<Ipv4>()
                        .update_configuration(&id, ipv4_config)
                        .unwrap();
                }
                if let Some(ipv6_config) = ipv6_config {
                    let _previous = ctx
                        .core_api()
                        .device_ip::<Ipv6>()
                        .update_configuration(&id, ipv6_config)
                        .unwrap();
                }
                ctx.test_api().enable_device(&id);
                match ip_and_subnet {
                    Some(addr_sub) => {
                        ctx.core_api().device_ip_any().add_ip_addr_subnet(&id, addr_sub).unwrap();
                    }
                    None => {}
                }
                eth_id
            })
            .collect();
        for (idx, ip, mac) in arp_table_entries {
            let device = &idx_to_device_id[idx];
            ctx.core_api()
                .neighbor::<Ipv4, EthernetLinkDevice>()
                .insert_static_entry(&device, ip.get(), mac.get())
                .expect("error inserting static ARP entry");
        }
        for (idx, ip, mac) in ndp_table_entries {
            let device = &idx_to_device_id[idx];
            ctx.core_api()
                .neighbor::<Ipv6, EthernetLinkDevice>()
                .insert_static_entry(&device, ip.get(), mac.get())
                .expect("error inserting static NDP entry");
        }

        for (subnet, idx) in device_routes {
            let device = &idx_to_device_id[idx];
            ctx.test_api()
                .add_route(AddableEntryEither::without_gateway(
                    subnet,
                    device.clone().into(),
                    AddableMetric::ExplicitMetric(RawMetric(0)),
                ))
                .expect("add device route");
        }

        (ctx, idx_to_device_id)
    }
}

#[cfg(test)]
pub(crate) enum FakeCtxNetworkSpec {}

#[cfg(test)]
impl FakeNetworkSpec for FakeCtxNetworkSpec {
    type Context = FakeCtx;
    type TimerId = TimerId<FakeBindingsCtx>;
    type SendMeta = DispatchedFrame;
    type RecvMeta = EthernetDeviceId<FakeBindingsCtx>;
    fn handle_frame(ctx: &mut FakeCtx, device_id: Self::RecvMeta, data: Buf<Vec<u8>>) {
        ctx.core_api()
            .device::<crate::device::ethernet::EthernetLinkDevice>()
            .receive_frame(crate::device::ethernet::RecvEthernetFrameMeta { device_id }, data)
    }
    fn handle_timer(ctx: &mut FakeCtx, timer: Self::TimerId) {
        ctx.core_api().handle_timer(timer)
    }
    fn process_queues(ctx: &mut FakeCtx) -> bool {
        ctx.test_api().handle_queued_rx_packets()
    }
    fn fake_frames(ctx: &mut FakeCtx) -> &mut impl WithFakeFrameContext<Self::SendMeta> {
        &mut ctx.bindings_ctx
    }
}

impl<I: crate::IpExt> UdpReceiveBindingsContext<I, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_udp<B: BufferMut>(
        &mut self,
        id: &UdpSocketId<I, WeakDeviceId<Self>, FakeBindingsCtx>,
        _device: &DeviceId<Self>,
        _dst_addr: (<I>::Addr, core::num::NonZeroU16),
        _src_addr: (<I>::Addr, Option<core::num::NonZeroU16>),
        body: &B,
    ) {
        let mut state = self.state_mut();
        let received =
            (&mut *state).udp_state_mut::<I>().entry(id.clone()).or_insert_with(Vec::default);
        received.push(body.as_ref().to_owned());
    }
}

impl UdpBindingsTypes for FakeBindingsCtx {
    type ExternalData<I: Ip> = ();
}

impl<I: crate::IpExt> IcmpEchoBindingsContext<I, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        conn: &IcmpSocketId<I, WeakDeviceId<FakeBindingsCtx>, FakeBindingsCtx>,
        _device: &DeviceId<Self>,
        _src_ip: I::Addr,
        _dst_ip: I::Addr,
        _id: u16,
        data: B,
    ) {
        I::map_ip(
            (IpInvariant(self.state_mut()), conn.clone()),
            |(IpInvariant(mut state), conn)| {
                let replies = state.icmpv4_replies.entry(conn).or_insert_with(Vec::default);
                replies.push(data.as_ref().to_owned());
            },
            |(IpInvariant(mut state), conn)| {
                let replies = state.icmpv6_replies.entry(conn).or_insert_with(Vec::default);
                replies.push(data.as_ref().to_owned());
            },
        )
    }
}

impl IcmpEchoBindingsTypes for FakeBindingsCtx {
    type ExternalData<I: Ip> = ();
}

impl crate::device::socket::DeviceSocketTypes for FakeBindingsCtx {
    type SocketState = Mutex<Vec<(WeakDeviceId<FakeBindingsCtx>, Vec<u8>)>>;
}

impl RawIpSocketsBindingsTypes for FakeBindingsCtx {
    type RawIpSocketState<I: Ip> = ();
}

impl crate::device::socket::DeviceSocketBindingsContext<DeviceId<Self>> for FakeBindingsCtx {
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &DeviceId<Self>,
        _frame: crate::device::socket::Frame<&[u8]>,
        raw_frame: &[u8],
    ) {
        state.lock().push((device.downgrade(), raw_frame.into()));
    }
}

impl<I: crate::IpExt> RawIpSocketsBindingsContext<I, DeviceId<Self>> for FakeBindingsCtx {
    fn receive_packet<B: ByteSlice>(
        &self,
        _socket: &RawIpSocketId<I, Self>,
        _packet: &I::Packet<B>,
        _device: &DeviceId<Self>,
    ) {
        unimplemented!()
    }
}

impl DeviceLayerStateTypes for FakeBindingsCtx {
    type LoopbackDeviceState = ();
    type EthernetDeviceState = ();
    type PureIpDeviceState = ();
    type DeviceIdentifier = MonotonicIdentifier;
}

impl ReceiveQueueBindingsContext<LoopbackDeviceId<Self>> for FakeBindingsCtx {
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<FakeBindingsCtx>) {
        self.state_mut().rx_available.push(device.clone());
    }
}

impl<D: Clone + Into<DeviceId<Self>>> TransmitQueueBindingsContext<D> for FakeBindingsCtx {
    fn wake_tx_task(&mut self, device: &D) {
        self.state_mut().tx_available.push(device.clone().into());
    }
}

impl DeviceLayerEventDispatcher for FakeBindingsCtx {
    fn send_ethernet_frame(
        &mut self,
        device: &EthernetDeviceId<FakeBindingsCtx>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        let frame_meta = DispatchedFrame::Ethernet(device.downgrade());
        self.with_inner_mut(|ctx| ctx.frames.push(frame_meta, frame.into_inner()));
        Ok(())
    }

    fn send_ip_packet(
        &mut self,
        device: &PureIpDeviceId<FakeBindingsCtx>,
        packet: Buf<Vec<u8>>,
        ip_version: IpVersion,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        let frame_meta = DispatchedFrame::PureIp(PureIpDeviceAndIpVersion {
            device: device.downgrade(),
            version: ip_version,
        });
        self.with_inner_mut(|ctx| ctx.frames.push(frame_meta, packet.into_inner()));
        Ok(())
    }
}

/// Wraps all events emitted by Core into a single enum type.
#[derive(Debug, Eq, PartialEq, Hash, GenericOverIp)]
#[generic_over_ip()]
#[allow(missing_docs)]
pub enum DispatchedEvent {
    IpDeviceIpv4(IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>),
    IpDeviceIpv6(IpDeviceEvent<WeakDeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>),
    IpLayerIpv4(IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, Ipv4>),
    IpLayerIpv6(IpLayerEvent<WeakDeviceId<FakeBindingsCtx>, Ipv6>),
    NeighborIpv4(nud::Event<Mac, EthernetWeakDeviceId<FakeBindingsCtx>, Ipv4, FakeInstant>),
    NeighborIpv6(nud::Event<Mac, EthernetWeakDeviceId<FakeBindingsCtx>, Ipv6, FakeInstant>),
}

/// A tuple of device ID and IP version.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct PureIpDeviceAndIpVersion<BT: DeviceLayerTypes> {
    pub(crate) device: PureIpWeakDeviceId<BT>,
    pub(crate) version: IpVersion,
}

/// A frame that's been dispatched to Bindings to be sent out the device driver.
pub enum DispatchedFrame {
    /// A frame that's been dispatched to an Ethernet device.
    Ethernet(EthernetWeakDeviceId<FakeBindingsCtx>),
    /// A frame that's been dispatched to a PureIp device.
    PureIp(PureIpDeviceAndIpVersion<FakeBindingsCtx>),
}

impl<I: Ip> From<IpDeviceEvent<DeviceId<FakeBindingsCtx>, I, FakeInstant>> for DispatchedEvent {
    fn from(e: IpDeviceEvent<DeviceId<FakeBindingsCtx>, I, FakeInstant>) -> DispatchedEvent {
        let e = e.map_device(|d| d.downgrade());
        I::map_ip(e, |e| DispatchedEvent::IpDeviceIpv4(e), |e| DispatchedEvent::IpDeviceIpv6(e))
    }
}

impl<I: Ip> From<IpLayerEvent<DeviceId<FakeBindingsCtx>, I>> for DispatchedEvent {
    fn from(e: IpLayerEvent<DeviceId<FakeBindingsCtx>, I>) -> DispatchedEvent {
        let e = e.map_device(|d| d.downgrade());
        I::map_ip(e, |e| DispatchedEvent::IpLayerIpv4(e), |e| DispatchedEvent::IpLayerIpv6(e))
    }
}

impl<I: Ip> From<nud::Event<Mac, EthernetDeviceId<FakeBindingsCtx>, I, FakeInstant>>
    for DispatchedEvent
{
    fn from(
        e: nud::Event<Mac, EthernetDeviceId<FakeBindingsCtx>, I, FakeInstant>,
    ) -> DispatchedEvent {
        let e = e.map_device(|d| d.downgrade());
        I::map_ip(e, |e| DispatchedEvent::NeighborIpv4(e), |e| DispatchedEvent::NeighborIpv6(e))
    }
}

pub(crate) const IPV6_MIN_IMPLIED_MAX_FRAME_SIZE: MaxEthernetFrameSize =
    const_unwrap::const_unwrap_option(MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU));

impl DeviceIdAndNameMatcher for MonotonicIdentifier {
    fn id_matches(&self, _id: &NonZeroU64) -> bool {
        unimplemented!()
    }

    fn name_matches(&self, _name: &str) -> bool {
        unimplemented!()
    }
}

/// Creates a new [`FakeNetwork`] of [`Ctx`]s in a simple two-host
/// configuration.
///
/// Two hosts are created with the given names. Packets emitted by one
/// arrive at the other and vice-versa.
#[cfg(test)]
pub(crate) fn new_simple_fake_network<CtxId: Copy + Debug + Hash + Eq>(
    a_id: CtxId,
    a: FakeCtx,
    a_device_id: EthernetWeakDeviceId<FakeBindingsCtx>,
    b_id: CtxId,
    b: FakeCtx,
    b_device_id: EthernetWeakDeviceId<FakeBindingsCtx>,
) -> FakeNetwork<
    FakeCtxNetworkSpec,
    CtxId,
    impl FakeNetworkLinks<DispatchedFrame, EthernetDeviceId<FakeBindingsCtx>, CtxId>,
> {
    let contexts = vec![(a_id, a), (b_id, b)].into_iter();
    FakeNetwork::new(contexts, move |net, _frame: DispatchedFrame| {
        if net == a_id {
            b_device_id
                .upgrade()
                .map(|device_id| (b_id, device_id, None))
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            a_device_id
                .upgrade()
                .map(|device_id| (a_id, device_id, None))
                .into_iter()
                .collect::<Vec<_>>()
        }
    })
}
