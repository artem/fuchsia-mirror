// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/ops.h>
#include <arch/spinlock.h>

// We need to disable thread safety analysis in this file, since we're
// implementing the locks themselves.  Without this, the header-level
// annotations cause Clang to detect violations.

void arch_spin_lock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  unsigned long val = arch_curr_cpu_num() + 1;
  uint64_t temp;

  __asm__ volatile(
      "sevl;"
      "1: wfe;"
      "2: ldaxr   %[temp], [%[lock]];"
      "cbnz    %[temp], 1b;"
      "stxr    %w[temp], %[val], [%[lock]];"
      "cbnz    %w[temp], 2b;"
      : [temp] "=&r"(temp)
      : [lock] "r"(&lock->value), [val] "r"(val)
      : "cc", "memory");
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
}

bool arch_spin_trylock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  unsigned long val = arch_curr_cpu_num() + 1;
  uint64_t out;
  __asm__ volatile(
      "1:"
      "ldaxr   %[out], [%[lock]];"
      "cbnz    %[out], 2f;"
      "stxr    %w[out], %[val], [%[lock]];"
      // Even though this is a try lock, if the store on acquire fails we try
      // again. This is to prevent spurious failures from misleading the caller
      // into thinking the lock is held by another thread. See
      // http://fxbug.dev/83983 for details.
      "cbnz    %w[out], 1b;"
      "2:"
      "clrex;"
      : [out] "=&r"(out)
      : [lock] "r"(&lock->value), [val] "r"(val)
      : "cc", "memory");

  if (out == 0) {
    WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) + 1);
  }
  return out;
}

void arch_spin_unlock(arch_spin_lock_t* lock) TA_NO_THREAD_SAFETY_ANALYSIS {
  WRITE_PERCPU_FIELD(num_spinlocks, READ_PERCPU_FIELD(num_spinlocks) - 1);
  __atomic_store_n(&lock->value, 0UL, __ATOMIC_RELEASE);
}
