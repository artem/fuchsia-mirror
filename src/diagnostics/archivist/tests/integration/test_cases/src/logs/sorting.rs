// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use diagnostics_data::{Data, Logs};
use diagnostics_reader::{ArchiveReader, Subscription};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics as fdiagnostics;
use futures::StreamExt;
use realm_proxy_client::RealmProxyClient;

#[fuchsia::test]
async fn timestamp_sorting_for_batches() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![
            test_topology::PuppetDeclBuilder::new("tort").into(),
            test_topology::PuppetDeclBuilder::new("hare").into(),
        ]),
        ..Default::default()
    })
    .await
    .unwrap();

    let child_tort = test_topology::connect_to_puppet(&realm_proxy, "tort").await.unwrap();
    let child_hare = test_topology::connect_to_puppet(&realm_proxy, "hare").await.unwrap();
    child_tort.wait_for_interest_change().await.unwrap();
    child_hare.wait_for_interest_change().await.unwrap();

    let hare_times = (1_000, 10_000);
    let tort_times = (5_000, 15_000);

    // connect to log_sink and make sure we have a clean slate
    let mut early_listener = Listener::new(&realm_proxy).await;

    // Log to tortoise
    child_tort.log(&log_message(tort_times.0)).await.expect("logged");
    let mut expected_dump = vec![(tort_times.0, "tort".to_string())];
    early_listener.check_next(tort_times.0, "tort").await;
    check_log_snapshot(&realm_proxy, &expected_dump).await;

    // Log to hare
    child_hare.log(&log_message(hare_times.0)).await.expect("logged");
    expected_dump.push((hare_times.0, "hare".to_string()));
    expected_dump.sort_by_key(|(time, _)| *time);

    early_listener.check_next(hare_times.0, "hare").await;
    check_log_snapshot(&realm_proxy, &expected_dump).await;

    // start a new listener and make sure it gets backlog reversed from early listener
    let mut middle_listener = Listener::new(&realm_proxy).await;
    middle_listener.check_next(hare_times.0, "hare").await;
    middle_listener.check_next(tort_times.0, "tort").await;

    // send the second tortoise message and assert it's seen
    child_tort.log(&log_message(tort_times.1)).await.expect("logged");
    expected_dump.push((tort_times.1, "tort".to_string()));
    expected_dump.sort_by_key(|(time, _)| *time);
    early_listener.check_next(tort_times.1, "tort").await;
    middle_listener.check_next(tort_times.1, "tort").await;
    check_log_snapshot(&realm_proxy, &expected_dump).await;

    // send the second hare message and assert it's seen
    child_hare.log(&log_message(hare_times.1)).await.expect("logged");
    expected_dump.push((hare_times.1, "hare".to_string()));
    expected_dump.sort_by_key(|(time, _)| *time);
    early_listener.check_next(hare_times.1, "hare").await;
    middle_listener.check_next(hare_times.1, "hare").await;
    check_log_snapshot(&realm_proxy, &expected_dump).await;

    // listening after all messages were seen by archivist-for-embedding should be time-ordered
    let mut final_listener = Listener::new(&realm_proxy).await;
    final_listener.check_next(hare_times.0, "hare").await;
    final_listener.check_next(tort_times.0, "tort").await;
    final_listener.check_next(hare_times.1, "hare").await;
    final_listener.check_next(tort_times.1, "tort").await;
}

struct Listener {
    stream: Subscription<Data<Logs>>,
}

impl Listener {
    async fn new(realm_proxy: &RealmProxyClient) -> Self {
        let accessor =
            realm_proxy.connect_to_protocol::<fdiagnostics::ArchiveAccessorMarker>().await.unwrap();
        let stream = ArchiveReader::new()
            .with_archive(accessor)
            .snapshot_then_subscribe::<Logs>()
            .expect("snapshot then subscribe");
        Self { stream }
    }

    async fn check_next(&mut self, time: i64, expected_moniker: &str) {
        let log = self.stream.next().await.unwrap().unwrap();
        assert_eq!(log.msg().unwrap(), "timing log");
        assert_eq!(log.metadata.timestamp, time);
        assert_eq!(log.moniker, expected_moniker);
    }
}

fn log_message(time: i64) -> ftest::LogPuppetLogRequest {
    ftest::LogPuppetLogRequest {
        severity: Some(fdiagnostics::Severity::Info),
        time: Some(time),
        message: Some("timing log".into()),
        ..Default::default()
    }
}

async fn check_log_snapshot(realm_proxy: &RealmProxyClient, expected_dump: &Vec<(i64, String)>) {
    let accessor =
        realm_proxy.connect_to_protocol::<fdiagnostics::ArchiveAccessorMarker>().await.unwrap();
    let logs = ArchiveReader::new().with_archive(accessor).snapshot::<Logs>().await.unwrap();
    let result = logs
        .into_iter()
        .map(|log| (log.metadata.timestamp, log.moniker.clone()))
        .collect::<Vec<_>>();
    assert_eq!(result, *expected_dump);
}
