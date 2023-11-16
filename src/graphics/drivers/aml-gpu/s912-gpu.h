// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_AML_GPU_S912_GPU_H_
#define SRC_GRAPHICS_DRIVERS_AML_GPU_S912_GPU_H_

#include <soc/aml-s912/s912-hw.h>

#include "aml-gpu.h"

enum {
  S912_XTAL = 0,  // 25MHz
  S912_GP0 = 1,
  S912_MP2 = 2,
  S912_MP1 = 3,
  S912_FCLK_DIV7 = 4,  // 285.7 MHz
  S912_FCLK_DIV4 = 5,  // 500 MHz
  S912_FCLK_DIV3 = 6,  // 666 MHz
  S912_FCLK_DIV5 = 7,  // 400 MHz
};

static aml_gpu_block_t s912_gpu_blocks = {
    .reset0_level_offset = 4 * S912_RESET0_LEVEL,
    .reset0_mask_offset = 4 * S912_RESET0_MASK,
    .reset2_level_offset = 4 * S912_RESET2_LEVEL,
    .reset2_mask_offset = 4 * S912_RESET2_MASK,
    .hhi_clock_cntl_offset = 0x6C,
    .initial_clock_index = 2,
    .enable_gp0 = false,
    .non_gp0_index = 2,
    .gpu_clk_freq =
        {
            S912_FCLK_DIV7,
            S912_FCLK_DIV5,
            S912_FCLK_DIV4,
            S912_FCLK_DIV3,
        },
    .input_freq_map =
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
        {
            [S912_XTAL] = 24'000'000,
            [S912_GP0] = 0,
            [S912_MP2] = 0,
            [S912_MP1] = 0,
            [S912_FCLK_DIV7] = 285'714'285,
            [S912_FCLK_DIV4] = 500'000'000,
            [S912_FCLK_DIV3] = 666'666'666,
            [S912_FCLK_DIV5] = 400'000'000,
        },
#pragma GCC diagnostic pop
};

#endif  // SRC_GRAPHICS_DRIVERS_AML_GPU_S912_GPU_H_
