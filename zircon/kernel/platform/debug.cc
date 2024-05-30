// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/cbuf.h>
#include <lib/debuglog.h>
#include <lib/uart/all.h>
#include <lib/uart/null.h>
#include <lib/uart/qemu.h>
#include <lib/uart/uart.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zircon-internal/macros.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cassert>
#include <type_traits>

#include <arch/arch_interrupt.h>
#include <dev/init.h>
#include <dev/interrupt.h>
#include <kernel/deadline.h>
#include <kernel/spinlock.h>
#include <kernel/timer.h>
#include <ktl/optional.h>
#include <ktl/type_traits.h>
#include <ktl/variant.h>
#include <lockdep/guard.h>
#include <platform/debug.h>
#include <platform/legacy_debug.h>
#include <platform/uart.h>

#include "platform.h"

namespace {

// No locks will be acquired.
struct NullLockPolicy {};

// Temporary flag for conditionally delegating to legacy_platform_* variants.
bool use_new_serial = false;
bool is_tx_irq_enabled = false;
bool is_serial_enabled = false;

// Forward declare class with capability annotation to avoid formatter doing
// the wrong thing.
class TA_SCOPED_CAP NullGuard;

// TODO(https://fxbug.dev/42073424): Switch to lockdep::NullGuard once fbl::NullLock
// and NullGuard are both annotated, and the existing use cases are
// TA compliant.
class NullGuard {
 public:
  template <typename Lockable>
  NullGuard(Lockable* lock, const char*) TA_ACQ(lock) {}
  ~NullGuard() TA_REL() {}

  template <typename T>
  void CallUnlocked(T&& op) {
    op();
  }
};

template <typename LockPolicy, typename Guard>
using GuardSelector =
    std::conditional_t<std::is_same_v<LockPolicy, NullLockPolicy>, NullGuard, Guard>;

// Implements SyncPolicy as defined in <lib/uart/sync.h>
struct UartSyncPolicy {
  template <typename MemberOf>
  using Lock = DECLARE_SPINLOCK_WITH_TYPE(MemberOf, MonitoredSpinLock);

  template <typename LockPolicy>
  using Guard = GuardSelector<LockPolicy, Guard<MonitoredSpinLock, LockPolicy>>;

  class Waiter {
   public:
    enum class Blocking {
      // `Wait` is allowed to block callers, e.g. wait on an event.
      kYes,
      // `Wait` is not allowed to block callers, should spin instead.
      kNo,
    };

    template <typename Guard, typename T>
    void Wait(Guard& guard, T&& enable_tx_interrupt, Blocking blocking) TA_REQ(guard) {
      if (blocking == Blocking::kYes && is_tx_irq_enabled) {
        enable_tx_interrupt();
        guard.CallUnlocked([this]() { tx_fifo_not_full_.Wait(); });
      } else {
        // Drop the spinlock while spinning.
        guard.CallUnlocked([]() { arch::Yield(); });
      }
    }

    void Wake() { tx_fifo_not_full_.Signal(); }

   private:
    AutounsignalEvent tx_fifo_not_full_{true};
  };

  using DefaultLockPolicy = NullLockPolicy;

  template <typename LockType>
  static void AssertHeld(LockType& lock) TA_ASSERT(lock) TA_ASSERT(lock.lock()) {
    lock.lock().AssertHeld();
  }
};

uart::all::KernelDriver<PlatformUartIoProvider, UartSyncPolicy> gUart;

// Initialized by UartInitLate, provides buffered output of the uart,
// which helps readers catch up.
// Also provides synchronization mechanisms for character availability.
Cbuf rx_queue;

// Size of the rx queue. The bigger the buffer, the bigger the window for
// the reader to catch up. Useful when the incoming data is bursty.
constexpr size_t kRxQueueSize = 1024;

// When Polling is enabled, this will fire the polling callback for draining UART's RX Queue.
Timer gUartPollTimer;

constexpr zx_duration_t kPollingPeriod = ZX_MSEC(10);
constexpr TimerSlack kPollingSlack = {ZX_MSEC(10), TIMER_SLACK_CENTER};

// Callback used by |gUartPollTimer| when deadline is met. See |Timer| interface for more
// information.
template <bool DrainUart>
void UartPoll(Timer* uart_timer, zx_time_t now, void* arg) {
  uart_timer->Set(Deadline(zx_time_add_duration(now, kPollingPeriod), kPollingSlack),
                  &UartPoll<true>, nullptr);
  if constexpr (DrainUart) {
    gUart.Visit([&](auto& driver) {
      // Drain until there is nothing else in the RX Queue of the device.
      while (auto c = driver.Read()) {
        rx_queue.WriteChar(*c);
      }
    });
  }
}

struct IrqConfig {
  interrupt_trigger_mode trigger;
  interrupt_polarity polarity;
};

ktl::optional<IrqConfig> GetIrqConfigFromFlags(uint32_t uart_flags) {
  if (uart_flags == 0) {
    return ktl::nullopt;
  }

  if ((uart_flags & (ZBI_KERNEL_DRIVER_IRQ_FLAGS_LEVEL_TRIGGERED |
                     ZBI_KERNEL_DRIVER_IRQ_FLAGS_EDGE_TRIGGERED)) == 0) {
    return ktl::nullopt;
  }

  if ((uart_flags & (ZBI_KERNEL_DRIVER_IRQ_FLAGS_POLARITY_HIGH |
                     ZBI_KERNEL_DRIVER_IRQ_FLAGS_POLARITY_LOW)) == 0) {
    return ktl::nullopt;
  }

  // In order to configure the IRQ all information, trigger and polarity must be provided.
  // Otherwise the step must be omitted and let defaults take over.
  IrqConfig config = {};
  config.trigger = uart_flags & ZBI_KERNEL_DRIVER_IRQ_FLAGS_LEVEL_TRIGGERED ? IRQ_TRIGGER_MODE_LEVEL
                                                                            : IRQ_TRIGGER_MODE_EDGE;
  config.polarity = uart_flags & ZBI_KERNEL_DRIVER_IRQ_FLAGS_POLARITY_HIGH
                        ? IRQ_POLARITY_ACTIVE_HIGH
                        : IRQ_POLARITY_ACTIVE_LOW;
  return config;
}

}  // namespace

bool platform_serial_enabled(void) { return is_serial_enabled; }

void UartDriverHandoffEarly(const uart::all::Driver& serial) {
  ktl::visit(
      [&](auto& driver) {
        is_serial_enabled = !(ktl::is_same_v<ktl::decay_t<decltype(driver)>, uart::null::Driver>);
      },
      serial);

  use_new_serial = gBootOptions->experimental_serial_migration;
  if (use_new_serial) {
    // Serial driver has been initialized by physboot.
    gUart = serial;
  }

  PlatformUartDriverHandoffEarly(serial);
}

void UartDriverHandoffLate(const uart::all::Driver& serial) {
  if (use_new_serial) {
    // This buffer is needed even when serial is disabled, to prevent uninitialized
    // access to it.
    rx_queue.Initialize(kRxQueueSize, malloc(kRxQueueSize));

    if (!platform_serial_enabled()) {
      return;
    }

    // Check for interrupt support or explicitly polling uart.
    ktl::optional<uint32_t> uart_irq;
    bool polling_mode = false;
    gUart.Visit([&](auto& driver) {
      using cfg_type = ktl::decay_t<decltype(driver.uart().config())>;
      if constexpr (ktl::is_same_v<cfg_type, zbi_dcfg_simple_pio_t> ||
                    ktl::is_same_v<cfg_type, zbi_dcfg_simple_t>) {
        uart_irq = PlatformUartGetIrqNumber(driver.uart().config().irq);
      } else {  // Only |uart::null::Driver| is expected to have a different configuration type.
        using driver_type = ktl::decay_t<decltype(driver.uart())>;
        constexpr auto kIsNullDriver = ktl::is_same_v<driver_type, uart::null::Driver>;
        ZX_ASSERT_MSG(kIsNullDriver, "Unexpected UART Configuration.");
        // No IRQ Handler for null driver.
        return;
      }

      // Check for polling mode.
      if (!uart_irq || gBootOptions->debug_uart_poll) {
        // Start the polling without performing any drain.
        UartPoll</*DrainUart=*/false>(&gUartPollTimer, current_time(), nullptr);
        printf("UART: POLLING mode enabled.\n");
        polling_mode = true;
        return;
      }

      static constexpr auto rx_irq_handler = [](auto& spinlock, auto&& read_char,
                                                auto&& mask_rx_interrupt) {
        // This check needs to be performed under a lock, such that we prevent operation
        // interleaving that would leave us in a blocked state.
        //
        // E.g.
        // Assume a simple MT scenario with one reader R and one writer R:
        //
        // * W: Observes the buffer is full.
        // * R: Reads a character. The buffer is now empty.
        // * R: Unmasks RX.
        // * W: Masks RX.
        //
        //  At this point, we have an empty buffer and RX interrupts are masked -
        //  we're stuck! Thus, to avoid this, we acquire the spinlock before
        //  checking if the buffer is full, and release after (conditionally)
        //  masking RX interrupts. By pairing this with the acquisition of the
        //  same lock around unmasking RX interrupts, we prevent the writer above
        //  from being interrupted by a read-and-unmask.
        {
          Guard<MonitoredSpinLock, NoIrqSave> lock(&spinlock, SOURCE_TAG);
          if (rx_queue.Full()) {
            // disables RX interrupts.
            mask_rx_interrupt();
            return;
          }
        }
        rx_queue.WriteChar(static_cast<char>(read_char()));
      };

      static constexpr auto tx_irq_handler = [](auto& spinlock, auto& waiter,
                                                auto&& mask_tx_interrupts) {
        // Mask the TX interrupt before signalling any blocked thread as there may
        // be a race between masking TX here below and unmasking by the blocked
        // thread.
        {
          Guard<MonitoredSpinLock, NoIrqSave> lock(&spinlock, SOURCE_TAG);
          mask_tx_interrupts();
        }

        // Do not signal the event while holding the sync capability, this could lead
        // to invalid lock dependencies.
        waiter.Wake();
      };

      constexpr auto irq_handler = [](void* driver_ptr) {
        auto* typed_driver = static_cast<ktl::decay_t<decltype(driver)>*>(driver_ptr);
        typed_driver->Interrupt(tx_irq_handler, rx_irq_handler);
      };

      if constexpr (ktl::is_same_v<cfg_type, zbi_dcfg_simple_t>) {
        // Configure the interrupt if available.
        auto irq_config = GetIrqConfigFromFlags(driver.uart().config().flags);
        if (irq_config) {
          zx_status_t irq_config_result =
              configure_interrupt(*uart_irq, irq_config->trigger, irq_config->polarity);
          DEBUG_ASSERT(irq_config_result == ZX_OK);
        }
      }

      // Register IRQ Handler.
      zx_status_t irq_register_result =
          register_permanent_int_handler(*uart_irq, irq_handler, &driver);
      DEBUG_ASSERT(irq_register_result == ZX_OK);
      // Init Rx Interrupt.
      driver.InitInterrupt([uart_irq]() { unmask_interrupt(*uart_irq); });
    });

    if (!polling_mode) {
      printf("UART: IRQ driven RX: enabled\n");

      is_tx_irq_enabled = !dlog_bypass();
      printf("UART: IRQ driven TX: %s\n", is_tx_irq_enabled ? "enabled" : "disabled");
    }
  }
  PlatformUartDriverHandoffLate(serial);
}

void platform_dputs_thread(const char* str, size_t len) {
  if (!platform_serial_enabled()) {
    return;
  }

  if (!use_new_serial) {
    legacy_platform_dputs_thread(str, len);
    return;
  }

  gUart.Visit([str, len](auto& driver) {
    driver.template Write<IrqSave>({str, len}, UartSyncPolicy::Waiter::Blocking::kYes);
  });
}

void platform_dputs_irq(const char* str, size_t len) {
  if (!platform_serial_enabled()) {
    return;
  }

  if (!use_new_serial) {
    legacy_platform_dputs_irq(str, len);
    return;
  }

  gUart.Visit([str, len](auto& driver) {
    driver.template Write<IrqSave>({str, len}, UartSyncPolicy::Waiter::Blocking::kNo);
  });
}

int platform_dgetc(char* c, bool wait) {
  if (!platform_serial_enabled()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!use_new_serial) {
    return legacy_platform_dgetc(c, wait);
  }

  auto read = rx_queue.ReadCharWithContext(wait);

  // 1 => Character read.
  if (read.is_ok()) {
    // This is safe because:
    //   * The RX IRQ handler is holding the UART lock while the queue is being inspected (Full) and
    //     the RX IRQ is being disabled.
    //   * The Read path, which is the only path which can transition the queue from full to not
    //     full, is not holding the the UART lock while inspecting, but the operations is deferred
    //     and acquires the lock before enabling interrupts.
    //
    // As a consequence, the IRQ RX Interrupt cannot be enabled by this path, until the RX IRQ
    // Handler, has disabled it and released the lock. This means there is no possible interleaving,
    // where both paths observe full queue, and we enable the RX IRQ followed by the IRQ RX Handler
    // disabling them.
    if (read->transitioned_from_full) {
      gUart.Visit([](auto& driver) { driver.template EnableRxInterrupt<IrqSave>(); });
    }
    *c = read->c;
    return 1;
  }

  // 0 => No character yet.
  if (read.status_value() == ZX_ERR_SHOULD_WAIT) {
    return 0;
  }

  // < 0 => Error.
  return read.status_value();
}

int platform_pgetc(char* c) {
  if (!platform_serial_enabled()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!use_new_serial) {
    return legacy_platform_pgetc(c);
  }
  ktl::optional<char> read;
  gUart.Visit([&](auto& driver) { read = driver.Read(); });

  if (read) {
    *c = *read;
    return 0;
  }

  return -1;
}

void platform_pputc(char c) {
  if (!platform_serial_enabled()) {
    return;
  }

  if (!use_new_serial) {
    legacy_platform_pputc(c);
    return;
  }

  gUart.Visit([c](auto& driver) { driver.Write({&c, 1}, UartSyncPolicy::Waiter::Blocking::kNo); });
}
