// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCHEDULING_FRAME_STATS_H_
#define SRC_UI_SCENIC_LIB_SCHEDULING_FRAME_STATS_H_

#include <lib/async/cpp/task.h>
#include <lib/zx/time.h>

#include <deque>
#include <functional>

#include "lib/inspect/cpp/inspect.h"
#include "src/ui/scenic/lib/scheduling/frame_scheduler.h"
#include "src/ui/scenic/lib/utils/metrics.h"
#include "third_party/cobalt/src/lib/client/cpp/buckets_config.h"

namespace scheduling {

// Class for managing and reporting frame stats from FrameScheduler. Used for debug data, i.e.
// inspect.
class FrameStats {
 public:
  FrameStats(inspect::Node inspect_node, metrics::Metrics* metrics_logger);

  struct Timestamps {
    zx::time latch_point_time;
    zx::time render_start_time;
    zx::time render_done_time;
    zx::time target_presentation_time;
    zx::time actual_presentation_time;
  };

  void RecordFrame(Timestamps timestamps, zx::duration display_vsync_interval);

  // Time interval between each flush is 10 minutes.
  static constexpr zx::duration kCobaltDataCollectionInterval = zx::min(10);

  // Delta being 0 or very close to 0 can happen by chance and skew results. Use a minimum delta
  // of 5ms when reporting results.
  static constexpr zx::duration kMinTimeElapsedInCurrentMin = zx::msec(5);

 private:
  typedef std::unordered_map<uint32_t, uint32_t> CobaltFrameHistogram;
  static constexpr size_t kNumFramesToReport = 200;
  static constexpr size_t kNumDroppedFramesToReport = 50;
  static constexpr size_t kNumDelayedFramesToReport = 50;
  static constexpr size_t kNumMinutesHistory = 10;

  struct HistoryStats {
    // The key for this history.
    // Calculated by truncating the frame timestamp (to minutes).
    //
    // This denotes the interval for the following measurements.
    uint64_t key;

    // The total number of frames that attempted rendering during the interval.
    uint64_t total_frames;

    // The total number of frames that were successfully rendered during the interval.
    uint64_t rendered_frames;

    // The number of rendered frames that were delayed during the interval.
    uint64_t delayed_rendered_frames;

    // The total amount of time spent rendering the rendered frames during the interval.
    zx::duration render_time;

    // The total amount of time spent rendering just the delayed frames during the interval.
    zx::duration delayed_frame_render_time;

    // The total number of frames that were dropped during the interval.
    uint64_t dropped_frames;

    // The latest frame's timestamp.
    zx::time timestamp;

    HistoryStats& operator+=(const HistoryStats& other) {
      key = std::max(key, other.key);
      total_frames += other.total_frames;
      rendered_frames += other.rendered_frames;
      delayed_rendered_frames += other.delayed_rendered_frames;
      render_time += other.render_time;
      delayed_frame_render_time += other.delayed_frame_render_time;
      dropped_frames += other.dropped_frames;
      timestamp = std::max(timestamp, other.timestamp);

      return *this;
    }

    template <typename T>
    void RecordToNode(inspect::Node* node, T* list, zx::duration duration) const {
      node->CreateUint("minute_key", key, list);
      node->CreateInt("total_frames", total_frames, list);
      node->CreateInt("rendered_frames", rendered_frames, list);
      node->CreateInt("delayed_rendered_frames", delayed_rendered_frames, list);
      node->CreateInt("render_time_ns", render_time.get(), list);
      node->CreateInt("delayed_frame_render_time_ns", delayed_frame_render_time.get(), list);
      node->CreateInt("dropped_frames", dropped_frames, list);
      if (rendered_frames) {
        node->CreateInt("average_time_per_frame_ms", render_time.to_msecs() / rendered_frames,
                        list);
      }
      if (delayed_rendered_frames) {
        node->CreateInt("average_time_per_delayed_frame_ms",
                        delayed_frame_render_time.to_msecs() / delayed_rendered_frames, list);
      }
      // Skip recording skewed fps results.
      if (duration > kMinTimeElapsedInCurrentMin) {
        node->CreateDouble(
            "average_frames_per_second",
            static_cast<double>(rendered_frames) / static_cast<double>(duration / zx::sec(1)),
            list);
      }
    }
  };

  static zx::duration CalculateMeanDuration(
      const std::deque<Timestamps>& timestamps,
      std::function<zx::duration(const Timestamps&)> duration_func, uint32_t percentile);

  void AddHistory(const HistoryStats& stats);
  void RecordDroppedFrame(const Timestamps& timestamps);
  void RecordDelayedFrame(const Timestamps& timestamps);
  void RecordOnTimeFrame(const Timestamps& timestamps);

  void ReportStats(inspect::Inspector* insp) const;

  // Note that both scenic_render_time and scenic_latch_to_actual_presentation
  // metrics in Cobalt has the same histogram settings. Therefore we create only
  // one bucket config for both usages.
  void InitializeFrameTimeBucketConfig();

  uint32_t GetCobaltBucketIndex(zx::duration duration);

  // Flush all histograms into Cobalt and empty them.
  void LogFrameTimes();

  // Helper function to convert histograms in unordered_map format to Cobalt-readable
  // vector of histogram buckets.
  std::vector<fuchsia_metrics::HistogramBucket> CreateMetricsBucketsFromHistogram(
      const CobaltFrameHistogram& histogram);

  uint64_t frame_count_ = 0;
  uint64_t dropped_frame_count_ = 0;
  uint64_t delayed_frame_count_ = 0;

  // Ring buffer of the last kNum*FramesToReport.
  std::deque<Timestamps> frame_times_;
  std::deque<Timestamps> dropped_frames_;
  std::deque<Timestamps> delayed_frames_;

  // Ring buffer of stats for the last kNumMinutesHistory minutes of frame timings.
  std::deque<HistoryStats> history_stats_;

  inspect::Node inspect_node_;
  inspect::LazyNode inspect_frame_stats_dump_;

  // Histograms for collecting latch point to actual presentation times.
  CobaltFrameHistogram cobalt_on_time_frame_times_histogram_;
  CobaltFrameHistogram cobalt_dropped_frame_times_histogram_;
  CobaltFrameHistogram cobalt_delayed_frame_times_histogram_;

  // Histogram for collecting render start to render done times.
  CobaltFrameHistogram cobalt_render_times_histogram_;

  // Used for getting the cobalt histogram bucket number given a frame time number.
  std::unique_ptr<cobalt::config::IntegerBucketConfig> frame_times_bucket_config_;

  metrics::Metrics* metrics_logger_;

  async::TaskClosureMethod<FrameStats, &FrameStats::LogFrameTimes> cobalt_logging_task_{this};
};

}  // namespace scheduling

#endif  // SRC_UI_SCENIC_LIB_SCHEDULING_FRAME_STATS_H_
