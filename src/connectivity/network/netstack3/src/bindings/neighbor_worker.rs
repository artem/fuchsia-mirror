// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{HashMap, VecDeque};

use fidl::endpoints::{ControlHandle as _, RequestStream as _, Responder as _};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext::IntoExt;
use fidl_fuchsia_net_neighbor::{
    self as fnet_neighbor, ControllerRequest, ControllerRequestStream, ViewRequest,
    ViewRequestStream,
};
use fidl_fuchsia_net_neighbor_ext as fnet_neighbor_ext;
use fuchsia_zircon as zx;

use assert_matches::assert_matches;
use futures::{
    channel::mpsc, task::Poll, Future, SinkExt as _, StreamExt as _, TryFutureExt as _,
    TryStreamExt as _,
};
use net_types::{
    ethernet::Mac,
    ip::{IpAddr, Ipv4, Ipv6},
    SpecifiedAddr, Witness as _,
};
use tracing::{error, info, warn};

use crate::bindings::{
    devices::{BindingId, DeviceIdAndName},
    BindingsNonSyncCtxImpl, Ctx, StackTime,
};
use netstack3_core::{
    device::EthernetWeakDeviceId,
    error::{NeighborRemovalError, NotFoundError, NotSupportedError, StaticNeighborInsertionError},
    ip::device::nud,
};

#[derive(Debug)]
pub(crate) struct Event {
    pub(crate) id: EthernetWeakDeviceId<BindingsNonSyncCtxImpl>,
    pub(crate) addr: SpecifiedAddr<IpAddr>,
    pub(crate) kind: nud::EventKind<Mac>,
    pub(crate) at: StackTime,
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { id, addr, kind, at } = self;
        write!(f, "neighbor event {:?} {} {:?} at {}", id.bindings_id(), addr, kind, at)
    }
}

fn new_fidl_entry(
    binding_id: BindingId,
    addr: SpecifiedAddr<IpAddr>,
    state: nud::EventState<Mac>,
    StackTime(at): StackTime,
) -> fnet_neighbor::Entry {
    let (state, mac) = match state {
        nud::EventState::Dynamic(dynamic_state) => match dynamic_state {
            nud::EventDynamicState::Incomplete => (fnet_neighbor::EntryState::Incomplete, None),
            nud::EventDynamicState::Reachable(mac) => {
                (fnet_neighbor::EntryState::Reachable, Some(mac))
            }
            nud::EventDynamicState::Stale(mac) => (fnet_neighbor::EntryState::Stale, Some(mac)),
            nud::EventDynamicState::Delay(mac) => (fnet_neighbor::EntryState::Delay, Some(mac)),
            nud::EventDynamicState::Probe(mac) => (fnet_neighbor::EntryState::Probe, Some(mac)),
            nud::EventDynamicState::Unreachable(mac) => {
                (fnet_neighbor::EntryState::Unreachable, Some(mac))
            }
        },
        nud::EventState::Static(mac) => (fnet_neighbor::EntryState::Static, Some(mac)),
    };
    fnet_neighbor_ext::Entry {
        interface: binding_id.into(),
        neighbor: addr.get().into_ext(),
        state,
        mac: mac.map(IntoExt::into_ext),
        updated_at: at.into_nanos(),
    }
    .into()
}

pub(crate) struct Worker {
    event_receiver: mpsc::UnboundedReceiver<Event>,
    watcher_receiver: mpsc::Receiver<NewWatcher>,
}

/// Arbitrarily picked constant to limit memory consumed by queued watcher requests.
const WATCHER_CHANNEL_CAPACITY: usize = 128;

pub(crate) fn new_worker() -> (Worker, mpsc::Sender<NewWatcher>, mpsc::UnboundedSender<Event>) {
    let (event_sink, event_receiver) = futures::channel::mpsc::unbounded();
    let (watcher_sink, watcher_receiver) =
        futures::channel::mpsc::channel(WATCHER_CHANNEL_CAPACITY);
    (Worker { event_receiver, watcher_receiver }, watcher_sink, event_sink)
}

fn handle_new_watcher(
    neighbor_state: &HashMap<
        BindingId,
        HashMap<SpecifiedAddr<IpAddr>, (nud::EventState<Mac>, StackTime)>,
    >,
    watchers: &mut futures::stream::FuturesUnordered<Watcher>,
    NewWatcher { options, stream }: NewWatcher,
) {
    let options = match options.try_into() {
        Ok(options) => options,
        Err(e) => {
            warn!("failed to initialize neighbor watcher: {:?}", e);
            stream.control_handle().shutdown_with_epitaph(zx::Status::INVALID_ARGS);
            return;
        }
    };
    let event_queue = EventQueue(
        neighbor_state
            .iter()
            .map(|(binding_id, entries)| {
                entries.iter().map(|(addr, (state, last_updated))| {
                    fnet_neighbor::EntryIteratorItem::Existing(new_fidl_entry(
                        *binding_id,
                        *addr,
                        *state,
                        *last_updated,
                    ))
                })
            })
            .flatten()
            .chain(std::iter::once(fnet_neighbor::EntryIteratorItem::Idle(
                fnet_neighbor::IdleEvent,
            )))
            .collect(),
    );
    watchers.push(Watcher { stream, options, event_queue, responder: None });
}

impl Worker {
    pub(crate) async fn run(self) {
        let Self { event_receiver: event_stream, watcher_receiver: new_watchers } = self;
        let mut watchers = futures::stream::FuturesUnordered::<Watcher>::new();
        let mut neighbor_state: HashMap<
            _,
            HashMap<SpecifiedAddr<IpAddr>, (nud::EventState<Mac>, StackTime)>,
        > = HashMap::new();

        enum SinkItem {
            NewWatcher(NewWatcher),
            Event(Event),
        }
        // Always consume events before watchers. That allows external
        // observers to assume all side effects of a call are already
        // applied before a watcher observes its initial existing set of
        // properties.
        let mut stream = futures::stream::select_with_strategy(
            event_stream.map(SinkItem::Event),
            new_watchers.map(SinkItem::NewWatcher),
            |_: &mut ()| futures::stream::PollNext::Left,
        );

        enum Item {
            WatcherEnded(Result<(), fidl::Error>),
            SinkItem(Option<SinkItem>),
        }
        loop {
            let item = futures::select! {
                i = stream.next() => Item::SinkItem(i),
                w = watchers.select_next_some() => Item::WatcherEnded(w),
                complete => break,
            };
            match item {
                Item::SinkItem(None) => {
                    info!("neighbor worker shutting down, waiting for watchers to end")
                }
                Item::WatcherEnded(r) => r.unwrap_or_else(|e| {
                    if !e.is_closed() {
                        error!("error operating neighbor watcher {:?}", e);
                    }
                }),
                Item::SinkItem(Some(SinkItem::NewWatcher(new_watcher))) => {
                    handle_new_watcher(&neighbor_state, &mut watchers, new_watcher);
                }
                Item::SinkItem(Some(SinkItem::Event(
                    ref event @ Event { ref id, kind, addr, at },
                ))) => {
                    // TODO(https://fxbug.dev/136401): Add flags to the log indicating if
                    // the neighbor is a default gateway and if it is on-link.
                    info!(tag = "NUD", "{event}");

                    let DeviceIdAndName { id: binding_id, name: _ } = *id.bindings_id();
                    let entry = neighbor_state
                        .entry(binding_id)
                        .or_insert_with(|| HashMap::new())
                        .entry(addr);
                    let fidl_event = match kind {
                        nud::EventKind::Added(state) => match entry {
                            std::collections::hash_map::Entry::Occupied(occupied) => {
                                panic!(
                                    "neighbor added but already exists: entry={:?}, event={:?}",
                                    occupied.get(),
                                    event
                                );
                            }
                            std::collections::hash_map::Entry::Vacant(vacant) => {
                                let _ = vacant.insert((state, at));
                                fnet_neighbor::EntryIteratorItem::Added(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                        nud::EventKind::Removed => match entry {
                            std::collections::hash_map::Entry::Vacant(_) => {
                                panic!("neighbor removed but not found: {event:?}");
                            }
                            std::collections::hash_map::Entry::Occupied(occupied) => {
                                let (state, at) = occupied.remove();

                                let entry = assert_matches!(
                                    neighbor_state.entry(binding_id),
                                    std::collections::hash_map::Entry::Occupied(o) => o
                                );
                                if entry.get().is_empty() {
                                    let _ = entry.remove();
                                }

                                fnet_neighbor::EntryIteratorItem::Removed(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                        nud::EventKind::Changed(state) => match entry {
                            std::collections::hash_map::Entry::Vacant(_) => {
                                panic!("neighbor changed but not found: {event:?}");
                            }
                            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                                let (current_state, _) = *occupied.get();
                                // NS3 core guarantees to only emit changed events if state
                                // actually changed.
                                assert_ne!(
                                    current_state, state,
                                    "neighbor changed but nothing changed: {event:?}",
                                );
                                let _ = occupied.insert((state, at));
                                fnet_neighbor::EntryIteratorItem::Changed(new_fidl_entry(
                                    binding_id, addr, state, at,
                                ))
                            }
                        },
                    };
                    watchers.iter_mut().for_each(|watcher| {
                        watcher.push(fidl_event.clone());
                    });
                }
            }
        }
        info!("all neighbor watchers closed, neighbor worker shutdown is complete");
    }
}

#[derive(Debug)]
/// A bounded queue of [`Events`] to be returned to `fuchsia.net.neighbor/EntryIterator`.
struct EventQueue(VecDeque<fnet_neighbor::EntryIteratorItem>);

const MAX_ITEM_BATCH_SIZE: usize = fnet_neighbor::MAX_ITEM_BATCH_SIZE as usize;
// Arbitrarily-chosen maximum number of events to queue per client (4 times the
// maximum number of entries held in core per IP per interface).
const MAX_EVENTS: usize = 4 * nud::MAX_ENTRIES;

impl EventQueue {
    fn is_empty(&self) -> bool {
        let Self(event_queue) = self;
        event_queue.is_empty()
    }

    fn push(
        &mut self,
        event: fnet_neighbor::EntryIteratorItem,
    ) -> Result<(), fnet_neighbor::EntryIteratorItem> {
        let Self(event_queue) = self;
        if event_queue.len() >= MAX_EVENTS {
            return Err(event);
        }
        event_queue.push_back(event);
        Ok(())
    }

    fn pop_max(&mut self) -> impl IntoIterator<Item = fnet_neighbor::EntryIteratorItem> + '_ {
        let Self(event_queue) = self;
        let count = std::cmp::min(MAX_ITEM_BATCH_SIZE, event_queue.len());
        event_queue.drain(0..count)
    }
}

/// The task that serves `fuchsia.net.neighbor/EntryIterator`.
///
/// The future implementation drives `stream` and responds to the requests
/// with events from `event_queue`, and completes when `stream` is exhausted
/// (possibly with an error).
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub(crate) struct Watcher {
    stream: fnet_neighbor::EntryIteratorRequestStream,
    options: fnet_neighbor_ext::EntryIteratorOptions,
    event_queue: EventQueue,
    responder: Option<fnet_neighbor::EntryIteratorGetNextResponder>,
}

fn send_events(
    responder: fnet_neighbor::EntryIteratorGetNextResponder,
    events: &[fnet_neighbor::EntryIteratorItem],
) {
    responder.send(events).unwrap_or_else(|e| {
        if e.is_closed() {
            warn!("neighbor watcher closed when sending event");
        } else {
            error!("error sending event to neighbor watcher: {e:?}");
        }
    })
}

impl Future for Watcher {
    type Output = Result<(), fidl::Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let next_request = self.as_mut().stream.poll_next_unpin(cx)?;
            match futures::ready!(next_request) {
                Some(fnet_neighbor::EntryIteratorRequest::GetNext { responder }) => {
                    if !self.event_queue.is_empty() {
                        let events = self.event_queue.pop_max().into_iter().collect::<Vec<_>>();
                        send_events(responder, &events);
                    } else {
                        match &self.responder {
                            Some(existing) => {
                                existing
                                    .control_handle()
                                    .shutdown_with_epitaph(zx::Status::ALREADY_EXISTS);
                                return Poll::Ready(Ok(()));
                            }
                            None => {
                                self.responder = Some(responder);
                            }
                        }
                    }
                }
                None => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl Watcher {
    fn push(&mut self, event: fnet_neighbor::EntryIteratorItem) {
        let Self {
            stream,
            event_queue,
            responder,
            options: fnet_neighbor_ext::EntryIteratorOptions {},
        } = self;

        match responder.take() {
            Some(responder) => {
                debug_assert!(event_queue.is_empty());
                send_events(responder, std::slice::from_ref(&event));
            }
            None => {
                event_queue.push(event).unwrap_or_else(|_: fnet_neighbor::EntryIteratorItem| {
                    warn!("too many pending events enqueued for neighbor watcher, closing channel");
                    stream.control_handle().shutdown();
                });
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("neighbor worker no longer available")]
pub(crate) struct WorkerClosedError;

/// Possible errors when serving `fuchsia.net.neighbor/View`.
#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("failed to send a Watcher task to parent")]
    Send(#[from] WorkerClosedError),
    #[error(transparent)]
    Fidl(#[from] fidl::Error),
}

pub(crate) struct NewWatcher {
    stream: fnet_neighbor::EntryIteratorRequestStream,
    options: fnet_neighbor::EntryIteratorOptions,
}

pub(super) async fn serve_view(
    stream: ViewRequestStream,
    sink: mpsc::Sender<NewWatcher>,
) -> Result<(), Error> {
    stream
        .err_into()
        .try_fold(sink, |mut sink, request| async move {
            match request {
                ViewRequest::OpenEntryIterator { it, options, control_handle: _ } => sink
                    .send(NewWatcher { stream: it.into_stream()?, options })
                    .await
                    .map_err(|_: mpsc::SendError| Error::Send(WorkerClosedError))?,
                ViewRequest::GetUnreachabilityConfig { interface, ip_version, responder } => {
                    warn!("not responding to GetUnreachabilityConfig");
                    let _ = (interface, ip_version, responder);
                }
            }
            Ok(sink)
        })
        .map_ok(|_: mpsc::Sender<NewWatcher>| ())
        .await
}

pub(super) async fn serve_controller(
    ctx: Ctx,
    stream: ControllerRequestStream,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each(|request| async {
            let mut ctx = ctx.clone();
            let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();

            match request {
                ControllerRequest::AddEntry { interface, neighbor, mac, responder } => {
                    let Some(device_id) = BindingId::new(interface)
                        .and_then(|id| non_sync_ctx.devices.get_core_id(id))
                    else {
                        return responder.send(Err(zx::Status::NOT_FOUND.into_raw()));
                    };
                    let mac = mac.into_ext();
                    let result = match neighbor.into_ext() {
                        IpAddr::V4(v4) => netstack3_core::device::insert_static_neighbor_entry::<
                            Ipv4,
                            BindingsNonSyncCtxImpl,
                        >(
                            sync_ctx, non_sync_ctx, &device_id, v4, mac
                        ),
                        IpAddr::V6(v6) => netstack3_core::device::insert_static_neighbor_entry::<
                            Ipv6,
                            BindingsNonSyncCtxImpl,
                        >(
                            sync_ctx, non_sync_ctx, &device_id, v6, mac
                        ),
                    }
                    .map_err(|e| match e {
                        StaticNeighborInsertionError::MacAddressNotUnicast
                        | StaticNeighborInsertionError::IpAddressInvalid => {
                            zx::Status::INVALID_ARGS.into_raw()
                        }
                        StaticNeighborInsertionError::NotSupported(NotSupportedError) => {
                            zx::Status::NOT_SUPPORTED.into_raw()
                        }
                    });
                    responder.send(result)
                }
                ControllerRequest::RemoveEntry { interface, neighbor, responder } => {
                    let Some(device_id) = BindingId::new(interface)
                        .and_then(|id| non_sync_ctx.devices.get_core_id(id))
                    else {
                        return responder.send(Err(zx::Status::NOT_FOUND.into_raw()));
                    };
                    let result = match neighbor.into_ext() {
                        IpAddr::V4(v4) => {
                            netstack3_core::device::remove_neighbor_table_entry::<
                                Ipv4,
                                BindingsNonSyncCtxImpl,
                            >(sync_ctx, non_sync_ctx, &device_id, v4)
                        }
                        IpAddr::V6(v6) => {
                            netstack3_core::device::remove_neighbor_table_entry::<
                                Ipv6,
                                BindingsNonSyncCtxImpl,
                            >(sync_ctx, non_sync_ctx, &device_id, v6)
                        }
                    }
                    .map_err(|e| {
                        match e {
                            NeighborRemovalError::IpAddressInvalid => zx::Status::INVALID_ARGS,
                            NeighborRemovalError::NotFound(NotFoundError) => zx::Status::NOT_FOUND,
                            NeighborRemovalError::NotSupported(NotSupportedError) => {
                                zx::Status::NOT_SUPPORTED
                            }
                        }
                        .into_raw()
                    });
                    responder.send(result)
                }
                ControllerRequest::ClearEntries { interface, ip_version, responder } => {
                    let device_id = match BindingId::new(interface)
                        .and_then(|id| non_sync_ctx.devices.get_core_id(id))
                    {
                        Some(device_id) => device_id,
                        None => {
                            return responder.send(Err(zx::Status::NOT_FOUND.into_raw()));
                        }
                    };
                    let result = match ip_version {
                        fnet::IpVersion::V4 => {
                            netstack3_core::device::flush_neighbor_table::<
                                Ipv4,
                                BindingsNonSyncCtxImpl,
                            >(sync_ctx, non_sync_ctx, &device_id)
                        }
                        fnet::IpVersion::V6 => {
                            netstack3_core::device::flush_neighbor_table::<
                                Ipv6,
                                BindingsNonSyncCtxImpl,
                            >(sync_ctx, non_sync_ctx, &device_id)
                        }
                    }
                    .map_err(|NotSupportedError| zx::Status::NOT_SUPPORTED.into_raw());
                    responder.send(result)
                }
                ControllerRequest::UpdateUnreachabilityConfig {
                    interface: _,
                    ip_version: _,
                    config: _,
                    responder,
                } => {
                    warn!(
                        "TODO(https://fxbug.dev/124960): \
                            Implement fuchsia.net.neighbor/Controller.UpdateUnreachabilityConfig"
                    );
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
                }
            }
        })
        .await
}
