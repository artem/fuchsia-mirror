// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    async_trait::async_trait,
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased, Status},
    fxfs_testing::TestFixture,
    std::sync::Arc,
    syncio::{
        zxio, zxio_node_attr_has_t, zxio_node_attributes_t, OpenOptions, SeekOrigin, XattrSetMode,
        Zxio,
    },
    vfs::{
        directory::entry::{DirectoryEntry, EntryInfo},
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode},
        node::Node,
        path::Path,
        pseudo_directory,
        symlink::Symlink,
        ToObjectRequest,
    },
};

#[fuchsia::test]
async fn test_symlink() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let symlink_zxio =
            dir_zxio.create_symlink("symlink", b"target").expect("create_symlink failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");

        // Test some error cases
        assert_eq!(
            dir_zxio.create_symlink("symlink", b"target").expect_err("create symlink succeeded"),
            zx::Status::ALREADY_EXISTS
        );
        assert_eq!(
            dir_zxio.create_symlink("a/b", b"target").expect_err("create symlink succeeded"),
            zx::Status::INVALID_ARGS
        );
        assert_eq!(
            dir_zxio
                .create_symlink("symlink2", &vec![65; 300])
                .expect_err("create symlink succeeded"),
            zx::Status::BAD_PATH
        );

        let symlink_zxio =
            dir_zxio.open(fio::OpenFlags::RIGHT_READABLE, "symlink").expect("open failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_read_link_error() {
    struct ErrorSymlink;

    #[async_trait]
    impl Symlink for ErrorSymlink {
        async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
            Err(zx::Status::IO)
        }
    }

    #[async_trait]
    impl Node for ErrorSymlink {
        async fn get_attributes(
            &self,
            _requested_attributes: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, zx::Status> {
            unreachable!();
        }

        async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
            unreachable!();
        }
    }

    let dir = pseudo_directory! {
        "error_symlink" => Arc::new(ErrorSymlink),
    };

    let (dir_client, dir_server) = create_endpoints();
    let scope = ExecutionScope::new();
    dir.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        Path::dot(),
        dir_server,
    );

    fasync::unblock(|| {
        let dir_zxio =
            Zxio::create(dir_client.into_channel().into_handle()).expect("create failed");

        assert_eq!(
            dir_zxio
                .open(fio::OpenFlags::RIGHT_READABLE, "error_symlink")
                .expect_err("open succeeded"),
            zx::Status::IO
        );
    })
    .await;
}

#[fuchsia::test]
async fn test_xattr_dir() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            dir_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        assert_matches!(foo_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        foo_zxio.xattr_set(b"security.selinux", b"bar", XattrSetMode::Set).unwrap();

        assert_eq!(foo_zxio.xattr_get(b"security.selinux").unwrap(), b"bar");

        {
            let names = foo_zxio.xattr_list().unwrap();
            assert_eq!(names, vec![b"security.selinux".to_owned()]);
        }

        foo_zxio.xattr_remove(b"security.selinux").unwrap();

        assert_matches!(foo_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        {
            let names = foo_zxio.xattr_list().unwrap();
            assert_eq!(names, Vec::<Vec<u8>>::new());
        }
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_dir_multiple_attributes() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            dir_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let names = &[
            b"security.selinux".to_vec(),
            b"user.sha".to_vec(),
            b"fuchsia.merkle".to_vec(),
            b"starnix.mode".to_vec(),
            b"invalid in linux but fine for fuchsia!".to_vec(),
        ];
        let mut sorted_names = names.to_vec();
        sorted_names.sort();
        let values = &[
            b"important security attribute".to_vec(),
            b"abc1234".to_vec(),
            b"fffffffffff".to_vec(),
            b"drwxrwxrwx".to_vec(),
            b"\0\0 nulls are fine in the value \0\0\0".to_vec(),
        ];

        for (name, value) in names.iter().zip(values.iter()) {
            foo_zxio.xattr_set(&name, &value, XattrSetMode::Set).unwrap();
        }

        let mut listed_names = foo_zxio.xattr_list().unwrap();
        listed_names.sort();
        // Sort the two lists, because there isn't any guaranteed order they will come back from the
        // server.
        assert_eq!(listed_names, sorted_names);
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_symlink() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let symlink_zxio =
            dir_zxio.create_symlink("symlink", b"target").expect("create_symlink failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");

        assert_matches!(symlink_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        symlink_zxio.xattr_set(b"security.selinux", b"bar", XattrSetMode::Set).unwrap();

        assert_eq!(symlink_zxio.xattr_get(b"security.selinux").unwrap(), b"bar");

        {
            let names = symlink_zxio.xattr_list().unwrap();
            assert_eq!(names, vec![b"security.selinux".to_owned()]);
        }

        symlink_zxio.xattr_remove(b"security.selinux").unwrap();

        assert_matches!(symlink_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        {
            let names = symlink_zxio.xattr_list().unwrap();
            assert_eq!(names, Vec::<Vec<u8>>::new());
        }
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_file_large_attribute() {
    let fixture = TestFixture::new().await;

    let (foo_client, foo_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            foo_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(foo_client.into_handle()).expect("create failed");

        let value_len = fio::MAX_INLINE_ATTRIBUTE_VALUE as usize + 64;
        let value = std::iter::repeat(0xff).take(value_len).collect::<Vec<u8>>();
        foo_zxio.xattr_set(b"user.big_attribute", &value, XattrSetMode::Set).unwrap();

        assert_eq!(foo_zxio.xattr_get(b"user.big_attribute").unwrap(), value);
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        // Create a test directory.
        let test_dir = dir_zxio
            .open2(
                "test_dir",
                OpenOptions { mode: fio::OpenMode::AlwaysCreate, ..OpenOptions::directory(None) },
                None,
            )
            .expect("open2 failed");

        // Now create a file in that directory.
        let test_file = test_dir
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        // Open the directory without specifying any protocols and ask for attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t {
                protocols: true,
                abilities: true,
                id: true,
                link_count: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let test_dir = dir_zxio
            .open2("test_dir", OpenOptions::default(), Some(&mut attr))
            .expect("open2 failed");

        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_DIRECTORY);
        assert_eq!(
            attr.abilities,
            (fio::Operations::ENUMERATE
                | fio::Operations::TRAVERSE
                | fio::Operations::MODIFY_DIRECTORY
                | fio::Operations::GET_ATTRIBUTES
                | fio::Operations::UPDATE_ATTRIBUTES)
                .bits()
        );
        // Fxfs will always return a non-zero ID.
        assert_ne!(attr.id, 0);
        assert_eq!(attr.link_count, 2);

        // And now the file, and ask for attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t {
                protocols: true,
                abilities: true,
                id: true,
                content_size: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let _test_file = test_dir
            .open2("test_file", OpenOptions::default(), Some(&mut attr))
            .expect("open2 failed");
        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_FILE);
        assert_eq!(
            attr.abilities,
            (fio::Operations::READ_BYTES
                | fio::Operations::WRITE_BYTES
                | fio::Operations::GET_ATTRIBUTES
                | fio::Operations::UPDATE_ATTRIBUTES)
                .bits()
        );
        // Fxfs will always return a non-zero ID.
        assert_ne!(attr.id, 0);
        assert_eq!(attr.content_size, 5);

        // Create and open a symlink.
        test_dir.create_symlink("symlink", b"target").expect("create_symlink failed");

        let symlink =
            test_dir.open2("symlink", OpenOptions::default(), None).expect("open2 failed");
        assert_eq!(symlink.read_link().expect("read_link failed"), b"target");

        // Test file protocol flags.
        let test_file = test_dir
            .open2("test_file", OpenOptions::file(fio::FileProtocolFlags::APPEND), None)
            .expect("open2 failed");
        const APPEND_CONTENT: &[u8] = b" there";
        test_file.write(APPEND_CONTENT).expect("write failed");
        test_file.seek(SeekOrigin::Start, 0).expect("seek failed");
        let mut buf = [0; 20];
        assert_eq!(
            test_file.read(&mut buf).expect("read failed"),
            CONTENT.len() + APPEND_CONTENT.len()
        );
        assert_eq!(&buf[..CONTENT.len()], CONTENT);
        assert_eq!(&buf[CONTENT.len()..CONTENT.len() + APPEND_CONTENT.len()], APPEND_CONTENT);

        // TODO(fxbug.dev/127341): Test create attributes.
        // TODO(fxbug.dev/125830): Test for POSIX attributes.
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2_rights() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        // Create and write to a file.
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        // Check rights
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions { rights: fio::Operations::READ_BYTES, ..Default::default() },
                None,
            )
            .expect("open2 failed");
        let mut buf = [0; 20];
        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // Make sure we can't write to the file.
        assert_eq!(test_file.write(&buf).expect_err("write succeeded"), zx::Status::BAD_HANDLE);

        // Check rights inherited from directory.
        let dir = dir_zxio
            .open2(
                ".",
                OpenOptions { rights: fio::Operations::READ_BYTES, ..Default::default() },
                None,
            )
            .expect("open2 failed");

        let test_file = dir.open2("test_file", OpenOptions::default(), None).expect("open2 failed");

        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // Make sure we can't write to the file.
        assert_eq!(test_file.write(&buf).expect_err("write succeeded"), zx::Status::BAD_HANDLE);

        // Optional rights on directories.
        let test_dir = dir_zxio
            .open2(
                ".",
                OpenOptions {
                    rights: fio::Operations::READ_BYTES,
                    ..OpenOptions::directory(Some(fio::Operations::WRITE_BYTES))
                },
                None,
            )
            .expect("open2 failed");

        // Now make sure we can open and write to the test file.
        let test_file =
            test_dir.open2("test_file", OpenOptions::default(), None).expect("open2 failed");

        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // This time we should be able to write to the file.
        test_file.write(b"foo").expect("write failed");
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2_node() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        dir_zxio
            .open_node(".", fio::NodeProtocolFlags::MUST_BE_DIRECTORY, None)
            .expect("open_node failed");

        // Create a file.
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        assert_eq!(
            dir_zxio
                .open_node("test_file", fio::NodeProtocolFlags::MUST_BE_DIRECTORY, None,)
                .expect_err("open_node succeeded"),
            zx::Status::NOT_DIR
        );

        // Check we can get attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { protocols: true, content_size: true, ..Default::default() },
            ..Default::default()
        };

        dir_zxio
            .open_node("test_file", fio::NodeProtocolFlags::empty(), Some(&mut attr))
            .expect("open_node failed");

        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_FILE);
        assert_eq!(attr.content_size, 5);
    })
    .await;

    fixture.close().await;
}

// TODO(fxbug.dev/293943124): once fxfs supports allocate, use the test fixture.
struct AllocateFile {
    res: Result<(), Status>,
}

impl AllocateFile {
    fn new(res: Result<(), Status>) -> Arc<Self> {
        Arc::new(AllocateFile { res })
    }
}

impl DirectoryEntry for AllocateFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        _path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            object_request.spawn_connection(
                scope.clone(),
                self.clone(),
                flags,
                FidlIoConnection::create,
            )
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

#[async_trait]
impl FileIo for AllocateFile {
    async fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<u64, Status> {
        unimplemented!()
    }
    async fn write_at(&self, _offset: u64, _content: &[u8]) -> Result<u64, Status> {
        unimplemented!()
    }
    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        unimplemented!()
    }
}

#[async_trait]
impl vfs::node::Node for AllocateFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        unimplemented!()
    }
    async fn get_attributes(
        &self,
        _query: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        unimplemented!()
    }
}

#[async_trait]
impl File for AllocateFile {
    fn writable(&self) -> bool {
        true
    }
    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }
    async fn truncate(&self, _length: u64) -> Result<(), Status> {
        unimplemented!()
    }
    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, Status> {
        unimplemented!()
    }
    async fn get_size(&self) -> Result<u64, Status> {
        unimplemented!()
    }
    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        unimplemented!()
    }
    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        unimplemented!()
    }
    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        Ok(())
    }
    async fn allocate(
        &self,
        _offset: u64,
        _length: u64,
        _mode: fio::AllocateMode,
    ) -> Result<(), Status> {
        self.res
    }
}

#[fuchsia::test]
async fn test_allocate_file() {
    let dir = pseudo_directory! {
        "foo" => AllocateFile::new(Ok(())),
    };
    let (dir_client, dir_server) = create_endpoints();
    let scope = ExecutionScope::new();
    dir.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        Path::dot(),
        dir_server,
    );

    fasync::unblock(|| {
        let dir_zxio =
            Zxio::create(dir_client.into_channel().into_handle()).expect("create failed");
        let foo_zxio = dir_zxio
            .open(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE, "foo")
            .expect("open failed");

        foo_zxio.allocate(0, 10, syncio::AllocateMode::empty()).unwrap();
    })
    .await;
}

#[fuchsia::test]
async fn test_allocate_file_not_sup() {
    // For now we just tell it what error to send back, but this is mimicking the first pass at
    // allocate which won't support any options.
    let dir = pseudo_directory! {
        "foo" => AllocateFile::new(Err(Status::NOT_SUPPORTED)),
    };
    let (dir_client, dir_server) = create_endpoints();
    let scope = ExecutionScope::new();
    dir.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        Path::dot(),
        dir_server,
    );

    fasync::unblock(|| {
        let dir_zxio =
            Zxio::create(dir_client.into_channel().into_handle()).expect("create failed");
        let foo_zxio = dir_zxio
            .open(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE, "foo")
            .expect("open failed");

        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::COLLAPSE_RANGE),
            Err(zx::Status::NOT_SUPPORTED)
        );
        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::KEEP_SIZE),
            Err(zx::Status::NOT_SUPPORTED)
        );
        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::INSERT_RANGE),
            Err(zx::Status::NOT_SUPPORTED)
        );
        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::PUNCH_HOLE),
            Err(zx::Status::NOT_SUPPORTED)
        );
        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::UNSHARE_RANGE),
            Err(zx::Status::NOT_SUPPORTED)
        );
        assert_matches!(
            foo_zxio.allocate(0, 10, syncio::AllocateMode::ZERO_RANGE),
            Err(zx::Status::NOT_SUPPORTED)
        );
    })
    .await;
}

#[fuchsia::test]
async fn test_get_set_attributes_node() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        dir_zxio
            .open_node(".", fio::NodeProtocolFlags::MUST_BE_DIRECTORY, None)
            .expect("open_node failed");

        // Create a file.
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        let attr = zxio_node_attributes_t {
            gid: 111,
            access_time: 222,
            modification_time: 333,
            has: zxio_node_attr_has_t {
                gid: true,
                access_time: true,
                modification_time: true,
                ..Default::default()
            },
            ..Default::default()
        };

        test_file.attr_set(&attr).expect("attr_set failed");

        let query = zxio_node_attr_has_t {
            gid: true,
            access_time: true,
            modification_time: true,
            ..Default::default()
        };

        let attributes = test_file.attr_get(query).expect("attr_get failed");
        assert_eq!(attributes.gid, 111);
        assert_eq!(attributes.access_time, 222);
        assert_eq!(attributes.modification_time, 333);
    })
    .await;

    fixture.close().await;
}
