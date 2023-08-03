// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_ARCH_MP_H_
#define ZIRCON_KERNEL_INCLUDE_ARCH_MP_H_

#include <zircon/types.h>

#include <kernel/cpu.h>
#include <kernel/mp.h>

/* send inter processor interrupt, if supported */
void arch_mp_send_ipi(mp_ipi_target_t target, cpu_mask_t mask, mp_ipi_t ipi);

/* Reschedules tasks on the cpus specified by mask. Mask will not
 * contain the local cpu_id.
 */
void arch_mp_reschedule(cpu_mask_t mask);

/* Bring a CPU up and enter it into the scheduler */
zx_status_t platform_mp_cpu_hotplug(cpu_num_t cpu_id);

/* Prepare for CPU unplug.  The platform may want to shift
 * around external interrupts at this time. */
zx_status_t platform_mp_prep_cpu_unplug(cpu_num_t cpu_id);

/* shutdown the specified CPU.  called after it is no longer
 * being scheduled on.  */
zx_status_t platform_mp_cpu_unplug(cpu_num_t cpu_id);

/* Should be invoked by platform_mp_cpu_hotplug to ask the arch
 * to bring a CPU up and enter it into the scheduler */
zx_status_t arch_mp_cpu_hotplug(cpu_num_t cpu_id);

/* Should be invoked by platform_mp_prep_cpu_unplug to ask the
 * arch to do whatever it needs to do to stop the CPU */
zx_status_t arch_mp_prep_cpu_unplug(cpu_num_t cpu_id);

/* Should be invoked by platform_mp_cpu_unplug to ask the
 * arch to do whatever it needs to do to stop the CPU */
zx_status_t arch_mp_cpu_unplug(cpu_num_t cpu_id);

void arch_mp_init_percpu();

#endif  // ZIRCON_KERNEL_INCLUDE_ARCH_MP_H_
