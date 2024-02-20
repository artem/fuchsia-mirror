// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device layer api.

use core::marker::PhantomData;

use net_types::ip::{Ipv4, Ipv6};
use packet::BufferMut;
use tracing::debug;

use crate::{
    base::ContextPair,
    context::{RecvFrameContext, ReferenceNotifiers},
    device::{
        config::{
            ArpConfiguration, ArpConfigurationUpdate, DeviceConfiguration,
            DeviceConfigurationContext, DeviceConfigurationUpdate, DeviceConfigurationUpdateError,
            NdpConfiguration, NdpConfigurationUpdate,
        },
        ethernet::EthernetLinkDevice,
        loopback::LoopbackDevice,
        pure_ip::PureIpDevice,
        state::{BaseDeviceState, DeviceStateSpec, IpLinkDeviceStateInner},
        AnyDevice, BaseDeviceId, BasePrimaryDeviceId, Device, DeviceCollectionContext, DeviceId,
        DeviceIdContext, DeviceLayerStateTypes, DeviceLayerTypes, DeviceReceiveFrameSpec,
        OriginTrackerContext,
    },
    for_any_device_id,
    ip::{
        device::{
            IpDeviceBindingsContext, IpDeviceConfigurationContext, Ipv6DeviceConfigurationContext,
        },
        types::RawMetric,
    },
    sync::{
        types::{RemoveResourceResult, RemoveResourceResultWithContext},
        PrimaryRc,
    },
};

/// Pending device configuration update.
///
/// This type is a witness for a valid [`DeviceConfigurationUpdate`] for some
/// device ID `D` and is obtained through
/// [`DeviceApi::new_configuration_update`].
///
/// The configuration is only applied when [`DeviceApi::apply_configuration`] is
/// called.
pub struct PendingDeviceConfigurationUpdate<'a, D>(DeviceConfigurationUpdate, &'a D);

/// The device API.
pub struct DeviceApi<D, C>(C, PhantomData<D>);

impl<D, C> DeviceApi<D, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, PhantomData)
    }
}

impl<D, C> DeviceApi<D, C>
where
    D: Device + DeviceStateSpec + DeviceReceiveFrameSpec,
    C: ContextPair,
    C::CoreContext: DeviceApiCoreContext<D, C::BindingsContext>,
    C::BindingsContext: DeviceApiBindingsContext,
{
    pub(crate) fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, PhantomData) = self;
        pair.contexts()
    }

    pub(crate) fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, PhantomData) = self;
        pair.core_ctx()
    }

    /// Adds a new device to the stack and returns its identifier.
    ///
    /// # Panics
    ///
    /// Panics if more than 1 loopback device is added to the stack.
    pub fn add_device(
        &mut self,
        bindings_id: <C::BindingsContext as DeviceLayerStateTypes>::DeviceIdentifier,
        properties: D::CreationProperties,
        metric: RawMetric,
        external_state: D::External<C::BindingsContext>,
    ) -> <C::CoreContext as DeviceIdContext<D>>::DeviceId {
        debug!("adding {} device with {:?} metric:{metric}", D::DEBUG_TYPE, properties);
        let core_ctx = self.core_ctx();
        let origin = core_ctx.origin_tracker();
        let primary = BasePrimaryDeviceId::new(
            IpLinkDeviceStateInner::new(D::new_link_state(properties), metric, origin),
            external_state,
            bindings_id,
        );
        let id = primary.clone_strong();
        core_ctx.insert(primary);
        id
    }

    /// Like [`DeviceApi::add_device`] but using default values for
    /// `bindings_id` and `external_state`.
    ///
    /// This is provided as a convenience method for tests with faked bindings
    /// contexts that have simple implementations for bindings state.
    #[cfg(any(test, feature = "testutils"))]
    pub(crate) fn add_device_with_default_state(
        &mut self,
        properties: D::CreationProperties,
        metric: RawMetric,
    ) -> <C::CoreContext as DeviceIdContext<D>>::DeviceId
    where
        <C::BindingsContext as DeviceLayerStateTypes>::DeviceIdentifier: Default,
        D::External<C::BindingsContext>: Default,
    {
        self.add_device(Default::default(), properties, metric, Default::default())
    }

    /// Removes `device` from the stack.
    ///
    /// If the return value is `RemoveDeviceResult::Removed` the device is
    /// immediately removed from the stack, otherwise
    /// `RemoveDeviceResult::Deferred` indicates that the device was marked for
    /// destruction but there are still references to it. It carries a
    /// `ReferenceReceiver` from the bindings context that can be awaited on
    /// until removal is complete.
    ///
    /// # Panics
    ///
    /// Panics if the device is not currently in the stack.
    pub fn remove_device(
        &mut self,
        device: BaseDeviceId<D, C::BindingsContext>,
    ) -> RemoveResourceResultWithContext<D::External<C::BindingsContext>, C::BindingsContext>
    where
        // Required to call into IP layer for cleanup on removal:
        BaseDeviceId<D, C::BindingsContext>: Into<DeviceId<C::BindingsContext>>,
        C::CoreContext: IpDeviceConfigurationContext<Ipv4, C::BindingsContext>
            + Ipv6DeviceConfigurationContext<C::BindingsContext>
            + DeviceIdContext<AnyDevice, DeviceId = DeviceId<C::BindingsContext>>,
        C::BindingsContext: IpDeviceBindingsContext<Ipv4, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>
            + IpDeviceBindingsContext<Ipv6, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    {
        // Start cleaning up the device by disabling IP state. This removes timers
        // for the device that would otherwise hold references to defunct device
        // state.
        let (core_ctx, bindings_ctx) = self.contexts();
        let debug_references = {
            let device = device.clone().into();
            crate::ip::device::clear_ipv4_device_state(core_ctx, bindings_ctx, &device);
            crate::ip::device::clear_ipv6_device_state(core_ctx, bindings_ctx, &device);
            device.downgrade().debug_references()
        };

        tracing::debug!("removing {device:?}");
        let primary = core_ctx.remove(&device).expect("tried to remove device not in stack");
        assert_eq!(device, primary);
        core::mem::drop(device);
        match PrimaryRc::unwrap_or_notify_with(primary.into_inner(), || {
            let (notifier, receiver) = C::BindingsContext::new_reference_notifier::<
                D::External<C::BindingsContext>,
                _,
            >(debug_references);
            let notifier =
                crate::sync::MapRcNotifier::new(notifier, |state: BaseDeviceState<_, _>| {
                    state.external_state
                });
            (notifier, receiver)
        }) {
            Ok(s) => RemoveResourceResult::Removed(s.external_state),
            Err(receiver) => RemoveResourceResult::Deferred(receiver),
        }
    }

    /// Receive a device layer frame from the network.
    pub fn receive_frame<B: BufferMut>(
        &mut self,
        meta: D::FrameMetadata<BaseDeviceId<D, C::BindingsContext>>,
        frame: B,
    ) {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.receive_frame(bindings_ctx, meta, frame)
    }

    /// Applies the configuration and returns a [`DeviceConfigurationUpdate`]
    /// with the previous values for all configurations for all `Some` fields.
    ///
    /// Note that even if the previous value matched the requested value, it is
    /// still populated in the returned `DeviceConfigurationUpdate`.
    pub fn apply_configuration(
        &mut self,
        pending: PendingDeviceConfigurationUpdate<'_, BaseDeviceId<D, C::BindingsContext>>,
    ) -> DeviceConfigurationUpdate {
        let PendingDeviceConfigurationUpdate(DeviceConfigurationUpdate { arp, ndp }, device_id) =
            pending;
        let core_ctx = self.core_ctx();
        let arp = core_ctx.with_nud_config_mut::<Ipv4, _, _>(device_id, move |device_config| {
            let device_config = match device_config {
                Some(c) => c,
                None => {
                    // Can't set ARP configuration if device doesn't support it,
                    // this is validated when creating the
                    // `PendingDeviceConfigurationUpdate`.
                    assert!(arp.is_none());
                    return None;
                }
            };
            arp.map(|ArpConfigurationUpdate { nud }| {
                let nud = nud.map(|config| config.apply_and_take_previous(device_config));
                ArpConfigurationUpdate { nud }
            })
        });
        let ndp = core_ctx.with_nud_config_mut::<Ipv6, _, _>(device_id, move |device_config| {
            let device_config = match device_config {
                Some(c) => c,
                None => {
                    // Can't set NDP configuration if device doesn't support it,
                    // this is validated when creating the
                    // `PendingDeviceConfigurationUpdate`.
                    assert!(ndp.is_none());
                    return None;
                }
            };
            ndp.map(|NdpConfigurationUpdate { nud }| {
                let nud = nud.map(|config| config.apply_and_take_previous(device_config));
                NdpConfigurationUpdate { nud }
            })
        });
        DeviceConfigurationUpdate { arp, ndp }
    }

    /// Creates a new device configuration update for the given device.
    ///
    /// This method only validates that `config` is valid for `device`.
    /// [`DeviceApi::apply`] must be called to apply the configuration.
    pub fn new_configuration_update<'a>(
        &mut self,
        device: &'a BaseDeviceId<D, C::BindingsContext>,
        config: DeviceConfigurationUpdate,
    ) -> Result<
        PendingDeviceConfigurationUpdate<'a, BaseDeviceId<D, C::BindingsContext>>,
        DeviceConfigurationUpdateError,
    > {
        let core_ctx = self.core_ctx();
        let DeviceConfigurationUpdate { arp, ndp } = &config;
        if arp.is_some() && core_ctx.with_nud_config::<Ipv4, _, _>(device, |c| c.is_none()) {
            return Err(DeviceConfigurationUpdateError::ArpNotSupported);
        }
        if ndp.is_some() && core_ctx.with_nud_config::<Ipv6, _, _>(device, |c| c.is_none()) {
            return Err(DeviceConfigurationUpdateError::NdpNotSupported);
        }
        Ok(PendingDeviceConfigurationUpdate(config, device))
    }

    /// Returns a snapshot of the given device's configuration.
    pub fn get_configuration(
        &mut self,
        device: &BaseDeviceId<D, C::BindingsContext>,
    ) -> DeviceConfiguration {
        let core_ctx = self.core_ctx();
        let arp = core_ctx
            .with_nud_config::<Ipv4, _, _>(device, |config| config.cloned())
            .map(|nud| ArpConfiguration { nud });
        let ndp = core_ctx
            .with_nud_config::<Ipv6, _, _>(device, |config| config.cloned())
            .map(|nud| NdpConfiguration { nud });
        DeviceConfiguration { arp, ndp }
    }
}

/// The device API interacting with any kind of supported device.
pub struct DeviceAnyApi<C>(C);

impl<C> DeviceAnyApi<C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

impl<C> DeviceAnyApi<C>
where
    C: ContextPair,
    C::CoreContext: DeviceApiCoreContext<EthernetLinkDevice, C::BindingsContext>
        + DeviceApiCoreContext<LoopbackDevice, C::BindingsContext>
        + DeviceApiCoreContext<PureIpDevice, C::BindingsContext>,
    C::BindingsContext: DeviceApiBindingsContext,
{
    fn device<D>(&mut self) -> DeviceApi<D, &mut C> {
        let Self(pair) = self;
        DeviceApi::new(pair)
    }

    /// Like [`DeviceApi::apply_configuration`] but for any device types.
    pub fn apply_configuration(
        &mut self,
        pending: PendingDeviceConfigurationUpdate<'_, DeviceId<C::BindingsContext>>,
    ) -> DeviceConfigurationUpdate {
        let PendingDeviceConfigurationUpdate(config, device) = pending;
        for_any_device_id!(DeviceId, device,
            device => {
                self.device().apply_configuration(PendingDeviceConfigurationUpdate(config, device))
            }
        )
    }

    /// Like [`DeviceApi::new_configuration_update`] but for any device
    /// types.
    pub fn new_configuration_update<'a>(
        &mut self,
        device: &'a DeviceId<C::BindingsContext>,
        config: DeviceConfigurationUpdate,
    ) -> Result<
        PendingDeviceConfigurationUpdate<'a, DeviceId<C::BindingsContext>>,
        DeviceConfigurationUpdateError,
    > {
        for_any_device_id!(DeviceId, device,
            inner => {
                self.device()
                .new_configuration_update(inner, config)
                .map(|PendingDeviceConfigurationUpdate(config, _)| {
                    PendingDeviceConfigurationUpdate(config, device)
                })
            }
        )
    }

    /// Like [`DeviceApi::get_configuration`] but for any device types.
    pub fn get_configuration(
        &mut self,
        device: &DeviceId<C::BindingsContext>,
    ) -> DeviceConfiguration {
        for_any_device_id!(DeviceId, device,
            device => self.device().get_configuration(device))
    }
}

/// A marker trait for all the core context traits required to fulfill the
/// [`DeviceApi`].
pub trait DeviceApiCoreContext<
    D: Device + DeviceStateSpec + DeviceReceiveFrameSpec,
    BC: DeviceApiBindingsContext,
>:
    DeviceIdContext<D, DeviceId = BaseDeviceId<D, BC>>
    + OriginTrackerContext
    + DeviceCollectionContext<D, BC>
    + DeviceConfigurationContext<D>
    + RecvFrameContext<BC, D::FrameMetadata<BaseDeviceId<D, BC>>>
{
}

impl<O, D, BC> DeviceApiCoreContext<D, BC> for O
where
    D: Device + DeviceStateSpec + DeviceReceiveFrameSpec,
    BC: DeviceApiBindingsContext,
    O: DeviceIdContext<D, DeviceId = BaseDeviceId<D, BC>>
        + OriginTrackerContext
        + DeviceCollectionContext<D, BC>
        + DeviceConfigurationContext<D>
        + RecvFrameContext<BC, D::FrameMetadata<BaseDeviceId<D, BC>>>,
{
}

/// A marker trait for all the bindings context traits required to fulfill the
/// [`DeviceApi`].
pub trait DeviceApiBindingsContext: DeviceLayerTypes + ReferenceNotifiers {}

impl<O> DeviceApiBindingsContext for O where O: DeviceLayerTypes + ReferenceNotifiers {}
