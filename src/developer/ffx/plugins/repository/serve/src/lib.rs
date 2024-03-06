// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context as _},
    async_lock::RwLock,
    async_trait::async_trait,
    ffx_config::EnvironmentContext,
    ffx_repository_serve_args::ServeCommand,
    ffx_target::{get_default_target, knock_target_by_name},
    fho::{daemon_protocol, AvailabilityFlag, FfxMain, FfxTool, Result, SimpleWriter},
    fidl_fuchsia_developer_ffx::{
        RepositoryError as FfxCliRepositoryError, RepositoryTarget as FfxCliRepositoryTarget,
        TargetQuery as FfxTargetQuery,
    },
    fidl_fuchsia_developer_ffx::{
        TargetCollectionProxy, TargetCollectionReaderMarker, TargetCollectionReaderRequest,
        TargetInfo,
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
        manager::RepositoryManager, repo_client::RepoClient, repository::PmRepository,
        repository::RepoProvider, server::RepositoryServer,
    },
    futures::TryStreamExt as _,
    pkg::repo::register_target_with_fidl_proxies,
    std::{fs, io::Write, path::Path, sync::Arc, time::Duration},
    timeout::timeout,
    tracing,
};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const REPO_FOREGROUND_FEATURE_FLAG: &str = "repository.foreground.enabled";
const REPOSITORY_MANAGER_MONIKER: &str = "/core/pkg-resolver";
const ENGINE_MONIKER: &str = "/core/pkg-resolver";

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
    repo_target: FfxCliRepositoryTarget,
    repo_server_listen_addr: std::net::SocketAddr,
    repo: &Arc<RwLock<RepoClient<Box<dyn RepoProvider>>>>,
    target_collection_proxy: &TargetCollectionProxy,
    alias_conflict_mode: FfxRepositoryRegistrationAliasConflictMode,
) -> Result<(), anyhow::Error> {
    let target_query = FfxTargetQuery {
        string_matcher: repo_target.target_identifier.clone(),
        ..Default::default()
    };

    // Construct RepositoryTarget from same args as `ffx target repository register`
    let repo_target_info = FfxDaemonRepositoryTarget::try_from(repo_target)
        .map_err(|e| anyhow!("Failed to build RepositoryTarget: {:?}", e))?;

    let rcs_proxy = connect_to_rcs(target_collection_proxy, &target_query).await?;

    let tv = get_target_infos(target_collection_proxy, &target_query).await?;

    let target = if tv.len() != 1 {
        return Err(anyhow!(
            "Exactly one target is expected in the target collection, found {}.",
            tv.len()
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

    register_target_with_fidl_proxies(
        repo_proxy,
        engine_proxy,
        &repo_target_info,
        &target,
        repo_server_listen_addr,
        repo,
        alias_conflict_mode.into(),
    )
    .await
    .map_err(|e| anyhow!("Failed to register repository: {:?}", e))?;

    Ok(())
}

#[async_trait(?Send)]
impl FfxMain for ServeTool {
    type Writer = SimpleWriter;

    async fn main(self, mut writer: SimpleWriter) -> Result<()> {
        let repo_name = self.cmd.repository;
        let repo_server_listen_addr = self.cmd.address;

        let repo_path = if let Some(repo_path) = self.cmd.repo_path {
            repo_path
        } else {
            // Default to "FUCHSIA_BUILD_DIR/amber-files"
            let fuchsia_build_dir = self.context.build_dir().unwrap_or(&Path::new(""));

            fuchsia_build_dir.join("amber-files")
        };

        // Create PmRepository and RepoClient
        let repo_path = fs::canonicalize(repo_path.clone())
            .with_context(|| format!("canonicalizing repo path {:?}", repo_path))?;
        let pm_backend = PmRepository::new(
            repo_path
                .clone()
                .try_into()
                .with_context(|| format!("converting repo path to UTF-8 {:?}", repo_path))?,
        );
        let pm_repo_client = RepoClient::from_trusted_remote(Box::new(pm_backend) as Box<_>)
            .await
            .with_context(|| format!("creating repo client"))?;

        // Add PmRepository to RepositoryManager
        let repo_manager: Arc<RepositoryManager> = RepositoryManager::new();
        repo_manager.add(repo_name.clone(), pm_repo_client);
        let repo = repo_manager
            .get(&repo_name)
            .ok_or_else(|| FfxCliRepositoryError::NoMatchingRepository)
            .map_err(|e| anyhow!("Failed to fetch repository: {:?}", e))?;

        // Serve RepositoryManager over a RepositoryServer
        let (server_fut, _, server) =
            RepositoryServer::builder(repo_server_listen_addr, Arc::clone(&repo_manager))
                .start()
                .await
                .with_context(|| format!("starting repository server"))?;

        // Write port file if needed
        if let Some(port_path) = self.cmd.port_path {
            let port = server.local_addr().port().to_string();

            fs::write(port_path, port.clone())
                .with_context(|| format!("creating port file for port {}", port))?;
        };
        let task = fasync::Task::local(server_fut);

        if !self.cmd.no_device {
            loop {
                // Resolving the default target is typically fast
                // or does not succeed, in which case we return from the
                // function with an error.
                tracing::info!("Resolving default target");
                let target_identifier =
                    timeout(Duration::from_secs(1), get_default_target(&self.context))
                        .await
                        .with_context(|| format!("getting target identifier for default target"))??
                        .ok_or(anyhow!("no default target value"))?;

                tracing::info!("Default target: {}", target_identifier);

                let repo_target = FfxCliRepositoryTarget {
                    repo_name: Some(repo_name.clone()),
                    target_identifier: Some(target_identifier.clone()),
                    aliases: Some(self.cmd.alias.clone()),
                    storage_type: self.cmd.storage_type,
                    ..Default::default()
                };

                // This blocks until the default target becomes available
                // if a connection is possible.
                tracing::info!("Attempting connection to default target");
                let connection = connect_to_target(
                    repo_target,
                    repo_server_listen_addr,
                    &repo,
                    &self.target_collection_proxy,
                    self.cmd.alias_conflict_mode.into(),
                )
                .await;

                match connection {
                    Ok(()) => {
                        let s = format!("Serving repository '{}' to target '{target_identifier}' over address '{repo_server_listen_addr}'.", repo_path.display());
                        writeln!(writer, "{}", s,)
                            .map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;
                        tracing::info!("{}", s);
                        loop {
                            fuchsia_async::Timer::new(std::time::Duration::from_secs(10)).await;
                            if let Err(e) = knock_target_by_name(
                                &Some(target_identifier.clone()),
                                &self.target_collection_proxy,
                                std::time::Duration::from_secs(2),
                                std::time::Duration::from_secs(2),
                            )
                            .await
                            {
                                tracing::warn!("Connection to target lost, retrying. Error: {}", e);
                                break;
                            };
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Cannot (yet) serve repository, retrying. Error: {}", e);
                        // If a a connection can't be established due to PEER_CLOSED, we end up
                        // here immediately after connect_to_target() fails. Wait before a
                        // reconnect attempt is made.
                        fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
                    }
                };
            }
        } else {
            let s = format!(
                "Serving repository '{}' over address '{repo_server_listen_addr}'.",
                repo_path.display()
            );
            writeln!(writer, "{}", s,)
                .map_err(|e| anyhow!("Failed to write to output: {:?}", e))?;
            tracing::info!("{}", s);
        }

        // Wait for the server to shut down.
        task.await;

        Ok(())
    }
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
            RemoteControlState, RepositoryRegistrationAliasConflictMode, RepositoryStorageType,
            SshHostAddrInfo, TargetAddrInfo, TargetCollectionRequest, TargetIpPort, TargetRequest,
            TargetState,
        },
        fidl_fuchsia_developer_remotecontrol as frcs,
        fidl_fuchsia_net::{IpAddress, Ipv4Address},
        fidl_fuchsia_pkg::{
            MirrorConfig, RepositoryConfig, RepositoryKeyConfig, RepositoryManagerRequest,
            RepositoryManagerRequestStream,
        },
        fidl_fuchsia_pkg_rewrite::{
            EditTransactionRequest, EngineRequest, EngineRequestStream, RuleIteratorRequest,
        },
        fidl_fuchsia_pkg_rewrite_ext::Rule,
        frcs::RemoteControlMarker,
        fuchsia_repo::repository::HttpRepository,
        futures::{channel::mpsc, SinkExt, StreamExt as _, TryStreamExt},
        std::{collections::BTreeSet, sync::Mutex, time},
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
        concat!(env!("ROOT_OUT_DIR"), "/test_data/ffx_plugin_serve_repo/empty-repo");

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
    struct FakeTargetCollection;

    impl FakeTargetCollection {
        fn new(repo_manager: FakeRepositoryManager, engine: FakeEngine) -> TargetCollectionProxy {
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
                                        FakeRcs::new(
                                            remote_control.into_stream().unwrap(),
                                            repo_manager,
                                            engine,
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
            fake_target_collection_proxy
        }
    }

    struct FakeRcs;

    impl FakeRcs {
        fn new(
            mut rcs_server: fidl_fuchsia_developer_remotecontrol::RemoteControlRequestStream,
            repo_manager: FakeRepositoryManager,
            engine: FakeEngine,
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
                                RepositoryManagerMarker::PROTOCOL_NAME => repo_manager.spawn(
                                    fidl::endpoints::ServerEnd::<RepositoryManagerMarker>::new(
                                        server_channel,
                                    )
                                    .into_stream()
                                    .unwrap(),
                                ),
                                EngineMarker::PROTOCOL_NAME => engine.spawn(
                                    fidl::endpoints::ServerEnd::<EngineMarker>::new(server_channel)
                                        .into_stream()
                                        .unwrap(),
                                ),
                                RemoteControlMarker::PROTOCOL_NAME => {
                                    // Serve the periodic knock whether the fake target is alive
                                    let mut stream =
                                        fidl::endpoints::ServerEnd::<RemoteControlMarker>::new(
                                            server_channel,
                                        )
                                        .into_stream()
                                        .unwrap();
                                    fasync::Task::local(async move {
                                        // By knock_rcs_impl() in
                                        // src/developer/ffx/lib/rcs/src/lib.rs
                                        // a knock is considered successful if the knock
                                        // channel is being kept open for more than 1 second
                                        // after sending the request. Hence we keep the binding
                                        // for a bit longer before calling it quits.
                                        let _ = stream.try_next().await;
                                        fuchsia_async::Timer::new(
                                            std::time::Duration::from_secs_f64(1.2),
                                        )
                                        .await;
                                    })
                                    .detach();
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
            let (sender, rx) = mpsc::channel::<()>(1);
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
            let (sender, rx) = mpsc::channel::<()>(1);
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
        let target_collection_proxy =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone());

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
                repository: REPO_NAME.to_string(),
                address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                repo_path: Some(EMPTY_REPO_PATH.into()),
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
        let _task = fasync::Task::local(serve_tool.main(writer));

        // Future resolves once repo server communicates with them.
        let _timeout = timeout(time::Duration::from_secs(10), async {
            let _ = fake_repo_rx.next().await.unwrap();
            let _ = fake_engine_rx.next().await.unwrap();
        })
        .await
        .unwrap();

        assert_eq!(
            fake_repo.take_events(),
            vec![RepositoryManagerEvent::Add {
                repo: RepositoryConfig {
                    mirrors: Some(vec![MirrorConfig {
                        mirror_url: Some(format!(
                            "http://{}:{}/{}",
                            REPO_ADDR, REPO_PORT, REPO_NAME
                        )),
                        subscribe: Some(true),
                        ..Default::default()
                    }]),
                    repo_url: Some(format!("fuchsia-pkg://{}", REPO_NAME)),
                    root_keys: Some(vec![RepositoryKeyConfig::Ed25519Key(vec![
                        29, 76, 86, 76, 184, 70, 108, 73, 249, 127, 4, 47, 95, 63, 36, 35, 101,
                        255, 212, 33, 10, 154, 26, 130, 117, 157, 125, 88, 175, 214, 109, 113,
                    ])]),
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

        // Get dynamic port
        let dynamic_repo_port =
            fs::read_to_string(tmp_port_file.path()).unwrap().parse::<u16>().unwrap();
        tmp_port_file.close().unwrap();

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}")).unwrap(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}/blobs"))
                .unwrap(),
            BTreeSet::new(),
        );
        let mut repo_client = RepoClient::from_trusted_remote(http_repo).await.unwrap();

        assert_matches!(repo_client.update().await, Ok(true));
        fuchsia_async::Timer::new(std::time::Duration::from_secs(15)).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_device() {
        let test_env = get_test_env().await;

        let tmp_port_file = tempfile::NamedTempFile::new().unwrap();

        let (fake_repo, _fake_repo_rx) = FakeRepositoryManager::new();
        let (fake_engine, _fake_engine_rx) = FakeEngine::new();

        let target_collection_proxy =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone());

        let serve_tool = ServeTool {
            cmd: ServeCommand {
                repository: REPO_NAME.to_string(),
                address: (REPO_IPV4_ADDR, REPO_PORT).into(),
                repo_path: Some(EMPTY_REPO_PATH.into()),
                alias: vec![],
                storage_type: None,
                alias_conflict_mode: RepositoryRegistrationAliasConflictMode::Replace,
                port_path: Some(tmp_port_file.path().to_owned()),
                no_device: true,
            },
            context: test_env.context.clone(),
            target_collection_proxy: target_collection_proxy,
        };

        let test_stdout = TestBuffer::default();
        let writer = SimpleWriter::new_buffers(test_stdout.clone(), Vec::new());

        // Run main in background
        let _task = fasync::Task::local(serve_tool.main(writer));

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

        // Check repository state.
        let http_repo = HttpRepository::new(
            fuchsia_hyper::new_client(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}")).unwrap(),
            Url::parse(&format!("http://{REPO_ADDR}:{dynamic_repo_port}/{REPO_NAME}/blobs"))
                .unwrap(),
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
        let target_collection_proxy =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone());
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
        let target_collection_proxy =
            FakeTargetCollection::new(fake_repo.clone(), fake_engine.clone());
        let (target_proxy, target_marker) = fidl::endpoints::create_proxy().unwrap();
        let (_rcs_proxy, rcs_marker) = fidl::endpoints::create_proxy().unwrap();
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
}
