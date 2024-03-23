// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Support for running the ServiceFs until stalled.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use detect_stall::StallableRequestStream;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot::{self, Canceled},
    future::FusedFuture,
    FutureExt, Stream, StreamExt,
};
use pin_project::pin_project;
use vfs::{
    directory::immutable::{connection::ImmutableConnection, Simple},
    execution_scope::{ActiveGuard, ExecutionScope},
    ToObjectRequest,
};
use zx::Duration;

use super::{ServiceFs, ServiceObjTrait};

/// The future type that resolves when an outgoing directory connection has stalled
/// for a timeout or completed.
type StalledFut = Pin<Box<dyn FusedFuture<Output = Option<zx::Channel>>>>;

/// A wrapper around the base [`ServiceFs`] that streams out capability connection requests.
/// Additionally, it will yield [`Item::Stalled`] if there is no work happening in the fs
/// and the main outgoing directory connection has not received messages for some time.
///
/// Use [`ServiceFs::until_stalled`] to produce an instance. Refer to details there.
#[pin_project]
pub struct StallableServiceFs<ServiceObjTy: ServiceObjTrait> {
    #[pin]
    fs: ServiceFs<ServiceObjTy>,
    connector: OutgoingConnector,
    state: State,
    debounce_interval: zx::Duration,
    is_terminated: bool,
}

/// The item yielded by a [`StallableServiceFs`] stream.
pub enum Item<Output> {
    /// A new connection request to a capability. `ServiceObjTy::Output` contains more
    /// information identifying the capability requested. The [`ActiveGuard`] should be
    /// held alive as long as you are processing the connection, or doing any other work
    /// where you would like to prevent the [`ServiceFs`] from shutting down.
    Request(Output, ActiveGuard),

    /// The [`ServiceFs`] has stalled. The unbound outgoing directory server endpoint will
    /// be returned here. The stream will complete right after this. You should typically
    /// escrow the server endpoint back to component manager, and then exit the component.
    Stalled(zx::Channel),
}

// Implementation detail below

/// We use a state machine to detect stalling. The general structure is:
/// - When the service fs is running, wait for the outgoing directory connection to stall.
/// - If the outgoing directory stalled, unbind it and wait for readable.
/// - If it is readable, we'll add back the connection to the service fs and back to wait for stall.
/// - If the service fs finished while the outgoing directory is unbound, we'll
///   complete the stream and return the endpoint to the user. Note that the service fs might take
///   a while to finish even after the outgoing directory has been unbound, due to
///   [`ActiveGuard`]s held by the user or due to other long-running connections.
enum State {
    Running { stalled: StalledFut },
    // If the `channel` is `None`, the outgoing directory stream completed without stalling.
    // We just need to wait for the `ServiceFs` to finish.
    Stalled { channel: Option<fasync::OnSignals<'static, zx::Channel>> },
}

impl<ServiceObjTy: ServiceObjTrait + Send> Stream for StallableServiceFs<ServiceObjTy> {
    type Item = Item<ServiceObjTy::Output>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        if *this.is_terminated {
            return Poll::Ready(None);
        }

        // Poll the underlying service fs to handle requests.
        let poll_fs = this.fs.poll_next_unpin(cx);
        if let Poll::Ready(Some(request)) = poll_fs {
            // If there is some connection request, always return that to the user first.
            return Poll::Ready(Some(Item::Request(request, this.connector.scope.active_guard())));
        }

        // If we get here, the underlying service fs is either finished, or pending.
        // Poll in a loop until the state no longer changes.
        loop {
            match &mut this.state {
                State::Running { stalled } => {
                    let channel = std::task::ready!(stalled.as_mut().poll(cx));
                    let channel = channel
                        .map(|c| fasync::OnSignals::new(c.into(), zx::Signals::CHANNEL_READABLE));
                    // The state will be polled on the next loop iteration.
                    *this.state = State::Stalled { channel };
                }
                State::Stalled { channel } => {
                    if let Poll::Ready(None) = poll_fs {
                        // The service fs finished. Return the channel if we have it.
                        *this.is_terminated = true;
                        return Poll::Ready(
                            channel.take().map(|wait| Item::Stalled(wait.take_handle().into())),
                        );
                    }
                    if channel.is_none() {
                        // The outgoing directory FIDL stream completed (client closed or
                        // errored) without stalling, but the service fs is processing
                        // other requests. Simply wait for that to finish.
                        return Poll::Pending;
                    }
                    // Otherwise, arrange to be polled again if the channel is readable.
                    let readable = channel.as_mut().unwrap().poll_unpin(cx);
                    let _ = std::task::ready!(readable);
                    // Server endpoint is readable again. Restore the connection.
                    let wait = channel.take().unwrap();
                    let stalled =
                        this.connector.serve(wait.take_handle().into(), *this.debounce_interval);
                    // The state will be polled on the next loop iteration.
                    *this.state = State::Running { stalled };
                }
            }
        }
    }
}

struct OutgoingConnector {
    flags: fio::OpenFlags,
    scope: ExecutionScope,
    dir: Arc<Simple>,
}

impl OutgoingConnector {
    /// Adds a stallable outgoing directory connection.
    ///
    /// If the request stream completed, the returned future will resolve with `None`.
    /// If the request stream did not encounter new requests for `debounce_interval`, it will be
    /// unbound, and the returned future will resolve with `Some(channel)`.
    fn serve(
        &mut self,
        server_end: ServerEnd<fio::DirectoryMarker>,
        debounce_interval: Duration,
    ) -> StalledFut {
        let (unbound_sender, unbound_receiver) = oneshot::channel();
        let object_request = self.flags.to_object_request(server_end);
        let scope = self.scope.clone();
        let dir = self.dir.clone();
        let flags = self.flags;
        object_request.spawn(&scope.clone(), move |object_request_ref| {
            async move {
                ImmutableConnection::create_transform_stream(
                    scope,
                    dir,
                    flags,
                    object_request_ref,
                    move |stream| {
                        StallableRequestStream::new(
                            stream,
                            debounce_interval,
                            // This function will be called with the server endpoint when
                            // the directory request stream is stalled for `debounce_interval`
                            move |maybe_channel: Option<zx::Channel>| {
                                _ = unbound_sender.send(maybe_channel);
                            },
                        )
                    },
                )
            }
            .boxed()
        });
        Box::pin(
            unbound_receiver
                .map(|result| match result {
                    Ok(maybe_channel) => maybe_channel,
                    Err(Canceled) => None,
                })
                .fuse(),
        )
    }
}

impl<ServiceObjTy: ServiceObjTrait> StallableServiceFs<ServiceObjTy> {
    pub(crate) fn new(mut fs: ServiceFs<ServiceObjTy>, debounce_interval: zx::Duration) -> Self {
        let channel_queue =
            fs.channel_queue.as_mut().expect("Must not poll the original ServiceFs");
        assert!(
            channel_queue.len() == 1,
            "Must have exactly one connection to serve, \
            e.g. did you call ServiceFs::take_and_serve_directory_handle?"
        );
        let server_end = std::mem::replace(channel_queue, vec![]).into_iter().next().unwrap();
        let flags = ServiceFs::<ServiceObjTy>::base_connection_flags();
        let scope = fs.scope.clone();
        let dir = fs.dir.clone();
        let mut connector = OutgoingConnector { flags, scope, dir };
        let stalled = connector.serve(server_end, debounce_interval);
        Self {
            fs,
            connector,
            state: State::Running { stalled },
            debounce_interval,
            is_terminated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    use assert_matches::assert_matches;
    use fasync::TestExecutor;
    use fidl::endpoints::ClientEnd;
    use fidl_fuchsia_component_client_test::{
        ServiceAMarker, ServiceARequest, ServiceARequestStream,
    };
    use futures::{future::BoxFuture, pin_mut, select, TryStreamExt};
    use test_util::Counter;
    use zx::AsHandleRef;

    use super::*;

    enum Requests {
        ServiceA(ServiceARequestStream),
    }

    #[derive(Clone)]
    struct MockServer {
        call_count: Arc<Counter>,
        stalled: Arc<AtomicBool>,
        server_end: Arc<Mutex<Option<zx::Channel>>>,
    }

    impl MockServer {
        fn new() -> Self {
            let call_count = Arc::new(Counter::new(0));
            let stalled = Arc::new(AtomicBool::new(false));
            let server_end = Arc::new(Mutex::new(None));
            Self { call_count, stalled, server_end }
        }

        fn handle(&self, item: Item<Requests>) -> BoxFuture<'static, ()> {
            let stalled = self.stalled.clone();
            let call_count = self.call_count.clone();
            let server_end = self.server_end.clone();
            async move {
                match item {
                    Item::Request(requests, active_guard) => {
                        let _active_guard = active_guard;
                        let Requests::ServiceA(mut request_stream) = requests;
                        while let Ok(Some(request)) = request_stream.try_next().await {
                            match request {
                                ServiceARequest::Foo { responder } => {
                                    call_count.inc();
                                    let _ = responder.send();
                                }
                            }
                        }
                    }
                    Item::Stalled(channel) => {
                        *server_end.lock().unwrap() = Some(channel);
                        stalled.store(true, Ordering::SeqCst);
                    }
                }
            }
            .boxed()
        }

        #[track_caller]
        fn assert_fs_gave_back_server_end(self, client_end: ClientEnd<fio::DirectoryMarker>) {
            let reclaimed_server_end: zx::Channel = self.server_end.lock().unwrap().take().unwrap();
            assert_eq!(
                client_end.get_koid().unwrap(),
                reclaimed_server_end.basic_info().unwrap().related_koid
            )
        }
    }

    /// Initializes fake time; creates VFS with a single mock server, and returns them.
    async fn setup_test(
        server_end: ServerEnd<fio::DirectoryMarker>,
    ) -> (fasync::Time, MockServer, impl FusedFuture<Output = ()>) {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);

        let mut fs = ServiceFs::new();
        fs.serve_connection(server_end).unwrap().dir("svc").add_fidl_service(Requests::ServiceA);

        let mock_server = MockServer::new();
        let mock_server_clone = mock_server.clone();
        let fs = fs
            .until_stalled(IDLE_DURATION)
            .for_each_concurrent(None, move |item| mock_server_clone.handle(item));

        (initial, mock_server, fs)
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn drain_request() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);
        const NUM_FOO_REQUESTS: usize = 10;
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        let mut proxies = Vec::new();
        for _ in 0..NUM_FOO_REQUESTS {
            proxies.push(
                crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end)
                    .unwrap(),
            );
        }

        // Accept the connections.
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

        // Active FIDL connections block idle, no matter the wait.
        TestExecutor::advance_to(initial + (IDLE_DURATION * 2)).await;
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

        // Make some requests.
        for proxy in proxies.iter() {
            select! {
                result = proxy.foo().fuse() => assert_matches!(result, Ok(_)),
                _ = fs => unreachable!(),
            };
        }

        // Dropping FIDL connections free the ServiceFs to complete.
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        drop(proxies);
        fs.await;

        // Requests were handled.
        assert_eq!(mock_server.call_count.get(), NUM_FOO_REQUESTS);
        assert!(mock_server.stalled.load(Ordering::SeqCst));
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn no_request() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        TestExecutor::advance_to(initial + IDLE_DURATION).await;
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_ready());

        assert_eq!(mock_server.call_count.get(), 0);
        assert!(mock_server.stalled.load(Ordering::SeqCst));
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn outgoing_dir_client_closed() {
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (_initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        drop(client_end);
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_ready());

        assert_eq!(mock_server.call_count.get(), 0);
        assert!(!mock_server.stalled.load(Ordering::SeqCst));
        assert!(mock_server.server_end.lock().unwrap().is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn request_then_stalled() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);

        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let proxy =
            crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end).unwrap();

        let foo = proxy.foo().fuse();
        pin_mut!(foo);
        assert!(TestExecutor::poll_until_stalled(&mut foo).await.is_pending());

        let (initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        // Poll the fs to process the FIDL.
        assert_eq!(mock_server.call_count.get(), 0);
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        assert_eq!(mock_server.call_count.get(), 1);
        assert_matches!(foo.await, Ok(_));

        drop(proxy);
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        TestExecutor::advance_to(initial + IDLE_DURATION).await;
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_ready());

        assert_eq!(mock_server.call_count.get(), 1);
        assert!(mock_server.stalled.load(Ordering::SeqCst));
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn stalled_then_request() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        TestExecutor::advance_to(initial + (IDLE_DURATION / 2)).await;
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

        let proxy =
            crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end).unwrap();
        select! {
            result = proxy.foo().fuse() => assert_matches!(result, Ok(_)),
            _ = fs => unreachable!(),
        };
        assert_eq!(mock_server.call_count.get(), 1);

        drop(proxy);
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        TestExecutor::advance_to(initial + (IDLE_DURATION / 2) + IDLE_DURATION).await;
        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_ready());

        assert!(mock_server.stalled.load(Ordering::SeqCst));
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    /// If periodic FIDL connections are made at an interval below the idle
    /// duration, the service fs should not stall.
    ///
    /// If periodic FIDL connections are made at an interval above the idle
    /// duration, the service fs should stall.
    #[fuchsia::test(allow_stalls = false)]
    async fn periodic_requests() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (mut current_time, mock_server, fs) = setup_test(server_end).await;
        let fs = fasync::Task::local(fs);

        // Interval below the idle duration.
        const NUM_FOO_REQUESTS: usize = 10;
        for _ in 0..NUM_FOO_REQUESTS {
            let request_interval = IDLE_DURATION / 2;
            current_time += request_interval;
            TestExecutor::advance_to(current_time).await;
            let proxy =
                crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end)
                    .unwrap();
            assert_matches!(proxy.foo().await, Ok(_));
        }
        assert_eq!(mock_server.call_count.get(), NUM_FOO_REQUESTS);

        // Interval above the idle duration.
        for _ in 0..NUM_FOO_REQUESTS {
            let request_interval = IDLE_DURATION * 2;
            current_time += request_interval;
            TestExecutor::advance_to(current_time).await;
            let proxy =
                crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end)
                    .unwrap();
            let foo = proxy.foo();
            pin_mut!(foo);
            assert_matches!(TestExecutor::poll_until_stalled(&mut foo).await, Poll::Pending);
        }
        assert_eq!(mock_server.call_count.get(), NUM_FOO_REQUESTS);

        fs.await;
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    /// If there are other connections to the outgoing directory, then the fs will not return unless
    /// those connections are closed by the client. That's because we currently don't have a way to
    /// escrow those connections, so we don't want to disrupt them.
    #[fuchsia::test(allow_stalls = false)]
    async fn some_other_outgoing_dir_connection_blocks_stalling() {
        const IDLE_DURATION: Duration = Duration::from_nanos(1_000_000);
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let (initial, mock_server, fs) = setup_test(server_end).await;
        pin_mut!(fs);

        assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

        {
            // We can open another connection that's not the main outgoing directory connection,
            let svc = crate::directory::open_directory_no_describe(
                &client_end,
                "svc",
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            )
            .unwrap();

            TestExecutor::advance_to(initial + IDLE_DURATION).await;
            assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

            assert_matches!(
                fuchsia_fs::directory::readdir(&svc).await,
                Ok(ref entries)
                if entries.len() == 1 && entries[0].name == "fuchsia.component.client.test.ServiceA"
            );
            assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());

            // ... and the service fs won't stall even if we wait past the timeout.
            TestExecutor::advance_to(initial + (IDLE_DURATION * 3)).await;
            assert!(TestExecutor::poll_until_stalled(&mut fs).await.is_pending());
        }

        // Closing that connection frees the fs to stall.
        fs.await;
        assert!(mock_server.stalled.load(Ordering::SeqCst));
        mock_server.assert_fs_gave_back_server_end(client_end);
    }

    /// Emulates a component that receives a bunch of requests, processes them, and then stalls.
    /// After that, if the outgoing directory is readable, serve it again. No request should be
    /// dropped, and the fs should stall a bunch of times.
    #[fuchsia::test(allow_stalls = false)]
    async fn end_to_end() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;

        let mock_server = MockServer::new();
        let mock_server_clone = mock_server.clone();

        const MIN_REQUEST_INTERVAL: i64 = 10_000_000;
        let idle_duration = Duration::from_nanos(MIN_REQUEST_INTERVAL * 5);
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();

        let component_task = async move {
            let mut server_end = Some(server_end);
            let mut loop_count = 0;
            loop {
                let mut fs = ServiceFs::new();
                fs.serve_connection(server_end.unwrap())
                    .unwrap()
                    .dir("svc")
                    .add_fidl_service(Requests::ServiceA);

                let mock_server_clone = mock_server_clone.clone();
                fs.until_stalled(idle_duration)
                    .for_each_concurrent(None, move |item| mock_server_clone.handle(item))
                    .await;

                let stalled_server_end = mock_server.server_end.lock().unwrap().take();
                let Some(stalled_server_end) = stalled_server_end else {
                    // Client closed.
                    return loop_count;
                };

                fasync::OnSignals::new(
                    &stalled_server_end,
                    zx::Signals::CHANNEL_READABLE | zx::Signals::CHANNEL_PEER_CLOSED,
                )
                .await
                .unwrap();
                server_end = Some(stalled_server_end.into());
                loop_count += 1;
            }
        };
        let component_task = fasync::Task::local(component_task);

        // Make connection requests at increasing intervals, starting from below the idle duration,
        // to above the idle duration.
        let mut deadline = initial;
        const NUM_REQUESTS: usize = 30;
        for delay_factor in 0..NUM_REQUESTS {
            let proxy =
                crate::client::connect_to_protocol_at_dir_svc::<ServiceAMarker>(&client_end)
                    .unwrap();
            proxy.foo().await.unwrap();
            drop(proxy);
            deadline += Duration::from_nanos(MIN_REQUEST_INTERVAL * (delay_factor as i64));
            TestExecutor::advance_to(deadline).await;
        }

        drop(client_end);
        let loop_count = component_task.await;
        // Why 25: there are 30 requests. The first 5 intervals are below the idle duration.
        assert_eq!(loop_count, 25);
        assert_eq!(mock_server.call_count.get(), NUM_REQUESTS);
        assert!(mock_server.stalled.load(Ordering::SeqCst));
    }
}
