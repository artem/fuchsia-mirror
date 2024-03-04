// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The `element_management` library provides utilities for sessions to service
//! incoming [`fidl_fuchsia_element::ManagerRequest`]s.
//!
//! Elements are instantiated as dynamic component instances in a component collection of the
//! calling component.

use {
    crate::annotation::{
        handle_annotation_controller_stream, AnnotationError, AnnotationHolder, WatchResponder,
    },
    crate::element::Element,
    anyhow::{format_err, Error},
    fidl::endpoints::{
        create_proxy, create_request_stream, ClientEnd, ControlHandle, Proxy, RequestStream,
        ServerEnd,
    },
    fidl_connector::Connect,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_element as felement, fidl_fuchsia_io as fio,
    fidl_fuchsia_ui_app as fuiapp,
    fuchsia_async::{self as fasync, DurationExt},
    fuchsia_component,
    fuchsia_fs::directory as ffs_dir,
    fuchsia_scenic as scenic, fuchsia_zircon as zx,
    futures::{lock::Mutex, select, FutureExt, StreamExt, TryStreamExt},
    rand::{
        distributions::{Alphanumeric, DistString},
        thread_rng,
    },
    std::{collections::HashMap, sync::Arc},
    tracing::{error, info, warn},
};

// Timeout duration for a ViewControllerProxy to close, in seconds.
static VIEW_CONTROLLER_DISMISS_TIMEOUT: zx::Duration = zx::Duration::from_seconds(3_i64);

// Annotations in the ElementManager namespace.
static ELEMENT_MANAGER_NS: &'static str = "element_manager";

/// Errors returned by calls to [`ElementManager`].
#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum ElementManagerError {
    /// Returned when the element manager fails to created the component instance associated with
    /// a given element.
    #[error("Element {} not created at \"{}/{}\": {:?}", url, collection, name, err)]
    NotCreated { name: String, collection: String, url: String, err: fcomponent::Error },

    /// Returned when the element manager fails to open the exposed directory
    /// of the component instance associated with a given element.
    #[error("Element {} not bound at \"{}/{}\": {:?}", url, collection, name, err)]
    ExposedDirNotOpened { name: String, collection: String, url: String, err: fcomponent::Error },

    /// Returned when the element manager fails to bind to the component instance associated with
    /// a given element.
    #[error("Element {} not bound at \"{}/{}\": {:?}", url, collection, name, err_str)]
    NotBound { name: String, collection: String, url: String, err_str: String },
}

impl ElementManagerError {
    pub fn not_created(
        name: impl Into<String>,
        collection: impl Into<String>,
        url: impl Into<String>,
        err: impl Into<fcomponent::Error>,
    ) -> ElementManagerError {
        ElementManagerError::NotCreated {
            name: name.into(),
            collection: collection.into(),
            url: url.into(),
            err: err.into(),
        }
    }

    pub fn exposed_dir_not_opened(
        name: impl Into<String>,
        collection: impl Into<String>,
        url: impl Into<String>,
        err: impl Into<fcomponent::Error>,
    ) -> ElementManagerError {
        ElementManagerError::ExposedDirNotOpened {
            name: name.into(),
            collection: collection.into(),
            url: url.into(),
            err: err.into(),
        }
    }

    pub fn not_bound(
        name: impl Into<String>,
        collection: impl Into<String>,
        url: impl Into<String>,
        err_str: impl Into<String>,
    ) -> ElementManagerError {
        ElementManagerError::NotBound {
            name: name.into(),
            collection: collection.into(),
            url: url.into(),
            err_str: err_str.into(),
        }
    }
}

pub type GraphicalPresenterConnector =
    Box<dyn Connect<Proxy = felement::GraphicalPresenterProxy> + Send + Sync>;

/// A mapping from component URL to the name of the collection in which
/// components with that URL should be run.
#[derive(Debug, Clone)]
pub struct CollectionConfig {
    pub url_to_collection: HashMap<String, String>,
    pub default_collection: String,
}

impl CollectionConfig {
    /// For a given component URL, returns the collection in which to run
    /// components with that URL.
    fn for_url(&self, url: &str) -> &str {
        self.url_to_collection.get(url).unwrap_or(&self.default_collection)
    }
}

/// Manages the elements associated with a session.
pub struct ElementManager {
    /// The realm which this element manager uses to create components.
    realm: fcomponent::RealmProxy,

    /// The presenter that will make launched elements visible to the user.
    ///
    /// This is typically provided by the system shell, or other similar configurable component.
    graphical_presenter_connector: Option<GraphicalPresenterConnector>,

    /// Policy that determines the collection in which elements will be launched.
    ///
    /// The component that is running the `ElementManager` must have a collection in its CML file
    /// for each collection listed in `collections_config`.
    collection_config: CollectionConfig,
}

impl ElementManager {
    pub fn new(
        realm: fcomponent::RealmProxy,
        graphical_presenter_connector: Option<GraphicalPresenterConnector>,
        collection_config: CollectionConfig,
    ) -> ElementManager {
        ElementManager { realm, graphical_presenter_connector, collection_config }
    }

    /// Launches a component as an element.
    ///
    /// The component is created as a child in the Element Manager's realm.
    ///
    /// # Parameters
    /// - `child_name`: The name of the element, must be unique within a session. The name must be
    ///                 non-empty, of the form [a-z0-9-_.].
    /// - `child_url`: The component url of the child added to the session.
    ///
    /// # Returns
    /// An Element backed by the component.
    pub async fn launch_element(
        &self,
        child_url: &str,
        child_name: &str,
    ) -> Result<Element, ElementManagerError> {
        let collection = self.collection_config.for_url(child_url);

        info!(
            child_name,
            child_url,
            collection = %collection,
            "launch_v2_element"
        );

        realm_management::create_child_component(
            &child_name,
            &child_url,
            collection,
            fcomponent::CreateChildArgs::default(),
            &self.realm,
        )
        .await
        .map_err(|err: fcomponent::Error| {
            ElementManagerError::not_created(child_name, collection, child_url, err)
        })?;

        let (exposed_directory, exposed_dir_server_end) =
            create_proxy::<fio::DirectoryMarker>().unwrap();
        match realm_management::open_child_component_exposed_dir(
            child_name,
            collection,
            &self.realm,
            exposed_dir_server_end,
        )
        .await
        {
            Ok(exposed_directory) => exposed_directory,
            Err(err) => {
                return Err(ElementManagerError::exposed_dir_not_opened(
                    child_name,
                    collection.to_owned(),
                    child_url,
                    err,
                ))
            }
        };

        // Determine whether or not ViewProvider is exposed.
        let use_view_provider = match ffs_dir::dir_contains(
            &exposed_directory,
            "fuchsia.ui.app.ViewProvider",
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                warn!("could not read directory contents; assuming fuchsia.ui.app.ViewProvider is unavailable: {:?}", e);
                false
            }
        };

        // Connect to fuchsia.component.Binder in order to start the component.
        let _ = fuchsia_component::client::connect_to_protocol_at_dir_root::<
            fcomponent::BinderMarker,
        >(&exposed_directory)
        .map_err(|err| {
            ElementManagerError::not_bound(
                child_name,
                collection.to_owned(),
                child_url,
                err.to_string(),
            )
        })?;

        Ok(Element::from_directory_channel(
            exposed_directory.into_channel().unwrap().into_zx_channel(),
            child_name,
            child_url,
            collection,
            use_view_provider,
        ))
    }

    /// Handles requests to the [`Manager`] protocol.
    ///
    /// # Parameters
    /// `stream`: The stream that receives [`Manager`] requests.
    ///
    /// # Returns
    /// `Ok` if the request stream has been successfully handled once the client closes
    /// its connection. `Error` if a FIDL IO error was encountered.
    pub async fn handle_requests(
        &self,
        mut stream: felement::ManagerRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                felement::ManagerRequest::ProposeElement { spec, controller, responder } => {
                    let result = self.handle_propose_element(spec, controller).await;
                    let _ = responder.send(result);
                }
            }
        }
        Ok(())
    }

    /// Attempts to connect to the element's view provider and if it does
    /// expose the view provider will tell the proxy to present the view.
    async fn present_view_for_element(
        &self,
        element: &mut Element,
        initial_annotations: Vec<felement::Annotation>,
        annotation_controller: Option<ClientEnd<felement::AnnotationControllerMarker>>,
    ) -> Result<felement::ViewControllerProxy, Error> {
        let view_provider = element.connect_to_protocol::<fuiapp::ViewProviderMarker>()?;

        let link_token_pair = scenic::flatland::ViewCreationTokenPair::new()?;

        // Drop the view provider once it has received the view creation token.
        view_provider.create_view2(fuiapp::CreateView2Args {
            view_creation_token: Some(link_token_pair.view_creation_token),
            ..Default::default()
        })?;

        let view_spec = felement::ViewSpec {
            annotations: Some(initial_annotations),
            viewport_creation_token: Some(link_token_pair.viewport_creation_token),
            ..Default::default()
        };

        let (view_controller_proxy, server_end) =
            fidl::endpoints::create_proxy::<felement::ViewControllerMarker>()?;

        if let Some(graphical_presenter_connector) = &self.graphical_presenter_connector {
            graphical_presenter_connector
                .connect()?
                .present_view(view_spec, annotation_controller, Some(server_end))
                .await?
                .map_err(|err| format_err!("Failed to present element: {:?}", err))?;
        }

        Ok(view_controller_proxy)
    }

    async fn handle_propose_element(
        &self,
        spec: felement::Spec,
        element_controller: Option<ServerEnd<felement::ControllerMarker>>,
    ) -> Result<(), felement::ProposeElementError> {
        let component_url = spec.component_url.ok_or_else(|| {
            error!("ProposeElement() failed to launch element: spec.component_url is missing");
            felement::ProposeElementError::InvalidArgs
        })?;

        let url_key = felement::AnnotationKey {
            namespace: ELEMENT_MANAGER_NS.to_string(),
            value: "url".to_string(),
        };

        let mut initial_annotations = match spec.annotations {
            Some(annotations) => annotations,
            None => vec![],
        };

        if !initial_annotations.iter().any(|annotation| annotation.key == url_key) {
            initial_annotations.push(felement::Annotation {
                key: url_key,
                value: felement::AnnotationValue::Text(component_url.to_string()),
            });
        }

        // Create AnnotationHolder and populate the initial annotations from the Spec.
        let mut annotation_holder = AnnotationHolder::new();
        annotation_holder.update_annotations(initial_annotations, vec![]).map_err(|err| {
            error!(?err, "ProposeElement() failed to set initial annotations");
            felement::ProposeElementError::InvalidArgs
        })?;

        // If there is a name specified for the element, use it, otherwise generate a
        // random name.
        let child_name = match annotation_holder.get_annotation(ELEMENT_MANAGER_NS, "name") {
            Some(felement::AnnotationValue::Text(name)) => name.clone(),
            _ => {
                let mut name = Alphanumeric.sample_string(&mut thread_rng(), 16);
                name.make_ascii_lowercase();
                name
            }
        };

        let mut element =
            self.launch_element(&component_url, &child_name).await.map_err(|err| match err {
                ElementManagerError::NotCreated { .. } => felement::ProposeElementError::NotFound,
                err => {
                    error!(?err, "ProposeElement() failed to launch element");
                    felement::ProposeElementError::InvalidArgs
                }
            })?;

        let (annotation_controller_client_end, annotation_controller_stream) =
            create_request_stream::<felement::AnnotationControllerMarker>().unwrap();
        let initial_view_annotations = annotation_holder.get_annotations().unwrap();

        let view_controller_proxy = match element.use_view_provider() {
            true => {
                Some(
                    self.present_view_for_element(
                        &mut element,
                        initial_view_annotations,
                        Some(annotation_controller_client_end),
                    )
                    .await
                    .map_err(|err| {
                        // TODO(https://fxbug.dev/42163510): ProposeElement should propagate GraphicalPresenter errors back to caller
                        error!(?err, "ProposeElement() failed to present element");
                        felement::ProposeElementError::InvalidArgs
                    })?,
                )
            }
            false => {
                // Instead of connecting to fuchsia.ui.app.ViewProvider, wait
                // for the child to call fuchsia.element.GraphicalPresenter.
                None
            }
        };

        let element_controller_stream = match element_controller {
            Some(controller) => match controller.into_stream() {
                Ok(stream) => Ok(Some(stream)),
                Err(_) => Err(felement::ProposeElementError::InvalidArgs),
            },
            None => Ok(None),
        }?;

        fasync::Task::local(run_element_until_closed(
            annotation_holder,
            element_controller_stream,
            annotation_controller_stream,
            view_controller_proxy,
        ))
        .detach();

        Ok(())
    }
}

/// Runs the Element until it receives a signal to shutdown.
///
/// The Element can receive a signal to shut down from any of the
/// following:
///   - Element. The component represented by the element can close on its own.
///   - ControllerRequestStream. The element controller can signal that the element should close.
///   - ViewControllerProxy. The view controller can signal that the element can close.
///
/// The Element will shutdown when any of these signals are received.
///
/// The Element will also listen for any incoming events from the element controller and
/// forward them to the view controller.
async fn run_element_until_closed(
    annotation_holder: AnnotationHolder,
    controller_stream: Option<felement::ControllerRequestStream>,
    annotation_controller_stream: felement::AnnotationControllerRequestStream,
    view_controller_proxy: Option<felement::ViewControllerProxy>,
) {
    let annotation_holder = Arc::new(Mutex::new(annotation_holder));

    // This task will fall out of scope when the select!() below returns.
    let _annotation_task = fasync::Task::spawn(handle_annotation_controller_stream(
        annotation_holder.clone(),
        annotation_controller_stream,
    ));

    if let Some(view_controller_proxy) = view_controller_proxy {
        select!(
            _ = wait_for_view_controller_close(view_controller_proxy.clone()).fuse() =>  {
                // signals that the presenter would like to close the element.
                // We do not need to do anything here but exit which will cause
                // the element to be dropped and will kill the component.
            },
            _ = handle_element_controller_stream(annotation_holder.clone(), controller_stream).fuse() => {
                // the proposer has decided they want to shut down the element.

                // We want to allow the presenter the ability to dismiss
                // the view so we tell it to dismiss and then wait for
                // the view controller stream to close.
                let _ = view_controller_proxy.dismiss();
                let timeout = fuchsia_async::Timer::new(VIEW_CONTROLLER_DISMISS_TIMEOUT.after_now());
                wait_for_view_controller_close_or_timeout(view_controller_proxy, timeout).await;
            },
        );
    }
}

/// Waits for this view controller to close.
async fn wait_for_view_controller_close(proxy: felement::ViewControllerProxy) {
    let stream = proxy.take_event_stream();
    let _ = stream.collect::<Vec<_>>().await;
}

/// Waits for this view controller to close.
async fn wait_for_view_controller_close_or_timeout(
    proxy: felement::ViewControllerProxy,
    timeout: fasync::Timer,
) {
    let _ = futures::future::select(timeout, Box::pin(wait_for_view_controller_close(proxy))).await;
}

/// Handles element Controller protocol requests.
///
/// If the `ControllerRequestStream` is None then this future will never resolve.
///
/// # Parameters
/// - `annotation_holder`: The [`AnnotationHolder`] for the controlled element.
/// - `stream`: The stream of [`Controller`] requests.
async fn handle_element_controller_stream(
    annotation_holder: Arc<Mutex<AnnotationHolder>>,
    stream: Option<felement::ControllerRequestStream>,
) {
    // TODO(https://fxbug.dev/42163991): Unify this with handle_annotation_controller_stream(), once FIDL
    // provides a mechanism to do so.
    if let Some(mut stream) = stream {
        let mut watch_subscriber = annotation_holder.lock().await.new_watch_subscriber();
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                felement::ControllerRequest::UpdateAnnotations {
                    annotations_to_set,
                    annotations_to_delete,
                    responder,
                } => {
                    let result = annotation_holder
                        .lock()
                        .await
                        .update_annotations(annotations_to_set, annotations_to_delete);
                    match result {
                        Ok(()) => responder.send(Ok(())),
                        Err(AnnotationError::Update(e)) => responder.send(Err(e)),
                        Err(_) => unreachable!(),
                    }
                    .ok();
                }
                felement::ControllerRequest::GetAnnotations { responder } => {
                    let result = annotation_holder.lock().await.get_annotations();
                    match result {
                        Ok(annotation_vec) => responder.send(Ok(annotation_vec)),
                        Err(AnnotationError::Get(e)) => responder.send(Err(e)),
                        Err(_) => unreachable!(),
                    }
                    .ok();
                }
                felement::ControllerRequest::WatchAnnotations { responder } => {
                    if let Err(e) = watch_subscriber
                        .watch_annotations(WatchResponder::ElementController(responder))
                    {
                        // There is already a `WatchAnnotations` request pending for the client. Since the responder gets dropped (TODO(https://fxbug.dev/42176516)), the connection will be closed to indicate unexpected client behavior.
                        error!("ControllerRequest error: {}. Dropping connection", e);
                        stream.control_handle().shutdown_with_epitaph(zx::Status::BAD_STATE);
                        return;
                    }
                }
            }
        }
    } else {
        // If the element controller is None then we never exit and rely
        // on the other futures to signal the end of the element.
        futures::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{CollectionConfig, ElementManager, ElementManagerError},
        fidl::{endpoints::spawn_stream_handler, prelude::*},
        fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_zircon as zx,
        futures::{channel::mpsc::channel, prelude::*},
        maplit::hashmap,
        session_testing::spawn_directory_server,
    };

    fn example_collection_config() -> CollectionConfig {
        CollectionConfig {
            url_to_collection: hashmap! {
                "https://special_component.cm".to_string() => "special_collection".to_string(),
            },
            default_collection: "elements".to_string(),
        }
    }

    /// Tests that launching a *.cm element successfully returns an Element with
    /// outgoing directory routing appropriate for v2 components.
    #[fuchsia::test]
    async fn launch_v2_element_success() {
        let component_url = "fuchsia-pkg://fuchsia.com/simple_element#meta/simple_element.cm";
        let child_name = "child";

        let (directory_open_sender, directory_open_receiver) = channel::<String>(1);
        let directory_request_handler = move |directory_request| match directory_request {
            fio::DirectoryRequest::Open { path: capability_path, .. } => {
                let mut result_sender = directory_open_sender.clone();
                fasync::Task::spawn(async move {
                    let _ = result_sender.send(capability_path).await;
                })
                .detach()
            }
            fio::DirectoryRequest::ReadDirents { responder, .. } => {
                responder.send(zx::sys::ZX_OK, &[]).expect("Failed to service ReadDirents");
            }
            fio::DirectoryRequest::Rewind { responder } => {
                responder.send(zx::sys::ZX_OK).expect("Failed to service Rewind");
            }
            req => panic!("Directory handler received an unexpected request: {:?}", req),
        };

        let realm = spawn_stream_handler(move |realm_request| {
            let directory_request_handler = directory_request_handler.clone();
            async move {
                match realm_request {
                    fcomponent::RealmRequest::CreateChild {
                        collection,
                        decl,
                        args: _,
                        responder,
                    } => {
                        assert_eq!(decl.url.unwrap(), component_url);
                        assert_eq!(decl.name.unwrap(), child_name);
                        assert_eq!(&collection.name, "elements");

                        let _ = responder.send(Ok(()));
                    }
                    fcomponent::RealmRequest::OpenExposedDir { child, exposed_dir, responder } => {
                        assert_eq!(child.collection, Some("elements".to_string()));
                        spawn_directory_server(exposed_dir, directory_request_handler);
                        let _ = responder.send(Ok(()));
                    }
                    _ => panic!("Realm handler received an unexpected request"),
                }
            }
        })
        .unwrap();

        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let result = element_manager.launch_element(component_url, child_name).await;
        let element = result.unwrap();

        // Now use the element api to open a service in the element's outgoing dir. Verify
        // that the directory channel received the request with the correct path.
        let (_client_channel, server_channel) = zx::Channel::create();
        let _ = element.connect_to_named_protocol_with_channel("myProtocol", server_channel);
        let open_paths = directory_open_receiver.take(2).collect::<Vec<_>>().await;
        assert_eq!(vec![fcomponent::BinderMarker::DEBUG_NAME, "myProtocol"], open_paths);
    }

    #[fuchsia::test]
    async fn launch_v2_element_in_special_collection_success() {
        let component_url = "https://special_component.cm";
        let child_name = "child";

        let (directory_open_sender, directory_open_receiver) = channel::<String>(1);
        let directory_request_handler = move |directory_request| match directory_request {
            fio::DirectoryRequest::Open { path: capability_path, .. } => {
                let mut result_sender = directory_open_sender.clone();
                fasync::Task::spawn(async move {
                    let _ = result_sender.send(capability_path).await;
                })
                .detach()
            }
            fio::DirectoryRequest::ReadDirents { responder, .. } => {
                responder.send(zx::sys::ZX_OK, &[]).expect("Failed to service ReadDirents");
            }
            fio::DirectoryRequest::Rewind { responder } => {
                responder.send(zx::sys::ZX_OK).expect("Failed to service Rewind");
            }
            _ => panic!("Directory handler received an unexpected request"),
        };

        let realm = spawn_stream_handler(move |realm_request| {
            let directory_request_handler = directory_request_handler.clone();
            async move {
                match realm_request {
                    fcomponent::RealmRequest::CreateChild {
                        collection,
                        decl,
                        args: _,
                        responder,
                    } => {
                        assert_eq!(decl.url.unwrap(), component_url);
                        assert_eq!(decl.name.unwrap(), child_name);
                        assert_eq!(&collection.name, "special_collection");

                        let _ = responder.send(Ok(()));
                    }
                    fcomponent::RealmRequest::OpenExposedDir { child, exposed_dir, responder } => {
                        assert_eq!(child.collection, Some("special_collection".to_string()));
                        spawn_directory_server(exposed_dir, directory_request_handler);
                        let _ = responder.send(Ok(()));
                    }
                    _ => panic!("Realm handler received an unexpected request"),
                }
            }
        })
        .unwrap();

        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let result = element_manager.launch_element(component_url, child_name).await;
        let element = result.unwrap();

        // Now use the element api to open a service in the element's outgoing dir. Verify
        // that the directory channel received the request with the correct path.
        let (_client_channel, server_channel) = zx::Channel::create();
        let _ = element.connect_to_named_protocol_with_channel("myProtocol", server_channel);
        let open_paths = directory_open_receiver.take(2).collect::<Vec<_>>().await;
        assert_eq!(vec![fcomponent::BinderMarker::DEBUG_NAME, "myProtocol"], open_paths);
    }

    /// Tests that launching an element which is not successfully created in the realm returns an
    /// appropriate error.
    #[fuchsia::test]
    async fn launch_element_create_error_internal() {
        let component_url = "fuchsia-pkg://fuchsia.com/simple_element#meta/simple_element.cm";

        // The following match errors if it sees a bind request: since the child was not created
        // successfully the bind should not be called.
        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl: _,
                    args: _,
                    responder,
                } => {
                    let _ = responder.send(Err(fcomponent::Error::Internal));
                }
                _ => panic!("Realm handler received an unexpected request"),
            }
        })
        .unwrap();
        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let result = element_manager.launch_element(component_url, "").await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap(),
            ElementManagerError::not_created(
                "",
                "elements",
                component_url,
                fcomponent::Error::Internal
            )
        );
    }

    /// Tests that adding an element which is not successfully created in the realm returns an
    /// appropriate error.
    #[fuchsia::test]
    async fn launch_element_create_error_no_space() {
        let component_url = "fuchsia-pkg://fuchsia.com/simple_element#meta/simple_element.cm";

        // The following match errors if it sees a bind request: since the child was not created
        // successfully the bind should not be called.
        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl: _,
                    args: _,
                    responder,
                } => {
                    let _ = responder.send(Err(fcomponent::Error::ResourceUnavailable));
                }
                _ => panic!("Realm handler received an unexpected request"),
            }
        })
        .unwrap();
        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let result = element_manager.launch_element(component_url, "").await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap(),
            ElementManagerError::not_created(
                "",
                "elements",
                component_url,
                fcomponent::Error::ResourceUnavailable
            )
        );
    }

    /// Tests that adding an element which can't have its exposed directory opened
    /// returns an appropriate error.
    #[fuchsia::test]
    async fn open_exposed_dir_error() {
        let component_url = "fuchsia-pkg://fuchsia.com/simple_element#meta/simple_element.cm";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl: _,
                    args: _,
                    responder,
                } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir {
                    child: _,
                    exposed_dir: _,
                    responder,
                } => {
                    let _ = responder.send(Err(fcomponent::Error::InstanceCannotResolve));
                }
                _ => panic!("Realm handler received an unexpected request"),
            }
        })
        .unwrap();

        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let result = element_manager.launch_element(component_url, "").await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap(),
            ElementManagerError::exposed_dir_not_opened(
                "",
                "elements",
                component_url,
                fcomponent::Error::InstanceCannotResolve,
            )
        );
    }

    /// Tests that adding an element which is not successfully bound in the
    /// realm results in observing PEER_CLOSED on the exposed directory.
    #[fuchsia::test]
    async fn launch_element_bind_error() {
        let component_url = "fuchsia-pkg://fuchsia.com/simple_element#meta/simple_element.cm";

        let realm = spawn_stream_handler(move |realm_request| async move {
            match realm_request {
                fcomponent::RealmRequest::CreateChild {
                    collection: _,
                    decl: _,
                    args: _,
                    responder,
                } => {
                    let _ = responder.send(Ok(()));
                }
                fcomponent::RealmRequest::OpenExposedDir { child: _, exposed_dir, responder } => {
                    // Close the incoming channel before responding to avoid race conditions.
                    let () = std::mem::drop(exposed_dir);
                    let () = responder.send(Ok(())).unwrap();
                }
                _ => panic!("Realm handler received an unexpected request"),
            }
        })
        .unwrap();
        let element_manager = ElementManager::new(realm, None, example_collection_config());

        let element = element_manager.launch_element(component_url, "").await.unwrap();

        assert_eq!(element.use_view_provider(), false);
        let exposed_dir = element.directory_channel();
        exposed_dir
            .wait_handle(zx::Signals::CHANNEL_PEER_CLOSED, zx::Time::INFINITE_PAST)
            .expect("exposed_dir should be closed");
    }
}
