// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_VDSO_CONSTANTS_H_
#define ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_VDSO_CONSTANTS_H_

// This file is used both in the kernel and in the vDSO implementation.
// So it must be compatible with both the kernel and userland header
// environments.  It must use only the basic types so that struct
// layouts match exactly in both contexts.

#include <arch/defines.h>  // Defines PAGE_SIZE usable from assembly.

// The constants are put on their own whole page though the actual struct
// is much smaller. Eventually, this will be used to change the contents
// after boot. For now, it just ensures that there's always a bunch of free
// space at the end where the version string can go.
#define VDSO_CONSTANTS_ALIGN PAGE_SIZE
#define VDSO_CONSTANTS_SIZE PAGE_SIZE

#ifndef __ASSEMBLER__

#include <stdint.h>
#include <zircon/time.h>

// This struct contains constants that are initialized by the kernel
// once at boot time.  From the vDSO code's perspective, they are
// read-only data that can never change.  Hence, no synchronization is
// required to read them.
struct vdso_constants {
  // Maximum number of CPUs that might be online during the lifetime
  // of the booted system.
  uint32_t max_num_cpus;

  // Bit map indicating features.  For specific feature bits, see
  // zircon/features.h.
  // TODO(fxbug.dev/30418): This struct may need to grow over time as new features
  // are added and/or supported.  A mask may be needed to indicate which
  // bits are valid.
  struct {
    uint32_t cpu;

    // Total amount of debug registers available in the system.
    uint32_t hw_breakpoint_count;
    uint32_t hw_watchpoint_count;

    // Bitmask indicating which address tagging features are available.
    uint32_t address_tagging;

    // Bitmask for vm related features.
    uint32_t vm;
  } features;

  // Number of bytes in a data cache line.
  uint32_t dcache_line_size;

  // Number of bytes in an instruction cache line.
  uint32_t icache_line_size;

  // System page size in bytes. Guaranteed to be a power of 2.
  uint32_t page_size;

  uint32_t padding;

  // Conversion factor for zx_ticks_get return values to seconds.
  zx_ticks_t ticks_per_second;

  // Offset for converting from the raw system timer to zx_ticks_t
  zx_ticks_t raw_ticks_to_ticks_offset;

  // Ratio which relates ticks (zx_ticks_get) to clock monotonic (zx_clock_get_monotonic).
  // Specifically...
  //
  // ClockMono(ticks) = (ticks * N) / D
  //
  uint32_t ticks_to_mono_numerator;
  uint32_t ticks_to_mono_denominator;

  // Total amount of physical memory in the system, in bytes.
  uint64_t physmem;

  // Actual length of .version_string, not including the NUL terminator.
  uint64_t version_string_len;

  // A NUL-terminated UTF-8 string returned by zx_system_get_version_string.
  char version_string[];
};

// This always leaves space for the NUL terminator.
constexpr size_t kMaxVersionString = VDSO_CONSTANTS_SIZE - sizeof(vdso_constants) - 1;

static_assert(VDSO_CONSTANTS_SIZE > sizeof(vdso_constants), "Need to adjust VDSO_CONSTANTS_SIZE");
static_assert(VDSO_CONSTANTS_ALIGN >= alignof(vdso_constants),
              "Need to adjust VDSO_CONSTANTS_ALIGN");

#endif  // __ASSEMBLER__

#endif  // ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_VDSO_CONSTANTS_H_
