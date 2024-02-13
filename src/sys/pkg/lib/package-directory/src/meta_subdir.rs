// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{meta_file::MetaFile, root_dir::RootDir},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::Arc,
    vfs::{
        common::send_on_open_with_error,
        directory::{
            entry::EntryInfo, immutable::connection::ImmutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        immutable_attributes,
        path::Path as VfsPath,
        ToObjectRequest,
    },
};

#[cfg(feature = "supports_open2")]
use vfs::{ObjectRequestRef, ProtocolsExt as _};

pub(crate) struct MetaSubdir<S: crate::NonMetaStorage> {
    root_dir: Arc<RootDir<S>>,
    // The object relative path expression of the subdir relative to the package root with a
    // trailing slash appended.
    path: String,
}

impl<S: crate::NonMetaStorage> MetaSubdir<S> {
    pub(crate) fn new(root_dir: Arc<RootDir<S>>, path: String) -> Arc<Self> {
        Arc::new(MetaSubdir { root_dir, path })
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry::DirectoryEntry for MetaSubdir<S> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: VfsPath,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags = flags & !(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);

        if path.is_empty() {
            flags.to_object_request(server_end).handle(|object_request| {
                if flags.intersects(
                    fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::RIGHT_EXECUTABLE
                        | fio::OpenFlags::CREATE
                        | fio::OpenFlags::CREATE_IF_ABSENT
                        | fio::OpenFlags::TRUNCATE
                        | fio::OpenFlags::APPEND,
                ) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }

                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
            });
            return;
        }

        // <path as vfs::path::Path>::as_str() is an object relative path expression [1], except
        // that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let file_path = format!(
            "{}{}",
            self.path,
            path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref())
        );

        if let Some(location) = self.root_dir.meta_files.get(&file_path).copied() {
            let () = MetaFile::new(self.root_dir.clone(), location).open(
                scope,
                flags,
                VfsPath::dot(),
                server_end,
            );
            return;
        }

        let directory_path = file_path + "/";
        for k in self.root_dir.meta_files.keys() {
            if k.starts_with(&directory_path) {
                let () = MetaSubdir::new(self.root_dir.clone(), directory_path).open(
                    scope,
                    flags,
                    VfsPath::dot(),
                    server_end,
                );
                return;
            }
        }

        let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_FOUND);
    }

    #[cfg(feature = "supports_open2")]
    fn open2(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: VfsPath,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if path.is_empty() {
            match protocols.open_mode() {
                fio::OpenMode::OpenExisting => {}
                fio::OpenMode::AlwaysCreate | fio::OpenMode::MaybeCreate => {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
            }

            if let Some(rights) = protocols.rights() {
                if rights.intersects(fio::Operations::WRITE_BYTES)
                    | rights.intersects(fio::Operations::EXECUTE)
                {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
            }
            // Note that `ImmutableConnection::create` will check that protocols contain
            // directory-only protocols.
            return object_request.spawn_connection(
                scope,
                self,
                protocols,
                ImmutableConnection::create,
            );
        }

        // <path as vfs::path::Path>::as_str() is an object relative path expression [1], except
        // that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let file_path = format!(
            "{}{}",
            self.path,
            path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref())
        );

        if let Some(location) = self.root_dir.meta_files.get(&file_path).copied() {
            return MetaFile::new(self.root_dir.clone(), location).open2(
                scope,
                VfsPath::dot(),
                protocols,
                object_request,
            );
        }

        let directory_path = file_path + "/";
        for k in self.root_dir.meta_files.keys() {
            if k.starts_with(&directory_path) {
                return MetaSubdir::new(self.root_dir.clone(), directory_path).open2(
                    scope,
                    VfsPath::dot(),
                    protocols,
                    object_request,
                );
            }
        }

        Err(zx::Status::NOT_FOUND)
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

#[async_trait]
impl<S: crate::NonMetaStorage> vfs::node::Node for MetaSubdir<S> {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        let size = crate::usize_to_u64_safe(self.root_dir.meta_files.len());
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY
                | vfs::common::rights_to_posix_mode_bits(
                    true, // read
                    true, // write
                    true, // execute
                ),
            id: 1,
            content_size: size,
            storage_size: size,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let size = crate::usize_to_u64_safe(self.root_dir.meta_files.len());
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE,
                content_size: size,
                storage_size: size,
                link_count: 1,
                id: 1,
            }
        ))
    }
}

#[async_trait]
impl<S: crate::NonMetaStorage> vfs::directory::entry_container::Directory for MetaSubdir<S> {
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<(dyn vfs::directory::dirents_sink::Sink + 'static)>,
    ) -> Result<
        (TraversalPosition, Box<(dyn vfs::directory::dirents_sink::Sealed + 'static)>),
        zx::Status,
    > {
        vfs::directory::read_dirents::read_dirents(
            &crate::get_dir_children(
                self.root_dir.meta_files.keys().map(|s| s.as_str()),
                &self.path,
            ),
            pos,
            sink,
        )
        .await
    }

    fn register_watcher(
        self: Arc<Self>,
        _: ExecutionScope,
        _: fio::WatchMask,
        _: vfs::directory::entry_container::DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    // `register_watcher` is unsupported so no need to do anything here.
    fn unregister_watcher(self: Arc<Self>, _: usize) {}
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_pkg_testing::{blobfs::Fake as FakeBlobfs, PackageBuilder},
        futures::prelude::*,
        std::convert::TryInto as _,
        vfs::{
            directory::{entry::DirectoryEntry, entry_container::Directory},
            node::Node,
        },
    };

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn new() -> (Self, Arc<MetaSubdir<blobfs::Client>>) {
            let pkg = PackageBuilder::new("pkg")
                .add_resource_at("meta/dir/dir/file", &b"contents"[..])
                .build()
                .await
                .unwrap();
            let (metafar_blob, _) = pkg.contents();
            let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
            blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
            let root_dir = RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap();
            let sub_dir = MetaSubdir::new(root_dir, "meta/dir/".to_string());
            (Self { _blobfs_fake: blobfs_fake }, sub_dir)
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_unsets_posix_flags() {
        let (_env, sub_dir) = TestEnv::new().await;

        let () = crate::verify_open_adjusts_flags(
            sub_dir,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_disallowed_flags() {
        let (_env, sub_dir) = TestEnv::new().await;

        for forbidden_flag in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
        ] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            DirectoryEntry::open(
                sub_dir.clone(),
                ExecutionScope::new(),
                fio::OpenFlags::DESCRIBE | forbidden_flag,
                VfsPath::dot(),
                server_end.into_channel().into(),
            );

            assert_matches!(
                proxy.take_event_stream().next().await,
                Some(Ok(fio::DirectoryEvent::OnOpen_{ s, info: None}))
                    if s == zx::Status::NOT_SUPPORTED.into_raw()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_self() {
        let (_env, sub_dir) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        DirectoryEntry::open(
            sub_dir,
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![fuchsia_fs::directory::DirEntry {
                name: "dir".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory
            }]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_file() {
        let (_env, sub_dir) = TestEnv::new().await;

        for path in ["dir/file", "dir/file/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            DirectoryEntry::open(
                sub_dir.clone(),
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_directory() {
        let (_env, sub_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            DirectoryEntry::open(
                sub_dir.clone(),
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![fuchsia_fs::directory::DirEntry {
                    name: "file".to_string(),
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_entry_info() {
        let (_env, sub_dir) = TestEnv::new().await;

        assert_eq!(
            DirectoryEntry::entry_info(sub_dir.as_ref()),
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_read_dirents() {
        let (_env, sub_dir) = TestEnv::new().await;

        let (pos, sealed) = Directory::read_dirents(
            sub_dir.as_ref(),
            &TraversalPosition::Start,
            Box::new(crate::tests::FakeSink::new(3)),
        )
        .await
        .expect("read_dirents failed");
        assert_eq!(
            crate::tests::FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("dir".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_register_watcher_not_supported() {
        let (_env, sub_dir) = TestEnv::new().await;

        let (_client, server) = fidl::endpoints::create_endpoints();

        assert_eq!(
            Directory::register_watcher(
                sub_dir,
                ExecutionScope::new(),
                fio::WatchMask::empty(),
                server.try_into().unwrap(),
            ),
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_get_attrs() {
        let (_env, sub_dir) = TestEnv::new().await;

        assert_eq!(
            Node::get_attrs(sub_dir.as_ref()).await.unwrap(),
            fio::NodeAttributes {
                mode: fio::MODE_TYPE_DIRECTORY | 0o700,
                id: 1,
                content_size: 4,
                storage_size: 4,
                link_count: 1,
                creation_time: 0,
                modification_time: 0,
            }
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_get_attributes() {
        let (_env, sub_dir) = TestEnv::new().await;

        assert_eq!(
            Node::get_attributes(sub_dir.as_ref(), fio::NodeAttributesQuery::all()).await.unwrap(),
            immutable_attributes!(
                fio::NodeAttributesQuery::all(),
                Immutable {
                    protocols: fio::NodeProtocolKinds::DIRECTORY,
                    abilities: fio::Operations::GET_ATTRIBUTES
                        | fio::Operations::ENUMERATE
                        | fio::Operations::TRAVERSE,
                    content_size: 4,
                    storage_size: 4,
                    link_count: 1,
                    id: 1,
                }
            )
        );
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_self() {
        let (_env, sub_dir) = TestEnv::new().await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        let scope = ExecutionScope::new();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            ..Default::default()
        });
        protocols
            .to_object_request(server_end)
            .handle(|req| DirectoryEntry::open2(sub_dir, scope, VfsPath::dot(), protocols, req));

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![fuchsia_fs::directory::DirEntry {
                name: "dir".to_string(),
                kind: fuchsia_fs::directory::DirentKind::Directory
            }]
        );
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_file() {
        let (_env, sub_dir) = TestEnv::new().await;

        for path in ["dir/file", "dir/file/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            });
            let path = VfsPath::validate_and_split(path).unwrap();
            protocols
                .to_object_request(server_end)
                .handle(|req| DirectoryEntry::open2(sub_dir.clone(), scope, path, protocols, req));

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_directory() {
        let (_env, sub_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            });
            let path = VfsPath::validate_and_split(path).unwrap();
            protocols
                .to_object_request(server_end)
                .handle(|req| DirectoryEntry::open2(sub_dir.clone(), scope, path, protocols, req));

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![fuchsia_fs::directory::DirEntry {
                    name: "file".to_string(),
                    kind: fuchsia_fs::directory::DirentKind::File
                }]
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_open_modes() {
        let (_env, sub_dir) = TestEnv::new().await;

        for forbidden_open_mode in [fio::OpenMode::AlwaysCreate, fio::OpenMode::MaybeCreate] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                mode: Some(forbidden_open_mode),
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(sub_dir.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_rights() {
        let (_env, sub_dir) = TestEnv::new().await;

        for forbidden_rights in [fio::Operations::WRITE_BYTES, fio::Operations::EXECUTE] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(forbidden_rights),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(sub_dir.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_file_protocols() {
        let (_env, sub_dir) = TestEnv::new().await;

        for file_protocols in [fio::FileProtocolFlags::APPEND, fio::FileProtocolFlags::TRUNCATE] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols {
                    file: Some(file_protocols),
                    ..Default::default()
                }),
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(sub_dir.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FILE, .. })
            );
        }
    }
}
