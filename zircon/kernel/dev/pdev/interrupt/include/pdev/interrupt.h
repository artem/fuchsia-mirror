// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_PDEV_INTERRUPT_INCLUDE_PDEV_INTERRUPT_H_
#define ZIRCON_KERNEL_DEV_PDEV_INTERRUPT_INCLUDE_PDEV_INTERRUPT_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

#include <dev/interrupt.h>
#include <kernel/cpu.h>

// Invokes the handler for the given vector if one is registered. If true is
// returned a handler was present, otherwise false is returned.
bool pdev_invoke_int_if_present(unsigned int vector);

// Interrupt Controller interface
struct pdev_interrupt_ops {
  zx_status_t (*mask)(unsigned int vector);
  zx_status_t (*unmask)(unsigned int vector);
  zx_status_t (*deactivate)(unsigned int vector);
  zx_status_t (*configure)(unsigned int vector, enum interrupt_trigger_mode tm,
                           enum interrupt_polarity pol);
  zx_status_t (*get_config)(unsigned int vector, enum interrupt_trigger_mode* tm,
                            enum interrupt_polarity* pol);
  zx_status_t (*set_affinity)(unsigned int vector, cpu_mask_t mask);
  bool (*is_valid)(unsigned int vector, uint32_t flags);
  uint32_t (*get_base_vector)();
  uint32_t (*get_max_vector)();
  unsigned int (*remap)(unsigned int vector);
  zx_status_t (*send_ipi)(cpu_mask_t target, mp_ipi_t ipi);
  void (*init_percpu_early)();
  void (*init_percpu)();
  void (*handle_irq)(iframe_t* frame);
  void (*shutdown)();
  void (*shutdown_cpu)();

  bool (*msi_is_supported)();
  bool (*msi_supports_masking)();
  void (*msi_mask_unmask)(const msi_block_t* block, uint msi_id, bool mask);
  zx_status_t (*msi_alloc_block)(uint requested_irqs, bool can_target_64bit, bool is_msix,
                                 msi_block_t* out_block);
  void (*msi_free_block)(msi_block_t* block);
  void (*msi_register_handler)(const msi_block_t* block, uint msi_id, int_handler handler,
                               void* ctx);
};

void pdev_register_interrupts(const struct pdev_interrupt_ops* ops);

#endif  // ZIRCON_KERNEL_DEV_PDEV_INTERRUPT_INCLUDE_PDEV_INTERRUPT_H_
