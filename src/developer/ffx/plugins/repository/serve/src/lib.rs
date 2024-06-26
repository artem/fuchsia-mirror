// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context as _},
    async_trait::async_trait,
    camino::{Utf8Path, Utf8PathBuf},
    errors::ffx_bail,
    ffx_config::{environment::EnvironmentKind, EnvironmentContext},
    ffx_repository_serve_args::ServeCommand,
    ffx_target::{get_target_specifier, knock_target_by_name},
    fho::{daemon_protocol, AvailabilityFlag, FfxMain, FfxTool, Result, SimpleWriter},
    fidl_fuchsia_developer_ffx::{
        RepositoryStorageType, RepositoryTarget as FfxCliRepositoryTarget, TargetCollectionProxy,
        TargetCollectionReaderMarker, TargetCollectionReaderRequest, TargetInfo,
        TargetQuery as FfxTargetQuery,
    },
    fidl_fuchsia_developer_ffx_ext::{
        RepositoryRegistrationAliasConflictMode as FfxRepositoryRegistrationAliasConflictMode,
        RepositoryTarget as FfxDaemonRepositoryTarget,
    },
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fidl_fuchsia_pkg::RepositoryManagerMarker,
    fidl_fuchsia_pkg_rewrite::EngineMarker,
    fuchsia_async as fasync,
    fuchsia_repo::{
        manager::RepositoryManager,
        repo_client::RepoClient,
        repository::{PmRepository, RepoProvider},
        server::RepositoryServer,
    },
    futures::{executor::block_on, SinkExt, StreamExt, TryStreamExt as _},
    pkg::repo::register_target_with_fidl_proxies,
    signal_hook::{
        consts::signal::{SIGHUP, SIGINT, SIGTERM},
        iterator::Signals,
    },
    std::{fs, io::Write, sync::Arc, time::Duration},
    timeout::timeout,
    tuf::metadata::RawSignedMetadata,
};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const SERVE_KNOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const REPO_BACKGROUND_FEATURE_FLAG: &str = "repository.server.enabled";
const REPO_FOREGROUND_FEATURE_FLAG: &str = "repository.foreground.enabled";
const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
const ENGINE_MONIKER: &str = "/core/pkg-resolver";
const DEFAULT_REPO_NAME: &str = "devhost";
const REPO_PATH_RELATIVE_TO_BUILD_DIR: &str = "amber-files";

#[derive(FfxTool)]
#[check(AvailabilityFlag(REPO_FOREGROUND_FEATURE_FLAG))]
pub struct ServeTool {
    #[command]
    cmd: ServeCommand,
    context: EnvironmentContext,
    #[with(daemon_protocol())]
    target_collection_proxy: TargetCollectionProxy,
}

fho::embedded_plugin!(ServeTool);

async fn get_target_infos(
    target_collection_proxy: &TargetCollectionProxy,
    query: &FfxTargetQuery,
) -> Result<Vec<TargetInfo>, anyhow::Error> {
    let (reader, server) = fidl::endpoints::create_endpoints::<TargetCollectionReaderMarker>();
    target_collection_proxy.list_targets(query, reader)?;

    let mut targets = Vec::new();
    let mut stream = server.into_stream()?;

    while let Some(TargetCollectionReaderRequest::Next { entry, responder }) =
        stream.try_next().await?
    {
        responder.send()?;
        if entry.len() > 0 {
            targets.extend(entry);
        } else {
            break;
        }
    }

    Ok(targets)
}

#[tracing::instrument(skip(conn_quit_tx, server_quit_tx))]
fn start_signal_monitoring(
    mut conn_quit_tx: futures::channel::mpsc::Sender<()>,
    mut server_quit_tx: futures::channel::mpsc::Sender<()>,
) {
    tracing::debug!("Starting monitoring for SIGHUP, SIGINT, SIGTERM");
    let mut signals = Signals::new(&[SIGHUP, SIGINT, SIGTERM]).unwrap();
    // Can't use async here, as signals.forever() is blocking.
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            match signal {
                SIGINT | SIGHUP | SIGTERM => {
                    tracing::info!("Received signal {signal}, quitting");
                    let _ = block_on(conn_quit_tx.send(())).ok();
                    let _ = block_on(server_quit_tx.send(())).ok();
                }
                _ => unreachable!(),
            }
        }
    });
}

async fn connect_to_rcs(
    target_collection_proxy: &TargetCollectionProxy,
    query: &FfxTargetQuery,
) -> Result<RemoteControlProxy, anyhow::Error> {
    let (target_proxy, target_server_end) = fidl::endpoints::create_proxy()?;
    let (rcs_proxy, rcs_server_end) = fidl::endpoints::create_proxy()?;

    target_collection_proxy
        .open_target(query, target_server_end)
        .await?
        .map_err(|e| anyhow!("Failed to open target: {:?}", e))?;
    target_proxy
        .open_remote_control(rcs_server_end)
        .await?
        .map_err(|e| anyhow!("Failed to open remote control: {:?}", e))?;
    Ok(rcs_proxy)
}

async fn connect_to_target(
    target_spec: Option<String>,
    aliases: Option<Vec<String>>,
    storage_type: Option<RepositoryStorageType>,
    repo_server_listen_addr: std::net::SocketAddr,
    repo_manager: Arc<RepositoryManager>,
    target_collection_proxy: &TargetCollectionProxy,
    alias_conflict_mode: FfxRepositoryRegistrationAliasConflictMode,
) -> Result<Option<String>, anyhow::Error> {
    let target_query = FfxTargetQuery { string_matcher: target_spec.clone(), ..Default::default() };

    let rcs_proxy = connect_to_rcs(target_collection_proxy, &target_query).await?;

    let tv = get_target_infos(target_collection_proxy, &target_query).await?;

    let target = if tv.len() != 1 {
        return Err(anyhow!(
            "Exactly one target is expected in the target collection, found {}, nodenames: {}",
            tv.len(),
            tv.into_iter().filter_map(|e| e.nodename.clone()).collect::<Vec<_>>().join(",")
        ));
    } else {
        &tv[0]
    };

    let repo_proxy = rcs::connect_to_protocol::<RepositoryManagerMarker>(
        TIMEOUT,
        REPOSITORY_MANAGER_MONIKER,
        &rcs_proxy,
    )
    .await
    .with_context(|| format!("connecting to repository manager on {:?}", target_query))?;

    let engine_proxy =
        rcs::connect_to_protocol::<EngineMarker>(TIMEOUT, ENGINE_MONIKER, &rcs_proxy)
            .await
            .with_context(|| format!("binding engine to stream on {:?}", target_query))?;

    for (repo_name, repo) in repo_manager.repositories() {
        let repo_target = FfxCliRepositoryTarget {
            repo_name: Some(repo_name),
            target_identifier: target_spec.clone(),
            aliases: aliases.clone(),
            storage_type,
            ..Default::default()
        };

        // Construct RepositoryTarget from same args as `ffx target repository register`
        let repo_target_info = FfxDaemonRepositoryTarget::try_from(repo_target)
            .map_err(|e| anyhow!("Failed to build RepositoryTarget: {:?}", e))?;

        register_target_with_fidl_proxies(
            repo_proxy.clone(),
            engine_proxy.clone(),
            &repo_target_info,
            &target,
            repo_server_listen_addr,
            &repo,
            alias_conflict_mode.clone(),
        )
        .await
        .map_err(|e| anyhow!("Failed to register repository: {:?}", e))?;
    }
    Ok(target.nodename.clone())
}

// Constructs a repo client with an explicitly passed trusted
// root, or defaults to the trusted root of the repository if
// none is provided.
async fn repo_client_from_optional_trusted_root(
    trusted_root: Option<Utf8PathBuf>,
    repository: impl RepoProvider + 'static,
) -> Result<RepoClient<Box<dyn RepoProvider>>, anyhow::Error> {
    let repo_client = if let Some(ref trusted_root_path) = trusted_root {
        let buf = async_fs::read(&trusted_root_path)
            .await
            .with_context(|| format!("reading trusted root {trusted_root_path}"))?;

        let trusted_root = RawSignedMetadata::new(buf);

        RepoClient::from_trusted_root(&trusted_root, Box::new(repository) as Box<_>)
            .await
            .with_context(|| {
                format!("Creating repo client using trusted root {trusted_root_path}")
            })?
    } else {
        RepoClient::from_trusted_remote(Box::new(repository) as Box<_>)
            .await
            .with_context(|| format!("Creating repo client using default trusted root"))?
    };
    Ok(repo_client)
}

#[async_trait(?Send)]
impl FfxMain for ServeTool {
    type Writer = SimpleWriter;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        serve_impl(self.target_collection_proxy, self.cmd, self.context, writer).await?;
        Ok(())
    }
}

async fn serve_impl<W: Write + 'static>(
    target_collection_proxy: TargetCollectionProxy,
    cmd: ServeCommand,
    context: EnvironmentContext,
    mut writer: W,
) -> Result<()> {
    let bg: bool =
        context.get(REPO_BACKGROUND_FEATURE_FLAG).context("checking for background server flag")?;
    if bg {
        ffx_bail!(
            r#"The ffx setting '{}' and the foreground server '{}' are mutually incompatible.
Please disable background serving by running the following commands:
$ ffx config remove repository.server.enabled
$ ffx doctor --restart-daemon"#,
            REPO_BACKGROUND_FEATURE_FLAG,
            REPO_FOREGROUND_FEATURE_FLAG,
        );
    }

    let repo_manager: Arc<RepositoryManager> = RepositoryManager::new();

    let repo_path = match (cmd.repo_path, cmd.product_bundle) {
        (Some(_), Some(_)) => {
            ffx_bail!("Cannot specify both --repo-path and --product-bundle");
        }
        (None, Some(product_bundle)) => {
            if cmd.repository.is_some() {
                ffx_bail!("--repository is not supported with --product-bundle");
            }
            let repositories = sdk_metadata::get_repositories(product_bundle.clone())
                .with_context(|| {
                    format!("getting repositories from product bundle {product_bundle}")
                })?;
            for repository in repositories {
                let repo_name = repository.aliases().first().unwrap().clone();

                let repo_client = RepoClient::from_trusted_remote(Box::new(repository) as Box<_>)
                    .await
                    .with_context(|| format!("Creating a repo client for {repo_name}"))?;
                repo_manager.add(repo_name, repo_client);
            }
            product_bundle
        }
        (repo_path, None) => {
            let repo_path = if let Some(repo_path) = repo_path {
                repo_path
            } else if let EnvironmentKind::InTree { build_dir: Some(build_dir), .. } =
                context.env_kind()
            {
                let build_dir = Utf8Path::from_path(build_dir)
                    .with_context(|| format!("converting repo path to UTF-8 {:?}", repo_path))?;

                build_dir.join(REPO_PATH_RELATIVE_TO_BUILD_DIR)
            } else {
                ffx_bail!("Either --repo-path or --product-bundle need to be specified");
            };

            // Create PmRepository and RepoClient
            let repo_path = repo_path
                .canonicalize_utf8()
                .with_context(|| format!("canonicalizing repo path {:?}", repo_path))?;
            let repository = PmRepository::new(repo_path.clone());

            let mut repo_client =
                repo_client_from_optional_trusted_root(cmd.trusted_root.clone(), repository)
                    .await?;

            repo_client.update().await.context("updating the repository metadata")?;

            let repo_name = cmd.repository.unwrap_or_else(|| DEFAULT_REPO_NAME.to_string());
            repo_manager.add(repo_name, repo_client);
            repo_path
        }
    };

    // Serve RepositoryManager over a RepositoryServer
    let (server_fut, _, server) = RepositoryServer::builder(cmd.address, Arc::clone(&repo_manager))
        .start()
        .await
        .with_context(|| format!("starting repository server"))?;

    // Write port file if needed
    if let Some(port_path) = cmd.port_path {
        let port = server.local_addr().port().to_string();

        fs::write(port_path, port.clone())
            .with_context(|| format!("creating port file for port {}", port))?;
    };

    let server_addr = server.local_addr().clone();

    let server_task = fasync::Task::local(server_fut);
    let (server_stop_tx, mut server_stop_rx) = futures::channel::mpsc::channel::<()>(1);
    let (loop_stop_tx, mut loop_stop_rx) = futures::channel::mpsc::channel::<()>(1);

    // Register signal handler.
    start_signal_monitoring(loop_stop_tx.clone(), server_stop_tx.clone());

    if !cmd.no_device {
        // Resolving the default target is typically fast
        // or does not succeed, in which case we return
        tracing::info!("Getting target specifier");
        let target_spec = timeout(Duration::from_secs(1), get_target_specifier(&context))
            .await
            .context("getting target specifier")??;

        let mut server_stop_tx = server_stop_tx.clone();
        let _conn_loop_task = fasync::Task::local(async move {
               'conn: loop {
                    // This blocks until the default target becomes available
                    // if a connection is possible.
                    match target_spec {
                        Some(ref s) => tracing::info!("Attempting connection to target: {s}"),
                        None => tracing::info!("Attempting connection to first target if unambiguously detectable"),
                    }
                    let connection = connect_to_target(
                        target_spec.clone(),
                        Some(cmd.alias.clone()),
                        cmd.storage_type,
                        server_addr,
                        Arc::clone(&repo_manager),
                        &target_collection_proxy,
                        cmd.alias_conflict_mode.into(),
                    )
                    .await;

                    match connection {
                        Ok(target) => {
                            let s = match target {
                                Some(t) => format!(
                                    "Serving repository '{repo_path}' to target '{t}' over address '{}'.",
                                    server_addr),
                                None => format!(
                                    "Serving repository '{repo_path}' over address '{}'.",
                                    server_addr),
                            };
                            if let Err(e) = writeln!(writer, "{}", s) {
                                tracing::error!("Failed to write to output: {:?}", e);
                            }
                            tracing::info!("{}", s);
                            loop {
                                fuchsia_async::Timer::new(std::time::Duration::from_secs(10)).await;
                                // If serving is supposed to shut down and this task has not been
                                // cancelled already, exit voluntarily.
                                if let Ok(Some(_)) = loop_stop_rx.try_next() {
                                    break 'conn;
                                }
                                if let Err(e) = knock_target_by_name(
                                    &target_spec.clone(),
                                    &target_collection_proxy,
                                    SERVE_KNOCK_TIMEOUT,
                                    SERVE_KNOCK_TIMEOUT,
                                )
                                .await
                                {
                                    tracing::warn!("Connection to target lost, retrying. Error: {}", e);
                                    break;
                                };
                            }
                        }
                        Err(e) => {
                            // If we end up here, it is unlikely a reconnect will be successful without
                            // ffx being restarted.
                            tracing::error!("Cannot serve repository to target, exiting. Error: {}", e);
                            let _: Result<(), _> = server_stop_tx.send(()).await;
                            break;
                        }
                    };
                }
            }).detach();
    } else {
        let s = format!("Serving repository '{repo_path}' over address '{}'.", server_addr);
        writeln!(writer, "{}", s).map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;
        tracing::info!("{}", s);
    }

    let _ = fasync::Task::local(async move {
        if let Some(_) = server_stop_rx.next().await {
            server.stop();
        }
    })
    .detach();

    // Wait for the server to shut down.
    server_task.await;
    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
mod test {
    use {
        super::*,
        assert_matches::assert_matches,
        ffx_config::{keys::TARGET_DEFAULT_KEY, ConfigLevel, TestEnv},
        fho::macro_deps::ffx_writer::TestBuffer,
        fidl::endpoints::DiscoverableProtocolMarker,
        fidl_fuchsia_developer_ffx::{
            RemoteControlState, RepositoryRegistrationAliasConflictMode, SshHostAddrInfo,
            TargetAddrInfo, TargetCollectionRequest, TargetIpPort, TargetRequest, TargetState,
        },
        fidl_fuchsia_developer_remotecontrol as frcs,
        fidl_fuchsia_net::{IpAddress, Ipv4Address},
        fidl_fuchsia_pkg::{
            MirrorConfig, RepositoryConfig, RepositoryManagerRequest,
            RepositoryManagerRequestStream,
        },
        fidl_fuchsia_pkg_rewrite::{
            EditTransactionRequest, EngineRequest, EngineRequestStream, RuleIteratorRequest,
        },
        fidl_fuchsia_pkg_rewrite_ext::Rule,
        frcs::RemoteControlMarker,
        fuchsia_repo::{
            repo_builder::RepoBuilder, repo_keys::RepoKeys, repository::HttpRepository, test_utils,
        },
        futures::channel::mpsc::{self, Receiver},
        std::{collections::BTreeSet, sync::Mutex, time},
        tuf::{crypto::Ed25519PrivateKey, metadata::Metadata},
        url::Url,
    };

    const REPO_NAME: &str = "some-repo";
    const REPO_IPV4_ADDR: [u8; 4] = [127, 0, 0, 1];
    const REPO_ADDR: &str = "127.0.0.1";
    const REPO_PORT: u16 = 0;
    const DEVICE_PORT: u16 = 5;
    const HOST_ADDR: &str = "1.2.3.4";
    const TARGET_NODENAME: &str = "some-target";
    const EMPTY_REPO_PATH: &str =
        concat!(env!("ROOT_OUT_DIR"), "/test_data/ffx_lib_pkg/empty-repo");

    macro_rules! rule {
        ($host_match:expr => $host_replacement:expr,
         $path_prefix_match:expr => $path_prefix_replacement:expr) => {
            Rule::new($host_match, $host_replacement, $path_prefix_match, $path_prefix_replacement)
                .unwrap()
        };
    }

    fn to_target_info(nodename: String) -> TargetInfo {
        let device_addr = TargetAddrInfo::IpPort(TargetIpPort {
            ip: IpAddress::Ipv4(Ipv4Address { addr: [127, 0, 0, 1] }),
            scope_id: 0,
            port: DEVICE_PORT,
        });

        TargetInfo {
            nodename: Some(nodename),
            addresses: Some(vec![device_addr.clone()]),
            ssh_address: Some(device_addr.clone()),
            ssh_host_address: Some(SshHostAddrInfo { address: HOST_ADDR.to_string() }),
            age_ms: Some(101),
            rcs_state: Some(RemoteControlState::Up),
            target_state: Some(TargetState::Unknown),
            ..Default::default()
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum FakeTargetResponse {
        Ignore,
        Respond,
    }
    struct FakeTargetCollection;

    impl FakeTargetCollection {
        fn new(
            repo_manager: FakeRepositoryManager,
            engine: FakeEngine,
            // Optional: Let the fake RCS connection
            // know which target requests should be answered.
            // The vector is pop()ed at each incoming request.
            mut target_responses: Option<Vec<FakeTargetResponse>>,
        ) -> (Self, TargetCollectionProxy, Receiver<String>) {
            let (sender, rx) = futures::channel::mpsc::channel::<String>(8);
            let fake_rcs = FakeRcs::new();
            // We pop vector elements below, so reverse the vec to
            // maintain the expected temporal order.
            if let Some(v) = &mut target_responses {
                v.reverse();
            }
            let fake_target_collection_proxy: TargetCollectionProxy =
                fho::testing::fake_proxy(move |req| match req {
                    TargetCollectionRequest::ListTargets { query, reader, control_handle: _ } => {
                        let reader = reader.into_proxy().unwrap();
                        let target_info_values: Vec<TargetInfo> = match query.string_matcher {
                            Some(s) => vec![to_target_info(s)],
                            None => vec![to_target_info(TARGET_NODENAME.to_string())],
                        };
                        fuchsia_async::Task::local(async move {
                            let mut iter = target_info_values.chunks(10);
                            loop {
                                let chunk = iter.next().unwrap_or(&[]);
                                reader.next(&chunk).await.unwrap();
                                if chunk.is_empty() {
                                    break;
                                }
                            }
                        })
                        .detach();
                    }
                    TargetCollectionRequest::OpenTarget { query: _, target_handle, responder } => {
                        let mut target_stream = target_handle.into_stream().unwrap();
                        let repo_manager = repo_manager.clone();
                        let engine = engine.clone();
                        let fake_rcs = fake_rcs.clone();
                        let sender = sender.clone();
                        // The default is to respond successfully to a request,
                        // unless there is a value in the vector saying otherwise.
                        let target_response = if let Some(v) = &mut target_responses {
                            if let Some(r) = v.pop() {
                                r
                            } else {
                                FakeTargetResponse::Respond
                            }
                        } else {
                            FakeTargetResponse::Respond
                        };
                        fuchsia_async::Task::local(async move {
                            while let Ok(Some(req)) = target_stream.try_next().await {
                                match req {
                                    // Fake RCS
                                    TargetRequest::OpenRemoteControl {
                                        remote_control,
                                        responder,
                                    } => {
                                        let repo_manager = repo_manager.clone();
                                        let engine = engine.clone();
                                        let sender = sender.clone();
                                        fake_rcs.spawn(
                                            remote_control.into_stream().unwrap(),
                                            repo_manager,
                                            engine,
                                            sender,
                                            target_response,
                                        );
                                        responder.send(Ok(())).unwrap();
                                    }
                                    _ => panic!("unexpected request: {:?}", req),
                                }
                            }
                        })
                        .detach();
                        responder.send(Ok(())).unwrap();
                    }
                    _ => panic!("unexpected request: {:?}", req),
                });
            (Self {}, fake_target_collection_proxy, rx)
        }
    }

    #[derive(Clone)]
    struct FakeRcs;

    impl FakeRcs {
        fn new() -> Self {
            Self {}
        }

        fn spawn(
            &self,
            mut rcs_server: fidl_fuchsia_developer_remotecontrol::RemoteControlRequestStream,
            repo_manager: FakeRepositoryManager,
            engine: FakeEngine,
            mut sender: mpsc::Sender<String>,
            response: FakeTargetResponse,
        ) {
            fuchsia_async::Task::local(async move {
                while let Ok(Some(req)) = rcs_server.try_next().await {
                    match req {
                        frcs::RemoteControlRequest::OpenCapability {
                            moniker: _,
                            capability_set: _,
                            capability_name,
                            server_channel,
                            flags: _,
                            responder,
                        } => {
                            match capability_name.as_str() {
                                RepositoryManagerMarker::PROTOCOL_NAME => {
                                    if response == FakeTargetResponse::Respond {
                                        repo_manager.spawn(
                                            fidl::endpoints::ServerEnd::<RepositoryManagerMarker>::new(
                                                server_channel,
                                            )
                                            .into_stream()
                                            .unwrap(),
                                        );
                                    }
                                    let _send = sender
                                        .send(RepositoryManagerMarker::PROTOCOL_NAME.to_owned())
                                        .await;
                                }
                                EngineMarker::PROTOCOL_NAME => {
                                    if response == FakeTargetResponse::Respond {
                                        engine.spawn(
                                            fidl::endpoints::ServerEnd::<EngineMarker>::new(
                                                server_channel,
                                            )
                                            .into_stream()
                                            .unwrap(),
                                        );
                                    }
                                    let _send = sender
                                        .send(EngineMarker::PROTOCOL_NAME.to_owned())
                                        .await;
                                }
                                RemoteControlMarker::PROTOCOL_NAME => {
                                    // Serve the periodic knock whether the fake target is alive
                                    // By knock_rcs_impl() in
                                    // src/developer/ffx/lib/rcs/src/lib.rs
                                    // a knock is considered unsuccessful if there is no
                                    // channel connection available within the timeout.
                                    // Hence we will provide a viable channel (or not), depending
                                    // on the input args for this test.
                                    if response == FakeTargetResponse::Respond {
                                        let mut stream =
                                            fidl::endpoints::ServerEnd::<RemoteControlMarker>::new(
                                                server_channel,
                                            )
                                            .into_stream()
                                            .unwrap();
                                        fasync::Task::local(async move {
                                            while let Some(Ok(req)) = stream.next().await {
                                                // Do nada, just take the request
                                                let _ = req;
                                            }
                                        })
                                        .detach();
                                    }
                                    let _send = sender
                                        .send(RemoteControlMarker::PROTOCOL_NAME.to_owned())
                                        .await;
                                }
                                e => {
                                    panic!("Requested capability not implemented: {}", e);
                                }
                            }
                            responder.send(Ok(())).unwrap();
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }
    }

    #[derive(Debug, PartialEq)]
    enum RepositoryManagerEvent {
        Add { repo: RepositoryConfig },
    }

    #[derive(Clone)]
    struct FakeRepositoryManager {
        events: Arc<Mutex<Vec<RepositoryManagerEvent>>>,
        sender: mpsc::Sender<()>,
    }

    impl FakeRepositoryManager {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));

            (Self { events, sender }, rx)
        }

        fn spawn(&self, mut stream: RepositoryManagerRequestStream) {
            let sender = self.sender.clone();
            let events_closure = Arc::clone(&self.events);

            fasync::Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        RepositoryManagerRequest::Add { repo, responder } => {
                            let mut sender = sender.clone();
                            let events_closure = events_closure.clone();

                            fasync::Task::local(async move {
                                events_closure
                                    .lock()
                                    .unwrap()
                                    .push(RepositoryManagerEvent::Add { repo });
                                responder.send(Ok(())).unwrap();
                                let _send = sender.send(()).await.unwrap();
                            })
                            .detach();
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }

        fn take_events(&self) -> Vec<RepositoryManagerEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    #[derive(Debug, PartialEq)]
    enum RewriteEngineEvent {
        ResetAll,
        ListDynamic,
        IteratorNext,
        EditTransactionAdd { rule: Rule },
        EditTransactionCommit,
    }

    #[derive(Clone)]
    struct FakeEngine {
        events: Arc<Mutex<Vec<RewriteEngineEvent>>>,
        sender: mpsc::Sender<()>,
    }

    impl FakeEngine {
        fn new() -> (Self, mpsc::Receiver<()>) {
            let (sender, rx) = futures::channel::mpsc::channel::<()>(1);
            let events = Arc::new(Mutex::new(Vec::new()));

            (Self { events, sender }, rx)
        }

        fn spawn(&self, mut stream: EngineRequestStream) {
            let rules: Arc<Mutex<Vec<Rule>>> = Arc::new(Mutex::new(Vec::<Rule>::new()));
            let sender = self.sender.clone();
            let events_closure = Arc::clone(&self.events);

            fasync::Task::local(async move {
                while let Some(Ok(req)) = stream.next().await {
                    match req {
                        EngineRequest::StartEditTransaction { transaction, control_handle: _ } => {
                            let mut sender = sender.clone();
                            let rules = Arc::clone(&rules);
                            let events_closure = Arc::clone(&events_closure);

                            fasync::Task::local(async move {
                                let mut stream = transaction.into_stream().unwrap();
                                while let Some(request) = stream.next().await {
                                    let request = request.unwrap();
                                    match request {
                                        EditTransactionRequest::ResetAll { control_handle: _ } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::ResetAll);
                                        }
                                        EditTransactionRequest::ListDynamic {
                                            iterator,
                                            control_handle: _,
                                        } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::ListDynamic);
                                            let mut stream = iterator.into_stream().unwrap();

                                            let mut rules =
                                                rules.lock().unwrap().clone().into_iter();

                                            while let Some(req) = stream.try_next().await.unwrap() {
                                                let RuleIteratorRequest::Next { responder } = req;
                                                events_closure
                                                    .lock()
                                                    .unwrap()
                                                    .push(RewriteEngineEvent::IteratorNext);

                                                if let Some(rule) = rules.next() {
                                                    responder.send(&[rule.into()]).unwrap();
                                                } else {
                                                    responder.send(&[]).unwrap();
                                                }
                                            }
                                        }
                                        EditTransactionRequest::Add { rule, responder } => {
                                            events_closure.lock().unwrap().push(
                                                RewriteEngineEvent::EditTransactionAdd {
                                                    rule: rule.try_into().unwrap(),
                                                },
                                            );
                                            responder.send(Ok(())).unwrap()
                                        }
                                        EditTransactionRequest::Commit { responder } => {
                                            events_closure
                                                .lock()
                                                .unwrap()
                                                .push(RewriteEngineEvent::EditTransactionCommit);
                                            let res = responder.send(Ok(())).unwrap();
                                            let _send = sender.send(()).await.unwrap();
                                            res
                                        }
                                    }
                                }
                            })
                            .detach();
                        }
                        _ => panic!("unexpected request: {:?}", req),
                    }
                }
            })
            .detach();
        }

        fn take_events(&self) -> Vec<RewriteEngineEvent> {
            self.events.lock().unwrap().drain(..).collect::<Vec<_>>()
        }
    }

    async fn get_test_env() -> TestEnv {
        let test_env = ffx_config::test_init().await.expect("test initialization");

        test_env
            .context
            .query(REPO_FOREGROUND_FEATURE_FLAG)
            .level(Some(ConfigLevel::User))
            .set("true".into())
            .await
            .unwrap();

        test_env
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_register() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, mut fake_engine_rx) = FakeEngine::new();
        let (_, target_collection_proxy, mut target_collection_proxy_rx) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

        // Future resolves once fake target exists
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let fake_targets = get_target_infos(
                &target_collection_proxy,
                &FfxTargetQuery { string_matcher: None, ..Default::default() },
            )
            .await
            .unwrap();

            assert_eq!(fake_targets[0], to_target_info(TARGET_NODENAME.to_string()));
        })
        .await
        .unwrap();

        let serve_tool = ServeTool {
            cmd: ServeCommand {
                repository: Some(REPO_NAME.to_string()),
                trusted_root: None,
                address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                repo_path: Some(EMPTY_REPO_PATH.into()),
                product_bundle: None,
                alias: vec!["example.com".into(), "fuchsia.com".into()],
                storage_type: Some(RepositoryStorageType::Ephemeral),
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                port_path: Some(tmp_port_file.path().to_owned()),
                no_device: false,
            },
            context: test_env.context.clone(),
            target_collection_proxy: target_collection_proxy,
        };

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move { serve_tool.main(writer).await.unwrap() });

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_repo_rx.next().await.unwrap();
            let _ = fake_engine_rx.next().await.unwrap();
            let _fake_repo_response = target_collection_proxy_rx.next().await.unwrap();
            let _fake_engine_response = target_collection_proxy_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        assert_eq!(
            fake_repo.take_events(),
            vec![RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(repo_url.clone()),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                    root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            }],
        );

        assert_eq!(
            fake_engine.take_events(),
            vec![
                RewriteEngineEvent::ListDynamic,
                RewriteEngineEvent::IteratorNext,
                RewriteEngineEvent::ResetAll,
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionCommit,
            ],
        );

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_auto_reconnect() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        // For this test, we specify which requests the Fake RCS should respond to:
        let open_target_responses = vec![
            FakeTargetResponse::Respond, // First OpenTarget request (fake repo + fake engine)
            FakeTargetResponse::Respond, // First knock request
            FakeTargetResponse::Ignore, // Second knock request, this is expected to lead to a reconnect
        ];
        let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, mut fake_engine_rx) = FakeEngine::new();
        let (_, target_collection_proxy, mut target_collection_proxy_rx) =
            FakeTargetCollection::new(
                fake_repo.clone(),
                fake_engine.clone(),
                Some(open_target_responses),
            );

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        // Future resolves once fake target exists
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let fake_targets = get_target_infos(
                &target_collection_proxy,
                &FfxTargetQuery { string_matcher: None, ..Default::default() },
            )
            .await
            .unwrap();

            assert_eq!(fake_targets[0], to_target_info(TARGET_NODENAME.to_string()));
        })
        .await
        .unwrap();

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                target_collection_proxy,
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(EMPTY_REPO_PATH.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                },
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_repo_rx.next().await.unwrap();
            let _ = fake_engine_rx.next().await.unwrap();
            let fake_repo_response = target_collection_proxy_rx.next().await.unwrap();
            let fake_engine_response = target_collection_proxy_rx.next().await.unwrap();
            assert_eq!(fake_repo_response, RepositoryManagerMarker::PROTOCOL_NAME);
            assert_eq!(fake_engine_response, EngineMarker::PROTOCOL_NAME);
        })
        .await
        .unwrap();

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        assert_eq!(
            fake_repo.take_events(),
            vec![RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(repo_url.clone()),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                    root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            }],
        );

        assert_eq!(
            fake_engine.take_events(),
            vec![
                RewriteEngineEvent::ListDynamic,
                RewriteEngineEvent::IteratorNext,
                RewriteEngineEvent::ResetAll,
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("example.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionAdd {
                    rule: rule!("fuchsia.com" => REPO_NAME, "/" => "/"),
                },
                RewriteEngineEvent::EditTransactionCommit,
            ],
        );

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));

        // Wait for the two target knock with nothing else in between
        let mut reqs = Vec::new();
        for _ in 0..2 {
            reqs.push(target_collection_proxy_rx.next().await.unwrap());
        }

        assert_eq!(
            reqs,
            vec![RemoteControlMarker::PROTOCOL_NAME, RemoteControlMarker::PROTOCOL_NAME,]
        );

        // The second target knock is not answered, see the open_target_responses vector above.
        // Hence we expect reconnect instead of another RemoteControlMarker::PROTOCOL_NAME
        // We check the next three requests to verify the reconnect has happened.
        reqs.clear();
        for _ in 0..3 {
            reqs.push(target_collection_proxy_rx.next().await.unwrap());
        }
        assert_eq!(
            reqs,
            vec![
                RepositoryManagerMarker::PROTOCOL_NAME,
                EngineMarker::PROTOCOL_NAME,
                RemoteControlMarker::PROTOCOL_NAME,
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_device() {
        let test_env = get_test_env().await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();

        let (_, target_collection_proxy, _) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                target_collection_proxy,
                ServeCommand {
                    repository: Some(REPO_NAME.to_string()),
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: Some(EMPTY_REPO_PATH.into()),
                    product_bundle: None,
                    alias: vec!["example.com".into(), "fuchsia.com".into()],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                },
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Wait for the "Serving repository ..." output
        for _ in 0..10 {
            if !test_stdout.clone().into_string().is_empty() {
                break;
            }
            fasync::Timer::new(time::Duration::from_millis(100)).await;
        }

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_get_target_infos_from_fake_target_collection() {
        let _test_env = get_test_env().await;

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, target_collection_proxy, _) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);
        let target_query = FfxTargetQuery { string_matcher: None, ..Default::default() };

        let targets = get_target_infos(&target_collection_proxy, &target_query).await.unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], to_target_info(TARGET_NODENAME.to_string()));

        let targets = get_target_infos(&target_collection_proxy, &target_query).await.unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], to_target_info(TARGET_NODENAME.to_string()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_rcs_from_fake_target_collection() {
        let _test_env = get_test_env().await;

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, target_collection_proxy, _) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);
        let (target_proxy, target_marker) = fidl::endpoints::create_proxy().unwrap();
        let (_, rcs_marker) = fidl::endpoints::create_proxy().unwrap();
        let c = target_collection_proxy
            .open_target(
                &FfxTargetQuery { string_matcher: None, ..Default::default() },
                target_marker,
            )
            .await;

        assert!(c.is_ok());

        let c = target_proxy.open_remote_control(rcs_marker).await;
        assert!(c.is_ok());
    }

    async fn write_product_bundle(pb_dir: &Utf8Path) {
        let blobs_dir = pb_dir.join("blobs");

        let mut repositories = vec![];
        for repo_name in ["fuchsia.com", "example.com"] {
            let metadata_path = pb_dir.join(repo_name);
            fuchsia_repo::test_utils::make_repo_dir(metadata_path.as_ref(), blobs_dir.as_ref())
                .await;
            repositories.push(sdk_metadata::Repository {
                name: repo_name.into(),
                metadata_path,
                blobs_path: blobs_dir.clone(),
                delivery_blob_type: 1,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            });
        }

        let pb = sdk_metadata::ProductBundle::V2(sdk_metadata::ProductBundleV2 {
            product_name: "test".into(),
            product_version: "test-product-version".into(),
            partitions: assembly_partitions_config::PartitionsConfig::default(),
            sdk_version: "test-sdk-version".into(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories,
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(&pb_dir).unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_serve_product_bundle() {
        let test_env = get_test_env().await;

        test_env
            .context
            .query(TARGET_DEFAULT_KEY)
            .level(Some(ConfigLevel::User))
            .set(TARGET_NODENAME.into())
            .await
            .unwrap();

        let tmp_pb_dir = tempfile::tempdir().unwrap();
        let pb_dir = Utf8Path::from_path(tmp_pb_dir.path()).unwrap().canonicalize_utf8().unwrap();
        write_product_bundle(&pb_dir).await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();
        let tmp_port_file_path = tmp_port_file.path().to_owned();

        let (fake_repo, mut fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();
        let (_, target_collection_proxy, _) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);

        // Future resolves once fake target exists
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let fake_targets = get_target_infos(
                &target_collection_proxy,
                &FfxTargetQuery { string_matcher: None, ..Default::default() },
            )
            .await
            .unwrap();

            assert_eq!(fake_targets[0], to_target_info(TARGET_NODENAME.to_string()));
        })
        .await
        .unwrap();

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                target_collection_proxy,
                ServeCommand {
                    repository: None,
                    trusted_root: None,
                    address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                    repo_path: None,
                    product_bundle: Some(pb_dir),
                    alias: vec![],
                    storage_type: Some(RepositoryStorageType::Ephemeral),
                    alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                    port_path: Some(tmp_port_file_path),
                    no_device: false,
                },
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_repo_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        assert_eq!(
            fake_repo.take_events(),
            ["example.com", "fuchsia.com"].map(|repo_name| RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(format!(
                            "http://{REPO_ADDR}:{dynamic_repo_port}/{repo_name}"
                        )),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", repo_name)),
                    root_keys: Some(vec![fuchsia_repo::test_utils::repo_key().into()]),
                    root_version: Some(1),
                    root_threshold: Some(1),
                    use_local_mirror: Some(false),
                    storage_type: Some(fidl_fuchsia_pkg::RepositoryStorageType::Ephemeral),
                    ..Default::default()
                }
            },)
        );

        // Check repository state.
        for repo_name in ["example.com", "fuchsia.com"] {
            let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{repo_name}");
            let http_repo = HttpRepository::new(
                fuchsia_hyper::new_client(),
                Url::parse(&repo_url).unwrap(),
                Url::parse(&format!("{repo_url}/blobs")).unwrap(),
                BTreeSet::new(),
            );
            let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

            assert_matches!(repo_client.update().await, Ok(true));
        }
    }

    fn generate_ed25519_private_key() -> Ed25519PrivateKey {
        Ed25519PrivateKey::from_pkcs8(&Ed25519PrivateKey::pkcs8().unwrap()).unwrap()
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_trusted_root_file() {
        let test_env = get_test_env().await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

        // Set up a simple test repository
        let tmp_repo = tempfile::tempdir().unwrap();
        let tmp_repo_path = Utf8Path::from_path(tmp_repo.path()).unwrap();
        let tmp_pm_repo = test_utils::make_pm_repository(tmp_repo_path).await;
        let mut tmp_repo_client = RepoClient::from_trusted_remote(&tmp_pm_repo).await.unwrap();
        tmp_repo_client.update().await.unwrap();

        // Generate a newer set of keys.
        let repo_keys_new = RepoKeys::builder()
            .add_root_key(Box::new(generate_ed25519_private_key()))
            .add_targets_key(Box::new(generate_ed25519_private_key()))
            .add_snapshot_key(Box::new(generate_ed25519_private_key()))
            .add_timestamp_key(Box::new(generate_ed25519_private_key()))
            .build();

        // Generate new metadata that trusts the new keys, but signs it with the old keys.
        let repo_signing_keys = tmp_pm_repo.repo_keys().unwrap();
        RepoBuilder::from_database(
            tmp_repo_client.remote_repo(),
            &repo_keys_new,
            tmp_repo_client.database(),
        )
        .signing_repo_keys(&repo_signing_keys)
        .commit()
        .await
        .unwrap();

        assert_eq!(tmp_repo_client.database().trusted_timestamp().unwrap().version(), 1);

        // Delete 1.root.json to ensure it can't be accessed when initializing
        // root of trust with 2.root.json
        std::fs::remove_file(tmp_repo_path.join("repository").join("1.root.json")).unwrap();
        // Move root.json and 2.root.json out of the repository/ dir, to verify
        // we can pass the root of trust file from anywhere to a repo constructor
        let tmp_root = tempfile::tempdir().unwrap();
        let tmp_root_dir = Utf8Path::from_path(tmp_root.path()).unwrap();
        let trusted_root_path = tmp_root_dir.join("2.root.json");
        std::fs::rename(
            tmp_repo_path.join("repository").join("root.json"),
            tmp_root_dir.join("root.json"),
        )
        .unwrap();
        std::fs::rename(tmp_repo_path.join("repository").join("2.root.json"), &trusted_root_path)
            .unwrap();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();

        let (_, target_collection_proxy, _) =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone(), None);

        // Prepare serving the repo without passing the trusted root, and
        // passing of the trusted root 2.root.json explicitly
        let serve_cmd_without_root = ServeCommand {
            repository: Some(REPO_NAME.to_string()),
            trusted_root: None,
            address: (REPO_IPV4_ADDR, REPO_PORT).into(),
            repo_path: Some(tmp_repo_path.into()),
            product_bundle: None,
            alias: vec![],
            storage_type: None,
            alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
            port_path: Some(tmp_port_file.path().to_owned()),
            no_device: true,
        };
        let mut serve_cmd_with_root = serve_cmd_without_root.clone();
        serve_cmd_with_root.trusted_root = trusted_root_path.clone().into();

        // Serving the repo should error out since it does not find root.json and
        // and can't initialize root of trust 1.root.json.
        assert_eq!(
            serve_impl(
                target_collection_proxy.clone(),
                serve_cmd_without_root,
                test_env.context.clone(),
                SimpleWriter::new()
            )
            .await
            .is_err(),
            true
        );

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(async move {
            serve_impl(
                target_collection_proxy,
                serve_cmd_with_root,
                test_env.context.clone(),
                writer,
            )
            .await
            .unwrap()
        });

        // Wait for the "Serving repository ..." output
        for _ in 0..10 {
            if !test_stdout.clone().into_string().is_empty() {
                break;
            }
            fasync::Timer::new(time::Duration::from_millis(100)).await;
        }

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        let repo_url = format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}");

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&repo_url).unwrap(),
            Url::parse(&format!("{repo_url}/blobs")).unwrap(),
            BTreeSet::new(),
        );

        // As there was no key rotation since we created 2.root.json above,
        // and we removed root.json out of the repo, creating a repo client via
        // RepoClient::from_trusted_remote would error out trying to find root.json.
        // Hence we need to initialize the http client with 2.root.json, too.
        let mut repo_client =
            repo_client_from_optional_trusted_root(Some(trusted_root_path), http_repo)
                .await
                .unwrap();

        // The repo metadata should be at version 2
        assert_matches!(repo_client.update().await, Ok(true));
        assert_eq!(repo_client.database().trusted_timestamp().unwrap().version(), 2);
    }
}
