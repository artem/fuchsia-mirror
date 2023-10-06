// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::file_handler::{self, PersistData, PersistPayload, PersistSchema, Timestamps};
use diagnostics_data::{Data, DiagnosticsHierarchy, Inspect};
use diagnostics_reader::ArchiveReader;
use fidl_fuchsia_diagnostics::ArchiveAccessorProxy;
use fidl_fuchsia_diagnostics::Selector;
use fuchsia_async::{self as fasync, Task};
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc::{self, UnboundedSender},
    StreamExt,
};
use persistence_config::{Config, ServiceName, Tag, TagConfig};
use serde_json::{Map, Value};
use std::collections::{hash_map::Entry, HashMap};
use tracing::*;

// The capability name for the Inspect reader
const INSPECT_SERVICE_PATH: &str = "/svc/fuchsia.diagnostics.FeedbackArchiveAccessor";

#[derive(Clone)]
pub(crate) struct Fetcher(UnboundedSender<FetchCommand>);

pub(crate) struct FetchCommand {
    pub(crate) service: ServiceName,
    pub(crate) tags: Vec<Tag>,
}

fn extract_json_map(hierarchy: Option<DiagnosticsHierarchy>) -> Option<Map<String, Value>> {
    let Some(hierarchy) = hierarchy else {
        return None;
    };
    let Ok(Value::Object(mut map)) = serde_json::to_value(hierarchy) else {
        return None;
    };
    if map.len() != 1 || !map.contains_key("root") {
        return Some(map);
    }
    if let Value::Object(map) = map.remove("root").unwrap() {
        return Some(map);
    }
    None
}

fn condensed_map_of_data(items: impl IntoIterator<Item = Data<Inspect>>) -> HashMap<String, Value> {
    items.into_iter().fold(HashMap::new(), |mut entries, item| {
        let Data { payload, moniker, .. } = item;
        if let Some(new_map) = extract_json_map(payload) {
            match entries.entry(moniker) {
                Entry::Occupied(mut o) => {
                    let existing_payload = o.get_mut();
                    if let Value::Object(existing_payload_map) = existing_payload {
                        existing_payload_map.extend(new_map.into_iter());
                    }
                }
                Entry::Vacant(v) => {
                    v.insert(Value::Object(new_map));
                }
            }
        }
        entries
    })
}

fn save_data_for_tag(
    inspect_data: Vec<Data<Inspect>>,
    timestamps: Timestamps,
    tag: &Tag,
    tag_info: &TagInfo,
) -> PersistSchema {
    let mut filtered_datas = vec![];
    for data in inspect_data {
        match data.filter(&tag_info.selectors) {
            Ok(Some(data)) => filtered_datas.push(data),
            Ok(None) => {}
            Err(e) => return PersistSchema::error(timestamps, format!("Filter error: {e}")),
        }
    }

    if filtered_datas.is_empty() {
        return PersistSchema::error(timestamps, format!("No data available for tag '{tag}'"));
    }
    // We may have multiple entries with the same moniker. Fold those together into a single entry.
    let entries = condensed_map_of_data(filtered_datas);
    let data_length = match serde_json::to_string(&entries) {
        Ok(string) => string.len(),
        Err(e) => {
            return PersistSchema::error(timestamps, format!("Unexpected serialize error: {e}"))
        }
    };
    if data_length > tag_info.max_save_length {
        let error_description =
            format!("Data too big: {data_length} > max length {0}", tag_info.max_save_length);
        return PersistSchema::error(timestamps, error_description);
    }
    PersistSchema {
        timestamps,
        payload: PersistPayload::Data(PersistData { data_length, entries }),
    }
}

fn utc_now() -> i64 {
    let now_utc = chrono::prelude::Utc::now(); // Consider using SystemTime::now()?
    now_utc.timestamp() * 1_000_000_000 + now_utc.timestamp_subsec_nanos() as i64
}

#[derive(Debug)]
struct TagInfo {
    max_save_length: usize,
    selectors: Vec<Selector>,
}

type TagsInfo = HashMap<Tag, TagInfo>;

type ServicesInfo = HashMap<ServiceName, TagsInfo>;

/// If we've gotten an error before trying to fetch, save it to all tags and log it as an error.
fn record_service_error(service_name: &ServiceName, tags: &[Tag], error: String) {
    let utc = utc_now();
    let monotonic = zx::Time::get_monotonic().into_nanos();
    let timestamps = Timestamps {
        before_monotonic: monotonic,
        after_monotonic: monotonic,
        before_utc: utc,
        after_utc: utc,
    };
    record_timestamped_error(service_name, tags, timestamps, error);
}

/// If we've gotten an error, save it and the fetch timestamp to all tags and log it as a warning.
fn record_timestamped_error(
    service_name: &ServiceName,
    tags: &[Tag],
    timestamps: Timestamps,
    error: String,
) {
    warn!("{error}");
    let error = PersistSchema::error(timestamps, error);
    for tag in tags {
        file_handler::write(&service_name, &tag, &error);
    }
}

// Selectors for Inspect data must start with this exact string.
const INSPECT_PREFIX: &str = "INSPECT:";

fn strip_inspect_prefix<'a>(selectors: &'a [String]) -> impl Iterator<Item = &str> {
    let get_inspect = |s: &'a String| -> Option<&str> {
        if &s[..INSPECT_PREFIX.len()] == INSPECT_PREFIX {
            Some(&s[INSPECT_PREFIX.len()..])
        } else {
            warn!("All selectors should begin with 'INSPECT:' - '{}'", s);
            None
        }
    };
    selectors.iter().filter_map(get_inspect)
}

async fn fetch_and_save(
    proxy: &ArchiveAccessorProxy,
    services: &ServicesInfo,
    service_name: ServiceName,
    tags: Vec<Tag>,
) {
    let service_info = match services.get(&service_name) {
        Some(info) => info,
        None => {
            warn!("Bad service {service_name} received in fetch");
            return;
        }
    };

    // Assemble the selectors
    // This could be done with filter_map, map, flatten, peekable, except for
    // https://github.com/rust-lang/rust/issues/102211#issuecomment-1513931928
    // - some iterators (including peekable) don't play well with async.
    let selectors = {
        let mut selectors = vec![];
        for tag in &tags {
            if let Some(tag_info) = service_info.get(tag) {
                for selector in tag_info.selectors.iter() {
                    selectors.push(selector.clone());
                }
            }
        }
        if selectors.is_empty() {
            record_service_error(
                &service_name,
                &tags,
                format!("Empty selectors from service {} and tags {:?}", service_name, tags),
            );
            return;
        }
        selectors
    };

    let mut source = ArchiveReader::new();
    source.with_archive(proxy.clone()).retry_if_empty(false).add_selectors(selectors.into_iter());

    // Do the fetch and record the timestamps.
    let before_utc = utc_now();
    let before_monotonic = zx::Time::get_monotonic().into_nanos();
    let data = source.snapshot::<Inspect>().await;
    let after_utc = utc_now();
    let after_monotonic = zx::Time::get_monotonic().into_nanos();
    let timestamps = Timestamps { before_utc, before_monotonic, after_utc, after_monotonic };
    let data = match data {
        Err(e) => {
            record_timestamped_error(
                &service_name,
                &tags,
                timestamps,
                format!("Failed to fetch Inspect data: {:?}", e),
            );
            return;
        }
        Ok(data) => data,
    };

    // Process the data for each tag
    for tag in tags {
        let Some(tag_info) = service_info.get(&tag) else {
            warn!("Tag '{tag}' was not found in config; skipping it.");
            continue;
        };
        let data_to_save = save_data_for_tag(data.clone(), timestamps.clone(), &tag, tag_info);
        file_handler::write(&service_name, &tag, &data_to_save);
    }
}

impl Fetcher {
    /// Creates a Fetcher accessible through an unbounded channel. The receiving task and
    /// its data structures will be preserved by the `async move {...}`'d Fetcher task.
    /// so we just have to return the channel sender.
    pub(crate) fn new(config: &Config) -> Result<(Fetcher, Task<()>), anyhow::Error> {
        let mut services = HashMap::new();
        let proxy = fuchsia_component::client::connect_to_protocol_at_path::<
            fidl_fuchsia_diagnostics::ArchiveAccessorMarker,
        >(INSPECT_SERVICE_PATH)?;
        for (service_name, tags_info) in config.iter() {
            let mut tags = HashMap::new();
            for (tag_name, TagConfig { selectors, max_bytes, .. }) in tags_info.iter() {
                let selectors = strip_inspect_prefix(selectors)
                    .map(|s| selectors::parse_selector::<selectors::VerboseError>(s))
                    .collect::<Result<Vec<_>, _>>()?;
                let info = TagInfo { selectors, max_save_length: *max_bytes };
                tags.insert(tag_name.clone(), info);
            }
            services.insert(service_name.clone(), tags);
        }
        let (invoke_fetch, mut receiver) = mpsc::unbounded::<FetchCommand>();
        let task = fasync::Task::spawn(async move {
            while let Some(FetchCommand { service, tags }) = receiver.next().await {
                fetch_and_save(&proxy, &services, service, tags).await;
            }
        });
        Ok((Fetcher(invoke_fetch), task))
    }

    pub(crate) fn send(&mut self, command: FetchCommand) {
        let _ = self.0.unbounded_send(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_data::InspectHandleName;
    use diagnostics_hierarchy::hierarchy;
    use serde_json::json;

    #[fuchsia::test]
    fn test_selector_stripping() {
        assert_eq!(
            strip_inspect_prefix(&vec![
                "INSPECT:foo".to_string(),
                "oops:bar".to_string(),
                "INSPECT:baz".to_string()
            ])
            .collect::<Vec<_>>(),
            vec!["foo".to_string(), "baz".to_string()]
        )
    }

    #[fuchsia::test]
    fn test_condense_empty() {
        let empty_data = Data::for_inspect(
            "a/b/c/d",
            None,
            123456i64,
            "fuchsia-pkg://test",
            Some(InspectHandleName::filename("test_file_plz_ignore.inspect")),
            Vec::new(),
        );
        let empty_data_result = condensed_map_of_data(vec![empty_data].into_iter());
        let empty_vec_result = condensed_map_of_data(vec![].into_iter());

        let expected_map = HashMap::new();

        pretty_assertions::assert_eq!(empty_data_result, expected_map, "golden diff failed.");
        pretty_assertions::assert_eq!(empty_vec_result, expected_map, "golden diff failed.");
    }

    fn make_data(mut hierarchy: DiagnosticsHierarchy, moniker: &str) -> Data<Inspect> {
        hierarchy.sort();
        Data::for_inspect(
            moniker,
            Some(hierarchy),
            123456i64,
            "fuchsia-pkg://test",
            Some(InspectHandleName::filename("test_file_plz_ignore.inspect")),
            Vec::new(),
        )
    }

    #[fuchsia::test]
    fn test_condense_one() {
        let data = make_data(
            hierarchy! {
                root: {
                    "x": "foo",
                    "y": "bar",
                }
            },
            "a/b/c/d",
        );

        let expected_json = json!({
            "a/b/c/d": {
                "x": "foo",
                "y": "bar",
            }
        });

        let result = condensed_map_of_data(vec![data].into_iter());

        pretty_assertions::assert_eq!(
            serde_json::to_value(&result).unwrap(),
            expected_json,
            "golden diff failed."
        );
    }

    #[fuchsia::test]
    fn test_condense_several_with_merge() {
        let data_abcd = make_data(
            hierarchy! {
                root: {
                    "x": "foo",
                    "y": "bar",
                }
            },
            "a/b/c/d",
        );
        let data_efgh = make_data(
            hierarchy! {
                root: {
                    "x": "ex",
                    "y": "why",
                }
            },
            "e/f/g/h",
        );
        let data_abcd2 = make_data(
            hierarchy! {
                root: {
                    "x": "X",
                    "z": "zebra",
                }
            },
            "a/b/c/d",
        );

        let expected_json = json!({
            "a/b/c/d": {
                "x": "X",
                "y": "bar",
                "z": "zebra",
            },
            "e/f/g/h": {
                "x": "ex",
                "y": "why"
            }
        });

        let result = condensed_map_of_data(vec![data_abcd, data_efgh, data_abcd2]);

        pretty_assertions::assert_eq!(
            serde_json::to_value(&result).unwrap(),
            expected_json,
            "golden diff failed."
        );
    }

    const TIMESTAMPS: Timestamps =
        Timestamps { after_monotonic: 200, after_utc: 111, before_monotonic: 100, before_utc: 110 };

    fn tag_info(max_save_length: usize, selectors: Vec<&str>) -> TagInfo {
        let selectors = selectors
            .iter()
            .map(|s| selectors::parse_selector::<selectors::VerboseError>(s))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        TagInfo { selectors, max_save_length }
    }

    #[fuchsia::test]
    fn save_data_no_data() {
        let tag = Tag::new("tag".to_string()).unwrap();
        let result = save_data_for_tag(
            vec![],
            TIMESTAMPS.clone(),
            &tag,
            &tag_info(1000, vec!["moniker:path:property"]),
        );

        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "@timestamps": {
                    "after_monotonic": 200,
                    "after_utc": 111,
                    "before_monotonic": 100,
                    "before_utc": 110,
                },
                ":error": {
                    "description": "No data available for tag 'tag'",
                },
            })
        );
    }

    #[fuchsia::test]
    fn save_data_too_big() {
        let tag = Tag::new("tag".to_string()).unwrap();
        let data_abcd = make_data(
            hierarchy! {
                root: {
                    "x": "foo",
                    "y": "bar",
                }
            },
            "a/b/c/d",
        );

        let result = save_data_for_tag(
            vec![data_abcd],
            TIMESTAMPS.clone(),
            &tag,
            &tag_info(20, vec!["a/b/c/d:root:y"]),
        );

        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "@timestamps": {
                    "after_monotonic": 200,
                    "after_utc": 111,
                    "before_monotonic": 100,
                    "before_utc": 110,
                },
                ":error": {
                    "description": "Data too big: 23 > max length 20",
                },
            })
        );
    }

    #[fuchsia::test]
    fn save_string_with_data() {
        let tag = Tag::new("tag".to_string()).unwrap();
        let data_abcd = make_data(
            hierarchy! {
                root: {
                    "x": "foo",
                    "y": "bar",
                }
            },
            "a/b/c/d",
        );
        let data_efgh = make_data(
            hierarchy! {
                root: {
                    "x": "ex",
                    "y": "why",
                }
            },
            "e/f/g/h",
        );
        let data_abcd2 = make_data(
            hierarchy! {
                root: {
                    "x": "X",
                    "z": "zebra",
                }
            },
            "a/b/c/d",
        );

        let result = save_data_for_tag(
            vec![data_abcd, data_efgh, data_abcd2],
            TIMESTAMPS.clone(),
            &tag,
            &tag_info(1000, vec!["a/b/c/d:root:y"]),
        );

        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "@timestamps": {
                    "after_monotonic": 200,
                    "after_utc": 111,
                    "before_monotonic": 100,
                    "before_utc": 110,
                },
                "@persist_size": 23,
                "a/b/c/d": {
                    "y": "bar",
                },
            })
        );
    }
}
