// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_UART_NS8250_H_
#define LIB_UART_NS8250_H_

#include <lib/acpi_lite/debug_port.h>
#include <lib/stdcompat/array.h>
#include <lib/zbi-format/driver-config.h>
#include <zircon/limits.h>

#include <array>
#include <optional>
#include <string_view>

#include <hwreg/bitfields.h>

#include "uart.h"

// 8250 and derivatives, including 16550.

namespace uart {
namespace ns8250 {

constexpr uint32_t kDefaultBaudRate = 115200;
constexpr uint32_t kMaxBaudRate = 115200;

constexpr uint8_t kFifoDepth16750 = 64;
constexpr uint8_t kFifoDepth16550A = 16;
constexpr uint8_t kFifoDepthDw8250Minimum = 16;
constexpr uint8_t kFifoDepthGeneric = 1;

// Traditional COM1 configuration.
constexpr zbi_dcfg_simple_pio_t kLegacyConfig{.base = 0x3f8, .irq = 4};

enum class InterruptType : uint8_t {
  kModemStatus = 0b0000,
  kNone = 0b0001,
  kTxEmpty = 0b0010,
  kRxDataAvailable = 0b0100,
  kRxLineStatus = 0b0110,
  kDw8250BusyDetect = 0b0111,  // dw8250 only
  kCharTimeout = 0b1100,
};

class RxBufferRegister : public hwreg::RegisterBase<RxBufferRegister, uint8_t> {
 public:
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<RxBufferRegister>(0); }
};

class TxBufferRegister : public hwreg::RegisterBase<TxBufferRegister, uint8_t> {
 public:
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<TxBufferRegister>(0); }
};

class InterruptEnableRegister : public hwreg::RegisterBase<InterruptEnableRegister, uint8_t> {
 public:
  DEF_RSVDZ_FIELD(7, 4);
  DEF_BIT(3, modem_status);
  DEF_BIT(2, line_status);
  DEF_BIT(1, tx_empty);
  DEF_BIT(0, rx_available);
  static auto Get() { return hwreg::RegisterAddr<InterruptEnableRegister>(1); }
};

class InterruptIdentRegister : public hwreg::RegisterBase<InterruptIdentRegister, uint8_t> {
 public:
  DEF_FIELD(7, 6, fifos_enabled);
  DEF_BIT(5, extended_fifo_enabled);
  DEF_RSVDZ_BIT(4);
  DEF_ENUM_FIELD(InterruptType, 3, 0, interrupt_id);
  static auto Get() { return hwreg::RegisterAddr<InterruptIdentRegister>(2); }
};

class FifoControlRegister : public hwreg::RegisterBase<FifoControlRegister, uint8_t> {
 public:
  DEF_FIELD(7, 6, receiver_trigger);
  // Note: dw8250 has a TX IRQ trigger field in 5...4.
  DEF_BIT(5, extended_fifo_enable);
  DEF_RSVDZ_BIT(4);
  DEF_BIT(3, dma_mode);
  DEF_BIT(2, tx_fifo_reset);
  DEF_BIT(1, rx_fifo_reset);
  DEF_BIT(0, fifo_enable);

  static constexpr uint8_t kMaxTriggerLevel = 0b11;

  static auto Get() { return hwreg::RegisterAddr<FifoControlRegister>(2); }
};

class LineControlRegister : public hwreg::RegisterBase<LineControlRegister, uint8_t> {
 public:
  DEF_BIT(7, divisor_latch_access);
  DEF_BIT(6, break_control);
  DEF_BIT(5, stick_parity);
  DEF_BIT(4, even_parity);
  DEF_BIT(3, parity_enable);
  DEF_BIT(2, stop_bits);
  DEF_FIELD(1, 0, word_length);

  static constexpr uint8_t kWordLength5 = 0b00;
  static constexpr uint8_t kWordLength6 = 0b01;
  static constexpr uint8_t kWordLength7 = 0b10;
  static constexpr uint8_t kWordLength8 = 0b11;

  static constexpr uint8_t kStopBits1 = 0b0;
  static constexpr uint8_t kStopBits2 = 0b1;

  static auto Get() { return hwreg::RegisterAddr<LineControlRegister>(3); }
};

class ModemControlRegister : public hwreg::RegisterBase<ModemControlRegister, uint8_t> {
 public:
  DEF_RSVDZ_FIELD(7, 6);
  DEF_BIT(5, automatic_flow_control_enable);
  DEF_BIT(4, loop);
  DEF_BIT(3, auxiliary_out_2);
  DEF_BIT(2, auxiliary_out_1);
  DEF_BIT(1, request_to_send);
  DEF_BIT(0, data_terminal_ready);
  static auto Get() { return hwreg::RegisterAddr<ModemControlRegister>(4); }
};

class LineStatusRegister : public hwreg::RegisterBase<LineStatusRegister, uint8_t> {
 public:
  DEF_BIT(7, error_in_rx_fifo);
  DEF_BIT(6, tx_empty);
  DEF_BIT(5, tx_register_empty);
  DEF_BIT(4, break_interrupt);
  DEF_BIT(3, framing_error);
  DEF_BIT(2, parity_error);
  DEF_BIT(1, overrun_error);
  DEF_BIT(0, data_ready);
  static auto Get() { return hwreg::RegisterAddr<LineStatusRegister>(5); }
};

class ModemStatusRegister : public hwreg::RegisterBase<ModemStatusRegister, uint8_t> {
 public:
  DEF_BIT(7, data_carrier_detect);
  DEF_BIT(6, ring_indicator);
  DEF_BIT(5, data_set_ready);
  DEF_BIT(4, clear_to_send);
  DEF_BIT(3, delta_data_carrier_detect);
  DEF_BIT(2, trailing_edge_ring_indicator);
  DEF_BIT(1, delta_data_set_ready);
  DEF_BIT(0, delta_clear_to_send);
  static auto Get() { return hwreg::RegisterAddr<ModemStatusRegister>(6); }
};

class ScratchRegister : public hwreg::RegisterBase<ScratchRegister, uint8_t> {
 public:
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<ScratchRegister>(7); }
};

class DivisorLatchLowerRegister : public hwreg::RegisterBase<DivisorLatchLowerRegister, uint8_t> {
 public:
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<DivisorLatchLowerRegister>(0); }
};

class DivisorLatchUpperRegister : public hwreg::RegisterBase<DivisorLatchUpperRegister, uint8_t> {
 public:
  DEF_FIELD(7, 0, data);
  static auto Get() { return hwreg::RegisterAddr<DivisorLatchUpperRegister>(1); }
};

// dW8250 only
class UartStatusRegister : public hwreg::RegisterBase<UartStatusRegister, uint32_t> {
 public:
  DEF_RSVDZ_FIELD(31, 5);
  // Bits 4...1 are optionally configured in the dw8250 core.
  DEF_BIT(4, receive_fifo_full);
  DEF_BIT(3, receive_fifo_not_empty);
  DEF_BIT(2, transmit_fifo_empty);
  DEF_BIT(1, transmit_fifo_not_full);
  DEF_BIT(0, uart_busy);
  static auto Get() { return hwreg::RegisterAddr<UartStatusRegister>(0x7c / 4); }
};

// The scaled number of `IoSlots` used by this driver, for PIO corresponds to the number of
// IO Ports used by the driver.
template <uint32_t KdrvExtra>
inline constexpr uint32_t kIoSlots = 8;

// Accomodates for the registers specific to this model that are used in the implementation.
// Specifically `UartStatusRegister`. For Scaled MMIO, this corresponds to the number of
// unscaled registers that need to be accessed by the implementation. The MMIO region size
// can be obtained by scaling the register by their access width(`sizeof(uint32_t)`).
template <>
inline constexpr uint32_t kIoSlots<ZBI_KERNEL_DRIVER_DW8250_UART> = (0x7c + sizeof(uint32_t)) / 4;

// This provides the actual driver logic common to MMIO and PIO variants.
template <uint32_t KdrvExtra, typename KdrvConfig, IoRegisterType IoRegType,
          size_t IoSlots = kIoSlots<KdrvExtra>>
class DriverImpl : public DriverBase<DriverImpl<KdrvExtra, KdrvConfig, IoRegType, IoSlots>,
                                     KdrvExtra, KdrvConfig, IoRegType, IoSlots> {
 public:
  using Base = DriverBase<DriverImpl<KdrvExtra, KdrvConfig, IoRegType, IoSlots>, KdrvExtra,
                          KdrvConfig, IoRegType, IoSlots>;

  static constexpr auto kDevicetreeBindings = []() {
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO32_UART ||
                  KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO8_UART) {
      return cpp20::to_array<std::string_view>(
          {"ns8250", "ns16450", "ns16550a", "ns16550", "ns16750", "ns16850"});
    } else if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
      return cpp20::to_array<std::string_view>({"snps,dw-apb-uart"});
    } else {
      return std::array<std::string_view, 0>{};
    }
  }();

  static constexpr std::string_view config_name() {
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_PIO_UART) {
      return "ioport";
    }
#if defined(__i386__) || defined(__x86_64__)
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO32_UART) {
      return "mmio";
    }
#endif
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO8_UART) {
      return "ns8250-8bit";
    }
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
      return "dw8250";
    }
    return "ns8250";
  }

  template <typename... Args>
  explicit DriverImpl(Args&&... args) : Base(std::forward<Args>(args)...) {}

  using Base::MaybeCreate;

  static std::optional<DriverImpl> MaybeCreate(
      const acpi_lite::AcpiDebugPortDescriptor& debug_port) {
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO32_UART) {
      if (debug_port.type == acpi_lite::AcpiDebugPortDescriptor::Type::kMmio) {
        return DriverImpl(KdrvConfig{.mmio_phys = debug_port.address});
      }
    }

    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_PIO_UART) {
      if (debug_port.type == acpi_lite::AcpiDebugPortDescriptor::Type::kPio) {
        return DriverImpl(KdrvConfig{.base = static_cast<uint16_t>(debug_port.address)});
      }
    }
    return {};
  }

  static std::optional<DriverImpl> MaybeCreate(std::string_view string) {
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_PIO_UART) {
      if (string == "legacy") {
        return DriverImpl{kLegacyConfig};
      }
    }
    return Base::MaybeCreate(string);
  }

  static bool MatchDevicetree(const devicetree::PropertyDecoder& decoder) {
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_PIO_UART) {
      return false;
    } else {
      // Check that the compatible property contains a compatible devicetree binding.
      if (!Base::MatchDevicetree(decoder)) {
        return false;
      }

      auto [io_width_prop, reg_shift_prop] = decoder.FindProperties("reg-io-width", "reg-shift");

      std::optional<uint32_t> io_width = io_width_prop ? io_width_prop->AsUint32() : std::nullopt;
      std::optional<uint32_t> reg_shift =
          reg_shift_prop ? reg_shift_prop->AsUint32() : std::nullopt;

      // Must provide io-width and reg-shift of 32 bits.
      if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO32_UART ||
                    KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
        return io_width == 4 && reg_shift == 2;
      } else {
        return io_width.value_or(1) == 1 && reg_shift.value_or(0) == 0;
      }
    }
  }

  template <class IoProvider>
  void Init(IoProvider& io) {
    // Get basic config done so that tx functions.

    // Disable all interrupts.
    InterruptEnableRegister::Get().FromValue(0).WriteTo(io.io());

    // Extended FIFO mode must be enabled while the divisor latch is.
    // Be sure to preserve the line controls, modulo divisor latch access,
    // which should be disabled immediately after configuring the FIFO.
    auto lcr = LineControlRegister::Get().ReadFrom(io.io());
    lcr.set_divisor_latch_access(true).WriteTo(io.io());

    auto fcr = FifoControlRegister::Get().FromValue(0);
    fcr.set_fifo_enable(true).set_rx_fifo_reset(true).set_tx_fifo_reset(true).set_receiver_trigger(
        FifoControlRegister::kMaxTriggerLevel);

    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
      // dw8250 does not have an extended fifo enable bit in bit 5, but
      // instead has a TX fifo threshold field in bits 4-5. Leave these
      // as zero by not setting it here.
    } else {
      fcr.set_extended_fifo_enable(true);
    }
    fcr.WriteTo(io.io());

    // Commit divisor by clearing the latch.
    lcr.set_divisor_latch_access(false).WriteTo(io.io());

    // Drive flow control bits high since we don't actively manage them.
    auto mcr = ModemControlRegister::Get().FromValue(0);
    mcr.set_data_terminal_ready(true).set_request_to_send(true).WriteTo(io.io());

    // Figure out the FIFO depth.
    auto iir = InterruptIdentRegister::Get().ReadFrom(io.io());
    if (iir.fifos_enabled()) {
      if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
        // The fifo depth isn't easily known on the dw8250, but it
        // must be at least 16 bytes if the fifo is enabled.
        fifo_depth_ = kFifoDepthDw8250Minimum;
      } else {
        // This is a 16750 or a 16550A.
        fifo_depth_ = iir.extended_fifo_enabled() ? kFifoDepth16750 : kFifoDepth16550A;
      }
    } else {
      fifo_depth_ = kFifoDepthGeneric;
    }
  }

  template <class IoProvider>
  void SetLineControl(IoProvider& io, std::optional<DataBits> data_bits,
                      std::optional<Parity> parity, std::optional<StopBits> stop_bits) {
    constexpr uint32_t kDivisor = kMaxBaudRate / kDefaultBaudRate;

    LineControlRegister::Get().FromValue(0).set_divisor_latch_access(true).WriteTo(io.io());

    DivisorLatchLowerRegister::Get()
        .FromValue(0)
        .set_data(static_cast<uint8_t>(kDivisor))
        .WriteTo(io.io());
    DivisorLatchUpperRegister::Get()
        .FromValue(0)
        .set_data(static_cast<uint8_t>(kDivisor >> 8))
        .WriteTo(io.io());

    auto lcr = LineControlRegister::Get().FromValue(0).set_divisor_latch_access(false);

    if (data_bits) {
      const uint8_t word_length = [bits = *data_bits]() {
        switch (bits) {
          case DataBits::k5:
            return LineControlRegister::kWordLength5;
          case DataBits::k6:
            return LineControlRegister::kWordLength6;
          case DataBits::k7:
            return LineControlRegister::kWordLength7;
          case DataBits::k8:
            return LineControlRegister::kWordLength8;
        }
        ZX_PANIC("Unknown value for DataBits enum class (%u)", static_cast<unsigned int>(bits));
      }();
      lcr.set_word_length(word_length);
    }

    if (parity) {
      lcr.set_parity_enable(*parity != Parity::kNone).set_even_parity(*parity == Parity::kEven);
    }

    if (stop_bits) {
      const uint8_t num_stop_bits = [bits = *stop_bits]() {
        switch (bits) {
          case StopBits::k1:
            return LineControlRegister::kStopBits1;
          case StopBits::k2:
            return LineControlRegister::kStopBits2;
        }
        ZX_PANIC("Unknown value for StopBits enum class (%u)", static_cast<unsigned int>(bits));
      }();
      lcr.set_stop_bits(num_stop_bits);
    }

    lcr.WriteTo(io.io());
  }

  template <class IoProvider>
  bool TxReady(IoProvider& io) {
    return LineStatusRegister::Get().ReadFrom(io.io()).tx_register_empty();
  }

  template <class IoProvider, typename It1, typename It2>
  auto Write(IoProvider& io, bool, It1 it, const It2& end) {
    // The FIFO is empty now and we know the size, so fill it completely.
    auto tx = TxBufferRegister::Get().FromValue(0);
    auto space = fifo_depth_;
    do {
      tx.set_data(*it).WriteTo(io.io());
    } while (++it != end && --space > 0);
    return it;
  }

  template <class IoProvider>
  std::optional<uint8_t> Read(IoProvider& io) {
    if (LineStatusRegister::Get().ReadFrom(io.io()).data_ready()) {
      return RxBufferRegister::Get().ReadFrom(io.io()).data();
    }
    return {};
  }

  template <class IoProvider>
  void EnableTxInterrupt(IoProvider& io, bool enable = true) {
    auto ier = InterruptEnableRegister::Get().ReadFrom(io.io());
    ier.set_tx_empty(enable).WriteTo(io.io());
  }

  template <class IoProvider>
  void EnableRxInterrupt(IoProvider& io, bool enable = true) {
    auto ier = InterruptEnableRegister::Get().ReadFrom(io.io());
    ier.set_rx_available(enable).WriteTo(io.io());
  }

  template <class IoProvider, typename EnableInterruptCallback>
  void InitInterrupt(IoProvider& io, EnableInterruptCallback&& enable_interrupt_callback) {
    // In x86 drivers enabling the interrupt after setting up the hardware
    // may cause the Rx Interrupt never to fire.
    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_I8250_PIO_UART ||
                  KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO32_UART) {
      enable_interrupt_callback();
    }
    // Enable receive interrupts.
    EnableRxInterrupt(io);

    // Modem Control Register: Auxiliary Output 2 is another IRQ enable bit.
    auto mcr = ModemControlRegister::Get().ReadFrom(io.io());
    mcr.set_auxiliary_out_2(true).WriteTo(io.io());

    if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART ||
                  KdrvExtra == ZBI_KERNEL_DRIVER_I8250_MMIO8_UART) {
      enable_interrupt_callback();
    }
  }

  template <class IoProvider, typename Lock, typename Waiter, typename Tx, typename Rx>
  void Interrupt(IoProvider& io, Lock& lock, Waiter& waiter, Tx&& tx, Rx&& rx) {
    auto iir = InterruptIdentRegister::Get();
    InterruptType id;
    while ((id = iir.ReadFrom(io.io()).interrupt_id()) != InterruptType::kNone) {
      if constexpr (KdrvExtra == ZBI_KERNEL_DRIVER_DW8250_UART) {
        if (id == InterruptType::kDw8250BusyDetect) {
          // dw8250 only. From the manual:
          // "Master has tried to write to the Line Control Register while the DW_apb_uart is busy
          // (USR[0] is set to one)." Read the UART Status Register to clear it.
          UartStatusRegister::Get().ReadFrom(io.io());
        }
      }

      // Reading LSR will clear kRxLineStatus signal.
      auto lsr = LineStatusRegister::Get().ReadFrom(io.io());

      // Notify TX.
      if (lsr.tx_register_empty()) {
        tx(lock, waiter, [&]() { EnableTxInterrupt(io, false); });
      }

      // Drain RX while the line status bit is ready.
      bool should_drain_rx = true;
      for (; should_drain_rx && lsr.data_ready();
           lsr = LineStatusRegister::Get().ReadFrom(io.io())) {
        rx(
            lock,  //
            [&]() { return RxBufferRegister::Get().ReadFrom(io.io()).data(); },
            [&]() {
              // If the buffer is full, disable the receive interrupt instead and
              // exit the loop.
              EnableRxInterrupt(io, false);
              should_drain_rx = false;
            });
      }
    }
  }

 protected:
  uint8_t fifo_depth_ = kFifoDepthGeneric;
};

// uart::KernelDriver UartDriver API for PIO via MMIO where offsets expressed in bytes are scaled
// by 4. Additionally all read or write operations are performed in 4 byte regions.
using Mmio32Driver =
    DriverImpl<ZBI_KERNEL_DRIVER_I8250_MMIO32_UART, zbi_dcfg_simple_t, IoRegisterType::kMmio32>;

// uart::KernelDriver UartDriver API for PIO via MMIO where offsets are expressed in bytes.
using Mmio8Driver =
    DriverImpl<ZBI_KERNEL_DRIVER_I8250_MMIO8_UART, zbi_dcfg_simple_t, IoRegisterType::kMmio8>;

// uart::KernelDriver UartDriver API for direct PIO.
using PioDriver =
    DriverImpl<ZBI_KERNEL_DRIVER_I8250_PIO_UART, zbi_dcfg_simple_pio_t, IoRegisterType::kPio>;

// uart::KernelDriver UartDriver API for PIO via MMIO using legacy item type.
using Dw8250Driver =
    DriverImpl<ZBI_KERNEL_DRIVER_DW8250_UART, zbi_dcfg_simple_t, IoRegisterType::kMmio32>;

}  // namespace ns8250
}  // namespace uart

#endif  // LIB_UART_NS8250_H_
