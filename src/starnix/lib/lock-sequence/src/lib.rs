// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tools for describing and enforcing lock acquisition order.
//!
//! To use these tools:
//! 1. A lock level must be defined for each type of lock. This can be a simple enum.
//! 2. Then a relation `LockedAfter` between these levels must be described,
//! forming a graph. This graph must be acyclic, since a cycle would indicate
//! a potential deadlock.
//! 3. Each time a lock is acquired, it must be done using an object of a `Locked<P>`
//! type, where `P` is any lock level that comes before the level `L` that is
//! associated with this lock. Doing so yields a new object of type `Locked<L>`
//! that can be used to acquire subsequent locks.
//! 3. Each place where a lock is used must be marked with the maximum lock level
//! that can be already acquired before attempting to acquire this lock. To do this,
//! it takes a special marker object `Locked<P>` where `P` is a lock level that
//! must come before the level associated in this lock in the graph. This object
//! is then used to acquire the lock, and a new object `Locked<L>` is returned, with
//! a new lock level `L` that comes after `P` in the lock ordering graph.
//!
//! ## Example
//! See also tests for this crate.
//!
//! ```
//! use std::sync::Mutex;
//! use lock_sequence::{impl_lock_after, lock_level, lock::LockFor, relation::LockAfter, Unlocked};
//!
//! #[derive(Default)]
//! struct HoldsLocks {
//!    a: Mutex<u8>,
//!    b: Mutex<u32>,
//! }
//!
//! pub enum A {}
//! pub enum B {}
//!
//! impl LockFor<LockA> for HoldsLocks {
//!    type Data = u8;
//!    type Guard<'l> = std::sync::MutexGuard<'l, u8>
//!        where Self: 'l;
//!    fn lock(&self) -> Self::Guard<'_> {
//!        self.a.lock().unwrap()
//!    }
//! }
//!
//! impl LockFor<LockB> for HoldsLocks {
//!    type Data = u32;
//!    type Guard<'l> = std::sync::MutexGuard<'l, u32>
//!        where Self: 'l;
//!    fn lock(&self) -> Self::Guard<'_> {
//!        self.b.lock().unwrap()
//!    }
//! }
//!
//! // LockA is the top of the lock hierarchy.
//! impl LockAfter<Unlocked> for LockA {}
//! // LockA can be acquired before LockB.
//! impl_lock_after!(LockA => LockB);
//!
//! // Accessing locked state looks like this:
//!
//! let state = HoldsLocks::default();
//! // Create a new lock session with the "root" lock level (empty tuple).
//! let mut locked = Unlocked::new();
//! // Access locked state.
//! let (a, mut locked_a) = locked.lock_and::<LockA, _>(&state);
//! let b = locked_a.lock::<LockB, _>(&state);
//! ```
//!
//! The [impl_lock_after] macro provides implementations of [LockAfter] for
//! a pair of locks. The complete lock ordering graph can be spelled out by
//! calling `impl_lock_after` for each parent and child in the hierarchy. One
//! of the upsides to using `impl_lock_after` is that it also prevents
//! accidental lock ordering inversion introduced while defining the graph.
//! This won't compile:
//! ```compile_fail
//! pub enum A {}
//! pub enum B {}
//!
//! impl_lock_after(LockA => LockB);
//! impl_lock_after(LockB => LockA);
//! ```
//!
//! The methods on [Locked] prevent out-of-order locking according to the
//! specified lock relationships.
//!
//! This won't compile because `LockB` does not implement `LockBefore<LockA>`:
//! ```compile_fail
//! # use std::sync::Mutex;
//! # use lock_order::{impl_lock_after, lock::LockFor, relation::LockAfter, Locked, Unlocked};
//! #
//! # #[derive(Default)]
//! # struct HoldsLocks {
//! #    a: Mutex<u8>,
//! #    b: Mutex<u32>,
//! # }
//! #
//! # pub enum A {}
//! # pub enum B {}
//! #
//! # impl LockFor<LockA> for HoldsLocks {
//! #    type Data = u8;
//! #    type Guard<'l> = std::sync::MutexGuard<'l, u8>
//! #        where Self: 'l;
//! #    fn lock(&self) -> Self::Guard<'_> {
//! #        self.a.lock().unwrap()
//! #    }
//! # }
//! #
//! # impl LockFor<LockB> for HoldsLocks {
//! #     type Data = u32;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u32>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.b.lock().unwrap()
//! #     }
//! # }
//! #
//! # // LockA is the top of the lock hierarchy.
//! # impl LockAfter<Unlocked> for LockA {}
//! # // LockA can be acquired before LockB.
//! # impl_lock_after!(LockA => LockB);
//!
//! let state = HoldsLocks::default();
//! let mut locked = Unlocked::new();
//!
//! // Locking B without A is fine, but locking A after B is not.
//! let (b, mut locked_b) = locked.lock_and::<LockB, _>(&state);
//! // compile error: LockB does not implement LockBefore<LockA>
//! let a = locked_b.lock::<LockA, _>(&state);
//! ```
//!
//! Even if the lock guard goes out of scope, the new `Locked` instance returned
//! by [Locked::lock_and] will prevent the original one from being used to
//! access state. This doesn't work:
//!
//! ```compile_fail
//! # use std::sync::Mutex;
//! # use lock_order::{impl_lock_after, lock::LockFor, relation::LockAfter, Locked, Unlocked};
//! #
//! # #[derive(Default)]
//! # struct HoldsLocks {
//! #     a: Mutex<u8>,
//! #     b: Mutex<u32>,
//! # }
//! #
//! # pub enum A {}
//! # pub enum B {}
//! #
//! # impl LockFor<LockA> for HoldsLocks {
//! #     type Data = u8;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u8>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.a.lock().unwrap()
//! #     }
//! # }
//! #
//! # impl LockFor<LockB> for HoldsLocks {
//! #     type Data = u32;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u32>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.b.lock().unwrap()
//! #     }
//! # }
//! #
//! # // LockA is the top of the lock hierarchy.
//! # impl LockAfter<Unlocked> for LockA {}
//! # // LockA can be acquired before LockB.
//! # impl_lock_after!(LockA => LockB);
//!
//! let state = HoldsLocks::default();
//! let mut locked = Unlocked::new();
//!
//! let (b, mut locked_b) = locked.lock_and::<LockB, _>();
//! drop(b);
//! let b = locked_b.lock::<LockB, _>(&state);
//! // Won't work; `locked` is mutably borrowed by `locked_b`.
//! let a = locked.lock::<LockA, _>(&state);
//! ```

pub mod lock;
pub mod relation;

use core::marker::PhantomData;

pub use crate::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
};

/// Enforcement mechanism for lock ordering.
///
/// `Locked` is a context that holds the lock level marker. Any state that
/// requires a lock to access should acquire this lock by calling `lock_and`
/// on a `Locked` object that is of an appropriate lock level. Acquiring
/// a lock in this way produces the guard and a new `Locked` instance
/// (with a different lock level) that mutably borrows from the original
/// instance. This means the original instance can't be used to acquire
/// new locks until the new instance leaves scope.
pub struct Locked<'a, L>(PhantomData<&'a L>);

/// "Highest" lock level
///
/// The lock level for the thing returned by `Locked::new`. Users of this crate
/// should implement `LockAfter<Unlocked>` for the root of any lock ordering
/// trees.
pub enum Unlocked {}

impl Unlocked {
    /// Entry point for locked access.
    ///
    /// `Unlocked` is the "root" lock level and can be acquired before any lock.
    pub fn new() -> Locked<'static, Unlocked> {
        Locked::<'static, Unlocked>(Default::default())
    }
}

// It's important that the lifetime on `Locked` here be anonymous. That means
// that the lifetimes in the returned `Locked` objects below are inferred to
// be the lifetimes of the references to self (mutable or immutable).
impl<L> Locked<'_, L> {
    /// Acquire the given lock.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock<'a, M, S>(&'a mut self, source: &'a S) -> S::Guard<'a>
    where
        M: 'a,
        S: LockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _) = self.lock_and::<M, S>(source);
        data
    }

    /// Acquire the given lock and a new locked context.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock_and<'a, M, S>(&'a mut self, source: &'a S) -> (S::Guard<'a>, Locked<'a, M>)
    where
        M: 'a,
        S: LockFor<M>,
        L: LockBefore<M>,
    {
        let data = S::lock(source);
        (data, Locked::<'a, M>(PhantomData::default()))
    }

    /// Acquire two locks that are on the same level, in a consistent order (sorted by memory address) and return both guards
    /// as well as the new locked context.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock_both_and<'a, M, S>(
        &'a mut self,
        source1: &'a S,
        source2: &'a S,
    ) -> (S::Guard<'a>, S::Guard<'a>, Locked<'a, M>)
    where
        M: 'a,
        S: LockFor<M>,
        L: LockBefore<M>,
    {
        let ptr1: *const S = source1;
        let ptr2: *const S = source2;
        if ptr1 < ptr2 {
            let data1 = S::lock(source1);
            let data2 = S::lock(source2);
            (data1, data2, Locked::<'a, M>(PhantomData::default()))
        } else {
            let data2 = S::lock(source2);
            let data1 = S::lock(source1);
            (data1, data2, Locked::<'a, M>(PhantomData::default()))
        }
    }
    /// Acquire two locks that are on the same level, in a consistent order (sorted by memory address) and return both guards.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock_both<'a, M, S>(
        &'a mut self,
        source1: &'a S,
        source2: &'a S,
    ) -> (S::Guard<'a>, S::Guard<'a>)
    where
        M: 'a,
        S: LockFor<M>,
        L: LockBefore<M>,
    {
        let (data1, data2, _) = self.lock_both_and(source1, source2);
        (data1, data2)
    }

    /// Attempt to acquire the given read lock and a new locked context.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn read_lock_and<'a, M, S>(&'a mut self, source: &'a S) -> (S::ReadGuard<'a>, Locked<'a, M>)
    where
        M: 'a,
        S: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let data = S::read_lock(source);
        (data, Locked::<'a, M>(PhantomData::default()))
    }

    /// Attempt to acquire the given read lock.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn read_lock<'a, M, S>(&'a mut self, source: &'a S) -> S::ReadGuard<'a>
    where
        M: 'a,
        S: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _) = self.read_lock_and::<M, S>(source);
        data
    }

    /// Attempt to acquire the given write lock and a new locked context.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn write_lock_and<'a, M, S>(
        &'a mut self,
        source: &'a S,
    ) -> (S::WriteGuard<'a>, Locked<'a, M>)
    where
        M: 'a,
        S: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let data = S::write_lock(source);
        (data, Locked::<'a, M>(PhantomData::default()))
    }

    /// Attempt to acquire the given write lock.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn write_lock<'a, M, S>(&'a mut self, source: &'a S) -> S::WriteGuard<'a>
    where
        M: 'a,
        S: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _) = self.write_lock_and::<M, S>(source);
        data
    }

    /// Restrict locking as if a lock was acquired.
    ///
    /// Like `lock_and` but doesn't actually acquire the lock `M`. This is
    /// safe because any locks that could be acquired with the lock `M` held can
    /// also be acquired without `M` being held.
    pub fn cast_locked<'a, M>(&'a mut self) -> Locked<'a, M>
    where
        M: 'a,
        L: LockBefore<M>,
    {
        Locked::<'a, M>(PhantomData::default())
    }
}

#[cfg(test)]
mod test {
    use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

    #[test]
    fn example() {
        extern crate self as lock_sequence;
        use crate::{impl_lock_after, relation::LockAfter, Unlocked};

        #[derive(Default)]
        pub struct HoldsLocks {
            a: Mutex<u8>,
            b: Mutex<u32>,
        }

        pub enum LockA {}
        pub enum LockB {}

        impl LockFor<LockA> for HoldsLocks {
            type Data = u8;
            type Guard<'l> = std::sync::MutexGuard<'l, u8>
                where Self: 'l;
            fn lock(&self) -> Self::Guard<'_> {
                self.a.lock().unwrap()
            }
        }

        impl LockFor<LockB> for HoldsLocks {
            type Data = u32;
            type Guard<'l> = std::sync::MutexGuard<'l, u32>
                where Self: 'l;
            fn lock(&self) -> Self::Guard<'_> {
                self.b.lock().unwrap()
            }
        }

        // LockA is the top of the lock hierarchy.
        impl LockAfter<Unlocked> for LockA {}
        // LockA can be acquired before LockB.
        impl_lock_after!(LockA => LockB);

        // Accessing locked state looks like this:

        let state = HoldsLocks::default();
        // Create a new lock session with the "root" lock level (empty tuple).
        let mut locked = Unlocked::new();
        // Access locked state.
        let (_a, mut locked_a) = locked.lock_and::<LockA, _>(&state);
        let _b = locked_a.lock::<LockB, _>(&state);
    }

    mod lock_levels {
        //! Lock ordering tree:
        //! A -> B -> {C, D, E -> F, G -> H}

        extern crate self as lock_sequence;

        use crate::{impl_lock_after, relation::LockAfter, Unlocked};

        pub enum A {}
        pub enum B {}
        pub enum C {}
        pub enum D {}
        pub enum E {}
        pub enum F {}
        pub enum G {}
        pub enum H {}

        impl LockAfter<Unlocked> for A {}
        impl_lock_after!(A => B);
        impl_lock_after!(B => C);
        impl_lock_after!(B => D);
        impl_lock_after!(B => E);
        impl_lock_after!(E => F);
        impl_lock_after!(B => G);
        impl_lock_after!(G => H);
    }

    use crate::{
        lock::{LockFor, RwLockFor},
        Unlocked,
    };
    use lock_levels::{A, B, C, D, E, F, G, H};

    /// Data type with multiple locked fields.
    #[derive(Default)]
    pub struct Data {
        a: Mutex<u8>,
        b: Mutex<u16>,
        c: Mutex<u64>,
        d: RwLock<u128>,
        e: Mutex<Mutex<u8>>,
        g: Mutex<Vec<Mutex<u8>>>,
        u: usize,
    }

    impl LockFor<A> for Data {
        type Data = u8;
        type Guard<'l> = MutexGuard<'l, u8>;
        fn lock(&self) -> Self::Guard<'_> {
            self.a.lock().unwrap()
        }
    }

    impl LockFor<B> for Data {
        type Data = u16;
        type Guard<'l> = MutexGuard<'l, u16>;
        fn lock(&self) -> Self::Guard<'_> {
            self.b.lock().unwrap()
        }
    }

    impl LockFor<C> for Data {
        type Data = u64;
        type Guard<'l> = MutexGuard<'l, u64>;
        fn lock(&self) -> Self::Guard<'_> {
            self.c.lock().unwrap()
        }
    }

    impl RwLockFor<D> for Data {
        type Data = u128;
        type ReadGuard<'l> = RwLockReadGuard<'l, u128>;
        type WriteGuard<'l> = RwLockWriteGuard<'l, u128>;
        fn read_lock(&self) -> Self::ReadGuard<'_> {
            self.d.read().unwrap()
        }
        fn write_lock(&self) -> Self::WriteGuard<'_> {
            self.d.write().unwrap()
        }
    }

    impl LockFor<E> for Data {
        type Data = Mutex<u8>;
        type Guard<'l> = MutexGuard<'l, Mutex<u8>>;
        fn lock(&self) -> Self::Guard<'_> {
            self.e.lock().unwrap()
        }
    }

    impl LockFor<F> for Mutex<u8> {
        type Data = u8;
        type Guard<'l> = MutexGuard<'l, u8>;
        fn lock(&self) -> Self::Guard<'_> {
            self.lock().unwrap()
        }
    }

    impl LockFor<G> for Data {
        type Data = Vec<Mutex<u8>>;
        type Guard<'l> = MutexGuard<'l, Vec<Mutex<u8>>>;
        fn lock(&self) -> Self::Guard<'_> {
            self.g.lock().unwrap()
        }
    }

    impl LockFor<H> for Mutex<u8> {
        type Data = u8;
        type Guard<'l> = MutexGuard<'l, u8>;
        fn lock(&self) -> Self::Guard<'_> {
            self.lock().unwrap()
        }
    }

    #[derive(Debug)]
    struct NotPresent;

    #[test]
    fn lock_a_then_c() {
        let data = Data::default();

        let mut w = Unlocked::new();
        let (_a, mut wa) = w.lock_and::<A, _>(&data);
        let (_c, _wc) = wa.lock_and::<C, _>(&data);
        // This won't compile!
        // let _b = _wc.lock::<B, _>(&data);
    }

    #[test]
    fn cast_a_then_c() {
        let data = Data::default();

        let mut w = Unlocked::new();
        let mut wa = w.cast_locked::<A>();
        let (_c, _wc) = wa.lock_and::<C, _>(&data);
        // This should not compile:
        // let _b = w.lock::<B, _>(&data);
    }

    #[test]
    fn unlocked_access_does_not_prevent_locking() {
        let data = Data { a: Mutex::new(15), u: 34, ..Data::default() };

        let mut locked = Unlocked::new();
        let u = &data.u;

        // Prove that `u` does not prevent locked state from being accessed.
        let a = locked.lock::<A, _>(&data);

        assert_eq!(u, &34);
        assert_eq!(&*a, &15);
    }

    #[test]
    fn nested_locks() {
        let data = Data { e: Mutex::new(Mutex::new(1)), ..Data::default() };

        let mut locked = Unlocked::new();
        let (e, mut next_locked) = locked.lock_and::<E, _>(&data);
        let v = next_locked.lock::<F, _>(&*e);
        assert_eq!(*v, 1);
    }

    #[test]
    fn rw_lock() {
        let data = Data { d: RwLock::new(1), ..Data::default() };

        let mut locked = Unlocked::new();
        {
            let mut d = locked.write_lock::<D, _>(&data);
            *d = 10;
        }
        let d = locked.read_lock::<D, _>(&data);
        assert_eq!(*d, 10);
    }

    #[test]
    fn collections() {
        let data = Data { g: Mutex::new(vec![Mutex::new(0), Mutex::new(1)]), ..Data::default() };

        let mut locked = Unlocked::new();
        let (g, mut next_locked) = locked.lock_and::<G, _>(&data);
        let v = next_locked.lock::<H, _>(&g[1]);
        assert_eq!(*v, 1);
    }

    #[test]
    fn lock_same_level() {
        let data1 = Data { a: Mutex::new(5), b: Mutex::new(15), ..Data::default() };
        let data2 = Data { a: Mutex::new(10), b: Mutex::new(20), ..Data::default() };
        let mut locked = Unlocked::new();
        {
            let (a1, a2, mut new_locked) = locked.lock_both_and::<A, _>(&data1, &data2);
            assert_eq!(*a1, 5);
            assert_eq!(*a2, 10);
            let (b1, b2) = new_locked.lock_both::<B, _>(&data1, &data2);
            assert_eq!(*b1, 15);
            assert_eq!(*b2, 20);
        }
        {
            let (a2, a1) = locked.lock_both::<A, _>(&data2, &data1);
            assert_eq!(*a1, 5);
            assert_eq!(*a2, 10);
        }
    }
}
