// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/monitor/metrics.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/time.h>

#include <algorithm>
#include <array>
#include <string>

#include <src/lib/cobalt/cpp/metric_event_builder.h>

#include "lib/fpromise/result.h"
#include "src/developer/memory/metrics/digest.h"
#include "src/developer/memory/metrics/printer.h"

namespace monitor {

using namespace memory;
using cobalt_registry::MemoryMigratedMetricDimensionBucket;
using TimeSinceBoot = cobalt_registry::MemoryLeakMigratedMetricDimensionTimeSinceBoot;

namespace {
static const std::map<zx_duration_t, TimeSinceBoot> UptimeLevelMap = {
    {zx_duration_from_min(1), TimeSinceBoot::Up},
    {zx_duration_from_min(30), TimeSinceBoot::UpOneMinute},
    {zx_duration_from_hour(1), TimeSinceBoot::UpThirtyMinutes},
    {zx_duration_from_hour(6), TimeSinceBoot::UpOneHour},
    {zx_duration_from_hour(12), TimeSinceBoot::UpSixHours},
    {zx_duration_from_hour(24), TimeSinceBoot::UpTwelveHours},
    {zx_duration_from_hour(48), TimeSinceBoot::UpOneDay},
    {zx_duration_from_hour(72), TimeSinceBoot::UpTwoDays},
    {zx_duration_from_hour(144), TimeSinceBoot::UpThreeDays},
};
}  // namespace

// Metrics polls the memory state periodically asynchroniously using the passed dispatcher and sends
// information about the memory Digests to Cobalt, in the form of several Events.
Metrics::Metrics(const std::vector<memory::BucketMatch>& bucket_matches,
                 zx::duration poll_frequency, async_dispatcher_t* dispatcher,
                 inspect::ComponentInspector* inspector,
                 fuchsia::metrics::MetricEventLogger_Sync* logger, CaptureCb capture_cb,
                 DigestCb digest_cb)
    : poll_frequency_(poll_frequency),
      dispatcher_(dispatcher),
      logger_(logger),
      capture_cb_(std::move(capture_cb)),
      digest_cb_(std::move(digest_cb)),
      bucket_name_to_code_({
          {"TotalBytes", MemoryMigratedMetricDimensionBucket::TotalBytes},
          {"Free", MemoryMigratedMetricDimensionBucket::Free},
          {"Kernel", MemoryMigratedMetricDimensionBucket::Kernel},
          {"Orphaned", MemoryMigratedMetricDimensionBucket::Orphaned},
          {"Undigested", MemoryMigratedMetricDimensionBucket::Undigested},
          {"[Addl]PagerTotal", MemoryMigratedMetricDimensionBucket::__Addl_PagerTotal},
          {"[Addl]PagerNewest", MemoryMigratedMetricDimensionBucket::__Addl_PagerNewest},
          {"[Addl]PagerOldest", MemoryMigratedMetricDimensionBucket::__Addl_PagerOldest},
          {"[Addl]DiscardableLocked",
           MemoryMigratedMetricDimensionBucket::__Addl_DiscardableLocked},
          {"[Addl]DiscardableUnlocked",
           MemoryMigratedMetricDimensionBucket::__Addl_DiscardableUnlocked},
      }),
      inspector_(inspector),
      platform_metric_node_(inspector_->root().CreateChild(kInspectPlatformNodeName)),
      // Diagram of hierarchy can be seen below:
      // root
      // - platform_metrics
      //   - memory_usages
      //     - Amber
      //     - Amlogic
      //     - ...
      //     - timestamp
      //   - memory_bandwidth
      //     - readings
      metric_memory_node_(platform_metric_node_.CreateChild(kMemoryNodeName)),
      inspect_memory_timestamp_(metric_memory_node_.CreateInt(kReadingMemoryTimestamp, 0)),
      metric_memory_bandwidth_node_(platform_metric_node_.CreateChild(kMemoryBandwidthNodeName)),
      inspect_memory_bandwidth_(
          metric_memory_bandwidth_node_.CreateUintArray(kReadings, kMemoryBandwidthArraySize)),
      inspect_memory_bandwidth_timestamp_(
          metric_memory_bandwidth_node_.CreateInt(kReadingMemoryTimestamp, 0)) {
  for (const auto& bucket : bucket_matches) {
    if (!bucket.event_code().has_value()) {
      continue;
    }
    bucket_name_to_code_.emplace(
        bucket.name(), static_cast<MemoryMigratedMetricDimensionBucket>(*bucket.event_code()));
  }
  for (const auto& element : bucket_name_to_code_) {
    inspect_memory_usages_.insert(std::pair<std::string, inspect::UintProperty>(
        element.first, metric_memory_node_.CreateUint(element.first, 0)));
  }

  task_.PostDelayed(dispatcher_, zx::usec(1));
}

void Metrics::CollectMetrics() {
  TRACE_DURATION("memory_monitor", "Watcher::Metrics::CollectMetrics");
  Capture capture;
  capture_cb_(&capture);
  Digest digest;
  digest_cb_(capture, &digest);

  WriteDigestToInspect(digest);

  std::vector<fuchsia::metrics::MetricEvent> events;
  const auto& kmem = capture.kmem();
  AddKmemEvents(kmem, &events);
  AddKmemEventsWithUptime(kmem, capture.time(), &events);
  auto builder = cobalt::MetricEventBuilder(cobalt_registry::kMemoryMigratedMetricId);
  for (const auto& bucket : digest.buckets()) {
    if (bucket.size() == 0) {
      continue;
    }
    const auto& code_iter = bucket_name_to_code_.find(bucket.name());
    if (code_iter == bucket_name_to_code_.end()) {
      FX_LOGS_FIRST_N(ERROR, 3) << "Metrics::CollectMetrics: Invalid bucket name: "
                                << bucket.name();
      continue;
    }
    events.push_back(builder.Clone().with_event_code(code_iter->second).as_integer(bucket.size()));
  }
  fuchsia::metrics::MetricEventLogger_LogMetricEvents_Result response =
      fpromise::error(fuchsia::metrics::Error::INTERNAL_ERROR);
  logger_->LogMetricEvents(std::move(events), &response);
  if (response.is_err() && response.err() == fuchsia::metrics::Error::INVALID_ARGUMENTS) {
    FX_LOGS(ERROR) << "LogMetricEvents() returned status INVALID_ARGUMENTS";
  }
  task_.PostDelayed(dispatcher_, poll_frequency_);
}

void Metrics::WriteDigestToInspect(const memory::Digest& digest) {
  TRACE_DURATION("memory_monitor", "Watcher::Metrics::WriteDigestToInspect");
  // The division is to address JSON int range problem in b/156523968
  // JSON does not suport 64 bit integers and some readers using JSON libraries will
  // crash if we write a value greater than MAX_INT_32.
  // The unit in seconds is sufficient.
  inspect_memory_timestamp_.Set(digest.time() / 1000000000);
  for (auto const& bucket : digest.buckets()) {
    auto it = inspect_memory_usages_.find(bucket.name());
    if (it == inspect_memory_usages_.end()) {
      FX_LOGS_FIRST_N(INFO, 3) << "Not_in_map: " << bucket.name() << ": " << bucket.size() << "\n";
      continue;
    }
    it->second.Set(bucket.size());
  }
}

void Metrics::AddKmemEvents(const zx_info_kmem_stats_t& kmem,
                            std::vector<fuchsia::metrics::MetricEvent>* events) {
  TRACE_DURATION("memory_monitor", "Metrics::AddKmemEvents");
  auto builder =
      cobalt::MetricEventBuilder(cobalt_registry::kMemoryGeneralBreakdownMigratedMetricId);
  using Breakdown = cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown;
  events->push_back(
      builder.Clone().with_event_code(Breakdown::TotalBytes).as_integer(kmem.total_bytes));
  events->push_back(builder.Clone()
                        .with_event_code(Breakdown::UsedBytes)
                        .as_integer(kmem.total_bytes - kmem.free_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::FreeBytes).as_integer(kmem.free_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::VmoBytes).as_integer(kmem.vmo_bytes));
  events->push_back(builder.Clone()
                        .with_event_code(Breakdown::KernelFreeHeapBytes)
                        .as_integer(kmem.free_heap_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::MmuBytes).as_integer(kmem.mmu_overhead_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::IpcBytes).as_integer(kmem.ipc_bytes));
  events->push_back(builder.Clone()
                        .with_event_code(Breakdown::KernelTotalHeapBytes)
                        .as_integer(kmem.total_heap_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::WiredBytes).as_integer(kmem.wired_bytes));
  events->push_back(
      builder.Clone().with_event_code(Breakdown::OtherBytes).as_integer(kmem.other_bytes));
}

// TODO(https://fxbug.dev/42113456): Refactor this when dedup enum is availble in generated
// cobalt config source code.
void Metrics::AddKmemEventsWithUptime(const zx_info_kmem_stats_t& kmem,
                                      const zx_time_t capture_time,
                                      std::vector<fuchsia::metrics::MetricEvent>* events) {
  TRACE_DURATION("memory_monitor", "Metrics::AddKmemEventsWithUptime");
  auto builder = std::move(cobalt::MetricEventBuilder(cobalt_registry::kMemoryLeakMigratedMetricId)
                               .with_event_code_at(1, GetUpTimeEventCode(capture_time)));
  using Breakdown = cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown;
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::TotalBytes).as_integer(kmem.total_bytes));
  events->push_back(builder.Clone()
                        .with_event_code_at(0, Breakdown::UsedBytes)
                        .as_integer(kmem.total_bytes - kmem.free_bytes));
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::FreeBytes).as_integer(kmem.free_bytes));
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::VmoBytes).as_integer(kmem.vmo_bytes));
  events->push_back(builder.Clone()
                        .with_event_code_at(0, Breakdown::KernelFreeHeapBytes)
                        .as_integer(kmem.free_heap_bytes));
  events->push_back(builder.Clone()
                        .with_event_code_at(0, Breakdown::MmuBytes)
                        .as_integer(kmem.mmu_overhead_bytes));
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::IpcBytes).as_integer(kmem.ipc_bytes));
  events->push_back(builder.Clone()
                        .with_event_code_at(0, Breakdown::KernelTotalHeapBytes)
                        .as_integer(kmem.total_heap_bytes));
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::WiredBytes).as_integer(kmem.wired_bytes));
  events->push_back(
      builder.Clone().with_event_code_at(0, Breakdown::OtherBytes).as_integer(kmem.other_bytes));
}

TimeSinceBoot Metrics::GetUpTimeEventCode(const zx_time_t capture_time) {
  zx_duration_t uptime = zx_duration_from_nsec(capture_time);
  for (auto const& map : UptimeLevelMap) {
    if (uptime < map.first) {
      return map.second;
    }
  }
  return TimeSinceBoot::UpSixDays;
}

void Metrics::NextMemoryBandwidthReading(uint64_t reading, zx_time_t ts) {
  inspect_memory_bandwidth_.Set(memory_bandwidth_index_, reading);
  inspect_memory_bandwidth_timestamp_.Set(ts);
  if (++memory_bandwidth_index_ >= kMemoryBandwidthArraySize)
    memory_bandwidth_index_ = 0;
}

}  // namespace monitor
