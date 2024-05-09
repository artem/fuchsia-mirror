// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Abstractions to expose netstack3 core state to human inspection in bindings.
//!
//! This module mostly exists to abstract the dependency to Fuchsia inspect via
//! a trait, so we don't have to expose all of the internal core types to
//! bindings for it to perform the inspection.

use alloc::{format, string::String};
use core::fmt::{Debug, Display};

use net_types::{ip::IpAddress, AddrAndPortFormatter, ZonedAddr};

use crate::counters::Counter;

/// A trait abstracting a state inspector.
///
/// This trait follows roughly the same shape as the API provided by the
/// fuchsia_inspect crate, but we abstract it out so not to take the dependency.
///
/// Given we have the trait, we can fill it in with some helpful default
/// implementations for common types that are exposed as well, like IP addresses
/// and such.
pub trait Inspector: Sized {
    /// The type given to record contained children.
    type ChildInspector<'a>: Inspector;

    /// Records a nested inspector with `name` calling `f` with the nested child
    /// to be filled in.
    ///
    /// This is used to group and contextualize data.
    fn record_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, name: &str, f: F);

    /// Records a child without a name.
    ///
    /// The `Inpector` is expected to keep track of the number of unnamed
    /// children and allocate names appropriately from that.
    fn record_unnamed_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, f: F);

    /// Records a child whose name is the display implementation of `T`.
    fn record_display_child<T: Display, F: FnOnce(&mut Self::ChildInspector<'_>)>(
        &mut self,
        name: T,
        f: F,
    ) {
        self.record_child(&format!("{name}"), f)
    }

    /// Records a child whose name is the Debug implementation of `T`.
    fn record_debug_child<T: Debug, F: FnOnce(&mut Self::ChildInspector<'_>)>(
        &mut self,
        name: T,
        f: F,
    ) {
        self.record_child(&format!("{name:?}"), f)
    }

    /// Records anything that can be represented by a usize.
    fn record_usize<T: Into<usize>>(&mut self, name: &str, value: T);

    /// Records anything that can be represented by a u64.
    fn record_uint<T: Into<u64>>(&mut self, name: &str, value: T);

    /// Records anything that can be represented by a i64.
    fn record_int<T: Into<i64>>(&mut self, name: &str, value: T);

    /// Records anything that can be represented by a f64.
    fn record_double<T: Into<f64>>(&mut self, name: &str, value: T);

    /// Records a str value.
    fn record_str(&mut self, name: &str, value: &str);

    /// Records an owned string.
    fn record_string(&mut self, name: &str, value: String);

    /// Records a boolean.
    fn record_bool(&mut self, name: &str, value: bool);

    /// Records a counter.
    fn record_counter(&mut self, name: &str, value: &Counter) {
        self.record_uint(name, value.get())
    }

    /// Records a `value` that implements `Display` as its display string.
    fn record_display<T: Display>(&mut self, name: &str, value: T) {
        self.record_string(name, format!("{value}"))
    }

    /// Records a `value` that implements `Debug` as its debug string.
    fn record_debug<T: Debug>(&mut self, name: &str, value: T) {
        self.record_string(name, format!("{value:?}"))
    }

    /// Records an IP address.
    fn record_ip_addr<A: IpAddress>(&mut self, name: &str, value: A) {
        self.record_display(name, value)
    }

    /// Records a `ZonedAddr` and it's port, mapping the zone into an
    /// inspectable device identifier.
    fn record_zoned_addr_with_port<I: InspectorDeviceExt<D>, A: IpAddress, D, P: Display>(
        &mut self,
        name: &str,
        addr: ZonedAddr<A, D>,
        port: P,
    ) {
        self.record_display(
            name,
            AddrAndPortFormatter::<_, _, A::Version>::new(
                addr.map_zone(|device| I::device_identifier_as_address_zone(device)),
                port,
            ),
        )
    }

    /// Records the local address of a socket.
    fn record_local_socket_addr<I: InspectorDeviceExt<D>, A: IpAddress, D, P: Display>(
        &mut self,
        addr_with_port: Option<(ZonedAddr<A, D>, P)>,
    ) {
        const NAME: &str = "LocalAddress";
        if let Some((addr, port)) = addr_with_port {
            self.record_zoned_addr_with_port::<I, _, _, _>(NAME, addr, port);
        } else {
            self.record_str(NAME, "[NOT BOUND]")
        }
    }

    /// Records the remote address of a socket.
    fn record_remote_socket_addr<I: InspectorDeviceExt<D>, A: IpAddress, D, P: Display>(
        &mut self,
        addr_with_port: Option<(ZonedAddr<A, D>, P)>,
    ) {
        const NAME: &str = "RemoteAddress";
        if let Some((addr, port)) = addr_with_port {
            self.record_zoned_addr_with_port::<I, _, _, _>(NAME, addr, port);
        } else {
            self.record_str(NAME, "[NOT CONNECTED]")
        }
    }

    /// Records an implementor of [`InspectableValue`].
    fn record_inspectable_value<V: InspectableValue>(&mut self, name: &str, value: &V) {
        value.record(name, self)
    }

    /// Delegates more fields to be added by an [`Inspectable`] implementation.
    fn delegate_inspectable<V: Inspectable>(&mut self, value: &V) {
        value.record(self)
    }
}

/// A trait that allows a type to record its fields to an `inspector`.
///
/// This trait is used for types that are exposed to [`Inspector`]s many times
/// so recording them can be deduplicated.
pub trait Inspectable {
    /// Records this value into `inspector`.
    fn record<I: Inspector>(&self, inspector: &mut I);
}

impl Inspectable for () {
    fn record<I: Inspector>(&self, _inspector: &mut I) {}
}

/// A trait that marks a type as inspectable.
///
/// This trait is used for types that are exposed to [`Inspector`]s many times
/// so recording them can be deduplicated.
///
/// This type differs from [`Inspectable`] in that it receives a `name`
/// parameter. This is typically used for types that record a single entry.
pub trait InspectableValue {
    /// Records this value into `inspector`.
    fn record<I: Inspector>(&self, name: &str, inspector: &mut I);
}

/// An extension to `Inspector` that allows transforming and recording device
/// identifiers.
///
/// How to record device IDs is delegated to bindings via this trait, so we
/// don't need to propagate `InspectableValue` implementations everywhere in
/// core unnecessarily.
pub trait InspectorDeviceExt<D> {
    /// Records an entry named `name` with value `device`.
    fn record_device<I: Inspector>(inspector: &mut I, name: &str, device: &D);

    /// Returns the `Display` representation of the IPv6 scoped address zone
    /// associated with `D`.
    fn device_identifier_as_address_zone(device: D) -> impl Display;
}
