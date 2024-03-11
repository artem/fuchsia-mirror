// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! General utilities for converting types.

/// Produces an owned value `T` from Self.
///
/// This trait is useful for implementing functions that take a value that may
/// or may not end up needing to be consumed, but where the caller doesn't
/// necessarily want or need to keep the value either.
///
/// This trait is blanket-implemented for `T` as a pass-through and for `&T`
/// where `T: Clone` by cloning `self`.
pub(crate) trait OwnedOrCloned<T> {
    fn into_owned(self) -> T;
}

impl<T> OwnedOrCloned<T> for T {
    fn into_owned(self) -> T {
        self
    }
}

impl<'s, T: Clone> OwnedOrCloned<T> for &'s T {
    fn into_owned(self) -> T {
        self.clone()
    }
}

/// Provides functions for converting infallibly between types.
///
/// This trait can be implemented on types that allow converting between two
/// related types. It has two blanket implementations: `()` for identity
/// conversions, i.e. `Input=Output`, and [`UninstantiableConverter`] as an
/// uninstantiable type that implements the trait for any input and output.
pub(crate) trait BidirectionalConverter<Input, Output> {
    /// Converts an instance of `Input` into an instance of `Output`.
    fn convert(&self, a: Input) -> Output;

    /// Converts an instance of `Output` into an instance of `Input`.
    fn convert_back(&self, b: Output) -> Input;
}

impl<I> BidirectionalConverter<I, I> for () {
    fn convert_back(&self, value: I) -> I {
        value
    }
    fn convert(&self, value: I) -> I {
        value
    }
}

/// A marker trait for [`BidirectionalConverter`] of owned or reference types.
pub trait OwnedOrRefsBidirectionalConverter<Input, Output>:
    BidirectionalConverter<Input, Output>
    + for<'a> BidirectionalConverter<&'a Input, &'a Output>
    + for<'a> BidirectionalConverter<&'a mut Input, &'a mut Output>
{
}

impl<I, O, B> OwnedOrRefsBidirectionalConverter<I, O> for B where
    B: BidirectionalConverter<I, O>
        + for<'a> BidirectionalConverter<&'a I, &'a O>
        + for<'a> BidirectionalConverter<&'a mut I, &'a mut O>
{
}

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct Uncloneable(u8);

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Cloneable(u16);

    #[test]
    fn owned_as_owned_or_cloned() {
        assert_eq!(Uncloneable(45).into_owned(), Uncloneable(45));
    }

    #[test]
    fn reference_as_owned_or_cloned() {
        let cloneable = Cloneable(32);
        let reference = &cloneable;
        let cloned: Cloneable = reference.into_owned();
        assert_eq!(cloned, cloneable);
    }

    #[test]
    fn bidirectional_converter_identity() {
        let a = ();
        assert_eq!(a.convert(123), 123);
        assert_eq!(a.convert_back(123), 123);
    }
}
