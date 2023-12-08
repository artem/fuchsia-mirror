// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains for creating and serving a `Flatland` view using a `Framebuffer`.
//!
//! A lot of the code in this file is temporary to enable developers to see the contents of a
//! `Framebuffer` in the workstation UI (e.g., using `ffx session add`).
//!
//! To display the `Framebuffer` as its view, a component must add the `framebuffer` feature to its
//! `.cml`.

use crate::task::Kernel;
use anyhow::anyhow;
use fidl::{
    endpoints::{create_proxy, create_request_stream},
    HandleBased,
};
use fidl_fuchsia_element as felement;
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_sysmem as fsysmem;
use fidl_fuchsia_ui_app as fuiapp;
use fidl_fuchsia_ui_composition as fuicomposition;
use fidl_fuchsia_ui_views as fuiviews;
use flatland_frame_scheduling_lib::{
    PresentationInfo, PresentedInfo, SchedulingLib, ThroughputScheduler,
};
use fuchsia_async as fasync;
use fuchsia_component::{
    client::{connect_to_protocol, connect_to_protocol_at_dir_root, connect_to_protocol_sync},
    server::ServiceFs,
};
use fuchsia_framebuffer::{sysmem::BufferCollectionAllocator, FrameUsage};
use fuchsia_scenic::{flatland::ViewCreationTokenPair, BufferCollectionTokenPair};
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    FutureExt, StreamExt, TryStreamExt,
};
use starnix_lifecycle::AtomicU64Counter;
use starnix_lock::Mutex;
use std::{
    ops::{Deref, DerefMut},
    sync::{mpsc::channel, Arc},
};

use starnix_logging::{log_error, log_warn};
use starnix_uapi::{errno, errors::Errno};

/// The offset at which the framebuffer will be placed.
pub const TRANSLATION_X: i32 = 0;

/// The Flatland identifier for the transform associated with the framebuffer.
const ROOT_TRANSFORM_ID: fuicomposition::TransformId = fuicomposition::TransformId { value: 1 };

/// The Flatland identifier for the framebuffer image.
const FB_IMAGE_ID: fuicomposition::ContentId = fuicomposition::ContentId { value: 1 };

/// The protocols that are exposed by the framebuffer server.
enum ExposedProtocols {
    ViewProvider(fuiapp::ViewProviderRequestStream),
}

/// The Scene states that `FramebufferServer` may serve.
#[derive(Copy, Clone)]
pub enum SceneState {
    Fb,
    Viewport,
}

/// Unbounded sender used for presentation messages.
pub type PresentationSender = UnboundedSender<SceneState>;

/// Unbounded receiver used for presentation messages.
pub type PresentationReceiver = UnboundedReceiver<SceneState>;

/// A `FramebufferServer` contains initialized proxies to Flatland, as well as a buffer collection
/// that is registered with Flatland.
pub struct FramebufferServer {
    /// The Flatland proxy associated with this server.
    flatland: fuicomposition::FlatlandProxy,

    /// The buffer collection that is registered with Flatland.
    collection: fsysmem::BufferCollectionInfo2,

    /// The width of the display and framebuffer image.
    image_width: u32,

    /// The height of the display and framebuffer image.
    image_height: u32,

    /// Keeps track if this class is serving FB or a Viewport.
    scene_state: Arc<Mutex<SceneState>>,

    /// Keeps track of the Flatland viewport ID.
    viewport_id: AtomicU64Counter,

    /// Channel to send Present requests on.
    presentation_sender: PresentationSender,

    /// Channel to receive Present requests on.
    presentation_receiver: Arc<Mutex<Option<PresentationReceiver>>>,
}

impl FramebufferServer {
    /// Returns a `FramebufferServer` that has created a scene and registered a buffer with
    /// Flatland.
    pub fn new(width: u32, height: u32) -> Result<Self, Errno> {
        let allocator = connect_to_protocol_sync::<fuicomposition::AllocatorMarker>()
            .map_err(|_| errno!(ENOENT))?;
        let flatland =
            connect_to_protocol::<fuicomposition::FlatlandMarker>().map_err(|_| errno!(ENOENT))?;
        flatland.set_debug_name("StarnixFrameBufferServer").map_err(|_| errno!(EINVAL))?;

        let collection =
            init_fb_scene(&flatland, &allocator, width, height).map_err(|_| errno!(EINVAL))?;

        let (presentation_sender, presentation_receiver) = unbounded();
        Ok(Self {
            flatland,
            collection,
            image_width: width,
            image_height: height,
            scene_state: Arc::new(Mutex::new(SceneState::Fb)),
            viewport_id: (FB_IMAGE_ID.value + 1).into(),
            presentation_sender: presentation_sender,
            presentation_receiver: Arc::new(Mutex::new(Some(presentation_receiver))),
        })
    }

    /// Returns a clone of the VMO that is shared with Flatland.
    pub fn get_vmo(&self) -> Result<zx::Vmo, Errno> {
        self.collection.buffers[0]
            .vmo
            .as_ref()
            .ok_or_else(|| errno!(EINVAL))?
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .map_err(|_| errno!(EINVAL))
    }

    // Present according to the scene state.
    pub fn present(&self) {
        let scene_state = self.scene_state.lock();
        let scene_state = scene_state.deref();
        self.presentation_sender.unbounded_send(*scene_state).expect("send failed");
    }
}

/// Initializes the flatland scene, and returns the associated buffer collection.
///
/// SAFETY: This function `.expect`'s a lot, because it isn't meant to be used in the long time and
/// most of the failures would be unexpected and unrecoverable.
fn init_fb_scene(
    flatland: &fuicomposition::FlatlandProxy,
    allocator: &fuicomposition::AllocatorSynchronousProxy,
    width: u32,
    height: u32,
) -> Result<fsysmem::BufferCollectionInfo2, anyhow::Error> {
    flatland
        .create_transform(&ROOT_TRANSFORM_ID)
        .map_err(|_| anyhow!("error creating transform"))?;
    flatland
        .set_root_transform(&ROOT_TRANSFORM_ID)
        .map_err(|_| anyhow!("error setting root transform"))?;

    let (collection_sender, collection_receiver) = channel();
    let (allocation_sender, allocation_receiver) = channel();
    // This thread is spawned to deal with the mix of asynchronous and synchronous proxies.
    // In particular, we want to keep Framebuffer creation synchronous, while still making use of
    // BufferCollectionAllocator (which exposes an async api).
    //
    // The spawned thread will execute the futures and send results back to this thread via a
    // channel.
    std::thread::Builder::new()
        .name("kthread-fb-alloc".to_string())
        .spawn(move || -> Result<(), anyhow::Error> {
            let mut executor = fasync::LocalExecutor::new();

            let mut buffer_allocator = BufferCollectionAllocator::new(
                width,
                height,
                fidl_fuchsia_sysmem::PixelFormatType::R8G8B8A8,
                FrameUsage::Cpu,
                1,
            )?;
            buffer_allocator.set_name(100, "Starnix ViewProvider")?;

            let sysmem_buffer_collection_token =
                executor.run_singlethreaded(buffer_allocator.duplicate_token())?;
            // Notify the async code that the sysmem buffer collection token is available.
            collection_sender
                .send(sysmem_buffer_collection_token)
                .expect("Failed to send collection");

            let allocation =
                executor.run_singlethreaded(buffer_allocator.allocate_buffers(true))?;
            // Notify the async code that the buffer allocation completed.
            allocation_sender.send(allocation).expect("Failed to send allocation");

            Ok(())
        })
        .expect("able to create threads");

    // Wait for the async code to generate the buffer collection token.
    let sysmem_buffer_collection_token = collection_receiver
        .recv()
        .map_err(|_| anyhow!("Error receiving buffer collection token"))?;

    let buffer_tokens = BufferCollectionTokenPair::new();
    let args = fuicomposition::RegisterBufferCollectionArgs {
        export_token: Some(buffer_tokens.export_token),
        buffer_collection_token: Some(sysmem_buffer_collection_token),
        ..Default::default()
    };

    allocator
        .register_buffer_collection(args, zx::Time::INFINITE)
        .map_err(|_| anyhow!("FIDL error registering buffer collection"))?
        .map_err(|_| anyhow!("Error registering buffer collection"))?;

    // Now that the buffer collection is registered, wait for the buffer allocation to happen.
    let allocation =
        allocation_receiver.recv().map_err(|_| anyhow!("Error receiving buffer allocation"))?;

    let image_props = fuicomposition::ImageProperties {
        size: Some(fmath::SizeU { width, height }),
        ..Default::default()
    };
    flatland
        .create_image(&FB_IMAGE_ID, buffer_tokens.import_token, 0, &image_props)
        .map_err(|_| anyhow!("FIDL error creating image"))?;
    flatland
        .set_image_destination_size(&FB_IMAGE_ID, &fmath::SizeU { width, height })
        .expect("FIDL error resizing image");
    flatland
        .set_content(&ROOT_TRANSFORM_ID, &FB_IMAGE_ID)
        .map_err(|_| anyhow!("error setting content"))?;
    flatland
        .set_translation(&ROOT_TRANSFORM_ID, &fmath::Vec_ { x: TRANSLATION_X, y: 0 })
        .map_err(|_| anyhow!("error setting translation"))?;

    Ok(allocation)
}

/// Initializes a flatland scene where only the child view is presented through
/// `ViewportCreationToken`.
pub fn init_viewport_scene(
    server: Arc<FramebufferServer>,
    viewport_token: fuiviews::ViewportCreationToken,
) {
    let (_, child_view_watcher_request) = create_proxy::<fuicomposition::ChildViewWatcherMarker>()
        .expect("failed to create child view watcher channel");
    let viewport_properties = fuicomposition::ViewportProperties {
        logical_size: Some(fmath::SizeU { width: server.image_width, height: server.image_height }),
        ..Default::default()
    };
    let new_viewport = fuicomposition::ContentId { value: server.viewport_id.next() };
    let old_viewport = fuicomposition::ContentId { value: new_viewport.value - 1 };
    server
        .flatland
        .create_viewport(
            &new_viewport,
            viewport_token,
            &viewport_properties,
            child_view_watcher_request,
        )
        .expect("failed to create child viewport");
    server.flatland.set_content(&ROOT_TRANSFORM_ID, &new_viewport).expect("error setting content");

    {
        let mut scene_state = server.scene_state.lock();
        let scene_state = scene_state.deref_mut();
        match scene_state {
            // We are switching from Fb presentation to Viewport. We can clean up resources as this
            // change only happens once and there is no switching back to Fb.
            SceneState::Fb => {
                server.flatland.release_image(&FB_IMAGE_ID).expect("failed to release image");
            }
            SceneState::Viewport => {
                let _ = server
                    .flatland
                    .release_viewport(&old_viewport)
                    .check()
                    .expect("failed to release child viewport");
            }
        }
        *scene_state = SceneState::Viewport;
    }
    server.present();
}

/// Spawns a thread to serve a `ViewProvider` in `outgoing_dir`.
///
/// SAFETY: This function `.expect`'s a lot, because it isn't meant to be used in the long time and
/// most of the failures would be unexpected and unrecoverable.
pub fn spawn_view_provider(
    kernel: &Arc<Kernel>,
    server: Arc<FramebufferServer>,
    view_bound_protocols: fuicomposition::ViewBoundProtocols,
    view_identity: fuiviews::ViewIdentityOnCreation,
    outgoing_dir: fidl::endpoints::ServerEnd<fidl_fuchsia_io::DirectoryMarker>,
) {
    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        let mut view_bound_protocols = Some(view_bound_protocols);
        let mut view_identity = Some(view_identity);
        executor.run_singlethreaded(async move {
            let mut service_fs = ServiceFs::new_local();
            service_fs.dir("svc").add_fidl_service(ExposedProtocols::ViewProvider);
            service_fs.serve_connection(outgoing_dir).expect("");

            while let Some(ExposedProtocols::ViewProvider(mut request_stream)) =
                service_fs.next().await
            {
                while let Ok(Some(event)) = request_stream.try_next().await {
                    match event {
                        fuiapp::ViewProviderRequest::CreateView2 { args, control_handle: _ } => {
                            let view_creation_token = args.view_creation_token.unwrap();
                            // We don't actually care about the parent viewport at the moment, because we don't resize.
                            let (_parent_viewport_watcher, parent_viewport_watcher_request) =
                                create_proxy::<fuicomposition::ParentViewportWatcherMarker>()
                                    .expect("failed to create ParentViewportWatcherProxy");
                            server
                                .flatland
                                .create_view2(
                                     view_creation_token,
                                     view_identity.take().expect("cannot create view because view identity has been consumed"),
                                    view_bound_protocols.take().expect("cannot create view because view bound protocols have been consumed"),
                                    parent_viewport_watcher_request,
                                )
                                .expect("FIDL error");

                            // Now that the view has been created, start presenting.
                            server.present();
                        }
                        r => {
                            log_warn!("Got unexpected view provider request: {:?}", r);
                        }
                    }
                }
            }
        });
    });
}

pub fn send_view_to_graphical_presenter(
    kernel: &Arc<Kernel>,
    server: Arc<FramebufferServer>,
    view_bound_protocols: fuicomposition::ViewBoundProtocols,
    view_identity: fuiviews::ViewIdentityOnCreation,
    incoming_svc_dir: fidl_fuchsia_io::DirectorySynchronousProxy,
) {
    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        let mut view_bound_protocols = Some(view_bound_protocols);
        let mut view_identity = Some(view_identity);
        let mut maybe_view_controller_proxy = None;
        executor.run_singlethreaded(async move {
            let link_token_pair =
                ViewCreationTokenPair::new().expect("failed to create ViewCreationTokenPair");
            // We don't actually care about the parent viewport at the moment, because we don't resize.
            let (_parent_viewport_watcher, parent_viewport_watcher_request) =
                create_proxy::<fuicomposition::ParentViewportWatcherMarker>()
                    .expect("failed to create ParentViewportWatcherProxy");
            server
                .flatland
                .create_view2(
                    link_token_pair.view_creation_token,
                    view_identity
                        .take()
                        .expect("cannot create view because view identity has been consumed"),
                    view_bound_protocols.take().expect(
                        "cannot create view because view bound protocols have been consumed",
                    ),
                    parent_viewport_watcher_request,
                )
                .expect("FIDL error");

            // Now that the view has been created, start presenting.
            server.present();

            let graphical_presenter = connect_to_protocol_at_dir_root::<
                felement::GraphicalPresenterMarker,
            >(&incoming_svc_dir)
            .map_err(|_| errno!(ENOENT))
            .expect("Failed to connect to GraphicalPresenter");

            let (view_controller_proxy, view_controller_server_end) =
                fidl::endpoints::create_proxy::<felement::ViewControllerMarker>()
                    .expect("failed to create ViewControllerProxy");
            let _ = maybe_view_controller_proxy.insert(view_controller_proxy);

            let view_spec = felement::ViewSpec {
                annotations: None,
                viewport_creation_token: Some(link_token_pair.viewport_creation_token),
                ..Default::default()
            };

            // TODO: b/307790211 - Service annotation controller stream.
            let (annotation_controller_client_end, _annotation_controller_stream) =
                create_request_stream::<felement::AnnotationControllerMarker>().unwrap();

            graphical_presenter
                .present_view(
                    view_spec,
                    Some(annotation_controller_client_end),
                    Some(view_controller_server_end),
                )
                .await
                .expect("failed to present view")
                .unwrap_or_else(|e| println!("{:?}", e));
        });
    });
}

/// Starts a flatland presentation loop, using the flatland proxy in `server`.
pub fn start_flatland_presentation_loop(kernel: &Arc<Kernel>, server: Arc<FramebufferServer>) {
    let flatland = server.flatland.clone();
    let mut flatland_event_stream = flatland.take_event_stream();
    let mut presentation_receiver = server.presentation_receiver.lock();
    let mut presentation_receiver = presentation_receiver.deref_mut().take().unwrap();
    kernel.kthreads.spawner().spawn(|_, _| {
        let mut executor = fasync::LocalExecutor::new();
        let scheduler = ThroughputScheduler::new();
        executor.run_singlethreaded(async move {
            let mut scene_state = None;
            loop {
                futures::select! {
                    message = presentation_receiver.next() => {
                        if message.is_some() {
                            scene_state = message;
                            scheduler.request_present();
                        }
                    }
                    flatland_event = flatland_event_stream.next() => {
                        match flatland_event {
                            Some(Ok(fuicomposition::FlatlandEvent::OnNextFrameBegin{ values })) => {
                                let credits = values
                                            .additional_present_credits
                                            .expect("Present credits must exist");
                                let infos = values
                                    .future_presentation_infos
                                    .expect("Future presentation infos must exist")
                                    .iter()
                                    .map(
                                    |x| PresentationInfo{
                                        latch_point: zx::Time::from_nanos(x.latch_point.unwrap()),
                                        presentation_time: zx::Time::from_nanos(
                                                            x.presentation_time.unwrap())
                                    })
                                    .collect();
                                scheduler.on_next_frame_begin(credits, infos);
                                // Keep presenting as long as we are in Fb state.
                                match scene_state {
                                    Some(SceneState::Fb) => {
                                        scheduler.request_present();
                                    }
                                    _ => {}
                                }
                            }
                            Some(Ok(fuicomposition::FlatlandEvent::OnFramePresented{ frame_presented_info })) => {
                                let actual_presentation_time =
                                    zx::Time::from_nanos(frame_presented_info.actual_presentation_time);
                                let presented_infos: Vec<PresentedInfo> =
                                    frame_presented_info.presentation_infos
                                    .into_iter()
                                    .map(|x| x.into())
                                    .collect();
                                scheduler.on_frame_presented(actual_presentation_time, presented_infos);
                            }
                            Some(Ok(fuicomposition::FlatlandEvent::OnError{ error })) => {
                                log_error!(
                                    "Received FlatlandError code: {}; exiting listener loop",
                                    error.into_primitive()
                                );
                                return;
                            }
                            _ => {}
                        }
                    }
                    present_parameters = scheduler.wait_to_update().fuse() => {
                        flatland
                        .present(fuicomposition::PresentArgs {
                            requested_presentation_time: Some(
                                present_parameters.requested_presentation_time.into_nanos(),
                            ),
                            acquire_fences: None,
                            release_fences: None,
                            unsquashable: Some(present_parameters.unsquashable),
                            ..Default::default()
                        })
                        .unwrap_or(());
                    }
                }
            }
        })
    });
}
