// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_METRICS_WATCHER_H_
#define SRC_DEVELOPER_MEMORY_METRICS_WATCHER_H_

#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/fit/function.h>

#include "src/developer/memory/metrics/capture.h"
#include "src/lib/fxl/macros.h"

namespace memory {

// Used by Watcher to capture memory periodically.
using CaptureFn = fit::function<zx_status_t(Capture*, CaptureLevel)>;
// Used by Watcher to notify when new high water marks have been reached.
using HighWaterFn = fit::function<void(const Capture&)>;

// Watches memory usage and reports back when memory reaches a new high.
class Watcher {
 public:
  // Constructs a new Watcher which will check memory usage at the rate
  // specified by |poll_frequency|, using the |async_dispatcher|.
  // Each time usage increasess by at least |high_water_threshold| the
  // |high_water_cb| will be called.
  // |capture_cb| is used to access memory usage.
  Watcher(zx::duration poll_frequency, uint64_t high_water_threshold,
          async_dispatcher_t* dispatcher, CaptureFn capture_cb, HighWaterFn high_water_cb);
  ~Watcher() = default;

 private:
  void CaptureMemory();

  uint64_t least_free_bytes_;
  zx::duration poll_frequency_;
  uint64_t high_water_threshold_;
  async_dispatcher_t* dispatcher_;
  CaptureFn capture_cb_;
  HighWaterFn high_water_cb_;
  async::TaskClosureMethod<Watcher, &Watcher::CaptureMemory> task_{this};

  FXL_DISALLOW_COPY_AND_ASSIGN(Watcher);
};

}  // namespace memory

#endif  // SRC_DEVELOPER_MEMORY_METRICS_WATCHER_H_
