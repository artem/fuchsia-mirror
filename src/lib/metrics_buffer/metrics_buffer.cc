// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/metrics_buffer/metrics_buffer.h"

#include <fidl/fuchsia.io/cpp/hlcpp_conversion.h>
#include <inttypes.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <memory>
#include <mutex>
#include <utility>

#include "fidl/fuchsia.io/cpp/markers.h"
#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "lib/sync/completion.h"
#include "lib/syslog/global.h"
#include "log.h"
#include "src/lib/metrics_buffer/metrics_impl.h"

namespace cobalt {

// static
std::shared_ptr<MetricsBuffer> MetricsBuffer::Create(uint32_t project_id) {
  return std::shared_ptr<MetricsBuffer>(new MetricsBuffer(project_id));
}

// static
std::shared_ptr<MetricsBuffer> MetricsBuffer::Create(
    uint32_t project_id, std::shared_ptr<sys::ServiceDirectory> service_directory) {
  return std::shared_ptr<MetricsBuffer>(new MetricsBuffer(project_id, service_directory));
}

MetricsBuffer::MetricsBuffer(uint32_t project_id) : project_id_(project_id) {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(!loop_ && !cobalt_logger_);
}

MetricsBuffer::MetricsBuffer(uint32_t project_id,
                             std::shared_ptr<sys::ServiceDirectory> service_directory)
    : project_id_(project_id) {
  SetServiceDirectory(service_directory);
}

MetricsBuffer::~MetricsBuffer() { SetServiceDirectory(nullptr); }

void MetricsBuffer::SetServiceDirectory(std::shared_ptr<sys::ServiceDirectory> service_directory) {
  FX_LOGS(INFO) << "SetServiceDirectory is called";
  std::unique_ptr<cobalt::MetricsImpl> logger_to_delete_outside_lock;
  std::unique_ptr<async::Loop> loop_to_stop_outside_lock;
  {  // scope lock
    std::lock_guard<std::mutex> lock(lock_);
    ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
    if (cobalt_logger_) {
      FX_LOGS(INFO) << "cobalt_logger already exists";
      ZX_DEBUG_ASSERT(loop_);
      // Clean these up after we've released lock_, to avoid potential deadlock waiting on a thread
      // that may be trying to get lock_.
      loop_to_stop_outside_lock = std::move(loop_);
      logger_to_delete_outside_lock = std::move(cobalt_logger_);
    }
    ZX_DEBUG_ASSERT(!loop_ && !cobalt_logger_);
    if (service_directory) {
      FX_LOGS(INFO) << "Creating new cobalt_logger";
      std::unique_ptr<cobalt::MetricsImpl> new_logger;
      fidl::ClientEnd<fuchsia_io::Directory> directory =
          fidl::HLCPPToNatural(service_directory->CloneChannel());
      auto loop = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);
      zx_status_t status = loop->StartThread("MetricsBuffer");
      if (status != ZX_OK) {
        LOG(WARNING, "MetricsBuffer::SetServiceDirectory() thread creation failed.");
        // ~loop
        // ~service_directory
        return;
      }
      sync_completion_t finished;
      // Must create fuchsia_metrics::MetricEventLogger on same dispatcher that it'll use.
      async::PostTask(loop->dispatcher(), [this, &loop, &directory, &new_logger, &finished] {
        // MetricsImpl will internally use the directory to reconnect as needed, should we ever lose
        // connection.
        new_logger = std::make_unique<cobalt::MetricsImpl>(loop->dispatcher(), std::move(directory),
                                                           project_id_);
        sync_completion_signal(&finished);
      });
      sync_completion_wait(&finished, ZX_TIME_INFINITE);
      loop_ = std::move(loop);
      cobalt_logger_ = std::move(new_logger);
      ZX_DEBUG_ASSERT(!!loop_ && !!cobalt_logger_);
      if (!pending_counts_.empty()) {
        LOG(INFO, "MetricsBuffer::SetServiceDirectory() flushing counts soon.");
        TryPostFlushCountsLocked();
      }
    }
    ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  }  // ~lock
  ZX_DEBUG_ASSERT(!!loop_to_stop_outside_lock == !!logger_to_delete_outside_lock);
  if (loop_to_stop_outside_lock) {
    // Need to delete the old MetricsImpl with the same dispatcher that created the old MetricsImpl.
    sync_completion_t finished;
    async::PostTask(loop_to_stop_outside_lock->dispatcher(),
                    [&logger_to_delete_outside_lock, &finished] {
                      logger_to_delete_outside_lock = nullptr;
                      sync_completion_signal(&finished);
                    });
    ZX_ASSERT(ZX_OK == sync_completion_wait(&finished, ZX_TIME_INFINITE));
    ZX_DEBUG_ASSERT(!logger_to_delete_outside_lock);
    loop_to_stop_outside_lock->Quit();
    loop_to_stop_outside_lock->JoinThreads();
    loop_to_stop_outside_lock->Shutdown();
    // Delete here for clarity.
    loop_to_stop_outside_lock = nullptr;
  }
}

void MetricsBuffer::SetMinLoggingPeriod(zx::duration min_logging_period) {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(last_flushed_ == zx::time::infinite_past());
  min_logging_period_ = min_logging_period;
}

void MetricsBuffer::LogEventCount(uint32_t metric_id, std::vector<uint32_t> dimension_values,
                                  uint32_t count) {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  bool was_empty = pending_counts_.empty();
  MetricKey key(metric_id, std::move(dimension_values));
  pending_counts_[key] += count;
  if (was_empty) {
    // We don't try to process locally, because if we're logging infrequently then the optimization
    // wouldn't matter, and if we're logging frequently then we need to post in order to delay
    // anyway.  So we opt to keep the code simpler and always post even if the deadline is in the
    // past.
    TryPostFlushCountsLocked();
  }
}

void MetricsBuffer::LogEvent(uint32_t metric_id, std::vector<uint32_t> dimension_values) {
  LogEventCount(metric_id, std::move(dimension_values), 1);
}

void MetricsBuffer::LogString(uint32_t metric_id, std::vector<uint32_t> dimension_values,
                              std::string string_value) {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  bool was_empty = pending_strings_.empty();
  MetricKey key(metric_id, std::move(dimension_values));
  // By design, this emplace can fail if a matching string is already in the set.
  pending_strings_[key].emplace(std::move(string_value));
  if (was_empty) {
    TryPostFlushCountsLocked();
  }
}

void MetricsBuffer::LogHistogramValue(const HistogramInfo& histogram_info,
                                      std::vector<uint32_t> dimension_values, int64_t value) {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  bool was_empty = pending_histograms_.empty();
  MetricKey key(histogram_info.metric_id, std::move(dimension_values));
  uint32_t bucket_index = histogram_info.bucket_config->BucketIndex(value);
  pending_histograms_[key][bucket_index]++;
  if (was_empty) {
    TryPostFlushCountsLocked();
  }
}

void MetricsBuffer::FlushPendingEventCounts() {
  std::lock_guard<std::mutex> lock(lock_);
  ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  if (!cobalt_logger_) {
    // In some testing scenarios, we may not have access to a real MetricEventLoggerFactory, and we
    // can end up here if SetServiceDirectory() hit an error while (or shortly after) switching from
    // an old loop_ and cobalt_logger_ to a new loop_ and cobalt_logger_.
    //
    // If later we get a new cobalt_logger_ from a new SetServiceDirectory(), this method will run
    // again.
    return;
  }
  last_flushed_ = zx::clock::get_monotonic();

  constexpr uint32_t kMaxBatchSize = 64;
  std::vector<fuchsia_metrics::MetricEvent> batch;

  PendingCounts snapped_pending_event_counts;
  snapped_pending_event_counts.swap(pending_counts_);
  for (auto& [key, count] : snapped_pending_event_counts) {
    batch.emplace_back(key.metric_id(), key.dimension_values(),
                       fuchsia_metrics::MetricEventPayload::WithCount(count));
    ZX_DEBUG_ASSERT(batch.size() <= kMaxBatchSize);
    if (batch.size() == kMaxBatchSize) {
      cobalt_logger_->LogMetricEvents(std::move(batch));
      batch.clear();
    }
  }

  PendingStrings snapped_pending_strings;
  snapped_pending_strings.swap(pending_strings_);
  for (auto& [key, strings] : snapped_pending_strings) {
    for (auto& string : strings) {
      batch.emplace_back(key.metric_id(), key.dimension_values(),
                         fuchsia_metrics::MetricEventPayload::WithStringValue(string));
      ZX_DEBUG_ASSERT(batch.size() <= kMaxBatchSize);
      if (batch.size() == kMaxBatchSize) {
        cobalt_logger_->LogMetricEvents(std::move(batch));
        batch.clear();
      }
    }
  }

  PendingHistograms snapped_pending_histograms;
  snapped_pending_histograms.swap(pending_histograms_);
  for (auto& [histogram_key, pending_buckets] : snapped_pending_histograms) {
    std::vector<fuchsia_metrics::HistogramBucket> buckets;
    auto bucket_iter = pending_buckets.begin();
    while (bucket_iter != pending_buckets.end()) {
      auto [bucket_index, tally] = *bucket_iter;
      bucket_iter++;
      fuchsia_metrics::HistogramBucket bucket;
      bucket.index() = bucket_index;
      bucket.count() = tally;
      buckets.emplace_back(std::move(bucket));
      if (buckets.size() == fuchsia_metrics::kMaxHistogramBuckets ||
          bucket_iter == pending_buckets.end()) {
        batch.emplace_back(histogram_key.metric_id(), histogram_key.dimension_values(),
                           fuchsia_metrics::MetricEventPayload::WithHistogram(std::move(buckets)));
        buckets.clear();
        ZX_DEBUG_ASSERT(batch.size() <= kMaxBatchSize);
        if (batch.size() == kMaxBatchSize) {
          cobalt_logger_->LogMetricEvents(std::move(batch));
          batch.clear();
        }
      }
    }
  }

  ZX_DEBUG_ASSERT(batch.size() < kMaxBatchSize);
  if (!batch.empty()) {
    cobalt_logger_->LogMetricEvents(std::move(batch));
    batch.clear();
  }
}

void MetricsBuffer::TryPostFlushCountsLocked() {
  ZX_DEBUG_ASSERT(!!loop_ == !!cobalt_logger_);
  if (cobalt_logger_) {
    ZX_DEBUG_ASSERT(loop_);
    async::PostTaskForTime(
        loop_->dispatcher(), [this] { FlushPendingEventCounts(); },
        last_flushed_ + min_logging_period_);
  }
}

MetricsBuffer::MetricKey::MetricKey(uint32_t metric_id, std::vector<uint32_t> dimension_values)
    : metric_id_(metric_id), dimension_values_(std::move(dimension_values)) {}

uint32_t MetricsBuffer::MetricKey::metric_id() const { return metric_id_; }

const std::vector<uint32_t>& MetricsBuffer::MetricKey::dimension_values() const {
  return dimension_values_;
}

size_t MetricsBuffer::MetricKeyHash::operator()(const MetricKey& key) const noexcept {
  // Rely on size_t being unsigned so it'll wrap without being undefined behavior.
  size_t hash = hash_uint32_(key.metric_id());
  for (auto value : key.dimension_values()) {
    hash += hash_uint32_(value);
  }
  return hash;
}

bool MetricsBuffer::MetricKeyEqual::operator()(const MetricKey& lhs,
                                               const MetricKey& rhs) const noexcept {
  if (lhs.metric_id() != rhs.metric_id()) {
    return false;
  }
  if (lhs.dimension_values().size() != rhs.dimension_values().size()) {
    return false;
  }
  size_t size = lhs.dimension_values().size();
  for (uint32_t i = 0; i < size; ++i) {
    if (lhs.dimension_values()[i] != rhs.dimension_values()[i]) {
      return false;
    }
  }
  return true;
}

MetricBuffer MetricsBuffer::CreateMetricBuffer(uint32_t metric_id) {
  return MetricBuffer(shared_from_this(), metric_id);
}

StringMetricBuffer MetricsBuffer::CreateStringMetricBuffer(uint32_t metric_id) {
  return StringMetricBuffer(shared_from_this(), metric_id);
}

HistogramMetricBuffer MetricsBuffer::CreateHistogramMetricBuffer(HistogramInfo histogram_info) {
  return HistogramMetricBuffer(shared_from_this(), std::move(histogram_info));
}

MetricBuffer::MetricBuffer(std::shared_ptr<MetricsBuffer> parent, uint32_t metric_id)
    : parent_(parent), metric_id_(metric_id) {}

void MetricBuffer::LogEvent(std::vector<uint32_t> dimension_values) {
  parent_->LogEvent(metric_id_, std::move(dimension_values));
}

void MetricBuffer::LogEventCount(std::vector<uint32_t> dimension_values, uint32_t count) {
  parent_->LogEventCount(metric_id_, std::move(dimension_values), count);
}

StringMetricBuffer::StringMetricBuffer(std::shared_ptr<MetricsBuffer> parent, uint32_t metric_id)
    : parent_(parent), metric_id_(metric_id) {}

void StringMetricBuffer::LogString(std::vector<uint32_t> dimension_values,
                                   std::string string_value) {
  parent_->LogString(metric_id_, std::move(dimension_values), std::move(string_value));
}

HistogramMetricBuffer::HistogramMetricBuffer(std::shared_ptr<MetricsBuffer> parent,
                                             HistogramInfo histogram_info)
    : parent_(parent), histogram_info_(std::move(histogram_info)) {}

void HistogramMetricBuffer::LogValue(std::vector<uint32_t> dimension_values, int64_t value) {
  parent_->LogHistogramValue(histogram_info_, std::move(dimension_values), value);
}

}  // namespace cobalt
