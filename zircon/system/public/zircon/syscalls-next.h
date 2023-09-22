// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSCALLS_NEXT_H_
#define ZIRCON_SYSCALLS_NEXT_H_

#ifndef _KERNEL

#include <zircon/syscalls.h>

__BEGIN_CDECLS

// Make sure this matches <zircon/syscalls.h>.
#define _ZX_SYSCALL_DECL(name, type, attrs, nargs, arglist, prototype) \
  extern attrs type zx_##name prototype;                               \
  extern attrs type _zx_##name prototype;

#ifdef __clang__
#define _ZX_SYSCALL_ANNO(attr) __attribute__((attr))
#else
#define _ZX_SYSCALL_ANNO(attr)  // Nothing for compilers without the support.
#endif

#include <zircon/syscalls/internal/cdecls-next.inc>

#undef _ZX_SYSCALL_ANNO
#undef _ZX_SYSCALL_DECL

__END_CDECLS

#endif  // !_KERNEL

#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>

// ====== Pager writeback support ====== //

// Range type used by the zx_pager_query_dirty_ranges() syscall.
typedef struct zx_vmo_dirty_range {
  // Represents the range [offset, offset + length).
  uint64_t offset;
  uint64_t length;
  // Any options applicable to the range.
  // ZX_VMO_DIRTY_RANGE_IS_ZERO indicates that the range contains all zeros.
  uint64_t options;
} zx_vmo_dirty_range_t;

// options flags for zx_vmo_dirty_range_t
#define ZX_VMO_DIRTY_RANGE_IS_ZERO ((uint64_t)1u)

// ====== End of pager writeback support ====== //

// ====== Restricted mode support ====== //
// Structures used for the experimental restricted mode syscalls.
// Declared here in the next syscall header since it is not published
// in the SDK.

#define ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL ((uint32_t)1)

typedef uint64_t zx_restricted_reason_t;

// Reason codes provided to normal mode when a restricted process traps
// back to normal mode.
// clang-format off
#define ZX_RESTRICTED_REASON_SYSCALL   ((zx_restricted_reason_t)0)
#define ZX_RESTRICTED_REASON_EXCEPTION ((zx_restricted_reason_t)1)
#define ZX_RESTRICTED_REASON_KICK      ((zx_restricted_reason_t)2)
// clang-format on

// Structure to read and write restricted mode state
//
// When exiting restricted mode for certain reasons, additional information
// may be provided by zircon. However, regardless of the reason code this
// will always be the first structure inside the restricted mode state VMO.
#if __aarch64__
typedef struct zx_restricted_state {
  uint64_t x[31];
  uint64_t sp;
  uint64_t pc;
  uint64_t tpidr_el0;
  // Contains only the user-controllable upper 4-bits (NZCV).
  uint32_t cpsr;
  uint8_t padding1[4];
} zx_restricted_state_t;
#elif __x86_64__
typedef struct zx_restricted_state {
  // User space active registers
  uint64_t rdi, rsi, rbp, rbx, rdx, rcx, rax, rsp;
  uint64_t r8, r9, r10, r11, r12, r13, r14, r15;
  uint64_t ip, flags;

  uint64_t fs_base, gs_base;
} zx_restricted_state_t;
#elif __riscv
typedef zx_riscv64_thread_state_general_regs_t zx_restricted_state_t;
#else
#error what architecture?
#endif

// Structure populated by zircon when exiting restricted mode with the
// reason code `ZX_RESTRICTED_REASON_SYSCALL`.
typedef struct zx_restricted_syscall {
  // Must be first.
  zx_restricted_state_t state;
} zx_restricted_syscall_t;

// Structure populated by zircon when exiting restricted mode with the
// reason code `ZX_RESTRICTED_REASON_EXCEPTION`.
typedef struct zx_restricted_exception {
  // Must be first.
  zx_restricted_state_t state;
  zx_exception_report_t exception;
} zx_restricted_exception_t;

// ====== End of restricted mode support ====== //

// ====== Kernel-based memory attribution support ====== //
// Topic for zx_object_get_info.
#define ZX_INFO_MEMORY_ATTRIBUTION ((zx_object_info_topic_t)34u)  // zx_info_memory_attribution_t[n]

typedef struct zx_info_memory_attribution {
  // The koid of the process for which these attribution statistics apply.
  zx_koid_t process_koid;

  uint64_t private_resident_pages_allocated;
  uint64_t private_resident_pages_deallocated;

  uint64_t total_resident_pages_allocated;
  uint64_t total_resident_pages_deallocated;
} zx_info_memory_attribution_t;

// ====== End of kernel-based memory attribution support ====== //

// ====== IOB support ====== //
#define ZX_INFO_IOB ((zx_object_info_topic_t)35u)  // zx_info_iob_t[1]
#define ZX_INFO_IOB_REGIONS \
  ((zx_object_info_topic_t)36u)  // zx_iob_region_info_t[n]
                                 //
typedef struct zx_info_iob {
  // The value of the *options* parameter passed to `zx_iob_create`.
  uint64_t options;
  // The number of regions in the IOB.
  uint32_t region_count;
  uint8_t padding1[4];
} zx_info_iob_t;

typedef struct zx_iob_region_info {
  // The region description, with potentially swapped access bits.
  zx_iob_region_t region;

  /// The koid of the underlying memory object.
  zx_koid_t koid;
} zx_iob_region_info_t;
// ====== End of IOB support ====== //
#endif  // ZIRCON_SYSCALLS_NEXT_H_
