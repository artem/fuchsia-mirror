// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Inspector;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use regex::bytes::Regex;
use starnix_sync::Mutex;
use std::collections::{hash_map::Entry, BTreeMap, HashMap};

/// Path prefixes for which Starnix is responsible.
const DESIRED_PATH_PREFIXES: &[&str] = &["/dev/", "/proc/", "/sys/"];

/// Regular expression to deduplicate commonly seen numbered elements of paths in internal
/// filesystems.
const NUMBER_DEDUPER: &str = r#"(block/[A-Za-z]+|cpu|proc/|pid_|uid_)\d+"#;

static NOT_FOUND_COUNTS: Lazy<Mutex<HashMap<Vec<u8>, u64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn track_file_not_found(path: Vec<u8>) {
    if DESIRED_PATH_PREFIXES.iter().any(|&prefix| path.starts_with(prefix.as_bytes())) {
        match NOT_FOUND_COUNTS.lock().entry(path) {
            Entry::Occupied(mut o) => *o.get_mut() += 1,
            Entry::Vacant(v) => {
                crate::log_warn!(
                    tag = "not_found",
                    path = %String::from_utf8_lossy(v.key()),
                    "couldn't resolve",
                );
                v.insert(1);
            }
        }
    }
}

pub fn not_found_lazy_node_callback() -> BoxFuture<'static, Result<Inspector, anyhow::Error>> {
    Box::pin(async {
        let inspector = Inspector::default();

        // The internal paths we care about often include process or user IDs and those don't
        // matter for our ability to understand feature gaps. Replace them all with `N` and merge
        // the counts together.
        //
        // This regular expression should be bounded to the number of digits that appear in a path,
        // and we only run this code when inspect is being collected.
        let original_counts = NOT_FOUND_COUNTS.lock();
        let original_counts = original_counts.iter().map(|(p, n)| (p.as_slice(), *n));
        for (path, count) in dedupe_uninteresting_numbers_in_paths(original_counts) {
            inspector.root().record_uint(path, count);
        }
        Ok(inspector)
    })
}

fn dedupe_uninteresting_numbers_in_paths<'a>(
    original_counts: impl Iterator<Item = (&'a [u8], u64)>,
) -> BTreeMap<String, u64> {
    let number_deduper = Regex::new(NUMBER_DEDUPER).unwrap();
    let mut numbers_collapsed = BTreeMap::new();
    for (orig_path, count) in original_counts {
        let collapsed = number_deduper.replace_all(orig_path, "${1}N".as_bytes());
        let collapsed = String::from_utf8_lossy(&collapsed);
        *numbers_collapsed.entry(collapsed.to_string()).or_default() += count;
    }
    numbers_collapsed
}

#[cfg(test)]
mod tests {
    use super::dedupe_uninteresting_numbers_in_paths;

    #[test]
    fn dedupe_expected_paths() {
        let original_paths = &[
            "/dev/cpuctl/dex2oat/cgroup.procs",
            "/dev/pmsg0",
            "/proc/1006/cgroup",
            "/proc/1006/schedstat",
            "/proc/268/cgroup",
            "/proc/470/schedstat",
            "/proc/47/schedstat",
            "/proc/32/cgroup",
            "/proc/2/schedstat",
            "/proc/2/cgroup",
            "/proc/sys/kernel/domainname",
            "/proc/sys/net/ipv4/conf",
            "/proc/sys/net/ipv6/conf/default/accept_ra_rt_info_min_plen",
            "/proc/uid_concurrent_policy_time",
            "/sys/block/loop0/queue/nr_requests",
            "/sys/block/loop10/queue/nr_requests",
            "/sys/devices/system/cpu/cpu0/cpufreq/stats/time_in_state",
            "/sys/devices/system/cpu/cpu1/cpufreq/stats/time_in_state",
            "/sys/devices/system/cpu/cpu0uevent",
            "/sys/devices/system/cpu/cpu1uevent",
            "/sys/devices/virtual/block/loop0/queueuevent",
            "/sys/devices/virtual/block/loop1/queueuevent",
            "/sys/fs/cgroup/uid_0/pid_1006/cgroup.events",
            "/sys/fs/cgroup/uid_0/pid_182/cgroup.events",
            "/sys/fs/cgroup/uid_0/pid_45/cgroup.events",
            "/sys/fs/cgroup/uid_1000/pid_184/cgroup.events",
            "/sys/fs/cgroup/uid_1000/pid_50/cgroup.events",
            "/sys/fs/cgroup/uid_1002/pid_1006/cgroup.events",
            "/sys/fs/f2fs/features",
            "/sys/kernel/debug/tracing/events/ext4/ext4_da_write_begin/enable",
            "/sys/kernel/debug/tracing/events/f2fs/f2fs_get_data_block/enable",
            "/sys/kernel/debug/tracing/events/gpu_mem/gpu_mem_total/enable",
            "/sys/kernel/debug/tracing/events/i2c/enable",
            "/sys/kernel/debug/tracing/events/i2c/i2c_read/enable",
            "/sys/kernel/debug/tracing/per_cpu/cpu20/trace",
            "/sys/kernel/debug/tracing/per_cpu/cpu7/trace",
            "/sys/kernel/tracing/options/record-tgid",
            "/sys/kernel/tracing/per_cpu/cpu20/trace",
            "/sys/kernel/tracing/per_cpu/cpu3/trace",
        ];
        let observed =
            dedupe_uninteresting_numbers_in_paths(original_paths.iter().map(|p| (p.as_bytes(), 1)))
                .into_iter()
                .map(|(p, n)| (p.to_string(), n))
                .collect::<Vec<(String, u64)>>();
        let expected = [
            ("/dev/cpuctl/dex2oat/cgroup.procs", 1),
            ("/dev/pmsg0", 1),
            ("/proc/N/cgroup", 4),
            ("/proc/N/schedstat", 4),
            ("/proc/sys/kernel/domainname", 1),
            ("/proc/sys/net/ipv4/conf", 1),
            ("/proc/sys/net/ipv6/conf/default/accept_ra_rt_info_min_plen", 1),
            ("/proc/uid_concurrent_policy_time", 1),
            ("/sys/block/loopN/queue/nr_requests", 2),
            ("/sys/devices/system/cpu/cpuN/cpufreq/stats/time_in_state", 2),
            ("/sys/devices/system/cpu/cpuNuevent", 2),
            ("/sys/devices/virtual/block/loopN/queueuevent", 2),
            ("/sys/fs/cgroup/uid_N/pid_N/cgroup.events", 6),
            ("/sys/fs/f2fs/features", 1),
            ("/sys/kernel/debug/tracing/events/ext4/ext4_da_write_begin/enable", 1),
            ("/sys/kernel/debug/tracing/events/f2fs/f2fs_get_data_block/enable", 1),
            ("/sys/kernel/debug/tracing/events/gpu_mem/gpu_mem_total/enable", 1),
            ("/sys/kernel/debug/tracing/events/i2c/enable", 1),
            ("/sys/kernel/debug/tracing/events/i2c/i2c_read/enable", 1),
            ("/sys/kernel/debug/tracing/per_cpu/cpuN/trace", 2),
            ("/sys/kernel/tracing/options/record-tgid", 1),
            ("/sys/kernel/tracing/per_cpu/cpuN/trace", 2),
        ]
        .iter()
        .map(|(p, n)| (p.to_string(), *n))
        .collect::<Vec<(String, u64)>>();
        pretty_assertions::assert_eq!(observed, expected);
    }
}
