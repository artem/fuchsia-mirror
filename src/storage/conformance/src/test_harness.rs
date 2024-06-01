// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::flags::Rights, fidl::endpoints::create_proxy, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio, fidl_fuchsia_io_test as io_test,
    fuchsia_zircon as zx,
};

/// Helper struct for connecting to an io1 test harness and running a conformance test on it.
pub struct TestHarness {
    /// FIDL proxy to the io1 test harness.
    pub proxy: io_test::Io1HarnessProxy,

    /// Config for the filesystem.
    pub config: io_test::Io1Config,

    /// All [`io_test::Directory`] rights supported by the filesystem.
    pub dir_rights: Rights,

    /// All [`io_test::File`] rights supported by the filesystem.
    pub file_rights: Rights,

    /// All [`io_test::ExecutableFile`] rights supported by the filesystem.
    pub executable_file_rights: Rights,
}

impl TestHarness {
    /// Connects to the test harness and returns a `TestHarness` struct.
    pub async fn new() -> TestHarness {
        let proxy = connect_to_harness().await;
        let config = proxy.get_config().await.expect("Could not get config from proxy");

        // Validate configuration options for consistency, disallow invalid combinations.
        if config.supports_modify_directory {
            assert!(
                config.supports_get_token,
                "GetToken must be supported for testing Rename/Link!"
            );
        }

        // Generate set of supported open rights for each object type.
        let dir_rights = Rights::new(get_supported_dir_rights(&config));
        let file_rights =
            Rights::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let executable_file_rights =
            Rights::new(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE);

        TestHarness { proxy, config, dir_rights, file_rights, executable_file_rights }
    }

    /// Creates a [`fio::DirectoryProxy`] with the given root directory structure.
    pub fn get_directory(
        &self,
        root: io_test::Directory,
        flags: fio::OpenFlags,
    ) -> fio::DirectoryProxy {
        let (client, server) = create_proxy::<fio::DirectoryMarker>().expect("Cannot create proxy");
        self.proxy
            .get_directory(root, flags, server)
            .expect("Cannot get directory from test harness");
        client
    }

    /// Returns the abilities [`io_test::File`] objects should have for the harness.
    pub fn supported_file_abilities(&self) -> fio::Abilities {
        fio::Abilities::READ_BYTES
            | fio::Abilities::WRITE_BYTES
            | fio::Abilities::GET_ATTRIBUTES
            | fio::Abilities::UPDATE_ATTRIBUTES
    }

    /// Returns the abilities [`io_test::Directory`] objects should have for the harness.
    pub fn supported_dir_abilities(&self) -> fio::Abilities {
        fio::Abilities::GET_ATTRIBUTES
            | fio::Abilities::UPDATE_ATTRIBUTES
            | fio::Abilities::ENUMERATE
            | fio::Abilities::TRAVERSE
            | fio::Abilities::MODIFY_DIRECTORY
    }

    /// Returns true if the harness supports at least one mutable attribute, false otherwise.
    ///
    /// *NOTE*: To allow testing both the io1 SetAttrs and io2 UpdateAttributes methods, harnesses
    /// that support mutable attributes must support [`fio::NodeAttributesQuery::CREATION_TIME`]
    /// and [`fio::NodeAttributesQuery::MODIFICATION_TIME`].
    pub fn supports_mutable_attrs(&self) -> bool {
        let all_mutable_attrs: fio::NodeAttributesQuery = fio::NodeAttributesQuery::ACCESS_TIME
            | fio::NodeAttributesQuery::MODIFICATION_TIME
            | fio::NodeAttributesQuery::CREATION_TIME
            | fio::NodeAttributesQuery::MODE
            | fio::NodeAttributesQuery::GID
            | fio::NodeAttributesQuery::UID
            | fio::NodeAttributesQuery::RDEV;
        if self.config.supported_attributes.intersects(all_mutable_attrs) {
            assert!(
                self.config.supported_attributes.contains(
                    fio::NodeAttributesQuery::CREATION_TIME
                        | fio::NodeAttributesQuery::MODIFICATION_TIME
                ),
                "Harnesses must support at least CREATION_TIME if attributes are mutable."
            );
            return true;
        }
        false
    }
}

async fn connect_to_harness() -> io_test::Io1HarnessProxy {
    // Connect to the realm to get acccess to the outgoing directory for the harness.
    let (client, server) = zx::Channel::create();
    fuchsia_component::client::connect_channel_to_protocol::<fcomponent::RealmMarker>(server)
        .expect("Cannot connect to Realm service");
    let realm = fcomponent::RealmSynchronousProxy::new(client);
    // fs_test is the name of the child component defined in the manifest.
    let child_ref = fdecl::ChildRef { name: "fs_test".to_string(), collection: None };
    let (client, server) = zx::Channel::create();
    realm
        .open_exposed_dir(
            &child_ref,
            fidl::endpoints::ServerEnd::<fio::DirectoryMarker>::new(server),
            zx::Time::INFINITE,
        )
        .expect("FIDL error when binding to child in Realm")
        .expect("Cannot bind to test harness child in Realm");

    let exposed_dir = fio::DirectoryProxy::new(fidl::AsyncChannel::from_channel(client));

    fuchsia_component::client::connect_to_protocol_at_dir_root::<io_test::Io1HarnessMarker>(
        &exposed_dir,
    )
    .expect("Cannot connect to test harness protocol")
}

/// Returns the aggregate of all io1 rights that are supported for [`io_test::Directory`] objects.
fn get_supported_dir_rights(config: &io_test::Io1Config) -> fio::OpenFlags {
    fio::OpenFlags::RIGHT_READABLE
        | fio::OpenFlags::RIGHT_WRITABLE
        | if config.supports_executable_file {
            fio::OpenFlags::RIGHT_EXECUTABLE
        } else {
            fio::OpenFlags::empty()
        }
}
