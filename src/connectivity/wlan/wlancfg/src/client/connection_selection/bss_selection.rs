// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{connection_selection::scoring_functions, types},
        config_management::network_config::PastConnectionList,
        telemetry::{TelemetryEvent, TelemetrySender},
        util::pseudo_energy::SignalData,
    },
    fuchsia_inspect_contrib::{
        auto_persist::AutoPersist, inspect_insert, inspect_log, log::InspectList,
        nodes::BoundedListNode as InspectBoundedListNode,
    },
    futures::lock::Mutex,
    std::{cmp::Reverse, sync::Arc},
    tracing::{info, trace},
};

const LOCAL_ROAM_THRESHOLD_RSSI_2G: f64 = -72.0;
const LOCAL_ROAM_THRESHOLD_RSSI_5G: f64 = -75.0;
const LOCAL_ROAM_THRESHOLD_SNR_2G: f64 = 20.0;
const LOCAL_ROAM_THRESHOLD_SNR_5G: f64 = 17.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoamReason {
    RssiBelowThreshold,
    SnrBelowThreshold,
}

/// Aggregated information about the current BSS's connection quality, used for evaluation.
#[cfg_attr(test, derive(Debug, PartialEq))]
#[derive(Clone)]
pub struct BssQualityData {
    pub signal_data: SignalData,
    pub channel: types::WlanChan,
    // TX and RX rate, respectively.
    pub phy_rates: (u32, u32),
    // Connection data  of past successful connections to this BSS.
    pub past_connections_list: PastConnectionList,
}

impl BssQualityData {
    pub fn new(
        signal_data: SignalData,
        channel: types::WlanChan,
        past_connections_list: PastConnectionList,
    ) -> Self {
        BssQualityData { signal_data, channel, phy_rates: (0, 0), past_connections_list }
    }
}

/// BSS selection. Selects the best from a list of candidates that are available for
/// connection.
pub async fn select_bss(
    allowed_candidate_list: Vec<types::ScannedCandidate>,
    reason: types::ConnectReason,
    inspect_node: Arc<Mutex<AutoPersist<InspectBoundedListNode>>>,
    telemetry_sender: TelemetrySender,
) -> Option<types::ScannedCandidate> {
    if allowed_candidate_list.is_empty() {
        info!("No BSSs available to select from.");
    } else {
        info!("Selecting from {} BSSs found for allowed networks", allowed_candidate_list.len());
    }

    let mut inspect_node = inspect_node.lock().await;

    let mut scored_candidates = allowed_candidate_list
        .iter()
        .inspect(|&candidate| {
            info!("{}", candidate.to_string_without_pii());
        })
        .filter(|&candidate| {
            if !candidate.bss.is_compatible() {
                trace!("BSS is incompatible, filtering: {}", candidate.to_string_without_pii());
                false
            } else {
                true
            }
        })
        .map(|candidate| {
            (candidate.clone(), scoring_functions::score_bss_scanned_candidate(candidate.clone()))
        })
        .collect::<Vec<(types::ScannedCandidate, i16)>>();

    scored_candidates.sort_by_key(|(_, score)| Reverse(*score));
    let selected_candidate = scored_candidates.first();

    // Log the candidates into Inspect
    inspect_log!(
        inspect_node.get_mut(),
        candidates: InspectList(&allowed_candidate_list),
        selected?: selected_candidate.map(|(candidate, _)| candidate)
    );

    telemetry_sender.send(TelemetryEvent::BssSelectionResult {
        reason,
        scored_candidates: scored_candidates.clone(),
        selected_candidate: selected_candidate.cloned(),
    });

    if let Some((candidate, _)) = selected_candidate {
        info!("Selected BSS:");
        info!("{}", candidate.to_string_without_pii());
        Some(candidate.clone())
    } else {
        None
    }
}

// Returns a set of RoamReasons and a score (for metrics).
pub fn evaluate_current_bss(bss: &BssQualityData) -> (Vec<RoamReason>, u8) {
    let mut roam_reasons: Vec<RoamReason> = vec![];

    let (rssi_threshold, snr_threshold) = if bss.channel.is_5ghz() {
        (LOCAL_ROAM_THRESHOLD_RSSI_5G, LOCAL_ROAM_THRESHOLD_SNR_5G)
    } else {
        (LOCAL_ROAM_THRESHOLD_RSSI_2G, LOCAL_ROAM_THRESHOLD_SNR_2G)
    };

    if bss.signal_data.ewma_rssi.get() <= rssi_threshold {
        roam_reasons.push(RoamReason::RssiBelowThreshold)
    }
    if bss.signal_data.ewma_snr.get() <= snr_threshold {
        roam_reasons.push(RoamReason::SnrBelowThreshold)
    }

    let signal_score = scoring_functions::score_current_connection_signal_data(bss.signal_data);
    return (roam_reasons, signal_score);
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::{
            client::connection_selection::EWMA_VELOCITY_SMOOTHING_FACTOR,
            config_management::{ConnectFailure, FailureReason},
            util::testing::{
                create_inspect_persistence_channel, generate_channel,
                generate_random_bss_with_compatibility, generate_random_connect_reason,
                generate_random_scanned_candidate,
            },
        },
        diagnostics_assertions::{
            assert_data_tree, AnyBoolProperty, AnyNumericProperty, AnyProperty, AnyStringProperty,
        },
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_internal as fidl_internal,
        fuchsia_async as fasync, fuchsia_inspect as inspect,
        futures::channel::mpsc,
        ieee80211_testutils::{BSSID_REGEX, SSID_REGEX},
        rand::Rng,
        wlan_common::{assert_variant, channel, random_fidl_bss_description},
    };

    struct TestValues {
        inspector: inspect::Inspector,
        inspect_node: Arc<Mutex<AutoPersist<InspectBoundedListNode>>>,
        telemetry_sender: TelemetrySender,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
    }

    fn test_setup() -> TestValues {
        let inspector = inspect::Inspector::default();
        let inspect_node =
            InspectBoundedListNode::new(inspector.root().create_child("bss_select_test"), 10);
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let inspect_node = AutoPersist::new(inspect_node, "test", persistence_req_sender);
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);

        TestValues {
            inspector,
            inspect_node: Arc::new(Mutex::new(inspect_node)),
            telemetry_sender: TelemetrySender::new(telemetry_sender),
            telemetry_receiver,
        }
    }

    fn generate_candidate_for_scoring(
        rssi: i8,
        snr_db: i8,
        channel: types::WlanChan,
    ) -> types::ScannedCandidate {
        let bss = types::Bss {
            rssi,
            snr_db,
            channel: channel,
            bss_description: fidl_internal::BssDescription {
                rssi_dbm: rssi,
                snr_db,
                channel: channel.into(),
                ..random_fidl_bss_description!()
            }
            .into(),
            ..generate_random_bss_with_compatibility()
        };
        types::ScannedCandidate { bss, ..generate_random_scanned_candidate() }
    }

    fn connect_failure_with_bssid(bssid: types::Bssid) -> ConnectFailure {
        ConnectFailure {
            reason: FailureReason::GeneralFailure,
            time: fasync::Time::INFINITE,
            bssid,
        }
    }

    #[fuchsia::test]
    fn test_evaluate_trivial_roam_reasons() {
        // Low RSSI and SNR
        let weak_signal_bss = BssQualityData::new(
            SignalData::new(-90, 5, 10, EWMA_VELOCITY_SMOOTHING_FACTOR),
            channel::Channel::new(11, channel::Cbw::Cbw20),
            PastConnectionList::default(),
        );
        let (roam_reasons, _) = evaluate_current_bss(&weak_signal_bss);
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::SnrBelowThreshold));
        assert!(roam_reasons.iter().any(|&r| r == RoamReason::RssiBelowThreshold));
    }

    #[fuchsia::test]
    fn select_bss_sorts_by_score() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut candidates = vec![];

        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(36)));
        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-20, 30, generate_channel(1)));

        // there's a network on 5G, it should get a boost and be selected
        let reason = generate_random_connect_reason();
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                reason,
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[0].clone())
        );

        // make the 5GHz network into a 2.4GHz network
        let mut modified_network = candidates[0].clone();
        let modified_bss =
            types::Bss { channel: generate_channel(6), ..modified_network.bss.clone() };
        modified_network.bss = modified_bss;
        candidates[0] = modified_network;

        // all networks are 2.4GHz, strongest RSSI network returned
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                reason,
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[2].clone())
        );
    }

    #[fuchsia::test]
    fn select_bss_sorts_by_failure_count() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut candidates = vec![];

        candidates.push(generate_candidate_for_scoring(-25, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(1)));

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[0].clone()),
        );

        // mark the stronger network as having some failures
        let num_failures = 4;
        candidates[0].saved_network_info.recent_failures =
            vec![connect_failure_with_bssid(candidates[0].bss.bssid); num_failures];

        // weaker network (with no failures) returned
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[1].clone())
        );

        // give them both the same number of failures
        candidates[1].saved_network_info.recent_failures =
            vec![connect_failure_with_bssid(candidates[1].bss.bssid); num_failures];

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[0].clone())
        );
    }

    #[fuchsia::test]
    fn select_bss_ignore_incompatible() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut candidates = vec![];

        // Add two BSSs, both compatible to start.
        candidates.push(generate_candidate_for_scoring(-14, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-90, 30, generate_channel(1)));

        // The stronger BSS is selected initially.
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[0].clone())
        );

        // Make the stronger BSS incompatible.
        candidates[0].bss.compatibility = None;

        // The weaker, but still compatible, BSS is selected.
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[1].clone())
        );

        // TODO(https://fxbug.dev/120520): After `select_bss` filters out incompatible BSSs, this None
        // compatibility should change to a Some, to test that logic.
        // Make both BSSs incompatible.
        candidates[1].bss.compatibility = None;

        // No BSS is selected.
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            None
        );
    }

    #[fuchsia::test]
    fn select_bss_logs_to_inspect() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        let mut candidates = vec![];

        candidates.push(generate_candidate_for_scoring(-50, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-60, 30, generate_channel(3)));
        candidates.push(generate_candidate_for_scoring(-20, 30, generate_channel(6)));

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                candidates.clone(),
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            Some(candidates[2].clone())
        );

        let fidl_channel = fidl_common::WlanChannel::from(candidates[2].bss.channel);
        assert_data_tree!(test_values.inspector, root: {
            bss_select_test: {
                "0": {
                    "@time": AnyNumericProperty,
                    "candidates": {
                        "0": contains {
                            score: AnyNumericProperty,
                            bssid: &*BSSID_REGEX,
                            ssid: &*SSID_REGEX,
                            rssi: AnyNumericProperty,
                            security_type_saved: AnyStringProperty,
                            security_type_scanned: AnyStringProperty,
                            channel: {
                                cbw: AnyProperty,
                                primary: AnyNumericProperty,
                                secondary80: AnyNumericProperty,
                            },
                            compatible: AnyBoolProperty,
                            recent_failure_count: AnyNumericProperty,
                            saved_network_has_ever_connected: AnyBoolProperty,
                        },
                        "1": contains {
                            score: AnyProperty,
                        },
                        "2": contains {
                            score: AnyProperty,
                        },
                    },
                    "selected": {
                        ssid: candidates[2].network.ssid.to_string(),
                        bssid: candidates[2].bss.bssid.to_string(),
                        rssi: i64::from(candidates[2].bss.rssi),
                        score: i64::from(scoring_functions::score_bss_scanned_candidate(candidates[2].clone())),
                        security_type_saved: candidates[2].saved_security_type_to_string(),
                        security_type_scanned: format!("{}", wlan_common::bss::Protection::from(candidates[2].security_type_detailed)),
                        channel: {
                            cbw: AnyProperty,
                            primary: u64::from(fidl_channel.primary),
                            secondary80: u64::from(fidl_channel.secondary80),
                        },
                        compatible: candidates[2].bss.compatibility.is_some(),
                        recent_failure_count: candidates[2].recent_failure_count(),
                        saved_network_has_ever_connected: candidates[2].saved_network_info.has_ever_connected,
                    },
                }
            },
        });
    }

    #[fuchsia::test]
    fn select_bss_empty_list_logs_to_inspect() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = test_setup();
        assert_eq!(
            exec.run_singlethreaded(select_bss(
                vec![],
                generate_random_connect_reason(),
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            )),
            None
        );

        // Verify that an empty list of candidates is still logged to inspect.
        assert_data_tree!(test_values.inspector, root: {
            bss_select_test: {
                "0": {
                    "@time": AnyProperty,
                    "candidates": {},
                }
            },
        });
    }

    #[fuchsia::test]
    fn select_bss_logs_cobalt_metrics() {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = test_setup();

        let reason_code = generate_random_connect_reason();
        let candidates =
            vec![generate_random_scanned_candidate(), generate_random_scanned_candidate()];
        assert!(exec
            .run_singlethreaded(select_bss(
                candidates.clone(),
                reason_code,
                test_values.inspect_node.clone(),
                test_values.telemetry_sender.clone()
            ))
            .is_some());

        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::BssSelectionResult {
                reason,
                scored_candidates,
                selected_candidate: _,
            } => {
                assert_eq!(reason, reason_code);
                let mut prior_score = i16::MAX;
                for (candidate, score) in scored_candidates {
                    assert!(candidates.contains(&candidate));
                    assert!(prior_score >= score);
                    prior_score = score;
                }
            })
        });
    }
}
