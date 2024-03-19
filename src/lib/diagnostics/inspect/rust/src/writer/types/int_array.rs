// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::writer::{ArithmeticArrayProperty, ArrayProperty, Inner, InnerValueType, InspectType};
use tracing::error;

/// Inspect int array data type.
///
/// NOTE: do not rely on PartialEq implementation for true comparison.
/// Instead leverage the reader.
///
/// NOTE: Operations on a Default value are no-ops.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct IntArrayProperty {
    pub(crate) inner: Inner<InnerValueType>,
}

impl InspectType for IntArrayProperty {}

crate::impl_inspect_type_internal!(IntArrayProperty);

impl ArrayProperty for IntArrayProperty {
    type Type = i64;

    fn set(&self, index: usize, value: impl Into<Self::Type>) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| {
                    state.set_array_int_slot(inner_ref.block_index, index, value.into())
                })
                .unwrap_or_else(|err| {
                    error!(?err, "Failed to set property");
                });
        }
    }

    fn clear(&self) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.clear_array(inner_ref.block_index, 0))
                .unwrap_or_else(|e| {
                    error!("Failed to clear property. Error: {:?}", e);
                });
        }
    }
}

impl ArithmeticArrayProperty for IntArrayProperty {
    fn add(&self, index: usize, value: i64) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| state.add_array_int_slot(inner_ref.block_index, index, value))
                .unwrap_or_else(|err| {
                    error!(?err, "Failed to add property");
                });
        }
    }

    fn subtract(&self, index: usize, value: i64) {
        if let Some(ref inner_ref) = self.inner.inner_ref() {
            inner_ref
                .state
                .try_lock()
                .and_then(|mut state| {
                    state.subtract_array_int_slot(inner_ref.block_index, index, value)
                })
                .unwrap_or_else(|err| {
                    error!(?err, "Failed to subtract property");
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assert_update_is_atomic,
        writer::{testing_utils::GetBlockExt, Length},
        Inspector,
    };

    #[fuchsia::test]
    fn test_int_array() {
        // Create and use a default value.
        let default = IntArrayProperty::default();
        default.add(1, 1);

        let inspector = Inspector::default();
        let root = inspector.root();
        let node = root.create_child("node");
        {
            let array = node.create_int_array("array_property", 5);
            assert_eq!(array.len().unwrap(), 5);

            array.set(0, 5);
            array.get_block(|array_block| {
                assert_eq!(array_block.array_get_int_slot(0).unwrap(), 5);
            });

            array.add(0, 5);
            array.get_block(|array_block| {
                assert_eq!(array_block.array_get_int_slot(0).unwrap(), 10);
            });

            array.subtract(0, 3);

            array.get_block(|array_block| {
                assert_eq!(array_block.array_get_int_slot(0).unwrap(), 7);
            });

            array.set(1, 2);
            array.set(3, -3);

            array.get_block(|array_block| {
                for (i, value) in [7, 2, 0, -3, 0].iter().enumerate() {
                    assert_eq!(array_block.array_get_int_slot(i).unwrap(), *value);
                }
            });

            array.clear();
            array.get_block(|array_block| {
                for i in 0..5 {
                    assert_eq!(0, array_block.array_get_int_slot(i).unwrap());
                }
            });

            node.get_block(|node_block| {
                assert_eq!(node_block.child_count().unwrap(), 1);
            });
        }
        node.get_block(|node_block| {
            assert_eq!(node_block.child_count().unwrap(), 0);
        });
    }

    #[fuchsia::test]
    fn property_atomics() {
        let inspector = Inspector::default();
        let array = inspector.root().create_int_array("array", 5);

        assert_update_is_atomic!(array, |array| {
            array.set(0, 0);
            array.set(1, 1);
            array.set(2, 2);
            array.set(3, 3);
            array.set(4, 4);
        });
    }
}
