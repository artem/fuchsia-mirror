// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `triage-detect` is responsible for auto-triggering crash reports in Fuchsia.

mod delay_tracker;
mod diagnostics;
mod snapshot;
mod triage_shim;

use {
    anyhow::{bail, Error},
    argh::FromArgs,
    delay_tracker::DelayTracker,
    fidl_fuchsia_feedback::MAX_CRASH_SIGNATURE_LENGTH,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_inspect::{self as inspect, health::Reporter, NumericProperty},
    fuchsia_inspect_derive::{Inspect, WithInspect},
    fuchsia_triage::SnapshotTrigger,
    fuchsia_zircon as zx,
    futures::StreamExt,
    glob::glob,
    injectable_time::MonotonicTime,
    serde_derive::Deserialize,
    snapshot::SnapshotRequest,
    std::collections::HashMap,
    tracing::{error, info, warn},
    triage_detect_config::Config as ComponentConfig,
};

const MINIMUM_CHECK_TIME_NANOS: i64 = 60 * 1_000_000_000;
const CONFIG_GLOB: &str = "/config/data/*.triage";
const PROGRAM_CONFIG_PATH: &str = "/config/data/config.json";
const SIGNATURE_PREFIX: &str = "fuchsia-detect-";
const MINIMUM_SIGNATURE_INTERVAL_NANOS: i64 = 3600 * 1_000_000_000;

/// The name of the subcommand and the logs-tag.
pub const PROGRAM_NAME: &str = "detect";

/// Command line args
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "detect")]
pub struct CommandLine {}

#[derive(PartialEq, Debug, Clone, Copy)]
pub(crate) enum Mode {
    Test,
    Production,
}

#[derive(Deserialize, Default, Debug)]
struct ProgramConfig {
    enable_filing: Option<bool>,
}

fn load_program_config() -> Result<ProgramConfig, Error> {
    let config_text = match std::fs::read_to_string(PROGRAM_CONFIG_PATH) {
        Ok(text) => text,
        Err(_) => {
            info!("Program config file not found; using default config.");
            "{}".to_string()
        }
    };
    match serde_json5::from_str::<ProgramConfig>(&config_text) {
        Ok(config) => Ok(config),
        Err(e) => bail!("Program config format error: {}", e),
    }
}

fn load_configuration_files() -> Result<HashMap<String, String>, Error> {
    fn file_stem(file_path: &std::path::PathBuf) -> Result<String, Error> {
        if let Some(s) = file_path.file_stem() {
            if let Some(s) = s.to_str() {
                return Ok(s.to_owned());
            }
        }
        bail!("Bad path {:?} - can't find file_stem", file_path)
    }

    let mut file_contents = HashMap::new();
    for file_path in glob(CONFIG_GLOB)? {
        let file_path = file_path?;
        let stem = file_stem(&file_path)?;
        let config_text = std::fs::read_to_string(file_path)?;
        file_contents.insert(stem, config_text);
    }
    Ok(file_contents)
}

/// appropriate_check_interval determines the interval to check diagnostics, or signals error.
///
/// If the command line can't be evaluated to an integer, an error is returned.
/// If the integer is below minimum and mode isn't Test, an error is returned.
/// If a valid integer is determined, it is returned as a zx::Duration.
fn appropriate_check_interval(expression: &str, mode: &Mode) -> Result<zx::Duration, Error> {
    let check_every = triage_shim::evaluate_int_math(&expression)
        .or_else(|e| bail!("Check_every argument must be Minutes(n), Hours(n), etc. but: {}", e))?;
    if check_every < MINIMUM_CHECK_TIME_NANOS && *mode != Mode::Test {
        bail!(
            "Minimum time to check is {} seconds; {} nanos is too small",
            MINIMUM_CHECK_TIME_NANOS / 1_000_000_000,
            check_every
        );
    }
    info!(
        "Checking every {} seconds from command line '{:?}'",
        check_every / 1_000_000_000,
        expression
    );
    Ok(zx::Duration::from_nanos(check_every))
}

fn build_signature(snapshot: SnapshotTrigger, mode: Mode) -> String {
    // Character and length restrictions are documented in
    // https://fuchsia.dev/reference/fidl/fuchsia.feedback#CrashReport
    let sanitized: String = snapshot
        .signature
        .chars()
        .map(|char| match char {
            c if char.is_ascii_lowercase() => c,
            c if char.is_ascii_uppercase() => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    if sanitized != snapshot.signature {
        let message = format!("Signature {} was sanitized to {}", snapshot.signature, sanitized);
        if mode == Mode::Test {
            warn!("{}", message);
        } else {
            error!("{}", message);
        }
    }
    let mut signature = format!("{}{}", SIGNATURE_PREFIX, sanitized);
    if signature.len() > fidl_fuchsia_feedback::MAX_CRASH_SIGNATURE_LENGTH as usize {
        let new_signature =
            signature.chars().take(MAX_CRASH_SIGNATURE_LENGTH as usize).collect::<String>();
        let message = format!("Signature '{}' truncated to '{}'", signature, new_signature);
        if mode == Mode::Test {
            warn!("{}", message);
        } else {
            error!("{}", message);
        }
        signature = new_signature;
    }
    signature
}

// on_error logs any errors from `value` and then returns a Result.
// value must return a Result; error_message must contain one {} to put the error in.
macro_rules! on_error {
    ($value:expr, $error_message:expr) => {
        $value.or_else(|e| {
            let message = format!($error_message, e);
            warn!("{}", message);
            bail!("{}", message)
        })
    };
}

#[derive(Inspect, Default)]
struct Stats {
    scan_count: inspect::UintProperty,
    missed_deadlines: inspect::UintProperty,
    triage_warnings: inspect::UintProperty,
    issues_detected: inspect::UintProperty,
    issues_throttled: inspect::UintProperty,
    issues_send_count: inspect::UintProperty,
    issues_send_errors: inspect::UintProperty,
    inspect_node: fuchsia_inspect::Node,
}

impl Stats {
    fn new() -> Self {
        Self::default()
    }
}

pub async fn main() -> Result<(), Error> {
    let inspector = inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let mut service_fs = ServiceFs::new();
    service_fs.take_and_serve_directory_handle()?;
    fasync::Task::spawn(async move {
        service_fs.collect::<()>().await;
    })
    .detach();

    let component_config = ComponentConfig::take_from_startup_handle();
    inspector
        .root()
        .record_child("config", |config_node| component_config.record_inspect(config_node));

    let stats = Stats::new().with_inspect(inspector.root(), "stats")?;
    let mode = match component_config.test_only {
        true => Mode::Test,
        false => Mode::Production,
    };
    let check_every = on_error!(
        appropriate_check_interval(&component_config.check_every, &mode),
        "Invalid command line arg for check time: {}"
    )?;
    let program_config = on_error!(load_program_config(), "Error loading program config: {}")?;
    info!("Test mode: {:?}, program config: {:?}", mode, program_config);
    let configuration =
        on_error!(load_configuration_files(), "Error reading configuration files: {}")?;

    let triage_engine = on_error!(
        triage_shim::TriageLib::new(configuration),
        "Failed to parse Detect configuration files: {}"
    )?;
    info!("Loaded and parsed .triage files");

    let selectors = triage_engine.selectors();
    let mut diagnostic_source = diagnostics::DiagnosticFetcher::create(selectors)?;
    let snapshot_service =
        snapshot::CrashReportHandlerBuilder::new(MonotonicTime::new()).build().await?;
    let system_time = MonotonicTime::new();
    let mut delay_tracker = DelayTracker::new(&system_time, &mode);

    inspect::component::health().set_ok();

    // Start the first scan as soon as the program starts, via the "missed deadline" logic below.
    let mut next_check_time = fasync::Time::INFINITE_PAST;
    loop {
        if next_check_time < fasync::Time::now() {
            // We missed a deadline, so don't wait at all; start the check. But first
            // schedule the next check time at now() + check_every.
            if next_check_time != fasync::Time::INFINITE_PAST {
                stats.missed_deadlines.add(1);
                warn!(
                    "Missed diagnostic check deadline {:?} by {:?} nanos",
                    next_check_time,
                    fasync::Time::now() - next_check_time
                );
            }
            next_check_time = fasync::Time::now() + check_every;
        } else {
            // Wait until time for the next check.
            fasync::Timer::new(next_check_time).await;
            // Now it should be approximately next_check_time o'clock. To avoid drift from
            // delays, calculate the next check time by adding check_every to the current
            // next_check_time.
            next_check_time += check_every;
        }
        stats.scan_count.add(1);
        let diagnostics = diagnostic_source.get_diagnostics().await;
        let diagnostics = match diagnostics {
            Ok(diagnostics) => diagnostics,
            Err(e) => {
                // This happens when the integration tester runs out of Inspect data.
                if mode != Mode::Test {
                    error!("Fetching diagnostics failed: {}", e);
                }
                continue;
            }
        };

        let (snapshot_requests, warnings) = triage_engine.evaluate(diagnostics);
        stats.triage_warnings.add(warnings.len() as u64);

        for snapshot in snapshot_requests.into_iter() {
            stats.issues_detected.add(1);
            if delay_tracker.ok_to_send(&snapshot) {
                let signature = build_signature(snapshot, mode);
                if program_config.enable_filing == Some(true) {
                    stats.issues_send_count.add(1);
                    if let Err(e) =
                        snapshot_service.request_snapshot(SnapshotRequest::new(signature))
                    {
                        stats.issues_send_errors.add(1);
                        error!("Snapshot request failed: {}", e);
                    }
                } else {
                    warn!("Detect would have filed {}", signature);
                }
            } else {
                stats.issues_throttled.add(1);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn verify_appropriate_check_interval() -> Result<(), Error> {
        let error_a = "a".to_string();
        let error_empty = "".to_string();
        let raw_1 = "1".to_string();
        let raw_1_result = zx::Duration::from_nanos(1);
        let short_time = format!("Nanos({})", MINIMUM_CHECK_TIME_NANOS - 1);
        let short_time_result = zx::Duration::from_nanos(MINIMUM_CHECK_TIME_NANOS - 1);
        let minimum_time = format!("Nanos({})", MINIMUM_CHECK_TIME_NANOS);
        let minimum_time_result = zx::Duration::from_nanos(MINIMUM_CHECK_TIME_NANOS);
        let long_time = format!("Nanos({})", MINIMUM_CHECK_TIME_NANOS + 1);
        let long_time_result = zx::Duration::from_nanos(MINIMUM_CHECK_TIME_NANOS + 1);

        assert!(appropriate_check_interval(&error_a, &Mode::Test).is_err());
        assert!(appropriate_check_interval(&error_empty, &Mode::Test).is_err());
        assert_eq!(appropriate_check_interval(&raw_1, &Mode::Test)?, raw_1_result);
        assert_eq!(appropriate_check_interval(&short_time, &Mode::Test)?, short_time_result);
        assert_eq!(appropriate_check_interval(&minimum_time, &Mode::Test)?, minimum_time_result);
        assert_eq!(appropriate_check_interval(&long_time, &Mode::Test)?, long_time_result);

        assert!(appropriate_check_interval(&error_a, &Mode::Production).is_err());
        assert!(appropriate_check_interval(&error_empty, &Mode::Production).is_err());
        assert!(appropriate_check_interval(&raw_1, &Mode::Production).is_err());
        assert!(appropriate_check_interval(&short_time, &Mode::Production).is_err());
        assert_eq!(
            appropriate_check_interval(&minimum_time, &Mode::Production)?,
            minimum_time_result
        );
        assert_eq!(appropriate_check_interval(&long_time, &Mode::Test)?, long_time_result);
        assert_eq!(appropriate_check_interval(&long_time, &Mode::Production)?, long_time_result);
        Ok(())
    }

    #[fuchsia::test]
    fn verify_build_signature() {
        fn sig(signature: &str) -> fuchsia_triage::SnapshotTrigger {
            fuchsia_triage::SnapshotTrigger { interval: 0, signature: signature.to_string() }
        }

        let long_sig_input = "A very very long string that just keeps going and going and won't \
          quit no matter how long it goes and I don't know when it will ever stop";
        let desired_long_output = "fuchsia-detect-a-very-very-long-string-that-just-keeps-going-and-going-and-won-t-quit-no-matter-how-long-it-goes-and-i-don-t-kno";
        let long_sig_output = build_signature(sig(long_sig_input), Mode::Production);

        assert_eq!(long_sig_output, desired_long_output.to_string());
        assert_eq!(long_sig_output.len(), MAX_CRASH_SIGNATURE_LENGTH as usize);
        assert_eq!(
            build_signature(sig("Test lowercase"), Mode::Production),
            "fuchsia-detect-test-lowercase".to_string()
        );
    }
}
