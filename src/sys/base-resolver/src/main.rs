// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context},
    fidl::endpoints::{ClientEnd, Proxy},
    fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_io::{self as fio, DirectoryProxy},
    fidl_fuchsia_mem as fmem,
    fidl_fuchsia_sys2::{self as fsys, ComponentResolverRequest, ComponentResolverRequestStream},
    fuchsia_component::server::ServiceFs,
    fuchsia_url::{
        errors::{ParseError as PkgUrlParseError, ResourcePathError},
        pkg_url::PkgUrl,
    },
    fuchsia_zircon::Status,
    futures::prelude::*,
    log::*,
    thiserror::Error,
};

mod pkg_cache_resolver;

#[fuchsia::component]
async fn main() -> anyhow::Result<()> {
    info!("started");

    let mut service_fs = ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(Services::BaseResolver);
    service_fs.dir("pkg-cache-resolver").add_fidl_service(Services::PkgCacheResolver);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    let () = service_fs
        .for_each_concurrent(None, |request| async {
            match request {
                Services::BaseResolver(stream) => {
                    serve(stream)
                        .unwrap_or_else(|e| {
                            error!("failed to serve base resolver request: {:#}", e)
                        })
                        .await
                }
                Services::PkgCacheResolver(stream) => {
                    pkg_cache_resolver::serve(stream)
                        .unwrap_or_else(|e| {
                            error!("failed to serve pkg cache resolver request: {:#}", e)
                        })
                        .await
                }
            }
        })
        .await;

    Ok(())
}

enum Services {
    BaseResolver(ComponentResolverRequestStream),
    PkgCacheResolver(ComponentResolverRequestStream),
}

async fn serve(mut stream: ComponentResolverRequestStream) -> anyhow::Result<()> {
    let packages_dir = io_util::open_directory_in_namespace(
        "/pkgfs/packages",
        fio::OPEN_RIGHT_READABLE | fio::OPEN_RIGHT_EXECUTABLE,
    )
    .context("failed to open /pkgfs")?;
    while let Some(ComponentResolverRequest::Resolve { component_url, responder }) =
        stream.try_next().await.context("failed to read request from FIDL stream")?
    {
        match resolve_component(&component_url, &packages_dir).await {
            Ok(result) => responder.send(&mut Ok(result)),
            Err(err) => {
                let fidl_err = (&err).into();
                error!(
                    "failed to resolve component URL {}: {:#}",
                    &component_url,
                    anyhow::anyhow!(err)
                );
                responder.send(&mut Err(fidl_err))
            }
        }
        .context("failed sending response")?;
    }
    Ok(())
}

async fn resolve_component(
    component_url: &str,
    packages_dir: &DirectoryProxy,
) -> Result<fsys::Component, ResolverError> {
    let package_url = PkgUrl::parse(component_url)?;
    let cm_path = package_url.resource().ok_or_else(|| {
        ResolverError::InvalidUrl(PkgUrlParseError::InvalidResourcePath(
            ResourcePathError::PathIsEmpty,
        ))
    })?;
    let package_dir = resolve_package(&package_url, packages_dir).await?;

    let data =
        get_data_from_package_path(&package_dir, cm_path, ResolverError::ComponentNotFound).await?;
    let raw_bytes = raw_bytes_from_data(&data)?;
    let decl: fdecl::Component = fidl::encoding::decode_persistent(&raw_bytes[..])
        .map_err(ResolverError::ParsingManifest)?;

    let config_values = if let Some(config_decl) = decl.config.as_ref() {
        // if we have a config declaration, we need to read the value file from the package dir
        let strategy =
            config_decl.value_source.as_ref().ok_or(ResolverError::InvalidConfigSource)?;
        let config_path = match strategy {
            fdecl::ConfigValueSource::PackagePath(path) => path,
            other => return Err(ResolverError::UnsupportedConfigSource(other.to_owned())),
        };
        Some(
            get_data_from_package_path(
                &package_dir,
                &config_path,
                ResolverError::ConfigValuesNotFound,
            )
            .await?,
        )
    } else {
        None
    };

    let package_dir = ClientEnd::new(
        package_dir.into_channel().expect("could not convert proxy to channel").into_zx_channel(),
    );
    Ok(fsys::Component {
        resolved_url: Some(component_url.into()),
        decl: Some(data),
        package: Some(fsys::Package {
            package_url: Some(package_url.root_url().to_string()),
            package_dir: Some(package_dir),
            ..fsys::Package::EMPTY
        }),
        config_values,
        ..fsys::Component::EMPTY
    })
}

async fn resolve_package(
    package_url: &PkgUrl,
    packages_dir: &DirectoryProxy,
) -> Result<DirectoryProxy, ResolverError> {
    let root_url = package_url.root_url();
    if root_url.host() != "fuchsia.com" {
        return Err(ResolverError::UnsupportedRepo);
    }
    if root_url.package_hash().is_some() {
        return Err(ResolverError::PackageHashNotSupported);
    }
    let package_name = io_util::canonicalize_path(root_url.path());
    // Package contents are available at `packages/$PACKAGE_NAME/0`.
    let dir = io_util::directory::open_directory(
        packages_dir,
        &format!("{}/0", package_name),
        fio::OPEN_RIGHT_READABLE | fio::OPEN_RIGHT_EXECUTABLE,
    )
    .await
    .map_err(ResolverError::PackageNotFound)?;
    Ok(dir)
}

async fn get_data_from_package_path(
    package: &fio::DirectoryProxy,
    cm_path: &str,
    map_not_found_err: impl FnOnce(io_util::node::OpenError) -> ResolverError,
) -> Result<fmem::Data, ResolverError> {
    let cm_file = io_util::directory::open_file(&package, cm_path, fio::OPEN_RIGHT_READABLE)
        .await
        .map_err(map_not_found_err)?;
    let (status, buffer) =
        cm_file.get_buffer(fio::VMO_FLAG_READ).await.map_err(ResolverError::IOError)?;
    let () = Status::ok(status).map_err(ResolverError::VmoFailure)?;
    Ok(match buffer {
        Some(buffer) => fmem::Data::Buffer(*buffer),
        None => fmem::Data::Bytes(
            io_util::file::read(&cm_file).await.map_err(ResolverError::ReadManifest)?,
        ),
    })
}

fn raw_bytes_from_data(data: &fmem::Data) -> Result<Vec<u8>, ResolverError> {
    Ok(match data {
        fmem::Data::Buffer(buf) => {
            let size = buf.size as usize;
            let mut raw_bytes = Vec::with_capacity(size);
            raw_bytes.resize(size, 0);
            buf.vmo.read(&mut raw_bytes, 0).map_err(ResolverError::VmoFailure)?;
            raw_bytes
        }
        fmem::Data::Bytes(b) => b.clone(),
        _ => return Err(ResolverError::UnrecognizedDataVariant),
    })
}

#[derive(Error, Debug)]
enum ResolverError {
    #[error("invalid component URL")]
    InvalidUrl(#[from] PkgUrlParseError),
    #[error("component URL with package hash not supported")]
    PackageHashNotSupported,
    #[error("the hostname refers to an unsupported repo")]
    UnsupportedRepo,
    #[error("component not found")]
    ComponentNotFound(#[source] io_util::node::OpenError),
    #[error("package not found")]
    PackageNotFound(#[source] io_util::node::OpenError),
    #[error("couldn't parse component manifest")]
    ParsingManifest(#[source] fidl::Error),
    #[error("couldn't find config values")]
    ConfigValuesNotFound(#[source] io_util::node::OpenError),
    #[error("config source missing or invalid")]
    InvalidConfigSource,
    #[error("unsupported config source: {:?}", _0)]
    UnsupportedConfigSource(fdecl::ConfigValueSource),
    #[error("unrecognized fuchsia.mem.Data variant")]
    UnrecognizedDataVariant,
    #[error("read manifest error")]
    ReadManifest(#[source] io_util::file::ReadError),
    #[error("IO error")]
    IOError(#[source] fidl::Error),
    #[error("failed to get manifest VMO")]
    VmoFailure(#[source] Status),
    #[error("failed to create FIDL endpoints")]
    CreateEndpoints(#[source] fidl::Error),
    #[error("serve package directory")]
    ServePackageDirectory(#[source] package_directory::Error),
}

impl From<&ResolverError> for fsys::ResolverError {
    fn from(err: &ResolverError) -> fsys::ResolverError {
        use ResolverError::*;
        match err {
            InvalidUrl(_) | PackageHashNotSupported => fsys::ResolverError::InvalidArgs,
            UnsupportedRepo => fsys::ResolverError::NotSupported,
            ComponentNotFound(_) => fsys::ResolverError::ManifestNotFound,
            PackageNotFound(_) => fsys::ResolverError::PackageNotFound,
            ConfigValuesNotFound(_) => fsys::ResolverError::ConfigValuesNotFound,
            ParsingManifest(_)
            | UnsupportedConfigSource(_)
            | UnrecognizedDataVariant
            | InvalidConfigSource => fsys::ResolverError::InvalidManifest,
            ReadManifest(_)
            | IOError(_)
            | VmoFailure(_)
            | CreateEndpoints(_)
            | ServePackageDirectory(_) => fsys::ResolverError::Io,
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fake_pkgfs::{Entry, MockDir, MockFile},
        fidl::encoding::encode_persistent,
        fidl::endpoints::{create_proxy, ServerEnd},
        fidl::prelude::*,
        fidl_fuchsia_component_config as fconfig, fidl_fuchsia_component_decl as fdecl,
        fidl_fuchsia_io::{DirectoryMarker, DirectoryObject, NodeInfo, NodeMarker},
        std::sync::Arc,
    };

    /// A DirectoryEntry implementation that checks whether an expected set of flags
    /// are set in the Open request.
    struct FlagVerifier(u32);

    impl Entry for FlagVerifier {
        fn open(
            self: Arc<Self>,
            flags: u32,
            _mode: u32,
            _path: &str,
            server_end: ServerEnd<NodeMarker>,
        ) {
            let status = if flags & self.0 != self.0 { Status::INVALID_ARGS } else { Status::OK };
            let stream = server_end.into_stream().expect("failed to create stream");
            let control_handle = stream.control_handle();
            control_handle
                .send_on_open_(
                    status.into_raw(),
                    Some(&mut NodeInfo::Directory(DirectoryObject {})),
                )
                .expect("failed to send OnOpen event");
            control_handle.shutdown_with_epitaph(status);
        }
    }

    fn serve_pkgfs(pseudo_dir: Arc<dyn Entry>) -> Result<DirectoryProxy, anyhow::Error> {
        let (proxy, server_end) = create_proxy::<DirectoryMarker>()
            .context("failed to create DirectoryProxy/Server pair")?;
        pseudo_dir.open(
            fio::OPEN_RIGHT_READABLE | fio::OPEN_RIGHT_EXECUTABLE,
            fio::MODE_TYPE_DIRECTORY,
            ".",
            ServerEnd::new(server_end.into_channel()),
        );
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn resolves_package_with_executable_rights() {
        let pkg_url = PkgUrl::new_package("fuchsia.com".into(), "/test-package".into(), None)
            .expect("failed to create test PkgUrl");
        let flag_verifier =
            Arc::new(FlagVerifier(fio::OPEN_RIGHT_READABLE | fio::OPEN_RIGHT_EXECUTABLE));
        let pkgfs_dir = serve_pkgfs(Arc::new(
            MockDir::new()
                .add_entry("test-package", Arc::new(MockDir::new().add_entry("0", flag_verifier))),
        ))
        .expect("failed to serve pkgfs");

        let package =
            resolve_package(&pkg_url, &pkgfs_dir).await.expect("failed to resolve package");
        let event_stream = package.take_event_stream().map_ok(|_| ());
        assert_matches!(
            event_stream.try_collect::<()>().await,
            Err(fidl::Error::ClientChannelClosed { status: Status::OK, .. })
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_package_unsupported_repo() {
        let pkg_url = PkgUrl::new_package("fuchsia.ca".into(), "/test-package".into(), None)
            .expect("failed to create test PkgUrl");
        let pkgfs_dir = serve_pkgfs(Arc::new(MockDir::new())).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_package(&pkg_url, &pkgfs_dir).await,
            Err(ResolverError::UnsupportedRepo)
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_invalid_url() {
        let pkgfs_dir = serve_pkgfs(Arc::new(MockDir::new())).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_component("fuchsia://fuchsia.com/foo#meta/bar.cm", &pkgfs_dir).await,
            Err(ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/foo", &pkgfs_dir).await,
            Err(ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/#meta/bar.cm", &pkgfs_dir).await,
            Err(ResolverError::InvalidUrl(_))
        );
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.ca/foo#meta/bar.cm", &pkgfs_dir).await,
            Err(ResolverError::UnsupportedRepo)
        );

        let url_with_hash = concat!(
            "fuchsia-pkg://fuchsia.com/test-package",
            "?hash=f241b31d5913b66c90a44d44537d6bec62672e1f05dbc4c4f22b863b01c68749",
            "#meta/test.cm"
        );
        assert_matches!(
            resolve_component(url_with_hash, &pkgfs_dir).await,
            Err(ResolverError::PackageHashNotSupported)
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_package_not_found() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/missing-package#meta/foo.cm", &pkgfs_dir)
                .await,
            Err(ResolverError::PackageNotFound(_))
        );
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_missing_manifest() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/test-package#meta/bar.cm", &pkgfs_dir)
                .await,
            Err(ResolverError::ComponentNotFound(_))
        );
    }

    #[fuchsia::test]
    async fn resolves_component_vmo_manifest() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/test-package#meta/vmo.cm", &pkgfs_dir)
                .await,
            Ok(fsys::Component { decl: Some(fmem::Data::Buffer(_)), .. })
        );
    }

    #[fuchsia::test]
    async fn resolves_component_file_manifest() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).expect("failed to serve pkgfs");
        assert_matches!(
            resolve_component("fuchsia-pkg://fuchsia.com/test-package#meta/foo.cm", &pkgfs_dir)
                .await,
            Ok(fsys::Component {
                decl: Some(fidl_fuchsia_mem::Data::Buffer(fidl_fuchsia_mem::Buffer { .. })),
                ..
            })
        );
    }

    #[fuchsia::test]
    async fn resolves_component_with_config() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).unwrap();
        let component = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-config.cm",
            &pkgfs_dir,
        )
        .await
        .unwrap();
        assert_matches!(component, fsys::Component { decl: Some(..), config_values: Some(..), .. });
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_missing_config_values() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).unwrap();
        let error = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-without-config.cm",
            &pkgfs_dir,
        )
        .await
        .unwrap_err();
        assert_matches!(error, ResolverError::ConfigValuesNotFound(..));
    }

    #[fuchsia::test]
    async fn fails_to_resolve_component_bad_config_source() {
        let pkgfs_dir = serve_pkgfs(build_fake_pkgfs()).unwrap();
        let error = resolve_component(
            "fuchsia-pkg://fuchsia.com/test-package#meta/foo-with-bad-config.cm",
            &pkgfs_dir,
        )
        .await
        .unwrap_err();
        assert_matches!(error, ResolverError::InvalidConfigSource);
    }

    fn build_fake_pkgfs() -> Arc<MockDir> {
        let cm_bytes = encode_persistent(&mut fdecl::Component::EMPTY.clone())
            .expect("failed to encode ComponentDecl FIDL");
        Arc::new(
            MockDir::new().add_entry(
                "test-package",
                Arc::new(
                    MockDir::new().add_entry(
                        "0",
                        Arc::new(
                            MockDir::new().add_entry(
                                "meta",
                                Arc::new(
                                    MockDir::new()
                                        .add_entry(
                                            "foo.cm",
                                            Arc::new(MockFile::new(cm_bytes.clone())),
                                        )
                                        .add_entry(
                                            "foo-with-config.cm",
                                            Arc::new(MockFile::new(
                                                encode_persistent(&mut fdecl::Component {
                                                    config: Some(fdecl::Config {
                                                        value_source: Some(
                                                            fdecl::ConfigValueSource::PackagePath(
                                                                "meta/foo-with-config.cvf"
                                                                    .to_string(),
                                                            ),
                                                        ),
                                                        ..fdecl::Config::EMPTY
                                                    }),
                                                    ..fdecl::Component::EMPTY
                                                })
                                                .unwrap(),
                                            )),
                                        )
                                        .add_entry(
                                            "foo-with-config.cvf",
                                            Arc::new(MockFile::new(
                                                encode_persistent(&mut fconfig::ValuesData {
                                                    ..fconfig::ValuesData::EMPTY
                                                })
                                                .unwrap(),
                                            )),
                                        )
                                        .add_entry(
                                            "foo-with-bad-config.cm",
                                            Arc::new(MockFile::new(
                                                encode_persistent(&mut fdecl::Component {
                                                    config: Some(fdecl::Config {
                                                        ..fdecl::Config::EMPTY
                                                    }),
                                                    ..fdecl::Component::EMPTY
                                                })
                                                .unwrap(),
                                            )),
                                        )
                                        .add_entry(
                                            "foo-without-config.cm",
                                            Arc::new(MockFile::new(
                                                encode_persistent(&mut fdecl::Component {
                                                    config: Some(fdecl::Config {
                                                        value_source: Some(
                                                            fdecl::ConfigValueSource::PackagePath(
                                                                "doesnt-exist.cvf".to_string(),
                                                            ),
                                                        ),
                                                        ..fdecl::Config::EMPTY
                                                    }),
                                                    ..fdecl::Component::EMPTY
                                                })
                                                .unwrap(),
                                            )),
                                        )
                                        .add_entry("vmo.cm", Arc::new(MockFile::new(cm_bytes))),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        )
    }
}
