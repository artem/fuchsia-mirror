// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl::endpoints::{create_proxy, ControlHandle, Proxy, RequestStream},
    fidl_fuchsia_element as element, fidl_fuchsia_session_scene as scene,
    fidl_fuchsia_ui_composition as ui_comp, fidl_fuchsia_ui_views as ui_views,
    fuchsia_async as fasync,
    fuchsia_component::{client::connect_to_protocol, server::ServiceFs, server::ServiceObj},
    fuchsia_scenic::{flatland::IdGenerator, flatland::ViewCreationTokenPair, ViewRefPair},
    fuchsia_zircon as zx,
    futures::{channel::mpsc::UnboundedSender, StreamExt, TryStreamExt},
    std::collections::HashMap,
    tracing::{error, info, warn},
};

// The maximum number of concurrent services to serve.
const NUM_CONCURRENT_REQUESTS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileId(pub u64);

impl std::fmt::Display for TileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "id={}", self.0)
    }
}

pub enum MessageInternal {
    GraphicalPresenterPresentView {
        view_spec: element::ViewSpec,
        annotation_controller: Option<element::AnnotationControllerProxy>,
        view_controller_request_stream: Option<element::ViewControllerRequestStream>,
        responder: element::GraphicalPresenterPresentViewResponder,
    },
    DismissClient {
        tile_id: TileId,
        control_handle: element::ViewControllerControlHandle,
    },
    ClientDied {
        tile_id: TileId,
    },
    ReceivedClientViewRef {
        tile_id: TileId,
        view_ref: ui_views::ViewRef,
    },
}

struct ChildView {
    viewport_transform_id: ui_comp::TransformId,
    viewport_content_id: ui_comp::ContentId,
}

pub struct TilingWm {
    internal_sender: UnboundedSender<MessageInternal>,
    flatland: ui_comp::FlatlandProxy,
    id_generator: IdGenerator,
    view_focuser: ui_views::FocuserProxy,
    root_transform_id: ui_comp::TransformId,
    layout_info: ui_comp::LayoutInfo,
    tiles: HashMap<TileId, ChildView>,
    next_tile_id: u64,
}

impl Drop for TilingWm {
    fn drop(&mut self) {
        info!("dropping TilingWm");
        let flatland = &self.flatland;
        let tiles = &mut self.tiles;
        tiles.retain(|key, tile| {
            if let Err(e) = Self::release_tile_resources(flatland, tile) {
                error!("Error releasing resources for tile {key}: {e}");
            }
            false
        });
        if let Err(e) = flatland.clear() {
            error!("Error clearing Flatland: {e}");
        }
    }
}

impl TilingWm {
    async fn handle_message(&mut self, message: MessageInternal) -> Result<(), Error> {
        match message {
            // The ElementManager has asked us (via GraphicalPresenter::PresentView()) to display
            // the view provided by a newly-launched element.
            MessageInternal::GraphicalPresenterPresentView {
                view_spec,
                annotation_controller,
                view_controller_request_stream,
                responder,
            } => {
                // We have either a view holder token OR a viewport_creation_token, but for
                // Flatland we can expect a viewport creation token.
                let viewport_creation_token = match view_spec.viewport_creation_token {
                    Some(token) => token,
                    None => {
                        warn!("Client attempted to present Gfx component but only Flatland is supported.");
                        return Ok(());
                    }
                };

                // Create a Viewport that houses the view we are creating.
                let (tile_watcher, tile_watcher_request) =
                    create_proxy::<ui_comp::ChildViewWatcherMarker>()?;
                let viewport_content_id = self.id_generator.next_content_id();
                let viewport_properties = ui_comp::ViewportProperties {
                    logical_size: Some(self.layout_info.logical_size.unwrap()),
                    ..Default::default()
                };
                self.flatland
                    .create_viewport(
                        &viewport_content_id,
                        viewport_creation_token,
                        &viewport_properties,
                        tile_watcher_request,
                    )
                    .context("GraphicalPresenterPresentView create_viewport")?;

                // Attach the Viewport to the scene graph.
                let viewport_transform_id = self.id_generator.next_transform_id();
                self.flatland
                    .create_transform(&viewport_transform_id)
                    .context("GraphicalPresenterPresentView create_transform")?;
                self.flatland
                    .set_content(&viewport_transform_id, &viewport_content_id)
                    .context("GraphicalPresenterPresentView create_transform")?;
                self.flatland
                    .add_child(&self.root_transform_id, &viewport_transform_id)
                    .context("GraphicalPresenterPresentView add_child")?;

                // Flush the changes.
                self.flatland
                    .present(ui_comp::PresentArgs {
                        requested_presentation_time: Some(0),
                        ..Default::default()
                    })
                    .context("GraphicalPresenterPresentView present")?;

                // Track all of the child view's resources.
                let new_tile_id = TileId(self.next_tile_id);
                self.next_tile_id += 1;
                self.tiles
                    .insert(new_tile_id, ChildView { viewport_transform_id, viewport_content_id });

                // Alert the client that the view has been presented, then begin servicing ViewController requests.
                let view_controller_request_stream = view_controller_request_stream.unwrap();
                view_controller_request_stream
                    .control_handle()
                    .send_on_presented()
                    .context("GraphicalPresenterPresentView send_on_presented")?;
                run_tile_controller_request_stream(
                    new_tile_id,
                    view_controller_request_stream,
                    self.internal_sender.clone(),
                );

                // Begin servicing ChildViewWatcher requests.
                Self::watch_tile(new_tile_id, tile_watcher, self.internal_sender.clone());

                // Ignore Annotations for now.
                let _ = annotation_controller;

                // Finally, acknowledge the PresentView request.
                if let Err(e) = responder.send(Ok(())) {
                    error!("Failed to send response for GraphicalPresenter.PresentView(): {}", e);
                }

                Ok(())
            }
            MessageInternal::DismissClient { tile_id, control_handle } => {
                // Explicitly shutting down the handle indicates intentionality, instead of
                // (for example) because this component crashed and the handle was auto-closed.
                control_handle.shutdown_with_epitaph(zx::Status::OK);
                match &mut self.tiles.remove(&tile_id) {
                    Some(tile) => Self::release_tile_resources(&self.flatland, tile)
                        .context("DismissClient release_tile_resources")?,
                    None => error!("Tile not found after client requested dismiss: {tile_id}"),
                }

                Ok(())
            }
            MessageInternal::ClientDied { tile_id } => {
                match &mut self.tiles.remove(&tile_id) {
                    Some(tile) => Self::release_tile_resources(&self.flatland, tile)
                        .context("ClientDied release_tile_resources")?,
                    None => error!("Tile not found after client died: {tile_id}"),
                }

                Ok(())
            }
            MessageInternal::ReceivedClientViewRef { tile_id, view_ref, .. } => {
                let result = self.view_focuser.request_focus(view_ref);
                fasync::Task::local(async move {
                    match result.await {
                        Ok(Ok(())) => {
                            info!("Successfully requested focus on child {tile_id}")
                        }
                        Ok(Err(e)) => {
                            error!("Error while requesting focus on child {tile_id}: {e:?}")
                        }
                        Err(e) => {
                            error!("FIDL error while requesting focus on child {tile_id}: {e:?}")
                        }
                    }
                })
                .detach();

                Ok(())
            }
        }
    }

    pub async fn new(internal_sender: UnboundedSender<MessageInternal>) -> Result<TilingWm, Error> {
        // TODO(https://fxbug.dev/42169911): do something like this to instantiate the library component that knows
        // how to generate a Flatland scene to lay views out on a tiled grid.  It will be used in the
        // event loop below.
        // let tiles_helper = tile_helper::TilesHelper::new();

        // Set the root view and then wait for scene_manager to reply with a CreateView2 request.
        // Don't await the result yet, because the future will not resolve until we handle the
        // ViewProvider request below.
        let scene_manager = connect_to_protocol::<scene::ManagerMarker>()
            .expect("failed to connect to fuchsia.scene.Manager");

        // TODO(https://fxbug.dev/42055565): see scene_manager.fidl.  If we awaited the future immediately we
        // would deadlock.  Conversely, if we just dropped the future, then scene_manager would barf
        // because it would try to reply to present_root_view() on a closed channel.  So we kick off
        // the async FIDL request (which is not idiomatic for Rust, where typically the "future
        // doesn't do anything" until awaited), and then call create_wm() so
        // that present_root_view() eventually returns a result.
        let ViewCreationTokenPair { view_creation_token, viewport_creation_token } =
            ViewCreationTokenPair::new()?;
        let fut = scene_manager.present_root_view(viewport_creation_token);
        let wm = Self::create_wm(view_creation_token, internal_sender).await?;
        let _ = fut.await?;
        Ok(wm)
    }

    async fn create_wm(
        view_creation_token: ui_views::ViewCreationToken,
        internal_sender: UnboundedSender<MessageInternal>,
    ) -> Result<TilingWm, Error> {
        let flatland = connect_to_protocol::<ui_comp::FlatlandMarker>()
            .expect("failed to connect to fuchsia.ui.flatland.Flatland");
        let mut id_generator = IdGenerator::new();

        // Create the root transform for tiles.
        let root_transform_id = id_generator.next_transform_id();
        flatland.create_transform(&root_transform_id)?;
        flatland.set_root_transform(&root_transform_id)?;

        // Create the root view for tiles.
        let (parent_viewport_watcher, parent_viewport_watcher_request) =
            create_proxy::<ui_comp::ParentViewportWatcherMarker>()
                .expect("Failed to create ParentViewportWatcher channel");
        let (view_focuser, view_focuser_request) =
            fidl::endpoints::create_proxy::<ui_views::FocuserMarker>()
                .expect("Failed to create Focuser channel");
        let view_identity = ui_views::ViewIdentityOnCreation::from(ViewRefPair::new()?);
        let view_bound_protocols = ui_comp::ViewBoundProtocols {
            view_focuser: Some(view_focuser_request),
            ..Default::default()
        };
        flatland.create_view2(
            view_creation_token,
            view_identity,
            view_bound_protocols,
            parent_viewport_watcher_request,
        )?;

        // Present the root scene.
        flatland.present(ui_comp::PresentArgs {
            requested_presentation_time: Some(0),
            ..Default::default()
        })?;

        // Get initial layout deterministically before proceeding.
        // Begin servicing ParentViewportWatcher requests.
        let layout_info = parent_viewport_watcher.get_layout().await?;
        Self::watch_layout(parent_viewport_watcher, internal_sender.clone());

        Ok(TilingWm {
            internal_sender,
            flatland,
            id_generator,
            view_focuser,
            root_transform_id,
            layout_info,
            tiles: HashMap::new(),
            next_tile_id: 0,
        })
    }

    fn release_tile_resources(
        flatland: &ui_comp::FlatlandProxy,
        tile: &mut ChildView,
    ) -> Result<(), Error> {
        let _ = flatland.release_viewport(&tile.viewport_content_id);
        flatland.release_transform(&tile.viewport_transform_id)?;
        Ok(())
    }

    fn watch_layout(
        proxy: ui_comp::ParentViewportWatcherProxy,
        _internal_sender: UnboundedSender<MessageInternal>,
    ) {
        // Listen for channel closure.
        // TODO(https://fxbug.dev/42169911): Actually watch for and respond to layout changes.
        fasync::Task::local(async move {
            let _ = proxy.on_closed().await;
        })
        .detach();
    }

    fn watch_tile(
        tile_id: TileId,
        proxy: ui_comp::ChildViewWatcherProxy,
        internal_sender: UnboundedSender<MessageInternal>,
    ) {
        // Get view ref, then listen for channel closure.
        fasync::Task::local(async move {
            match proxy.get_view_ref().await {
                Ok(view_ref) => {
                    internal_sender
                        .unbounded_send(MessageInternal::ReceivedClientViewRef {
                            tile_id,
                            view_ref,
                        })
                        .expect("Failed to send MessageInternal::ReceivedClientViewRef");
                }
                Err(_) => {
                    internal_sender
                        .unbounded_send(MessageInternal::ClientDied { tile_id })
                        .expect("Failed to send MessageInternal::ClientDied");
                    return;
                }
            }

            let _ = proxy.on_closed().await;

            internal_sender
                .unbounded_send(MessageInternal::ClientDied { tile_id })
                .expect("Failed to send MessageInternal::ClientDied");
        })
        .detach();
    }
}

enum ExposedServices {
    GraphicalPresenter(element::GraphicalPresenterRequestStream),
}

fn expose_services() -> Result<ServiceFs<ServiceObj<'static, ExposedServices>>, Error> {
    let mut fs = ServiceFs::new();

    // Add services for component outgoing directory.
    fs.dir("svc").add_fidl_service(ExposedServices::GraphicalPresenter);
    fs.take_and_serve_directory_handle()?;

    Ok(fs)
}

fn run_services(
    fs: ServiceFs<ServiceObj<'static, ExposedServices>>,
    internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        fs.for_each_concurrent(NUM_CONCURRENT_REQUESTS, |service_request: ExposedServices| async {
            match service_request {
                ExposedServices::GraphicalPresenter(request_stream) => {
                    run_graphical_presenter_service(request_stream, internal_sender.clone());
                }
            }
        })
        .await;
    })
    .detach();
}

fn run_graphical_presenter_service(
    mut request_stream: element::GraphicalPresenterRequestStream,
    mut internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        loop {
            let result = request_stream.try_next().await;
            match result {
                Ok(Some(request)) => {
                    internal_sender = handle_graphical_presenter_request(request, internal_sender)
                }
                Ok(None) => {
                    info!("GraphicalPresenterRequestStream ended with Ok(None)");
                    return;
                }
                Err(e) => {
                    error!(
                        "Error while retrieving requests from GraphicalPresenterRequestStream: {}",
                        e
                    );
                    return;
                }
            }
        }
    })
    .detach();
}

fn handle_graphical_presenter_request(
    request: element::GraphicalPresenterRequest,
    internal_sender: UnboundedSender<MessageInternal>,
) -> UnboundedSender<MessageInternal> {
    match request {
        element::GraphicalPresenterRequest::PresentView {
            view_spec,
            annotation_controller,
            view_controller_request,
            responder,
        } => {
            // "Unwrap" the optional element::AnnotationControllerProxy.
            let annotation_controller = match annotation_controller {
                Some(proxy) => match proxy.into_proxy() {
                    Ok(proxy) => Some(proxy),
                    Err(e) => {
                        warn!("Failed to obtain AnnotationControllerProxy: {}", e);
                        None
                    }
                },
                None => None,
            };
            // "Unwrap" the optional element::ViewControllerRequestStream.
            let view_controller_request_stream = match view_controller_request {
                Some(request_stream) => match request_stream.into_stream() {
                    Ok(request_stream) => Some(request_stream),
                    Err(e) => {
                        warn!("Failed to obtain ViewControllerRequestStream: {}", e);
                        None
                    }
                },
                None => None,
            };
            internal_sender
                .unbounded_send(
                    MessageInternal::GraphicalPresenterPresentView {
                        view_spec,
                        annotation_controller,
                        view_controller_request_stream,
                        responder,
                    },
                    // TODO(https://fxbug.dev/42169911): is this a safe expect()?  I think so, since
                    // we're using Task::local() instead of Task::spawn(), so we're on the
                    // same thread as main(), which will keep the receiver end alive until
                    // it exits, at which time the executor will not tick this task again.
                    // Assuming that we verify this understanding, what is the appropriate
                    // way to document this understanding?  Is it so idiomatic it needs no
                    // comment?  We're all Rust n00bs here, so maybe not?
                )
                .expect("Failed to send MessageInternal.");
        }
    }
    return internal_sender;
}

// Serve the fuchsia.element.ViewController protocol. This merely redispatches
// the requests onto the `MessageInternal` handler, which are handled by
// `TilingWm::handle_message`.
pub fn run_tile_controller_request_stream(
    tile_id: TileId,
    mut request_stream: fidl_fuchsia_element::ViewControllerRequestStream,
    internal_sender: UnboundedSender<MessageInternal>,
) {
    fasync::Task::local(async move {
        if let Some(Ok(fidl_fuchsia_element::ViewControllerRequest::Dismiss { control_handle })) =
            request_stream.next().await
        {
            {
                internal_sender
                    .unbounded_send(MessageInternal::DismissClient { tile_id, control_handle })
                    .expect("Failed to send MessageInternal::DismissClient");
            }
        }
    })
    .detach();
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    let (internal_sender, mut internal_receiver) =
        futures::channel::mpsc::unbounded::<MessageInternal>();

    // We start listening for service requests, but don't yet start serving those requests until we
    // we receive confirmation that we are hooked up to the Scene Manager.
    let fs = expose_services()?;

    // Connect to the scene owner and attach our tiles view to it.
    let mut wm = Box::new(TilingWm::new(internal_sender.clone()).await?);

    // Serve the FIDL services on the message loop, proxying them into internal messages.
    run_services(fs, internal_sender.clone());

    // Process internal messages using tiling wm, then cleanup when done.
    while let Some(message) = internal_receiver.next().await {
        if let Err(e) = wm.handle_message(message).await {
            error!("Error handling message: {e}");
            break;
        }
    }

    Ok(())
}
