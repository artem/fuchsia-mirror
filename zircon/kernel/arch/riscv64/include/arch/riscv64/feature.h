// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_

#include <stdbool.h>
#include <stdint.h>

// RISC-V features
extern bool riscv_feature_svpbmt;

void riscv64_feature_early_init();
void riscv64_feature_init();

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_
