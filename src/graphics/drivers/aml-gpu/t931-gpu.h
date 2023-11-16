// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_AML_GPU_T931_GPU_H_
#define SRC_GRAPHICS_DRIVERS_AML_GPU_T931_GPU_H_

#include <soc/aml-t931/t931-hw.h>

#include "aml-gpu.h"

enum {
  T931_XTAL = 0,  // 24MHz
  T931_GP0 = 1,   // Not used currently.
  T931_HIFI = 2,
  T931_FCLK_DIV2P5 = 3,  // 800 MHz
  T931_FCLK_DIV3 = 4,    // 666 MHz
  T931_FCLK_DIV4 = 5,    // 500 MHz
  T931_FCLK_DIV5 = 6,    // 400 MHz
  T931_FCLK_DIV7 = 7,    // 285.7 MHz
};

static aml_gpu_block_t t931_gpu_blocks = {
    .reset0_level_offset = T931_RESET0_LEVEL,
    .reset0_mask_offset = T931_RESET0_MASK,
    .reset2_level_offset = T931_RESET2_LEVEL,
    .reset2_mask_offset = T931_RESET2_MASK,
    .hhi_clock_cntl_offset = 0x6C,
    .initial_clock_index = 4,
    .enable_gp0 = false,
    .non_gp0_index = 4,
    .gpu_clk_freq =
        {
            T931_FCLK_DIV7,
            T931_FCLK_DIV5,
            T931_FCLK_DIV4,
            T931_FCLK_DIV3,
            T931_FCLK_DIV2P5,
        },
    .input_freq_map =
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
        {
            [T931_XTAL] = 24'000'000,
            [T931_GP0] = 0,
            [T931_HIFI] = 0,
            [T931_FCLK_DIV2P5] = 800'000'000,
            [T931_FCLK_DIV3] = 666'666'666,
            [T931_FCLK_DIV4] = 500'000'000,
            [T931_FCLK_DIV5] = 400'000'000,
            [T931_FCLK_DIV7] = 285'714'285,
        },
#pragma GCC diagnostic pop
};

#endif  // SRC_GRAPHICS_DRIVERS_AML_GPU_T931_GPU_H_
