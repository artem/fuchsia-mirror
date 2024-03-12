// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Execution contexts.
//!
//! This module defines "context" traits, which allow code in this crate to be
//! written agnostic to their execution context.
//!
//! All of the code in this crate operates in terms of "events". When an event
//! occurs (for example, a packet is received, an application makes a request,
//! or a timer fires), a function is called to handle that event. In response to
//! that event, the code may wish to emit new events (for example, to send a
//! packet, to respond to an application request, or to install a new timer).
//! The traits in this module provide the ability to emit new events. For
//! example, if, in order to handle some event, we need the ability to install
//! new timers, then the function to handle that event would take a
//! [`TimerContext`] parameter, which it could use to install new timers.
//!
//! Structuring code this way allows us to write code which is agnostic to
//! execution context - a test fake or any number of possible "real-world"
//! implementations of these traits all appear as indistinguishable, opaque
//! trait implementations to our code.
//!
//! The benefits are deeper than this, though. Large units of code can be
//! subdivided into smaller units that view each other as "contexts". For
//! example, the ARP implementation in the [`crate::device::arp`] module defines
//! the [`ArpContext`] trait, which is an execution context for ARP operations.
//! It is implemented both by the test fakes in that module, and also by the
//! Ethernet device implementation in the [`crate::device::ethernet`] module.
//!
//! This subdivision of code into small units in turn enables modularity. If,
//! for example, the IP code sees transport layer protocols as execution
//! contexts, then customizing which transport layer protocols are supported is
//! just a matter of providing a different implementation of the transport layer
//! context traits (this isn't what we do today, but we may in the future).

use core::{convert::Infallible as Never, ffi::CStr, fmt::Debug, time::Duration};

use lock_order::Unlocked;

use packet::{BufferMut, Serializer};
use rand::{CryptoRng, RngCore};

use crate::{
    counters::Counter,
    marker::{BindingsContext, BindingsTypes},
    state::StackState,
    sync, Instant,
};

pub use netstack3_base::{ContextPair, InstantBindingsTypes, InstantContext};

/// A marker trait indicating that the implementor is not the [`FakeCoreCtx`]
/// type found in test environments.
///
/// See [this issue] for details on why this is needed.
///
/// [`FakeCoreCtx`]: testutil::FakeCoreCtx
/// [this issue]: https://github.com/rust-lang/rust/issues/97811
pub(crate) trait NonTestCtxMarker {}

impl<BC: BindingsContext, L> NonTestCtxMarker for CoreCtx<'_, BC, L> {}

/// An [`InstantContext`] which stores a cached value for the current time.
///
/// `CachedInstantCtx`s are constructed via [`new_cached_instant_context`].
pub(crate) struct CachedInstantCtx<I>(I);

impl<I: Instant + 'static> InstantBindingsTypes for CachedInstantCtx<I> {
    type Instant = I;
}

impl<I: Instant + 'static> InstantContext for CachedInstantCtx<I> {
    fn now(&self) -> I {
        self.0.clone()
    }
}

/// Construct a new `CachedInstantCtx` from the current time.
///
/// This is a hack until we figure out a strategy for splitting context objects.
/// Currently, since most context methods take a `&mut self` argument, lifetimes
/// which don't need to conflict in principle - such as the lifetime of state
/// obtained mutably from [`StateContext`] and the lifetime required to call the
/// [`InstantContext::now`] method on the same object - do conflict, and thus
/// cannot overlap. Until we figure out an approach to deal with that problem,
/// this exists as a workaround.
pub(crate) fn new_cached_instant_context<I: InstantContext + ?Sized>(
    bindings_ctx: &I,
) -> CachedInstantCtx<I::Instant> {
    CachedInstantCtx(bindings_ctx.now())
}

/// A context that supports scheduling timers.
pub trait TimerContext<Id>: InstantContext {
    /// Schedule a timer to fire after some duration.
    ///
    /// `schedule_timer` schedules the given timer to be fired after `duration`
    /// has elapsed, overwriting any previous timer with the same ID.
    ///
    /// If there was previously a timer with that ID, return the time at which
    /// is was scheduled to fire.
    ///
    /// # Panics
    ///
    /// `schedule_timer` may panic if `duration` is large enough that
    /// `self.now() + duration` overflows.
    fn schedule_timer(&mut self, duration: Duration, id: Id) -> Option<Self::Instant> {
        self.schedule_timer_instant(self.now().checked_add(duration).unwrap(), id)
    }

    /// Schedule a timer to fire at some point in the future.
    ///
    /// `schedule_timer` schedules the given timer to be fired at `time`,
    /// overwriting any previous timer with the same ID.
    ///
    /// If there was previously a timer with that ID, return the time at which
    /// is was scheduled to fire.
    fn schedule_timer_instant(&mut self, time: Self::Instant, id: Id) -> Option<Self::Instant>;

    /// Cancel a timer.
    ///
    /// If a timer with the given ID exists, it is canceled and the instant at
    /// which it was scheduled to fire is returned.
    fn cancel_timer(&mut self, id: Id) -> Option<Self::Instant>;

    /// Cancel all timers which satisfy a predicate.
    ///
    /// `cancel_timers_with` calls `f` on each scheduled timer, and cancels any
    /// timer for which `f` returns true.
    fn cancel_timers_with<F: FnMut(&Id) -> bool>(&mut self, f: F);

    /// Get the instant a timer will fire, if one is scheduled.
    ///
    /// Returns the [`Instant`] a timer with ID `id` will be invoked. If no
    /// timer with the given ID exists, `scheduled_instant` will return `None`.
    fn scheduled_instant(&self, id: Id) -> Option<Self::Instant>;
}

/// A handler for timer firing events.
///
/// A `TimerHandler` is a type capable of handling the event of a timer firing.
pub trait TimerHandler<BC, Id> {
    /// Handle a timer firing.
    fn handle_timer(&mut self, bindings_ctx: &mut BC, id: Id);
}

// NOTE:
// - Code in this crate is required to only obtain random values through an
//   `RngContext`. This allows a deterministic RNG to be provided when useful
//   (for example, in tests).
// - The CSPRNG requirement exists so that random values produced within the
//   network stack are not predictable by outside observers. This helps prevent
//   certain kinds of fingerprinting and denial of service attacks.

/// A context that provides a random number generator (RNG).
pub trait RngContext {
    // TODO(joshlf): If the CSPRNG requirement becomes a performance problem,
    // introduce a second, non-cryptographically secure, RNG.

    /// The random number generator (RNG) provided by this `RngContext`.
    ///
    /// The provided RNG must be cryptographically secure, and users may rely on
    /// that property for their correctness and security.
    type Rng<'a>: RngCore + CryptoRng
    where
        Self: 'a;

    /// Gets the random number generator (RNG).
    fn rng(&mut self) -> Self::Rng<'_>;
}

/// A context for receiving frames.
pub trait RecvFrameContext<BC, Meta> {
    /// Receive a frame.
    ///
    /// `receive_frame` receives a frame with the given metadata.
    fn receive_frame<B: BufferMut>(&mut self, bindings_ctx: &mut BC, metadata: Meta, frame: B);
}

/// Any type can implement a receive frame context with uninstantiable metadata.
impl<T, BC> RecvFrameContext<BC, Never> for T {
    fn receive_frame<B: BufferMut>(&mut self, _bindings_ctx: &mut BC, metadata: Never, _frame: B) {
        match metadata {}
    }
}

/// A context for sending frames.
pub trait SendFrameContext<BC, Meta> {
    // TODO(joshlf): Add an error type parameter or associated type once we need
    // different kinds of errors.

    /// Send a frame.
    ///
    /// `send_frame` sends a frame with the given metadata. The frame itself is
    /// passed as a [`Serializer`] which `send_frame` is responsible for
    /// serializing. If serialization fails for any reason, the original,
    /// unmodified `Serializer` is returned.
    ///
    /// [`Serializer`]: packet::Serializer
    fn send_frame<S>(&mut self, bindings_ctx: &mut BC, metadata: Meta, frame: S) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

/// A context that stores counters.
///
/// `CounterContext` exposes access to counters for observation and debugging.
pub trait CounterContext<T> {
    /// Call the function with an immutable reference to counter type T.
    fn with_counters<O, F: FnOnce(&T) -> O>(&self, cb: F) -> O;

    /// Increments the counter returned by the callback.
    fn increment<F: FnOnce(&T) -> &Counter>(&self, cb: F) {
        self.with_counters(|counters| cb(counters).increment());
    }
}

/// A context that provides access to per-resource counters for observation and
/// debugging.
pub trait ResourceCounterContext<R, T>: CounterContext<T> {
    /// Call `cb` with an immutable reference to the set of counters on `resource`.
    fn with_per_resource_counters<O, F: FnOnce(&T) -> O>(&mut self, resource: &R, cb: F) -> O;

    /// Increments both the per-resource and stackwide versions of
    /// the counter returned by the callback.
    fn increment<F: Fn(&T) -> &Counter>(&mut self, resource: &R, cb: F) {
        self.with_per_resource_counters(resource, |counters| cb(counters).increment());
        self.with_counters(|counters| cb(counters).increment());
    }
}

/// A context for emitting events.
///
/// `EventContext` encodes the common pattern for emitting atomic events of type
/// `T` from core. An implementation of `EventContext` must guarantee that
/// events are processed in the order they are emitted.
pub trait EventContext<T> {
    /// Handles `event`.
    fn on_event(&mut self, event: T);
}

/// A context for emitting tracing data.
pub trait TracingContext {
    /// The scope of a trace duration.
    ///
    /// Its lifetime corresponds to the beginning and end of the duration.
    type DurationScope;

    /// Writes a duration event which ends when the returned scope is dropped.
    ///
    /// Durations describe work which is happening synchronously on one thread.
    /// Care should be taken to avoid a duration's scope spanning an `await`
    /// point in asynchronous code.
    fn duration(&self, name: &'static CStr) -> Self::DurationScope;
}

/// A context trait determining the types to be used for reference notifications.
pub trait ReferenceNotifiers {
    /// The receiver for shared reference destruction notifications.
    type ReferenceReceiver<T: 'static>: 'static;
    /// The notifier for shared reference destruction notifications.
    type ReferenceNotifier<T: Send + 'static>: sync::RcNotifier<T> + 'static;

    /// Creates a new Notifier/Receiver pair for `T`.
    ///
    /// `debug_references` is given to provide information on outstanding
    /// references that caused the notifier to be requested.
    fn new_reference_notifier<T: Send + 'static, D: Debug>(
        debug_references: D,
    ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>);
}

/// Provides access to core context implementations.
///
/// `L` is the current lock level of `CoreCtx`. The alias [`UnlockedCoreCtx`] is
/// provided at the [`Unlocked`] level.
pub type CoreCtx<'a, BT, L> = Locked<&'a StackState<BT>, L>;

pub(crate) type CoreCtxAndResource<'a, BT, R, L> =
    Locked<lock_order::OwnedTupleWrapper<&'a StackState<BT>, &'a R>, L>;

/// An alias for an unlocked [`CoreCtx`].
pub type UnlockedCoreCtx<'a, BT> = CoreCtx<'a, BT, Unlocked>;

pub(crate) use locked::Locked;

/// A type that provides a context implementation.
///
/// This trait allows for [`CtxPair`] to hold context implementations
/// agnostically of the storage method and how they're implemented. For example,
/// tests usually create API structs with a mutable borrow to contexts, while
/// the core context exposed to bindings is implemented on an owned [`CoreCtx`]
/// type.
///
/// The shape of this trait is equivalent to [`core::ops::DerefMut`] but we
/// decide against using that because of the automatic dereferencing semantics
/// the compiler provides around implementers of `DerefMut`.
pub trait ContextProvider {
    /// The context provided by this `ContextProvider`.
    type Context: Sized;

    /// Gets a mutable borrow to this context.
    fn context(&mut self) -> &mut Self::Context;
}

impl<'a, T: Sized> ContextProvider for &'a mut T {
    type Context = T;

    fn context(&mut self) -> &mut Self::Context {
        &mut *self
    }
}

impl<'a, BT, L> ContextProvider for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    type Context = Self;

    fn context(&mut self) -> &mut Self::Context {
        self
    }
}

/// A concrete implementation of [`ContextPair`].
///
///
/// `CtxPair` provides a [`ContextPair`] implementation when `CC` and `BC` are
/// [`ContextProvider`] and using their respective targets as the `CoreContext`
/// and `BindingsContext` associated types.
pub struct CtxPair<CC, BC> {
    pub(crate) core_ctx: CC,
    pub(crate) bindings_ctx: BC,
}

impl<CC, BC> ContextPair for CtxPair<CC, BC>
where
    CC: ContextProvider,
    BC: ContextProvider,
{
    type CoreContext = CC::Context;
    type BindingsContext = BC::Context;

    fn contexts(&mut self) -> (&mut Self::CoreContext, &mut Self::BindingsContext) {
        let Self { core_ctx, bindings_ctx } = self;
        (core_ctx.context(), bindings_ctx.context())
    }
}

/// Provides a crate-local wrapper for `[lock_order::Locked]`.
///
/// This module is intentionally private so usage is limited to the type alias
/// in [`CoreCtx`].
mod locked {
    use super::{BindingsTypes, CoreCtx, StackState};

    use core::ops::Deref;
    use lock_order::{wrap::LockedWrapper, Locked as ExternalLocked, TupleWrapper, Unlocked};

    /// A crate-local wrapper on [`lock_order::Locked`].
    pub struct Locked<T, L>(ExternalLocked<T, L>);

    impl<T, L> LockedWrapper<T, L> for Locked<T, L>
    where
        T: Deref,
        T::Target: Sized,
    {
        type AtLockLevel<'l, M> = Locked<&'l T::Target, M>
    where
        M: 'l,
        T: 'l;

        type CastWrapper<X> = Locked<X, L>
    where
        X: Deref,
        X::Target: Sized;

        fn wrap<'l, M>(locked: ExternalLocked<&'l T::Target, M>) -> Self::AtLockLevel<'l, M>
        where
            M: 'l,
            T: 'l,
        {
            Locked(locked)
        }

        fn wrap_cast<R: Deref>(locked: ExternalLocked<R, L>) -> Self::CastWrapper<R>
        where
            R::Target: Sized,
        {
            Locked(locked)
        }

        fn get_mut(&mut self) -> &mut ExternalLocked<T, L> {
            let Self(locked) = self;
            locked
        }

        fn get(&self) -> &ExternalLocked<T, L> {
            let Self(locked) = self;
            locked
        }
    }

    impl<'a, BT: BindingsTypes> CoreCtx<'a, BT, Unlocked> {
        /// Creates a new `CoreCtx` from a borrowed [`StackState`].
        pub fn new(stack_state: &'a StackState<BT>) -> Self {
            Self(ExternalLocked::new(stack_state))
        }
    }

    impl<'a, BT, R, L, T> Locked<T, L>
    where
        R: 'a,
        T: Deref<Target = TupleWrapper<&'a StackState<BT>, &'a R>>,
        BT: BindingsTypes,
    {
        pub(crate) fn cast_resource(&mut self) -> Locked<&'_ R, L> {
            let Self(locked) = self;
            Locked(locked.cast_with(|c| c.right()))
        }

        pub(crate) fn cast_core_ctx(&mut self) -> CoreCtx<'_, BT, L> {
            let Self(locked) = self;
            crate::CoreCtx::<BT, L>::wrap(locked.cast_with(|c| c.left()))
        }
    }
}

/// Fake implementations of context traits.
///
/// Each trait `Xxx` has a fake called `FakeXxx`. `FakeXxx` implements `Xxx`,
/// and `impl<T> FakeXxx for T` where either `T: AsRef<FakeXxx>` or `T:
/// AsMut<FakeXxx>` or both (depending on the trait). This allows fake
/// implementations to be composed easily - any container type need only provide
/// the appropriate `AsRef` and/or `AsMut` implementations, and the blanket impl
/// will take care of the rest.
#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::{boxed::Box, collections::BinaryHeap, format, string::String, sync::Arc, vec::Vec};
    #[cfg(test)]
    use alloc::{collections::HashMap, vec};
    use core::{convert::Infallible as Never, fmt::Debug, hash::Hash};
    #[cfg(test)]
    use core::{marker::PhantomData, ops};

    #[cfg(test)]
    use assert_matches::assert_matches;
    use derivative::Derivative;

    use net_types::ip::IpVersion;
    use packet::Buf;
    use rand_xorshift::XorShiftRng;

    use super::*;
    use crate::{
        data_structures::ref_counted_hash_map::{RefCountedHashSet, RemoveResult},
        device::{link::LinkDevice, pure_ip::PureIpWeakDeviceId, DeviceLayerTypes},
        filter::FilterBindingsTypes,
        ip::device::nud::{LinkResolutionContext, LinkResolutionNotifier},
        sync::Mutex,
        testutil::FakeCryptoRng,
    };
    #[cfg(test)]
    use crate::{
        device::{EthernetDeviceId, EthernetWeakDeviceId},
        testutil::DispatchedFrame,
    };

    pub use netstack3_base::testutil::{FakeInstant, FakeInstantCtx};

    /// Arbitrary data of type `D` attached to a `FakeInstant`.
    ///
    /// `InstantAndData` implements `Ord` and `Eq` to be used in a `BinaryHeap`
    /// and ordered by `FakeInstant`.
    #[derive(Clone, Debug)]
    pub(crate) struct InstantAndData<D>(pub(crate) FakeInstant, pub(crate) D);

    impl<D> InstantAndData<D> {
        pub(crate) fn new(time: FakeInstant, data: D) -> Self {
            Self(time, data)
        }
    }

    impl<D> Eq for InstantAndData<D> {}

    impl<D> PartialEq for InstantAndData<D> {
        fn eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }

    impl<D> Ord for InstantAndData<D> {
        fn cmp(&self, other: &Self) -> core::cmp::Ordering {
            other.0.cmp(&self.0)
        }
    }

    impl<D> PartialOrd for InstantAndData<D> {
        fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    /// A fake [`TimerContext`] which stores time as a [`FakeInstantCtx`].
    pub(crate) struct FakeTimerCtx<Id> {
        pub(crate) instant: FakeInstantCtx,
        timers: BinaryHeap<InstantAndData<Id>>,
    }

    impl<Id> Default for FakeTimerCtx<Id> {
        fn default() -> FakeTimerCtx<Id> {
            FakeTimerCtx { instant: FakeInstantCtx::default(), timers: BinaryHeap::default() }
        }
    }

    impl<Id> AsMut<FakeTimerCtx<Id>> for FakeTimerCtx<Id> {
        fn as_mut(&mut self) -> &mut FakeTimerCtx<Id> {
            self
        }
    }

    impl<Id: Clone> FakeTimerCtx<Id> {
        /// Get an ordered list of all currently-scheduled timers.
        #[cfg(test)]
        pub(crate) fn timers(&self) -> Vec<(FakeInstant, Id)> {
            self.timers
                .clone()
                .into_sorted_vec()
                .into_iter()
                .map(|InstantAndData(i, id)| (i, id))
                .collect()
        }
    }

    #[cfg(test)]
    impl<Id: Debug + Clone + Hash + Eq> FakeTimerCtx<Id> {
        /// Asserts that `self` contains exactly the timers in `timers`.
        ///
        /// # Panics
        ///
        /// Panics if `timers` contains the same ID more than once or if `self`
        /// does not contain exactly the timers in `timers`.
        ///
        /// [`RangeBounds<FakeInstant>`]: core::ops::RangeBounds
        #[track_caller]
        pub(crate) fn assert_timers_installed<I: IntoIterator<Item = (Id, FakeInstant)>>(
            &self,
            timers: I,
        ) {
            self.assert_timers_installed_range(
                timers.into_iter().map(|(id, instant)| (id, instant..=instant)),
            );
        }

        /// Like [`assert_timers_installed`] but receives a range instants to
        /// match.
        ///
        /// Each timer must be present, and its deadline must fall into the
        /// specified range.
        #[track_caller]
        pub(crate) fn assert_timers_installed_range<
            R: ops::RangeBounds<FakeInstant> + Debug,
            I: IntoIterator<Item = (Id, R)>,
        >(
            &self,
            timers: I,
        ) {
            self.assert_timers_installed_inner(timers, true);
        }

        /// Asserts that `self` contains at least the timers in `timers`.
        ///
        /// Like [`assert_timers_installed`], but only asserts that `timers` is
        /// a subset of the timers installed; other timers may be installed in
        /// addition to those in `timers`.
        #[track_caller]
        pub(crate) fn assert_some_timers_installed<I: IntoIterator<Item = (Id, FakeInstant)>>(
            &self,
            timers: I,
        ) {
            self.assert_some_timers_installed_range(
                timers.into_iter().map(|(id, instant)| (id, instant..=instant)),
            );
        }

        /// Like [`assert_some_timers_installed`] but receives instant ranges
        /// to match like [`assert_timers_installed_range`].
        #[track_caller]
        pub(crate) fn assert_some_timers_installed_range<
            R: ops::RangeBounds<FakeInstant> + Debug,
            I: IntoIterator<Item = (Id, R)>,
        >(
            &self,
            timers: I,
        ) {
            self.assert_timers_installed_inner(timers, false);
        }

        /// Asserts that no timers are installed.
        ///
        /// # Panics
        ///
        /// Panics if any timers are installed.
        #[track_caller]
        pub(crate) fn assert_no_timers_installed(&self) {
            self.assert_timers_installed([]);
        }

        #[track_caller]
        fn assert_timers_installed_inner<
            R: ops::RangeBounds<FakeInstant> + Debug,
            I: IntoIterator<Item = (Id, R)>,
        >(
            &self,
            timers: I,
            exact: bool,
        ) {
            let mut timers = timers.into_iter().fold(HashMap::new(), |mut timers, (id, range)| {
                assert_matches!(timers.insert(id, range), None);
                timers
            });

            enum Error<Id, R: ops::RangeBounds<FakeInstant>> {
                ExpectedButMissing { id: Id, range: R },
                UnexpectedButPresent { id: Id, instant: FakeInstant },
                UnexpectedInstant { id: Id, range: R, instant: FakeInstant },
            }

            let mut errors = Vec::new();

            // Make sure that all installed timers were expected (present in
            // `timers`).
            for InstantAndData(instant, id) in self.timers.iter().cloned() {
                match timers.remove(&id) {
                    None => {
                        if exact {
                            errors.push(Error::UnexpectedButPresent { id, instant })
                        }
                    }
                    Some(range) => {
                        if !range.contains(&instant) {
                            errors.push(Error::UnexpectedInstant { id, range, instant })
                        }
                    }
                }
            }

            // Make sure that all expected timers were already found in
            // `self.timers` (and removed from `timers`).
            errors
                .extend(timers.drain().map(|(id, range)| Error::ExpectedButMissing { id, range }));

            if errors.len() > 0 {
                let mut s = String::from("Unexpected timer contents:");
                for err in errors {
                    s += &match err {
                        Error::ExpectedButMissing { id, range } => {
                            format!("\n\tMissing timer {:?} with deadline {:?}", id, range)
                        }
                        Error::UnexpectedButPresent { id, instant } => {
                            format!("\n\tUnexpected timer {:?} with deadline {:?}", id, instant)
                        }
                        Error::UnexpectedInstant { id, range, instant } => format!(
                            "\n\tTimer {:?} has unexpected deadline {:?} (wanted {:?})",
                            id, instant, range
                        ),
                    };
                }
                panic!("{}", s);
            }
        }
    }

    impl<Id> AsRef<FakeInstantCtx> for FakeTimerCtx<Id> {
        fn as_ref(&self) -> &FakeInstantCtx {
            &self.instant
        }
    }

    impl<Id: PartialEq> FakeTimerCtx<Id> {
        // Just like `TimerContext::cancel_timer`, but takes a reference to `Id`
        // rather than a value. This allows us to implement
        // `schedule_timer_instant`, which needs to retain ownership of the
        // `Id`.
        fn cancel_timer_inner(&mut self, id: &Id) -> Option<FakeInstant> {
            let mut r: Option<FakeInstant> = None;
            // NOTE(brunodalbo): Cancelling timers can be made a faster than
            // this if we keep two data structures and require that `Id: Hash`.
            self.timers = self
                .timers
                .drain()
                .filter(|t| {
                    if &t.1 == id {
                        r = Some(t.0);
                        false
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .into();
            r
        }
    }

    impl<Id: PartialEq> TimerContext<Id> for FakeTimerCtx<Id> {
        fn schedule_timer_instant(&mut self, time: FakeInstant, id: Id) -> Option<FakeInstant> {
            let ret = self.cancel_timer_inner(&id);
            self.timers.push(InstantAndData::new(time, id));
            ret
        }

        fn cancel_timer(&mut self, id: Id) -> Option<FakeInstant> {
            self.cancel_timer_inner(&id)
        }

        fn cancel_timers_with<F: FnMut(&Id) -> bool>(&mut self, mut f: F) {
            self.timers = self.timers.drain().filter(|t| !f(&t.1)).collect::<Vec<_>>().into();
        }

        fn scheduled_instant(&self, id: Id) -> Option<FakeInstant> {
            self.timers.iter().find_map(|x| if x.1 == id { Some(x.0) } else { None })
        }
    }

    /// Adds methods for interacting with [`FakeTimerCtx`] and its wrappers.
    pub trait FakeTimerCtxExt<Id>: Sized {
        /// Triggers the next timer, if any, by using the provided `handler`.
        ///
        /// `trigger_next_timer` triggers the next timer, if any, advances the
        /// internal clock to the timer's scheduled time, and returns its ID.
        fn trigger_next_timer<H: TimerHandler<Self, Id>>(&mut self, handler: &mut H) -> Option<Id>;

        /// Skips the current time forward until `instant`, triggering all
        /// timers until then, inclusive, by calling `f` on them.
        ///
        /// Returns the timers which were triggered.
        ///
        /// # Panics
        ///
        /// Panics if `instant` is in the past.
        fn trigger_timers_until_instant<H: TimerHandler<Self, Id>>(
            &mut self,
            instant: FakeInstant,
            handler: &mut H,
        ) -> Vec<Id>;

        /// Skips the current time forward by `duration`, triggering all timers
        /// until then, inclusive, by passing them to the `handler`.
        ///
        /// Returns the timers which were triggered.
        fn trigger_timers_for<H: TimerHandler<Self, Id>>(
            &mut self,
            duration: Duration,
            handler: &mut H,
        ) -> Vec<Id>;

        /// Triggers timers and expects them to be the given timers.
        ///
        /// The number of timers to be triggered is taken to be the number of
        /// timers produced by `timers`. Timers may be triggered in any order.
        ///
        /// # Panics
        ///
        /// Panics under the following conditions:
        /// - Fewer timers could be triggered than expected
        /// - Timers were triggered that were not expected
        /// - Timers that were expected were not triggered
        #[track_caller]
        fn trigger_timers_and_expect_unordered<
            I: IntoIterator<Item = Id>,
            H: TimerHandler<Self, Id>,
        >(
            &mut self,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq;

        /// Triggers timers until `instant` and expects them to be the given
        /// timers.
        ///
        /// Like `trigger_timers_and_expect_unordered`, except that timers will
        /// only be triggered until `instant` (inclusive).
        fn trigger_timers_until_and_expect_unordered<
            I: IntoIterator<Item = Id>,
            H: TimerHandler<Self, Id>,
        >(
            &mut self,
            instant: FakeInstant,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq;

        /// Triggers timers for `duration` and expects them to be the given
        /// timers.
        ///
        /// Like `trigger_timers_and_expect_unordered`, except that timers will
        /// only be triggered for `duration` (inclusive).
        fn trigger_timers_for_and_expect<I: IntoIterator<Item = Id>, H: TimerHandler<Self, Id>>(
            &mut self,
            duration: Duration,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq;
    }

    // TODO(https://fxbug.dev/42081080): hold lock on `FakeTimerCtx` across entire
    // method to avoid potential race conditions.
    impl<Id: Clone, Ctx: WithFakeTimerContext<Id>> FakeTimerCtxExt<Id> for Ctx {
        /// Triggers the next timer, if any, by calling `f` on it.
        ///
        /// `trigger_next_timer` triggers the next timer, if any, advances the
        /// internal clock to the timer's scheduled time, and returns its ID.
        fn trigger_next_timer<H: TimerHandler<Self, Id>>(&mut self, handler: &mut H) -> Option<Id> {
            self.with_fake_timer_ctx_mut(|timers| {
                timers.timers.pop().map(|InstantAndData(t, id)| {
                    timers.instant.time = t;
                    id
                })
            })
            .map(|id| {
                handler.handle_timer(self, id.clone());
                id
            })
        }

        /// Skips the current time forward until `instant`, triggering all
        /// timers until then, inclusive, by giving them to `handler`.
        ///
        /// Returns the timers which were triggered.
        ///
        /// # Panics
        ///
        /// Panics if `instant` is in the past.
        fn trigger_timers_until_instant<H: TimerHandler<Self, Id>>(
            &mut self,
            instant: FakeInstant,
            handler: &mut H,
        ) -> Vec<Id> {
            assert!(instant >= self.with_fake_timer_ctx(|ctx| ctx.now()));
            let mut timers = Vec::new();

            while self.with_fake_timer_ctx_mut(|ctx| {
                ctx.timers.peek().map(|InstantAndData(i, _id)| i <= &instant).unwrap_or(false)
            }) {
                timers.push(self.trigger_next_timer(handler).unwrap())
            }

            self.with_fake_timer_ctx_mut(|ctx| {
                assert!(ctx.now() <= instant);
                ctx.instant.time = instant;
            });

            timers
        }

        /// Skips the current time forward by `duration`, triggering all timers
        /// until then, inclusive, by calling `f` on them.
        ///
        /// Returns the timers which were triggered.
        fn trigger_timers_for<H: TimerHandler<Self, Id>>(
            &mut self,
            duration: Duration,
            handler: &mut H,
        ) -> Vec<Id> {
            let instant = self.with_fake_timer_ctx(|ctx| ctx.now().saturating_add(duration));
            // We know the call to `self.trigger_timers_until_instant` will not
            // panic because we provide an instant that is greater than or equal
            // to the current time.
            self.trigger_timers_until_instant(instant, handler)
        }

        /// Triggers timers and expects them to be the given timers.
        ///
        /// The number of timers to be triggered is taken to be the number of
        /// timers produced by `timers`. Timers may be triggered in any order.
        ///
        /// # Panics
        ///
        /// Panics under the following conditions:
        /// - Fewer timers could be triggered than expected
        /// - Timers were triggered that were not expected
        /// - Timers that were expected were not triggered
        #[track_caller]
        fn trigger_timers_and_expect_unordered<
            I: IntoIterator<Item = Id>,
            H: TimerHandler<Self, Id>,
        >(
            &mut self,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq,
        {
            let mut timers = RefCountedHashSet::from_iter(timers);

            for _ in 0..timers.len() {
                let id = self.trigger_next_timer(handler).expect("ran out of timers to trigger");
                match timers.remove(id.clone()) {
                    RemoveResult::Removed(()) | RemoveResult::StillPresent => {}
                    RemoveResult::NotPresent => panic!("triggered unexpected timer: {:?}", id),
                }
            }

            if timers.len() > 0 {
                let mut s = String::from("Expected timers did not trigger:");
                for (id, count) in timers.iter_counts() {
                    s += &format!("\n\t{count}x {id:?}");
                }
                panic!("{}", s);
            }
        }

        /// Triggers timers until `instant` and expects them to be the given
        /// timers.
        ///
        /// Like `trigger_timers_and_expect_unordered`, except that timers will
        /// only be triggered until `instant` (inclusive).
        fn trigger_timers_until_and_expect_unordered<
            I: IntoIterator<Item = Id>,
            H: TimerHandler<Self, Id>,
        >(
            &mut self,
            instant: FakeInstant,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq,
        {
            let mut timers = RefCountedHashSet::from_iter(timers);

            let triggered_timers = self.trigger_timers_until_instant(instant, handler);

            for id in triggered_timers {
                match timers.remove(id.clone()) {
                    RemoveResult::Removed(()) | RemoveResult::StillPresent => {}
                    RemoveResult::NotPresent => panic!("triggered unexpected timer: {:?}", id),
                }
            }

            if timers.len() > 0 {
                let mut s = String::from("Expected timers did not trigger:");
                for (id, count) in timers.iter_counts() {
                    s += &format!("\n\t{count}x {id:?}");
                }
                panic!("{}", s);
            }
        }

        /// Triggers timers for `duration` and expects them to be the given
        /// timers.
        ///
        /// Like `trigger_timers_and_expect_unordered`, except that timers will
        /// only be triggered for `duration` (inclusive).
        fn trigger_timers_for_and_expect<I: IntoIterator<Item = Id>, H: TimerHandler<Self, Id>>(
            &mut self,
            duration: Duration,
            timers: I,
            handler: &mut H,
        ) where
            Id: Debug + Hash + Eq,
        {
            let instant = self.with_fake_timer_ctx(|ctx| ctx.now().saturating_add(duration));
            self.trigger_timers_until_and_expect_unordered(instant, timers, handler);
        }
    }

    /// A fake [`FrameContext`].
    pub struct FakeFrameCtx<Meta> {
        frames: Vec<(Meta, Vec<u8>)>,
        should_error_for_frame: Option<Box<dyn Fn(&Meta) -> bool>>,
    }

    #[cfg(test)]
    impl<Meta> FakeFrameCtx<Meta> {
        /// Closure which can decide to cause an error to be thrown when
        /// handling a frame, based on the metadata.
        pub(crate) fn set_should_error_for_frame<F: Fn(&Meta) -> bool + 'static>(&mut self, f: F) {
            self.should_error_for_frame = Some(Box::new(f));
        }
    }

    impl<Meta> Default for FakeFrameCtx<Meta> {
        fn default() -> FakeFrameCtx<Meta> {
            FakeFrameCtx { frames: Vec::new(), should_error_for_frame: None }
        }
    }

    impl<Meta> FakeFrameCtx<Meta> {
        /// Take all frames sent so far.
        pub(crate) fn take_frames(&mut self) -> Vec<(Meta, Vec<u8>)> {
            core::mem::take(&mut self.frames)
        }

        /// Get the frames sent so far.
        #[cfg(test)]
        pub(crate) fn frames(&self) -> &[(Meta, Vec<u8>)] {
            self.frames.as_slice()
        }

        pub(crate) fn push(&mut self, meta: Meta, frame: Vec<u8>) {
            self.frames.push((meta, frame))
        }
    }

    impl<C, Meta> SendFrameContext<C, Meta> for FakeFrameCtx<Meta> {
        fn send_frame<S>(
            &mut self,
            _bindings_ctx: &mut C,
            metadata: Meta,
            frame: S,
        ) -> Result<(), S>
        where
            S: Serializer,
            S::Buffer: BufferMut,
        {
            if let Some(should_error_for_frame) = &self.should_error_for_frame {
                if should_error_for_frame(&metadata) {
                    return Err(frame);
                }
            }

            let buffer = frame.serialize_vec_outer().map_err(|(_err, s)| s)?;
            self.push(metadata, buffer.as_ref().to_vec());
            Ok(())
        }
    }

    /// A fake [`EventContext`].
    pub struct FakeEventCtx<E: Debug> {
        events: Vec<E>,
        must_watch_all_events: bool,
    }

    impl<E: Debug> EventContext<E> for FakeEventCtx<E> {
        fn on_event(&mut self, event: E) {
            self.events.push(event)
        }
    }

    impl<E: Debug> Drop for FakeEventCtx<E> {
        fn drop(&mut self) {
            if self.must_watch_all_events {
                assert!(
                    self.events.is_empty(),
                    "dropped context with unacknowledged events: {:?}",
                    self.events
                );
            }
        }
    }

    impl<E: Debug> Default for FakeEventCtx<E> {
        fn default() -> Self {
            Self { events: Default::default(), must_watch_all_events: false }
        }
    }

    impl<E: Debug> FakeEventCtx<E> {
        #[cfg(test)]
        pub(crate) fn take(&mut self) -> Vec<E> {
            // Any client that calls `take()` is opting into watching events
            // and must watch them all.
            self.must_watch_all_events = true;
            core::mem::take(&mut self.events)
        }
    }

    /// A fake [`TracingContext`].
    #[derive(Default)]
    pub struct FakeTracingCtx;

    impl TracingContext for FakeTracingCtx {
        type DurationScope = ();

        fn duration(&self, _: &'static CStr) {}
    }

    /// A tuple of device ID and IP version.
    #[derive(Derivative)]
    #[derivative(Debug(bound = ""))]
    pub struct PureIpDeviceAndIpVersion<BT: DeviceLayerTypes> {
        pub(crate) device: PureIpWeakDeviceId<BT>,
        pub(crate) version: IpVersion,
    }

    /// A test helper used to provide an implementation of a bindings context.
    pub(crate) struct FakeBindingsCtx<TimerId, Event: Debug, State, FrameMeta> {
        pub(crate) rng: FakeCryptoRng<XorShiftRng>,
        pub(crate) timers: FakeTimerCtx<TimerId>,
        pub(crate) events: FakeEventCtx<Event>,
        pub(crate) frames: FakeFrameCtx<FrameMeta>,
        state: State,
    }

    impl<TimerId, Event: Debug, State, FrameMeta> ContextProvider
        for FakeBindingsCtx<TimerId, Event, State, FrameMeta>
    {
        type Context = Self;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    impl<TimerId, Event: Debug, State: Default, FrameMeta> Default
        for FakeBindingsCtx<TimerId, Event, State, FrameMeta>
    {
        fn default() -> Self {
            Self {
                rng: FakeCryptoRng::new_xorshift(0),
                timers: FakeTimerCtx::default(),
                events: FakeEventCtx::default(),
                frames: FakeFrameCtx::default(),
                state: Default::default(),
            }
        }
    }

    impl<TimerId, Event: Debug, State, FrameMeta> FakeBindingsCtx<TimerId, Event, State, FrameMeta> {
        /// Seed the testing RNG with a specific value.
        #[cfg(test)]
        pub(crate) fn seed_rng(&mut self, seed: u128) {
            self.rng = FakeCryptoRng::new_xorshift(seed);
        }

        /// Move the clock forward by the given duration without firing any
        /// timers.
        ///
        /// If any timers are scheduled to fire in the given duration, future
        /// use of this `FakeCoreCtx` may have surprising or buggy behavior.
        #[cfg(test)]
        pub(crate) fn sleep_skip_timers(&mut self, duration: Duration) {
            self.timers.instant.sleep(duration);
        }

        #[cfg(test)]
        pub(crate) fn timer_ctx(&self) -> &FakeTimerCtx<TimerId> {
            &self.timers
        }

        #[cfg(test)]
        pub(crate) fn timer_ctx_mut(&mut self) -> &mut FakeTimerCtx<TimerId> {
            &mut self.timers
        }

        #[cfg(test)]
        pub(crate) fn take_events(&mut self) -> Vec<Event> {
            self.events.take()
        }

        pub(crate) fn frame_ctx_mut(&mut self) -> &mut FakeFrameCtx<FrameMeta> {
            &mut self.frames
        }

        pub(crate) fn state(&self) -> &State {
            &self.state
        }

        pub(crate) fn state_mut(&mut self) -> &mut State {
            &mut self.state
        }
    }

    impl<TimerId, Event: Debug, State, FrameMeta> FilterBindingsTypes
        for FakeBindingsCtx<TimerId, Event, State, FrameMeta>
    {
        type DeviceClass = ();
    }

    impl<TimerId, Event: Debug, State, FrameMeta> RngContext
        for FakeBindingsCtx<TimerId, Event, State, FrameMeta>
    {
        type Rng<'a> = FakeCryptoRng<XorShiftRng> where Self: 'a;

        fn rng(&mut self) -> Self::Rng<'_> {
            self.rng.clone()
        }
    }

    impl<Id, Event: Debug, State, FrameMeta> AsRef<FakeInstantCtx>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        fn as_ref(&self) -> &FakeInstantCtx {
            self.timers.as_ref()
        }
    }

    impl<Id, Event: Debug, State, FrameMeta> AsRef<FakeTimerCtx<Id>>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        fn as_ref(&self) -> &FakeTimerCtx<Id> {
            &self.timers
        }
    }

    impl<Id, Event: Debug, State, FrameMeta> AsMut<FakeTimerCtx<Id>>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        fn as_mut(&mut self) -> &mut FakeTimerCtx<Id> {
            &mut self.timers
        }
    }

    impl<Id: Debug + PartialEq, Event: Debug, State, FrameMeta> TimerContext<Id>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        fn schedule_timer_instant(&mut self, time: FakeInstant, id: Id) -> Option<FakeInstant> {
            self.timers.schedule_timer_instant(time, id)
        }

        fn cancel_timer(&mut self, id: Id) -> Option<FakeInstant> {
            self.timers.cancel_timer(id)
        }

        fn cancel_timers_with<F: FnMut(&Id) -> bool>(&mut self, f: F) {
            self.timers.cancel_timers_with(f);
        }

        fn scheduled_instant(&self, id: Id) -> Option<FakeInstant> {
            self.timers.scheduled_instant(id)
        }
    }

    impl<Id, Event: Debug, State, FrameMeta> EventContext<Event>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        fn on_event(&mut self, event: Event) {
            self.events.on_event(event)
        }
    }

    impl<Id, Event: Debug, State, FrameMeta> TracingContext
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        type DurationScope = ();

        fn duration(&self, _: &'static CStr) {}
    }

    impl<D: LinkDevice, Id, Event: Debug, State, FrameMeta> LinkResolutionContext<D>
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        type Notifier = FakeLinkResolutionNotifier<D>;
    }

    impl<Id, Event: Debug, State, FrameMeta> crate::ReferenceNotifiers
        for FakeBindingsCtx<Id, Event, State, FrameMeta>
    {
        type ReferenceReceiver<T: 'static> = Never;

        type ReferenceNotifier<T: Send + 'static> = Never;

        fn new_reference_notifier<T: Send + 'static, D: Debug>(
            debug_references: D,
        ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
            // NB: We don't want deferred destruction in core tests. These are
            // always single-threaded and single-task, and we want to encourage
            // explicit cleanup.
            panic!(
                "FakeBindingsCtx can't create deferred reference notifiers for type {}: \
                debug_references={debug_references:?}",
                core::any::type_name::<T>()
            );
        }
    }

    #[derive(Debug)]
    pub(crate) struct FakeLinkResolutionNotifier<D: LinkDevice>(
        Arc<Mutex<Option<Result<D::Address, crate::error::AddressResolutionFailed>>>>,
    );

    impl<D: LinkDevice> LinkResolutionNotifier<D> for FakeLinkResolutionNotifier<D> {
        type Observer =
            Arc<Mutex<Option<Result<D::Address, crate::error::AddressResolutionFailed>>>>;

        fn new() -> (Self, Self::Observer) {
            let inner = Arc::new(Mutex::new(None));
            (Self(inner.clone()), inner)
        }

        fn notify(self, result: Result<D::Address, crate::error::AddressResolutionFailed>) {
            let Self(inner) = self;
            let mut inner = inner.lock();
            assert_eq!(*inner, None, "resolved link address was set more than once");
            *inner = Some(result);
        }
    }

    pub(crate) trait WithFakeTimerContext<TimerId> {
        fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId>) -> O>(&self, f: F) -> O;

        fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId>) -> O>(
            &mut self,
            f: F,
        ) -> O;
    }

    #[allow(dead_code)]
    pub(crate) trait WithFakeFrameContext<SendMeta> {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<SendMeta>) -> O>(
            &mut self,
            f: F,
        ) -> O;
    }

    #[cfg(test)]
    impl<CC, TimerId, Event: Debug, State> WithFakeTimerContext<TimerId>
        for FakeCtxWithCoreCtx<CC, TimerId, Event, State>
    {
        fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId>) -> O>(&self, f: F) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&bindings_ctx.timers)
        }

        fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            let Self { core_ctx: _, bindings_ctx } = self;
            f(&mut bindings_ctx.timers)
        }
    }

    #[cfg(test)]
    impl<TimerId, Event: Debug, State, FrameMeta> WithFakeTimerContext<TimerId>
        for FakeBindingsCtx<TimerId, Event, State, FrameMeta>
    {
        fn with_fake_timer_ctx<O, F: FnOnce(&FakeTimerCtx<TimerId>) -> O>(&self, f: F) -> O {
            f(&self.timers)
        }

        fn with_fake_timer_ctx_mut<O, F: FnOnce(&mut FakeTimerCtx<TimerId>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.timers)
        }
    }

    #[cfg(test)]
    pub(crate) type FakeCtxWithCoreCtx<CC, TimerId, Event, BindingsCtxState> =
        crate::testutil::ContextPair<CC, FakeBindingsCtx<TimerId, Event, BindingsCtxState, ()>>;

    #[cfg(test)]
    pub(crate) type FakeCtx<S, TimerId, Meta, Event, DeviceId, BindingsCtxState> =
        FakeCtxWithCoreCtx<FakeCoreCtx<S, Meta, DeviceId>, TimerId, Event, BindingsCtxState>;

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsRef<FakeInstantCtx>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_ref(&self) -> &FakeInstantCtx {
            self.bindings_ctx.timers.as_ref()
        }
    }

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsRef<FakeTimerCtx<Id>>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_ref(&self) -> &FakeTimerCtx<Id> {
            &self.bindings_ctx.timers
        }
    }

    #[cfg(test)]
    impl<CC, Id, Event: Debug, BindingsCtxState> AsMut<FakeTimerCtx<Id>>
        for FakeCtxWithCoreCtx<CC, Id, Event, BindingsCtxState>
    {
        fn as_mut(&mut self) -> &mut FakeTimerCtx<Id> {
            &mut self.bindings_ctx.timers
        }
    }

    #[cfg(test)]
    impl<S, Id, Meta, Event: Debug, DeviceId, BindingsCtxState> AsMut<FakeFrameCtx<Meta>>
        for FakeCtx<S, Id, Meta, Event, DeviceId, BindingsCtxState>
    {
        fn as_mut(&mut self) -> &mut FakeFrameCtx<Meta> {
            &mut self.core_ctx.frames
        }
    }

    #[cfg(test)]
    impl<S, Id, Meta, Event: Debug, DeviceId, BindingsCtxState> WithFakeFrameContext<Meta>
        for FakeCtx<S, Id, Meta, Event, DeviceId, BindingsCtxState>
    {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.core_ctx.frames)
        }
    }

    #[cfg(test)]
    #[derive(Default)]
    pub(crate) struct Wrapped<Outer, Inner> {
        pub(crate) inner: Inner,
        pub(crate) outer: Outer,
    }

    #[cfg(test)]
    impl<Outer, Inner> ContextProvider for Wrapped<Outer, Inner> {
        type Context = Self;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    #[cfg(test)]
    pub(crate) type WrappedFakeCoreCtx<Outer, S, Meta, DeviceId> =
        Wrapped<Outer, FakeCoreCtx<S, Meta, DeviceId>>;

    #[cfg(test)]
    impl<Outer, S, Meta, DeviceId> WrappedFakeCoreCtx<Outer, S, Meta, DeviceId> {
        pub(crate) fn with_inner_and_outer_state(inner: S, outer: Outer) -> Self {
            Self { inner: FakeCoreCtx::with_state(inner), outer }
        }
    }

    #[cfg(test)]
    impl<Outer, T, Inner: AsRef<T>> AsRef<T> for Wrapped<Outer, Inner> {
        fn as_ref(&self) -> &T {
            self.inner.as_ref()
        }
    }

    #[cfg(test)]
    impl<Outer, T, Inner: AsMut<T>> AsMut<T> for Wrapped<Outer, Inner> {
        fn as_mut(&mut self) -> &mut T {
            self.inner.as_mut()
        }
    }

    /// A test helper used to provide an implementation of a core context.
    #[cfg(test)]
    #[derive(Derivative)]
    #[derivative(Default(bound = "S: Default"))]
    pub(crate) struct FakeCoreCtx<S, Meta, DeviceId> {
        pub(crate) state: S,
        pub(crate) frames: FakeFrameCtx<Meta>,
        _devices_marker: PhantomData<DeviceId>,
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> ContextProvider for FakeCoreCtx<S, Meta, DeviceId> {
        type Context = Self;

        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsRef<FakeCoreCtx<S, Meta, DeviceId>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_ref(&self) -> &FakeCoreCtx<S, Meta, DeviceId> {
            self
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsMut<FakeCoreCtx<S, Meta, DeviceId>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_mut(&mut self) -> &mut FakeCoreCtx<S, Meta, DeviceId> {
            self
        }
    }

    #[cfg(test)]
    impl<BC, S, Meta, DeviceId> CounterContext<BC> for FakeCoreCtx<S, Meta, DeviceId>
    where
        S: CounterContext<BC>,
    {
        fn with_counters<O, F: FnOnce(&BC) -> O>(&self, cb: F) -> O {
            CounterContext::<BC>::with_counters(&self.state, cb)
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> FakeCoreCtx<S, Meta, DeviceId> {
        /// Constructs a `FakeCoreCtx` with the given state and default
        /// `FakeTimerCtx`, and `FakeFrameCtx`.
        pub(crate) fn with_state(state: S) -> Self {
            FakeCoreCtx { state, frames: FakeFrameCtx::default(), _devices_marker: PhantomData }
        }

        /// Get an immutable reference to the inner state.
        ///
        /// This method is provided instead of an [`AsRef`] impl to avoid
        /// conflicting with user-provided implementations of `AsRef<T> for
        /// FakeCtx<S, Id, Meta, Event>` for other types, `T`. It is named
        /// `get_ref` instead of `as_ref` so that programmer doesn't need to
        /// specify which `as_ref` method is intended.
        pub(crate) fn get_ref(&self) -> &S {
            &self.state
        }

        /// Get a mutable reference to the inner state.
        ///
        /// `get_mut` is like `get_ref`, but it returns a mutable reference.
        pub(crate) fn get_mut(&mut self) -> &mut S {
            &mut self.state
        }

        /// Get the list of frames sent so far.
        pub(crate) fn frames(&self) -> &[(Meta, Vec<u8>)] {
            self.frames.frames()
        }

        /// Take the list of frames sent so far.
        pub(crate) fn take_frames(&mut self) -> Vec<(Meta, Vec<u8>)> {
            self.frames.take_frames()
        }

        /// Consumes the `FakeCoreCtx` and returns the inner state.
        pub(crate) fn into_state(self) -> S {
            self.state
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> AsMut<FakeFrameCtx<Meta>> for FakeCoreCtx<S, Meta, DeviceId> {
        fn as_mut(&mut self) -> &mut FakeFrameCtx<Meta> {
            &mut self.frames
        }
    }

    #[cfg(test)]
    impl<S, Meta, DeviceId> WithFakeFrameContext<Meta> for FakeCoreCtx<S, Meta, DeviceId> {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            f(&mut self.frames)
        }
    }

    #[cfg(test)]
    impl<Outer, Inner: WithFakeFrameContext<Meta>, Meta> WithFakeFrameContext<Meta>
        for Wrapped<Outer, Inner>
    {
        fn with_fake_frame_ctx_mut<O, F: FnOnce(&mut FakeFrameCtx<Meta>) -> O>(
            &mut self,
            f: F,
        ) -> O {
            self.inner.with_fake_frame_ctx_mut(f)
        }
    }

    #[cfg(test)]
    impl<S, Id, Meta, Event: Debug, DeviceId, BindingsCtxState, FrameMeta>
        SendFrameContext<FakeBindingsCtx<Id, Event, BindingsCtxState, FrameMeta>, Meta>
        for FakeCoreCtx<S, Meta, DeviceId>
    {
        fn send_frame<SS>(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtx<Id, Event, BindingsCtxState, FrameMeta>,
            metadata: Meta,
            frame: SS,
        ) -> Result<(), SS>
        where
            SS: Serializer,
            SS::Buffer: BufferMut,
        {
            self.frames.send_frame(bindings_ctx, metadata, frame)
        }
    }

    #[cfg(test)]
    #[derive(Debug)]
    pub(crate) struct PendingFrameData<CtxId, Meta> {
        pub(crate) dst_context: CtxId,
        pub(crate) meta: Meta,
        pub(crate) frame: Vec<u8>,
    }

    #[cfg(test)]
    pub(crate) type PendingFrame<CtxId, Meta> = InstantAndData<PendingFrameData<CtxId, Meta>>;

    /// A fake network, composed of many `FakeCoreCtx`s.
    ///
    /// Provides a utility to have many contexts keyed by `CtxId` that can
    /// exchange frames.
    #[cfg(test)]
    pub(crate) struct FakeNetwork<CtxId, Ctx: FakeNetworkContext, Links>
    where
        Links: FakeNetworkLinks<Ctx::SendMeta, Ctx::RecvMeta, CtxId>,
    {
        links: Links,
        current_time: FakeInstant,
        pending_frames: BinaryHeap<PendingFrame<CtxId, Ctx::RecvMeta>>,
        // Declare `contexts` last to ensure that it is dropped last. See
        // https://doc.rust-lang.org/std/ops/trait.Drop.html#drop-order for
        // details.
        contexts: HashMap<CtxId, Ctx>,
    }

    /// A context which can be used with a [`FakeNetwork`].
    #[allow(dead_code)]
    pub(crate) trait FakeNetworkContext {
        /// The type of timer IDs installed by this context.
        type TimerId;
        /// The type of metadata associated with frames sent by this context.
        type SendMeta;
        /// The type of metadata associated with frames received by this
        /// context.
        type RecvMeta;

        /// Handles a single received frame in this context.
        fn handle_frame(&mut self, recv: Self::RecvMeta, data: Buf<Vec<u8>>);
        /// Handles a single timer id in this context.
        fn handle_timer(&mut self, timer: Self::TimerId);
        /// Processes any context-internal queues, returning `true` if any work
        /// was done.
        ///
        /// This is used to drive queued frames that may be sitting inside the
        /// context and invisible to the [`FakeNetwork`].
        fn process_queues(&mut self) -> bool;
    }

    /// A set of links in a `FakeNetwork`.
    ///
    /// A `FakeNetworkLinks` represents the set of links in a `FakeNetwork`.
    /// It exposes the link information by providing the ability to map from a
    /// frame's sending metadata - including its context, local state, and
    /// `SendMeta` - to the set of appropriate receivers, each represented by
    /// a context ID, receive metadata, and latency.
    #[allow(dead_code)]
    pub(crate) trait FakeNetworkLinks<SendMeta, RecvMeta, CtxId> {
        fn map_link(&self, ctx: CtxId, meta: SendMeta) -> Vec<(CtxId, RecvMeta, Option<Duration>)>;
    }

    impl<
            SendMeta,
            RecvMeta,
            CtxId,
            F: Fn(CtxId, SendMeta) -> Vec<(CtxId, RecvMeta, Option<Duration>)>,
        > FakeNetworkLinks<SendMeta, RecvMeta, CtxId> for F
    {
        fn map_link(&self, ctx: CtxId, meta: SendMeta) -> Vec<(CtxId, RecvMeta, Option<Duration>)> {
            (self)(ctx, meta)
        }
    }

    /// The result of a single step in a `FakeNetwork`
    #[cfg(test)]
    #[derive(Debug)]
    pub(crate) struct StepResult {
        pub(crate) timers_fired: usize,
        pub(crate) frames_sent: usize,
        pub(crate) contexts_with_queued_frames: usize,
    }

    #[cfg(test)]
    impl StepResult {
        fn new(
            timers_fired: usize,
            frames_sent: usize,
            contexts_with_queued_frames: usize,
        ) -> Self {
            Self { timers_fired, frames_sent, contexts_with_queued_frames }
        }

        fn new_idle() -> Self {
            Self::new(0, 0, 0)
        }

        /// Returns `true` if the last step did not perform any operations.
        pub(crate) fn is_idle(&self) -> bool {
            return self.timers_fired == 0
                && self.frames_sent == 0
                && self.contexts_with_queued_frames == 0;
        }
    }

    /// Error type that marks that one of the `run_until` family of functions
    /// reached a maximum number of iterations.
    #[derive(Debug)]
    #[allow(dead_code)]
    pub(crate) struct LoopLimitReachedError;

    #[cfg(test)]
    impl<CtxId, Ctx, Links> FakeNetwork<CtxId, Ctx, Links>
    where
        CtxId: Eq + Hash + Copy + Debug,
        Ctx: FakeNetworkContext,
        Links: FakeNetworkLinks<Ctx::SendMeta, Ctx::RecvMeta, CtxId>,
    {
        /// Retrieves a context named `context`.
        pub(crate) fn context<K: Into<CtxId>>(&mut self, context: K) -> &mut Ctx {
            self.contexts.get_mut(&context.into()).unwrap()
        }

        pub(crate) fn with_context<K: Into<CtxId>, O, F: FnOnce(&mut Ctx) -> O>(
            &mut self,
            context: K,
            f: F,
        ) -> O {
            f(self.context(context))
        }
    }

    #[cfg(test)]
    impl<CtxId, Ctx, Links> FakeNetwork<CtxId, Ctx, Links>
    where
        CtxId: Eq + Hash + Copy + Debug,
        Ctx: FakeNetworkContext
            + WithFakeTimerContext<Ctx::TimerId>
            + WithFakeFrameContext<Ctx::SendMeta>,
        Ctx::TimerId: Clone,
        Links: FakeNetworkLinks<Ctx::SendMeta, Ctx::RecvMeta, CtxId>,
    {
        /// Creates a new `FakeNetwork`.
        ///
        /// Creates a new `FakeNetwork` with the collection of `FakeCoreCtx`s in
        /// `contexts`. `Ctx`s are named by type parameter `CtxId`.
        ///
        /// # Panics
        ///
        /// Calls to `new` will panic if given a `FakeCoreCtx` with timer events.
        /// `FakeCoreCtx`s given to `FakeNetwork` **must not** have any timer
        /// events already attached to them, because `FakeNetwork` maintains
        /// all the internal timers in dispatchers in sync to enable synchronous
        /// simulation steps.
        pub(crate) fn new<I: IntoIterator<Item = (CtxId, Ctx)>>(contexts: I, links: Links) -> Self {
            let mut contexts = contexts.into_iter().collect::<HashMap<_, _>>();
            // Take the current time to be the latest of the times of any of the
            // contexts. This ensures that no context has state which is based
            // on having observed a time in the future, which could cause bugs.
            // For any contexts which have a time further in the past, it will
            // appear as though time has jumped forwards, but that's fine. The
            // only way that this could be a problem would be if a timer were
            // installed which should have fired in the interim (code might
            // become buggy in this case). However, we assert below that no
            // timers are installed.
            let latest_time = contexts
                .iter()
                .map(|(_, ctx)| ctx.with_fake_timer_ctx(|ctx| ctx.instant.time))
                .max()
                // If `max` returns `None`, it means that we were called with no
                // contexts. That's kind of silly, but whatever - arbitrarily
                // choose the current time as the epoch.
                .unwrap_or(FakeInstant::default());

            assert!(
                !contexts
                    .iter()
                    .any(|(_, ctx)| { !ctx.with_fake_timer_ctx(|ctx| ctx.timers.is_empty()) }),
                "can't start network with contexts that already have timers set"
            );

            // Synchronize all contexts' current time to the latest time of any
            // of the contexts. See comment above for more details.
            for (_, ctx) in contexts.iter_mut() {
                ctx.with_fake_timer_ctx_mut(|ctx| ctx.instant.time = latest_time);
            }

            Self { contexts, current_time: latest_time, pending_frames: BinaryHeap::new(), links }
        }

        /// Iterates over pending frames in an arbitrary order.
        pub(crate) fn iter_pending_frames(
            &self,
        ) -> impl Iterator<Item = &PendingFrame<CtxId, Ctx::RecvMeta>> {
            self.pending_frames.iter()
        }

        /// Drops all pending frames; they will not be delivered.
        pub(crate) fn drop_pending_frames(&mut self) {
            self.pending_frames.clear();
        }

        /// Performs a single step in network simulation.
        ///
        /// `step` performs a single logical step in the collection of `Ctx`s
        /// held by this `FakeNetwork`. A single step consists of the following
        /// operations:
        ///
        /// - All pending frames, kept in each `FakeCoreCtx`, are mapped to their
        ///   destination context/device pairs and moved to an internal
        ///   collection of pending frames.
        /// - The collection of pending timers and scheduled frames is inspected
        ///   and a simulation time step is retrieved, which will cause a next
        ///   event to trigger. The simulation time is updated to the new time.
        /// - All scheduled frames whose deadline is less than or equal to the
        ///   new simulation time are sent to their destinations, handled using
        ///   `handle_frame`.
        /// - All timer events whose deadline is less than or equal to the new
        ///   simulation time are fired, handled using `handle_timer`.
        ///
        /// If any new events are created during the operation of frames or
        /// timers, they **will not** be taken into account in the current
        /// `step`. That is, `step` collects all the pending events before
        /// dispatching them, ensuring that an infinite loop can't be created as
        /// a side effect of calling `step`.
        ///
        /// The return value of `step` indicates which of the operations were
        /// performed.
        ///
        /// # Panics
        ///
        /// If `FakeNetwork` was set up with a bad `links`, calls to `step` may
        /// panic when trying to route frames to their context/device
        /// destinations.
        pub(crate) fn step(&mut self) -> StepResult
        where
            Ctx::TimerId: core::fmt::Debug,
        {
            self.step_with(|_, meta, buf| Some((meta, buf)))
        }

        /// Like [`FakeNetwork::step`], but receives a function
        /// `filter_map_frame` that can modify the an inbound frame before
        /// delivery or drop it altogether by returning `None`.
        pub(crate) fn step_with<
            F: FnMut(&mut Ctx, Ctx::RecvMeta, Buf<Vec<u8>>) -> Option<(Ctx::RecvMeta, Buf<Vec<u8>>)>,
        >(
            &mut self,
            mut filter_map_frame: F,
        ) -> StepResult
        where
            Ctx::TimerId: core::fmt::Debug,
        {
            let mut ret = StepResult::new_idle();
            // Drive all queues before checking for the network and time
            // simulation.
            for (_, ctx) in self.contexts.iter_mut() {
                if ctx.process_queues() {
                    ret.contexts_with_queued_frames += 1;
                }
            }

            self.collect_frames();

            let next_step = if let Some(t) = self.next_step() {
                t
            } else {
                return ret;
            };

            // This assertion holds the contract that `next_step` does not
            // return a time in the past.
            assert!(next_step >= self.current_time);

            // Move time forward:
            self.current_time = next_step;
            for (_, ctx) in self.contexts.iter_mut() {
                ctx.with_fake_timer_ctx_mut(|ctx| ctx.instant.time = next_step);
            }

            // Dispatch all pending frames:
            while let Some(InstantAndData(t, _)) = self.pending_frames.peek() {
                // TODO(https://github.com/rust-lang/rust/issues/53667): Remove
                // this break once let_chains is stable.
                if *t > self.current_time {
                    break;
                }
                // We can unwrap because we just peeked.
                let PendingFrameData { dst_context, meta, frame } =
                    self.pending_frames.pop().unwrap().1;
                let dst_context = self.context(dst_context);
                if let Some((meta, frame)) =
                    filter_map_frame(dst_context, meta, Buf::new(frame, ..))
                {
                    dst_context.handle_frame(meta, frame)
                }
                ret.frames_sent += 1;
            }

            // Dispatch all pending timers.
            for (_, ctx) in self.contexts.iter_mut() {
                // We have to collect the timers before dispatching them, to
                // avoid an infinite loop in case handle_timer schedules another
                // timer for the same or older FakeInstant.
                let mut timers = Vec::<Ctx::TimerId>::new();
                ctx.with_fake_timer_ctx_mut(|ctx| {
                    while let Some(InstantAndData(t, id)) = ctx.timers.peek() {
                        // TODO(https://github.com/rust-lang/rust/issues/53667):
                        // Remove this break once let_chains is stable.
                        if *t > ctx.now() {
                            break;
                        }
                        timers.push(id.clone());
                        assert_ne!(ctx.timers.pop(), None);
                    }
                });

                for t in timers {
                    ctx.handle_timer(t);
                    ret.timers_fired += 1;
                }
            }
            ret
        }

        /// Runs the network until it is starved of events.
        ///
        /// # Panics
        ///
        /// Panics if 1,000,000 steps are performed without becoming idle.
        /// Also panics under the same conditions as [`step`].
        pub(crate) fn run_until_idle(&mut self)
        where
            Ctx::TimerId: core::fmt::Debug,
        {
            self.run_until_idle_with(|_, meta, frame| Some((meta, frame)))
        }

        /// Like [`FakeNetwork::run_until_idle`] but receives a function
        /// `filter_map_frame` that can modify the an inbound frame before
        /// delivery or drop it altogether by returning `None`.
        pub(crate) fn run_until_idle_with<
            F: FnMut(&mut Ctx, Ctx::RecvMeta, Buf<Vec<u8>>) -> Option<(Ctx::RecvMeta, Buf<Vec<u8>>)>,
        >(
            &mut self,
            mut filter_map_frame: F,
        ) where
            Ctx::TimerId: core::fmt::Debug,
        {
            for _ in 0..1_000_000 {
                if self.step_with(&mut filter_map_frame).is_idle() {
                    return;
                }
            }
            panic!("FakeNetwork seems to have gotten stuck in a loop.");
        }

        /// Collects all queued frames.
        ///
        /// Collects all pending frames and schedules them for delivery to the
        /// destination context/device based on the result of `links`. The
        /// collected frames are queued for dispatching in the `FakeNetwork`,
        /// ordered by their scheduled delivery time given by the latency result
        /// provided by `links`.
        pub(crate) fn collect_frames(&mut self) {
            let all_frames: Vec<(CtxId, Vec<(Ctx::SendMeta, Vec<u8>)>)> = self
                .contexts
                .iter_mut()
                .filter_map(|(n, ctx)| {
                    ctx.with_fake_frame_ctx_mut(|ctx| {
                        if ctx.frames.is_empty() {
                            None
                        } else {
                            Some((n.clone(), ctx.frames.drain(..).collect()))
                        }
                    })
                })
                .collect();

            for (src_context, frames) in all_frames.into_iter() {
                for (send_meta, frame) in frames.into_iter() {
                    for (dst_context, recv_meta, latency) in
                        self.links.map_link(src_context, send_meta)
                    {
                        self.pending_frames.push(PendingFrame::new(
                            self.current_time + latency.unwrap_or(Duration::from_millis(0)),
                            PendingFrameData { frame: frame.clone(), dst_context, meta: recv_meta },
                        ));
                    }
                }
            }
        }

        /// Calculates the next `FakeInstant` when events are available.
        ///
        /// Returns the smallest `FakeInstant` greater than or equal to the
        /// current time for which an event is available. If no events are
        /// available, returns `None`.
        pub(crate) fn next_step(&self) -> Option<FakeInstant> {
            // Get earliest timer in all contexts.
            let next_timer = self
                .contexts
                .iter()
                .filter_map(|(_, ctx)| {
                    ctx.with_fake_timer_ctx(|ctx| match ctx.timers.peek() {
                        Some(tmr) => Some(tmr.0),
                        None => None,
                    })
                })
                .min();
            // Get the instant for the next packet.
            let next_packet_due = self.pending_frames.peek().map(|t| t.0);

            // Return the earliest of them both, and protect against returning a
            // time in the past.
            match next_timer {
                Some(t) if next_packet_due.is_some() => Some(t).min(next_packet_due),
                Some(t) => Some(t),
                None => next_packet_due,
            }
            .map(|t| t.max(self.current_time))
        }
    }

    #[cfg(test)]
    impl<CtxId, Links, CC, BC> FakeNetwork<CtxId, crate::testutil::ContextPair<CC, BC>, Links>
    where
        crate::testutil::ContextPair<CC, BC>: FakeNetworkContext,
        CtxId: Eq + Hash + Copy + Debug,
        Links: FakeNetworkLinks<
            <crate::testutil::ContextPair<CC, BC> as FakeNetworkContext>::SendMeta,
            <crate::testutil::ContextPair<CC, BC> as FakeNetworkContext>::RecvMeta,
            CtxId,
        >,
    {
        /// Retrieves a `FakeCoreCtx` named `context`.
        pub(crate) fn core_ctx<K: Into<CtxId>>(&mut self, context: K) -> &mut CC {
            let crate::testutil::ContextPair { core_ctx, bindings_ctx: _ } = self.context(context);
            core_ctx
        }

        /// Retrieves a `FakeBindingsCtx` named `context`.
        pub(crate) fn bindings_ctx<K: Into<CtxId>>(&mut self, context: K) -> &mut BC {
            let crate::testutil::ContextPair { core_ctx: _, bindings_ctx } = self.context(context);
            bindings_ctx
        }
    }

    /// Creates a new [`FakeNetwork`] of [`Ctx`]s in a simple two-host
    /// configuration.
    ///
    /// Two hosts are created with the given names. Packets emitted by one
    /// arrive at the other and vice-versa.
    #[cfg(test)]
    pub(crate) fn new_simple_fake_network<CtxId: Copy + Debug + Hash + Eq>(
        a_id: CtxId,
        a: crate::testutil::FakeCtx,
        a_device_id: EthernetWeakDeviceId<crate::testutil::FakeBindingsCtx>,
        b_id: CtxId,
        b: crate::testutil::FakeCtx,
        b_device_id: EthernetWeakDeviceId<crate::testutil::FakeBindingsCtx>,
    ) -> FakeNetwork<
        CtxId,
        crate::testutil::FakeCtx,
        impl FakeNetworkLinks<
            DispatchedFrame,
            EthernetDeviceId<crate::testutil::FakeBindingsCtx>,
            CtxId,
        >,
    > {
        let contexts = vec![(a_id, a), (b_id, b)].into_iter();
        FakeNetwork::new(contexts, move |net, _frame: DispatchedFrame| {
            if net == a_id {
                b_device_id
                    .upgrade()
                    .map(|device_id| (b_id, device_id, None))
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                a_device_id
                    .upgrade()
                    .map(|device_id| (a_id, device_id, None))
                    .into_iter()
                    .collect::<Vec<_>>()
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use crate::device::testutil::FakeDeviceId;

        use super::*;

        const ONE_SEC: Duration = Duration::from_secs(1);
        const ONE_SEC_INSTANT: FakeInstant = FakeInstant { offset: ONE_SEC };

        #[test]
        fn test_instant_and_data() {
            // Verify implementation of InstantAndData to be used as a complex
            // type in a BinaryHeap.
            let mut heap = BinaryHeap::<InstantAndData<usize>>::new();
            let now = FakeInstant::default();

            fn new_data(time: FakeInstant, id: usize) -> InstantAndData<usize> {
                InstantAndData::new(time, id)
            }

            heap.push(new_data(now + Duration::from_secs(1), 1));
            heap.push(new_data(now + Duration::from_secs(2), 2));

            // Earlier timer is popped first.
            assert_eq!(heap.pop().unwrap().1, 1);
            assert_eq!(heap.pop().unwrap().1, 2);
            assert_eq!(heap.pop(), None);

            heap.push(new_data(now + Duration::from_secs(1), 1));
            heap.push(new_data(now + Duration::from_secs(1), 1));

            // Can pop twice with identical data.
            assert_eq!(heap.pop().unwrap().1, 1);
            assert_eq!(heap.pop().unwrap().1, 1);
            assert_eq!(heap.pop(), None);
        }

        #[test]
        fn test_fake_timer_context() {
            // An implementation of `TimerContext` that uses `usize` timer IDs
            // and stores every timer in a `Vec`.
            impl<M, E: Debug, D, S, F> TimerHandler<FakeBindingsCtx<usize, E, S, F>, usize>
                for FakeCoreCtx<Vec<(usize, FakeInstant)>, M, D>
            {
                fn handle_timer(
                    &mut self,
                    bindings_ctx: &mut FakeBindingsCtx<usize, E, S, F>,
                    id: usize,
                ) {
                    let now = bindings_ctx.now();
                    self.get_mut().push((id, now));
                }
            }

            let new_ctx =
                || FakeCtx::<Vec<(usize, FakeInstant)>, usize, (), (), FakeDeviceId, ()>::default();

            let FakeCtx { mut core_ctx, mut bindings_ctx } = new_ctx();

            // When no timers are installed, `trigger_next_timer` should return
            // `false`.
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), None);
            assert_eq!(core_ctx.get_ref().as_slice(), []);

            // When one timer is installed, it should be triggered.
            let FakeCtx { mut core_ctx, mut bindings_ctx } = new_ctx();

            // No timer with id `0` exists yet.
            assert_eq!(bindings_ctx.scheduled_instant(0), None);

            assert_eq!(bindings_ctx.schedule_timer(ONE_SEC, 0), None);

            // Timer with id `0` scheduled to execute at `ONE_SEC_INSTANT`.
            assert_eq!(bindings_ctx.scheduled_instant(0).unwrap(), ONE_SEC_INSTANT);

            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(0));
            assert_eq!(core_ctx.get_ref().as_slice(), [(0, ONE_SEC_INSTANT)]);

            // After the timer fires, it should not still be scheduled at some
            // instant.
            assert_eq!(bindings_ctx.scheduled_instant(0), None);

            // The time should have been advanced.
            assert_eq!(bindings_ctx.now(), ONE_SEC_INSTANT);

            // Once it's been triggered, it should be canceled and not
            // triggerable again.
            let FakeCtx { mut core_ctx, mut bindings_ctx } = new_ctx();
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), None);
            assert_eq!(core_ctx.get_ref().as_slice(), []);

            // If we schedule a timer but then cancel it, it shouldn't fire.
            let FakeCtx { mut core_ctx, mut bindings_ctx } = new_ctx();

            assert_eq!(bindings_ctx.schedule_timer(ONE_SEC, 0), None);
            assert_eq!(bindings_ctx.cancel_timer(0), Some(ONE_SEC_INSTANT));
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), None);
            assert_eq!(core_ctx.get_ref().as_slice(), []);

            // If we schedule a timer but then schedule the same ID again, the
            // second timer should overwrite the first one.
            let FakeCtx { core_ctx: _, mut bindings_ctx } = new_ctx();
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(0), 0), None);
            assert_eq!(
                bindings_ctx.schedule_timer(ONE_SEC, 0),
                Some(Duration::from_secs(0).into())
            );
            assert_eq!(bindings_ctx.cancel_timer(0), Some(ONE_SEC_INSTANT));

            // If we schedule three timers and then run `trigger_timers_until`
            // with the appropriate value, only two of them should fire.
            let FakeCtx { mut core_ctx, mut bindings_ctx } = new_ctx();
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(0), 0), None,);
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(1), 1), None,);
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(2), 2), None,);
            assert_eq!(
                bindings_ctx.trigger_timers_until_instant(ONE_SEC_INSTANT, &mut core_ctx),
                vec![0, 1],
            );

            // The first two timers should have fired.
            assert_eq!(
                core_ctx.get_ref().as_slice(),
                [(0, FakeInstant::from(Duration::from_secs(0))), (1, ONE_SEC_INSTANT)]
            );

            // They should be canceled now.
            assert_eq!(bindings_ctx.cancel_timer(0), None);
            assert_eq!(bindings_ctx.cancel_timer(1), None);

            // The clock should have been updated.
            assert_eq!(bindings_ctx.now(), ONE_SEC_INSTANT);

            // The last timer should not have fired.
            assert_eq!(
                bindings_ctx.cancel_timer(2),
                Some(FakeInstant::from(Duration::from_secs(2)))
            );
        }

        #[test]
        fn test_trigger_timers_until_and_expect_unordered() {
            // If the requested instant does not coincide with a timer trigger
            // point, the time should still be advanced.
            let FakeCtx { mut core_ctx, mut bindings_ctx } =
                FakeCtx::<Vec<(usize, FakeInstant)>, usize, (), (), FakeDeviceId, ()>::default();
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(0), 0), None);
            assert_eq!(bindings_ctx.schedule_timer(Duration::from_secs(2), 1), None);
            bindings_ctx.trigger_timers_until_and_expect_unordered(
                ONE_SEC_INSTANT,
                vec![0],
                &mut core_ctx,
            );
            assert_eq!(bindings_ctx.now(), ONE_SEC_INSTANT);
        }
    }
}
