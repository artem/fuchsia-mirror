// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-spi.h"

#include <endian.h>
#include <fuchsia/hardware/spiimpl/c/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fpromise/bridge.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <string.h>
#include <threads.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <memory>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "registers.h"
#include "sdk/lib/driver/compat/cpp/metadata.h"

namespace spi {

constexpr size_t kNelsonRadarBurstSize = 23224;

// The TX and RX buffer size to allocate for DMA (only if a BTI is provided). This value is set to
// support the Selina driver on Nelson.
constexpr size_t kDmaBufferSize = fbl::round_up<size_t, size_t>(kNelsonRadarBurstSize, PAGE_SIZE);

constexpr size_t kFifoSizeWords = 16;

constexpr size_t kReset6RegisterOffset = 0x1c;
constexpr uint32_t kSpi0ResetMask = 1 << 1;
constexpr uint32_t kSpi1ResetMask = 1 << 6;

#define dump_reg(reg) FDF_LOG(ERROR, "%-21s (+%02x): %08x", #reg, reg, mmio_.Read32(reg))

void AmlSpi::DumpState() {
  // skip registers with side-effects
  // dump_reg(AML_SPI_RXDATA);
  // dump_reg(AML_SPI_TXDATA);
  dump_reg(AML_SPI_CONREG);
  dump_reg(AML_SPI_INTREG);
  dump_reg(AML_SPI_DMAREG);
  dump_reg(AML_SPI_STATREG);
  dump_reg(AML_SPI_PERIODREG);
  dump_reg(AML_SPI_TESTREG);
  dump_reg(AML_SPI_DRADDR);
  dump_reg(AML_SPI_DWADDR);
  dump_reg(AML_SPI_LD_CNTL0);
  dump_reg(AML_SPI_LD_CNTL1);
  dump_reg(AML_SPI_LD_RADDR);
  dump_reg(AML_SPI_LD_WADDR);
  dump_reg(AML_SPI_ENHANCE_CNTL);
  dump_reg(AML_SPI_ENHANCE_CNTL1);
  dump_reg(AML_SPI_ENHANCE_CNTL2);
}

#undef dump_reg

zx::result<cpp20::span<uint8_t>> AmlSpi::GetVmoSpan(uint32_t chip_select, uint32_t vmo_id,
                                                    uint64_t offset, uint64_t size,
                                                    uint32_t right) {
  vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info = registered_vmos(chip_select)->GetVmo(vmo_id);
  if (!vmo_info) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if ((vmo_info->meta().rights & right) == 0) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  if (offset + size > vmo_info->meta().size) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  return zx::ok(vmo_info->data().subspan(vmo_info->meta().offset + offset));
}

void AmlSpi::Exchange8(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) {
  // transfer settings
  auto conreg = ConReg::Get().ReadFrom(&mmio_).set_bits_per_word(CHAR_BIT - 1).WriteTo(&mmio_);

  while (size > 0) {
    // Burst size in words (with one byte per word).
    const uint32_t burst_size = std::min(kFifoSizeWords, size);

    // fill fifo
    if (txdata) {
      for (uint32_t i = 0; i < burst_size; i++) {
        mmio_.Write32(txdata[i], AML_SPI_TXDATA);
      }
      txdata += burst_size;
    } else {
      for (uint32_t i = 0; i < burst_size; i++) {
        mmio_.Write32(UINT8_MAX, AML_SPI_TXDATA);
      }
    }

    // start burst
    StatReg::Get().FromValue(0).set_tc(1).WriteTo(&mmio_);
    conreg.set_burst_length(burst_size - 1).set_xch(1).WriteTo(&mmio_);

    WaitForTransferComplete();

    // The RX FIFO may not be full immediately after receiving the transfer complete interrupt.
    // Poll until the FIFO has at least one word that can be read.
    for (uint32_t i = 0; i < burst_size; i++) {
      while (StatReg::Get().ReadFrom(&mmio_).rx_fifo_empty()) {
      }

      const uint8_t data = mmio_.Read32(AML_SPI_RXDATA) & 0xff;
      if (out_rxdata) {
        out_rxdata[i] = data;
      }
    }

    if (out_rxdata) {
      out_rxdata += burst_size;
    }

    size -= burst_size;
  }
}

void AmlSpi::Exchange64(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) {
  constexpr size_t kBytesPerWord = sizeof(uint64_t);
  constexpr size_t kMaxBytesPerBurst = kBytesPerWord * kFifoSizeWords;

  auto conreg = ConReg::Get()
                    .ReadFrom(&mmio_)
                    .set_bits_per_word((kBytesPerWord * CHAR_BIT) - 1)
                    .WriteTo(&mmio_);

  while (size >= kBytesPerWord) {
    // Burst size in 64-bit words.
    const uint32_t burst_size_words = std::min(kMaxBytesPerBurst, size) / kBytesPerWord;

    if (txdata) {
      const uint64_t* tx = reinterpret_cast<const uint64_t*>(txdata);
      for (uint32_t i = 0; i < burst_size_words; i++) {
        uint64_t value;
        memcpy(&value, &tx[i], sizeof(value));
        value = be64toh(value);
        // The controller interprets each FIFO entry as a number when they are actually just
        // bytes. To make sure the bytes come out in the intended order, treat them as big-endian,
        // and convert to little-endian for the controller.
        mmio_.Write32(value >> 32, AML_SPI_TXDATA);
        mmio_.Write32(value & UINT32_MAX, AML_SPI_TXDATA);
      }
      txdata += burst_size_words * kBytesPerWord;
    } else {
      for (uint32_t i = 0; i < burst_size_words; i++) {
        mmio_.Write32(UINT32_MAX, AML_SPI_TXDATA);
        mmio_.Write32(UINT32_MAX, AML_SPI_TXDATA);
      }
    }

    StatReg::Get().FromValue(0).set_tc(1).WriteTo(&mmio_);
    conreg.set_burst_length(burst_size_words - 1).set_xch(1).WriteTo(&mmio_);

    WaitForTransferComplete();

    // Same as Exchange8 -- poll until the FIFO has a word that can be read.
    for (uint32_t i = 0; i < burst_size_words; i++) {
      while (StatReg::Get().ReadFrom(&mmio_).rx_fifo_empty()) {
      }

      uint64_t value = mmio_.Read32(AML_SPI_RXDATA);
      value = (value << 32) | mmio_.Read32(AML_SPI_RXDATA);
      value = be64toh(value);

      if (out_rxdata) {
        memcpy(reinterpret_cast<uint64_t*>(out_rxdata) + i, &value, sizeof(value));
      }
    }

    if (out_rxdata) {
      out_rxdata += burst_size_words * kBytesPerWord;
    }

    size -= burst_size_words * kBytesPerWord;
  }

  Exchange8(txdata, out_rxdata, size);
}

void AmlSpi::SetThreadProfile() {
  if (!apply_scheduler_role_) {
    return;
  }

  apply_scheduler_role_ = false;

  if (!profile_provider_) {
    FDF_LOG(WARNING, "No profile provider, can't apply scheduler profile");
    return;
  }

  zx::thread duplicate_thread;
  zx_status_t status =
      zx::thread::self()->duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_MANAGE_THREAD, &duplicate_thread);
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "Failed to duplicate thread");
    return;
  }

  async::PostTask(
      dispatcher_->async_dispatcher(), [this, thread = std::move(duplicate_thread)]() mutable {
        // Set profile for bus transaction thread.
        const char* role_name = "fuchsia.devices.spi.drivers.aml-spi.transaction";
        profile_provider_
            ->SetProfileByRole(std::move(thread), fidl::StringView::FromExternal(role_name))
            .Then([](auto& result) {
              if (!result.ok()) {
                FDF_LOG(WARNING, "Call to apply scheduler profile failed: %s",
                        result.status_string());
              } else if (result->status != ZX_OK) {
                FDF_LOG(WARNING, "Failed to apply scheduler profile: %s",
                        zx_status_get_string(result->status));
              }
            });
      });
}

void AmlSpi::WaitForTransferComplete() {
  auto statreg = StatReg::Get().FromValue(0);
  while (!statreg.ReadFrom(&mmio_).tc()) {
    interrupt_.wait(nullptr);
  }

  statreg.WriteTo(&mmio_);
}

void AmlSpi::WaitForDmaTransferComplete() {
  auto statreg = StatReg::Get().FromValue(0);
  while (!statreg.te()) {
    interrupt_.wait(nullptr);
    // Clear the transfer complete bit (all others are read-only).
    statreg.set_reg_value(0).set_tc(1).WriteTo(&mmio_).ReadFrom(&mmio_);
  }

  // Wait for the enable bit in DMAREG to be cleared. The TX FIFO empty interrupt apparently
  // indicates this, however in some cases enable is still set after receiving it. Returning
  // without waiting for enable to be cleared leads to data loss, so just poll after the interrupt
  // to make sure.
  while (DmaReg::Get().ReadFrom(&mmio_).enable()) {
  }
}

void AmlSpi::InitRegisters() {
  fbl::AutoLock lock(&bus_lock_);
  InitRegistersLocked();
}

void AmlSpi::InitRegistersLocked() {
  ConReg::Get().FromValue(0).WriteTo(&mmio_);

  TestReg::Get().FromValue(0).set_dlyctl(config_.delay_control).set_clk_free_en(1).WriteTo(&mmio_);

  ConReg::Get()
      .ReadFrom(&mmio_)
      .set_data_rate(config_.use_enhanced_clock_mode ? 0 : config_.clock_divider_register_value)
      .set_drctl(0)
      .set_ssctl(0)
      .set_smc(0)
      .set_xch(0)
      .set_mode(ConReg::kModeMaster)
      .WriteTo(&mmio_);

  auto enhance_cntl = EnhanceCntl::Get().FromValue(0);
  if (config_.use_enhanced_clock_mode) {
    enhance_cntl.set_clk_cs_delay_enable(1)
        .set_cs_oen_enhance_enable(1)
        .set_clk_oen_enhance_enable(1)
        .set_mosi_oen_enhance_enable(1)
        .set_spi_clk_select(1)  // Use this register instead of CONREG.
        .set_enhance_clk_div(config_.clock_divider_register_value)
        .set_clk_cs_delay(0);
  }
  enhance_cntl.WriteTo(&mmio_);

  EnhanceCntl1::Get().FromValue(0).WriteTo(&mmio_);

  ConReg::Get().ReadFrom(&mmio_).set_en(1).WriteTo(&mmio_);
}

zx_status_t AmlSpi::SpiImplExchange(uint32_t cs, const uint8_t* txdata, size_t txdata_size,
                                    uint8_t* out_rxdata, size_t rxdata_size,
                                    size_t* out_rxdata_actual) {
  if (cs >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (txdata_size && rxdata_size && (txdata_size != rxdata_size)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AutoLock lock(&bus_lock_);
  if (shutdown_) {
    return ZX_ERR_CANCELED;
  }

  SetThreadProfile();

  const size_t exchange_size = txdata_size ? txdata_size : rxdata_size;

  const bool use_dma = UseDma(exchange_size);

  // There seems to be a hardware issue where transferring an odd number of bytes corrupts the TX
  // FIFO, but only for subsequent transfers that use 64-bit words. Resetting the IP avoids the
  // problem. DMA transfers do not seem to be affected.
  if (need_reset_ && reset_ && !use_dma && exchange_size >= sizeof(uint64_t)) {
    auto result = reset_->WriteRegister32(kReset6RegisterOffset, reset_mask_, reset_mask_);
    if (!result.ok() || result.value().is_error()) {
      FDF_LOG(WARNING, "Failed to reset SPI controller");
    }

    InitRegistersLocked();  // The registers must be reinitialized after resetting the IP.
    need_reset_ = false;
  } else {
    // reset both fifos
    auto testreg = TestReg::Get().ReadFrom(&mmio_).set_fiforst(3).WriteTo(&mmio_);
    do {
      testreg.ReadFrom(&mmio_);
    } while ((testreg.rxcnt() != 0) || (testreg.txcnt() != 0));

    // Resetting seems to leave an extra word in the RX FIFO, so do an extra read just in case.
    mmio_.Read32(AML_SPI_RXDATA);
    mmio_.Read32(AML_SPI_RXDATA);
  }

  IntReg::Get().FromValue(0).set_tcen(1).WriteTo(&mmio_);

  if (gpio(cs).is_valid()) {
    [[maybe_unused]] auto result = gpio(cs)->Write(0);
    ZX_ASSERT_MSG(result.ok(), "error: %s", result.FormatDescription().c_str());
    ZX_ASSERT_MSG(result->is_ok(), "error: %s", zx_status_get_string(result->error_value()));
  }

  zx_status_t status = ZX_OK;

  if (use_dma) {
    status = ExchangeDma(txdata, out_rxdata, exchange_size);
  } else if (reset_) {
    // Only use 64-bit words if we will be able to reset the controller.
    Exchange64(txdata, out_rxdata, exchange_size);
  } else {
    Exchange8(txdata, out_rxdata, exchange_size);
  }

  IntReg::Get().FromValue(0).WriteTo(&mmio_);

  if (gpio(cs).is_valid()) {
    [[maybe_unused]] auto result = gpio(cs)->Write(1);
    ZX_ASSERT_MSG(result.ok(), "error: %s", result.FormatDescription().c_str());
    ZX_ASSERT_MSG(result->is_ok(), "error: %s", zx_status_get_string(result->error_value()));
  }

  if (out_rxdata && out_rxdata_actual) {
    *out_rxdata_actual = rxdata_size;
  }

  if (exchange_size % 2 == 1) {
    need_reset_ = true;
  }

  return status;
}

zx_status_t AmlSpi::SpiImplRegisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo vmo,
                                       uint64_t offset, uint64_t size, uint32_t rights) {
  if (chip_select >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (rights & ~(SPI_VMO_RIGHT_READ | SPI_VMO_RIGHT_WRITE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  vmo_store::StoredVmo<OwnedVmoInfo> stored_vmo(std::move(vmo), OwnedVmoInfo{
                                                                    .offset = offset,
                                                                    .size = size,
                                                                    .rights = rights,
                                                                });
  const zx_vm_option_t map_opts = ((rights & SPI_VMO_RIGHT_READ) ? ZX_VM_PERM_READ : 0) |
                                  ((rights & SPI_VMO_RIGHT_WRITE) ? ZX_VM_PERM_WRITE : 0);
  zx_status_t status = stored_vmo.Map(map_opts);
  if (status != ZX_OK) {
    return status;
  }

  fbl::AutoLock lock(&vmo_lock_);
  return registered_vmos(chip_select)->RegisterWithKey(vmo_id, std::move(stored_vmo));
}

zx_status_t AmlSpi::SpiImplUnregisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo* out_vmo) {
  if (chip_select >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&vmo_lock_);

  vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info = registered_vmos(chip_select)->GetVmo(vmo_id);
  if (!vmo_info) {
    return ZX_ERR_NOT_FOUND;
  }

  zx_status_t status = vmo_info->vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, out_vmo);
  if (status != ZX_OK) {
    return status;
  }

  auto result = registered_vmos(chip_select)->Unregister(vmo_id);
  if (result.is_error()) {
    return result.status_value();
  }

  *out_vmo = std::move(result.value());
  return ZX_OK;
}

void AmlSpi::SpiImplReleaseRegisteredVmos(uint32_t chip_select) {
  fbl::AutoLock lock(&vmo_lock_);
  registered_vmos(chip_select).emplace(vmo_store::Options{});
}

zx_status_t AmlSpi::SpiImplTransmitVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                       uint64_t size) {
  if (chip_select >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&vmo_lock_);

  zx::result<cpp20::span<const uint8_t>> buffer =
      GetVmoSpan(chip_select, vmo_id, offset, size, SPI_VMO_RIGHT_READ);
  if (buffer.is_error()) {
    return buffer.error_value();
  }

  return SpiImplExchange(chip_select, buffer->data(), size, nullptr, 0, nullptr);
}

zx_status_t AmlSpi::SpiImplReceiveVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                      uint64_t size) {
  if (chip_select >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&vmo_lock_);

  zx::result<cpp20::span<uint8_t>> buffer =
      GetVmoSpan(chip_select, vmo_id, offset, size, SPI_VMO_RIGHT_WRITE);
  if (buffer.is_error()) {
    return buffer.error_value();
  }

  return SpiImplExchange(chip_select, nullptr, 0, buffer->data(), size, nullptr);
}

zx_status_t AmlSpi::SpiImplExchangeVmo(uint32_t chip_select, uint32_t tx_vmo_id, uint64_t tx_offset,
                                       uint32_t rx_vmo_id, uint64_t rx_offset, uint64_t size) {
  if (chip_select >= SpiImplGetChipSelectCount()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AutoLock lock(&vmo_lock_);

  zx::result<cpp20::span<uint8_t>> tx_buffer =
      GetVmoSpan(chip_select, tx_vmo_id, tx_offset, size, SPI_VMO_RIGHT_READ);
  if (tx_buffer.is_error()) {
    return tx_buffer.error_value();
  }

  zx::result<cpp20::span<uint8_t>> rx_buffer =
      GetVmoSpan(chip_select, rx_vmo_id, rx_offset, size, SPI_VMO_RIGHT_WRITE);
  if (rx_buffer.is_error()) {
    return rx_buffer.error_value();
  }

  return SpiImplExchange(chip_select, tx_buffer->data(), size, rx_buffer->data(), size, nullptr);
}

zx_status_t AmlSpi::ExchangeDma(const uint8_t* txdata, uint8_t* out_rxdata, uint64_t size) {
  constexpr size_t kBytesPerWord = sizeof(uint64_t);

  if (txdata) {
    if (config_.client_reverses_dma_transfers) {
      memcpy(tx_buffer_.mapped.start(), txdata, size);
    } else {
      // Copy the TX data into the pinned VMO and reverse the endianness.
      auto* tx_vmo = static_cast<uint64_t*>(tx_buffer_.mapped.start());
      for (size_t offset = 0; offset < size; offset += kBytesPerWord) {
        uint64_t tmp;
        memcpy(&tmp, &txdata[offset], sizeof(tmp));
        *tx_vmo++ = be64toh(tmp);
      }
    }
  } else {
    memset(tx_buffer_.mapped.start(), 0xff, size);
  }

  zx_status_t status = tx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN, 0, size, nullptr, 0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
    return status;
  }

  if (out_rxdata) {
    status = rx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN, 0, size, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
      return status;
    }
  }

  ConReg::Get().ReadFrom(&mmio_).set_bits_per_word((kBytesPerWord * CHAR_BIT) - 1).WriteTo(&mmio_);

  const fzl::PinnedVmo::Region tx_region = tx_buffer_.pinned.region(0);
  const fzl::PinnedVmo::Region rx_region = rx_buffer_.pinned.region(0);

  mmio_.Write32(tx_region.phys_addr, AML_SPI_DRADDR);
  mmio_.Write32(rx_region.phys_addr, AML_SPI_DWADDR);
  mmio_.Write32(0, AML_SPI_PERIODREG);

  DmaReg::Get().FromValue(0).WriteTo(&mmio_);

  // The SPI controller issues requests to DDR to fill the TX FIFO/drain the RX FIFO. The reference
  // driver uses requests up to the FIFO size (16 words) when that many words are remaining, or 2-8
  // word requests otherwise. 16-word requests didn't seem to work in testing, and only 8-word
  // requests are used by default here for simplicity.
  for (size_t words_remaining = size / kBytesPerWord; words_remaining > 0;) {
    const size_t transfer_size = DoDmaTransfer(words_remaining);

    // Enable the TX FIFO empty interrupt and set the start mode control bit on the first run
    // through the loop.
    if (words_remaining == (size / kBytesPerWord)) {
      IntReg::Get().FromValue(0).set_teen(1).WriteTo(&mmio_);
      ConReg::Get().ReadFrom(&mmio_).set_smc(1).WriteTo(&mmio_);
    }

    WaitForDmaTransferComplete();

    words_remaining -= transfer_size;
  }

  DmaReg::Get().ReadFrom(&mmio_).set_enable(0).WriteTo(&mmio_);
  IntReg::Get().FromValue(0).WriteTo(&mmio_);
  LdCntl0::Get().FromValue(0).WriteTo(&mmio_);
  ConReg::Get().ReadFrom(&mmio_).set_smc(0).WriteTo(&mmio_);

  if (out_rxdata) {
    status = rx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN_INVALIDATE, 0, size, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to invalidate cache: %s", zx_status_get_string(status));
      return status;
    }

    if (config_.client_reverses_dma_transfers) {
      memcpy(out_rxdata, rx_buffer_.mapped.start(), size);
    } else {
      const auto* rx_vmo = static_cast<uint64_t*>(rx_buffer_.mapped.start());
      for (size_t offset = 0; offset < size; offset += kBytesPerWord) {
        uint64_t tmp = htobe64(*rx_vmo++);
        memcpy(&out_rxdata[offset], &tmp, sizeof(tmp));
      }
    }
  }

  return ZX_OK;
}

size_t AmlSpi::DoDmaTransfer(size_t words_remaining) {
  // These are the limits used by the reference driver, although request sizes up to the FIFO size
  // should work, and the read/write counters are 16 bits wide.
  constexpr size_t kDefaultRequestSizeWords = 8;
  constexpr size_t kMaxRequestCount = 0xfff;

  // TODO(https://fxbug.dev/42051588): It may be possible to complete the transfer in fewer iterations by
  // using request sizes 2-7 instead of 8, like the reference driver does.
  const size_t request_size =
      words_remaining < kFifoSizeWords ? words_remaining : kDefaultRequestSizeWords;
  const size_t request_count = std::min(words_remaining / request_size, kMaxRequestCount);

  LdCntl0::Get().FromValue(0).set_read_counter_enable(1).set_write_counter_enable(1).WriteTo(
      &mmio_);
  LdCntl1::Get()
      .FromValue(0)
      .set_dma_read_counter(request_count)
      .set_dma_write_counter(request_count)
      .WriteTo(&mmio_);

  DmaReg::Get()
      .FromValue(0)
      .set_enable(1)
      // No explanation for these -- see the reference driver.
      .set_urgent(1)
      .set_txfifo_threshold(kFifoSizeWords + 1 - request_size)
      .set_read_request_burst_size(request_size - 1)
      .set_rxfifo_threshold(request_size - 1)
      .set_write_request_burst_size(request_size - 1)
      .WriteTo(&mmio_);

  return request_size * request_count;
}

bool AmlSpi::UseDma(size_t size) const {
  // TODO(https://fxbug.dev/42051588): Support DMA transfers greater than the pre-allocated buffer size.
  return size % sizeof(uint64_t) == 0 && size <= tx_buffer_.mapped.size() &&
         size <= rx_buffer_.mapped.size();
}

fbl::Array<AmlSpi::ChipInfo> AmlSpiDriver::InitChips(const amlogic_spi::amlspi_config_t& config) {
  fbl::Array<AmlSpi::ChipInfo> chips(new AmlSpi::ChipInfo[config.cs_count], config.cs_count);
  if (!chips) {
    return chips;
  }

  for (uint32_t i = 0; i < config.cs_count; i++) {
    uint32_t index = config.cs[i];
    if (index == amlogic_spi::amlspi_config_t::kCsClientManaged) {
      continue;
    }

    char fragment_name[32] = {};
    snprintf(fragment_name, 32, "gpio-cs-%d", index);
    zx::result client = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(fragment_name);
    if (client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to GPIO device: %s", client.status_string());
      return fbl::Array<AmlSpi::ChipInfo>();
    }
    chips[i].gpio = fidl::WireSyncClient(std::move(*client));
  }

  return chips;
}

void AmlSpiDriver::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()), dispatcher());

  {
    zx::result pdev_client =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (pdev_client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
      return completer(pdev_client.take_error());
    }

    pdev_.Bind(*std::move(pdev_client), dispatcher());
  }

  {
    zx::result compat_client = incoming()->Connect<fuchsia_driver_compat::Service::Device>("pdev");
    if (compat_client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to compat: %s", compat_client.status_string());
      return completer(compat_client.take_error());
    }

    compat_.Bind(*std::move(compat_client), dispatcher());
  }

  compat::DeviceServer::BanjoConfig banjo_config{
      .default_proto_id = ZX_PROTOCOL_SPI_IMPL,
      .generic_callback = fit::bind_member<&AmlSpiDriver::GetBanjoProto>(this),
  };

  compat_server_.Begin(
      incoming(), outgoing(), node_name(), component::kDefaultInstance,
      [this, completer = std::move(completer)](zx::result<> result) mutable {
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to initialize compat server: %s", result.status_string());
          return completer(result.take_error());
        }

        OnCompatServerInitialized(std::move(completer));
      },
      compat::ForwardMetadata::Some({DEVICE_METADATA_SPI_CHANNELS}), std::move(banjo_config));
}

void AmlSpiDriver::OnCompatServerInitialized(fdf::StartCompleter completer) {
  auto task =
      fpromise::join_promises(MapMmio(pdev_, 0), GetConfig(), GetInterrupt(), GetBti())
          .then([this, completer = std::move(completer)](
                    fpromise::result<
                        std::tuple<fpromise::result<fdf::MmioBuffer, zx_status_t>,
                                   fpromise::result<amlogic_spi::amlspi_config_t, zx_status_t>,
                                   fpromise::result<zx::interrupt, zx_status_t>,
                                   fpromise::result<zx::bti, zx_status_t>>>& results) mutable {
            if (results.is_error()) {
              FDF_LOG(ERROR, "Failed to get resources");
              return completer(zx::error(ZX_ERR_INTERNAL));
            }

            if (std::get<0>(results.value()).is_error()) {
              return completer(zx::error(std::get<0>(results.value()).error()));
            }
            auto& mmio = std::get<0>(results.value()).value();

            if (std::get<1>(results.value()).is_error()) {
              return completer(zx::error(std::get<1>(results.value()).error()));
            }
            const auto& config = std::get<1>(results.value()).value();

            if (std::get<2>(results.value()).is_error()) {
              return completer(zx::error(std::get<2>(results.value()).error()));
            }
            zx::interrupt interrupt = std::move(std::get<2>(results.value()).value());

            zx::bti bti{};
            if (std::get<3>(results.value()).is_ok()) {
              bti = std::move(std::get<3>(results.value()).value());
            }

            AddNode(std::move(mmio), config, std::move(interrupt), std::move(bti),
                    std::move(completer));
          });
  executor_.schedule_task(std::move(task));
}

void AmlSpiDriver::AddNode(fdf::MmioBuffer mmio, const amlogic_spi::amlspi_config_t& config,
                           zx::interrupt interrupt, zx::bti bti, fdf::StartCompleter completer) {
  // Stop DMA in case the driver is restarting and didn't shut down cleanly.
  DmaReg::Get().FromValue(0).WriteTo(&mmio);
  ConReg::Get().FromValue(0).WriteTo(&mmio);

  const uint32_t max_clock_div_reg_value =
      config.use_enhanced_clock_mode ? EnhanceCntl::kEnhanceClkDivMax : ConReg::kDataRateMax;
  if (config.clock_divider_register_value > max_clock_div_reg_value) {
    FDF_LOG(ERROR, "Metadata clock divider value is too large: %u",
            config.clock_divider_register_value);
    return completer(zx::error(ZX_ERR_INVALID_ARGS));
  }

  zx::result reset_register_client =
      incoming()->Connect<fuchsia_hardware_registers::Service::Device>("reset");
  if (reset_register_client.is_error()) {
    FDF_LOG(WARNING, "Did not bind the reset register client.");
  }

  zx::result profile_provider = incoming()->Connect<fuchsia_scheduler::ProfileProvider>();
  if (profile_provider.is_error()) {
    FDF_LOG(WARNING, "Failed to connect to ProfileProvider");
  }

  AmlSpi::DmaBuffer tx_buffer, rx_buffer;
  // Supplying a BTI is optional.
  if (bti.is_valid()) {
    // DMA was stopped above, so it's safe to release any quarantined pages.
    bti.release_quarantine();

    zx_status_t status;
    if ((status = AmlSpi::DmaBuffer::Create(bti, kDmaBufferSize, &tx_buffer)) != ZX_OK) {
      return completer(zx::error(status));
    }
    if ((status = AmlSpi::DmaBuffer::Create(bti, kDmaBufferSize, &rx_buffer)) != ZX_OK) {
      return completer(zx::error(status));
    }
    FDF_LOG(DEBUG, "Got BTI and contiguous buffers, DMA may be used");
  }

  fbl::Array<AmlSpi::ChipInfo> chips = InitChips(config);
  if (!chips) {
    return completer(zx::error(ZX_ERR_NO_RESOURCES));
  }
  if (chips.size() == 0) {
    return completer(zx::ok());
  }

  const uint32_t reset_mask =
      config.bus_id == 0 ? kSpi0ResetMask : (config.bus_id == 1 ? kSpi1ResetMask : 0);

  fbl::AllocChecker ac;
  device_.reset(new (&ac) AmlSpi(std::move(mmio), *std::move(reset_register_client),
                                 *std::move(profile_provider), reset_mask, std::move(chips),
                                 std::move(interrupt), config, std::move(bti), std::move(tx_buffer),
                                 std::move(rx_buffer)));
  if (!ac.check()) {
    return completer(zx::error(ZX_ERR_NO_MEMORY));
  }

  device_->InitRegisters();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return completer(controller_endpoints.take_error());
  }

  controller_.Bind(std::move(controller_endpoints->client), dispatcher());

  char devname[32];
  sprintf(devname, "aml-spi-%u", config.bus_id);

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, "fuchsia.hardware.spiimpl.Service",
                                    "fuchsia.hardware.spiimpl.Service.DriverTransport");

  std::vector offers = compat_server_.CreateOffers2(arena);
  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, devname)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  parent_->AddChild(args, std::move(controller_endpoints->server), {})
      .Then([completer = std::move(completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to add child failed: %s", result.status_string());
          return completer(zx::error(result.status()));
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to add child");
          return completer(zx::error(ZX_ERR_INTERNAL));
        }
        completer(zx::ok());
      });
}

fpromise::promise<amlogic_spi::amlspi_config_t, zx_status_t> AmlSpiDriver::GetConfig() {
  fpromise::bridge<amlogic_spi::amlspi_config_t, zx_status_t> bridge;

  auto task = compat::GetMetadataAsync<amlogic_spi::amlspi_config_t>(
      fdf::Dispatcher::GetCurrent()->async_dispatcher(), incoming(), DEVICE_METADATA_AMLSPI_CONFIG,
      [completer = std::move(bridge.completer)](
          zx::result<std::unique_ptr<amlogic_spi::amlspi_config_t>> result) mutable {
        if (result.is_ok()) {
          completer.complete_ok(**result);
        } else {
          FDF_LOG(ERROR, "Failed to get metadata: %s", zx_status_get_string(result.error_value()));
          completer.complete_error(result.error_value());
        }
      },
      "pdev");

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL))
      .then([task = std::move(task)](
                fpromise::result<amlogic_spi::amlspi_config_t, zx_status_t>& result) mutable {
        return result;
      });
}

fpromise::promise<zx::interrupt, zx_status_t> AmlSpiDriver::GetInterrupt() {
  fpromise::bridge<zx::interrupt, zx_status_t> bridge;

  pdev_->GetInterruptById(0, 0).Then(
      [completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to get SPI interrupt failed: %s", result.status_string());
          completer.complete_error(result.status());
        } else if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to get SPI interrupt: %s",
                  zx_status_get_string(result->error_value()));
          completer.complete_error(result->error_value());
        } else {
          completer.complete_ok(std::move(result->value()->irq));
        }
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL));
}

fpromise::promise<zx::bti, zx_status_t> AmlSpiDriver::GetBti() {
  fpromise::bridge<zx::bti, zx_status_t> bridge;

  pdev_->GetBtiById(0).Then([completer = std::move(bridge.completer)](auto& result) mutable {
    if (!result.ok()) {
      completer.complete_error(result.status());
    } else if (result->is_error()) {
      completer.complete_error(result->error_value());
    } else {
      completer.complete_ok(std::move(result->value()->bti));
    }
  });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL));
}

zx_status_t AmlSpi::DmaBuffer::Create(const zx::bti& bti, size_t size, DmaBuffer* out_dma_buffer) {
  zx_status_t status;
  if ((status = zx::vmo::create_contiguous(bti, size, 0, &out_dma_buffer->vmo)) != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create DMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  status = out_dma_buffer->pinned.Pin(out_dma_buffer->vmo, bti,
                                      ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE | ZX_BTI_CONTIGUOUS);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin DMA VMO: %s", zx_status_get_string(status));
    return status;
  }
  if (out_dma_buffer->pinned.region_count() != 1) {
    FDF_LOG(ERROR, "Invalid region count for contiguous VMO: %u",
            out_dma_buffer->pinned.region_count());
    return status;
  }

  if ((status = out_dma_buffer->mapped.Map(out_dma_buffer->vmo)) != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map DMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx::result<compat::DeviceServer::GenericProtocol> AmlSpiDriver::GetBanjoProto(
    compat::BanjoProtoId id) {
  ZX_DEBUG_ASSERT(device_);

  if (id != ZX_PROTOCOL_SPI_IMPL) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::ok(compat::DeviceServer::GenericProtocol{
      .ops = device_->ops(),
      .ctx = device_.get(),
  });
}

void AmlSpi::Shutdown() {
  // Wait for any pending transfer to complete, then stop DMA and disable the controller.
  fbl::AutoLock lock(&bus_lock_);

  shutdown_ = true;

  DmaReg::Get().FromValue(0).WriteTo(&mmio_);
  ConReg::Get().FromValue(0).WriteTo(&mmio_);

  // Under normal circumstances these are unpinned when the objects are destroyed. DdkRelease() is
  // not always called however, so manually unpin here after DMA has been stopped.
  tx_buffer_.pinned.Unpin();
  rx_buffer_.pinned.Unpin();
}

fpromise::promise<fdf::MmioBuffer, zx_status_t> AmlSpiDriver::MapMmio(
    fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) {
  fpromise::bridge<fdf::MmioBuffer, zx_status_t> bridge;

  pdev->GetMmioById(mmio_id).Then(
      [mmio_id, completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to get MMIO %u failed: %s", mmio_id, result.status_string());
          return completer.complete_error(result.status());
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to get MMIO %u: %s", mmio_id,
                  zx_status_get_string(result->error_value()));
          return completer.complete_error(result->error_value());
        }

        auto& mmio = *result->value();
        if (!mmio.has_offset() || !mmio.has_size() || !mmio.has_vmo()) {
          FDF_LOG(ERROR, "Invalid MMIO returned for ID %u", mmio_id);
          return completer.complete_error(ZX_ERR_BAD_STATE);
        }

        zx::result mmio_buffer = fdf::MmioBuffer::Create(
            mmio.offset(), mmio.size(), std::move(mmio.vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
        if (mmio_buffer.is_error()) {
          FDF_LOG(ERROR, "Failed to map MMIO %u: %s", mmio_id,
                  zx_status_get_string(mmio_buffer.error_value()));
          return completer.complete_error(mmio_buffer.error_value());
        }

        completer.complete_ok(*std::move(mmio_buffer));
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_BAD_STATE));
}

}  // namespace spi

FUCHSIA_DRIVER_EXPORT(spi::AmlSpiDriver);
