// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::AnyCapability;

lazy_static! {
    static ref REGISTRY: Mutex<Registry> = Mutex::new(Registry::default());
}

/// Inserts a capability into the global registry.
pub(crate) fn insert(capability: AnyCapability, koid: zx::Koid) {
    let mut registry = REGISTRY.lock().unwrap();
    let existing = registry.insert(koid, Entry { capability, task: None });
    assert!(existing.is_none());
}

/// Inserts a capability with an associated task into the global registry.
pub(crate) fn insert_with_task(capability: AnyCapability, koid: zx::Koid, task: fasync::Task<()>) {
    let mut registry = REGISTRY.lock().unwrap();
    let existing = registry.insert(koid, Entry { capability, task: Some(task) });
    assert!(existing.is_none());
}

/// Removes a capability from the global registry and returns it, if it exists.
///
/// The associated task is dropped, if any.
pub(crate) fn remove(koid: zx::Koid) -> Option<AnyCapability> {
    let mut registry = REGISTRY.lock().unwrap();
    registry.remove(koid).map(|entry| entry.capability)
}

pub struct Entry {
    pub capability: AnyCapability,
    pub task: Option<fasync::Task<()>>,
}

/// The [Registry] stores capabilities that have been converted to FIDL, providing a way to get
/// the original Rust object back from a FIDL representation of a capability.
///
/// There should only be a single Registry, outside of unit tests.
#[derive(Default)]
pub struct Registry {
    entries: HashMap<zx::Koid, Entry>,
}

impl Registry {
    /// Inserts an entry into the registry.
    ///
    /// If an entry with the same koid already exists, replaces the entry with the new one
    /// and returns the old one.
    ///
    /// Returns None if the entry with the given koid did not previously exist.
    pub(crate) fn insert(&mut self, koid: zx::Koid, entry: Entry) -> Option<Entry> {
        self.entries.insert(koid, entry)
    }

    /// Removes an entry from the registry, if one with a matching koid exists.
    pub(crate) fn remove(&mut self, koid: zx::Koid) -> Option<Entry> {
        self.entries.remove(&koid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Unit;

    /// Tests that a capability can be inserted and retrieved from a Registry.
    #[test]
    fn insert_remove() {
        let mut registry = Registry::default();

        // Insert a Unit capability into the registry.
        let koid = zx::Koid::from_raw(123);
        let unit = Unit::default();
        assert!(registry.insert(koid, Entry { capability: Box::new(unit), task: None }).is_none());

        // Remove a capability with the same koid. It should be a Unit.
        let entry = registry.remove(koid).unwrap();
        let got_unit: Unit = entry.capability.try_into().unwrap();
        assert_eq!(got_unit, Unit::default());
    }
}
