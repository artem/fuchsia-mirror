// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_REGS_H_
#define ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_REGS_H_

#define ARM64_IFRAME_OFFSET_R (0 * 8)
#define ARM64_IFRAME_OFFSET_LR (30 * 8)
#define ARM64_IFRAME_OFFSET_USP (31 * 8)
#define ARM64_IFRAME_OFFSET_ELR (32 * 8)
#define ARM64_IFRAME_OFFSET_SPSR (33 * 8)
#define ARM64_IFRAME_SIZE ((30 + 4) * 8)

#ifndef __ASSEMBLER__

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

// Registers saved on entering the kernel via architectural exception.
struct iframe_t {
  uint64_t r[30];
  uint64_t lr;
  uint64_t usp;
  uint64_t elr;
  uint64_t spsr;
};

static_assert(sizeof(iframe_t) % 16u == 0u);
static_assert(sizeof(iframe_t) == ARM64_IFRAME_SIZE);

static_assert(__offsetof(iframe_t, r[0]) == ARM64_IFRAME_OFFSET_R, "");
static_assert(__offsetof(iframe_t, lr) == ARM64_IFRAME_OFFSET_LR, "");
static_assert(__offsetof(iframe_t, usp) == ARM64_IFRAME_OFFSET_USP, "");
static_assert(__offsetof(iframe_t, elr) == ARM64_IFRAME_OFFSET_ELR, "");
static_assert(__offsetof(iframe_t, spsr) == ARM64_IFRAME_OFFSET_SPSR, "");

void PrintFrame(FILE* file, const iframe_t& frame);

// Registers saved on entering the kernel via syscall.
using syscall_regs_t = iframe_t;

#endif  // !__ASSEMBLER__

#endif  // ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_REGS_H_
