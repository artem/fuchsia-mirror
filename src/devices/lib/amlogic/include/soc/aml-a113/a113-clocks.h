// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_A113_A113_CLOCKS_H_
#define SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_A113_A113_CLOCKS_H_

#include <lib/mmio-ptr/mmio-ptr.h>
#include <lib/mmio/mmio-buffer.h>

#define SDM_FRACTIONALITY ((uint32_t)16384)
#define A113_FIXED_PLL_RATE ((uint32_t)2000000000)
#define A113_CLOCKS_BASE_PHYS 0xff63c000

// Clock register offsets (all are 32-bit registers)
#define A113_HHI_MPLL_CNTL (0xa0)
#define A113_HHI_MPLL_CNTL8 (0xa8)
#define A113_HHI_PLL_TOP_MISC (0xba)

static inline uint32_t a113_clk_get_reg(fdf::MmioBuffer *dev, uint32_t offset) {
  return dev->Read32(offset);
}

static inline uint32_t a113_clk_set_reg(fdf::MmioBuffer *dev, uint32_t offset, uint32_t value) {
  dev->Write32(value, offset);
  return a113_clk_get_reg(dev, offset);
}

zx_status_t a113_clk_set_mpll2(fdf::MmioBuffer *device, uint64_t rate, uint64_t *actual);

#endif  // SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_A113_A113_CLOCKS_H_
