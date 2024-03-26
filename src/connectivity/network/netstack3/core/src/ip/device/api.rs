// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Device IP API.

use alloc::vec::Vec;

use net_types::{
    ip::{
        AddrSubnet, AddrSubnetEither, GenericOverIp, Ip, IpAddr, IpAddress, IpInvariant,
        IpVersionMarker, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
    },
    SpecifiedAddr,
};
use thiserror::Error;
use tracing::trace;

use crate::{
    context::{ContextPair, EventContext as _, InstantBindingsTypes},
    device::{AnyDevice, DeviceIdContext},
    error::ExistsError,
    error::NotFoundError,
    inspect::Inspector,
    ip::{
        self,
        device::{
            config::{
                IpDeviceConfigurationHandler, PendingIpDeviceConfigurationUpdate,
                UpdateIpConfigurationError,
            },
            state::{
                Ipv4AddrConfig, Ipv4AddressState, Ipv6AddrConfig, Ipv6AddrManualConfig,
                Ipv6AddressState, Lifetime,
            },
            DelIpAddr, IpAddressId as _, IpDeviceAddressContext as _, IpDeviceBindingsContext,
            IpDeviceConfigurationContext, IpDeviceEvent, IpDeviceIpExt, IpDeviceStateContext as _,
        },
        forwarding::IpForwardingDeviceContext,
        types::RawMetric,
        AddressRemovedReason,
    },
    time::Instant,
};

/// Provides an API for dealing with devices at the IP layer, aka interfaces.
pub struct DeviceIpApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> DeviceIpApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new())
    }
}

impl<I, C> DeviceIpApi<I, C>
where
    I: IpDeviceIpExt,
    C: ContextPair,
    C::CoreContext: IpDeviceConfigurationContext<I, C::BindingsContext>
        + IpDeviceConfigurationHandler<I, C::BindingsContext>
        + IpForwardingDeviceContext<I>,
    C::BindingsContext:
        IpDeviceBindingsContext<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.contexts()
    }

    /// Like [`DeviceIpApi::add_ip_addr_subnet_with_config`] with a default
    /// address configuration.
    pub fn add_ip_addr_subnet(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        addr_subnet: AddrSubnet<I::Addr>,
    ) -> Result<(), AddIpAddrSubnetError> {
        self.add_ip_addr_subnet_with_config(device, addr_subnet, Default::default())
    }

    /// Adds an IP address and associated subnet to this device.
    ///
    /// If Duplicate Address Detection (DAD) is enabled, begins performing DAD.
    ///
    /// For IPv6, this function also joins the solicited-node multicast group.
    pub fn add_ip_addr_subnet_with_config(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        addr_subnet: AddrSubnet<I::Addr>,
        addr_config: I::ManualAddressConfig<<C::BindingsContext as InstantBindingsTypes>::Instant>,
    ) -> Result<(), AddIpAddrSubnetError> {
        let addr_subnet = addr_subnet
            .replace_witness::<I::AssignedWitness>()
            .ok_or(AddIpAddrSubnetError::InvalidAddr)?;
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_ip_device_configuration(device, |config, mut core_ctx| {
            ip::device::add_ip_addr_subnet_with_config(
                &mut core_ctx,
                bindings_ctx,
                device,
                addr_subnet,
                addr_config.into(),
                config,
            )
            .map(|_address_id| ())
            .map_err(|ExistsError| AddIpAddrSubnetError::Exists)
        })
    }

    /// Delete an IP address on a device.
    pub fn del_ip_addr(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Result<(), NotFoundError> {
        let (core_ctx, bindings_ctx) = self.contexts();
        ip::device::del_ip_addr(
            core_ctx,
            bindings_ctx,
            device,
            DelIpAddr::SpecifiedAddr(addr),
            AddressRemovedReason::Manual,
        )
    }

    /// Updates the IP configuration for a device.
    ///
    /// Each field in [`Ipv4DeviceConfigurationUpdate`] or
    /// [`Ipv6DeviceConfigurationUpdate`] represents an optionally updateable
    /// configuration. If the field has a `Some(_)` value, then an attempt will
    /// be made to update that configuration on the device. A `None` value
    /// indicates that an update for the configuration is not requested.
    ///
    /// Note that some fields have the type `Option<Option<T>>`. In this case,
    /// as long as the outer `Option` is `Some`, then an attempt will be made to
    /// update the configuration.
    ///
    /// This function returns a [`PendingDeviceConfigurationUpdate`] which is
    /// validated and [`DeviceIpApi::apply`] can be called to apply the
    /// configuration.
    pub fn new_configuration_update<'a>(
        &mut self,
        device_id: &'a <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        config: I::ConfigurationUpdate,
    ) -> Result<
        PendingIpDeviceConfigurationUpdate<
            'a,
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        >,
        UpdateIpConfigurationError,
    > {
        PendingIpDeviceConfigurationUpdate::new(config, device_id)
    }

    /// Applies a pre-validated pending configuration to the device.
    ///
    /// Returns a configuration update with the previous value for all the
    /// requested fields in `config`.
    pub fn apply_configuration(
        &mut self,
        config: PendingIpDeviceConfigurationUpdate<
            '_,
            I,
            <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        >,
    ) -> I::ConfigurationUpdate {
        let (core_ctx, bindings_ctx) = self.contexts();
        IpDeviceConfigurationHandler::apply_configuration(core_ctx, bindings_ctx, config)
    }

    /// A shortcut for [`DeviceIpApi::new_configuration_update`] followed by
    /// [`DeviceIpApi::apply_configuration`].
    pub fn update_configuration(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        config: I::ConfigurationUpdate,
    ) -> Result<I::ConfigurationUpdate, UpdateIpConfigurationError> {
        let pending = self.new_configuration_update(device_id, config)?;
        Ok(self.apply_configuration(pending))
    }

    /// Gets the IP configuration and flags for a `device_id`.
    pub fn get_configuration(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
    ) -> I::ConfigurationAndFlags {
        self.core_ctx()
            .with_ip_device_configuration(device_id, |config, mut core_ctx| {
                (config.clone(), core_ctx.with_ip_device_flags(device_id, |flags| flags.clone()))
            })
            .into()
    }

    /// Gets the routing metric for the device.
    pub fn get_routing_metric(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
    ) -> RawMetric {
        self.core_ctx().get_routing_metric(device_id)
    }

    /// Sets properties on an IP address.
    pub fn set_addr_properties(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        address: SpecifiedAddr<I::Addr>,
        next_valid_until: Lifetime<<C::BindingsContext as InstantBindingsTypes>::Instant>,
    ) -> Result<(), SetIpAddressPropertiesError> {
        trace!(
            "set_ip_addr_properties: setting valid_until={:?} for addr={:?}",
            next_valid_until,
            address
        );
        let (core_ctx, bindings_ctx) = self.contexts();
        let address_id = core_ctx.get_address_id(device, address)?;
        core_ctx.with_ip_address_state_mut(device, &address_id, |address_state| {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct Wrap<'a, I: IpDeviceIpExt, II: Instant>(&'a mut I::AddressState<II>);
            let IpInvariant(valid_until) = I::map_ip(
                Wrap(address_state),
                |Wrap(Ipv4AddressState { config: Ipv4AddrConfig { valid_until } })| {
                    IpInvariant(Ok(valid_until))
                },
                |Wrap(Ipv6AddressState { flags: _, config })| {
                    IpInvariant(match config {
                        Ipv6AddrConfig::Slaac(_) => Err(SetIpAddressPropertiesError::NotManual),
                        Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }) => {
                            Ok(valid_until)
                        }
                    })
                },
            );
            let valid_until = valid_until?;
            if core::mem::replace(valid_until, next_valid_until) != next_valid_until {
                bindings_ctx.on_event(IpDeviceEvent::AddressPropertiesChanged {
                    device: device.clone(),
                    addr: address,
                    valid_until: next_valid_until,
                });
            }
            Ok(())
        })
    }

    /// Calls `f` for each assigned IP address on the device.
    pub fn for_each_assigned_ip_addr_subnet<F: FnMut(AddrSubnet<I::Addr>)>(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        f: F,
    ) {
        self.core_ctx().with_address_ids(device, |addrs, core_ctx| {
            addrs
                .filter_map(|addr| {
                    let assigned = core_ctx.with_ip_address_state(device, &addr, |addr_state| {
                        I::is_addr_assigned(addr_state)
                    });
                    assigned.then(|| addr.addr_sub().to_witness())
                })
                .for_each(f);
        })
    }

    /// Shorthand for [`DeviceIpApi::Collect_assigned_ip_addr_subnets`],
    /// returning the addresses in a `Vec`.
    pub fn get_assigned_ip_addr_subnets(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
    ) -> Vec<AddrSubnet<I::Addr>> {
        let mut vec = Vec::new();
        self.for_each_assigned_ip_addr_subnet(device, |a| vec.push(a));
        vec
    }

    /// Exports IP state for `device` into `inspector`.
    pub fn inspect<N: Inspector>(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        inspector: &mut N,
    ) {
        inspector.record_child("Addresses", |inspector| {
            self.core_ctx().with_address_ids(device, |addrs, core_ctx| {
                for addr in addrs {
                    inspector.record_display_child(addr.addr_sub(), |inspector| {
                        core_ctx.with_ip_address_state(device, &addr, |addr_state| {
                            inspector.delegate_inspectable(addr_state)
                        })
                    });
                }
            })
        })
    }
}
/// The device IP API interacting with all IP versions.
pub struct DeviceIpAnyApi<C>(C);

impl<C> DeviceIpAnyApi<C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

impl<C> DeviceIpAnyApi<C>
where
    C: ContextPair,
    C::CoreContext: IpDeviceConfigurationContext<Ipv4, C::BindingsContext>
        + IpDeviceConfigurationHandler<Ipv4, C::BindingsContext>
        + IpForwardingDeviceContext<Ipv4>
        + IpDeviceConfigurationContext<Ipv6, C::BindingsContext>
        + IpDeviceConfigurationHandler<Ipv6, C::BindingsContext>
        + IpForwardingDeviceContext<Ipv6>,
    C::BindingsContext: IpDeviceBindingsContext<Ipv4, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>
        + IpDeviceBindingsContext<Ipv6, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn ip<I: Ip>(&mut self) -> DeviceIpApi<I, &mut C> {
        let Self(pair) = self;
        DeviceIpApi::new(pair)
    }

    /// Like [`DeviceIpApi::add_ip_addr_subnet`].
    pub fn add_ip_addr_subnet(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        addr_sub_and_config: impl Into<
            AddrSubnetAndManualConfigEither<<C::BindingsContext as InstantBindingsTypes>::Instant>,
        >,
    ) -> Result<(), AddIpAddrSubnetError> {
        match addr_sub_and_config.into() {
            AddrSubnetAndManualConfigEither::V4(addr_sub, config) => {
                self.ip::<Ipv4>().add_ip_addr_subnet_with_config(device, addr_sub, config)
            }
            AddrSubnetAndManualConfigEither::V6(addr_sub, config) => {
                self.ip::<Ipv6>().add_ip_addr_subnet_with_config(device, addr_sub, config)
            }
        }
    }

    /// Like [`DeviceIpApi::del_ip_addr`].
    pub fn del_ip_addr(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        addr: impl Into<SpecifiedAddr<IpAddr>>,
    ) -> Result<(), NotFoundError> {
        let addr = addr.into();
        match addr.into() {
            IpAddr::V4(addr) => self.ip::<Ipv4>().del_ip_addr(device, addr),
            IpAddr::V6(addr) => self.ip::<Ipv6>().del_ip_addr(device, addr),
        }
    }

    /// Like [`DeviceIpApi::get_routing_metric`].
    pub fn get_routing_metric(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
    ) -> RawMetric {
        // NB: The routing metric is kept only once for both IP versions, debug
        // assert that this is true, but return the v4 version otherwise.
        let metric = self.ip::<Ipv4>().get_routing_metric(device_id);
        debug_assert_eq!(metric, self.ip::<Ipv6>().get_routing_metric(device_id));
        metric
    }

    /// Like [`DeviceIpApi::collect_assigned_ip_addr_subnets`], collecting
    /// addresses for both IP versions.
    pub fn for_each_assigned_ip_addr_subnet<F: FnMut(AddrSubnetEither)>(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        mut f: F,
    ) {
        self.ip::<Ipv4>().for_each_assigned_ip_addr_subnet(device, |a| f(a.into()));
        self.ip::<Ipv6>().for_each_assigned_ip_addr_subnet(device, |a| f(a.into()));
    }

    /// Like [`DeviceIpApi::get_assigned_ip_addr_subnets`], returning addresses
    /// for both IP versions.
    pub fn get_assigned_ip_addr_subnets(
        &mut self,
        device: &<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
    ) -> Vec<AddrSubnetEither> {
        let mut vec = Vec::new();
        self.for_each_assigned_ip_addr_subnet(device, |a| vec.push(a));
        vec
    }
}

/// An AddrSubnet together with configuration specified for it when adding it
/// to the stack.
#[derive(Debug)]
pub enum AddrSubnetAndManualConfigEither<Instant> {
    /// Variant for an Ipv4 AddrSubnet.
    V4(AddrSubnet<Ipv4Addr>, Ipv4AddrConfig<Instant>),
    /// Variant for an Ipv6 AddrSubnet.
    V6(AddrSubnet<Ipv6Addr>, Ipv6AddrManualConfig<Instant>),
}

impl<Instant: crate::time::Instant> AddrSubnetAndManualConfigEither<Instant> {
    /// Constructs an `AddrSubnetAndManualConfigEither`.
    pub(crate) fn new<I: Ip + IpDeviceIpExt>(
        addr_subnet: AddrSubnet<I::Addr>,
        config: I::ManualAddressConfig<Instant>,
    ) -> Self {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct AddrSubnetAndConfig<I: IpDeviceIpExt, Instant: crate::time::Instant> {
            addr_subnet: AddrSubnet<I::Addr>,
            config: I::ManualAddressConfig<Instant>,
        }

        let IpInvariant(result) = I::map_ip(
            AddrSubnetAndConfig { addr_subnet, config },
            |AddrSubnetAndConfig { addr_subnet, config }| {
                IpInvariant(AddrSubnetAndManualConfigEither::V4(addr_subnet, config))
            },
            |AddrSubnetAndConfig { addr_subnet, config }| {
                IpInvariant(AddrSubnetAndManualConfigEither::V6(addr_subnet, config))
            },
        );
        result
    }

    /// Extracts the `AddrSubnetEither`.
    pub fn addr_subnet_either(&self) -> AddrSubnetEither {
        match self {
            Self::V4(addr_subnet, _) => AddrSubnetEither::V4(*addr_subnet),
            Self::V6(addr_subnet, _) => AddrSubnetEither::V6(*addr_subnet),
        }
    }
}

impl<Instant: crate::time::Instant> From<AddrSubnetEither>
    for AddrSubnetAndManualConfigEither<Instant>
{
    fn from(value: AddrSubnetEither) -> Self {
        match value {
            AddrSubnetEither::V4(addr_subnet) => {
                AddrSubnetAndManualConfigEither::new::<Ipv4>(addr_subnet, Default::default())
            }
            AddrSubnetEither::V6(addr_subnet) => {
                AddrSubnetAndManualConfigEither::new::<Ipv6>(addr_subnet, Default::default())
            }
        }
    }
}

impl<Instant: crate::time::Instant, I: IpAddress> From<AddrSubnet<I>>
    for AddrSubnetAndManualConfigEither<Instant>
{
    fn from(value: AddrSubnet<I>) -> Self {
        AddrSubnetEither::from(value).into()
    }
}

/// Errors that can be returned by the [`DeviceIpApiAny::add_ip_addr_subnet`]
/// function.
#[derive(Debug, Eq, PartialEq)]
pub enum AddIpAddrSubnetError {
    /// The address is already assigned to this device.
    Exists,
    /// The address is invalid and cannot be assigned to any device. For
    /// example, an IPv4-mapped-IPv6 address.
    InvalidAddr,
}

/// Error type for setting properties on IP addresses.
#[derive(Error, Debug, PartialEq)]
pub enum SetIpAddressPropertiesError {
    /// The address we tried to set properties on was not found.
    #[error("{0}")]
    NotFound(#[from] NotFoundError),

    /// We tried to set properties on a non-manually-configured address.
    #[error("tried to set properties on a non-manually-configured address")]
    NotManual,
}
