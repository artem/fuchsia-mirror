// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{constants::*, test_topology};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fidl_fuchsia_diagnostics as fdiagnostics;
use futures::StreamExt;

// This test verifies that Archivist knows about logging from this component.
#[fuchsia::test]
async fn log_attribution() {
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create base topology");
    test_topology::add_eager_child(&test_realm, "child", STUB_INSPECT_COMPONENT_URL)
        .await
        .expect("add child");

    let instance = builder.build().await.expect("create instance");

    let accessor = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fdiagnostics::ArchiveAccessorMarker>()
        .unwrap();
    let mut result = ArchiveReader::new()
        .with_archive(accessor)
        .snapshot_then_subscribe::<Logs>()
        .expect("snapshot then subscribe");

    for log_str in &["This is a syslog message", "This is another syslog message"] {
        let log_record = result.next().await.expect("received log").expect("log is not an error");

        assert_eq!(log_record.moniker, "child");
        assert_eq!(log_record.metadata.severity, Severity::Info);
        assert_data_tree!(log_record.payload.unwrap(), root: contains {
            message: {
              value: log_str.to_string(),
            }
        });
    }
}
