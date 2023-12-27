// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_MONITOR_MONITOR_H_
#define SRC_DEVELOPER_MEMORY_MONITOR_MONITOR_H_

#include <fuchsia/hardware/ram/metrics/cpp/fidl.h>
#include <fuchsia/memory/cpp/fidl.h>
#include <fuchsia/memory/inspection/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/trace/observer.h>
#include <lib/zx/socket.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <memory>

#include "lib/sys/inspect/cpp/component.h"
#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/digest.h"
#include "src/developer/memory/monitor/debugger.h"
#include "src/developer/memory/monitor/high_water.h"
#include "src/developer/memory/monitor/logger.h"
#include "src/developer/memory/monitor/metrics.h"
#include "src/developer/memory/monitor/pressure_notifier.h"
#include "src/lib/fxl/command_line.h"

namespace monitor {

namespace test {
class MonitorUnitTest;
class MemoryBandwidthInspectTest;
}  // namespace test

class Monitor : public fuchsia::memory::Monitor, public fuchsia::memory::inspection::Collector {
 public:
  Monitor(std::unique_ptr<sys::ComponentContext> context, const fxl::CommandLine& command_line,
          async_dispatcher_t* dispatcher, bool send_metrics, bool watch_memory_pressure,
          bool send_critical_pressure_crash_reports);
  ~Monitor();

  // For memory bandwidth measurement, SetRamDevice should be called once
  void SetRamDevice(fuchsia::hardware::ram::metrics::DevicePtr ptr);

  // Writes a memory capture and the bucket definition to |socket| in JSON,
  // in UTF-8.
  // See the fuchsia.memory.inspection FIDL library for a
  // description of the format of the JSON.
  void CollectJsonStats(zx::socket socket) override;

  void CollectJsonStatsWithOptions(
      fuchsia::memory::inspection::CollectorCollectJsonStatsWithOptionsRequest request) override;

  static const char kTraceName[];

 private:
  void CollectJsonStatsWithOptions(zx::socket socket);

  void PublishBucketConfiguration();

  void CreateMetrics(const std::vector<memory::BucketMatch>& bucket_matches);

  void UpdateState();

  void StartTracing();
  void StopTracing();

  void SampleAndPost();
  void MeasureBandwidthAndPost();
  void PeriodicMeasureBandwidth();
  void PrintHelp();
  inspect::Inspector Inspect(const std::vector<memory::BucketMatch>& bucket_matches);

  zx_status_t GetCapture(memory::Capture* capture, std::unique_ptr<memory::CaptureStrategy> filter);
  void GetDigest(const memory::Capture& capture, memory::Digest* digest);
  void PressureLevelChanged(Level level);

  memory::CaptureState capture_state_;
  std::unique_ptr<HighWater> high_water_;
  uint64_t prealloc_size_;
  zx::vmo prealloc_vmo_;
  bool logging_;
  bool tracing_;
  zx::duration delay_;
  zx_handle_t root_;
  async_dispatcher_t* dispatcher_;
  std::unique_ptr<sys::ComponentContext> component_context_;
  fuchsia::metrics::MetricEventLoggerSyncPtr metric_event_logger_;
  fidl::BindingSet<fuchsia::memory::inspection::Collector> bindings_;
  fidl::BindingSet<fuchsia::memory::Monitor> deprecated_bindings_;
  trace::TraceObserver trace_observer_;
  sys::ComponentInspector inspector_;
  Logger logger_;
  std::unique_ptr<Metrics> metrics_;
  std::unique_ptr<PressureNotifier> pressure_notifier_;
  std::unique_ptr<MemoryDebugger> memory_debugger_;
  std::unique_ptr<memory::Digester> digester_;
  std::mutex digester_mutex_;
  fuchsia::hardware::ram::metrics::DevicePtr ram_device_;
  uint64_t pending_bandwidth_measurements_ = 0;
  Level level_;

  friend class test::MonitorUnitTest;
  friend class test::MemoryBandwidthInspectTest;
  FXL_DISALLOW_COPY_AND_ASSIGN(Monitor);
};

}  // namespace monitor

#endif  // SRC_DEVELOPER_MEMORY_MONITOR_MONITOR_H_
