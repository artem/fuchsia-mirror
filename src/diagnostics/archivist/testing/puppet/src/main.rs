// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This program serves the fuchsia.archivist.test.Puppet protocol.
//
// It is meant to be controlled by a test suite and will emit log messages
// and inspect data as requested. This output can be retrieved from the
// archivist under test using fuchsia.diagnostics.ArchiveAccessor.
//
// For full documentation, see //src/diagnostics/archivist/testing/realm-factory/README.md

use anyhow::{Context, Error};
use diagnostics_hierarchy::Property;
use diagnostics_log::{OnInterestChanged, PublishOptions, Publisher};
use fidl::endpoints::create_request_stream;
use fidl_fuchsia_archivist_test as fpuppet;
use fidl_fuchsia_diagnostics::Severity;
use fidl_table_validation::ValidFidlTable;
use fuchsia_async::{TaskGroup, Timer};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter, Inspector};
use fuchsia_zircon::Duration;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    lock::Mutex,
    FutureExt, StreamExt, TryStreamExt,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// `logging = false` allows us to set the global default trace dispatcher
// ourselves. This can only be done once and is usually handled by fuchsia::main.
#[fuchsia::main(logging = false)]
async fn main() -> Result<(), Error> {
    let (sender, receiver) = unbounded::<InterestChangedEvent>();
    subscribe_to_log_interest_changes(InterestChangedNotifier(sender))?;

    // All connections share the same puppet_server instance.
    let puppet_server = Arc::new(PuppetServer::new(receiver));

    let mut fs = ServiceFs::new();
    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    fs.dir("svc").add_fidl_service(|stream: fpuppet::PuppetRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, |stream| async {
        serve_puppet(puppet_server.clone(), stream).await;
    })
    .await;
    Ok(())
}

fn subscribe_to_log_interest_changes(notifier: InterestChangedNotifier) -> Result<(), Error> {
    // It's unfortunate we can't just create the publisher directly and register
    // the interest listener without the roundtrip through tracing::dispatcher.
    // We must call diagnostics_log::initialize because it initializes global
    // state through private function calls.
    diagnostics_log::initialize(PublishOptions::default().wait_for_initial_interest(false))
        .expect("initialized tracing");
    tracing::dispatcher::get_default(|dispatcher| {
        let publisher: &Publisher = dispatcher.downcast_ref().unwrap();
        publisher.set_interest_listener(notifier.clone());
    });
    Ok(())
}

#[derive(Clone)]
struct InterestChangedEvent {
    severity: Severity,
}

struct PuppetServer {
    // A stream of noifications about interest changed events.
    interest_changed: Mutex<UnboundedReceiver<InterestChangedEvent>>,
    // Tasks waiting to be notified of interest changed events.
    interest_waiters: Mutex<TaskGroup>,
}

impl PuppetServer {
    fn new(receiver: UnboundedReceiver<InterestChangedEvent>) -> Self {
        Self {
            interest_changed: Mutex::new(receiver),
            interest_waiters: Mutex::new(TaskGroup::new()),
        }
    }
}

// Notifies the puppet when log interest changes.
// Together, `PuppetServer` and `InterestChangeNotifier` must gaurantee delivery
// of all interest change notifications to clients (test cases) regardless of
// whether a test case begins waiting for the interest change notification
// before or after it is received by this component. Failure to deliver will
// cause the test case to hang.
#[derive(Clone)]
struct InterestChangedNotifier(UnboundedSender<InterestChangedEvent>);

impl OnInterestChanged for InterestChangedNotifier {
    fn on_changed(&self, severity: &Severity) {
        let sender = self.0.clone();
        // Panic on failure since undelivered notifications may hang clients.
        sender.unbounded_send(InterestChangedEvent { severity: *severity }).unwrap();
    }
}

async fn serve_puppet(server: Arc<PuppetServer>, mut stream: fpuppet::PuppetRequestStream) {
    while let Ok(Some(request)) = stream.try_next().await {
        handle_puppet_request(server.clone(), request)
            .await
            .unwrap_or_else(|e| error!(?e, "handle_puppet_request"));
    }
}

async fn handle_puppet_request(
    server: Arc<PuppetServer>,
    request: fpuppet::PuppetRequest,
) -> Result<(), Error> {
    match request {
        fpuppet::PuppetRequest::EmitExampleInspectData { rows, columns, .. } => {
            inspect_testing::emit_example_inspect_data(inspect_testing::Options {
                rows: rows as usize,
                columns: columns as usize,
                extra_number: None,
            })
            .await?;
            Ok(())
        }
        fpuppet::PuppetRequest::RecordLazyValues { key, responder } => {
            let (client, requests) = create_request_stream()?;
            responder.send(client)?;
            record_lazy_values(key, requests).await?;
            Ok(())
        }
        fpuppet::PuppetRequest::RecordString { key, value, .. } => {
            component::inspector().root().record_string(key, value);
            Ok(())
        }
        fpuppet::PuppetRequest::RecordInt { key, value, .. } => {
            component::inspector().root().record_int(key, value);
            Ok(())
        }
        fpuppet::PuppetRequest::SetHealthOk { responder } => {
            component::health().set_ok();
            responder.send()?;
            Ok(())
        }
        fpuppet::PuppetRequest::Println { message, .. } => {
            println!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::Eprintln { message, .. } => {
            eprintln!("{message}");
            Ok(())
        }
        fpuppet::PuppetRequest::Log { payload, .. } => {
            let request = LogRequest::try_from(payload).context("Log")?;
            let LogRequest { message, severity, .. } = request;

            match severity {
                Severity::Debug => debug!("{message}"),
                Severity::Error => error!("{message}"),
                Severity::Info => info!("{message}"),
                Severity::Warn => warn!("{message}"),
                _ => unimplemented!("Logging with severity: {severity:?}"),
            }

            Ok(())
        }
        fpuppet::PuppetRequest::WaitForInterestChange { responder } => {
            let mut task_group = server.interest_waiters.lock().await;
            let server = server.clone();
            task_group.spawn(async move {
                let event = server.interest_changed.lock().await.next().await.unwrap();
                let response = &fpuppet::LogPuppetWaitForInterestChangeResponse {
                    severity: Some(event.severity),
                    ..Default::default()
                };
                responder.send(response).unwrap();
            });
            Ok(())
        }
        fpuppet::PuppetRequest::_UnknownMethod { .. } => unreachable!(),
    }
}

#[derive(Debug, Clone, ValidFidlTable)]
#[fidl_table_src(fpuppet::LogPuppetLogRequest)]
pub struct LogRequest {
    pub message: String,
    pub severity: Severity,
}

// Converts InspectPuppet requests into callbacks that report inspect values lazily.
// The values aren't truly lazy since they're computed in the client before the inspect
// data is fetched. They're just lazily reported.
async fn record_lazy_values(
    key: String,
    mut stream: fpuppet::LazyInspectPuppetRequestStream,
) -> Result<(), Error> {
    let mut properties = vec![];
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fpuppet::LazyInspectPuppetRequest::RecordString { key, value, .. } => {
                properties.push(Property::String(key, value));
            }
            fpuppet::LazyInspectPuppetRequest::RecordInt { key, value, .. } => {
                properties.push(Property::Int(key, value));
            }
            fpuppet::LazyInspectPuppetRequest::Commit { options, .. } => {
                component::inspector().root().record_lazy_values(key, move || {
                    let properties = properties.clone();
                    async move {
                        if options.hang.unwrap_or_default() {
                            Timer::new(Duration::from_minutes(60)).await;
                        }
                        let inspector = Inspector::default();
                        let node = inspector.root();
                        for property in properties.iter() {
                            match property {
                                Property::String(k, v) => node.record_string(k, v),
                                Property::Int(k, v) => node.record_int(k, *v),
                                _ => unimplemented!(),
                            }
                        }
                        Ok(inspector)
                    }
                    .boxed()
                });
                return Ok(()); // drop the connection.
            }
            fpuppet::LazyInspectPuppetRequest::_UnknownMethod { .. } => unreachable!(),
            _ => unimplemented!(),
        };
    }

    Ok(())
}
