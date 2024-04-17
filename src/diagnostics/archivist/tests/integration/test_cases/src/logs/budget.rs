// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use diagnostics_data::Logs;
use diagnostics_reader::{ArchiveReader, RetryConfig};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics as fdiagnostics;
use futures::StreamExt;

const SPAM_COUNT: usize = 9001;

#[fuchsia::test]
async fn test_budget() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![
            test_topology::PuppetDeclBuilder::new("spammer").into(),
            test_topology::PuppetDeclBuilder::new("victim").into(),
        ]),
        archivist_config: Some(ftest::ArchivistConfig {
            logs_max_cached_original_bytes: Some(3000),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .unwrap();

    let spammer_puppet = test_topology::connect_to_puppet(&realm_proxy, "spammer").await.unwrap();
    let victim_puppet = test_topology::connect_to_puppet(&realm_proxy, "victim").await.unwrap();
    spammer_puppet.wait_for_interest_change().await.unwrap();
    victim_puppet.wait_for_interest_change().await.unwrap();

    let letters = ('A'..'Z').map(|c| c.to_string()).collect::<Vec<_>>();
    let mut letters_iter = letters.iter().cycle();
    let expected = letters_iter.next().unwrap().repeat(50);
    victim_puppet
        .log(&ftest::LogPuppetLogRequest {
            severity: Some(fdiagnostics::Severity::Info),
            message: Some(expected.clone()),
            ..Default::default()
        })
        .await
        .expect("emitted log");

    let accessor =
        realm_proxy.connect_to_protocol::<fdiagnostics::ArchiveAccessorMarker>().await.unwrap();
    let mut log_reader = ArchiveReader::new();
    log_reader
        .with_archive(accessor)
        .with_minimum_schema_count(0) // we want this to return even when no log messages
        .retry(RetryConfig::never());

    let (mut observed_logs, _errors) =
        log_reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let (mut observed_logs_2, _errors) =
        log_reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();

    let msg_a = observed_logs.next().await.unwrap();
    let msg_a_2 = observed_logs_2.next().await.unwrap();
    assert_eq!(expected, msg_a.msg().unwrap());
    assert_eq!(expected, msg_a_2.msg().unwrap());

    // Spam many logs.
    for _ in 0..SPAM_COUNT {
        let message = letters_iter.next().unwrap().repeat(50);
        spammer_puppet
            .log(&ftest::LogPuppetLogRequest {
                severity: Some(fdiagnostics::Severity::Info),
                message: Some(message.clone()),
                ..Default::default()
            })
            .await
            .expect("emitted log");
        assert_eq!(message, observed_logs.next().await.unwrap().msg().unwrap());
    }

    // We observe some logs were rolled out.
    let log = observed_logs_2.skip(33).next().await.unwrap();
    assert_eq!(log.rolled_out_logs(), Some(8946));
    let mut observed_logs = log_reader.snapshot::<Logs>().await.unwrap().into_iter();
    let msg_b = observed_logs.next().unwrap();
    assert!(!msg_b.moniker.contains("puppet-victim"));

    // Vicitm logs should have been rolled out.
    let messages =
        observed_logs.filter(|log| log.moniker.contains("puppet-victim")).collect::<Vec<_>>();
    assert!(messages.is_empty());
    assert_ne!(msg_a.msg().unwrap(), msg_b.msg().unwrap());
}
