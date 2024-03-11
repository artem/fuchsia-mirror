// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    io_conformance_util::{test_harness::TestHarness, *},
};

#[fuchsia::test]
async fn set_attr_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_set_attr.unwrap_or_default() {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![file("file", vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        let file = open_file_with_flags(&test_dir, dir_flags, "file").await;

        let (status, old_attr) = file.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = file
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let (status, new_attr) = file.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn set_attr_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_set_attr.unwrap_or_default() {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_without(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![file("file", vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        let file = open_file_with_flags(&test_dir, dir_flags, "file").await;

        let status = file
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::BAD_HANDLE);
    }
}

#[fuchsia::test]
async fn set_attr_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_set_attr.unwrap_or_default() {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_with(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![directory("dir", vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        let dir = open_dir_with_flags(&test_dir, dir_flags, "dir").await;

        let (status, old_attr) = dir.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        // Set CREATION_TIME flag, but not MODIFICATION_TIME.
        let status = dir
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let (status, new_attr) = dir.get_attr().await.expect("get_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        // Check that only creation_time was updated.
        let expected = fio::NodeAttributes { creation_time: 111, ..old_attr };
        assert_eq!(new_attr, expected);
    }
}

#[fuchsia::test]
async fn set_attr_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_set_attr.unwrap_or_default() {
        return;
    }

    for dir_flags in harness.file_rights.valid_combos_without(fio::OpenFlags::RIGHT_WRITABLE) {
        let root = root_directory(vec![directory("dir", vec![])]);
        let test_dir = harness.get_directory(root, harness.dir_rights.all());
        let dir = open_dir_with_flags(&test_dir, dir_flags, "dir").await;

        let status = dir
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME,
                &fio::NodeAttributes {
                    creation_time: 111,
                    modification_time: 222,
                    ..EMPTY_NODE_ATTRS
                },
            )
            .await
            .expect("set_attr failed");
        assert_eq!(zx::Status::from_raw(status), zx::Status::BAD_HANDLE);
    }
}

#[fuchsia::test]
async fn get_attributes_empty_query() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    // Node attributes that were not requested should return None
    let (mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(fio::NodeAttributesQuery::empty())
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(
        mutable_attributes,
        fio::MutableNodeAttributes {
            creation_time: None,
            modification_time: None,
            mode: None,
            uid: None,
            gid: None,
            rdev: None,
            ..Default::default()
        }
    );
    assert_eq!(
        immutable_attributes,
        fio::ImmutableNodeAttributes {
            protocols: None,
            abilities: None,
            content_size: None,
            storage_size: None,
            link_count: None,
            id: None,
            ..Default::default()
        }
    );
}

#[fuchsia::test]
async fn get_attributes_return_some_value() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }
    const TEST_FILE_CONTENTS: &'static [u8] = b"test-file-contents";

    let root = root_directory(vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_owned())]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    // All of the attributes are requested. Filesystems are allowed to return None for attributes
    // they don't support.
    let (mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(fio::NodeAttributesQuery::all())
        .await
        .unwrap()
        .expect("get_attributes failed");

    // If ctime and mtime are supported then they shouldn't be 0.
    assert_matches!(mutable_attributes.creation_time, None | Some(1..));
    assert_matches!(mutable_attributes.modification_time, None | Some(1..));
    // The posix attributes weren't set so if the filesystem supports them then they should be 0.
    assert_matches!(mutable_attributes.mode, None | Some(0));
    assert_matches!(mutable_attributes.uid, None | Some(0));
    assert_matches!(mutable_attributes.gid, None | Some(0));
    assert_matches!(mutable_attributes.rdev, None | Some(0));

    assert_matches!(immutable_attributes.protocols, Some(fio::NodeProtocolKinds::FILE));
    assert_matches!(
        immutable_attributes.abilities,
        Some(x) if x.contains(fio::Operations::READ_BYTES | fio::Operations::GET_ATTRIBUTES)
    );
    assert_matches!(
        immutable_attributes.content_size,
        Some(x) if x == TEST_FILE_CONTENTS.len() as u64
    );
    assert!(immutable_attributes.storage_size.is_some());
    assert!(immutable_attributes.link_count.is_some());
    assert!(immutable_attributes.id.is_some());
}

#[fuchsia::test]
async fn get_attributes_file_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    assert_eq!(
        file_proxy.get_attributes(fio::NodeAttributesQuery::empty()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_file() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    let (_mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::CONTENT_SIZE,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::FILE);
    assert_eq!(immutable_attributes.content_size.unwrap(), TEST_FILE_CONTENTS.len() as u64);
}

#[fuchsia::test]
async fn update_attributes_file_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_update_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;

    // fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        file_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn update_attributes_file_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_update_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, TEST_FILE).await;

    let status = file_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw);
    assert_eq!(status, Err(zx::Status::BAD_HANDLE));
}

#[fuchsia::test]
async fn update_attributes_file_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default()
        || !harness.config.supports_update_attributes.unwrap_or_default()
    {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, TEST_FILE).await;

    let (mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS
                | fio::NodeAttributesQuery::CONTENT_SIZE
                | fio::NodeAttributesQuery::CREATION_TIME,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::FILE);
    assert_eq!(immutable_attributes.content_size.unwrap(), TEST_FILE_CONTENTS.len() as u64);

    let mut expected_attributes = fio::NodeAttributes2 { mutable_attributes, immutable_attributes };

    let _ = file_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            mode: Some(2),
            uid: Some(3),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS
                | fio::NodeAttributesQuery::CONTENT_SIZE
                | fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(expected_attributes.immutable_attributes, immutable_attributes);
    expected_attributes.mutable_attributes.modification_time = Some(111);
    expected_attributes.mutable_attributes.mode = Some(2);
    expected_attributes.mutable_attributes.uid = Some(3);
    assert_eq!(expected_attributes.mutable_attributes, mutable_attributes);

    // Change another mutable attribute (check that the other attributes remain unchanged)
    let _ = file_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            mode: Some(5),
            gid: Some(4),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS
                | fio::NodeAttributesQuery::CONTENT_SIZE
                | fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID
                | fio::NodeAttributesQuery::GID,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(expected_attributes.immutable_attributes, immutable_attributes);
    expected_attributes.mutable_attributes.gid = Some(4);
    expected_attributes.mutable_attributes.mode = Some(5);
    assert_eq!(expected_attributes.mutable_attributes, mutable_attributes);
}

#[fuchsia::test]
async fn get_attributes_file_node_reference_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    assert_eq!(
        file_proxy.get_attributes(fio::NodeAttributesQuery::empty()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_file_node_reference() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, TEST_FILE_CONTENTS.to_vec())]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;

    // fuchsia.io/Node.GetAttributes
    let (_mutable_attributes, immutable_attributes) = file_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::CONTENT_SIZE,
        )
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::FILE);
    assert_eq!(immutable_attributes.content_size.unwrap(), TEST_FILE_CONTENTS.len() as u64);
}

#[fuchsia::test]
async fn update_attributes_file_node_reference_not_allowed() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![file(TEST_FILE, vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let file_proxy =
        open_file_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, TEST_FILE).await;

    // Node references does not support fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        file_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::BAD_HANDLE.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_directory_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, "dir").await;

    assert_eq!(
        dir_proxy.get_attributes(fio::NodeAttributesQuery::empty()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_directory() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    let (_mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::DIRECTORY);
}

#[fuchsia::test]
async fn update_attributes_directory_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_update_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, "dir").await;

    // fuchsia.io/Node.UpdateAttributes
    assert_eq!(
        dir_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn update_attributes_directory_with_insufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_update_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_READABLE, "dir").await;

    let status = dir_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw);
    assert_eq!(status, Err(zx::Status::BAD_HANDLE));
}

#[fuchsia::test]
async fn update_attributes_directory_with_sufficient_rights() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default()
        || !harness.config.supports_update_attributes.unwrap_or_default()
    {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::RIGHT_WRITABLE, "dir").await;

    let (mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS | fio::NodeAttributesQuery::CREATION_TIME,
        )
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::DIRECTORY);

    let mut expected_attributes = fio::NodeAttributes2 { mutable_attributes, immutable_attributes };

    let _ = dir_proxy
        .update_attributes(&fio::MutableNodeAttributes {
            modification_time: Some(111),
            mode: Some(2),
            uid: Some(3),
            ..Default::default()
        })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS
                | fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");
    assert_eq!(expected_attributes.immutable_attributes, immutable_attributes);
    expected_attributes.mutable_attributes.modification_time = Some(111);
    expected_attributes.mutable_attributes.mode = Some(2);
    expected_attributes.mutable_attributes.uid = Some(3);
    assert_eq!(expected_attributes.mutable_attributes, mutable_attributes);

    // Change another mutable attribute (check that the other attributes remain unchanged)
    let _ = dir_proxy
        .update_attributes(&fio::MutableNodeAttributes { gid: Some(4), ..Default::default() })
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("update_attributes failed");

    let (mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(
            fio::NodeAttributesQuery::PROTOCOLS
                | fio::NodeAttributesQuery::CREATION_TIME
                | fio::NodeAttributesQuery::MODIFICATION_TIME
                | fio::NodeAttributesQuery::MODE
                | fio::NodeAttributesQuery::UID
                | fio::NodeAttributesQuery::GID,
        )
        .await
        .expect("FIDL call failed")
        .map_err(zx::Status::from_raw)
        .expect("get_attributes failed");

    assert_eq!(expected_attributes.immutable_attributes, immutable_attributes);
    expected_attributes.mutable_attributes.gid = Some(4);
    assert_eq!(expected_attributes.mutable_attributes, mutable_attributes);
}

#[fuchsia::test]
async fn get_attributes_directory_node_reference_unsupported() {
    let harness = TestHarness::new().await;
    if harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;

    // fuchsia.io/Node.GetAttributes
    assert_eq!(
        dir_proxy.get_attributes(fio::NodeAttributesQuery::empty()).await.unwrap(),
        Err(zx::Status::NOT_SUPPORTED.into_raw())
    );
}

#[fuchsia::test]
async fn get_attributes_directory_node_reference() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;

    // fuchsia.io/Node.GetAttributes
    let (_mutable_attributes, immutable_attributes) = dir_proxy
        .get_attributes(fio::NodeAttributesQuery::PROTOCOLS)
        .await
        .unwrap()
        .expect("get_attributes failed");
    assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::DIRECTORY);
}

#[fuchsia::test]
async fn update_attributes_directory_node_reference_not_allowed() {
    let harness = TestHarness::new().await;
    if !harness.config.supports_get_attributes.unwrap_or_default() {
        return;
    }

    let root = root_directory(vec![directory("dir", vec![])]);
    let test_dir = harness.get_directory(root, harness.dir_rights.all());
    let dir_proxy = open_dir_with_flags(&test_dir, fio::OpenFlags::NODE_REFERENCE, "dir").await;

    // Node reference doesn't allow for updating attributes
    assert_eq!(
        dir_proxy.update_attributes(&fio::MutableNodeAttributes::default()).await.unwrap(),
        Err(zx::Status::BAD_HANDLE.into_raw())
    );
}
