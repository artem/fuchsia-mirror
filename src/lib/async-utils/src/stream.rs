// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//! Streams always signal exhaustion with `None` return values. A stream epitaph can be used when
//! a specific value is desired as the last item returned by a stream before it is exhausted.
//!
//! Example usecase: often streams will be used without having direct access to the stream itself
//! such as from a `streammap::StreamMap` or a `futures::stream::FuturesUnordered`. Occasionally,
//! it is necessary to perform some cleanup procedure outside of a stream when it is exhausted. An
//! `epitaph` can be used to uniquely identify which stream has ended within a collection of
//! streams.

use {
    core::{
        pin::Pin,
        task::{Context, Poll},
    },
    futures::{
        stream::{FusedStream, Stream},
        Future,
    },
    pin_project::pin_project,
};

mod flatten_unordered;
mod future_map;
mod one_or_many;
mod stream_map;

pub use flatten_unordered::{
    FlattenUnordered, FlattenUnorderedExt, TryFlattenUnordered, TryFlattenUnorderedExt,
};
pub use future_map::FutureMap;
pub use one_or_many::OneOrMany;
pub use stream_map::StreamMap;

/// Values returned from a stream with an epitaph are of type `StreamItem`.
#[derive(Debug, PartialEq)]
pub enum StreamItem<T, E> {
    /// Item polled from the underlying `Stream`
    Item(T),
    /// Epitaph value returned after the underlying `Stream` is exhausted.
    Epitaph(E),
}

/// A `Stream` that returns the values of the wrapped stream until the wrapped stream is exhausted.
/// Then it returns a single epitaph value before being exhausted
#[cfg_attr(test, derive(Debug))]
pub struct StreamWithEpitaph<S, E> {
    inner: S,
    epitaph: Option<E>,
}

impl<S, E> StreamWithEpitaph<S, E> {
    /// Provide immutable access to the inner stream.
    /// This is safe as if the stream were being polled, we would not be able to access a
    /// reference to self to pass to this method.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Provide mutable access to the inner stream.
    /// This is safe as if the stream were being polled, we would not be able to access a mutable
    /// reference to self to pass to this method.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }
}

// The `Unpin` bounds are not strictly necessary, but make for a more convenient
// implementation. The bounds can be relaxed if !Unpin support is desired.
impl<S, T, E> Stream for StreamWithEpitaph<S, E>
where
    S: Stream<Item = T> + Unpin,
    E: Unpin,
    T: Unpin,
{
    type Item = StreamItem<T, E>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.epitaph.is_none() {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(None) => {
                let this = self.get_mut();
                let ep = this.epitaph.take().map(StreamItem::Epitaph);
                assert!(ep.is_some(), "epitaph must be present if stream is not terminated");
                Poll::Ready(ep)
            }
            Poll::Ready(item) => Poll::Ready(item.map(StreamItem::Item)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, T, E> FusedStream for StreamWithEpitaph<S, E>
where
    S: Stream<Item = T> + FusedStream + Unpin,
    E: Unpin,
    T: Unpin,
{
    fn is_terminated(&self) -> bool {
        self.epitaph.is_none()
    }
}

/// Extension trait to allow for easy creation of a `StreamWithEpitaph` from a `Stream`.
pub trait WithEpitaph: Sized {
    /// Map this stream to one producing a `StreamItem::Item` value for each item of the stream
    /// followed by a single `StreamItem::Epitaph` value with the provided `epitaph`.
    fn with_epitaph<E>(self, epitaph: E) -> StreamWithEpitaph<Self, E>;
}

impl<T> WithEpitaph for T
where
    T: Stream,
{
    fn with_epitaph<E>(self, epitaph: E) -> StreamWithEpitaph<T, E> {
        StreamWithEpitaph { inner: self, epitaph: Some(epitaph) }
    }
}

/// A Stream where each yielded item is tagged with a uniform key
/// Items yielded are (K, St::Item)
///
/// Tagged streams can be easily created by using the `.tagged()` function on the `WithTag` trait.
/// The stream produced by:
///   stream.tagged(k)
/// is equivalent to that created by
///   stream.map(move |v|, (k.clone(), v)
/// BUT the Tagged type combinator provides a statically nameable type that can easily be expressed
/// in type signatures such as `IndexedStreams` below.
#[cfg_attr(test, derive(Debug))]
#[pin_project]
pub struct Tagged<K, St> {
    tag: K,
    #[pin]
    stream: St,
}

impl<K: Clone, St> Tagged<K, St> {
    /// Get a clone of the tag associated with this `Stream`.
    pub fn tag(&self) -> K {
        self.tag.clone()
    }
}

/// Extension trait to allow for easy creation of a `Tagged` stream from a `Stream`.
pub trait WithTag: Sized {
    /// Produce a new stream from this one which yields item tupled with a constant tag
    fn tagged<T>(self, tag: T) -> Tagged<T, Self>;
}

impl<St: Sized> WithTag for St {
    fn tagged<T>(self, tag: T) -> Tagged<T, Self> {
        Tagged { tag, stream: self }
    }
}

impl<K: Clone, Fut: Future> Future for Tagged<K, Fut> {
    type Output = (K, Fut::Output);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let k = self.tag.clone();
        match self.project().stream.poll(cx) {
            Poll::Ready(out) => Poll::Ready((k, out)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<K: Clone, St: Stream> Stream for Tagged<K, St> {
    type Item = (K, St::Item);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let k = self.tag.clone();
        match self.project().stream.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some((k, item))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Convenient alias for a collection of Streams indexed by key where each message is tagged and
/// stream termination is notified by key. This is especially useful for maintaining a collection
/// of fidl client request streams, and being notified when each terminates
pub type IndexedStreams<K, St> = StreamMap<K, StreamWithEpitaph<Tagged<K, St>, K>>;

#[cfg(test)]
mod test {
    //! We validate the behavior of the StreamMap stream by enumerating all possible external
    //! events, and then generating permutations of valid sequences of those events. These model
    //! the possible executions sequences the stream could go through in program execution. We
    //! then assert that:
    //!   a) At all points during execution, all invariants are held
    //!   b) The final result is as expected
    //!
    //! In this case, the invariants are:
    //!   * If the map is empty, it is pending
    //!   * If all streams are pending, the map is pending
    //!   * otherwise the map is ready
    //!
    //! The result is:
    //!   * All test messages have been injected
    //!   * All test messages have been yielded
    //!   * All test streams have terminated
    //!   * No event is yielded with a given key after the stream for that key has terminated
    //!
    //! Together these show:
    //!   * Progress is always eventually made - the Stream cannot be stalled
    //!   * All inserted elements will eventually be yielded
    //!   * Elements are never duplicated
    use {
        super::*,
        core::hash::Hash,
        fuchsia_async as fasync,
        futures::{
            channel::mpsc,
            future::ready,
            stream::{empty, iter, once, Empty, StreamExt},
        },
        proptest::prelude::*,
        std::{collections::HashSet, fmt::Debug},
    };

    #[fasync::run_until_stalled(test)]
    async fn empty_stream_returns_epitaph_only() {
        let s: Empty<i32> = empty();
        let s = s.with_epitaph(0i64);
        let actual: Vec<_> = s.collect().await;
        let expected = vec![StreamItem::Epitaph(0i64)];
        assert_eq!(actual, expected);
    }

    #[fasync::run_until_stalled(test)]
    async fn populated_stream_returns_items_and_epitaph() {
        let s = iter(0i32..3).fuse().with_epitaph(3i64);
        let actual: Vec<_> = StreamExt::collect::<Vec<_>>(s).await;
        let expected = vec![
            StreamItem::Item(0),
            StreamItem::Item(1),
            StreamItem::Item(2),
            StreamItem::Epitaph(3i64),
        ];
        assert_eq!(actual, expected);
    }

    #[fasync::run_until_stalled(test)]
    async fn stream_is_terminated_after_end() {
        let mut s = once(ready(0i32)).with_epitaph(3i64);
        assert_eq!(s.next().await, Some(StreamItem::Item(0)));
        assert_eq!(s.next().await, Some(StreamItem::Epitaph(3)));
        assert!(s.is_terminated());
    }

    // We validate the behavior of the StreamMap stream by enumerating all possible external
    // events, and then generating permutations of valid sequences of those events. These model
    // the possible executions sequences the stream could go through in program execution. We
    // then assert that:
    //   a) At all points during execution, all invariants are held
    //   b) The final result is as expected
    //
    // In this case, the invariants are:
    //   * If the map is empty, it is pending
    //   * If all streams are pending, the map is pending
    //   * otherwise the map is ready
    //
    // The result is:
    //   * All test messages have been injected
    //   * All test messages have been yielded
    //   * All test streams have terminated
    //   * No event is yielded with a given key after the stream for that key has terminated
    //
    // Together these show:
    //   * Progress is always eventually made - the Stream cannot be stalled
    //   * All inserted elements will eventually be yielded
    //   * Elements are never duplicated

    /// Possible actions to take in evaluating the stream
    enum Event<K> {
        /// Insert a new request stream
        InsertStream(K, mpsc::Receiver<Result<u64, ()>>),
        /// Send a new request
        SendRequest(K, mpsc::Sender<Result<u64, ()>>),
        /// Close an existing request stream
        CloseStream(K, mpsc::Sender<Result<u64, ()>>),
        /// Schedule the executor. The executor will only run the task if awoken, otherwise it will
        /// do nothing
        Execute,
    }

    impl<K: Debug> Debug for Event<K> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Event::InsertStream(k, _) => write!(f, "InsertStream({:?})", k),
                Event::SendRequest(k, _) => write!(f, "SendRequest({:?})", k),
                Event::CloseStream(k, _) => write!(f, "CloseStream({:?})", k),
                Event::Execute => write!(f, "Execute"),
            }
        }
    }

    fn stream_events<K: Clone + Eq + Hash>(key: K) -> Vec<Event<K>> {
        // Ensure that the channel is big enough to always handle all the Sends we make
        let (sender, receiver) = mpsc::channel::<Result<u64, ()>>(10);
        vec![
            Event::InsertStream(key.clone(), receiver),
            Event::SendRequest(key.clone(), sender.clone()),
            Event::CloseStream(key, sender),
        ]
    }

    /// Determine how many events are sent on open channels (a channel is open if it has not been
    /// closed, even if it has not yet been inserted into the StreamMap)
    fn expected_yield<K: Eq + Hash>(events: &Vec<Event<K>>) -> usize {
        events
            .iter()
            .fold((HashSet::new(), 0), |(mut terminated, closed), event| match event {
                Event::CloseStream(k, _) => {
                    let _: bool = terminated.insert(k);
                    (terminated, closed)
                }
                Event::SendRequest(k, _) if !terminated.contains(k) => (terminated, closed + 1),
                _ => (terminated, closed),
            })
            .1
    }

    /// Strategy that produces random permutations of a set of events, corresponding to inserting,
    /// sending and completing up to n different streams in random order, also interspersed with
    /// running the executor
    fn execution_sequences(n: u64) -> impl Strategy<Value = Vec<Event<u64>>> {
        fn generate_events(n: u64) -> Vec<Event<u64>> {
            let mut events = (0..n).flat_map(|n| stream_events(n)).collect::<Vec<_>>();
            events.extend(std::iter::repeat_with(|| Event::Execute).take((n * 3) as usize));
            events
        }

        // We want to produce random permutations of these events
        (0..n).prop_map(generate_events).prop_shuffle()
    }

    proptest! {
        #[test]
        fn test_invariants(mut execution in execution_sequences(4)) {
            let expected = expected_yield(&execution);
            let expected_count:u64 = execution.iter()
                .filter(|event| match event {
                    Event::CloseStream(_, _) => true,
                    _ => false,
                }).count() as u64;

            // Add enough execution events to ensure we will complete, no matter the order
            execution.extend(std::iter::repeat_with(|| Event::Execute).take((expected_count * 3) as usize));

            let (waker, count) = futures_test::task::new_count_waker();
            let send_waker = futures_test::task::noop_waker();
            let mut streams = StreamMap::empty();
            let mut next_wake = 0;
            let mut yielded = 0;
            let mut inserted = 0;
            let mut closed = 0;
            let mut events = vec![];
            for event in execution {
                match event {
                    Event::InsertStream(key, stream) => {
                        assert_matches::assert_matches!(streams.insert(key, stream.tagged(key).with_epitaph(key)), None);
                        // StreamMap does *not* wake on inserting new streams, matching the
                        // behavior of streams::SelectAll. The client *must* arrange for it to be
                        // polled again after a stream is inserted; we model that here by forcing a
                        // wake up
                        next_wake = count.get();
                    }
                    Event::SendRequest(_, mut sender) => {
                        if let Poll::Ready(Ok(())) = sender.poll_ready(&mut Context::from_waker(&send_waker)) {
                            prop_assert_eq!(sender.start_send(Ok(1)), Ok(()));
                            inserted = inserted + 1;
                        }
                    }
                    Event::CloseStream(_, mut stream) => {
                        stream.close_channel();
                    }
                    Event::Execute if count.get() >= next_wake => {
                        match Pin::new(&mut streams.next()).poll(&mut Context::from_waker(&waker)) {
                            Poll::Ready(Some(StreamItem::Item((k, v)))) => {
                                events.push(StreamItem::Item((k, v)));
                                yielded = yielded + 1;
                                // Ensure that we wake up next time;
                                next_wake = count.get();
                                // Invariant: stream(k) must be in the map
                                prop_assert!(streams.contains_key(&k))
                            }
                            Poll::Ready(Some(StreamItem::Epitaph(k))) => {
                                events.push(StreamItem::Epitaph(k));
                                closed = closed + 1;
                                // Ensure that we wake up next time;
                                next_wake = count.get();
                                // stream(k) is now terminated, but until polled again (Yielding
                                // `None`), will still be in the map
                            }
                            Poll::Ready(None) => {
                                // the Stream impl for StreamMap never completes
                                unreachable!()
                            }
                            Poll::Pending => {
                                next_wake = count.get() + 1;
                            }
                        };
                    }
                    Event::Execute => (),
                }
            }
            prop_assert_eq!(inserted, expected, "All expected requests inserted");
            prop_assert_eq!((next_wake, count.get(), yielded), (next_wake, count.get(), expected), "All expected requests yielded");
            prop_assert_eq!(closed, expected_count, "All streams closed");
            let not_terminated =
                |key: u64, e: &StreamItem<(u64, Result<u64, ()>), u64>| match e {
                    StreamItem::Epitaph(k) if *k == key => false,
                    _ => true,
                };
            let event_of =
                |key: u64, e: &StreamItem<(u64, Result<u64, ()>), u64>| match e {
                    StreamItem::Item((k, _)) if *k == key => true,
                    _ => false,
                };
            let all_keys = 0..expected_count;
            for k in all_keys {
                prop_assert!(!streams.contains_key(&k), "All streams should now have been removed");
                prop_assert!(!events.iter().skip_while(|e| not_terminated(k, e)).any(|e| event_of(k, e)), "No events should have been yielded from a stream after it terminated");
            }
        }
    }
}
