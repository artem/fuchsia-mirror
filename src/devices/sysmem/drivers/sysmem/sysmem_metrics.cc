// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem_metrics.h"

#include <limits>

#include "src/devices/sysmem/metrics/metrics.cb.h"
#include "utils.h"

SysmemMetrics::SysmemMetrics()
    : metrics_buffer_(cobalt::MetricsBuffer::Create(sysmem_metrics::kProjectId)),
      unused_page_check_(
          metrics_buffer_->CreateMetricBuffer(sysmem_metrics::kUnusedPageCheckMetricId)),
      weak_vmo_events_(metrics_buffer_->CreateMetricBuffer(sysmem_metrics::kWeakVmoEventsMetricId)),
      weak_vmo_histograms_(metrics_buffer_->CreateHistogramMetricBuffer(
          COBALT_EXPONENTIAL_HISTOGRAM_INFO(::sysmem_metrics::kWeakVmoHistograms))) {}

cobalt::MetricsBuffer& SysmemMetrics::metrics_buffer() { return *metrics_buffer_; }

void SysmemMetrics::LogUnusedPageCheck(sysmem_metrics::UnusedPageCheckMetricDimensionEvent event) {
  unused_page_check_.LogEvent({event});
}

void SysmemMetrics::LogUnusedPageCheckCounts(uint32_t succeeded_count, uint32_t failed_count) {
  if (succeeded_count) {
    unused_page_check_pending_success_count_ += succeeded_count;
  }
  if (failed_count) {
    unused_page_check_.LogEventCount(
        {sysmem_metrics::UnusedPageCheckMetricDimensionEvent_PatternCheckFailed}, failed_count);
  }

  zx::time now = zx::clock::get_monotonic();
  if (((now >= unused_page_check_last_flush_time_ + kUnusedPageCheckFlushSuccessPeriod) &&
       unused_page_check_pending_success_count_) ||
      unused_page_check_pending_success_count_ >= std::numeric_limits<uint32_t>::max() / 2) {
    unused_page_check_.LogEventCount(
        {sysmem_metrics::UnusedPageCheckMetricDimensionEvent_PatternCheckOk},
        safe_cast<uint32_t>(unused_page_check_pending_success_count_));
    unused_page_check_pending_success_count_ = 0;
    unused_page_check_last_flush_time_ = now;
  }
}

void SysmemMetrics::LogCloseWeakAsapTakingTooLong() {
  weak_vmo_events_.LogEvent(
      {sysmem_metrics::WeakVmoEventsMetricDimensionEvent_CloseWeakAsapExceeded5Seconds});
}

void SysmemMetrics::LogCloseWeakAsapDuration(zx::duration duration) {
  weak_vmo_histograms_.LogValue(
      {sysmem_metrics::WeakVmoHistogramsMetricDimensionOperation_CloseWeakAsapComplete},
      duration.to_msecs());
}
