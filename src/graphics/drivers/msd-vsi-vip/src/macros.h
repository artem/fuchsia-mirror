// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_VSI_VIP_SRC_MACROS_H_
#define SRC_GRAPHICS_DRIVERS_MSD_VSI_VIP_SRC_MACROS_H_

#include <lib/magma/util/macros.h>

static inline bool fits_in_40_bits(uint64_t address) {
  return (address & 0xFFFFFF0000000000ull) == 0;
}

#endif  // SRC_GRAPHICS_DRIVERS_MSD_VSI_VIP_SRC_MACROS_H_
