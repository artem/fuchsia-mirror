// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    run_command,
    tests::utils::{
        inspect_accessor_data, make_inspect_with_length, make_inspects,
        make_inspects_for_lifecycle, setup_fake_archive_accessor, setup_fake_rcs,
    },
};
use errors::ResultExt as _;
use ffx_writer::{Format, MachineWriter, TestBuffers};
use fidl_fuchsia_diagnostics::{ClientSelectorConfiguration, SelectorArgument};
use iquery::commands::ShowCommand;

#[fuchsia::test]
async fn test_show_no_parameters() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ShowCommand { manifest: None, selectors: vec![], accessor: None };
    let mut inspects = make_inspects();
    let inspect_data =
        inspect_accessor_data(ClientSelectorConfiguration::SelectAll(true), inspects.clone());
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![inspect_data]),
        ShowCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    inspects.sort_by(|a, b| a.moniker.cmp(&b.moniker));
    let expected = serde_json::to_string(&inspects).unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}

#[fuchsia::test]
async fn test_show_unknown_manifest() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ShowCommand {
        manifest: Some(String::from("some-bad-moniker")),
        selectors: vec![],
        accessor: None,
    };
    let lifecycle_data = inspect_accessor_data(
        ClientSelectorConfiguration::SelectAll(true),
        make_inspects_for_lifecycle(),
    );
    let inspects = make_inspects();
    let inspect_data =
        inspect_accessor_data(ClientSelectorConfiguration::SelectAll(true), inspects.clone());
    assert!(run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![lifecycle_data, inspect_data]),
        ShowCommand::from(cmd),
        &mut writer
    )
    .await
    .unwrap_err()
    .ffx_error()
    .is_some());
}

#[fuchsia::test]
async fn test_show_with_manifest_that_exists() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd =
        ShowCommand { manifest: Some(String::from("moniker1")), selectors: vec![], accessor: None };
    let lifecycle_data = inspect_accessor_data(
        ClientSelectorConfiguration::SelectAll(true),
        make_inspects_for_lifecycle(),
    );
    let mut inspects = vec![
        make_inspect_with_length(String::from("test/moniker1"), 1, 20),
        make_inspect_with_length(String::from("test/moniker1"), 3, 10),
        make_inspect_with_length(String::from("test/moniker1"), 6, 30),
    ];
    let inspect_data = inspect_accessor_data(
        ClientSelectorConfiguration::Selectors(vec![SelectorArgument::RawSelector(String::from(
            "test/moniker1:root",
        ))]),
        inspects.clone(),
    );
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![lifecycle_data, inspect_data]),
        ShowCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    inspects.sort_by(|a, b| a.moniker.cmp(&b.moniker));
    let expected = serde_json::to_string(&inspects).unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}

#[fuchsia::test]
async fn test_show_with_selectors_with_no_data() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ShowCommand {
        manifest: None,
        selectors: vec![String::from("test/moniker1:name:hello_not_real")],
        accessor: None,
    };
    let lifecycle_data = inspect_accessor_data(
        ClientSelectorConfiguration::SelectAll(true),
        make_inspects_for_lifecycle(),
    );
    let inspect_data = inspect_accessor_data(
        ClientSelectorConfiguration::Selectors(vec![SelectorArgument::RawSelector(String::from(
            "test/moniker1:name:hello_not_real",
        ))]),
        vec![],
    );
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![lifecycle_data, inspect_data]),
        ShowCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    let expected = String::from("[]");
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}

#[fuchsia::test]
async fn test_show_with_selectors_with_data() {
    let test_buffers = TestBuffers::default();
    let mut writer = MachineWriter::new_test(Some(Format::Json), &test_buffers);
    let cmd = ShowCommand {
        manifest: None,
        selectors: vec![String::from("test/moniker1:name:hello_6")],
        accessor: None,
    };
    let lifecycle_data = inspect_accessor_data(
        ClientSelectorConfiguration::SelectAll(true),
        make_inspects_for_lifecycle(),
    );
    let mut inspects = vec![make_inspect_with_length(String::from("test/moniker1"), 6, 30)];
    let inspect_data = inspect_accessor_data(
        ClientSelectorConfiguration::Selectors(vec![SelectorArgument::RawSelector(String::from(
            "test/moniker1:name:hello_6",
        ))]),
        inspects.clone(),
    );
    run_command(
        setup_fake_rcs(),
        setup_fake_archive_accessor(vec![lifecycle_data, inspect_data]),
        ShowCommand::from(cmd),
        &mut writer,
    )
    .await
    .unwrap();

    inspects.sort_by(|a, b| a.moniker.cmp(&b.moniker));
    let expected = serde_json::to_string(&inspects).unwrap();
    let output = test_buffers.into_stdout_str();
    assert_eq!(output.trim_end(), expected);
}
