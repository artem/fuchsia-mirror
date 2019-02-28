//! Definition of the `JoinAll` combinator, waiting for all of a list of futures
//! to finish.

use std::fmt;
use std::future::Future;
use std::iter::FromIterator;
use std::mem;
use std::pin::Pin;
use std::prelude::v1::*;
use std::task::Poll;

#[derive(Debug)]
enum ElemState<F>
where
    F: Future,
{
    Pending(F),
    Done(Option<F::Output>),
}

impl<F> ElemState<F>
where
    F: Future,
{
    fn pending_pin_mut<'a>(self: Pin<&'a mut Self>) -> Option<Pin<&'a mut F>> {
        // Safety: Basic enum pin projection, no drop + optionally Unpin based
        // on the type of this variant
        match unsafe { self.get_unchecked_mut() } {
            ElemState::Pending(f) => Some(unsafe { Pin::new_unchecked(f) }),
            ElemState::Done(_) => None,
        }
    }

    fn take_done(self: Pin<&mut Self>) -> Option<F::Output> {
        // Safety: Going from pin to a variant we never pin-project
        match unsafe { self.get_unchecked_mut() } {
            ElemState::Pending(_) => None,
            ElemState::Done(output) => output.take(),
        }
    }
}

impl<F> Unpin for ElemState<F>
where
    F: Future + Unpin,
{
}

fn iter_pin_mut<T>(slice: Pin<&mut [T]>) -> impl Iterator<Item = Pin<&mut T>> {
    // Safety: `std` _could_ make this unsound if it were to decide Pin's
    // invariants aren't required to transmit through slices. Otherwise this has
    // the same safety as a normal field pin projection.
    unsafe { slice.get_unchecked_mut() }
        .iter_mut()
        .map(|t| unsafe { Pin::new_unchecked(t) })
}

/// A future which takes a list of futures and resolves with a vector of the
/// completed values.
///
/// This future is created with the `join_all` function.
#[must_use = "futures do nothing unless polled"]
pub struct JoinAll<F>
where
    F: Future,
{
    elems: Pin<Box<[ElemState<F>]>>,
}

impl<F> fmt::Debug for JoinAll<F>
where
    F: Future + fmt::Debug,
    F::Output: fmt::Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("JoinAll")
            .field("elems", &self.elems)
            .finish()
    }
}

/// Creates a future which represents a collection of the outputs of the futures
/// given.
///
/// The returned future will drive execution for all of its underlying futures,
/// collecting the results into a destination `Vec<T>` in the same order as they
/// were provided.
///
/// # See Also
///
/// This is purposefully a very simple API for basic use-cases. In a lot of
/// cases you will want to use the more powerful
/// [`FuturesUnordered`][crate::stream::FuturesUnordered] APIs, some
/// examples of additional functionality that provides:
///
///  * Adding new futures to the set even after it has been started.
///
///  * Only polling the specific futures that have been woken. In cases where
///    you have a lot of futures this will result in much more efficient polling.
///
/// # Examples
///
/// ```
/// #![feature(async_await, await_macro, futures_api)]
/// # futures::executor::block_on(async {
/// use futures::future::{join_all};
///
/// async fn foo(i: u32) -> u32 { i }
///
/// let futures = vec![foo(1), foo(2), foo(3)];
///
/// assert_eq!(await!(join_all(futures)), [1, 2, 3]);
/// # });
/// ```
pub fn join_all<I>(i: I) -> JoinAll<I::Item>
where
    I: IntoIterator,
    I::Item: Future,
{
    let elems: Box<[_]> = i.into_iter().map(ElemState::Pending).collect();
    JoinAll { elems: Box::into_pin(elems) }
}

impl<F> Future for JoinAll<F>
where
    F: Future,
{
    type Output = Vec<F::Output>;

    fn poll(
        mut self: Pin<&mut Self>,
        waker: &::std::task::Waker,
    ) -> Poll<Self::Output> {
        let mut all_done = true;

        for mut elem in iter_pin_mut(self.elems.as_mut()) {
            if let Some(pending) = elem.as_mut().pending_pin_mut() {
                if let Poll::Ready(output) = pending.poll(waker) {
                    elem.set(ElemState::Done(Some(output)));
                } else {
                    all_done = false;
                }
            }
        }

        if all_done {
            let mut elems = mem::replace(&mut self.elems, Box::pin([]));
            let result = iter_pin_mut(elems.as_mut())
                .map(|e| e.take_done().unwrap())
                .collect();
            Poll::Ready(result)
        } else {
            Poll::Pending
        }
    }
}

impl<F: Future> FromIterator<F> for JoinAll<F> {
    fn from_iter<T: IntoIterator<Item = F>>(iter: T) -> Self {
        join_all(iter)
    }
}
