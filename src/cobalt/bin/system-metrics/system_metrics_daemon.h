// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// The cobalt system metrics collection daemon uses cobalt to log system metrics
// on a regular basis.

#ifndef SRC_COBALT_BIN_SYSTEM_METRICS_SYSTEM_METRICS_DAEMON_H_
#define SRC_COBALT_BIN_SYSTEM_METRICS_SYSTEM_METRICS_DAEMON_H_

#include <fuchsia/metrics/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>

#include <chrono>
#include <memory>
#include <thread>
#include <unordered_map>
#include <vector>

#include "src/cobalt/bin/system-metrics/activity_listener.h"
#include "src/cobalt/bin/system-metrics/cpu_stats_fetcher.h"
#include "src/cobalt/bin/system-metrics/metrics_registry.cb.h"
#include "src/cobalt/bin/utils/clock.h"
#include "third_party/cobalt/src/lib/client/cpp/buckets_config.h"

// A daemon to send system metrics to Cobalt.
//
// Usage:
//
// async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
// std::unique_ptr<sys::ComponentContext> context(
//     sys::ComponentContext::CreateAndServeOutgoingDirectory());
// SystemMetricsDaemon daemon(loop.dispatcher(), context.get());
// daemon.StartLogging();
// loop.Run();
class SystemMetricsDaemon {
 public:
  // Constructor
  //
  // |dispatcher|. This is used to schedule future work.
  //
  // |context|. The Cobalt LoggerFactory interface is fetched from this context.
  SystemMetricsDaemon(async_dispatcher_t* dispatcher, sys::ComponentContext* context);

  // Starts asynchronously logging all system metrics.
  void StartLogging();

  // Reader side must use the exact name to read from Inspect.
  // Design doc in go/fuchsia-metrics-to-inspect-design.
  // Details about config file are in b/152076901#comment6.
  static constexpr const char* kInspecPlatformtNodeName = "platform_metrics";

  // Details about config file are in b/152073842#comment6.
  static constexpr const char* kCPUNodeName = "cpu";
  static constexpr const char* kReadingCPUMax = "max";
  static constexpr const char* kReadingCPUMean = "mean";
  static constexpr size_t kCPUArraySize = 6;

 private:
  friend class SystemMetricsDaemonTest;
  friend class SystemMetricsDaemonInitializationTest;

  struct MetricSpecs {
    static const MetricSpecs INVALID;

    uint32_t customer_id;
    uint32_t project_id;
    uint32_t metric_id;

    bool is_valid() const { return customer_id > 0 && project_id > 0 && metric_id > 0; }
  };

  // This private constructor is intended for use in tests. |context| may
  // be null because InitializeLogger() will not be invoked. Instead,
  // pass a non-null |logger| which may be a local mock that does not use FIDL.
  SystemMetricsDaemon(async_dispatcher_t* dispatcher, sys::ComponentContext* context,
                      fuchsia::metrics::MetricEventLogger_Sync* logger,
                      std::unique_ptr<cobalt::SteadyClock> clock,
                      std::unique_ptr<cobalt::CpuStatsFetcher> cpu_stats_fetcher,
                      std::unique_ptr<cobalt::ActivityListener> activity_listener,
                      std::string activation_file_prefix);

  void InitializeLogger();

  void InitializeRootResourceHandle();

  // If the peer has closed the FIDL connection, automatically reconnect.
  zx_status_t ReinitializeIfPeerClosed(zx_status_t zx_status);

  // Calls LogFuchsiaUpPing,
  // and then uses the |dispatcher| passed to the constructor to
  // schedule the next round.
  void RepeatedlyLogUpPing();

  // Calls LogLifetimeEventActivation and LogLifetimeEventBoot.
  void LogLifetimeEvents();

  // Calls LogFuchsiaLifetimeEventActivation,
  // and then uses the |dispatcher| passed to the constructor to
  // schedule the next round.
  void LogLifetimeEventActivation();

  // Calls LogFuchsiaLifetimeEventBoot,
  // and then uses the |dispatcher| passed to the constructor to
  // schedule the next round.
  void LogLifetimeEventBoot();

  // Calls LogFuchsiaUptime and then uses the |dispatcher| passed to the
  // constructor to schedule the next round.
  void RepeatedlyLogUptime();

  // Calls LogCpuUsage,
  // then uses the |dispatcher| passed to the constructor to schedule
  // the next round.
  void RepeatedlyLogCpuUsage();

  // Calls LogActiveTime,
  // then uses the |dispatcher| passed to the constructor to schedule
  // the next round.
  void RepeatedlyLogActiveTime();

  // Create linear bucket config with the bucket_floor, number of buckets and step size.
  std::unique_ptr<cobalt::config::IntegerBucketConfig> InitializeLinearBucketConfig(
      int64_t bucket_floor, int32_t num_buckets, int32_t step_size);

  // Returns the amount of time since SystemMetricsDaemon started.
  std::chrono::seconds GetUpTime();

  // Logs one or more UpPing events depending on how long the device has been
  // up.
  //
  // |uptime| An estimate of how long since device boot time.
  //
  // First the "Up" event is logged indicating only that the device is up.
  //
  // If the device has been up for at least a minute then "UpOneMinute" is also
  // logged.
  //
  // If the device has been up for at least 10 minutes, then "UpTenMinutes" is
  // also logged. Etc.
  //
  // Returns the amount of time before this method needs to be invoked again.
  std::chrono::seconds LogFuchsiaUpPing(std::chrono::seconds uptime);

  // Logs one FuchsiaLifetimeEvent event of type "Boot".
  //
  // Returns the logging status.
  bool LogFuchsiaLifetimeEventBoot();

  // Logs one FuchsiaLifetimeEvent event of type "Activation".
  //
  // Returns the logging status.
  bool LogFuchsiaLifetimeEventActivation();

  // Once per hour, rounds the current uptime down to the nearest number of
  // hours and logs an event for the fuchsia_uptime metric.
  //
  // Returns the amount of time before this method needs to be invoked again.
  // This is the number of seconds until the uptime reaches the next full hour.
  std::chrono::seconds LogFuchsiaUptime();

  // Fetches and logs system-wide CPU usage.
  //
  // Returns the amount of time before this method needs to be invoked again.
  std::chrono::seconds LogCpuUsage();

  // Logs active minutes since the last call.
  //
  // Returns the amount of time before this method needs to be invoked again.
  std::chrono::seconds LogActiveTime();

  // Helper function to store the fetched CPU data and store until flush.
  void StoreCpuData(double cpu_percentage);  // histogram, flush every 10 min

  // Helper function to call Cobalt logger's LogCobaltEvent to log
  // cpu percentages.
  bool LogCpuToCobalt();  // INT_HISTOGRAM metric type

  // Callback function to be called by ActivityListener to update current_state_
  void UpdateState(fuchsia::ui::activity::State state);

  async_dispatcher_t* const dispatcher_;
  sys::ComponentContext* context_;
  fuchsia::metrics::MetricEventLoggerFactorySyncPtr factory_;
  fuchsia::metrics::MetricEventLoggerSyncPtr logger_fidl_proxy_;
  fuchsia::metrics::MetricEventLogger_Sync* logger_;
  std::chrono::steady_clock::time_point start_time_;
  std::unique_ptr<cobalt::SteadyClock> clock_;
  std::unique_ptr<cobalt::CpuStatsFetcher> cpu_stats_fetcher_;
  std::unique_ptr<cobalt::ActivityListener> activity_listener_;
  fuchsia::ui::activity::State current_state_ = fuchsia::ui::activity::State::UNKNOWN;
  fidl::InterfacePtr<fuchsia::ui::activity::Provider> activity_provider_;
  std::string activation_file_prefix_;

  inspect::ComponentInspector inspector_;
  inspect::Node platform_metric_node_;

  inspect::Node metric_cpu_node_;
  inspect::DoubleArray inspect_cpu_max_;
  inspect::DoubleArray inspect_cpu_mean_;
  double cpu_usage_accumulator_ = 0;
  double cpu_usage_max_ = 0;
  size_t cpu_array_index_ = 0;

  std::chrono::steady_clock::time_point active_start_time_;
  std::chrono::steady_clock::duration unlogged_active_duration_;
  std::mutex active_time_mutex_;

  template <typename T>
  T GetCobaltEventCodeForDeviceState(fuchsia::ui::activity::State state) {
    switch (state) {
      case fuchsia::ui::activity::State::IDLE:
        return T::Idle;
      case fuchsia::ui::activity::State::ACTIVE:
        return T::Active;
      case fuchsia::ui::activity::State::UNKNOWN:
        return T::Unknown;
    }
  }
  struct CpuWithActivityState {
    double cpu_percentage;
    fuchsia::ui::activity::State state;
  };
  std::unordered_map<fuchsia::ui::activity::State, std::unordered_map<uint32_t, uint32_t>>
      activity_state_to_cpu_map_;
  uint32_t cpu_data_stored_ = 0;
  // This bucket config is used to calculate the histogram bucket index for a given cpu percentage.
  // Usage: cpu_bucket_config_->BucketIndex(cpu_percentage * 100)
  std::unique_ptr<cobalt::config::IntegerBucketConfig> cpu_bucket_config_;
};

#endif  // SRC_COBALT_BIN_SYSTEM_METRICS_SYSTEM_METRICS_DAEMON_H_
