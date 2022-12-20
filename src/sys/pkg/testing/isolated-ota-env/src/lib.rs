// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]
#![allow(clippy::let_unit_value)]

use {
    anyhow::{Context, Error},
    async_trait::async_trait,
    fidl::endpoints::{ClientEnd, Proxy},
    fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_io::DirectoryProxy,
    fidl_fuchsia_paver::PaverRequestStream,
    fidl_fuchsia_pkg_ext::RepositoryConfigs,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::LocalComponentHandles,
    fuchsia_merkle::Hash,
    fuchsia_pkg_testing::{serve::ServedRepository, Package, PackageBuilder, RepositoryBuilder},
    futures::prelude::*,
    isolated_ota::OmahaConfig,
    mock_omaha_server::{
        OmahaResponse, OmahaServer, OmahaServerBuilder, ResponseAndMetadata, ResponseMap,
    },
    mock_paver::{MockPaverService, MockPaverServiceBuilder},
    parking_lot::Mutex,
    serde_json::json,
    std::{collections::HashMap, io::Write, str::FromStr, sync::Arc},
    tempfile::TempDir,
};

pub const GLOBAL_SSL_CERTS_PATH: &str = "/config/ssl";
const EMPTY_REPO_PATH: &str = "/pkg/empty-repo";
const TEST_CERTS_PATH: &str = "/pkg/data/ssl";
const TEST_REPO_URL: &str = "fuchsia-pkg://integration.test.fuchsia.com";

pub enum OmahaState {
    /// Don't use Omaha for this update.
    Disabled,
    /// Set up an Omaha server automatically.
    Auto(OmahaResponse),
    /// Pass the given OmahaConfig to Omaha.
    Manual(OmahaConfig),
}

pub struct TestParams {
    pub blobfs: Option<ClientEnd<fio::DirectoryMarker>>,
    pub board: String,
    pub channel: String,
    pub packages: Vec<Package>,
    pub paver: Arc<MockPaverService>,
    pub repo_config_dir: TempDir,
    pub ssl_certs: DirectoryProxy,
    pub update_merkle: Hash,
    pub version: String,
    pub omaha_config: Option<OmahaConfig>,
    pub paver_connector: ClientEnd<fio::DirectoryMarker>,
}

/// Connects the local component to a mock paver.
///
/// Unlike other mocks, the `fuchsia.paver.Paver` is serviced by [`isolated_ota_env::TestEnv`], so
/// this function proxies to the given `paver_dir_proxy` which is expected to host a
/// file named "fuchsia.paver.Paver" which implements the `fuchsia.paver.Paver` FIDL protocol.
pub async fn expose_mock_paver(
    handles: LocalComponentHandles,
    paver_dir_proxy: fio::DirectoryProxy,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();

    fs.dir("svc").add_service_connector(
        move |server_end: ServerEnd<fidl_fuchsia_paver::PaverMarker>| {
            fdio::service_connect_at(
                paver_dir_proxy.as_channel().as_ref(),
                &format!("/{}", fidl_fuchsia_paver::PaverMarker::PROTOCOL_NAME),
                server_end.into_channel(),
            )
            .expect("failed to connect to paver service node");
        },
    );

    fs.serve_connection(handles.outgoing_dir).expect("failed to serve paver fs connection");
    fs.collect::<()>().await;
    Ok(())
}

#[async_trait(?Send)]
pub trait TestExecutor<R> {
    async fn run(&self, params: TestParams) -> R;
}

pub struct TestEnvBuilder<R> {
    blobfs: Option<ClientEnd<fio::DirectoryMarker>>,
    board: String,
    channel: String,
    images: HashMap<String, Vec<u8>>,
    omaha: OmahaState,
    packages: Vec<Package>,
    paver: MockPaverServiceBuilder,
    repo_config: Option<RepositoryConfigs>,
    version: String,
    test_executor: Option<Box<dyn TestExecutor<R>>>,
}

impl<R> TestEnvBuilder<R> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        TestEnvBuilder {
            blobfs: None,
            board: "test-board".to_owned(),
            channel: "test".to_owned(),
            images: HashMap::new(),
            omaha: OmahaState::Disabled,
            packages: vec![],
            paver: MockPaverServiceBuilder::new(),
            repo_config: None,
            version: "0.1.2.3".to_owned(),
            test_executor: None,
        }
    }

    /// Add an image to the update package that will be generated by this TestEnvBuilder
    pub fn add_image(mut self, name: &str, data: &[u8]) -> Self {
        self.images.insert(name.to_owned(), data.to_vec());
        self
    }

    /// Add a package to the repository generated by this TestEnvBuilder.
    /// The package will also be listed in the generated update package
    /// so that it will be downloaded as part of the OTA.
    pub fn add_package(mut self, pkg: Package) -> Self {
        self.packages.push(pkg);
        self
    }

    pub fn blobfs(mut self, client: ClientEnd<fio::DirectoryMarker>) -> Self {
        self.blobfs = Some(client);
        self
    }

    /// Provide a TUF repository configuration to the package resolver.
    /// This will override the repository that the builder would otherwise generate.
    pub fn repo_config(mut self, repo: RepositoryConfigs) -> Self {
        self.repo_config = Some(repo);
        self
    }

    /// Enable/disable Omaha. OmahaState::Auto will automatically set up an Omaha server and tell
    /// the updater to use it.
    pub fn omaha_state(mut self, state: OmahaState) -> Self {
        self.omaha = state;
        self
    }

    /// Mutate the MockPaverServiecBuilder used by this TestEnvBuilder.
    pub fn paver<F>(mut self, func: F) -> Self
    where
        F: FnOnce(MockPaverServiceBuilder) -> MockPaverServiceBuilder,
    {
        self.paver = func(self.paver);
        self
    }

    pub fn test_executor(mut self, executor: Box<dyn TestExecutor<R>>) -> Self {
        self.test_executor = Some(executor);
        self
    }

    fn generate_packages(&self) -> String {
        json!({
            "version": "1",
            "content": self.packages
                .iter()
                .map(|p| format!("{}/{}/0?hash={}",
                                 TEST_REPO_URL,
                                 p.name(),
                                 p.meta_far_merkle_root()))
                .collect::<Vec<String>>(),
        })
        .to_string()
    }

    /// Turn this |TestEnvBuilder| into a |TestEnv|
    pub async fn build(mut self) -> Result<TestEnv<R>, Error> {
        let mut update = PackageBuilder::new("update")
            .add_resource_at("packages.json", self.generate_packages().as_bytes());

        let (repo_config, served_repo, ssl_certs, packages, merkle) = if self.repo_config.is_none()
        {
            // If no repo config was specified, host a repo containing the provided packages,
            // and an update package containing given images + all packages in the repo.
            for (name, data) in self.images.iter() {
                update = update.add_resource_at(name, data.as_slice());
            }

            let update = update.build().await.expect("build update package");
            let repo = Arc::new(
                self.packages
                    .iter()
                    .fold(
                        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH).add_package(&update),
                        |repo, package| repo.add_package(package),
                    )
                    .build()
                    .await
                    .expect("build repo"),
            );

            let served_repo = Arc::clone(&repo).server().start().expect("serve repo");
            let config = RepositoryConfigs::Version1(vec![
                served_repo.make_repo_config(TEST_REPO_URL.parse().expect("make repo config"))
            ]);

            let update_merkle = *update.meta_far_merkle_root();
            // Add the update package to the list of packages, so that TestResult::check_packages
            // will expect to see the update package's blobs in blobfs.
            let mut packages = vec![update];
            packages.append(&mut self.packages);
            (
                config,
                Some(served_repo),
                fuchsia_fs::directory::open_in_namespace(
                    TEST_CERTS_PATH,
                    fio::OpenFlags::RIGHT_READABLE,
                )
                .unwrap(),
                packages,
                update_merkle,
            )
        } else {
            // Use the provided repo config. Assume that this means we'll actually want to use
            // real SSL certificates, and that we don't need to host our own repository.
            (
                self.repo_config.unwrap(),
                None,
                fuchsia_fs::directory::open_in_namespace(
                    GLOBAL_SSL_CERTS_PATH,
                    fio::OpenFlags::RIGHT_READABLE,
                )
                .unwrap(),
                vec![],
                Hash::from_str("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
                    .expect("make merkle"),
            )
        };

        let dir = tempfile::tempdir()?;
        let mut path = dir.path().to_owned();
        path.push("repo_config.json");
        let path = path.as_path();
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(path).context("creating file")?);
        serde_json::to_writer(&mut file, &repo_config).unwrap();
        file.flush().unwrap();

        Ok(TestEnv {
            blobfs: self.blobfs,
            board: self.board,
            channel: self.channel,
            omaha: self.omaha,
            packages,
            paver: Arc::new(self.paver.build()),
            _repo: served_repo,
            repo_config_dir: dir,
            ssl_certs,
            update_merkle: merkle,
            version: self.version,
            test_executor: self.test_executor.expect("test executor must be set"),
        })
    }
}

pub struct TestEnv<R> {
    blobfs: Option<ClientEnd<fio::DirectoryMarker>>,
    channel: String,
    omaha: OmahaState,
    packages: Vec<Package>,
    paver: Arc<MockPaverService>,
    _repo: Option<ServedRepository>,
    repo_config_dir: tempfile::TempDir,
    ssl_certs: DirectoryProxy,
    update_merkle: Hash,
    board: String,
    version: String,
    test_executor: Box<dyn TestExecutor<R>>,
}

impl<R> TestEnv<R> {
    fn start_omaha(omaha: OmahaState, merkle: Hash) -> Result<Option<OmahaConfig>, Error> {
        match omaha {
            OmahaState::Disabled => Ok(None),
            OmahaState::Manual(cfg) => Ok(Some(cfg)),
            OmahaState::Auto(response) => {
                let server = OmahaServerBuilder::default()
                    .responses_by_appid(
                        vec![(
                            "integration-test-appid".to_string(),
                            ResponseAndMetadata { response, merkle, ..Default::default() },
                        )]
                        .into_iter()
                        .collect::<ResponseMap>(),
                    )
                    .build()
                    .unwrap();
                let addr = OmahaServer::start(Arc::new(Mutex::new(server)))
                    .context("Starting omaha server")?;
                let config =
                    OmahaConfig { app_id: "integration-test-appid".to_owned(), server_url: addr };

                Ok(Some(config))
            }
        }
    }

    /// Run the update, consuming this |TestEnv| and returning a |TestResult|.
    pub async fn run(self) -> R {
        let omaha_config = TestEnv::<R>::start_omaha(self.omaha, self.update_merkle)
            .expect("Starting Omaha server");

        let mut service_fs = ServiceFs::new();
        let paver_clone = Arc::clone(&self.paver);
        service_fs.add_fidl_service(move |stream: PaverRequestStream| {
            fasync::Task::spawn(
                Arc::clone(&paver_clone)
                    .run_paver_service(stream)
                    .unwrap_or_else(|e| panic!("Failed to run mock paver: {e:?}")),
            )
            .detach();
        });

        let (client, server) = fidl::endpoints::create_proxy::<fidl_fuchsia_io::DirectoryMarker>()
            .expect("creating channel for servicefs");
        service_fs
            .serve_connection(server.into_channel().into())
            .expect("Failed to serve connection");
        fasync::Task::spawn(service_fs.collect()).detach();

        // TODO(https://fxbug.dev/108786): Use Proxy::into_client_end when available.
        let client =
            ClientEnd::new(client.into_channel().expect("proxy into channel").into_zx_channel());

        let params = TestParams {
            blobfs: self.blobfs,
            board: self.board,
            channel: self.channel,
            packages: self.packages,
            paver: self.paver,
            repo_config_dir: self.repo_config_dir,
            ssl_certs: self.ssl_certs,
            update_merkle: self.update_merkle,
            version: self.version,
            omaha_config,
            paver_connector: client,
        };

        self.test_executor.run(params).await
    }
}
