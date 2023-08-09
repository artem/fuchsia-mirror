// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/watcher.h"

#include <lib/async/cpp/task.h>
#include <lib/trace/event.h>
#include <lib/zx/time.h>
#include <stdio.h>
#include <unistd.h>

namespace memory {

Watcher::Watcher(zx::duration poll_frequency, uint64_t high_water_threshold,
                 async_dispatcher_t* dispatcher, CaptureFn capture_cb, HighWaterFn high_water_cb)
    : least_free_bytes_(UINT64_MAX),
      poll_frequency_(poll_frequency),
      high_water_threshold_(high_water_threshold),
      dispatcher_(dispatcher),
      capture_cb_(std::move(capture_cb)),
      high_water_cb_(std::move(high_water_cb)) {
  task_.PostDelayed(dispatcher_, zx::usec(1));
}

void Watcher::CaptureMemory() {
  TRACE_DURATION("memory_metrics", "Watcher::CaptureMemory");
  Capture c;
  capture_cb_(&c, CaptureLevel::KMEM);
  auto free_bytes = c.kmem().free_bytes;
  if ((free_bytes + high_water_threshold_) <= least_free_bytes_) {
    // Note: memory could have changed between the two captures, so we check
    // again.
    capture_cb_(&c, CaptureLevel::VMO);
    free_bytes = c.kmem().free_bytes;
    if ((free_bytes + high_water_threshold_) <= least_free_bytes_) {
      least_free_bytes_ = free_bytes;
      high_water_cb_(c);
    }
  }
  task_.PostDelayed(dispatcher_, poll_frequency_);
}

}  // namespace memory
