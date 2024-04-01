// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_INTERRUPT_EVENT_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_INTERRUPT_EVENT_DISPATCHER_H_

#include <sys/types.h>
#include <zircon/types.h>

#include <kernel/mp.h>
#include <object/interrupt_dispatcher.h>

class InterruptEventDispatcher final : public InterruptDispatcher {
 public:
  static zx_status_t Create(KernelHandle<InterruptDispatcher>* handle, zx_rights_t* rights,
                            uint32_t vector, uint32_t options,
                            bool allow_ack_without_port_for_test = false);

  ~InterruptEventDispatcher() final;

  InterruptEventDispatcher(const InterruptDispatcher&) = delete;
  InterruptEventDispatcher& operator=(const InterruptDispatcher&) = delete;

 private:
  explicit InterruptEventDispatcher(uint32_t vector);

  void MaskInterrupt() final;
  void UnmaskInterrupt() final;
  void DeactivateInterrupt() final;
  void UnregisterInterruptHandler() final;

  zx_status_t RegisterInterruptHandler();
  static void IrqHandler(void* ctx);

  const uint32_t vector_;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_INTERRUPT_EVENT_DISPATCHER_H_
