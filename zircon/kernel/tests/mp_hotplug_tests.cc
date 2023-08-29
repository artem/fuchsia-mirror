// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/unittest/unittest.h>
#include <platform.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <dev/power.h>
#include <kernel/cpu.h>
#include <kernel/mp.h>
#include <kernel/thread.h>
#include <ktl/iterator.h>

#include "tests.h"

#include <ktl/enforce.h>

static int resume_cpu_test_thread(void* arg) {
  *reinterpret_cast<cpu_num_t*>(arg) = arch_curr_cpu_num();
  return 0;
}

// "Unplug" online secondary (non-BOOT) cores
static zx_status_t unplug_all_cores(Thread** leaked_threads) {
  cpu_mask_t cpumask = mp_get_online_mask() & ~cpu_num_to_mask(BOOT_CPU_ID);
  return mp_unplug_cpu_mask(cpumask, ZX_TIME_INFINITE, leaked_threads);
}

static zx_status_t hotplug_core(cpu_num_t i) {
  cpu_mask_t cpumask = cpu_num_to_mask(i);
  return mp_hotplug_cpu_mask(cpumask);
}

static unsigned get_num_cpus_online() {
  unsigned count = 0;
  cpu_mask_t online = mp_get_online_mask();
  while (online) {
    online >>= 1;
    ++count;
  }
  return count;
}

static zx_status_t wait_for_cpu_offline(cpu_num_t i, Deadline deadline) {
  while (current_time() < deadline.when()) {
    zx::result<power_cpu_state> res = platform_get_cpu_state(i);
    if (res.is_error()) {
      if (res.error_value() == ZX_ERR_NOT_SUPPORTED) {
        // x86 does not implement platform_get_cpu_state, so return OK if the call returns
        // ZX_ERR_NOT_SUPPORTED.
        return ZX_OK;
      }
      return res.error_value();
    } else if (res.value() == power_cpu_state::OFF || res.value() == power_cpu_state::STOPPED) {
      return ZX_OK;
    }
    Thread::Current::SleepRelative(ZX_MSEC(10));
  }
  printf("timed out waiting for cpu-%u to go offline\n", i);
  return ZX_ERR_TIMED_OUT;
}

static zx_status_t wait_for_cpu_active(cpu_num_t i, Deadline deadline) {
  while (current_time() < deadline.when()) {
    if (mp_is_cpu_active(i)) {
      return ZX_OK;
    }
    Thread::Current::SleepRelative(ZX_MSEC(10));
  }
  printf("timed out waiting for cpu-%u to become active\n", i);
  return ZX_ERR_TIMED_OUT;
}

// Unplug all cores (except for Boot core), then hotplug
// the cores one by one and make sure that we can schedule
// tasks on that core.
[[maybe_unused]] static bool mp_hotplug_test() {
  BEGIN_TEST;

// Hotplug is only implemented on x86_64 and arm64.
#if !defined(__x86_64__) && !defined(__aarch64__)
  printf("skipping test mp_hotplug, hotplug only suported on x64 and arm64\n");
  END_TEST;
#endif
  uint num_cores = get_num_cpus_online();
  if (num_cores < 2) {
    printf("skipping test mp_hotplug, not enough online cpus\n");
    END_TEST;
  }
  Thread::Current::MigrateToCpu(BOOT_CPU_ID);
  // "Unplug" online secondary (non-BOOT) cores
  Thread* leaked_threads[SMP_MAX_CPUS] = {};
  ASSERT_OK(unplug_all_cores(leaked_threads), "unplugging all cores failed");
  for (cpu_num_t i = 0; i < num_cores; i++) {
    if (i == BOOT_CPU_ID) {
      continue;
    }
    // Wait until this core is fully offline.
    ASSERT_OK(wait_for_cpu_offline(i, Deadline::after(ZX_SEC(5))),
              "waiting for core to go offline failed");
    // Hotplug this core.
    ASSERT_OK(hotplug_core(i), "hotplugging core failed");
    // Wait until the core is active.
    ASSERT_OK(wait_for_cpu_active(i, Deadline::after(ZX_SEC(5))),
              "waiting for core to come online failed");
    // Create a thread, affine it to the core just hotplugged
    // and make sure the thread does get scheduled there.
    cpu_num_t running_core;
    Thread* nt = Thread::Create("resume-test-thread", resume_cpu_test_thread, &running_core,
                                DEFAULT_PRIORITY);
    ASSERT_NE(nullptr, nt, "Thread create failed");
    nt->SetCpuAffinity(cpu_num_to_mask(i));
    nt->SetMigrateFn([](auto...) {});
    nt->Resume();
    ASSERT_OK(nt->Join(nullptr, ZX_TIME_INFINITE), "thread join failed");
    ASSERT_EQ(i, running_core, "Thread not running on hotplugged core");
  }

  for (Thread* leaked_thread : leaked_threads) {
    if (leaked_thread) {
      leaked_thread->Forget();
    }
  }

  END_TEST;
}

UNITTEST_START_TESTCASE(mp_hotplug_tests)
UNITTEST("test unplug and hotplug cores one by one", mp_hotplug_test)
UNITTEST_END_TESTCASE(mp_hotplug_tests, "hotplug", "Tests for unplugging and hotplugging cores")
