// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-calculate-utc.h"

#include "vdso-aux.h"
#include "vdso-calculate-monotonic.h"

int64_t calculate_utc_time_nsec() {
  int64_t monotonic_time = calculate_monotonic_time_nsec();

  // Mono to utc transform is read from vvar_data. The data is protected by a seqlock, and so
  // a seqlock reader is implemented
  // Check that the state of the seqlock shows that the transform is not being updated
  uint64_t seq_num1 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 & 1) {
    // Cannot read, because a write is in progress
    return UTC_INVALID;
  }
  int64_t mono_to_utc_reference_offset =
      vvar.mono_to_utc_reference_offset.load(std::memory_order_acquire);
  int64_t mono_to_utc_synthetic_offset =
      vvar.mono_to_utc_synthetic_offset.load(std::memory_order_acquire);
  uint32_t mono_to_utc_reference_ticks =
      vvar.mono_to_utc_reference_ticks.load(std::memory_order_acquire);
  uint32_t mono_to_utc_synthetic_ticks =
      vvar.mono_to_utc_synthetic_ticks.load(std::memory_order_acquire);
  // Check that the state of the seqlock has not changed while reading the transform
  uint64_t seq_num2 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 != seq_num2) {
    // Data has been updated during the reading of it, so is invalid
    return UTC_INVALID;
  }
  int64_t utc_nsec = (monotonic_time - mono_to_utc_reference_offset) * mono_to_utc_synthetic_ticks /
                         mono_to_utc_reference_ticks +
                     mono_to_utc_synthetic_offset;
  return utc_nsec;
}
