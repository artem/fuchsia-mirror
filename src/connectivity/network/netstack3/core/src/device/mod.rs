// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The device layer.

pub(crate) mod arp;
pub mod ethernet;
pub mod id;
pub(crate) mod integration;
pub(crate) mod link;
pub mod loopback;
pub mod ndp;
pub mod queue;
pub mod socket;
mod state;

use alloc::{collections::HashMap, vec::Vec};
use core::{
    convert::Infallible as Never,
    fmt::{Debug, Display},
    marker::PhantomData,
};

use derivative::Derivative;
use lock_order::{lock::UnlockedAccess, Locked};
use net_types::{
    ethernet::Mac,
    ip::{Ip, IpAddr, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu},
    BroadcastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{Buf, BufferMut};
use smallvec::SmallVec;
use tracing::{debug, trace};

use crate::{
    context::{CounterContext, InstantBindingsTypes, InstantContext, RecvFrameContext},
    counters::Counter,
    device::{
        ethernet::{
            EthernetDeviceStateBuilder, EthernetIpLinkDeviceDynamicStateContext,
            EthernetLinkDevice, EthernetTimerId,
        },
        loopback::{
            LoopbackDevice, LoopbackDeviceId, LoopbackDeviceState, LoopbackPrimaryDeviceId,
        },
        queue::{
            rx::ReceiveQueueHandler,
            tx::{BufferTransmitQueueHandler, TransmitQueueConfiguration, TransmitQueueHandler},
        },
        socket::HeldSockets,
        state::{BaseDeviceState, DeviceStateSpec, IpLinkDeviceState, IpLinkDeviceStateInner},
    },
    error::{
        ExistsError, NotFoundError, NotSupportedError, SetIpAddressPropertiesError,
        StaticNeighborInsertionError,
    },
    ip::{
        device::{
            nud::{LinkResolutionContext, NeighborStateInspect},
            state::{
                AddrSubnetAndManualConfigEither, AssignedAddress as _, IpDeviceFlags,
                Ipv4DeviceConfigurationAndFlags, Ipv6DeviceConfigurationAndFlags, Lifetime,
            },
            DelIpv6Addr, IpDeviceIpExt, IpDeviceStateContext, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationUpdate,
        },
        forwarding::IpForwardingDeviceContext,
        types::RawMetric,
    },
    sync::{PrimaryRc, RwLock},
    trace_duration, BufferNonSyncContext, Instant, NonSyncContext, SyncCtx,
};

pub use id::*;

/// A device.
///
/// `Device` is used to identify a particular device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub trait Device: 'static {}

/// Marker type for a generic device.
pub(crate) enum AnyDevice {}

impl Device for AnyDevice {}

/// An execution context which provides device ID types type for various
/// netstack internals to share.
pub(crate) trait DeviceIdContext<D: Device> {
    /// The type of device IDs.
    type DeviceId: StrongId<Weak = Self::WeakDeviceId> + 'static;

    /// The type of weakly referenced device IDs.
    type WeakDeviceId: WeakId<Strong = Self::DeviceId> + 'static;

    /// Returns a weak ID for the strong ID.
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId;

    /// Attempts to upgrade the weak device ID to a strong ID.
    ///
    /// Returns `None` if the device has been removed.
    fn upgrade_weak_device_id(&self, weak_device_id: &Self::WeakDeviceId)
        -> Option<Self::DeviceId>;
}

struct RecvIpFrameMeta<D, I: Ip> {
    device: D,
    frame_dst: FrameDestination,
    _marker: PhantomData<I>,
}

impl<D, I: Ip> RecvIpFrameMeta<D, I> {
    fn new(device: D, frame_dst: FrameDestination) -> RecvIpFrameMeta<D, I> {
        RecvIpFrameMeta { device, frame_dst, _marker: PhantomData }
    }
}

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub(crate) struct DevicesIter<'s, C: NonSyncContext> {
    ethernet:
        alloc::collections::hash_map::Values<'s, EthernetDeviceId<C>, EthernetPrimaryDeviceId<C>>,
    loopback: core::option::Iter<'s, LoopbackPrimaryDeviceId<C>>,
}

impl<'s, C: NonSyncContext> Iterator for DevicesIter<'s, C> {
    type Item = DeviceId<C>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self { ethernet, loopback } = self;
        ethernet
            .map(|primary| primary.clone_strong().into())
            .chain(loopback.map(|primary| primary.clone_strong().into()))
            .next()
    }
}

impl<I: IpDeviceIpExt, NonSyncCtx: NonSyncContext, L> IpForwardingDeviceContext<I>
    for Locked<&SyncCtx<NonSyncCtx>, L>
where
    Self: IpDeviceStateContext<I, NonSyncCtx, DeviceId = DeviceId<NonSyncCtx>>,
{
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric {
        match device_id {
            DeviceId::Ethernet(id) => self::ethernet::get_routing_metric(self, id),
            DeviceId::Loopback(id) => self::loopback::get_routing_metric(self, id),
        }
    }

    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        IpDeviceStateContext::<I, _>::with_ip_device_flags(
            self,
            device_id,
            |IpDeviceFlags { ip_enabled }| *ip_enabled,
        )
    }
}

pub(crate) fn get_routing_metric<NonSyncCtx: NonSyncContext, L>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &DeviceId<NonSyncCtx>,
) -> RawMetric {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::get_routing_metric(sync_ctx, id),
        DeviceId::Loopback(id) => self::loopback::get_routing_metric(sync_ctx, id),
    }
}

/// Visitor for NUD state.
pub trait NeighborVisitor<C: NonSyncContext, T: Instant> {
    /// Performs a user-defined operation over an iterator of neighbor state
    /// describing the neighbors associated with a given `device`.
    ///
    /// This function will be called N times, where N is the number of devices
    /// in the stack.
    fn visit_neighbors<LinkAddress: Debug>(
        &self,
        device: DeviceId<C>,
        neighbors: impl Iterator<Item = NeighborStateInspect<LinkAddress, T>>,
    );
}

/// Creates a snapshot of the devices in the stack at the time of invocation.
///
/// Devices are copied into the return value.
///
/// The argument `filter_map` defines a filtering function, so that unneeded
/// devices are not copied and returned in the snapshot.
pub(crate) fn snapshot_device_ids<T, C: NonSyncContext, F: FnMut(DeviceId<C>) -> Option<T>>(
    sync_ctx: &SyncCtx<C>,
    filter_map: F,
) -> impl IntoIterator<Item = T> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let devices = sync_ctx.read_lock::<crate::lock_ordering::DeviceLayerState>();
    let Devices { ethernet, loopback } = &*devices;
    DevicesIter { ethernet: ethernet.values(), loopback: loopback.iter() }
        .filter_map(filter_map)
        .collect::<SmallVec<[T; 32]>>()
}

/// Provides access to NUD state via a `visitor`.
pub fn inspect_neighbors<C, V>(sync_ctx: &SyncCtx<C>, visitor: &V)
where
    C: NonSyncContext,
    V: NeighborVisitor<C, <C as InstantBindingsTypes>::Instant>,
{
    let device_ids = snapshot_device_ids(sync_ctx, |device| match device {
        DeviceId::Ethernet(d) => Some(d),
        // Loopback devices do not have neighbors.
        DeviceId::Loopback(_) => None,
    });
    let mut sync_ctx = Locked::new(sync_ctx);
    for device in device_ids {
        let id = device.clone();
        integration::with_ethernet_state(&mut sync_ctx, &id, |mut device_state| {
            let (arp, mut device_state) =
                device_state.lock_and::<crate::lock_ordering::EthernetIpv4Arp>();
            let nud = device_state.lock::<crate::lock_ordering::EthernetIpv6Nud>();
            visitor.visit_neighbors(
                DeviceId::from(device),
                arp.nud.state_iter().chain(nud.state_iter()),
            );
        })
    }
}

/// Visitor for Device state.
pub trait DevicesVisitor<C: NonSyncContext> {
    /// Performs a user-defined operation over an iterator of device state.
    fn visit_devices(&self, devices: impl Iterator<Item = InspectDeviceState<C>>);
}

/// The state of a Device, for exporting to Inspect.
pub struct InspectDeviceState<C: NonSyncContext> {
    /// A strong ID identifying a Device.
    pub device_id: DeviceId<C>,

    /// The IP addresses assigned to a Device by core.
    pub addresses: SmallVec<[IpAddr; 32]>,
}

/// Provides access to Device state via a `visitor`.
pub fn inspect_devices<C: NonSyncContext, V: DevicesVisitor<C>>(
    sync_ctx: &SyncCtx<C>,
    visitor: &V,
) {
    let devices = snapshot_device_ids(sync_ctx, Some).into_iter().map(|device| {
        let device_id = device.clone();
        let ip = match &device {
            DeviceId::Ethernet(d) => &d.device_state().ip,
            DeviceId::Loopback(d) => &d.device_state().ip,
        };
        let ipv4 =
            lock_order::lock::RwLockFor::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>::read_lock(
                ip,
            );
        let ipv4_addresses = ipv4.iter().map(|a| IpAddr::from(a.addr().into_addr()));
        let ipv6 =
            lock_order::lock::RwLockFor::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>::read_lock(
                ip,
            );
        let ipv6_addresses = ipv6.iter().map(|a| IpAddr::from(a.addr().into_addr()));
        InspectDeviceState { device_id, addresses: ipv4_addresses.chain(ipv6_addresses).collect() }
    });
    visitor.visit_devices(devices)
}

pub(crate) enum Ipv6DeviceLinkLayerAddr {
    Mac(Mac),
    // Add other link-layer address types as needed.
}

impl AsRef<[u8]> for Ipv6DeviceLinkLayerAddr {
    fn as_ref(&self) -> &[u8] {
        match self {
            Ipv6DeviceLinkLayerAddr::Mac(a) => a.as_ref(),
        }
    }
}

/// The identifier for timer events in the device layer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) struct DeviceLayerTimerId<C: DeviceLayerTypes>(DeviceLayerTimerIdInner<C>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
enum DeviceLayerTimerIdInner<C: DeviceLayerTypes> {
    /// A timer event for an Ethernet device.
    Ethernet(EthernetTimerId<EthernetDeviceId<C>>),
}

impl<C: DeviceLayerTypes> From<EthernetTimerId<EthernetDeviceId<C>>> for DeviceLayerTimerId<C> {
    fn from(id: EthernetTimerId<EthernetDeviceId<C>>) -> DeviceLayerTimerId<C> {
        DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id))
    }
}

impl_timer_context!(
    C: NonSyncContext,
    DeviceLayerTimerId<C>,
    EthernetTimerId<EthernetDeviceId<C>>,
    DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id)),
    id
);

/// Handle a timer event firing in the device layer.
pub(crate) fn handle_timer<NonSyncCtx: NonSyncContext>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, crate::lock_ordering::Unlocked>,
    ctx: &mut NonSyncCtx,
    DeviceLayerTimerId(id): DeviceLayerTimerId<NonSyncCtx>,
) {
    match id {
        DeviceLayerTimerIdInner::Ethernet(id) => ethernet::handle_timer(sync_ctx, ctx, id),
    }
}

// TODO(joshlf): Does the IP layer ever need to distinguish between broadcast
// and multicast frames?

/// The type of address used as the source address in a device-layer frame:
/// unicast or broadcast.
///
/// `FrameDestination` is used to implement RFC 1122 section 3.2.2 and RFC 4443
/// section 2.4.e, which govern when to avoid sending an ICMP error message for
/// ICMP and ICMPv6 respectively.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrameDestination {
    /// A unicast address - one which is neither multicast nor broadcast.
    Individual {
        /// Whether the frame's destination address belongs to the receiver.
        local: bool,
    },
    /// A multicast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Multicast,
    /// A broadcast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Broadcast,
}

impl FrameDestination {
    /// Is this `FrameDestination::Multicast`?
    pub(crate) fn is_multicast(self) -> bool {
        self == FrameDestination::Multicast
    }

    /// Is this `FrameDestination::Broadcast`?
    pub(crate) fn is_broadcast(self) -> bool {
        self == FrameDestination::Broadcast
    }

    pub(crate) fn from_dest(destination: Mac, local_mac: Mac) -> Self {
        BroadcastAddr::new(destination)
            .map(Into::into)
            .or_else(|| MulticastAddr::new(destination).map(Into::into))
            .unwrap_or_else(|| FrameDestination::Individual { local: destination == local_mac })
    }
}

impl From<BroadcastAddr<Mac>> for FrameDestination {
    fn from(_value: BroadcastAddr<Mac>) -> Self {
        Self::Broadcast
    }
}

impl From<MulticastAddr<Mac>> for FrameDestination {
    fn from(_value: MulticastAddr<Mac>) -> Self {
        Self::Multicast
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Devices<C: DeviceLayerTypes> {
    ethernet: HashMap<EthernetDeviceId<C>, EthernetPrimaryDeviceId<C>>,
    loopback: Option<LoopbackPrimaryDeviceId<C>>,
}

/// The state associated with the device layer.
pub(crate) struct DeviceLayerState<C: DeviceLayerTypes> {
    devices: RwLock<Devices<C>>,
    origin: OriginTracker,
    shared_sockets: HeldSockets<C>,
    counters: DeviceCounters,
}

impl<C: DeviceLayerTypes> DeviceLayerState<C> {
    pub(crate) fn get_device_counters(&self) -> &DeviceCounters {
        &self.counters
    }
}

/// Device layer counters.
#[derive(Default)]
pub(crate) struct DeviceCounters {
    /// Count of ip packets sent inside an ethernet frame.
    pub(crate) ethernet_send_ip_frame: Counter,
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::DeviceCounters> for SyncCtx<C> {
    type Data = DeviceCounters;
    type Guard<'l> = &'l DeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.state.get_device_counters()
    }
}

impl<NonSyncCtx: NonSyncContext, L> CounterContext<DeviceCounters>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_counters<O, F: FnOnce(&DeviceCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::DeviceCounters>())
    }
}

/// Light-weight tracker for recording the source of some instance.
///
/// This should be held as a field in a parent type that is cloned into each
/// child instance. Then, the origin of a child instance can be verified by
/// asserting equality against the parent's field.
///
/// This is only enabled in debug builds; in non-debug builds, all
/// `OriginTracker` instances are identical so all operations are no-ops.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OriginTracker(#[cfg(debug_assertions)] u64);

impl OriginTracker {
    /// Creates a new `OriginTracker` that isn't derived from any other
    /// instance.
    ///
    /// In debug builds, this creates a unique `OriginTracker` that won't be
    /// equal to any instances except those cloned from it. In non-debug builds
    /// all `OriginTracker` instances are identical.
    #[cfg_attr(not(debug_assertions), inline)]
    fn new() -> Self {
        Self(
            #[cfg(debug_assertions)]
            {
                static COUNTER: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(0);
                COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
            },
        )
    }
}

impl<C: DeviceLayerTypes + socket::NonSyncContext<DeviceId<C>>> DeviceLayerState<C> {
    /// Creates a new [`DeviceLayerState`] instance.
    pub(crate) fn new() -> Self {
        Self {
            devices: Default::default(),
            origin: OriginTracker::new(),
            shared_sockets: Default::default(),
            counters: Default::default(),
        }
    }

    /// Add a new ethernet device to the device layer.
    ///
    /// `add` adds a new `EthernetDeviceState` with the given MAC address and
    /// maximum frame size. The frame size is the limit on the size of the data
    /// payload and the header but not the FCS.
    pub(crate) fn add_ethernet_device<
        F: FnOnce() -> (C::EthernetDeviceState, C::DeviceIdentifier),
    >(
        &self,
        mac: UnicastAddr<Mac>,
        max_frame_size: ethernet::MaxFrameSize,
        metric: RawMetric,
        bindings_state: F,
    ) -> EthernetDeviceId<C> {
        let Devices { ethernet, loopback: _ } = &mut *self.devices.write();

        let (external_state, bindings_id) = bindings_state();
        let primary = EthernetPrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(
                EthernetDeviceStateBuilder::new(mac, max_frame_size, metric).build(),
                self.origin.clone(),
            ),
            external_state,
            bindings_id,
        );
        let id = primary.clone_strong();

        assert!(ethernet.insert(id.clone(), primary).is_none());
        debug!("adding Ethernet device {:?} with MTU {:?}", id, max_frame_size);
        id
    }

    /// Adds a new loopback device to the device layer.
    pub(crate) fn add_loopback_device<
        F: FnOnce() -> (C::LoopbackDeviceState, C::DeviceIdentifier),
    >(
        &self,
        mtu: Mtu,
        metric: RawMetric,
        bindings_state: F,
    ) -> Result<LoopbackDeviceId<C>, ExistsError> {
        let Devices { ethernet: _, loopback } = &mut *self.devices.write();

        if let Some(_) = loopback {
            return Err(ExistsError);
        }

        let (external_state, bindings_id) = bindings_state();
        let primary = LoopbackPrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(LoopbackDeviceState::new(mtu, metric), self.origin.clone()),
            external_state,
            bindings_id,
        );

        let id = primary.clone_strong();

        *loopback = Some(primary);

        debug!("added loopback device");

        Ok(id)
    }
}

/// Provides associated types used in the device layer.
pub trait DeviceLayerStateTypes: InstantContext {
    /// The state associated with loopback devices.
    type LoopbackDeviceState: Send + Sync;

    /// The state associated with ethernet devices.
    type EthernetDeviceState: Send + Sync;

    /// An opaque identifier that is available from both strong and weak device
    /// references.
    type DeviceIdentifier: Send + Sync + Debug + Display;
}

/// Provides associated types used in the device layer.
///
/// This trait groups together state types used throughout the device layer. It
/// is blanket-implemented for all types that implement
/// [`socket::DeviceSocketTypes`] and [`DeviceLayerStateTypes`].
pub trait DeviceLayerTypes:
    DeviceLayerStateTypes
    + socket::DeviceSocketTypes
    + LinkResolutionContext<EthernetLinkDevice>
    + 'static
{
}
impl<
        C: DeviceLayerStateTypes
            + socket::DeviceSocketTypes
            + LinkResolutionContext<EthernetLinkDevice>
            + 'static,
    > DeviceLayerTypes for C
{
}

/// An event dispatcher for the device layer.
///
/// See the `EventDispatcher` trait in the crate root for more details.
pub trait DeviceLayerEventDispatcher: DeviceLayerTypes + Sized {
    /// Signals to the dispatcher that RX frames are available and ready to be
    /// handled by [`handle_queued_rx_packets`].
    ///
    /// Implementations must make sure that [`handle_queued_rx_packets`] is
    /// scheduled to be called as soon as possible so that enqueued RX frames
    /// are promptly handled.
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self>);

    /// Signals to the dispatcher that TX frames are available and ready to be
    /// sent by [`transmit_queued_tx_frames`].
    ///
    /// Implementations must make sure that [`transmit_queued_tx_frames`] is
    /// scheduled to be called as soon as possible so that enqueued TX frames
    /// are promptly sent.
    fn wake_tx_task(&mut self, device: &DeviceId<Self>);

    /// Send a frame to a device driver.
    ///
    /// If there was an MTU error while attempting to serialize the frame, the
    /// original buffer is returned in the `Err` variant. All other errors (for
    /// example, errors in allocating a buffer) are silently ignored and
    /// reported as success. Implementations are expected to gracefully handle
    /// non-conformant but correctable input, e.g. by padding too-small frames.
    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>>;
}

/// An error encountered when sending a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceSendFrameError<T> {
    /// The device is not ready to send frames.
    DeviceNotReady(T),
}

/// Sets the TX queue configuration for a device.
pub fn set_tx_queue_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: TransmitQueueConfiguration,
) {
    let sync_ctx = &mut Locked::new(sync_ctx);
    match device {
        DeviceId::Ethernet(id) => TransmitQueueHandler::<EthernetLinkDevice, _>::set_configuration(
            sync_ctx, ctx, id, config,
        ),
        DeviceId::Loopback(id) => {
            TransmitQueueHandler::<LoopbackDevice, _>::set_configuration(sync_ctx, ctx, id, config)
        }
    }
}

/// Does the work of transmitting frames for a device.
pub fn transmit_queued_tx_frames<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
) -> Result<crate::WorkQueueReport, DeviceSendFrameError<()>> {
    let sync_ctx = &mut Locked::new(sync_ctx);
    match device {
        DeviceId::Ethernet(id) => {
            TransmitQueueHandler::<EthernetLinkDevice, _>::transmit_queued_frames(sync_ctx, ctx, id)
        }
        DeviceId::Loopback(id) => {
            TransmitQueueHandler::<LoopbackDevice, _>::transmit_queued_frames(sync_ctx, ctx, id)
        }
    }
}

/// Handle a batch of queued RX packets for the device.
///
/// If packets remain in the RX queue after a batch of RX packets has been
/// handled, the RX task will be scheduled to run again so the next batch of
/// RX packets may be handled. See [`DeviceLayerEventDispatcher::wake_rx_task`]
/// for more details.
pub fn handle_queued_rx_packets<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &LoopbackDeviceId<NonSyncCtx>,
) -> crate::WorkQueueReport {
    ReceiveQueueHandler::<LoopbackDevice, _>::handle_queued_rx_frames(
        &mut Locked::new(sync_ctx),
        ctx,
        device,
    )
}

/// The result of removing a device from core.
#[derive(Debug)]
pub enum RemoveDeviceResult<R, D> {
    /// The device was synchronously removed and no more references to it exist.
    Removed(R),
    /// The device was marked for destruction but there are still references to
    /// it in existence. The provided receiver can be polled on to observe
    /// device destruction completion.
    Deferred(D),
}

impl<R> RemoveDeviceResult<R, Never> {
    /// A helper function to unwrap a [`RemovedDeviceResult`] that can never be
    /// [`RemovedDeviceResult::Deferred`].
    pub fn into_removed(self) -> R {
        match self {
            Self::Removed(r) => r,
            Self::Deferred(never) => match never {},
        }
    }
}

/// An alias for [`RemoveDeviceResult`] that extracts the receiver type from the
/// NonSyncContext.
pub type RemoveDeviceResultWithContext<S, C> =
    RemoveDeviceResult<S, <C as crate::ReferenceNotifiers>::ReferenceReceiver<S>>;

fn remove_device<T: DeviceStateSpec, NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: BaseDeviceId<T, NonSyncCtx>,
    remove: impl FnOnce(
        &mut Devices<NonSyncCtx>,
        BaseDeviceId<T, NonSyncCtx>,
    ) -> (BasePrimaryDeviceId<T, NonSyncCtx>, BaseDeviceId<T, NonSyncCtx>),
) -> RemoveDeviceResultWithContext<T::External<NonSyncCtx>, NonSyncCtx>
where
    BaseDeviceId<T, NonSyncCtx>: Into<DeviceId<NonSyncCtx>>,
{
    // Start cleaning up the device by disabling IP state. This removes timers
    // for the device that would otherwise hold references to defunct device
    // state.
    let debug_references = {
        let mut sync_ctx = Locked::new(sync_ctx);

        let device = device.clone().into();

        crate::ip::device::clear_ipv4_device_state(&mut sync_ctx, ctx, &device);
        crate::ip::device::clear_ipv6_device_state(&mut sync_ctx, ctx, &device);
        device.downgrade().debug_references()
    };

    tracing::debug!("removing {device:?}");
    let (primary, strong) = {
        let mut devices = sync_ctx.state.device.devices.write();
        remove(&mut *devices, device)
    };
    assert_eq!(strong, primary);
    core::mem::drop(strong);
    match PrimaryRc::unwrap_or_notify_with(primary.into_inner(), || {
        let (notifier, receiver) =
            NonSyncCtx::new_reference_notifier::<T::External<NonSyncCtx>, _>(debug_references);
        let notifier = crate::sync::MapRcNotifier::new(notifier, |state: BaseDeviceState<_, _>| {
            state.external_state
        });
        (notifier, receiver)
    }) {
        Ok(s) => RemoveDeviceResult::Removed(s.external_state),
        Err(receiver) => RemoveDeviceResult::Deferred(receiver),
    }
}

/// Removes an Ethernet device from the device layer.
///
/// # Panics
///
/// Panics if the caller holds strong device IDs for `device`.
pub fn remove_ethernet_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: EthernetDeviceId<NonSyncCtx>,
) -> RemoveDeviceResultWithContext<NonSyncCtx::EthernetDeviceState, NonSyncCtx> {
    remove_device(sync_ctx, ctx, device, |devices, id| {
        let removed = devices
            .ethernet
            .remove(&id)
            .unwrap_or_else(|| panic!("no such Ethernet device: {id:?}"));

        (removed, id)
    })
}

/// Removes the Loopback device from the device layer.
///
/// # Panics
///
/// Panics if the caller holds strong device IDs for `device`.
pub fn remove_loopback_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: LoopbackDeviceId<NonSyncCtx>,
) -> RemoveDeviceResultWithContext<NonSyncCtx::LoopbackDeviceState, NonSyncCtx> {
    remove_device(sync_ctx, ctx, device, |devices, id| {
        let removed = devices.loopback.take().expect("loopback device not installed");
        (removed, id)
    })
}

/// Adds a new Ethernet device to the stack.
pub fn add_ethernet_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> (NonSyncCtx::EthernetDeviceState, NonSyncCtx::DeviceIdentifier),
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
    metric: RawMetric,
    bindings_state: F,
) -> EthernetDeviceId<NonSyncCtx> {
    sync_ctx.state.device.add_ethernet_device(mac, max_frame_size, metric, bindings_state)
}

/// Adds a new Ethernet device to the stack.
#[cfg(any(test, feature = "testutils"))]
pub(crate) fn add_ethernet_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
    metric: RawMetric,
) -> EthernetDeviceId<NonSyncCtx>
where
    NonSyncCtx::EthernetDeviceState: Default,
    NonSyncCtx::DeviceIdentifier: Default,
{
    add_ethernet_device_with_state(sync_ctx, mac, max_frame_size, metric, Default::default)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
pub fn add_loopback_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> (NonSyncCtx::LoopbackDeviceState, NonSyncCtx::DeviceIdentifier),
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
    metric: RawMetric,
    bindings_state: F,
) -> Result<LoopbackDeviceId<NonSyncCtx>, crate::error::ExistsError> {
    sync_ctx.state.device.add_loopback_device(mtu, metric, bindings_state)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
#[cfg(test)]
pub(crate) fn add_loopback_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
    metric: RawMetric,
) -> Result<LoopbackDeviceId<NonSyncCtx>, crate::error::ExistsError>
where
    NonSyncCtx::LoopbackDeviceState: Default,
    NonSyncCtx::DeviceIdentifier: Default,
{
    add_loopback_device_with_state(sync_ctx, mtu, metric, Default::default)
}

/// Receive a device layer frame from the network.
pub fn receive_frame<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &EthernetDeviceId<NonSyncCtx>,
    buffer: B,
) {
    trace_duration!(ctx, "device::receive_frame");
    self::ethernet::receive_frame(&mut Locked::new(sync_ctx), ctx, device, buffer)
}

/// Set the promiscuous mode flag on `device`.
// TODO(rheacock): remove `allow(dead_code)` when this is used.
#[allow(dead_code)]
pub(crate) fn set_promiscuous_mode<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    enabled: bool,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => {
            Ok(self::ethernet::set_promiscuous_mode(&mut Locked::new(sync_ctx), ctx, id, enabled))
        }
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Adds an IP address and associated subnet to this device.
///
/// For IPv6, this function also joins the solicited-node multicast group and
/// begins performing Duplicate Address Detection (DAD).
pub(crate) fn add_ip_addr_subnet<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr_sub_and_config: impl Into<AddrSubnetAndManualConfigEither<NonSyncCtx::Instant>>,
) -> Result<(), ExistsError> {
    let addr_sub_and_config = addr_sub_and_config.into();
    trace!(
        "add_ip_addr_subnet: adding addr_sub_and_config {:?} to device {:?}",
        addr_sub_and_config,
        device
    );
    let mut sync_ctx = Locked::new(sync_ctx);

    match addr_sub_and_config {
        AddrSubnetAndManualConfigEither::V4(addr_sub, config) => {
            crate::ip::device::add_ipv4_addr_subnet(&mut sync_ctx, ctx, device, addr_sub, config)
        }
        AddrSubnetAndManualConfigEither::V6(addr_sub, config) => {
            crate::ip::device::add_ipv6_addr_subnet(&mut sync_ctx, ctx, device, addr_sub, config)
        }
    }
}

/// Sets properties on an IP address.
pub fn set_ip_addr_properties<NonSyncCtx: NonSyncContext, A: IpAddress>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    address: SpecifiedAddr<A>,
    next_valid_until: Lifetime<NonSyncCtx::Instant>,
) -> Result<(), SetIpAddressPropertiesError> {
    trace!(
        "set_ip_addr_properties: setting valid_until={:?} for addr={}",
        next_valid_until,
        address
    );
    let mut sync_ctx = Locked::new(sync_ctx);

    match address.into() {
        IpAddr::V4(address) => crate::ip::device::set_ipv4_addr_properties(
            &mut sync_ctx,
            ctx,
            device,
            address,
            next_valid_until,
        ),
        IpAddr::V6(address) => crate::ip::device::set_ipv6_addr_properties(
            &mut sync_ctx,
            ctx,
            device,
            address,
            next_valid_until,
        ),
    }
}

/// Removes an IP address and associated subnet from this device.
///
/// Should only be called on user action.
pub(crate) fn del_ip_addr<NonSyncCtx: NonSyncContext, A: IpAddress>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: &SpecifiedAddr<A>,
) -> Result<(), NotFoundError> {
    trace!("del_ip_addr: removing addr {:?} from device {:?}", addr, device);
    let mut sync_ctx = Locked::new(sync_ctx);

    match Into::into(*addr) {
        IpAddr::V4(addr) => crate::ip::device::del_ipv4_addr(&mut sync_ctx, ctx, &device, &addr),
        IpAddr::V6(addr) => crate::ip::device::del_ipv6_addr_with_reason(
            &mut sync_ctx,
            ctx,
            &device,
            DelIpv6Addr::SpecifiedAddr(addr),
            crate::ip::device::state::DelIpv6AddrReason::ManualAction,
        ),
    }
}

/// Inserts a static neighbor entry for a neighbor.
pub fn insert_static_neighbor_entry<
    I: Ip,
    B: BufferMut,
    Id,
    C: BufferNonSyncContext<B> + crate::TimerContext<Id>,
>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    device: &DeviceId<C>,
    addr: I::Addr,
    mac: Mac,
) -> Result<(), StaticNeighborInsertionError> {
    let IpInvariant(result) = I::map_ip(
        (IpInvariant((sync_ctx, ctx, device, mac)), addr),
        |(IpInvariant((sync_ctx, ctx, device, mac)), addr)| {
            let result = UnicastAddr::new(mac)
                .ok_or(StaticNeighborInsertionError::AddressNotUnicast)
                .and_then(|mac| {
                    insert_static_arp_table_entry(sync_ctx, ctx, device, addr, mac)
                        .map_err(StaticNeighborInsertionError::NotSupported)
                });
            IpInvariant(result)
        },
        |(IpInvariant((sync_ctx, ctx, device, mac)), addr)| {
            let result = UnicastAddr::new(addr)
                .ok_or(StaticNeighborInsertionError::AddressNotUnicast)
                .and_then(|addr| {
                    insert_ndp_table_entry(sync_ctx, ctx, device, addr, mac)
                        .map_err(StaticNeighborInsertionError::NotSupported)
                });
            IpInvariant(result)
        },
    );
    result
}

/// Insert a static entry into this device's ARP table.
///
/// This will cause any conflicting dynamic entry to be removed, and
/// any future conflicting gratuitous ARPs to be ignored.
pub fn insert_static_arp_table_entry<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: Ipv4Addr,
    mac: UnicastAddr<Mac>,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_static_arp_table_entry(
            &mut Locked::new(sync_ctx),
            ctx,
            id,
            addr,
            mac.into(),
        )),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Insert an entry into this device's NDP table.
///
/// This method only gets called when testing to force set a neighbor's link
/// address so that lookups succeed immediately, without doing address
/// resolution.
pub fn insert_ndp_table_entry<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: UnicastAddr<Ipv6Addr>,
    mac: Mac,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_ndp_table_entry(
            &mut Locked::new(sync_ctx),
            ctx,
            id,
            addr,
            mac,
        )),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => Err(NotSupportedError),
    }
}

/// Gets the IPv4 configuration and flags for a `device`.
pub fn get_ipv4_configuration_and_flags<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv4DeviceConfigurationAndFlags {
    crate::ip::device::get_ipv4_configuration_and_flags(&mut Locked::new(sync_ctx), device)
}

/// Gets the IPv6 configuration and flags for a `device`.
pub fn get_ipv6_configuration_and_flags<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv6DeviceConfigurationAndFlags {
    crate::ip::device::get_ipv6_configuration_and_flags(&mut Locked::new(sync_ctx), device)
}

/// Updates the IPv4 configuration for a device.
///
/// Each field in [`Ipv4DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case,
/// as long as the outer `Option` is `Some`, then an attempt will be made to
/// update the configuration.
///
/// The IP device configuration is left unchanged if an `Err` is returned.
/// Otherwise, the previous values are returned for configurations that were
/// requested to be updated.
pub fn update_ipv4_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: Ipv4DeviceConfigurationUpdate,
) -> Result<Ipv4DeviceConfigurationUpdate, NotSupportedError> {
    crate::ip::device::update_ipv4_configuration(&mut Locked::new(sync_ctx), ctx, device, config)
}

/// Updates the IPv6 configuration for a device.
///
/// Each field in [`Ipv6DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case,
/// as long as the outer `Option` is `Some`, then an attempt will be made to
/// update the configuration.
///
/// The IP device configuration is left unchanged if an `Err` is returned.
/// Otherwise, the previous values are returned for configurations that were
/// requested to be updated.
pub fn update_ipv6_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: Ipv6DeviceConfigurationUpdate,
) -> Result<Ipv6DeviceConfigurationUpdate, NotSupportedError> {
    crate::ip::device::update_ipv6_configuration(&mut Locked::new(sync_ctx), ctx, device, config)
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    #[cfg(test)]
    use net_types::ip::IpVersion;

    use crate::ip::device::IpDeviceConfigurationUpdate;
    #[cfg(test)]
    use crate::testutil::Ctx;

    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
    pub(crate) struct FakeWeakDeviceId<D>(pub(crate) D);

    impl<D: PartialEq> PartialEq<D> for FakeWeakDeviceId<D> {
        fn eq(&self, other: &D) -> bool {
            let Self(this) = self;
            this == other
        }
    }

    impl<D: StrongId<Weak = Self>> WeakId for FakeWeakDeviceId<D> {
        type Strong = D;
    }

    impl<D: Id> Id for FakeWeakDeviceId<D> {
        fn is_loopback(&self) -> bool {
            let Self(inner) = self;
            inner.is_loopback()
        }
    }

    /// A fake device ID for use in testing.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
    pub(crate) struct FakeDeviceId;

    impl StrongId for FakeDeviceId {
        type Weak = FakeWeakDeviceId<Self>;
    }

    impl Id for FakeDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    pub(crate) trait FakeStrongDeviceId:
        StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord
    {
    }

    impl<D: StrongId<Weak = FakeWeakDeviceId<Self>> + 'static + Ord> FakeStrongDeviceId for D {}

    /// Calls [`receive_frame`], with a [`Ctx`].
    #[cfg(test)]
    pub(crate) fn receive_frame<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
        Ctx { sync_ctx, non_sync_ctx }: &mut Ctx<NonSyncCtx>,
        device: EthernetDeviceId<NonSyncCtx>,
        buffer: B,
    ) {
        crate::device::receive_frame(sync_ctx, non_sync_ctx, &device, buffer)
    }

    pub fn enable_device<NonSyncCtx: NonSyncContext>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
    ) {
        let ip_config =
            Some(IpDeviceConfigurationUpdate { ip_enabled: Some(true), ..Default::default() });
        let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
            sync_ctx,
            ctx,
            device,
            Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            ctx,
            device,
            Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
    }

    /// Enables or disables IP packet routing on `device`.
    #[cfg(test)]
    pub(crate) fn set_forwarding_enabled<NonSyncCtx: NonSyncContext, I: Ip>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        enabled: bool,
    ) -> Result<(), NotSupportedError> {
        let ip_config = Some(IpDeviceConfigurationUpdate {
            forwarding_enabled: Some(enabled),
            ..Default::default()
        });
        match I::VERSION {
            IpVersion::V4 => {
                let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
                    sync_ctx,
                    ctx,
                    device,
                    Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
                )
                .unwrap();
            }
            IpVersion::V6 => {
                let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                    sync_ctx,
                    ctx,
                    device,
                    Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
                )
                .unwrap();
            }
        }

        Ok(())
    }

    /// Returns whether IP packet routing is enabled on `device`.
    #[cfg(test)]
    pub(crate) fn is_forwarding_enabled<NonSyncCtx: NonSyncContext, I: Ip>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        device: &DeviceId<NonSyncCtx>,
    ) -> bool {
        let mut sync_ctx = Locked::new(sync_ctx);
        match I::VERSION {
            IpVersion::V4 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv4, _, _>(&mut sync_ctx, device)
            }
            IpVersion::V6 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv6, _, _>(&mut sync_ctx, device)
            }
        }
    }

    /// A device ID type that supports identifying more than one distinct
    /// device.
    #[cfg(test)]
    #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
    pub(crate) enum MultipleDevicesId {
        A,
        B,
        C,
    }

    #[cfg(test)]
    impl MultipleDevicesId {
        pub(crate) fn all() -> [Self; 3] {
            [Self::A, Self::B, Self::C]
        }
    }

    #[cfg(test)]
    impl Id for MultipleDevicesId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    impl StrongId for MultipleDevicesId {
        type Weak = FakeWeakDeviceId<Self>;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::num::NonZeroU8;

    use const_unwrap::const_unwrap_option;
    use net_declare::net_mac;
    use net_types::ip::AddrSubnet;
    use test_case::test_case;

    use super::*;
    use crate::{
        ip::device::{slaac::SlaacConfiguration, IpDeviceConfigurationUpdate},
        testutil::{
            Ctx, TestIpExt as _, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
    };

    #[test]
    fn test_origin_tracker() {
        let tracker = OriginTracker::new();
        if cfg!(debug_assertions) {
            assert_ne!(tracker, OriginTracker::new());
        } else {
            assert_eq!(tracker, OriginTracker::new());
        }
        assert_eq!(tracker.clone(), tracker);
    }

    #[test]
    fn frame_destination_from_dest() {
        const LOCAL_ADDR: Mac = net_mac!("88:88:88:88:88:88");

        assert_eq!(
            FrameDestination::from_dest(
                UnicastAddr::new(net_mac!("00:11:22:33:44:55")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Individual { local: false }
        );
        assert_eq!(
            FrameDestination::from_dest(LOCAL_ADDR, LOCAL_ADDR),
            FrameDestination::Individual { local: true }
        );
        assert_eq!(
            FrameDestination::from_dest(Mac::BROADCAST, LOCAL_ADDR),
            FrameDestination::Broadcast,
        );
        assert_eq!(
            FrameDestination::from_dest(
                MulticastAddr::new(net_mac!("11:11:11:11:11:11")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Multicast
        );
    }

    #[test]
    fn test_no_default_routes() {
        let Ctx { sync_ctx, non_sync_ctx: _ } = crate::testutil::FakeCtx::default();

        let _loopback_device: LoopbackDeviceId<_> =
            crate::device::add_loopback_device(&sync_ctx, Mtu::new(55), DEFAULT_INTERFACE_METRIC)
                .expect("error adding loopback device");

        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
        let _ethernet_device: EthernetDeviceId<_> = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::MIN,
            DEFAULT_INTERFACE_METRIC,
        );
        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
    }

    #[test]
    fn remove_ethernet_device_disables_timers() {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();

        let ethernet_device = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::from_mtu(Mtu::new(1500)).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        );

        {
            let device = ethernet_device.clone().into();
            // Enable the device, turning on a bunch of features that install
            // timers.
            let ip_config = Some(IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                gmp_enabled: Some(true),
                ..Default::default()
            });
            let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .unwrap();
            let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                Ipv6DeviceConfigurationUpdate {
                    max_router_solicitations: Some(Some(const_unwrap_option(NonZeroU8::new(2)))),
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        crate::device::remove_ethernet_device(&sync_ctx, &mut non_sync_ctx, ethernet_device)
            .into_removed();
        assert_eq!(non_sync_ctx.timer_ctx().timers(), &[]);
    }

    fn add_ethernet(
        sync_ctx: &mut &crate::testutil::FakeSyncCtx,
        _non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        crate::device::add_ethernet_device(
            sync_ctx,
            Ipv6::FAKE_CONFIG.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into()
    }

    fn add_loopback(
        sync_ctx: &mut &crate::testutil::FakeSyncCtx,
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        let device = crate::device::add_loopback_device(
            sync_ctx,
            Ipv6::MINIMUM_LINK_MTU,
            DEFAULT_INTERFACE_METRIC,
        )
        .unwrap()
        .into();
        crate::device::add_ip_addr_subnet(
            sync_ctx,
            non_sync_ctx,
            &device,
            AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                .unwrap(),
        )
        .unwrap();
        device
    }

    fn check_transmitted_ethernet(
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
        _device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        assert_eq!(non_sync_ctx.frames_sent().len(), count);
    }

    fn check_transmitted_loopback(
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
        device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        // Loopback frames leave the stack; outgoing frames land in
        // its RX queue.
        let rx_available = core::mem::take(&mut non_sync_ctx.state_mut().rx_available);
        if count == 0 {
            assert_eq!(rx_available, <[LoopbackDeviceId::<_>; 0]>::default());
        } else {
            assert_eq!(
                rx_available.into_iter().map(DeviceId::Loopback).collect::<Vec<_>>(),
                [device_id.clone()]
            );
        }
    }

    #[test_case(add_ethernet, check_transmitted_ethernet, true; "ethernet with queue")]
    #[test_case(add_ethernet, check_transmitted_ethernet, false; "ethernet without queue")]
    #[test_case(add_loopback, check_transmitted_loopback, true; "loopback with queue")]
    #[test_case(add_loopback, check_transmitted_loopback, false; "loopback without queue")]
    fn tx_queue(
        add_device: fn(
            &mut &crate::testutil::FakeSyncCtx,
            &mut crate::testutil::FakeNonSyncCtx,
        ) -> DeviceId<crate::testutil::FakeNonSyncCtx>,
        check_transmitted: fn(
            &mut crate::testutil::FakeNonSyncCtx,
            &DeviceId<crate::testutil::FakeNonSyncCtx>,
            usize,
        ),
        with_tx_queue: bool,
    ) {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let mut sync_ctx = &sync_ctx;
        let device = add_device(&mut sync_ctx, &mut non_sync_ctx);

        if with_tx_queue {
            crate::device::set_tx_queue_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                TransmitQueueConfiguration::Fifo,
            );
        }

        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            &sync_ctx,
            &mut non_sync_ctx,
            &device,
            Ipv6DeviceConfigurationUpdate {
                // Enable DAD so that the auto-generated address triggers a DAD
                // message immediately on interface enable.
                dad_transmits: Some(Some(const_unwrap_option(NonZeroU8::new(1)))),
                // Enable stable addresses so the link-local address is auto-
                // generated.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();

        if with_tx_queue {
            check_transmitted(&mut non_sync_ctx, &device, 0);
            assert_eq!(
                core::mem::take(&mut non_sync_ctx.state_mut().tx_available),
                [device.clone()]
            );
            assert_eq!(
                crate::device::transmit_queued_tx_frames(&sync_ctx, &mut non_sync_ctx, &device),
                Ok(crate::WorkQueueReport::AllDone)
            );
        }

        check_transmitted(&mut non_sync_ctx, &device, 1);
        assert_eq!(non_sync_ctx.state_mut().tx_available, <[DeviceId::<_>; 0]>::default());
    }
}
