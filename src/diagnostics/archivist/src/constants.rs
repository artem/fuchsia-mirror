// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Divide total batch timeout duration by this value to get the duration that should be allowed
// on individual lazy nodes/values.
pub const LAZY_NODE_TIMEOUT_PROPORTION: i64 = 2;

/// Name used by clients to connect to the feedback diagnostics protocol.
/// This protocol applies static selectors configured under config/data/feedback to
/// inspect exfiltration.
pub const FEEDBACK_ARCHIVE_ACCESSOR_NAME: &str = "fuchsia.diagnostics.FeedbackArchiveAccessor";

/// Name used by clients to connect to the legacy metrics diagnostics protocol.
/// This protocol applies static selectors configured under
/// config/data/legacy_metrics to inspect exfiltration.
pub const LEGACY_METRICS_ARCHIVE_ACCESSOR_NAME: &str =
    "fuchsia.diagnostics.LegacyMetricsArchiveAccessor";

/// Name used by clients to connect to the lowpan diagnostics protocol.
/// This protocol applies static selectors configured under config/data/lowpan to
/// inspect exfiltration.
pub const LOWPAN_ARCHIVE_ACCESSOR_NAME: &str = "fuchsia.diagnostics.LoWPANArchiveAccessor";

/// The maximum number of bytes in a formatted content VMO.
pub const FORMATTED_CONTENT_CHUNK_SIZE_TARGET: u64 = 1 << 20; // 1 MiB

/// For production archivist instances this value is retrieved from configuration but we still
/// provide a default here for internal testing purposes.
pub const LEGACY_DEFAULT_MAXIMUM_CACHED_LOGS_BYTES: u64 = 4 * 1024 * 1024;

/// Default path where pipeline configuration are located.
pub const DEFAULT_PIPELINES_PATH: &str = "/config/data";
