// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/x86/mp.h>

#ifndef ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_CURRENT_THREAD_H_
#define ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_CURRENT_THREAD_H_

// Routines to directly access the current thread pointer out of the current
// cpu structure pointed to by gs.

static inline Thread* arch_get_current_thread() {
  // Read directly from gs, rather than via x86_get_percpu()->current_thread,
  // so that this is atomic.  Otherwise, we could context switch between the
  // read of percpu from gs and the read of the current_thread pointer, and
  // discover the current thread on a different CPU
  return READ_PERCPU_FIELD(current_thread);
}

static inline void arch_set_current_thread(Thread* t) {
  // See above for why this is a direct gs write
  WRITE_PERCPU_FIELD(current_thread, t);
}

#endif  // ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_CURRENT_THREAD_H_
