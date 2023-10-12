// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This crates introduces a framework to handle explicit ownership.
//!
//! Explicit ownership is used for object that needs to be cleaned, but cannot use `Drop` because
//! the release operation requires a context. For example, when using the rust types to ensures the
//! locking order, taking a lock requires knowing what locks are already held at this point, and
//! uses an explicit object to represent this. If the object needs to take a lock during the
//! release operation, `Drop` cannot provide it.
//!
//! An object that uses explicit ownership uses the `Releasable` trait. The user must calls the
//! `release` method on it before it goes out of scope.
//!
//! A shared object that used explicit ownership used the `OwnedRef`/`WeakRef`/`TempRef`
//! containers.
//! The meaning are the following:
//! - Object that owns the shared object use `OwnedRef`. They are responsible to call `release`
//! before dropping the reference.
//! - Object that do not owned the shared object use `WeakRef`. This acts as a weak reference to
//! the object. They can convert it to a strong reference using the `upgrade` method. The returned
//! value is an `Option<TempRef>`. The `TempRef` allows access to the object. Because this doesn't
//! repsent ownership, the `TempRef` must not be kept, in particular, the user should not do any
//! blocking operation while having a `TempRef`.

// Not all instance of OwnedRef and Releasable are used in non test code yet.
#![allow(dead_code)]

// TODO(fxbug.dev/131097): Create a linter to ensure TempRef is not held while calling any blocking
// operation.

use core::hash::Hasher;
use fuchsia_zircon as zx;
use std::{
    hash::Hash,
    ops::Deref,
    sync::{
        atomic::{fence, AtomicUsize, Ordering},
        Arc, Weak,
    },
};

/// Macro to build a specific Releasable and OwnedRef.
macro_rules! make_ownership_types {
    ($($suffix:ident)?, $self:ty, $take_self:ty) => { paste::paste! {

/// The base trait for explicit ownership. Any `Releasable` object must call `release` before
/// being dropped.
pub trait [< Releasable $($suffix)? >] {
    type Context<'a>;

    // TODO(https://fxbug.dev/131095): Only the `self` version should exist, but this is
    // problematic with Task and CurrentTask at this point.
    fn release(self: $self, c: Self::Context<'_>);
}

/// Releasing an option calls release if the option is not empty.
impl<T: [< Releasable $($suffix)? >]> [< Releasable $($suffix)? >] for Option<T> {
    type Context<'a> = T::Context<'a>;

    fn release(self: $self, c: Self::Context<'_>) {
        if let Some(v) = self {
            v.release(c);
        }
    }
}

impl<T: [< Releasable $($suffix)? >]> [< Releasable $($suffix)? >] for ReleaseGuard<T> {
    type Context<'a> = T::Context<'a>;

    fn release(self: $self, c: Self::Context<'_>) {
        self.drop_guard.disarm();
        self.value.release(c);
    }
}

/// An owning reference to a shared owned object. Each instance must call `release` before being
/// dropped.
/// `OwnedRef` will panic on Drop in debug builds if it has not been released.
#[must_use = "OwnedRef must be released"]
pub struct [< OwnedRef $($suffix)? >]<T> {
    /// The shared data.
    inner: Option<Arc<RefInner<T>>>,

    /// A guard that will ensure a panic on drop if the ref has not been released.
    drop_guard: DropGuard,
}

impl<T> [< OwnedRef $($suffix)? >]<T> {
    pub fn new(value: T) -> Self {
        Self { inner: Some(Arc::new(RefInner::new(value))), drop_guard: Default::default() }
    }

    pub fn new_cyclic<F>(data_fn: F) -> Self
    where
        F: FnOnce(WeakRef<T>) -> T,
    {
        let inner = Arc::new_cyclic(|weak_inner| {
            let weak = WeakRef(weak_inner.clone());
            RefInner::new(data_fn(weak))
        });
        Self { inner: Some(inner), drop_guard: Default::default() }
    }

    /// Provides a raw pointer to the data.
    ///
    /// See `Arc::as_ptr`
    pub fn as_ptr(this: &Self) -> *const T {
        &Self::inner(this).value.value as *const T
    }

    /// Returns true if the two objects point to the same allocation
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Self::as_ptr(this) == Self::as_ptr(other)
    }

    /// Produce a `WeakRef` from a `OwnedRef`.
    pub fn downgrade(this: &Self) -> WeakRef<T> {
        WeakRef(Arc::downgrade(Self::inner(this)))
    }

    /// Produce a `TempRef` from a `OwnedRef`. As an `OwnedRef` exists at the time of the creation,
    /// this cannot fail.
    pub fn temp(this: &Self) -> TempRef<'_, T> {
        TempRef::new(Arc::clone(Self::inner(this)))
    }

    fn inner(this: &Self) -> &Arc<RefInner<T>> {
        this.inner.as_ref().expect("OwnedRef has been released.")
    }
}

impl<T: [< Releasable $($suffix)? >]> [< OwnedRef $($suffix)? >]<T> {
    /// Take the releasable from the `OwnedRef`. Returns None if the `OwnedRef` is not the last
    /// reference to the data.
    pub fn take(this: $take_self) -> Option<impl for<'a> [< Releasable $($suffix)? >]<Context<'a> = T::Context<'a>>> {
        this.drop_guard.disarm();
        let inner = Self::inner(&this);
        let previous_count = inner.owned_refs_count.fetch_sub(1, Ordering::Release);
        if previous_count == 1 {
            fence(Ordering::Acquire);
            Some(Self::wait_and_take_inner(this))
        } else {
            None
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for [< OwnedRef $($suffix)? >]<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("[< OwnedRef $($suffix)? >]").field(&Self::inner(self).value).finish()
    }
}

impl<T> Clone for [< OwnedRef $($suffix)? >]<T> {
    /// Clone the `OwnedRef`. Both the current and the new reference needs to be `release`d.
    fn clone(&self) -> Self {
        let inner = Self::inner(self);
        let previous_count = inner.owned_refs_count.fetch_add(1, Ordering::Relaxed);
        debug_assert!(previous_count > 0, "[< OwnedRef $($suffix)? >] should not be used after being released.");
        Self { inner: Some(Arc::clone(inner)), drop_guard: Default::default() }
    }
}

impl<T: [< Releasable $($suffix)? >]> [< Releasable $($suffix)? >] for [< OwnedRef $($suffix)? >]<T> {
    type Context<'a> = T::Context<'a>;

    /// Release the `OwnedRef`. If this is the last instance, this method will block until all
    /// `TempRef` instances are dropped, and will release the underlying object.
    #[allow(unused_mut)]
    fn release(mut self: $self, c: Self::Context<'_>) {
        if let Some(mut value) = [< OwnedRef $($suffix)? >]::take(&mut self) {
            value.release(c);
        }
    }
}

impl<T: Default> Default for [< OwnedRef $($suffix)? >]<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> std::ops::Deref for [< OwnedRef $($suffix)? >]<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &Self::inner(self).deref().value
    }
}

impl<T> std::borrow::Borrow<T> for [< OwnedRef $($suffix)? >]<T> {
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T> std::convert::AsRef<T> for [< OwnedRef $($suffix)? >]<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T: PartialEq> PartialEq for [< OwnedRef $($suffix)? >]<T> {
    fn eq(&self, other: &[< OwnedRef $($suffix)? >]<T>) -> bool {
        Arc::ptr_eq(Self::inner(self), Self::inner(other)) || **self == **other
    }
}

impl<T: Eq> Eq for [< OwnedRef $($suffix)? >]<T> {}

impl<T: PartialOrd> PartialOrd for [< OwnedRef $($suffix)? >]<T> {
    fn partial_cmp(&self, other: &[< OwnedRef $($suffix)? >]<T>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Ord> Ord for [< OwnedRef $($suffix)? >]<T> {
    fn cmp(&self, other: &[< OwnedRef $($suffix)? >]<T>) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Hash> Hash for [< OwnedRef $($suffix)? >]<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl<T> From<&[< OwnedRef $($suffix)? >]<T>> for WeakRef<T> {
    fn from(owner: &[< OwnedRef $($suffix)? >]<T>) -> Self {
        [< OwnedRef $($suffix)? >]::downgrade(owner)
    }
}

impl<'a, T> From<&'a [< OwnedRef $($suffix)? >]<T>> for TempRef<'a, T> {
    fn from(owner: &'a [< OwnedRef $($suffix)? >]<T>) -> Self {
        [< OwnedRef $($suffix)? >]::temp(owner)
    }
}

}}}

/// A weak reference to a shared owned object. The `upgrade` method try to build a `TempRef` from a
/// `WeakRef` and will fail if there is no `OwnedRef` left.
#[derive(Debug)]
pub struct WeakRef<T>(Weak<RefInner<T>>);

impl<T> WeakRef<T> {
    pub fn new() -> Self {
        Self(Weak::new())
    }

    /// Try to upgrade the `WeakRef` into a `TempRef`. This will fail as soon as the last
    /// `OwnedRef` is released, even if some `TempRef` still exist at that time. The returned
    /// `TempRef` must be dropped as soon as possible. In particular, it must not be kept across
    /// blocking calls.
    pub fn upgrade(&self) -> Option<TempRef<'_, T>> {
        if let Some(value) = self.0.upgrade() {
            // As soon as the Arc has been upgraded, creates a `TempRef` to ensure the futex is woken
            // up in case `upgrade` and `release` are racing.
            let temp_ref = TempRef::new(value);
            // Only returns a valid `TempRef` if there are still some un-released `OwnedRef`. As
            // soon as `release` is called, no more `TempRef` can be acquire.
            if temp_ref.0.owned_refs_count.load(Ordering::Acquire) > 0 {
                return Some(temp_ref);
            }
        }
        None
    }

    /// Returns a raw pointer to the object T pointed to by this WeakRef<T>.
    ///
    /// See `Weak::as_ptr`
    pub fn as_ptr(&self) -> *const T {
        let base = self.0.as_ptr();
        let value = memoffset::raw_field!(base, RefInner<T>, value);
        memoffset::raw_field!(value, ReleaseGuard<T>, value)
    }

    /// Returns true if the two objects point to the same allocation
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Self::as_ptr(this) == Self::as_ptr(other)
    }
}

impl<T> Default for WeakRef<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for WeakRef<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// A temporary reference to a shared owned object. This permits access to the shared object, but
/// will block any thread trying to release the last `OwnedRef`. As such, such reference must be
/// released as soon as possible. In particular, one must not do any blocking operation while
/// owning such a refeence.
// Until negative trait bound are implemented, using `*mut u8` to prevent transferring TempRef
// across threads.
pub struct TempRef<'a, T>(Arc<RefInner<T>>, std::marker::PhantomData<(&'a T, *mut u8)>);

impl<'a, T> Drop for TempRef<'a, T> {
    fn drop(&mut self) {
        self.0.dec_temp_ref();
    }
}

impl<'a, T> TempRef<'a, T> {
    /// Build a new TempRef. Ensures `temp_refs_count` is correctly updated.
    fn new(inner: Arc<RefInner<T>>) -> Self {
        inner.inc_temp_ref();
        Self(inner, Default::default())
    }

    /// Provides a raw pointer to the data.
    ///
    /// See `Arc::as_ptr`
    pub fn as_ptr(this: &Self) -> *const T {
        &this.0.value.value as *const T
    }

    /// Returns true if the two objects point to the same allocation
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Self::as_ptr(this) == Self::as_ptr(other)
    }

    /// This allows to change the lifetime annotation of a `TempRef` to static.
    ///
    /// # Safety
    ///
    /// As `TempRef` must be dropped as soon as possible, this provided the way to block the release
    /// of the related `OwnedRef`s and as such is considered unsafe. Any caller must ensure that
    /// the returned `TempRef` is not kept around while doing blocking calls.
    pub unsafe fn into_static(this: Self) -> TempRef<'static, T> {
        TempRef::new(this.0.clone())
    }
}

impl<'a, T> From<&TempRef<'a, T>> for WeakRef<T> {
    fn from(temp_ref: &TempRef<'a, T>) -> Self {
        Self(Arc::downgrade(&temp_ref.0))
    }
}

impl<'a, T> std::ops::Deref for TempRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.deref().value
    }
}

impl<'a, T> std::borrow::Borrow<T> for TempRef<'a, T> {
    fn borrow(&self) -> &T {
        &self.0.deref().value
    }
}

impl<'a, T> std::convert::AsRef<T> for TempRef<'a, T> {
    fn as_ref(&self) -> &T {
        &self.0.deref().value
    }
}

impl<'a, T: PartialEq> PartialEq for TempRef<'a, T> {
    fn eq(&self, other: &TempRef<'_, T>) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || **self == **other
    }
}

impl<'a, T: Eq> Eq for TempRef<'a, T> {}

impl<'a, T: PartialOrd> PartialOrd for TempRef<'a, T> {
    fn partial_cmp(&self, other: &TempRef<'_, T>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<'a, T: Ord> Ord for TempRef<'a, T> {
    fn cmp(&self, other: &TempRef<'_, T>) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<'a, T: Hash> Hash for TempRef<'a, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

/// Wrapper around `TempRef` allowing to use it in a Set or as a key of a Map.
pub struct TempRefKey<'a, T>(pub TempRef<'a, T>);
impl<'a, T> PartialEq for TempRefKey<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        TempRef::ptr_eq(&self.0, &other.0)
    }
}
impl<'a, T> Eq for TempRefKey<'a, T> {}
impl<'a, T> Hash for TempRefKey<'a, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        TempRef::as_ptr(&self.0).hash(state);
    }
}
impl<'a, T> std::ops::Deref for TempRefKey<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

/// A wrapper a round a Releasable object that will check, in test and when assertion are enabled,
/// that the value has been released before being dropped.
#[must_use = "ReleaseGuard must be released"]
pub struct ReleaseGuard<T> {
    /// The wrapped value.
    value: T,

    /// A guard that will ensure a panic on drop if the ref has not been released.
    drop_guard: DropGuard,
}

#[cfg(test)]
impl<T> ReleaseGuard<T> {
    pub fn new_released(value: T) -> Self {
        let result: Self = value.into();
        result.drop_guard.disarm();
        result
    }
}

#[cfg(test)]
impl<T: Default> ReleaseGuard<T> {
    pub fn default_released() -> Self {
        Self::new_released(T::default())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ReleaseGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}

impl<T: Default> Default for ReleaseGuard<T> {
    fn default() -> Self {
        T::default().into()
    }
}

impl<T: Clone> Clone for ReleaseGuard<T> {
    fn clone(&self) -> Self {
        self.value.clone().into()
    }
}

impl<T> From<T> for ReleaseGuard<T> {
    fn from(value: T) -> Self {
        Self { value, drop_guard: Default::default() }
    }
}

impl<T> std::ops::Deref for ReleaseGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> std::ops::DerefMut for ReleaseGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T> std::borrow::Borrow<T> for ReleaseGuard<T> {
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T> std::convert::AsRef<T> for ReleaseGuard<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T: PartialEq> PartialEq for ReleaseGuard<T> {
    fn eq(&self, other: &ReleaseGuard<T>) -> bool {
        **self == **other
    }
}

impl<T: Eq> Eq for ReleaseGuard<T> {}

impl<T: PartialOrd> PartialOrd for ReleaseGuard<T> {
    fn partial_cmp(&self, other: &ReleaseGuard<T>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Ord> Ord for ReleaseGuard<T> {
    fn cmp(&self, other: &ReleaseGuard<T>) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Hash> Hash for ReleaseGuard<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

#[derive(Default, Debug)]
struct DropGuard {
    #[cfg(any(test, debug_assertions))]
    released: std::sync::atomic::AtomicBool,
}

impl DropGuard {
    #[inline(always)]
    fn disarm(&self) {
        #[cfg(any(test, debug_assertions))]
        {
            if self
                .released
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                panic!("Guard was disarmed twice");
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for DropGuard {
    fn drop(&mut self) {
        assert!(*self.released.get_mut());
    }
}
#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Number of `TempRef` in the current thread. This is used to ensure there is no `TempRef`
    /// while doing a blocking operation.
    static TEMP_REF_LOCAL_COUNT: std::cell::RefCell<usize> = std::cell::RefCell::new(0);
}

/// Assert that no temp ref exist on the current thread. This is used before executing a blocking
/// operation to ensure it will not prevent a OwnedRef release.
pub fn debug_assert_no_local_temp_ref() {
    #[cfg(any(test, debug_assertions))]
    {
        TEMP_REF_LOCAL_COUNT.with(|count| {
            assert_eq!(*count.borrow(), 0, "Current threads owns {} TempRef", *count.borrow());
        });
    }
}

/// The internal data of `OwnedRef`/`WeakRef`/`TempRef`.
///
/// To ensure that `wait_for_no_ref_once` is correct, the following constraints must apply:
/// - Once `owned_refs_count` reaches 0, it must never increase again.
/// - The strong count of `Arc<Self>` must always be incremented before `temp_refs_count` is
///   incremented.
/// - Whenever a the strong count of `Arc<Self>` is incremented, `temp_ref_count` must be
///   increased.
/// This ensures that `wait_for_no_ref_once` will always be notified when it is waiting on the
/// `temp_refs_count` futex and the number of `TempRef` reaches 0.
struct RefInner<T> {
    /// The underlying value.
    value: ReleaseGuard<T>,
    /// The number of `OwnedRef` sharing this data.
    owned_refs_count: AtomicUsize,
    /// The number of `TempRef` sharing this data.
    // This is close to a duplicate of the Arc strong_count, and could be replaced by it if this
    // module reimplemented all of Arc/Weak. This can be changed without changing the API if this
    // becomes a performance issue.
    temp_refs_count: zx::sys::zx_futex_t,
}

impl<T> RefInner<T> {
    fn new(value: T) -> Self {
        Self {
            value: value.into(),
            owned_refs_count: AtomicUsize::new(1),
            temp_refs_count: 0.into(),
        }
    }

    /// Increase `temp_refs_count`. Must be called each time a new `TempRef` is built.
    fn inc_temp_ref(&self) {
        self.temp_refs_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(any(test, debug_assertions))]
        {
            TEMP_REF_LOCAL_COUNT.with(|count| {
                *count.borrow_mut() += 1;
            });
        }
    }

    /// Decrease `temp_refs_count`. Must be called each time a new `TempRef` is dropped.
    ///
    /// This will wake the futex on `temp_refs_count` when it reaches 0.
    fn dec_temp_ref(&self) {
        let previous_count = self.temp_refs_count.fetch_sub(1, Ordering::Release);
        if previous_count == 1 {
            fence(Ordering::Acquire);
            unsafe {
                // SAFETY: This is a ffi call to a zircon syscall.
                let result = zx::sys::zx_futex_wake(&self.temp_refs_count, 1);
                debug_assert_eq!(result, zx::sys::ZX_OK);
            }
        }
        #[cfg(any(test, debug_assertions))]
        {
            TEMP_REF_LOCAL_COUNT.with(|count| {
                *count.borrow_mut() -= 1;
            });
        }
    }

    /// Wait for `temp_refs_count` to reach 0 once using the futex.
    fn wait_for_no_ref_once(self: &Arc<Self>) {
        // Compute the current number of temp refs, and wait for it to drop to 0.
        let current_value = self.temp_refs_count.load(Ordering::Acquire);
        if current_value == 0 {
            // It is already 0, return.
            return;
        }
        // Otherwise, wait on the futex that will be waken up when the number of temp_ref drops
        // to 0.
        unsafe {
            // SAFETY: This is a ffi call to a zircon syscall.
            let result = zx::sys::zx_futex_wait(
                &self.temp_refs_count,
                current_value.into(),
                zx::sys::ZX_HANDLE_INVALID,
                zx::Time::INFINITE.into_nanos(),
            );
            debug_assert!(
                result == zx::sys::ZX_OK || result == zx::sys::ZX_ERR_BAD_STATE,
                "Unexpected result: {result}"
            );
        }
    }
}

impl<T> OwnedRefByRef<T> {
    /// Wait for this `OwnedRefByRef` to be the only left reference to the data. This should only
    /// be called on once the last `OwnedRefByRef` has been released. This will wait for all
    /// existing `TempRef >]` to be dropped before returning.
    fn wait_and_take_inner(self: &Self) -> Arc<RefInner<T>> {
        let inner = Self::inner(self);
        loop {
            // Ensure no more `OwnedRefByRef` exists.
            debug_assert_eq!(inner.owned_refs_count.load(Ordering::Acquire), 0);
            // If the strong count of the Arc is 1, there is no existing real `TempRef`. While
            // some can be temporarily created in the `WeakRef::upgrade` method, they will be
            // dropped immediately because `owned_refs_count` is 0. To be noted: the count might be
            // greater than 1 because of one of these temporary `TempRef`, but the futex call
            // will be woken up when they are dropped. Return.
            if Arc::strong_count(inner) == 1 {
                return Arc::clone(inner);
            }

            inner.wait_for_no_ref_once();
        }
    }
}

impl<T: ReleasableByRef> ReleasableByRef for Arc<RefInner<T>> {
    type Context<'a> = T::Context<'a>;
    fn release(self: &Self, c: Self::Context<'_>) {
        self.value.release(c);
    }
}

impl<T> OwnedRefByMut<T> {
    /// Wait for this `OwnedRefByMut` to be the only left reference to the data. This should only
    /// be called on once the last `OwnedRefByMut` has been released. This will wait for all
    /// existing `TempRef >]` to be dropped before returning.
    fn wait_and_take_inner(self: &mut Self) -> ReleaseGuard<T> {
        let mut inner = self.inner.take().expect("OwnedRef has been released.");
        loop {
            // Ensure no more `OwnedRefByMut` exists.
            debug_assert_eq!(inner.owned_refs_count.load(Ordering::Acquire), 0);
            match Arc::try_unwrap(inner) {
                Ok(value) => return value.value,
                Err(value) => inner = value,
            }
            inner.wait_for_no_ref_once();
        }
    }
}

impl<T> OwnedRef<T> {
    /// Wait for this `OwnedRef` to be the only left reference to the data. This should only be
    /// called on once the last `OwnedRef` has been released. This will wait for all existing
    /// `TempRef >]` to be dropped before returning.
    fn wait_and_take_inner(self: &mut Self) -> ReleaseGuard<T> {
        let mut inner = self.inner.take().expect("OwnedRef has been released.");
        loop {
            // Ensure no more `OwnedRef` exists.
            debug_assert_eq!(inner.owned_refs_count.load(Ordering::Acquire), 0);
            match Arc::try_unwrap(inner) {
                Ok(value) => return value.value,
                Err(value) => inner = value,
            }
            inner.wait_for_no_ref_once();
        }
    }
}

make_ownership_types!(ByRef, &Self, &Self);
make_ownership_types!(ByMut, &mut Self, &mut Self);
make_ownership_types!(, Self, &mut Self);

/// Macro that ensure the releasable is released with the given context if the body returns an
/// error.
macro_rules! release_on_error {
    ($releasable_name:ident, $context:expr, $body:block ) => {{
        #[allow(clippy::redundant_closure_call)]
        let result = { (|| $body)() };
        match result {
            Err(e) => {
                $releasable_name.release($context);
                return Err(e);
            }
            Ok(x) => x,
        }
    }};
}

/// Macro that ensure the releasable is released with the given context after the body returns.
macro_rules! release_after {
    ($releasable_name:ident, $context:expr, $(|| -> $output_type:ty)? $body:block ) => {{
        #[allow(clippy::redundant_closure_call)]
        let result = { (|| $(-> $output_type)? { $body })() };
        $releasable_name.release($context);
        result
    }};
}

pub mod internal {
    pub async fn async_try<E>(block: impl std::future::Future<Output = E>) -> E {
        block.await
    }
}

/// Macro that ensure the releasable is released with the given context after the block terminates,
/// whether there is an error or not.
macro_rules! async_release_after {
    ($releasable_name:ident, $context:expr, $(|| -> $output_type:ty)? $body:block ) => {{
        let result =
            crate::types::ownership::internal::async_try$(::<$output_type>)?(async { $body }).await;
        $releasable_name.release($context);
        result
    }};
}

pub(crate) use async_release_after;
pub(crate) use release_after;
pub(crate) use release_on_error;

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Default)]
    struct Data;

    impl ReleasableByRef for Data {
        type Context<'a> = ();
        fn release(&self, _: ()) {}
    }
    impl ReleasableByMut for Data {
        type Context<'a> = ();
        fn release(&mut self, _: ()) {}
    }
    impl Releasable for Data {
        type Context<'a> = ();
        fn release(self, _: ()) {}
    }

    #[derive(Default)]
    struct DataWithMutableReleaseContext;

    impl ReleasableByRef for DataWithMutableReleaseContext {
        type Context<'a> = &'a mut ();
        fn release(&self, _: &mut ()) {}
    }

    #[::fuchsia::test]
    #[should_panic]
    fn drop_without_release() {
        let _ = OwnedRefByRef::new(Data {});
    }

    #[::fuchsia::test]
    fn test_creation_and_reference_by_ref() {
        let value = OwnedRefByRef::new(Data {});
        let reference = WeakRef::from(&value);
        reference.upgrade().expect("upgrade");
        value.release(());
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_creation_and_reference_by_mut() {
        let mut value = OwnedRefByMut::new(Data {});
        let reference = WeakRef::from(&value);
        reference.upgrade().expect("upgrade");
        value.release(());
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_creation_and_reference_by_val() {
        let value = OwnedRef::new(Data {});
        let reference = WeakRef::from(&value);
        reference.upgrade().expect("upgrade");
        value.release(());
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_clone() {
        let value = OwnedRefByRef::new(Data {});
        {
            let value2 = OwnedRefByRef::clone(&value);
            value2.release(());
        }
        #[allow(clippy::redundant_clone)]
        {
            let reference = WeakRef::from(&value);
            let _reference2 = reference.clone();
        }
        value.release(());
    }

    #[::fuchsia::test]
    fn test_default() {
        let reference = WeakRef::<Data>::default();
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_release_on_error() {
        fn release_on_error() -> Result<(), ()> {
            let value = OwnedRefByRef::new(Data {});
            release_on_error!(value, (), {
                if true {
                    return Err(());
                }
                Ok(())
            });
            Ok(())
        }
        assert_eq!(release_on_error(), Err(()));
    }

    #[::fuchsia::test]
    fn test_into_static() {
        let value = OwnedRefByRef::new(Data {});
        let weak = WeakRef::from(&value);
        // SAFETY: This is safe, as static_ref remains on the stack.
        let static_ref = unsafe { TempRef::into_static(weak.upgrade().unwrap()) };
        // Check that weak can now be dropped.
        std::mem::drop(weak);
        // Drop static_ref
        std::mem::drop(static_ref);
        value.release(());
    }

    #[::fuchsia::test]
    fn test_debug_assert_no_local_temp_ref() {
        debug_assert_no_local_temp_ref();
        let value = OwnedRefByRef::new(Data {});
        debug_assert_no_local_temp_ref();
        let _temp_ref = OwnedRefByRef::temp(&value);
        std::thread::spawn(|| {
            debug_assert_no_local_temp_ref();
        })
        .join()
        .expect("join");
        std::mem::drop(_temp_ref);
        debug_assert_no_local_temp_ref();
        value.release(());
        debug_assert_no_local_temp_ref();
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_debug_assert_no_local_temp_ref_aborts() {
        let value = OwnedRefByRef::new(Data {});
        {
            let _temp_ref = OwnedRefByRef::temp(&value);
            debug_assert_no_local_temp_ref();
        }
        // This code should not be reached, but ensures the test will fail is
        // `debug_assert_no_local_temp_ref` fails to panic.
        value.release(());
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_clone_released_owned_ref_abort_in_test() {
        let value = OwnedRefByRef::new(Data {});
        value.release(());
        let _ = OwnedRefByRef::clone(&value);
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_unrelease_release_guard() {
        let _value = ReleaseGuard::<Data>::default();
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_double_release_release_guard() {
        let value = ReleaseGuard::<Data>::default();
        ReleasableByRef::release(&value, ());
        ReleasableByRef::release(&value, ());
    }

    #[::fuchsia::test]
    fn test_released_release_guard() {
        let _value = ReleaseGuard::<Data>::default_released();
    }

    #[::fuchsia::test]
    fn release_with_mutable_context() {
        let value = OwnedRefByRef::new(DataWithMutableReleaseContext {});
        let mut context = ();
        value.release(&mut context);
    }

    // If this test fails, it will almost always be with a very low probability. Any failure is a
    // real, high priority bug.
    #[::fuchsia::test]
    fn upgrade_while_release() {
        let value = OwnedRefByRef::new(Data {});
        // Run 10 threads trying to upgrade a weak pointer in a loop.
        for _ in 0..10 {
            std::thread::spawn({
                let weak = OwnedRefByRef::downgrade(&value);
                move || loop {
                    if weak.upgrade().is_none() {
                        return;
                    }
                }
            });
        }
        // Release the value after letting the threads make some progress.
        std::thread::sleep(std::time::Duration::from_millis(10));
        value.release(());
        // The test must finish, and no assertion should trigger.
    }

    #[::fuchsia::test]
    fn new_cyclic() {
        let mut weak_value = None;
        let value = OwnedRefByRef::new_cyclic(|weak| {
            weak_value = Some(weak);
            Data {}
        });
        let weak_value = weak_value.expect("weak_value");
        assert!(weak_value.upgrade().is_some());
        value.release(());
        assert!(weak_value.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn as_ptr() {
        let value = OwnedRefByRef::new(Data {});
        let weak = OwnedRefByRef::downgrade(&value);
        let temp = weak.upgrade().expect("upgrade");
        assert_eq!(OwnedRefByRef::as_ptr(&value), weak.as_ptr());
        assert_eq!(OwnedRefByRef::as_ptr(&value), TempRef::as_ptr(&temp));
        std::mem::drop(temp);
        value.release(());
    }
}
