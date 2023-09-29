// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <platform.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/mp.h>
#include <arch/mp_unplug_event.h>
#include <arch/ops.h>
#include <arch/quirks.h>
#include <dev/interrupt.h>
#include <kernel/cpu.h>
#include <ktl/iterator.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {
// Mask the MPIDR register to only leave the AFFx ids.
constexpr uint64_t kMpidAffMask = 0xFF00FFFFFF;

// A list of mpids, indexed by cpu_id.
// We can leave this zero-initialized as MPID 0 is only valid on CPU 0.
uint64_t arm64_cpu_list[SMP_MAX_CPUS];

}  // namespace

// cpu id to cluster and id within cluster map
uint arm64_cpu_cluster_ids[SMP_MAX_CPUS] = {0};
uint arm64_cpu_cpu_ids[SMP_MAX_CPUS] = {0};

// total number of detected cpus
uint arm_num_cpus = 1;

// per cpu structures, each cpu will point to theirs using the fixed register
arm64_percpu arm64_percpu_array[SMP_MAX_CPUS];

void arch_register_mpid(uint cpu_id, uint64_t mpid) {
  // TODO(fxbug.dev/32903) transition off of these maps to the topology.
  arm64_cpu_cluster_ids[cpu_id] = (mpid & 0xFF00) >> MPIDR_AFF1_SHIFT;  // "cluster" here is AFF1.
  arm64_cpu_cpu_ids[cpu_id] = mpid & 0xFF;                              // "cpu" here is AFF0.

  arm64_percpu_array[cpu_id].cpu_num = cpu_id;

  arm64_cpu_list[cpu_id] = mpid;
}

cpu_num_t arm64_mpidr_to_cpu_num(uint64_t mpidr) {
  mpidr &= kMpidAffMask;
  for (cpu_num_t i = 0; i < arm_num_cpus; ++i) {
    if (arm64_cpu_list[i] == mpidr) {
      return i;
    }
  }

  if (arm_num_cpus == 0) {
    // The only time we shouldn't find a cpu is when the list isn't
    // defined yet during early boot, in this case the only processor up is 0
    // so returning 0 is correct.
    return 0;
  }
  return INVALID_CPU;
}

uint64_t arch_cpu_num_to_mpidr(cpu_num_t cpu_num) {
  DEBUG_ASSERT(cpu_num < ktl::size(arm64_cpu_list));
  return arm64_cpu_list[cpu_num];
}

// do the 'slow' lookup by mpidr to cpu number
static cpu_num_t arch_curr_cpu_num_slow() {
  uint64_t mpidr = __arm_rsr64("mpidr_el1");
  return arm64_mpidr_to_cpu_num(mpidr);
}

void arch_mp_reschedule(cpu_mask_t mask) {
  arch_mp_send_ipi(MP_IPI_TARGET_MASK, mask, MP_IPI_RESCHEDULE);
}

void arch_mp_send_ipi(mp_ipi_target_t target, cpu_mask_t mask, mp_ipi_t ipi) {
  LTRACEF("target %d mask %#x, ipi %d\n", target, mask, ipi);

  // translate the high level target + mask mechanism into just a mask
  switch (target) {
    case MP_IPI_TARGET_ALL:
      mask = (1ul << SMP_MAX_CPUS) - 1;
      break;
    case MP_IPI_TARGET_ALL_BUT_LOCAL:
      mask = mask_all_but_one(arch_curr_cpu_num());
      break;
    case MP_IPI_TARGET_MASK:;
  }

  interrupt_send_ipi(mask, ipi);
}

void arm64_init_percpu_early(void) {
  // slow lookup the current cpu id and setup the percpu structure
  cpu_num_t cpu = arch_curr_cpu_num_slow();
  arm64_write_percpu_ptr(&arm64_percpu_array[cpu]);

  // read the midr and set the microarch of this cpu in the percpu array
  uint32_t midr = __arm_rsr64("midr_el1") & 0xFFFFFFFF;
  arm64_percpu_array[cpu].microarch = midr_to_microarch(midr);
}

void arch_mp_init_percpu(void) { interrupt_init_percpu(); }

void arch_flush_state_and_halt(MpUnplugEvent* flush_done) {
  DEBUG_ASSERT(arch_ints_disabled());
  Thread::Current::Get()->preemption_state().PreemptDisable();
  flush_done->Signal();
  platform_halt_cpu();
  panic("control should never reach here\n");
}

zx_status_t arch_mp_prep_cpu_unplug(cpu_num_t cpu_id) {
  if (cpu_id == 0 || cpu_id >= arm_num_cpus) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t arch_mp_cpu_unplug(cpu_num_t cpu_id) {
  // we do not allow unplugging the bootstrap processor
  if (cpu_id == 0 || cpu_id >= arm_num_cpus) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t arch_mp_cpu_hotplug(cpu_num_t cpu_id) {
  DEBUG_ASSERT(cpu_id != 0);

  if (cpu_id >= arm_num_cpus) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (mp_is_cpu_online(cpu_id)) {
    return ZX_ERR_BAD_STATE;
  }

  uint64_t mpid = arm64_cpu_list[cpu_id];
  // Create a stack for the thread running on the CPU.
  zx_status_t status = arm64_create_secondary_stack(cpu_id, mpid);
  if (status != ZX_OK) {
    return status;
  }

  // Start the CPU.
  status = platform_start_cpu(cpu_id, mpid);
  if (status != ZX_OK) {
    // Start failed, so free the stack.
    [[maybe_unused]] zx_status_t free_stack_status = arm64_free_secondary_stack(cpu_id);
    DEBUG_ASSERT(free_stack_status == ZX_OK);
  }
  return status;
}

// If there are any A73 cores in this system, then we need the clock read
// mitigation.
bool arch_quirks_needs_arm_erratum_858921_mitigation() {
  DEBUG_ASSERT(ktl::size(arm64_percpu_array) >= arch_max_num_cpus());
  for (uint i = 0; i < arch_max_num_cpus(); ++i) {
    if (arm64_percpu_array[i].microarch == ARM_CORTEX_A73) {
      return true;
    }
  }
  return false;
}

void arch_setup_percpu(cpu_num_t cpu_num, struct percpu* percpu) {
  arm64_percpu* arch_percpu = &arm64_percpu_array[cpu_num];
  DEBUG_ASSERT(arch_percpu->high_level_percpu == nullptr ||
               arch_percpu->high_level_percpu == percpu);
  arch_percpu->high_level_percpu = percpu;
}
