// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Test Utilities for the fuchsia.net.routes FIDL library.
//!
//! This library defines a mix of internal and external test utilities,
//! supporting tests of this `fidl_fuchsia_net_routes_ext` crate and tests
//! of clients of the `fuchsia.net.routes` FIDL library, respectively.

use crate::FidlRouteIpExt;

use fidl_fuchsia_net_routes as fnet_routes;
use futures::{Future, Stream, StreamExt as _};
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6};

// Responds to the given `Watch` request with the given batch of events.
fn handle_watch<I: FidlRouteIpExt>(
    request: <<I::WatcherMarker as fidl::endpoints::ProtocolMarker>::RequestStream as Stream>::Item,
    event_batch: Vec<I::WatchEvent>,
) {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct HandleInputs<I: FidlRouteIpExt> {
        request:
            <<I::WatcherMarker as fidl::endpoints::ProtocolMarker>::RequestStream as Stream>::Item,
        event_batch: Vec<I::WatchEvent>,
    }
    I::map_ip::<HandleInputs<I>, ()>(
        HandleInputs { request, event_batch },
        |HandleInputs { request, event_batch }| match request
            .expect("failed to receive `Watch` request")
        {
            fnet_routes::WatcherV4Request::Watch { responder } => {
                responder.send(&event_batch).expect("failed to respond to `Watch`")
            }
        },
        |HandleInputs { request, event_batch }| match request
            .expect("failed to receive `Watch` request")
        {
            fnet_routes::WatcherV6Request::Watch { responder } => {
                responder.send(&event_batch).expect("failed to respond to `Watch`")
            }
        },
    );
}

/// A fake implementation of the `WatcherV4` and `WatcherV6` protocols.
///
/// Feeds events received in `events` as responses to `Watch()`.
pub async fn fake_watcher_impl<I: FidlRouteIpExt>(
    events: impl Stream<Item = Vec<I::WatchEvent>>,
    server_end: fidl::endpoints::ServerEnd<I::WatcherMarker>,
) {
    let (request_stream, _control_handle) = server_end
        .into_stream_and_control_handle()
        .expect("failed to get `Watcher` request stream");
    request_stream
        .zip(events)
        .for_each(|(request, event_batch)| {
            handle_watch::<I>(request, event_batch);
            futures::future::ready(())
        })
        .await
}

/// Serve a `GetWatcher` request to the `State` protocol by instantiating a
/// watcher client backed by the given event stream. The returned future
/// drives the watcher implementation.
pub async fn serve_state_request<'a, I: FidlRouteIpExt>(
    request: <<I::StateMarker as fidl::endpoints::ProtocolMarker>::RequestStream as Stream>::Item,
    event_stream: impl Stream<Item = Vec<I::WatchEvent>> + 'a,
) {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct GetWatcherInputs<'a, I: FidlRouteIpExt> {
        request:
            <<I::StateMarker as fidl::endpoints::ProtocolMarker>::RequestStream as Stream>::Item,
        // Use `Box<dyn>` here because `event_stream` needs to have a know size.
        event_stream: Box<dyn Stream<Item = Vec<I::WatchEvent>> + 'a>,
    }
    let IpInvariant(watcher_fut) = I::map_ip::<
        GetWatcherInputs<'a, I>,
        // Use `Box<dyn>` here because `event_stream` needs to have a know size.
        // `Pin` ensures that `watcher_fut` implements `Future`.
        IpInvariant<std::pin::Pin<Box<dyn Future<Output = ()> + 'a>>>,
    >(
        GetWatcherInputs { request, event_stream: Box::new(event_stream) },
        |GetWatcherInputs { request, event_stream }| match request
            .expect("failed to receive `GetWatcherV4` request")
        {
            fnet_routes::StateV4Request::GetWatcherV4 {
                options: _,
                watcher,
                control_handle: _,
            } => IpInvariant(Box::pin(fake_watcher_impl::<Ipv4>(
                Box::into_pin(event_stream),
                watcher,
            ))),
        },
        |GetWatcherInputs { request, event_stream }| match request
            .expect("failed to receive `GetWatcherV6` request")
        {
            fnet_routes::StateV6Request::GetWatcherV6 {
                options: _,
                watcher,
                control_handle: _,
            } => IpInvariant(Box::pin(fake_watcher_impl::<Ipv6>(
                Box::into_pin(event_stream),
                watcher,
            ))),
        },
    );
    watcher_fut.await
}

/// Provides a stream of watcher events such that the stack appears to contain
/// no routes and never installs any.
pub fn empty_watch_event_stream<'a, I: FidlRouteIpExt>(
) -> impl Stream<Item = Vec<I::WatchEvent>> + 'a {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct Wrap<I: FidlRouteIpExt>(I::WatchEvent);

    let Wrap(event) = I::map_ip(
        (),
        |()| Wrap(fnet_routes::EventV4::Idle(fnet_routes::Empty)),
        |()| Wrap(fnet_routes::EventV6::Idle(fnet_routes::Empty)),
    );
    futures::stream::once(futures::future::ready(vec![event])).chain(futures::stream::pending())
}

/// Provides testutils for testing implementations of clients and servers of
/// fuchsia.net.routes.admin.
pub mod admin {
    use fidl::endpoints::ProtocolMarker;
    use fidl_fuchsia_net_routes_admin as fnet_routes_admin;
    use futures::{Stream, StreamExt as _};
    use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};

    use crate::admin::{FidlRouteAdminIpExt, Responder, RouteSetRequest};

    /// Provides a SetProvider implementation that provides one RouteSet and
    /// then panics on subsequent invocations. Returns the request stream for
    /// that RouteSet.
    pub fn serve_one_route_set<I: FidlRouteAdminIpExt>(
        server_end: fidl::endpoints::ServerEnd<I::SetProviderMarker>,
    ) -> impl Stream<
            Item = <
                    <<I as FidlRouteAdminIpExt>::RouteSetMarker as ProtocolMarker>
                        ::RequestStream as Stream
                >::Item
    >{
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct In<I: FidlRouteAdminIpExt>(
            <<<I as FidlRouteAdminIpExt>::SetProviderMarker as ProtocolMarker>
                ::RequestStream as Stream
            >::Item,
        );
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Out<I: FidlRouteAdminIpExt>(fidl::endpoints::ServerEnd<I::RouteSetMarker>);

        let stream = server_end.into_stream().expect("into stream");
        stream
            .scan(false, |responded, item| {
                let responded = std::mem::replace(responded, true);
                if responded {
                    panic!("received multiple SetProvider requests");
                }

                futures::future::ready(Some(item))
            })
            .map(|item| {
                let Out(route_set_server_end) = I::map_ip(
                    In(item),
                    |In(item)| match item.expect("set provider FIDL error") {
                        fnet_routes_admin::SetProviderV4Request::NewRouteSet {
                            route_set,
                            control_handle: _,
                        } => Out(route_set),
                    },
                    |In(item)| match item.expect("set provider FIDL error") {
                        fnet_routes_admin::SetProviderV6Request::NewRouteSet {
                            route_set,
                            control_handle: _,
                        } => Out(route_set),
                    },
                );
                route_set_server_end.into_stream().expect("into stream")
            })
            .flatten()
    }

    /// Provides a SetProvider implementation that serves no-op RouteSets.
    pub async fn serve_noop_route_sets<I: FidlRouteAdminIpExt>(
        server_end: fidl::endpoints::ServerEnd<I::SetProviderMarker>,
    ) {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct In<I: FidlRouteAdminIpExt>(
            <<<I as FidlRouteAdminIpExt>::SetProviderMarker as ProtocolMarker>
                ::RequestStream as Stream
            >::Item,
        );
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Out<I: FidlRouteAdminIpExt>(fidl::endpoints::ServerEnd<I::RouteSetMarker>);

        let stream = server_end.into_stream().expect("into stream");
        stream
            .for_each_concurrent(None, |item| async move {
                let Out(route_set_server_end) = I::map_ip(
                    In(item),
                    |In(item)| match item.expect("set provider FIDL error") {
                        fnet_routes_admin::SetProviderV4Request::NewRouteSet {
                            route_set,
                            control_handle: _,
                        } => Out(route_set),
                    },
                    |In(item)| match item.expect("set provider FIDL error") {
                        fnet_routes_admin::SetProviderV6Request::NewRouteSet {
                            route_set,
                            control_handle: _,
                        } => Out(route_set),
                    },
                );
                serve_noop_route_set::<I>(route_set_server_end).await;
            })
            .await;
    }

    /// Serves a RouteSet that returns OK for everything and does nothing.
    async fn serve_noop_route_set<I: FidlRouteAdminIpExt>(
        server_end: fidl::endpoints::ServerEnd<I::RouteSetMarker>,
    ) {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<I: FidlRouteAdminIpExt>(
            <<<I as FidlRouteAdminIpExt>::RouteSetMarker as ProtocolMarker>
                ::RequestStream as Stream
            >::Item,
        );

        let stream = server_end.into_stream().expect("into stream");
        stream
            .for_each(|item| async move {
                let request: RouteSetRequest<I> = I::map_ip(
                    Wrap(item),
                    |Wrap(item)| RouteSetRequest::<Ipv4>::from(item.expect("route set FIDL error")),
                    |Wrap(item)| RouteSetRequest::<Ipv6>::from(item.expect("route set FIDL error")),
                );
                match request {
                    RouteSetRequest::AddRoute { route, responder } => {
                        let _: crate::Route<I> = route.expect("AddRoute called with invalid route");
                        responder.send(Ok(true)).expect("respond to AddRoute");
                    }
                    RouteSetRequest::RemoveRoute { route, responder } => {
                        let _: crate::Route<I> =
                            route.expect("RemoveRoute called with invalid route");
                        responder.send(Ok(true)).expect("respond to RemoveRoute");
                    }
                    RouteSetRequest::AuthenticateForInterface { credential: _, responder } => {
                        responder.send(Ok(())).expect("respond to AuthenticateForInterface");
                    }
                }
            })
            .await;
    }
}

#[cfg(test)]
pub(crate) mod internal {
    use super::*;
    use net_declare::{fidl_ip_v4_with_prefix, fidl_ip_v6_with_prefix};

    // Generates an arbitrary `I::WatchEvent` that is unique for the given `seed`.
    pub(crate) fn generate_event<I: FidlRouteIpExt>(seed: u32) -> I::WatchEvent {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct BuildEventOutput<I: FidlRouteIpExt>(I::WatchEvent);
        let BuildEventOutput(event) = I::map_ip(
            IpInvariant(seed),
            |IpInvariant(seed)| {
                BuildEventOutput(fnet_routes::EventV4::Added(fnet_routes::InstalledRouteV4 {
                    route: Some(fnet_routes::RouteV4 {
                        destination: fidl_ip_v4_with_prefix!("192.168.0.0/24"),
                        action: fnet_routes::RouteActionV4::Forward(fnet_routes::RouteTargetV4 {
                            outbound_interface: 1,
                            next_hop: None,
                        }),
                        properties: fnet_routes::RoutePropertiesV4 {
                            specified_properties: Some(fnet_routes::SpecifiedRouteProperties {
                                metric: Some(fnet_routes::SpecifiedMetric::ExplicitMetric(seed)),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    }),
                    effective_properties: Some(fnet_routes::EffectiveRouteProperties {
                        metric: Some(seed),
                        ..Default::default()
                    }),
                    ..Default::default()
                }))
            },
            |IpInvariant(seed)| {
                BuildEventOutput(fnet_routes::EventV6::Added(fnet_routes::InstalledRouteV6 {
                    route: Some(fnet_routes::RouteV6 {
                        destination: fidl_ip_v6_with_prefix!("fe80::0/64"),
                        action: fnet_routes::RouteActionV6::Forward(fnet_routes::RouteTargetV6 {
                            outbound_interface: 1,
                            next_hop: None,
                        }),
                        properties: fnet_routes::RoutePropertiesV6 {
                            specified_properties: Some(fnet_routes::SpecifiedRouteProperties {
                                metric: Some(fnet_routes::SpecifiedMetric::ExplicitMetric(seed)),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    }),
                    effective_properties: Some(fnet_routes::EffectiveRouteProperties {
                        metric: Some(seed),
                        ..Default::default()
                    }),
                    ..Default::default()
                }))
            },
        );
        event
    }

    // Same as `generate_event()` except that it operates over a range of `seeds`,
    // producing `n` `I::WatchEvents` where `n` is the size of the range.
    pub(crate) fn generate_events_in_range<I: FidlRouteIpExt>(
        seeds: std::ops::Range<u32>,
    ) -> Vec<I::WatchEvent> {
        seeds.into_iter().map(|seed| generate_event::<I>(seed)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{get_watcher, testutil::internal as internal_testutil, watch};
    use assert_matches::assert_matches;
    use fuchsia_zircon_status as zx_status;
    use futures::FutureExt;
    use netstack_testing_macros::netstack_test;
    use test_case::test_case;

    // Tests the `fake_watcher_impl` with various "shapes". The test parameter
    // is a vec of ranges, where each range corresponds to the batch of events
    // that will be sent in response to a single call to `Watch().
    #[netstack_test]
    #[test_case(Vec::new(); "no events")]
    #[test_case(vec![0..1]; "single_batch_single_event")]
    #[test_case(vec![0..10]; "single_batch_many_events")]
    #[test_case(vec![0..10, 10..20, 20..30]; "many_batches_many_events")]
    async fn fake_watcher_impl_against_shape<I: net_types::ip::Ip + FidlRouteIpExt>(
        // TODO(https://fxbug.dev/42070381): remove `_test_name` once optional.
        _test_name: &str,
        test_shape: Vec<std::ops::Range<u32>>,
    ) {
        // Build the event stream based on the `test_shape`. Use a channel
        // so that the stream stays open until `close_channel` is called later.
        let (event_stream_sender, event_stream_receiver) =
            futures::channel::mpsc::unbounded::<Vec<I::WatchEvent>>();
        for batch_shape in &test_shape {
            event_stream_sender
                .unbounded_send(internal_testutil::generate_events_in_range::<I>(
                    batch_shape.clone(),
                ))
                .expect("failed to send event batch");
        }

        // Instantiate the fake Watcher implementation.
        let (state, state_server_end) =
            fidl::endpoints::create_proxy::<I::StateMarker>().expect("failed to create proxy");
        let (mut state_request_stream, _control_handle) = state_server_end
            .into_stream_and_control_handle()
            .expect("failed to get `State` request stream");
        let watcher_fut = state_request_stream
            .next()
            .then(|req| {
                serve_state_request::<I>(
                    req.expect("State request_stream unexpectedly ended"),
                    event_stream_receiver,
                )
            })
            .fuse();
        futures::pin_mut!(watcher_fut);

        // Drive the watcher, asserting it observes the expected data.
        let watcher = get_watcher::<I>(&state).expect("failed to get watcher");
        for batch_shape in test_shape {
            futures::select!(
                 () = watcher_fut => panic!("fake watcher implementation unexpectedly finished"),
                events = watch::<I>(&watcher).fuse() => assert_eq!(
                    events.expect("failed to watch for events"),
                    internal_testutil::generate_events_in_range::<I>(batch_shape.clone())));
        }

        // Close the event_stream_sender and observe the watcher_impl finish.
        event_stream_sender.close_channel();
        watcher_fut.await;

        // Trying to watch again after we've exhausted the data should
        // result in `PEER_CLOSED`.
        assert_matches!(
            watch::<I>(&watcher).await,
            Err(fidl::Error::ClientChannelClosed { status: zx_status::Status::PEER_CLOSED, .. })
        );
    }
}
