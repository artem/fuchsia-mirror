// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;
use argh::{ArgsInfo, FromArgs, TopLevelCommand};
use chrono::{DateTime, Local};
use chrono_english::{parse_date_string, Dialect};
use component_debug::query::get_instances_from_query;
use diagnostics_data::Severity;
use errors::{ffx_bail, FfxError};
use fidl_fuchsia_diagnostics::{LogInterestSelector, LogSettingsProxy};
use fidl_fuchsia_sys2::RealmQueryProxy;
use log_formatter::FormatterError;
use moniker::Moniker;
use selectors::{sanitize_moniker_for_selectors, SelectorExt};
use std::{
    borrow::Cow, io::Write, ops::Deref, str::FromStr, string::FromUtf8Error, time::Duration,
};
use thiserror::Error;
pub mod filter;
pub mod log_formatter;
pub mod log_socket_stream;

// Subcommand for ffx log (either watch or dump).
#[derive(ArgsInfo, FromArgs, Clone, PartialEq, Debug)]
#[argh(subcommand)]
pub enum LogSubCommand {
    Watch(WatchCommand),
    Dump(DumpCommand),
}

#[derive(ArgsInfo, FromArgs, Clone, PartialEq, Debug)]
/// Watches for and prints logs from a target. Default if no sub-command is specified.
#[argh(subcommand, name = "watch")]
pub struct WatchCommand {}

#[derive(ArgsInfo, FromArgs, Clone, PartialEq, Debug)]
/// Dumps all log from a given target's session.
#[argh(subcommand, name = "dump")]
pub struct DumpCommand {}

pub fn parse_time(value: &str) -> Result<DetailedDateTime, String> {
    let d = parse_date_string(value, Local::now(), Dialect::Us)
        .map(|time| DetailedDateTime { time, is_now: value == "now" })
        .map_err(|e| format!("invalid date string: {}", e));
    d
}

/// Parses a duration from a string. The input is in seconds
/// and the output is a Rust duration.
pub fn parse_seconds_string_as_duration(value: &str) -> Result<Duration, String> {
    Ok(Duration::from_secs(
        value.parse().map_err(|e| format!("value '{}' is not a number: {}", value, e))?,
    ))
}

// Time format for displaying logs
#[derive(Clone, Debug, PartialEq)]
pub enum TimeFormat {
    // UTC time
    Utc,
    // Local time
    Local,
    // Monotonic time
    Monotonic,
}

impl std::str::FromStr for TimeFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "local" => Ok(TimeFormat::Local),
            "utc" => Ok(TimeFormat::Utc),
            "monotonic" => Ok(TimeFormat::Monotonic),
            _ => Err(format!(
                "'{}' is not a valid value: must be one of 'local', 'utc', 'monotonic'",
                s
            )),
        }
    }
}

/// Date/time structure containing a "now"
/// field, set if it should be interpreted as the
/// current time (used to call Subscribe instead of SnapshotThenSubscribe).
#[derive(PartialEq, Clone, Debug)]
pub struct DetailedDateTime {
    /// The absolute timestamp as specified by the user
    /// or the current timestamp if 'now' is specified.
    pub time: DateTime<Local>,
    /// Whether or not the DateTime was "now".
    /// If the DateTime is "now", logs will be collected in subscribe
    /// mode, instead of SnapshotThenSubscribe.
    pub is_now: bool,
}

impl Deref for DetailedDateTime {
    type Target = DateTime<Local>;

    fn deref(&self) -> &Self::Target {
        &self.time
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum SymbolizeMode {
    /// Disable all symbolization
    Off,
    /// Use prettified symbolization
    Pretty,
    /// Use classic (non-prettified) symbolization
    Classic,
}

impl SymbolizeMode {
    pub fn is_prettification_disabled(&self) -> bool {
        matches!(self, SymbolizeMode::Classic)
    }

    pub fn is_symbolize_disabled(&self) -> bool {
        matches!(self, SymbolizeMode::Off)
    }
}

#[derive(ArgsInfo, FromArgs, Clone, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "log",
    description = "Display logs from a target device",
    note = "Logs are retrieve from the target at the moment this command is called.

You may see some additional information attached to the log line:

- `dropped=N`: this means that N logs attributed to the component were dropped when the component
  wrote to the log socket. This can happen when archivist cannot keep up with the rate of logs being
  emitted by the component and the component filled the log socket buffer in the kernel.

- `rolled=N`: this means that N logs rolled out from the archivist buffer and ffx never saw them.
  This can happen when more logs are being ingested by the archivist across all components and the
  ffx couldn't retrieve them fast enough.

Symbolization is performed in the background using the symbolizer host tool. You can pass
additional arguments to the symbolizer tool (for example, to add a remote symbol server) using:
  $ ffx config set proactive_log.symbolize.extra_args \"--symbol-server gs://some-url/path --symbol-server gs://some-other-url/path ...\"

To learn more about configuring the log viewer, visit https://fuchsia.dev/fuchsia-src/development/tools/ffx/commands/log",
    example = "\
Dump the most recent logs and stream new ones as they happen:
  $ ffx log

Stream new logs starting from the current time, filtering for severity of at least \"WARN\":
  $ ffx log --severity warn --since now

Stream logs where the source moniker, component url and message do not include \"sys\":
  $ ffx log --exclude sys

Stream ERROR logs with source moniker, component url or message containing either
\"netstack\" or \"remote-control.cm\", but not containing \"sys\":
  $ ffx log --severity error --filter netstack --filter remote-control.cm --exclude sys

Dump all available logs where the source moniker, component url, or message contains
\"remote-control\":
  $ ffx log --filter remote-control dump

Dump all logs from the last 30 minutes logged before 5 minutes ago:
  $ ffx log --since \"30m ago\" --until \"5m ago\" dump

Enable DEBUG logs from the \"core/audio\" component while logs are streaming:
  $ ffx log --select core/audio#DEBUG"
)]
pub struct LogCommand {
    #[argh(subcommand)]
    pub sub_command: Option<LogSubCommand>,

    /// filter for a string in either the message, component or url.
    /// May be repeated.
    #[argh(option)]
    pub filter: Vec<String>,

    /// filter for a component moniker.
    /// May be repeated.
    #[argh(option)]
    pub moniker: Vec<String>,

    /// exclude a string in either the message, component or url.
    /// May be repeated.
    #[argh(option)]
    pub exclude: Vec<String>,

    /// filter for only logs with a given tag. May be repeated.
    #[argh(option)]
    pub tag: Vec<String>,

    /// exclude logs with a given tag. May be repeated.
    #[argh(option)]
    pub exclude_tags: Vec<String>,

    /// set the minimum severity. Accepted values (from lower to higher) are: trace, debug, info,
    /// warn (or warning), error, fatal. This field is case insensitive.
    #[argh(option, default = "Severity::Info")]
    pub severity: Severity,

    /// outputs only kernel logs. Overrides any other moniker specified.
    #[argh(switch)]
    pub kernel: bool,

    /// show only logs after a certain time (exclusive)
    #[argh(option, from_str_fn(parse_time))]
    pub since: Option<DetailedDateTime>,

    /// show only logs after a certain time (as a monotonic
    /// timestamp: seconds from the target's boot time).
    #[argh(option, from_str_fn(parse_seconds_string_as_duration))]
    pub since_monotonic: Option<Duration>,

    /// show only logs until a certain time (exclusive)
    #[argh(option, from_str_fn(parse_time))]
    pub until: Option<DetailedDateTime>,

    /// show only logs until a certain time (as a monotonic
    /// timestamp: seconds since the target's boot time).
    #[argh(option, from_str_fn(parse_seconds_string_as_duration))]
    pub until_monotonic: Option<Duration>,

    /// hide the tag field from output (does not exclude any log messages)
    #[argh(switch)]
    pub hide_tags: bool,

    /// hide the file and line number field from output (does not exclude any log messages)
    #[argh(switch)]
    pub hide_file: bool,

    /// disable coloring logs according to severity.
    /// Note that you can permanently disable this with
    /// `ffx config set log_cmd.color false`
    #[argh(switch)]
    pub no_color: bool,

    /// shows process-id and thread-id in log output
    #[argh(switch)]
    pub show_metadata: bool,

    /// shows the full moniker in log output. By default this is false and only the last segment
    /// of the moniker is printed.
    #[argh(switch)]
    pub show_full_moniker: bool,

    /// how to display log timestamps.
    /// Options are "utc", "local", or "monotonic" (i.e. nanos since target boot).
    /// Default is monotonic.
    #[argh(option, default = "TimeFormat::Monotonic")]
    pub clock: TimeFormat,

    /// if provided, logs will not be symbolized
    #[argh(switch)]
    pub raw: bool,

    /// if provided, the symbolizer will not be spawned
    /// like raw, but actually disables the symbolizer
    /// process.
    #[cfg(not(target_os = "fuchsia"))]
    #[argh(switch)]
    pub no_symbolize: bool,

    /// configure symbolization options. Valid options are:
    /// - pretty (default): pretty concise symbolization
    /// - off: disables all symbolization
    /// - classic: traiditional, non-prettified symbolization
    #[cfg(not(target_os = "fuchsia"))]
    #[argh(option, default = "SymbolizeMode::Pretty")]
    pub symbolize: SymbolizeMode,

    /// configure the log settings on the target device for components matching
    /// the given selector. This modifies the minimum log severity level emitted
    /// by components during the logging session.
    /// Specify using the format <component-selector>#<log-level>, with level
    /// as one of FATAL|ERROR|WARN|INFO|DEBUG|TRACE.
    /// May be repeated.
    #[argh(option, from_str_fn(log_interest_selector))]
    pub select: Vec<LogInterestSelector>,
    /// filters by pid
    #[argh(option)]
    pub pid: Option<u64>,
    /// filters by tid
    #[argh(option)]
    pub tid: Option<u64>,
    /// if enabled, selectors will be passed directly to Archivist without any filtering.
    /// If disabled and no matching components are found, the user will be prompted to
    /// either enable this or be given a list of selectors to choose from.
    #[argh(switch)]
    pub force_select: bool,
    /// enables structured JSON logs.
    #[cfg(target_os = "fuchsia")]
    #[argh(switch)]
    pub json: bool,
}

impl Default for LogCommand {
    fn default() -> Self {
        LogCommand {
            filter: vec![],
            moniker: vec![],
            exclude: vec![],
            tag: vec![],
            exclude_tags: vec![],
            hide_tags: false,
            hide_file: false,
            clock: TimeFormat::Monotonic,
            no_color: false,
            kernel: false,
            severity: Severity::Info,
            show_metadata: false,
            raw: false,
            force_select: false,
            since: None,
            since_monotonic: None,
            until: None,
            until_monotonic: None,
            sub_command: None,
            select: vec![],
            show_full_moniker: false,
            pid: None,
            tid: None,
            #[cfg(target_os = "fuchsia")]
            json: false,
            #[cfg(not(target_os = "fuchsia"))]
            no_symbolize: false,
            #[cfg(not(target_os = "fuchsia"))]
            symbolize: SymbolizeMode::Pretty,
        }
    }
}

/// Result returned from processing logs
#[derive(PartialEq, Debug)]
pub enum LogProcessingResult {
    /// The caller should exit
    Exit,
    /// The caller should continue processing logs
    Continue,
}

impl FromStr for SymbolizeMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.to_lowercase();
        match s.as_str() {
            "off" => Ok(SymbolizeMode::Off),
            "pretty" => Ok(SymbolizeMode::Pretty),
            "classic" => Ok(SymbolizeMode::Classic),
            other => Err(format_err!("invalid symbolize flag: {}", other)),
        }
    }
}

#[derive(Error, Debug)]
pub enum LogError {
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
    #[error("No boot timestamp")]
    NoBootTimestamp,
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Cannot use dump with --since now")]
    DumpWithSinceNow,
    #[error("No symbolizer configuration provided")]
    NoSymbolizerConfig,
    #[error(transparent)]
    FfxError(#[from] FfxError),
    #[error(transparent)]
    Utf8Error(#[from] FromUtf8Error),
    #[error(transparent)]
    FidlError(#[from] fidl::Error),
    #[error(transparent)]
    FormatterError(#[from] FormatterError),
}

/// Trait used to get available instances given a moniker query.
#[async_trait::async_trait(?Send)]
pub trait InstanceGetter {
    async fn get_monikers_from_query(&self, query: &str) -> Result<Vec<Moniker>, LogError>;
}

#[async_trait::async_trait(?Send)]
impl InstanceGetter for RealmQueryProxy {
    async fn get_monikers_from_query(&self, query: &str) -> Result<Vec<Moniker>, LogError> {
        Ok(get_instances_from_query(query, &self)
            .await?
            .into_iter()
            .map(|value| value.moniker)
            .collect())
    }
}

impl LogCommand {
    async fn map_interest_selectors(
        &self,
        realm_query: &impl InstanceGetter,
    ) -> Result<Vec<Cow<'_, LogInterestSelector>>, LogError> {
        let selectors = self.get_selectors_and_monikers();
        let mut translated_selectors = vec![];
        for (moniker, selector) in selectors {
            // Attempt to translate to a single instance
            let instances = realm_query.get_monikers_from_query(moniker.as_str()).await?;
            // If exactly one match, perform rewrite
            if instances.len() == 1 {
                let mut translated_selector = selector.clone();
                translated_selector.selector = instances[0].clone().into_component_selector();
                translated_selectors.push((Cow::Owned(translated_selector), instances));
            } else {
                translated_selectors.push((Cow::Borrowed(selector), instances));
            }
        }
        if translated_selectors.iter().any(|(_, matches)| matches.len() > 1) {
            let mut err_output = vec![];
            writeln!(
                &mut err_output,
                "WARN: One or more of your selectors appears to be ambiguous"
            )?;
            writeln!(&mut err_output, "and may not match any components on your system.\n")?;
            writeln!(
                &mut err_output,
                "If this is unintentional you can explicitly match using the"
            )?;
            writeln!(&mut err_output, "following command:\n")?;
            writeln!(&mut err_output, "ffx log \\")?;
            let mut output = vec![];
            for (oselector, instances) in translated_selectors {
                for selector in instances {
                    writeln!(
                        output,
                        "\t--select {}#{} \\",
                        sanitize_moniker_for_selectors(selector.to_string().as_str())
                            .replace("\\", "\\\\"),
                        format!("{:?}", oselector.interest.min_severity.unwrap()).to_uppercase()
                    )?;
                }
            }
            // Intentionally ignored, removes the newline, space, and \
            let _ = output.pop();
            let _ = output.pop();
            let _ = output.pop();

            writeln!(&mut err_output, "{}", String::from_utf8(output).unwrap())?;
            writeln!(&mut err_output, "\nIf this is intentional, you can disable this with")?;
            writeln!(&mut err_output, "ffx log --force-select.")?;

            ffx_bail!("{}", String::from_utf8(err_output)?);
        }
        Ok(translated_selectors.into_iter().map(|(selector, _)| selector).collect())
    }

    /// Sets interest based on configured selectors.
    /// If a single ambiguous match is found, the monikers in the selectors
    /// are automatically re-written.
    pub async fn maybe_set_interest(
        &self,
        log_settings_client: &LogSettingsProxy,
        realm_query: &impl InstanceGetter,
        is_machine: bool,
    ) -> Result<(), LogError> {
        let mut selectors = Cow::Borrowed(&self.select);
        if !self.select.is_empty() && !is_machine && !self.force_select {
            let new_selectors = self.map_interest_selectors(realm_query).await?;
            if !new_selectors.is_empty() {
                selectors = Cow::Owned(
                    new_selectors.into_iter().map(|selector| selector.into_owned()).collect(),
                );
            }
        }
        if !self.select.is_empty() {
            log_settings_client.set_interest(selectors.as_ref()).await?;
        }
        Ok(())
    }

    fn get_selectors_and_monikers(&self) -> Vec<(String, &LogInterestSelector)> {
        let mut selectors = vec![];
        for selector in &self.select {
            let segments = selector.selector.moniker_segments.as_ref().unwrap();
            let mut full_moniker = String::new();
            for segment in segments {
                match segment {
                    fidl_fuchsia_diagnostics::StringSelector::ExactMatch(segment) => {
                        if full_moniker.is_empty() {
                            full_moniker.push_str(segment);
                        } else {
                            full_moniker.push_str("/");
                            full_moniker.push_str(segment);
                        }
                    }
                    _ => {
                        // If the user passed a non-exact match we assume they
                        // know what they're doing and skip this logic.
                        return vec![];
                    }
                }
            }
            selectors.push((full_moniker, selector));
        }
        selectors
    }
}

impl TopLevelCommand for LogCommand {}

fn log_interest_selector(s: &str) -> Result<LogInterestSelector, String> {
    selectors::parse_log_interest_selector(s).map_err(|s| s.to_string())
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use async_trait::async_trait;
    use fidl::endpoints::create_proxy;
    use fidl_fuchsia_diagnostics::{LogSettingsMarker, LogSettingsRequest};
    use futures_util::{future::Either, stream::FuturesUnordered, StreamExt};
    use selectors::parse_log_interest_selector;

    #[derive(Default)]
    struct FakeInstanceGetter {
        output: Vec<Moniker>,
        expected_selector: Option<String>,
    }

    #[async_trait(?Send)]
    impl InstanceGetter for FakeInstanceGetter {
        async fn get_monikers_from_query(&self, query: &str) -> Result<Vec<Moniker>, LogError> {
            if let Some(expected) = &self.expected_selector {
                assert_eq!(expected, query);
            }
            Ok(self.output.clone())
        }
    }

    #[fuchsia::test]
    async fn test_symbolize_mode_from_str() {
        assert_matches!(SymbolizeMode::from_str("off"), Ok(value) if value == SymbolizeMode::Off);
        assert_matches!(
            SymbolizeMode::from_str("pretty"),
            Ok(value) if value == SymbolizeMode::Pretty
        );
        assert_matches!(
            SymbolizeMode::from_str("classic"),
            Ok(value) if value == SymbolizeMode::Classic
        );
    }

    #[fuchsia::test]
    async fn maybe_set_interest_errors_if_ambiguous_selector() {
        let (settings_proxy, settings_server) = create_proxy::<LogSettingsMarker>().unwrap();
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![
            Moniker::try_from("core/some/ambiguous_selector:thing/test").unwrap(),
            Moniker::try_from("core/other/ambiguous_selector:thing/test").unwrap(),
        ];
        // Main should return an error

        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            ..LogCommand::default()
        };
        let mut set_interest_result = None;

        let mut scheduler = FuturesUnordered::new();
        scheduler.push(Either::Left(async {
            set_interest_result =
                Some(cmd.maybe_set_interest(&settings_proxy, &getter, false).await);
            drop(settings_proxy);
        }));
        scheduler.push(Either::Right(async {
            let request = settings_server.into_stream().unwrap().next().await;
            // The channel should be closed without sending any requests.
            assert_matches!(request, None);
        }));
        while let Some(_) = scheduler.next().await {}
        drop(scheduler);

        let error = format!("{}", set_interest_result.unwrap().unwrap_err());

        const EXPECTED_INTEREST_ERROR: &str = r#"WARN: One or more of your selectors appears to be ambiguous
and may not match any components on your system.

If this is unintentional you can explicitly match using the
following command:

ffx log \
	--select core/some/ambiguous_selector\\:thing/test#INFO \
	--select core/other/ambiguous_selector\\:thing/test#INFO

If this is intentional, you can disable this with
ffx log --force-select.
"#;
        assert_eq!(error, EXPECTED_INTEREST_ERROR);
    }

    #[fuchsia::test]
    async fn logger_translates_selector_if_one_match() {
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            ..LogCommand::default()
        };
        let mut set_interest_result = None;
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![Moniker::try_from("core/some/ambiguous_selector").unwrap()];
        let mut scheduler = FuturesUnordered::new();
        let (settings_proxy, settings_server) = create_proxy::<LogSettingsMarker>().unwrap();
        scheduler.push(Either::Left(async {
            set_interest_result =
                Some(cmd.maybe_set_interest(&settings_proxy, &getter, false).await);
            drop(settings_proxy);
        }));
        scheduler.push(Either::Right(async {
            let request = settings_server.into_stream().unwrap().next().await;
            let (selectors, responder) = assert_matches!(
                request,
                Some(Ok(LogSettingsRequest::SetInterest { selectors, responder })) =>
                (selectors, responder)
            );
            responder.send().unwrap();
            assert_eq!(
                selectors,
                vec![parse_log_interest_selector("core/some/ambiguous_selector#INFO").unwrap()]
            );
        }));
        while let Some(_) = scheduler.next().await {}
        drop(scheduler);
        assert_matches!(set_interest_result, Some(Ok(())));
    }

    #[fuchsia::test]
    async fn logger_prints_ignores_ambiguity_if_force_select_is_used() {
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            force_select: true,
            ..LogCommand::default()
        };
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![
            Moniker::try_from("core/some/ambiguous_selector:thing/test").unwrap(),
            Moniker::try_from("core/other/ambiguous_selector:thing/test").unwrap(),
        ];
        let mut set_interest_result = None;
        let mut scheduler = FuturesUnordered::new();
        let (settings_proxy, settings_server) = create_proxy::<LogSettingsMarker>().unwrap();
        scheduler.push(Either::Left(async {
            set_interest_result =
                Some(cmd.maybe_set_interest(&settings_proxy, &getter, false).await);
            drop(settings_proxy);
        }));
        scheduler.push(Either::Right(async {
            let request = settings_server.into_stream().unwrap().next().await;
            let (selectors, responder) = assert_matches!(
                request,
                Some(Ok(LogSettingsRequest::SetInterest { selectors, responder })) =>
                (selectors, responder)
            );
            responder.send().unwrap();
            assert_eq!(
                selectors,
                vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()]
            );
        }));
        while let Some(_) = scheduler.next().await {}
        drop(scheduler);
        assert_matches!(set_interest_result, Some(Ok(())));
    }

    #[fuchsia::test]
    async fn logger_prints_ignores_ambiguity_if_machine_output_is_used() {
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            select: vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()],
            force_select: true,
            ..LogCommand::default()
        };
        let mut getter = FakeInstanceGetter::default();
        getter.expected_selector = Some("ambiguous_selector".into());
        getter.output = vec![
            Moniker::try_from("core/some/collection:thing/test").unwrap(),
            Moniker::try_from("core/other/collection:thing/test").unwrap(),
        ];
        let mut set_interest_result = None;
        let mut scheduler = FuturesUnordered::new();
        let (settings_proxy, settings_server) = create_proxy::<LogSettingsMarker>().unwrap();
        scheduler.push(Either::Left(async {
            set_interest_result =
                Some(cmd.maybe_set_interest(&settings_proxy, &getter, false).await);
            drop(settings_proxy);
        }));
        scheduler.push(Either::Right(async {
            let request = settings_server.into_stream().unwrap().next().await;
            let (selectors, responder) = assert_matches!(
                request,
                Some(Ok(LogSettingsRequest::SetInterest { selectors, responder })) =>
                (selectors, responder)
            );
            responder.send().unwrap();
            assert_eq!(
                selectors,
                vec![parse_log_interest_selector("ambiguous_selector#INFO").unwrap()]
            );
        }));
        while let Some(_) = scheduler.next().await {}
        drop(scheduler);
        assert_matches!(set_interest_result, Some(Ok(())));
    }
    #[test]
    fn test_parse_selector() {
        assert_eq!(
            log_interest_selector("core/audio#DEBUG").unwrap(),
            parse_log_interest_selector("core/audio#DEBUG").unwrap()
        );
    }

    #[test]
    fn test_parse_time() {
        assert_eq!(parse_time("now").unwrap().is_now, true);
        let date_string = "04/20/2020";
        let res = parse_time(date_string).unwrap();
        assert_eq!(res.is_now, false);
        assert_eq!(
            res.date(),
            parse_date_string(date_string, Local::now(), Dialect::Us).unwrap().date()
        );
    }
}
