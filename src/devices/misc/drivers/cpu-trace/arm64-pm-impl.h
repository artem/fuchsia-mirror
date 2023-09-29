// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_MISC_DRIVERS_CPU_TRACE_ARM64_PM_IMPL_H_
#define SRC_DEVICES_MISC_DRIVERS_CPU_TRACE_ARM64_PM_IMPL_H_
#include <stdint.h>

namespace perfmon {

struct StagingState {
  // Maximum number of each event we can handle.
  unsigned max_num_fixed;
  unsigned max_num_programmable;

  // The number of events in use.
  unsigned num_fixed;
  unsigned num_programmable;

  // The maximum value the counter can have before overflowing.
  uint64_t max_fixed_value;
  uint64_t max_programmable_value;
};

}  // namespace perfmon
#endif  // SRC_DEVICES_MISC_DRIVERS_CPU_TRACE_ARM64_PM_IMPL_H_
