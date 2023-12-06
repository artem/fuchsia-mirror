// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_
#define ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_

// Get the generic file.
#include_next <lib/arch/asm.h>

#ifdef __ASSEMBLER__  // clang-format off

.macro assert.fail
  ud2
.endm

.macro .cfi.all_integer op
  .irp reg,%rax,%rbx,%rcx,%rdx,%rsi,%rdi,%rbp,%rsp,%r8,%r9,%r10,%r11,%r12,%r13,%r14,%r15
    \op \reg
  .endr
.endm

.macro .cfi.all_vectorfp op
  .irp reg,%xmm0,%xmm1,%xmm2,%xmm3,%xmm4,%xmm5,%xmm6,%xmm7,%xmm8,%xmm9,%xmm10,%xmm11,%xmm12,%xmm13,%xmm14,%xmm15,%mm0,%mm1,%mm2,%mm3,%mm4,%mm5,%mm6,%mm7,%st(0),%st(1),%st(2),%st(3),%st(4),%st(5),%st(6),%st(7)
    \op \reg
  .endr
.endm

// Standard prologue sequence for FP setup, with CFI.
// Note that this realigns the SP from entry state to be ready for a call.
.macro .prologue.fp
  push %rbp
  .cfi_adjust_cfa_offset 8
  .cfi_offset %rbp, -16
  mov %rsp, %rbp
.endm

// Epilogue sequence to match .prologue.fp.
.macro .epilogue.fp
  pop %rbp
  .cfi_same_value %rbp
  .cfi_adjust_cfa_offset -8
.endm

#endif  // clang-format on

#endif  // ZIRCON_KERNEL_LIB_ARCH_X86_INCLUDE_LIB_ARCH_ASM_H_
