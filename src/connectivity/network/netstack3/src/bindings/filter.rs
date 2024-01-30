// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod controller;

use std::collections::{hash_map::Entry, HashMap};
use std::num::NonZeroUsize;
use std::sync::Arc;

use fidl::endpoints::{ControlHandle as _, ProtocolMarker as _};
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext as fnet_filter_ext;
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc, future::FusedFuture as _, lock::Mutex, FutureExt as _, StreamExt as _,
    TryStreamExt as _,
};
use itertools::Itertools as _;
use thiserror::Error;
use tracing::{error, warn};

use controller::Controller;

// The maximum number of events a client for the `fuchsia.net.filter/Watcher` is
// allowed to have queued. Clients will be dropped if they exceed this limit.
// Keep this a multiple of `fnet_filter::MAX_BATCH_SIZE` (5 is somewhat
// arbitrary) so that we don't artificially truncate the allowed batch size.
const MAX_PENDING_EVENTS: usize = (fnet_filter::MAX_BATCH_SIZE * 5) as usize;

#[derive(Default)]
pub(crate) struct UpdateDispatcher(Arc<Mutex<UpdateDispatcherInner>>);

#[derive(Default)]
struct UpdateDispatcherInner {
    resources: HashMap<fnet_filter_ext::ControllerId, Controller>,
    clients: Vec<WatcherSink>,
}

impl UpdateDispatcherInner {
    fn new_controller(
        &mut self,
        id: fnet_filter_ext::ControllerId,
    ) -> fnet_filter_ext::ControllerId {
        let Self { resources, clients: _ } = self;
        let mut counter = 0usize;
        let fnet_filter_ext::ControllerId(id) = id;
        loop {
            // Suffix the `id` with `counter` after the first iteration.
            let id = fnet_filter_ext::ControllerId(
                NonZeroUsize::new(counter)
                    .map(|counter| format!("{id}-{counter}"))
                    .unwrap_or(id.clone()),
            );
            match resources.entry(id.clone()) {
                Entry::Vacant(entry) => {
                    let _ = entry.insert(Controller::default());
                    return id;
                }
                Entry::Occupied(_) => {
                    counter =
                        counter.checked_add(1).expect("should find a unique ID before overflowing");
                }
            }
        }
    }

    fn remove_controller(&mut self, id: &fnet_filter_ext::ControllerId) {
        let Self { resources, clients } = self;
        let resources = resources.remove(id).expect("controller should only be removed once");

        // Notify all existing watchers of the removed resources.
        let events = resources
            .existing_ids()
            .map(|resource| fnet_filter_ext::Event::Removed(id.clone(), resource))
            .chain(std::iter::once(fnet_filter_ext::Event::EndOfUpdate))
            .collect::<Vec<_>>()
            .into_iter();
        for client in clients {
            client.send(events.clone());
        }
    }

    fn commit_changes(
        &mut self,
        controller: &fnet_filter_ext::ControllerId,
        changes: Vec<fnet_filter_ext::Change>,
        idempotent: bool,
    ) -> Result<(), (usize, fnet_filter::CommitError)> {
        let Self { resources, clients } = self;

        // NB: we update NS3 core and broadcast the updates to clients under a
        // single lock to ensure bindings and core see updates in the same
        // order.

        let events = resources
            .get_mut(controller)
            .expect("controller should not be removed while serving")
            .validate_and_apply(changes, idempotent)?;

        // TODO(https://fxbug.dev/318738286): propagate changes to Netstack3
        // Core once the necessary functionality is available. This will require
        // merging this controller's state with all state owned by other
        // controllers.

        // Notify all existing watchers of the update, if it was not a no-op.
        if !events.is_empty() {
            let events = events
                .into_iter()
                .map(|event| match event {
                    controller::Event::Added(resource) => {
                        fnet_filter_ext::Event::Added(controller.clone(), resource)
                    }
                    controller::Event::Removed(resource) => {
                        fnet_filter_ext::Event::Removed(controller.clone(), resource)
                    }
                })
                .chain(std::iter::once(fnet_filter_ext::Event::EndOfUpdate));
            for client in clients {
                client.send(events.clone());
            }
        }

        Ok(())
    }

    fn connect_new_client(&mut self) -> Watcher {
        let Self { resources, clients } = self;
        let (watcher, sink) = Watcher::new_with_existing_resources(resources);
        clients.push(sink);
        watcher
    }

    fn disconnect_client(&mut self, watcher: Watcher) {
        let Self { resources: _, clients } = self;
        let (i, _): (usize, &WatcherSink) = clients
            .iter()
            .enumerate()
            .filter(|(_i, client)| client.is_connected_to(&watcher))
            .exactly_one()
            .expect("watcher should be connected to exactly one sink");
        let _: WatcherSink = clients.swap_remove(i);
    }
}

#[derive(Debug)]
struct WatcherSink {
    sink: mpsc::Sender<fnet_filter_ext::Event>,
    cancel: async_utils::event::Event,
}

impl WatcherSink {
    fn send(&mut self, changes: impl Iterator<Item = fnet_filter_ext::Event>) {
        for change in changes {
            self.sink.try_send(change).unwrap_or_else(|e| {
                if e.is_full() {
                    if self.cancel.signal() {
                        warn!(
                            "too many unconsumed events (the client may not be \
                            calling Watch frequently enough): {}",
                            MAX_PENDING_EVENTS
                        )
                    }
                } else {
                    panic!("unexpected error trying to send: {:?}", e)
                }
            })
        }
    }

    fn is_connected_to(&self, watcher: &Watcher) -> bool {
        self.sink.is_connected_to(&watcher.receiver)
    }
}

struct Watcher {
    existing_resources: <Vec<fnet_filter_ext::Event> as std::iter::IntoIterator>::IntoIter,
    receiver: mpsc::Receiver<fnet_filter_ext::Event>,
    canceled: async_utils::event::Event,
}

impl Watcher {
    fn new_with_existing_resources(
        resources: &HashMap<fnet_filter_ext::ControllerId, Controller>,
    ) -> (Self, WatcherSink) {
        let existing_resources = resources
            .into_iter()
            .flat_map(|(controller, resources)| {
                resources
                    .existing_resources()
                    .map(|resource| fnet_filter_ext::Event::Existing(controller.clone(), resource))
            })
            .chain(std::iter::once(fnet_filter_ext::Event::Idle))
            .collect::<Vec<_>>()
            .into_iter();
        let (sender, receiver) = mpsc::channel(MAX_PENDING_EVENTS);
        let cancel = async_utils::event::Event::new();

        (
            Watcher { existing_resources, receiver, canceled: cancel.clone() },
            WatcherSink { sink: sender, cancel },
        )
    }

    fn watch(&mut self) -> impl futures::Future<Output = Vec<fnet_filter_ext::Event>> + Unpin + '_ {
        let Self { existing_resources, receiver, canceled: _ } = self;
        futures::stream::iter(existing_resources)
            .chain(receiver)
            .ready_chunks(fnet_filter::MAX_BATCH_SIZE.into())
            .into_future()
            .map(|(r, _ready_chunks)| r.expect("underlying event stream unexpectedly ended"))
    }
}

pub(crate) async fn serve_state(
    stream: fnet_filter::StateRequestStream,
    dispatcher: &UpdateDispatcher,
) -> Result<(), fidl::Error> {
    stream
        .try_for_each_concurrent(None, |request| async {
            match request {
                fnet_filter::StateRequest::GetWatcher {
                    options: _,
                    request,
                    control_handle: _,
                } => {
                    let requests =
                        request.into_stream().expect("get request stream from server end");
                    serve_watcher(requests, dispatcher).await.unwrap_or_else(|e| {
                        warn!("error serving {}: {e:?}", fnet_filter::WatcherMarker::DEBUG_NAME)
                    });
                }
            }
            Ok(())
        })
        .await
}

#[derive(Debug, Error)]
enum ServeWatcherError {
    #[error("the request stream contained a FIDL error")]
    ErrorInStream(fidl::Error),
    #[error("a FIDL error was encountered while sending the response")]
    FailedToRespond(fidl::Error),
    #[error("the client called `Watch` while a previous call was already pending")]
    PreviousPendingWatch,
    #[error("the client was canceled")]
    Canceled,
}

async fn serve_watcher(
    stream: fnet_filter::WatcherRequestStream,
    UpdateDispatcher(dispatcher): &UpdateDispatcher,
) -> Result<(), ServeWatcherError> {
    let mut watcher = dispatcher.lock().await.connect_new_client();
    let canceled_fut = watcher.canceled.wait();

    let result = {
        let mut request_stream = stream.map_err(ServeWatcherError::ErrorInStream).fuse();
        futures::pin_mut!(canceled_fut);
        let mut hanging_get = futures::future::OptionFuture::default();
        loop {
            hanging_get = futures::select! {
                request = request_stream.try_next() => match request {
                    Ok(Some(fnet_filter::WatcherRequest::Watch { responder } )) => {
                        if hanging_get.is_terminated() {
                            // Convince the compiler that we're not holding on to a
                            // borrow of `watcher`.
                            drop(hanging_get);

                            // Either there is no pending request or the previously
                            // pending one has completed. We can accept a new
                            // hanging get.
                            Some(watcher.watch().map(move |events| (responder, events))).into()
                        } else {
                            break Err(ServeWatcherError::PreviousPendingWatch);
                        }
                    },
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                },
                r = hanging_get => {
                    let (responder, events) = r.expect("OptionFuture is not selected if empty");
                    let events = events.into_iter().map(Into::into).collect::<Vec<_>>();
                    match responder.send(&events) {
                        Ok(()) => None.into(),
                        Err(e) => break Err(ServeWatcherError::FailedToRespond(e)),
                    }
                },
                () = canceled_fut => break Err(ServeWatcherError::Canceled),
            };
        }
    };

    dispatcher.lock().await.disconnect_client(watcher);

    result
}

pub(crate) async fn serve_control(
    stream: fnet_filter::ControlRequestStream,
    dispatcher: &UpdateDispatcher,
) -> Result<(), fidl::Error> {
    use fnet_filter::ControlRequest;

    stream
        .try_for_each_concurrent(None, |request| async {
            match request {
                ControlRequest::OpenController { id, request, control_handle: _ } => {
                    let UpdateDispatcher(inner) = dispatcher;
                    let final_id =
                        inner.lock().await.new_controller(fnet_filter_ext::ControllerId(id));

                    let (stream, control_handle) = request.into_stream_and_control_handle()?;

                    serve_controller(&final_id, stream, control_handle, dispatcher)
                        .await
                        .unwrap_or_else(|e| warn!("error serving namespace controller: {e:?}"));

                    inner.lock().await.remove_controller(&final_id);
                }
                ControlRequest::ReopenDetachedController { key: _, request: _, control_handle } => {
                    error!("TODO(https://fxbug.dev/42182623): detaching is not implemented");
                    control_handle.shutdown_with_epitaph(zx::Status::NOT_SUPPORTED);
                }
            }
            Ok(())
        })
        .await
}

async fn serve_controller(
    id: &fnet_filter_ext::ControllerId,
    mut stream: fnet_filter::NamespaceControllerRequestStream,
    control_handle: fnet_filter::NamespaceControllerControlHandle,
    UpdateDispatcher(dispatcher): &UpdateDispatcher,
) -> Result<(), fidl::Error> {
    use fnet_filter::NamespaceControllerRequest;

    control_handle.send_on_id_assigned(&id.0)?;

    let mut pending_changes = Vec::new();
    while let Some(request) = stream.try_next().await? {
        match request {
            NamespaceControllerRequest::PushChanges { changes, responder } => {
                let num_changes = changes.len();
                if pending_changes.len() + num_changes > fnet_filter::MAX_COMMIT_SIZE.into() {
                    responder.send(fnet_filter::ChangeValidationResult::TooManyChanges(
                        fnet_filter::Empty {},
                    ))?;
                    continue;
                }

                enum Error {
                    ReturnToClient(fnet_filter::ChangeValidationError),
                    Fatal,
                }

                let result = changes
                    .into_iter()
                    .enumerate()
                    .map(|(i, change)| {
                        change.try_into().map_err(|error| {
                            use fnet_filter::ChangeValidationError;
                            use fnet_filter_ext::FidlConversionError;

                            warn!("encountered an invalid filter change at index {i}: {error:?}");

                            let error = match error {
                                FidlConversionError::UnknownUnionVariant(type_name) => {
                                    warn!("client specified unknown variant of {type_name}");
                                    Error::Fatal
                                }
                                FidlConversionError::MissingNamespaceId
                                | FidlConversionError::MissingNamespaceDomain
                                | FidlConversionError::MissingRoutineId
                                | FidlConversionError::MissingRoutineType
                                | FidlConversionError::MissingIpInstallationHook
                                | FidlConversionError::MissingNatInstallationHook => {
                                    Error::ReturnToClient(
                                        ChangeValidationError::MissingRequiredField,
                                    )
                                }
                                FidlConversionError::InvalidAddressRange
                                | FidlConversionError::AddressRangeFamilyMismatch
                                | FidlConversionError::SubnetPrefixTooLong
                                | FidlConversionError::SubnetHostBitsSet => Error::ReturnToClient(
                                    ChangeValidationError::InvalidAddressMatcher,
                                ),
                                FidlConversionError::InvalidPortRange => {
                                    Error::ReturnToClient(ChangeValidationError::InvalidPortMatcher)
                                }
                                FidlConversionError::NotAnError => unreachable!(
                                    "should not get this error when converting a `Change`"
                                ),
                            };
                            (i, error)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|changes| {
                        pending_changes.extend(changes);
                    });

                let result = match result {
                    Ok(()) => fnet_filter::ChangeValidationResult::Ok(fnet_filter::Empty {}),
                    Err((valid_change_count, error)) => {
                        let error = match error {
                            Error::ReturnToClient(error) => error,
                            Error::Fatal => {
                                error!("fatal error serving filtering controller; closing channel");
                                control_handle.shutdown_with_epitaph(zx::Status::NOT_SUPPORTED);
                                return Ok(());
                            }
                        };
                        // Return `Ok` for all valid changes, followed by the error we encountered,
                        // and pad the results with `NotReached` for the remaining changes.
                        let errors = std::iter::repeat(fnet_filter::ChangeValidationError::Ok)
                            .take(valid_change_count)
                            .chain(std::iter::once(error))
                            .chain(
                                std::iter::repeat(fnet_filter::ChangeValidationError::NotReached)
                                    .take(num_changes - valid_change_count - 1),
                            )
                            .collect::<Vec<_>>();
                        fnet_filter::ChangeValidationResult::ErrorOnChange(errors)
                    }
                };
                responder.send(result)?;
            }
            NamespaceControllerRequest::Commit { payload: options, responder } => {
                let fnet_filter::CommitOptions { idempotent, .. } = options;
                let idempotent = idempotent.unwrap_or(false);
                let changes = std::mem::take(&mut pending_changes);
                let num_changes = changes.len();
                let result = dispatcher.lock().await.commit_changes(id, changes, idempotent);
                let result = match result {
                    Ok(()) => fnet_filter::CommitResult::Ok(fnet_filter::Empty {}),
                    Err((i, error)) => {
                        let errors = std::iter::repeat(fnet_filter::CommitError::Ok)
                            .take(i)
                            .chain(std::iter::once(error))
                            .chain(
                                std::iter::repeat(fnet_filter::CommitError::NotReached)
                                    .take(num_changes - i - 1),
                            )
                            .collect::<Vec<_>>();
                        fnet_filter::CommitResult::ErrorOnChange(errors)
                    }
                };
                responder.send(result)?;
            }
            NamespaceControllerRequest::Detach { responder: _ } => {
                error!("TODO(https://fxbug.dev/42182623): detaching is not implemented");
                control_handle.shutdown_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
        }
    }
    Ok(())
}

// TODO(https://fxbug.dev/42182576): remove this once NetCfg interacts with
// fuchsia.net.filter for Netstack3.
pub(crate) async fn serve_deprecated(
    stream: fidl_fuchsia_net_filter_deprecated::FilterRequestStream,
) -> Result<(), fidl::Error> {
    use fidl_fuchsia_net_filter_deprecated::FilterRequest;

    stream
        .try_for_each(|request: FilterRequest| async move {
            match request {
                FilterRequest::DisableInterface { responder, .. } => {
                    error!(
                        "fuchsia.net.filter.deprecated.Filter is not implemented \
                           (https://fxbug.dev/42182576); ignoring DisableInterface"
                    );
                    responder.send(Ok(())).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
                FilterRequest::EnableInterface { responder, .. } => {
                    error!(
                        "fuchsia.net.filter.deprecated.Filter is not implemented \
                           (https://fxbug.dev/42182576); ignoring EnableInterface"
                    );
                    responder.send(Ok(())).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
                FilterRequest::GetRules { responder } => {
                    error!(
                        "fuchsia.net.filter.deprecated.Filter is not implemented \
                           (https://fxbug.dev/42182576); ignoring GetRules"
                    );
                    responder
                        .send(&[], 0)
                        .unwrap_or_else(|e| error!("Responder send error: {:?}", e))
                }
                FilterRequest::UpdateRules { rules, generation, responder } => {
                    error!(
                        "fuchsia.net.filter.deprecated.Filter is not implemented \
                            (https://fxbug.dev/42182576); ignoring UpdateRules \
                            {{ generation: {:?}, rules: {:?} }}",
                        generation, rules
                    );
                    responder.send(Ok(())).unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
                FilterRequest::GetNatRules { .. } => {
                    todo!("https://fxbug.dev/42182576: implement filtering support");
                }
                FilterRequest::UpdateNatRules { .. } => {
                    todo!("https://fxbug.dev/42182576: implement filtering support");
                }
                FilterRequest::GetRdrRules { .. } => {
                    todo!("https://fxbug.dev/42182576: implement filtering support");
                }
                FilterRequest::UpdateRdrRules { .. } => {
                    todo!("https://fxbug.dev/42182576: implement filtering support");
                }
            };
            Ok(())
        })
        .await
}
