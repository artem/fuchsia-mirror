// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::cell::RefCell;
use std::rc::Rc;
use tokio::task::LocalSet;

thread_local!(
    static LOCAL_EXECUTOR: RefCell<Option<Rc<LocalSet>>> = RefCell::new(None)
);

pub mod task {
    use super::LOCAL_EXECUTOR;
    use core::task::{Context, Poll};
    use std::future::Future;
    use std::pin::Pin;

    use futures::FutureExt;

    /// A handle to a task.
    ///
    /// A task can be polled for the output of the future it is executing. A
    /// dropped task will be cancelled after dropping. To immediately cancel a
    /// task, call the cancel() method. To run a task to completion without
    /// retaining the Task handle, call the detach() method.
    #[derive(Debug)]
    pub struct Task<T> {
        task: tokio::task::JoinHandle<T>,
        abort_on_drop: bool,
    }

    impl<T: 'static> Task<T> {
        /// Spawn a new `Send` task onto the current executor.
        ///
        /// # Panics
        ///
        /// `spawn` may panic if not called in the context of an executor (e.g.
        /// within a call to `run` or `run_singlethreaded`).
        pub fn spawn(fut: impl Future<Output = T> + Send + 'static) -> Self
        where
            T: Send,
        {
            let task = LOCAL_EXECUTOR.with(|e| {
                if let Some(e) = e.borrow().as_ref() {
                    e.spawn_local(fut)
                } else {
                    tokio::task::spawn(fut)
                }
            });
            Self { task, abort_on_drop: true }
        }

        /// Spawn a new non-`Send` task onto the single threaded executor.
        ///
        /// # Panics
        ///
        /// `local` may panic if not called in the context of a local executor
        /// (e.g. within a call to `run` or `run_singlethreaded`).
        pub fn local<'a>(fut: impl Future<Output = T> + 'static) -> Self {
            let task = LOCAL_EXECUTOR.with(|e| {
                e.borrow().as_ref().expect("Executor must be created first").spawn_local(fut)
            });
            Self { task, abort_on_drop: true }
        }

        /// detach the Task handle. The contained future will be polled until completion.
        pub fn detach(mut self) {
            self.abort_on_drop = false;
        }

        /// cancel a task and wait for cancellation to complete.
        pub async fn cancel(mut self) -> Option<T> {
            self.task.abort();
            let res = (&mut self.task).await;

            match res {
                Ok(value) => Some(value),
                Err(err) => {
                    if err.is_panic() {
                        // Propagate panic
                        std::panic::resume_unwind(err.into_panic());
                    }
                    None
                }
            }
        }
    }

    impl<T> Future for Task<T> {
        type Output = T;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            use futures_lite::FutureExt;
            match self.task.poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(err)) => {
                    if err.is_panic() {
                        // Propagate panic
                        std::panic::resume_unwind(err.into_panic());
                    } else {
                        // All codepaths for canceling/aborting a task consume said task.
                        // It will not be polled afterwards, and if it is there's something very
                        // wrong going on.
                        unreachable!("Task was polled after being cancelled");
                    }
                }
                Poll::Ready(Ok(v)) => Poll::Ready(v),
            }
        }
    }

    impl<T> Drop for Task<T> {
        fn drop(&mut self) {
            if self.abort_on_drop {
                self.task.abort();
            }
        }
    }

    /// Offload a blocking function call onto a different thread.
    ///
    /// This function can be called from an asynchronous function without blocking
    /// it, returning a future that can be `.await`ed normally. The provided
    /// function should contain at least one blocking operation, such as:
    ///
    /// - A synchronous syscall that does not yet have an async counterpart.
    /// - A compute operation which risks blocking the executor for an unacceptable
    ///   amount of time.
    ///
    /// If neither of these conditions are satisfied, just call the function normally,
    /// as synchronous functions themselves are allowed within an async context,
    /// as long as they are not blocking.
    ///
    /// If you have an async function that may block, refactor the function such that
    /// the blocking operations are offloaded onto the function passed to [`unblock`].
    ///
    /// NOTE: Synchronous functions cannot be cancelled and may keep running after
    /// the returned future is dropped. As a result, resources held by the function
    /// should be assumed to be held until the returned future completes.
    ///
    /// For details on performance characteristics and edge cases, see [`blocking::unblock`].
    pub fn unblock<T: 'static + Send>(
        f: impl 'static + Send + FnOnce() -> T,
    ) -> impl 'static + Send + Future<Output = T> {
        tokio::task::spawn_blocking(f).map(|res| res.unwrap())
    }
}

pub mod executor {
    use super::LOCAL_EXECUTOR;

    use crate::runtime::WakeupTime;
    use std::{
        future::Future,
        ops::{Deref, DerefMut},
        rc::Rc,
    };

    pub use std::time::Duration;
    /// A time relative to the executor's clock.
    pub use std::time::Instant as Time;
    use tokio::task::LocalSet;

    impl WakeupTime for Time {
        fn into_time(self) -> Time {
            self
        }
    }

    /// A multi-threaded executor.
    ///
    /// Mostly API-compatible with the Fuchsia variant.  This differs from Fuchsia in one important
    /// regard: tasks can only be spawned whilst the executor is running, whereas Fuchsia will allow
    /// you to spawn tasks before the executor is running.  LocalExecutor does not have this
    /// limitation.
    ///
    /// The current implementation of Executor does not isolate work (as the underlying executor is
    /// not yet capable of this).
    pub struct SendExecutor {
        num_threads: usize,
    }

    impl SendExecutor {
        /// Create a new executor running with actual time.
        pub fn new(num_threads: usize) -> Self {
            Self { num_threads }
        }

        /// Run a single future to completion using multiple threads.
        pub fn run<F>(&mut self, main_future: F) -> F::Output
        where
            F: Future + Send + 'static,
            F::Output: Send + 'static,
        {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(self.num_threads)
                .enable_io()
                .build()
                .expect("Could not start tokio runtime on current thread");

            rt.block_on(main_future)
        }
    }

    /// A single-threaded executor.
    ///
    /// API-compatible with the Fuchsia variant with the exception of testing APIs.
    ///
    /// The current implementation of Executor does not isolate work
    /// (as the underlying executor is not yet capable of this).
    pub struct LocalExecutor(Rc<LocalSet>);

    impl LocalExecutor {
        /// Create a new executor.
        pub fn new() -> Self {
            let local_set = Rc::new(LocalSet::new());
            LOCAL_EXECUTOR.with(|e| {
                assert!(
                    e.borrow_mut().replace(local_set.clone()).is_none(),
                    "Cannot create multiple executors"
                );
            });
            Self(local_set)
        }

        /// Run a single future to completion on a single thread.
        pub fn run_singlethreaded<F>(&mut self, main_future: F) -> F::Output
        where
            F: Future,
        {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .expect("Could not start tokio runtime on current thread");
            self.0.block_on(&rt, main_future)
        }
    }

    impl Drop for LocalExecutor {
        fn drop(&mut self) {
            LOCAL_EXECUTOR.with(|e| e.borrow_mut().take());
        }
    }

    /// A single-threaded executor for testing.
    ///
    /// The current implementation of Executor does not isolate work
    /// (as the underlying executor is not yet capable of this).
    pub struct TestExecutor {
        executor: LocalExecutor,
    }

    impl TestExecutor {
        /// Create a new executor for testing.
        pub fn new() -> Self {
            Self { executor: LocalExecutor::new() }
        }
    }

    impl Deref for TestExecutor {
        type Target = LocalExecutor;

        fn deref(&self) -> &Self::Target {
            &self.executor
        }
    }

    impl DerefMut for TestExecutor {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.executor
        }
    }
}

pub mod timer {
    use crate::runtime::WakeupTime;
    use futures::prelude::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// An asynchronous timer.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless polled"]
    pub struct Timer(async_io::Timer);

    impl Timer {
        /// Create a new timer scheduled to fire at `time`.
        pub fn new<WT>(time: WT) -> Self
        where
            WT: WakeupTime,
        {
            Timer(async_io::Timer::at(time.into_time()))
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.poll_unpin(cx).map(drop)
        }
    }
}
