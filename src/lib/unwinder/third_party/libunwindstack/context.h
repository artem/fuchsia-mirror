/*
 * Copyright (C) 2016 The Android Open Source Project
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 *  * Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 *  * Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in
 *    the documentation and/or other materials provided with the
 *    distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
 * "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
 * LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS
 * FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
 * COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING,
 * BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS
 * OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED
 * AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
 * OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF
 * SUCH DAMAGE.
 */

#ifndef SRC_LIB_UNWINDER_THIRD_PARTY_LIBUNWINDSTACK_CONTEXT_H_
#define SRC_LIB_UNWINDER_THIRD_PARTY_LIBUNWINDSTACK_CONTEXT_H_

#include <cstddef>
#include <cstdint>

#include "src/lib/unwinder/registers.h"

namespace unwinder {

#if defined(__aarch64__)

[[gnu::always_inline]] inline void AsmGetRegs(void* reg_data) {
  asm volatile(
      "1:\n"
      "stp x0, x1, [%[base], #0]\n"
      "stp x2, x3, [%[base], #16]\n"
      "stp x4, x5, [%[base], #32]\n"
      "stp x6, x7, [%[base], #48]\n"
      "stp x8, x9, [%[base], #64]\n"
      "stp x10, x11, [%[base], #80]\n"
      "stp x12, x13, [%[base], #96]\n"
      "stp x14, x15, [%[base], #112]\n"
      "stp x16, x17, [%[base], #128]\n"
      "stp x18, x19, [%[base], #144]\n"
      "stp x20, x21, [%[base], #160]\n"
      "stp x22, x23, [%[base], #176]\n"
      "stp x24, x25, [%[base], #192]\n"
      "stp x26, x27, [%[base], #208]\n"
      "stp x28, x29, [%[base], #224]\n"
      "str x30, [%[base], #240]\n"
      "mov x12, sp\n"
      "adr x13, 1b\n"
      "stp x12, x13, [%[base], #248]\n"
      : [base] "+r"(reg_data)
      :
      : "x12", "x13", "memory");
}

constexpr auto kCurrentArch = Registers::Arch::kArm64;

#elif defined(__x86_64__)

extern "C" void AsmGetRegs(void* regs);
constexpr auto kCurrentArch = Registers::Arch::kX64;

#elif defined(__riscv)

[[gnu::always_inline]] inline void AsmGetRegs(void* reg_data) {
  asm volatile(
      "1:\n"
      "sd ra, 8(%[base])\n"
      "sd sp, 16(%[base])\n"
      "sd gp, 24(%[base])\n"
      "sd tp, 32(%[base])\n"
      "sd t0, 40(%[base])\n"
      "sd t1, 48(%[base])\n"
      "sd t2, 56(%[base])\n"
      "sd s0, 64(%[base])\n"
      "sd s1, 72(%[base])\n"
      "sd a0, 80(%[base])\n"
      "sd a1, 88(%[base])\n"
      "sd a2, 96(%[base])\n"
      "sd a3, 104(%[base])\n"
      "sd a4, 112(%[base])\n"
      "sd a5, 120(%[base])\n"
      "sd a6, 128(%[base])\n"
      "sd a7, 136(%[base])\n"
      "sd s2, 144(%[base])\n"
      "sd s3, 152(%[base])\n"
      "sd s4, 160(%[base])\n"
      "sd s5, 168(%[base])\n"
      "sd s6, 176(%[base])\n"
      "sd s7, 184(%[base])\n"
      "sd s8, 192(%[base])\n"
      "sd s9, 200(%[base])\n"
      "sd s10, 208(%[base])\n"
      "sd s11, 216(%[base])\n"
      "sd t3, 224(%[base])\n"
      "sd t4, 232(%[base])\n"
      "sd t5, 240(%[base])\n"
      "sd t6, 248(%[base])\n"
      "la t1, 1b\n"
      "sd t1, 0(%[base])\n"
      : [base] "+r"(reg_data)
      :
      : "t1", "memory");
}
constexpr auto kCurrentArch = Registers::Arch::kRiscv64;

#else
#error What platform?
#endif

[[gnu::always_inline]] inline Registers GetContext() {
  constexpr auto kRegLast = static_cast<uint8_t>([]() {
    switch (kCurrentArch) {
      case Registers::Arch::kX64:
        return RegisterID::kX64_last;
      case Registers::Arch::kArm64:
        return RegisterID::kArm64_last;
      case Registers::Arch::kRiscv64:
        return RegisterID::kRiscv64_last;
    }
  }());

  uint64_t regs[kRegLast];
  AsmGetRegs(regs);

  Registers res(kCurrentArch);
  uint8_t i = 0;
  if constexpr (kCurrentArch == Registers::Arch::kRiscv64) {
    // RegisterID::kRiscv64_pc is not 0.
    res.SetPC(regs[0]);
    i = 1;
  }
  for (; i < kRegLast; i++) {
    res.Set(static_cast<RegisterID>(i), regs[i]);
  }
  return res;
}

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_THIRD_PARTY_LIBUNWINDSTACK_CONTEXT_H_
