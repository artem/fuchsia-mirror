// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Netstack3 bindings.
//!
//! This module provides Fuchsia bindings for the [`netstack3_core`] crate.

#![deny(clippy::redundant_clone)]

#[cfg(test)]
mod integration_tests;

mod debug_fidl_worker;
mod devices;
mod filter_worker;
mod inspect;
mod interfaces_admin;
mod interfaces_watcher;
mod neighbor_worker;
mod netdevice_worker;
mod root_fidl_worker;
mod routes;
mod routes_fidl_worker;
mod socket;
mod stack_fidl_worker;
mod timers;
mod util;
mod verifier_worker;

use std::{
    collections::HashMap,
    convert::{Infallible as Never, TryFrom as _},
    ffi::CStr,
    future::Future,
    num::NonZeroU16,
    ops::Deref,
    // TODO(https://fxbug.dev/125488): Use RC types exported from Core, after
    // we make sockets reference-backed.
    sync::Arc,
    time::Duration,
};

use fidl::endpoints::{DiscoverableProtocolMarker, RequestStream};
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{FutureExt as _, StreamExt as _};
use packet::{Buf, BufferMut};
use rand::{rngs::OsRng, CryptoRng, RngCore};
use tracing::{error, info};
use util::{ConversionContext, IntoFidl as _};

use devices::{
    BindingId, DeviceSpecificInfo, Devices, DynamicCommonInfo, DynamicNetdeviceInfo, LoopbackInfo,
    NetdeviceInfo, StaticCommonInfo,
};
use interfaces_watcher::{InterfaceEventProducer, InterfaceProperties, InterfaceUpdate};
use timers::TimerDispatcher;

use net_declare::net_subnet_v4;
use net_types::{
    ip::{AddrSubnet, AddrSubnetEither, Ip, IpAddr, IpAddress, Ipv4, Ipv4Addr, Ipv6, Mtu, Subnet},
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    context::{
        CounterContext, EventContext, InstantBindingsTypes, InstantContext, RngContext,
        TimerContext, TracingContext,
    },
    device::{
        loopback::LoopbackDeviceId, update_ipv4_configuration, update_ipv6_configuration, DeviceId,
        DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceSendFrameError, EthernetDeviceId,
    },
    error::NetstackError,
    handle_timer,
    ip::{
        device::{
            slaac::SlaacConfiguration,
            state::{Ipv6DeviceConfiguration, Lifetime},
            IpDeviceConfigurationUpdate, IpDeviceEvent, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate, RemovedReason,
        },
        icmp,
        types::RawMetric,
        IpExt,
    },
    transport::udp,
    NonSyncContext, SyncCtx, TimerId,
};

mod ctx {
    use super::*;

    /// Provides an implementation of [`NonSyncContext`].
    pub(crate) struct BindingsNonSyncCtxImpl(Arc<BindingsNonSyncCtxImplInner>);

    impl Deref for BindingsNonSyncCtxImpl {
        type Target = BindingsNonSyncCtxImplInner;

        fn deref(&self) -> &BindingsNonSyncCtxImplInner {
            let Self(this) = self;
            this.deref()
        }
    }

    pub(crate) struct Ctx {
        // `non_sync_ctx` is the first member so all strongly-held references are
        // dropped before primary references held in `sync_ctx` are dropped. Note
        // that dropping a primary reference while holding onto strong references
        // will cause a panic. See `netstack3_core::sync::PrimaryRc` for more
        // details.
        non_sync_ctx: BindingsNonSyncCtxImpl,
        sync_ctx: Arc<SyncCtx<BindingsNonSyncCtxImpl>>,
    }

    #[derive(Debug)]
    pub(crate) enum DestructionError {
        NonSyncCtxStillCloned(usize),
        SyncCtxStillCloned(usize),
    }

    impl Ctx {
        fn new(routes_change_sink: crate::bindings::routes::ChangeSink) -> Self {
            let mut non_sync_ctx = BindingsNonSyncCtxImpl(Arc::new(
                BindingsNonSyncCtxImplInner::new(routes_change_sink),
            ));
            let sync_ctx = Arc::new(SyncCtx::new(&mut non_sync_ctx));
            Self { non_sync_ctx, sync_ctx }
        }

        pub(crate) fn sync_ctx(&self) -> &Arc<SyncCtx<BindingsNonSyncCtxImpl>> {
            &self.sync_ctx
        }

        pub(crate) fn non_sync_ctx(&self) -> &BindingsNonSyncCtxImpl {
            &self.non_sync_ctx
        }

        pub(crate) fn non_sync_ctx_mut(&mut self) -> &mut BindingsNonSyncCtxImpl {
            &mut self.non_sync_ctx
        }

        pub(crate) fn contexts(
            &self,
        ) -> (&Arc<SyncCtx<BindingsNonSyncCtxImpl>>, &BindingsNonSyncCtxImpl) {
            let Ctx { non_sync_ctx, sync_ctx } = self;
            (sync_ctx, non_sync_ctx)
        }

        pub(crate) fn contexts_mut(
            &mut self,
        ) -> (&Arc<SyncCtx<BindingsNonSyncCtxImpl>>, &mut BindingsNonSyncCtxImpl) {
            let Ctx { non_sync_ctx, sync_ctx } = self;
            (sync_ctx, non_sync_ctx)
        }

        /// Destroys the last standing clone of [`Ctx`].
        pub(crate) fn try_destroy_last(self) -> Result<(), DestructionError> {
            let Self { non_sync_ctx: BindingsNonSyncCtxImpl(non_sync_ctx), sync_ctx } = self;

            fn unwrap_and_drop_or_get_count<T>(arc: Arc<T>) -> Result<(), usize> {
                match Arc::try_unwrap(arc) {
                    Ok(t) => Ok(std::mem::drop(t)),
                    Err(arc) => Err(Arc::strong_count(&arc)),
                }
            }

            // Always destroy NonSyncCtx first.
            unwrap_and_drop_or_get_count(non_sync_ctx)
                .map_err(DestructionError::NonSyncCtxStillCloned)?;
            unwrap_and_drop_or_get_count(sync_ctx).map_err(DestructionError::SyncCtxStillCloned)
        }
    }

    impl Clone for Ctx {
        fn clone(&self) -> Self {
            let Self { non_sync_ctx: BindingsNonSyncCtxImpl(inner_non_sync_ctx), sync_ctx } = self;
            Self {
                non_sync_ctx: BindingsNonSyncCtxImpl(inner_non_sync_ctx.clone()),
                sync_ctx: sync_ctx.clone(),
            }
        }
    }

    /// Contains the information needed to start serving a network stack over FIDL.
    pub(crate) struct NetstackSeed {
        pub(crate) netstack: Netstack,
        pub(crate) interfaces_worker: interfaces_watcher::Worker,
        pub(crate) interfaces_watcher_sink: interfaces_watcher::WorkerWatcherSink,
        pub(crate) routes_change_runner: routes::ChangeRunner,
    }

    impl Default for NetstackSeed {
        fn default() -> Self {
            let (interfaces_worker, interfaces_watcher_sink, interfaces_event_sink) =
                interfaces_watcher::Worker::new();
            let (routes_change_sink, routes_change_runner) = routes::create_sink_and_runner();
            Self {
                netstack: Netstack { ctx: Ctx::new(routes_change_sink), interfaces_event_sink },
                interfaces_worker,
                interfaces_watcher_sink,
                routes_change_runner,
            }
        }
    }
}

pub(crate) use ctx::{BindingsNonSyncCtxImpl, Ctx, NetstackSeed};

use crate::bindings::interfaces_watcher::AddressPropertiesUpdate;

/// Extends the methods available to [`DeviceId`].
trait DeviceIdExt {
    /// Returns the state associated with devices.
    fn external_state(&self) -> DeviceSpecificInfo<'_>;
}

impl DeviceIdExt for DeviceId<BindingsNonSyncCtxImpl> {
    fn external_state(&self) -> DeviceSpecificInfo<'_> {
        match self {
            DeviceId::Ethernet(d) => DeviceSpecificInfo::Netdevice(d.external_state()),
            DeviceId::Loopback(d) => DeviceSpecificInfo::Loopback(d.external_state()),
        }
    }
}

/// Extends the methods available to [`Lifetime`].
trait LifetimeExt {
    /// Converts `self` to `zx::Time`.
    fn into_zx_time(self) -> zx::Time;
}

impl LifetimeExt for Lifetime<StackTime> {
    fn into_zx_time(self) -> zx::Time {
        match self {
            Lifetime::Finite(StackTime(time)) => time.into_zx(),
            Lifetime::Infinite => zx::Time::INFINITE,
        }
    }
}

const LOOPBACK_NAME: &'static str = "lo";

/// Default MTU for loopback.
///
/// This value is also the default value used on Linux. As of writing:
///
/// ```shell
/// $ ip link show dev lo
/// 1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000
///     link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
/// ```
const DEFAULT_LOOPBACK_MTU: Mtu = Mtu::new(65536);

/// Subnet for the IPv4 Limited Broadcast Address.
const IPV4_LIMITED_BROADCAST_SUBNET: Subnet<Ipv4Addr> = net_subnet_v4!("255.255.255.255/32");

/// The default "Low Priority" metric to use for default routes.
///
/// The value is currently kept in sync with the Netstack2 implementation.
const DEFAULT_LOW_PRIORITY_METRIC: u32 = 99999;

/// Default routing metric for newly created interfaces, if unspecified.
///
/// The value is currently kept in sync with the Netstack2 implementation.
const DEFAULT_INTERFACE_METRIC: u32 = 100;

type UdpSockets = socket::datagram::SocketCollectionPair<socket::datagram::Udp>;

pub(crate) struct BindingsNonSyncCtxImplInner {
    timers: timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>,
    devices: Devices<DeviceId<BindingsNonSyncCtxImpl>>,
    udp_sockets: UdpSockets,
    routes: routes::ChangeSink,
}

impl BindingsNonSyncCtxImplInner {
    fn new(routes_change_sink: routes::ChangeSink) -> Self {
        Self {
            timers: Default::default(),
            devices: Default::default(),
            udp_sockets: Default::default(),
            routes: routes_change_sink,
        }
    }
}

impl AsRef<timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        &self.timers
    }
}

impl AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &Devices<DeviceId<BindingsNonSyncCtxImpl>> {
        &self.devices
    }
}

impl AsRef<UdpSockets> for BindingsNonSyncCtxImpl {
    fn as_ref(&self) -> &UdpSockets {
        &self.udp_sockets
    }
}

impl timers::TimerHandler<TimerId<BindingsNonSyncCtxImpl>> for Ctx {
    fn handle_expired_timer(&mut self, timer: TimerId<BindingsNonSyncCtxImpl>) {
        let (sync_ctx, non_sync_ctx) = self.contexts_mut();
        handle_timer(sync_ctx, non_sync_ctx, timer)
    }

    fn get_timer_dispatcher(
        &mut self,
    ) -> &timers::TimerDispatcher<TimerId<BindingsNonSyncCtxImpl>> {
        self.non_sync_ctx().as_ref()
    }
}

impl timers::TimerContext<TimerId<BindingsNonSyncCtxImpl>> for Netstack {
    type Handler = Ctx;
    fn handler(&self) -> Ctx {
        self.ctx.clone()
    }
}

impl<D> ConversionContext for D
where
    D: AsRef<Devices<DeviceId<BindingsNonSyncCtxImpl>>>,
{
    fn get_core_id(&self, binding_id: BindingId) -> Option<DeviceId<BindingsNonSyncCtxImpl>> {
        self.as_ref().get_core_id(binding_id)
    }

    fn get_binding_id(&self, core_id: DeviceId<BindingsNonSyncCtxImpl>) -> BindingId {
        core_id.external_state().static_common_info().binding_id
    }
}

/// A thin wrapper around `fuchsia_async::Time` that implements `core::Instant`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug)]
pub(crate) struct StackTime(fasync::Time);

impl netstack3_core::Instant for StackTime {
    fn duration_since(&self, earlier: StackTime) -> Duration {
        assert!(self.0 >= earlier.0);
        // guaranteed not to panic because the assertion ensures that the
        // difference is non-negative, and all non-negative i64 values are also
        // valid u64 values
        Duration::from_nanos(u64::try_from(self.0.into_nanos() - earlier.0.into_nanos()).unwrap())
    }

    fn saturating_duration_since(&self, earlier: StackTime) -> Duration {
        // Guaranteed not to panic because we are doing a saturating
        // subtraction, which means the difference will be >= 0, and all i64
        // values >=0 are also valid u64 values.
        Duration::from_nanos(
            u64::try_from(self.0.into_nanos().saturating_sub(earlier.0.into_nanos())).unwrap(),
        )
    }

    fn checked_add(&self, duration: Duration) -> Option<StackTime> {
        Some(StackTime(fasync::Time::from_nanos(
            self.0.into_nanos().checked_add(i64::try_from(duration.as_nanos()).ok()?)?,
        )))
    }

    fn checked_sub(&self, duration: Duration) -> Option<StackTime> {
        Some(StackTime(fasync::Time::from_nanos(
            self.0.into_nanos().checked_sub(i64::try_from(duration.as_nanos()).ok()?)?,
        )))
    }
}

impl InstantBindingsTypes for BindingsNonSyncCtxImpl {
    type Instant = StackTime;
}

impl InstantContext for BindingsNonSyncCtxImpl {
    fn now(&self) -> StackTime {
        StackTime(fasync::Time::now())
    }
}

impl CounterContext for BindingsNonSyncCtxImpl {}

impl TracingContext for BindingsNonSyncCtxImpl {
    type DurationScope = fuchsia_trace::DurationScope<'static>;

    fn duration(&self, name: &'static CStr) -> fuchsia_trace::DurationScope<'static> {
        fuchsia_trace::duration(cstr::cstr!("net"), name, &[])
    }
}

/// Convenience wrapper around the [`fuchsia_trace::duration`] macro that always
/// uses the "net" tracing category.
///
/// [`fuchsia_trace::duration`] uses RAII to begin and end the duration by tying
/// the scope of the duration to the lifetime of the object it returns. This
/// macro encapsulates that logic such that the trace duration will end when the
/// scope in which the macro is called ends.
macro_rules! trace_duration {
    ($name:expr) => {
        fuchsia_trace::duration!("net", $name);
    };
}

pub(crate) use trace_duration;

#[derive(Default)]
pub(crate) struct RngImpl;

/// [`RngCore`] for `RngImpl` relies entirely on the operating system to
/// generate random numbers and it needs not keep any state itself.
///
/// [`OsRng`] is a zero-sized type that provides randomness from the OS.
impl RngCore for RngImpl {
    fn next_u32(&mut self) -> u32 {
        OsRng::default().next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        OsRng::default().next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        OsRng::default().fill_bytes(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        OsRng::default().try_fill_bytes(dest)
    }
}

impl CryptoRng for RngImpl where OsRng: CryptoRng {}

impl RngContext for BindingsNonSyncCtxImpl {
    type Rng<'a> = RngImpl;

    fn rng(&mut self) -> RngImpl {
        // A change detector in case OsRng is no longer a ZST and we should keep
        // state for it inside RngImpl.
        let OsRng {} = OsRng::default();
        RngImpl {}
    }
}

impl TimerContext<TimerId<BindingsNonSyncCtxImpl>> for BindingsNonSyncCtxImpl {
    fn schedule_timer_instant(
        &mut self,
        time: StackTime,
        id: TimerId<BindingsNonSyncCtxImpl>,
    ) -> Option<StackTime> {
        self.timers.schedule_timer(id, time)
    }

    fn cancel_timer(&mut self, id: TimerId<BindingsNonSyncCtxImpl>) -> Option<StackTime> {
        self.timers.cancel_timer(&id)
    }

    fn cancel_timers_with<F: FnMut(&TimerId<BindingsNonSyncCtxImpl>) -> bool>(&mut self, f: F) {
        self.timers.cancel_timers_with(f);
    }

    fn scheduled_instant(&self, id: TimerId<BindingsNonSyncCtxImpl>) -> Option<StackTime> {
        self.timers.scheduled_time(&id)
    }
}

impl DeviceLayerStateTypes for BindingsNonSyncCtxImpl {
    type LoopbackDeviceState = LoopbackInfo;
    type EthernetDeviceState = NetdeviceInfo;
}

impl DeviceLayerEventDispatcher for BindingsNonSyncCtxImpl {
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self>) {
        let LoopbackInfo { static_common_info: _, dynamic_common_info: _, rx_notifier } =
            device.external_state();
        rx_notifier.schedule()
    }

    fn wake_tx_task(&mut self, device: &DeviceId<BindingsNonSyncCtxImpl>) {
        let external_state = device.external_state();
        let StaticCommonInfo { binding_id: _, name: _, tx_notifier } =
            external_state.static_common_info();
        tx_notifier.schedule()
    }

    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>> {
        let state = device.external_state();
        let enabled = state.with_dynamic_info(
            |DynamicNetdeviceInfo {
                 phy_up,
                 common_info:
                     DynamicCommonInfo {
                         admin_enabled,
                         mtu: _,
                         events: _,
                         control_hook: _,
                         addresses: _,
                     },
             }| { *admin_enabled && *phy_up },
        );

        if enabled {
            state.handler.send(frame.as_ref()).unwrap_or_else(|e| {
                tracing::warn!("failed to send frame to {:?}: {:?}", state.handler, e)
            })
        }

        Ok(())
    }
}

impl<I: icmp::IcmpIpExt> icmp::IcmpContext<I> for BindingsNonSyncCtxImpl {
    fn receive_icmp_error(&mut self, _conn: icmp::SocketId<I>, _seq_num: u16, _err: I::ErrorCode) {
        unimplemented!("TODO(https://fxbug.dev/125482): implement ICMP sockets")
    }
}

impl<I: icmp::IcmpIpExt, B: BufferMut> icmp::BufferIcmpContext<I, B> for BindingsNonSyncCtxImpl {
    fn receive_icmp_echo_reply(
        &mut self,
        _conn: icmp::SocketId<I>,
        _src_ip: I::Addr,
        _dst_ip: I::Addr,
        _id: u16,
        _seq_num: u16,
        _data: B,
    ) {
        unimplemented!("TODO(https://fxbug.dev/125482): implement ICMP sockets")
    }
}

impl<I> udp::NonSyncContext<I> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + icmp::IcmpIpExt,
{
    fn receive_icmp_error(&mut self, id: udp::SocketId<I>, err: I::ErrorCode) {
        I::with_collection_mut(self, |c| c.receive_icmp_error(id, err))
    }
}

impl<I, B: BufferMut> udp::BufferNonSyncContext<I, B> for BindingsNonSyncCtxImpl
where
    I: socket::datagram::SocketCollectionIpExt<socket::datagram::Udp> + IpExt,
{
    fn receive_udp(
        &mut self,
        id: udp::SocketId<I>,
        dst_addr: (<I>::Addr, NonZeroU16),
        src_addr: (<I>::Addr, Option<NonZeroU16>),
        body: &B,
    ) {
        I::with_collection_mut(self, |c| c.receive_udp(id, dst_addr, src_addr, body))
    }
}

impl<I: Ip> EventContext<IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I, StackTime>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(&mut self, event: IpDeviceEvent<DeviceId<BindingsNonSyncCtxImpl>, I, StackTime>) {
        match event {
            IpDeviceEvent::AddressAdded { device, addr, state, valid_until } => {
                let valid_until = valid_until.into_zx_time();

                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressAdded {
                        addr: addr.into(),
                        assignment_state: state,
                        valid_until,
                    },
                );
                self.notify_address_update(&device, addr.addr().into(), state);
            }
            IpDeviceEvent::AddressRemoved { device, addr, reason } => {
                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressRemoved(addr.to_ip_addr()),
                );
                match reason {
                    RemovedReason::Manual => (),
                    RemovedReason::DadFailed => self.notify_dad_failed(&device, addr.into()),
                }
            }
            IpDeviceEvent::AddressStateChanged { device, addr, state } => {
                self.notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressAssignmentStateChanged {
                        addr: addr.to_ip_addr(),
                        new_state: state,
                    },
                );
                self.notify_address_update(&device, addr.into(), state);
            }
            IpDeviceEvent::EnabledChanged { device, ip_enabled } => {
                self.notify_interface_update(&device, InterfaceUpdate::OnlineChanged(ip_enabled))
            }
            IpDeviceEvent::AddressPropertiesChanged { device, addr, valid_until } => self
                .notify_interface_update(
                    &device,
                    InterfaceUpdate::AddressPropertiesChanged {
                        addr: addr.to_ip_addr(),
                        update: AddressPropertiesUpdate { valid_until: valid_until.into_zx_time() },
                    },
                ),
        };
    }
}

impl<I: Ip> EventContext<netstack3_core::ip::IpLayerEvent<DeviceId<BindingsNonSyncCtxImpl>, I>>
    for BindingsNonSyncCtxImpl
{
    fn on_event(
        &mut self,
        event: netstack3_core::ip::IpLayerEvent<DeviceId<BindingsNonSyncCtxImpl>, I>,
    ) {
        // Core dispatched a routes-change request but has no expectation of
        // observing the result, so we just discard the result receiver.
        match event {
            netstack3_core::ip::IpLayerEvent::AddRoute(entry) => {
                self.routes.fire_change_and_forget(routes::Change::Add(
                    entry.map_device_id(|d| d.downgrade()),
                ))
            }
            netstack3_core::ip::IpLayerEvent::RemoveRoutes { subnet, device, gateway } => {
                self.routes.fire_change_and_forget(routes::Change::RemoveMatching {
                    subnet,
                    device: device.downgrade(),
                    gateway,
                })
            }
            netstack3_core::ip::IpLayerEvent::DeviceRemoved(device, routes_removed) => {
                self.routes.fire_change_and_forget(routes::Change::<I::Addr>::DeviceRemoved {
                    device: device.downgrade(),
                    routes_removed,
                });
            }
        };
    }
}

/// Implements `RcNotifier` for futures oneshot channels.
///
/// We need a newtype here because of orphan rules.
pub(crate) struct ReferenceNotifier<T>(Option<futures::channel::oneshot::Sender<T>>);

impl<T: Send> netstack3_core::sync::RcNotifier<T> for ReferenceNotifier<T> {
    fn notify(&mut self, data: T) {
        let Self(inner) = self;
        inner.take().expect("notified twice").send(data).unwrap_or_else(|_: T| {
            panic!(
                "receiver was dropped before notifying for {}",
                // Print the type name so we don't need Debug bounds.
                core::any::type_name::<T>()
            )
        })
    }
}

impl netstack3_core::ReferenceNotifiers for BindingsNonSyncCtxImpl {
    type ReferenceReceiver<T: 'static> = futures::channel::oneshot::Receiver<T>;

    type ReferenceNotifier<T: Send + 'static> = ReferenceNotifier<T>;

    fn new_reference_notifier<T: Send + 'static, D: std::fmt::Debug>(
        _debug_references: D,
    ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
        let (sender, receiver) = futures::channel::oneshot::channel();
        (ReferenceNotifier(Some(sender)), receiver)
    }
}

impl BindingsNonSyncCtxImpl {
    fn notify_interface_update(
        &self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        event: InterfaceUpdate,
    ) {
        device
            .external_state()
            .with_common_info(|i| i.events.notify(event).expect("interfaces worker closed"));
    }

    /// Notify `AddressStateProvider.WatchAddressAssignmentState` watchers.
    fn notify_address_update(
        &self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        address: SpecifiedAddr<IpAddr>,
        state: netstack3_core::ip::device::IpAddressState,
    ) {
        // Note that not all addresses have an associated watcher (e.g. loopback
        // address & autoconfigured SLAAC addresses).
        device.external_state().with_common_info(|i| {
            if let Some(address_info) = i.addresses.get(&address) {
                address_info
                    .assignment_state_sender
                    .unbounded_send(state.into_fidl())
                    .expect("assignment state receiver unexpectedly disconnected");
            }
        })
    }

    fn notify_dad_failed(
        &mut self,
        device: &DeviceId<BindingsNonSyncCtxImpl>,
        address: SpecifiedAddr<IpAddr>,
    ) {
        device.external_state().with_common_info_mut(|i| {
            if let Some(address_info) = i.addresses.get_mut(&address) {
                let devices::FidlWorkerInfo { worker: _, cancelation_sender } =
                    &mut address_info.address_state_provider;
                if let Some(sender) = cancelation_sender.take() {
                    sender
                        .send(interfaces_admin::AddressStateProviderCancellationReason::DadFailed)
                        .expect("assignment state receiver unexpectedly disconnected");
                }
            }
        })
    }

    pub(crate) async fn apply_route_change<I: Ip>(
        &self,
        change: routes::Change<I::Addr>,
    ) -> Result<(), routes::Error> {
        self.routes.send_change(change).await
    }

    pub(crate) async fn apply_route_change_either(
        &self,
        change: routes::ChangeEither,
    ) -> Result<(), routes::Error> {
        match change {
            routes::ChangeEither::V4(change) => self.apply_route_change::<Ipv4>(change).await,
            routes::ChangeEither::V6(change) => self.apply_route_change::<Ipv6>(change).await,
        }
    }
}

fn add_loopback_ip_addrs<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    non_sync_ctx: &mut NonSyncCtx,
    loopback: &DeviceId<NonSyncCtx>,
) -> Result<(), NetstackError> {
    for addr_subnet in [
        AddrSubnetEither::V4(
            AddrSubnet::from_witness(Ipv4::LOOPBACK_ADDRESS, Ipv4::LOOPBACK_SUBNET.prefix())
                .expect("error creating IPv4 loopback AddrSub"),
        ),
        AddrSubnetEither::V6(
            AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                .expect("error creating IPv6 loopback AddrSub"),
        ),
    ] {
        add_ip_addr_subnet(sync_ctx, non_sync_ctx, loopback, addr_subnet)?
    }
    Ok(())
}

/// Adds the IPv4 and IPv6 Loopback and multicast subnet routes, and the IPv4
/// limited broadcast subnet route.
async fn add_loopback_routes(
    non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    loopback: &DeviceId<BindingsNonSyncCtxImpl>,
) {
    use netstack3_core::ip::types::{AddableEntry, AddableMetric};

    let v4_changes = [
        AddableEntry::without_gateway(
            Ipv4::LOOPBACK_SUBNET,
            loopback.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
        AddableEntry::without_gateway(
            Ipv4::MULTICAST_SUBNET,
            loopback.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
        AddableEntry::without_gateway(
            IPV4_LIMITED_BROADCAST_SUBNET,
            loopback.downgrade(),
            AddableMetric::ExplicitMetric(RawMetric(DEFAULT_LOW_PRIORITY_METRIC)),
        ),
    ]
    .into_iter()
    .map(routes::Change::Add)
    .map(Into::into);

    let v6_changes = [
        AddableEntry::without_gateway(
            Ipv6::LOOPBACK_SUBNET,
            loopback.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
        AddableEntry::without_gateway(
            Ipv6::MULTICAST_SUBNET,
            loopback.downgrade(),
            AddableMetric::MetricTracksInterface,
        ),
    ]
    .into_iter()
    .map(routes::Change::Add)
    .map(Into::into);

    for change in v4_changes.chain(v6_changes) {
        non_sync_ctx
            .apply_route_change_either(change)
            .await
            .expect("adding loopback routes should succeed");
    }
}

/// The netstack.
///
/// Provides the entry point for creating a netstack to be served as a
/// component.
#[derive(Clone)]
pub(crate) struct Netstack {
    ctx: Ctx,
    interfaces_event_sink: interfaces_watcher::WorkerInterfaceSink,
}

fn create_interface_event_producer(
    interfaces_event_sink: &crate::bindings::interfaces_watcher::WorkerInterfaceSink,
    id: BindingId,
    properties: InterfaceProperties,
) -> InterfaceEventProducer {
    interfaces_event_sink.add_interface(id, properties).expect("interface worker not running")
}

impl Netstack {
    fn create_interface_event_producer(
        &self,
        id: BindingId,
        properties: InterfaceProperties,
    ) -> InterfaceEventProducer {
        create_interface_event_producer(&self.interfaces_event_sink, id, properties)
    }

    async fn add_loopback(
        &mut self,
    ) -> (
        futures::channel::oneshot::Sender<fnet_interfaces_admin::InterfaceRemovedReason>,
        BindingId,
        [NamedTask; 3],
    ) {
        // Add and initialize the loopback interface with the IPv4 and IPv6
        // loopback addresses and on-link routes to the loopback subnets.
        let devices: &Devices<_> = self.ctx.non_sync_ctx().as_ref();
        let (control_sender, control_receiver) =
            interfaces_admin::OwnedControlHandle::new_channel();
        let loopback_rx_notifier = Default::default();

        let loopback = netstack3_core::device::add_loopback_device_with_state(
            self.ctx.sync_ctx(),
            DEFAULT_LOOPBACK_MTU,
            RawMetric(DEFAULT_INTERFACE_METRIC),
            || {
                let binding_id = devices.alloc_new_id();

                let events = self.create_interface_event_producer(
                    binding_id,
                    InterfaceProperties {
                        name: LOOPBACK_NAME.to_string(),
                        device_class: fidl_fuchsia_net_interfaces::DeviceClass::Loopback(
                            fidl_fuchsia_net_interfaces::Empty {},
                        ),
                    },
                );
                events
                    .notify(InterfaceUpdate::OnlineChanged(true))
                    .expect("interfaces worker not running");

                LoopbackInfo {
                    static_common_info: StaticCommonInfo {
                        binding_id,
                        name: LOOPBACK_NAME.to_string(),
                        tx_notifier: Default::default(),
                    },
                    dynamic_common_info: DynamicCommonInfo {
                        mtu: DEFAULT_LOOPBACK_MTU,
                        admin_enabled: true,
                        events,
                        control_hook: control_sender,
                        addresses: HashMap::new(),
                    }
                    .into(),
                    rx_notifier: loopback_rx_notifier,
                }
            },
        )
        .expect("error adding loopback device");

        let LoopbackInfo { static_common_info: _, dynamic_common_info: _, rx_notifier } =
            loopback.external_state();
        let rx_task =
            crate::bindings::devices::spawn_rx_task(rx_notifier, self.ctx.clone(), &loopback);
        let loopback: DeviceId<_> = loopback.into();
        let external_state = loopback.external_state();
        let StaticCommonInfo { binding_id, name: _, tx_notifier } =
            external_state.static_common_info();
        let binding_id = *binding_id;
        devices.add_device(binding_id, loopback.clone());
        let tx_task = crate::bindings::devices::spawn_tx_task(
            tx_notifier,
            self.ctx.clone(),
            loopback.clone(),
        );

        // Don't need DAD and IGMP/MLD on loopback.
        let ip_config = Some(IpDeviceConfigurationUpdate {
            ip_enabled: Some(true),
            forwarding_enabled: Some(false),
            gmp_enabled: Some(false),
        });

        let (sync_ctx, non_sync_ctx) = self.ctx.contexts_mut();
        let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            Ipv4DeviceConfigurationUpdate { ip_config },
        )
        .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            non_sync_ctx,
            &loopback,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(None),
                max_router_solicitations: Some(None),
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    temporary_address_configuration: None,
                }),
                ip_config,
            },
        )
        .unwrap();
        add_loopback_ip_addrs(sync_ctx, non_sync_ctx, &loopback)
            .expect("error adding loopback addresses");
        add_loopback_routes(non_sync_ctx, &loopback).await;

        let (stop_sender, stop_receiver) = futures::channel::oneshot::channel();

        // Loopback interface can't be removed.
        let removable = false;
        // Loopback doesn't have a defined state stream, provide a stream that
        // never yields anything.
        let state_stream = futures::stream::pending();
        let control_task = fuchsia_async::Task::spawn(interfaces_admin::run_interface_control(
            self.ctx.clone(),
            binding_id,
            stop_receiver,
            control_receiver,
            removable,
            state_stream,
        ));
        (
            stop_sender,
            binding_id,
            [
                NamedTask::new("loopback control", control_task),
                NamedTask::new("loopback tx", tx_task),
                NamedTask::new("loopback rx", rx_task),
            ],
        )
    }
}

pub(crate) enum Service {
    DebugDiagnostics(fidl::endpoints::ServerEnd<fidl_fuchsia_net_debug::DiagnosticsMarker>),
    DebugInterfaces(fidl_fuchsia_net_debug::InterfacesRequestStream),
    Filter(fidl_fuchsia_net_filter::FilterRequestStream),
    Interfaces(fidl_fuchsia_net_interfaces::StateRequestStream),
    InterfacesAdmin(fidl_fuchsia_net_interfaces_admin::InstallerRequestStream),
    Neighbor(fidl_fuchsia_net_neighbor::ViewRequestStream),
    PacketSocket(fidl_fuchsia_posix_socket_packet::ProviderRequestStream),
    RawSocket(fidl_fuchsia_posix_socket_raw::ProviderRequestStream),
    RootInterfaces(fidl_fuchsia_net_root::InterfacesRequestStream),
    RoutesState(fidl_fuchsia_net_routes::StateRequestStream),
    RoutesStateV4(fidl_fuchsia_net_routes::StateV4RequestStream),
    RoutesStateV6(fidl_fuchsia_net_routes::StateV6RequestStream),
    Socket(fidl_fuchsia_posix_socket::ProviderRequestStream),
    Stack(fidl_fuchsia_net_stack::StackRequestStream),
    Verifier(fidl_fuchsia_update_verify::NetstackVerifierRequestStream),
}

trait RequestStreamExt: RequestStream {
    fn serve_with<F, Fut, E>(self, f: F) -> futures::future::Map<Fut, fn(Result<(), E>) -> ()>
    where
        E: std::error::Error,
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<(), E>>;
}

impl<D: DiscoverableProtocolMarker, S: RequestStream<Protocol = D>> RequestStreamExt for S {
    fn serve_with<F, Fut, E>(self, f: F) -> futures::future::Map<Fut, fn(Result<(), E>) -> ()>
    where
        E: std::error::Error,
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        f(self).map(|res| res.unwrap_or_else(|err| error!("{} error: {}", D::PROTOCOL_NAME, err)))
    }
}

/// A helper struct to have named tasks.
///
/// Tasks are already tracked in the executor by spawn location, but long
/// running tasks are not expected to terminate except during clean shutdown.
/// Naming these helps root cause debug assertions.
#[derive(Debug)]
pub(crate) struct NamedTask {
    name: &'static str,
    task: fuchsia_async::Task<()>,
}

impl NamedTask {
    /// Creates a new named task from `fut` with `name`.
    #[track_caller]
    fn spawn(name: &'static str, fut: impl futures::Future<Output = ()> + Send + 'static) -> Self {
        Self { name, task: fuchsia_async::Task::spawn(fut) }
    }

    fn new(name: &'static str, task: fuchsia_async::Task<()>) -> Self {
        Self { name, task }
    }

    fn into_future(self) -> impl futures::Future<Output = &'static str> + Send + 'static {
        let Self { name, task } = self;
        task.map(move |()| name)
    }
}

impl NetstackSeed {
    /// Consumes the netstack and starts serving all the FIDL services it
    /// implements to the outgoing service directory.
    pub(crate) async fn serve<S: futures::Stream<Item = Service>>(
        self,
        services: S,
        inspector: &fuchsia_inspect::Inspector,
    ) {
        info!("serving netstack with netstack3");

        let Self {
            mut netstack,
            interfaces_worker,
            interfaces_watcher_sink,
            mut routes_change_runner,
        } = self;

        // Start servicing timers.
        let timers_task =
            NamedTask::new("timers", netstack.ctx.non_sync_ctx().timers.spawn(netstack.clone()));

        let (route_update_dispatcher_v4, route_update_dispatcher_v6) =
            routes_change_runner.route_update_dispatchers();

        // Start executing routes changes.
        let routes_change_task = NamedTask::spawn("routes_changes", {
            let ctx = netstack.ctx.clone();
            async move { routes_change_runner.run(ctx).await }
        });

        let (loopback_stopper, _, loopback_tasks): (
            futures::channel::oneshot::Sender<_>,
            BindingId,
            _,
        ) = netstack.add_loopback().await;

        let interfaces_worker_task = NamedTask::spawn("interfaces worker", async move {
            let result = interfaces_worker.run().await;
            let watchers = result.expect("interfaces worker ended with an error");
            info!("interfaces worker shutting down, waiting for watchers to end");
            watchers
                .map(|res| match res {
                    Ok(()) => (),
                    Err(e) => {
                        if !e.is_closed() {
                            tracing::error!("error {e:?} collecting watchers");
                        }
                    }
                })
                .collect::<()>()
                .await;
            info!("all interface watchers closed, interfaces worker shutdown is complete");
        });

        let no_finish_tasks = loopback_tasks.into_iter().chain([
            interfaces_worker_task,
            timers_task,
            routes_change_task,
        ]);
        let no_finish_tasks = no_finish_tasks.map(NamedTask::into_future);
        let mut no_finish_tasks = futures::stream::FuturesUnordered::from_iter(no_finish_tasks);
        let mut no_finish_tasks_fut = no_finish_tasks
            .by_ref()
            .into_future()
            .map(|(name, _): (_, &mut futures::stream::FuturesUnordered<_>)| -> Never {
                match name {
                    Some(name) => panic!("task {name} ended unexpectedly"),
                    None => panic!("unexpected end of infinite task stream"),
                }
            })
            .fuse();

        let inspect_nodes = {
            let socket_ctx = netstack.ctx.clone();
            let sockets = inspector.root().create_lazy_child("Sockets", move || {
                futures::future::ok(inspect::sockets(&mut socket_ctx.clone())).boxed()
            });
            let routes_ctx = netstack.ctx.clone();
            let routes = inspector.root().create_lazy_child("Routes", move || {
                futures::future::ok(inspect::routes(&mut routes_ctx.clone())).boxed()
            });
            let devices_ctx = netstack.ctx.clone();
            let devices = inspector.root().create_lazy_child("Devices", move || {
                futures::future::ok(inspect::devices(&devices_ctx)).boxed()
            });
            let neighbors_ctx = netstack.ctx.clone();
            let neighbors = inspector.root().create_lazy_child("Neighbors", move || {
                futures::future::ok(inspect::neighbors(&neighbors_ctx)).boxed()
            });
            (sockets, routes, devices, neighbors)
        };

        let diagnostics_handler = debug_fidl_worker::DiagnosticsHandler::default();

        // Insert a stream after services to get a helpful log line when it
        // completes. The future we create from it will still wait for all the
        // user-created resources to be joined on before returning.
        let services =
            services.chain(futures::stream::poll_fn(|_: &mut std::task::Context<'_>| {
                info!("services stream ended");
                std::task::Poll::Ready(None)
            }));

        // Keep a clone of Ctx around for teardown before moving it to the
        // services future.
        let teardown_ctx = netstack.ctx.clone();

        // Use a reference to the watcher sink in the services loop.
        let interfaces_watcher_sink_ref = &interfaces_watcher_sink;

        // It is unclear why we need to wrap the `for_each_concurrent` call with
        // `async move { ... }` but it seems like we do. Without this, the
        // `Future` returned by this function fails to implement `Send` with the
        // same issue reported in https://github.com/rust-lang/rust/issues/64552.
        //
        // TODO(https://github.com/rust-lang/rust/issues/64552): Remove this
        // workaround.
        let services_fut = async move {
            services
                .for_each_concurrent(None, |s| async {
                    match s {
                        Service::Stack(stack) => {
                            stack
                                .serve_with(|rs| {
                                    stack_fidl_worker::StackFidlWorker::serve(netstack.clone(), rs)
                                })
                                .await
                        }
                        Service::Socket(socket) => {
                            // Run on a separate task so socket requests are not
                            // bound to the same thread as the main services
                            // loop.
                            let wait_group = fuchsia_async::Task::spawn(socket::serve(
                                netstack.ctx.clone(),
                                socket,
                            ))
                            .await;
                            // Wait for all socket tasks to finish.
                            wait_group.await;
                        }
                        Service::PacketSocket(socket) => {
                            // Run on a separate task so socket requests are not
                            // bound to the same thread as the main services
                            // loop.
                            let wait_group = fuchsia_async::Task::spawn(socket::packet::serve(
                                netstack.ctx.clone(),
                                socket,
                            ))
                            .await;
                            // Wait for all socket tasks to finish.
                            wait_group.await;
                        }
                        Service::RawSocket(socket) => {
                            socket.serve_with(|rs| socket::raw::serve(rs)).await
                        }
                        Service::RootInterfaces(root_interfaces) => {
                            root_interfaces
                                .serve_with(|rs| {
                                    root_fidl_worker::serve_interfaces(netstack.clone(), rs)
                                })
                                .await
                        }
                        Service::RoutesState(rs) => {
                            routes_fidl_worker::serve_state(rs, netstack.ctx.clone()).await
                        }
                        Service::RoutesStateV4(rs) => {
                            routes_fidl_worker::serve_state_v4(rs, &route_update_dispatcher_v4)
                                .await
                        }
                        Service::RoutesStateV6(rs) => {
                            routes_fidl_worker::serve_state_v6(rs, &route_update_dispatcher_v6)
                                .await
                        }
                        Service::Interfaces(interfaces) => {
                            interfaces
                                .serve_with(|rs| {
                                    interfaces_watcher::serve(
                                        rs,
                                        interfaces_watcher_sink_ref.clone(),
                                    )
                                })
                                .await
                        }
                        Service::InterfacesAdmin(installer) => {
                            tracing::debug!(
                                "serving {}",
                                fidl_fuchsia_net_interfaces_admin::InstallerMarker::PROTOCOL_NAME
                            );
                            interfaces_admin::serve(netstack.clone(), installer).await;
                        }
                        Service::DebugInterfaces(debug_interfaces) => {
                            debug_interfaces
                                .serve_with(|rs| {
                                    debug_fidl_worker::serve_interfaces(
                                        netstack.ctx.non_sync_ctx(),
                                        rs,
                                    )
                                })
                                .await
                        }
                        Service::DebugDiagnostics(debug_diagnostics) => {
                            diagnostics_handler.serve_diagnostics(debug_diagnostics).await
                        }
                        Service::Filter(filter) => {
                            filter.serve_with(|rs| filter_worker::serve(rs)).await
                        }
                        Service::Neighbor(neighbor) => {
                            neighbor
                                .serve_with(|rs| neighbor_worker::serve(netstack.clone(), rs))
                                .await
                        }
                        Service::Verifier(verifier) => {
                            verifier.serve_with(|rs| verifier_worker::serve(rs)).await
                        }
                    }
                })
                .await
        };

        {
            let services_fut = services_fut.fuse();
            // Pin services_fut to this block scope so it's dropped after the
            // select.
            futures::pin_mut!(services_fut);
            let () = futures::select! {
                () = services_fut => (),
                never = no_finish_tasks_fut => match never {}
            };
        }

        info!("all services terminated, starting shutdown");
        let ctx = teardown_ctx;
        // Stop the loopback interface.
        loopback_stopper
            .send(fnet_interfaces_admin::InterfaceRemovedReason::PortClosed)
            .expect("loopback task must still be running");
        // Stop the timer dispatcher.
        ctx.non_sync_ctx().timers.stop();
        // Stop the interfaces watcher worker.
        std::mem::drop(interfaces_watcher_sink);
        // Stop the routes change runner.
        ctx.non_sync_ctx().routes.close_senders();

        // We've signalled all long running tasks, now we can collect them.
        no_finish_tasks.map(|name| info!("{name} finished")).collect::<()>().await;

        // Drop all inspector data, it holds ctx clones.
        std::mem::drop(inspect_nodes);
        inspector.root().clear_recorded();

        // Last thing to happen is dropping the context.
        ctx.try_destroy_last().expect("all Ctx references must have been dropped")
    }
}
