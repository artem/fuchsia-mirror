// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-calculate-monotonic.h"

#include "vdso-aux.h"

int64_t calculate_monotonic_time_nsec() {
  uint64_t raw_ticks = get_raw_ticks();
  uint64_t ticks = raw_ticks + vvar.raw_ticks_to_ticks_offset.load(std::memory_order_acquire);
  // TODO(mariagl): This could potentially overflow; Find a way to avoid this.
  uint64_t monot_nsec = ticks * vvar.ticks_to_mono_numerator.load(std::memory_order_acquire) /
                        vvar.ticks_to_mono_denominator.load(std::memory_order_acquire);
  return monot_nsec;
}
