// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with the Fuchsia Inspect system.

use core::fmt::Display;
use core::marker::PhantomData;

use fuchsia_inspect::Node;

use tracing::warn;

use netstack3_base::{Inspector, InspectorDeviceExt};

/// Provides a Fuchsia implementation of `Inspector`.
pub struct FuchsiaInspector<'a, D> {
    node: &'a Node,
    unnamed_count: usize,
    _marker: PhantomData<D>,
}

impl<'a, D> FuchsiaInspector<'a, D> {
    /// Create a new `FuchsiaInspector` rooted at `node`.
    pub fn new(node: &'a Node) -> Self {
        Self { node, unnamed_count: 0, _marker: Default::default() }
    }
}

impl<'a, D> Inspector for FuchsiaInspector<'a, D> {
    type ChildInspector<'l> = FuchsiaInspector<'l, D>;

    fn record_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, name: &str, f: F) {
        self.node.record_child(name, |node| f(&mut FuchsiaInspector::new(node)))
    }

    fn record_unnamed_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, f: F) {
        let Self { node: _, unnamed_count, _marker: _ } = self;
        let id = core::mem::replace(unnamed_count, *unnamed_count + 1);
        self.record_child(&format!("{id}"), f)
    }

    fn record_usize<T: Into<usize>>(&mut self, name: &str, value: T) {
        let value: u64 = value.into().try_into().unwrap_or_else(|e| {
            warn!("failed to inspect usize value that does not fit in a u64: {e:?}");
            u64::MAX
        });
        self.node.record_uint(name, value)
    }

    fn record_uint<T: Into<u64>>(&mut self, name: &str, value: T) {
        self.node.record_uint(name, value.into())
    }

    fn record_int<T: Into<i64>>(&mut self, name: &str, value: T) {
        self.node.record_int(name, value.into())
    }

    fn record_double<T: Into<f64>>(&mut self, name: &str, value: T) {
        self.node.record_double(name, value.into())
    }

    fn record_str(&mut self, name: &str, value: &str) {
        self.node.record_string(name, value)
    }

    fn record_string(&mut self, name: &str, value: String) {
        self.node.record_string(name, value)
    }

    fn record_bool(&mut self, name: &str, value: bool) {
        self.node.record_bool(name, value)
    }
}

/// Provides an abstract interface for extracting inspect device identifier.
pub trait InspectorDeviceIdProvider<DeviceId> {
    /// Extracts the device identifier from the provided opaque type.
    fn device_id(id: &DeviceId) -> u64;
}

impl<'a, D, P: InspectorDeviceIdProvider<D>> InspectorDeviceExt<D> for FuchsiaInspector<'a, P> {
    fn record_device<I: Inspector>(inspector: &mut I, name: &str, device: &D) {
        inspector.record_uint(name, P::device_id(device))
    }

    fn device_identifier_as_address_zone(id: D) -> impl Display {
        P::device_id(&id)
    }
}

#[cfg(any(test, feature = "testutils"))]
pub mod testutils {
    pub use diagnostics_assertions::assert_data_tree;
    pub use fuchsia_inspect::Inspector;
}
