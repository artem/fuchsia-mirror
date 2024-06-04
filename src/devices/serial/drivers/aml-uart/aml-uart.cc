// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"

#ifdef DFV1
#include <lib/ddk/debug.h>  // nogncheck
#else
#include <lib/driver/compat/cpp/logging.h>  // nogncheck
#endif

#include <threads.h>
#include <zircon/syscalls-next.h>
#include <zircon/threads.h>

#include <fbl/auto_lock.h>

#include "src/devices/serial/drivers/aml-uart/registers.h"

namespace fhsi = fuchsia_hardware_serialimpl;

namespace serial {
namespace internal {

fit::closure DriverTransportReadOperation::MakeCallback(zx_status_t status, void* buf, size_t len) {
  return fit::closure(
      [arena = std::move(arena_), completer = std::move(completer_), status, buf, len]() mutable {
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess(
              fidl::VectorView<uint8_t>::FromExternal(static_cast<uint8_t*>(buf), len));
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      });
}

fit::closure DriverTransportWriteOperation::MakeCallback(zx_status_t status) {
  return fit::closure(
      [arena = std::move(arena_), completer = std::move(completer_), status]() mutable {
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess();
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      });
}

}  // namespace internal

AmlUart::AmlUart(ddk::PDevFidl pdev,
                 const fuchsia_hardware_serial::wire::SerialPortInfo& serial_port_info,
                 fdf::MmioBuffer mmio, fdf::UnownedSynchronizedDispatcher irq_dispatcher,
                 std::optional<fidl::ClientEnd<fuchsia_power_broker::Lessor>> lessor_client_end)
    : pdev_(std::move(pdev)),
      serial_port_info_(serial_port_info),
      mmio_(std::move(mmio)),
      irq_dispatcher_(std::move(irq_dispatcher)) {
  if (lessor_client_end != std::nullopt) {
    lessor_client_.Bind(std::move(*lessor_client_end));
  } else {
    zxlogf(INFO, "No lessor client end gets passed into AmlUart during initialization.");
  }
}

constexpr auto kMinBaudRate = 2;

bool AmlUart::Readable() { return !Status::Get().ReadFrom(&mmio_).rx_empty(); }
bool AmlUart::Writable() { return !Status::Get().ReadFrom(&mmio_).tx_full(); }

void AmlUart::GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(serial_port_info_);
}

zx_status_t AmlUart::Config(uint32_t baud_rate, uint32_t flags) {
  // Control register is determined completely by this logic, so start with a clean slate.
  if (baud_rate < kMinBaudRate) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto ctrl = Control::Get().FromValue(0);

  if ((flags & fhsi::kSerialSetBaudRateOnly) == 0) {
    switch (flags & fhsi::kSerialDataBitsMask) {
      case fhsi::kSerialDataBits5:
        ctrl.set_xmit_len(Control::kXmitLength5);
        break;
      case fhsi::kSerialDataBits6:
        ctrl.set_xmit_len(Control::kXmitLength6);
        break;
      case fhsi::kSerialDataBits7:
        ctrl.set_xmit_len(Control::kXmitLength7);
        break;
      case fhsi::kSerialDataBits8:
        ctrl.set_xmit_len(Control::kXmitLength8);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialStopBitsMask) {
      case fhsi::kSerialStopBits1:
        ctrl.set_stop_len(Control::kStopLen1);
        break;
      case fhsi::kSerialStopBits2:
        ctrl.set_stop_len(Control::kStopLen2);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialParityMask) {
      case fhsi::kSerialParityNone:
        ctrl.set_parity(Control::kParityNone);
        break;
      case fhsi::kSerialParityEven:
        ctrl.set_parity(Control::kParityEven);
        break;
      case fhsi::kSerialParityOdd:
        ctrl.set_parity(Control::kParityOdd);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialFlowCtrlMask) {
      case fhsi::kSerialFlowCtrlNone:
        ctrl.set_two_wire(1);
        break;
      case fhsi::kSerialFlowCtrlCtsRts:
        // CTS/RTS is on by default
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }
  }

  // Configure baud rate based on crystal clock speed.
  // See meson_uart_change_speed() in drivers/amlogic/uart/uart/meson_uart.c.
  constexpr uint32_t kCrystalClockSpeed = 24000000;
  uint32_t baud_bits = (kCrystalClockSpeed / 3) / baud_rate - 1;
  if (baud_bits & (~AML_UART_REG5_NEW_BAUD_RATE_MASK)) {
    zxlogf(ERROR, "%s: baud rate %u too large", __func__, baud_rate);
    return ZX_ERR_OUT_OF_RANGE;
  }
  auto baud = Reg5::Get()
                  .FromValue(0)
                  .set_new_baud_rate(baud_bits)
                  .set_use_xtal_clk(1)
                  .set_use_new_baud_rate(1);

  fbl::AutoLock al(&enable_lock_);

  if ((flags & fhsi::kSerialSetBaudRateOnly) == 0) {
    // Invert our RTS if we are we are not enabled and configured for flow control.
    if (!enabled_ && (ctrl.two_wire() == 0)) {
      ctrl.set_inv_rts(1);
    }
    ctrl.WriteTo(&mmio_);
  }

  baud.WriteTo(&mmio_);

  return ZX_OK;
}

void AmlUart::EnableLocked(bool enable) {
  auto ctrl = Control::Get().ReadFrom(&mmio_);

  if (enable) {
    // Reset the port.
    ctrl.set_rst_rx(1).set_rst_tx(1).set_clear_error(1).WriteTo(&mmio_);

    ctrl.set_rst_rx(0).set_rst_tx(0).set_clear_error(0).WriteTo(&mmio_);

    // Enable rx and tx.
    ctrl.set_tx_enable(1)
        .set_rx_enable(1)
        .set_tx_interrupt_enable(1)
        .set_rx_interrupt_enable(1)
        // Clear our RTS.
        .set_inv_rts(0)
        .WriteTo(&mmio_);

    // Set interrupt thresholds.
    // Generate interrupt if TX buffer drops below half full.
    constexpr uint32_t kTransmitIrqCount = 32;
    // Generate interrupt as soon as we receive any data.
    constexpr uint32_t kRecieveIrqCount = 1;
    Misc::Get()
        .FromValue(0)
        .set_xmit_irq_count(kTransmitIrqCount)
        .set_recv_irq_count(kRecieveIrqCount)
        .WriteTo(&mmio_);
  } else {
    ctrl.set_tx_enable(0)
        .set_rx_enable(0)
        // Invert our RTS if we are configured for flow control.
        .set_inv_rts(!ctrl.two_wire())
        .WriteTo(&mmio_);
  }
}

void AmlUart::HandleTXRaceForTest() {
  {
    fbl::AutoLock al(&enable_lock_);
    EnableLocked(true);
  }
  Writable();
  HandleTX();
  HandleTX();
}

void AmlUart::HandleRXRaceForTest() {
  {
    fbl::AutoLock al(&enable_lock_);
    EnableLocked(true);
  }
  Readable();
  HandleRX();
  HandleRX();
}

zx_status_t AmlUart::Enable(bool enable) {
  fbl::AutoLock al(&enable_lock_);

  if (enable && !enabled_) {
    zx_status_t status = pdev_.GetInterrupt(0, ZX_INTERRUPT_WAKE_VECTOR, &irq_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: pdev_get_interrupt failed %d", __func__, status);
      return status;
    }

    EnableLocked(true);

    irq_handler_.set_object(irq_.get());
    irq_handler_.Begin(irq_dispatcher_->async_dispatcher());
  } else if (!enable && enabled_) {
    irq_handler_.Cancel();
    EnableLocked(false);
  }

  enabled_ = enable;

  return ZX_OK;
}

void AmlUart::CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) {
  {
    fbl::AutoLock read_lock(&read_lock_);
    if (read_operation_) {
      auto cb = MakeReadCallbackLocked(ZX_ERR_CANCELED, nullptr, 0);
      read_lock.release();
      cb();
    }
  }
  {
    fbl::AutoLock write_lock(&write_lock_);
    if (write_operation_) {
      auto cb = MakeWriteCallbackLocked(ZX_ERR_CANCELED);
      write_lock.release();
      cb();
    }
  }

  completer.buffer(arena).Reply();
}

// Handles receiviung data into the buffer and calling the read callback when complete.
// Does nothing if read_pending_ is false.
void AmlUart::HandleRX() {
  fbl::AutoLock lock(&read_lock_);
  if (!read_operation_) {
    return;
  }
  unsigned char buf[128];
  size_t length = 128;
  auto* bufptr = static_cast<uint8_t*>(buf);
  const uint8_t* const end = bufptr + length;
  while (bufptr < end && Readable()) {
    uint32_t val = mmio_.Read32(AML_UART_RFIFO);
    *bufptr++ = static_cast<uint8_t>(val);
  }

  const size_t read = reinterpret_cast<uintptr_t>(bufptr) - reinterpret_cast<uintptr_t>(buf);
  if (read == 0) {
    return;
  }
  // Some bytes were read.  The client must queue another read to get any data.
  auto cb = MakeReadCallbackLocked(ZX_OK, buf, read);
  lock.release();
  cb();
}

// Handles transmitting the data in write_buffer_ until it is completely written.
// Does nothing if write_pending_ is not true.
void AmlUart::HandleTX() {
  fbl::AutoLock lock(&write_lock_);
  if (!write_operation_) {
    return;
  }
  const auto* bufptr = static_cast<const uint8_t*>(write_buffer_);
  const uint8_t* const end = bufptr + write_size_;
  while (bufptr < end && Writable()) {
    mmio_.Write32(*bufptr++, AML_UART_WFIFO);
  }

  const size_t written =
      reinterpret_cast<uintptr_t>(bufptr) - reinterpret_cast<uintptr_t>(write_buffer_);
  write_size_ -= written;
  write_buffer_ += written;
  if (!write_size_) {
    // The write has completed, notify the client.
    auto cb = MakeWriteCallbackLocked(ZX_OK);
    lock.release();
    cb();
  }
}

fit::closure AmlUart::MakeReadCallbackLocked(zx_status_t status, void* buf, size_t len) {
  if (read_operation_) {
    auto callback = read_operation_->MakeCallback(status, buf, len);
    read_operation_.reset();
    return callback;
  }

  ZX_PANIC("AmlUart::MakeReadCallbackLocked invalid state. No active Read operation.");
}

fit::closure AmlUart::MakeWriteCallbackLocked(zx_status_t status) {
  if (write_operation_) {
    auto callback = write_operation_->MakeCallback(status);
    write_operation_.reset();
    return callback;
  }

  ZX_PANIC("AmlUart::MakeWriteCallbackLocked invalid state. No active Write operation.");
}

void AmlUart::Config(ConfigRequestView request, fdf::Arena& arena,
                     ConfigCompleter::Sync& completer) {
  zx_status_t status = Config(request->baud_rate, request->flags);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlUart::Enable(EnableRequestView request, fdf::Arena& arena,
                     EnableCompleter::Sync& completer) {
  zx_status_t status = Enable(request->enable);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlUart::Read(fdf::Arena& arena, ReadCompleter::Sync& completer) {
  fbl::AutoLock lock(&read_lock_);
  if (read_operation_) {
    lock.release();
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  read_operation_.emplace(std::move(arena), completer.ToAsync());
  lock.release();
  HandleRX();
}

void AmlUart::Write(WriteRequestView request, fdf::Arena& arena, WriteCompleter::Sync& completer) {
  fbl::AutoLock lock(&write_lock_);
  if (write_operation_) {
    lock.release();
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  write_buffer_ = request->data.data();
  write_size_ = request->data.count();
  write_operation_.emplace(std::move(arena), completer.ToAsync());
  lock.release();
  HandleTX();
}

void AmlUart::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(WARNING, "handle_unknown_method in fuchsia_hardware_serialimpl::Device server.");
}

void AmlUart::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                        const zx_packet_interrupt_t* interrupt) {
  if (status != ZX_OK) {
    return;
  }

  auto uart_status = Status::Get().ReadFrom(&mmio_);
  if (!uart_status.rx_empty()) {
    HandleRX();
  }
  if (!uart_status.tx_full()) {
    HandleTX();
  }

  irq_.ack();
}

}  // namespace serial
