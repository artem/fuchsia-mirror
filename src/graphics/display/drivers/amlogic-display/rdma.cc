// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/rdma.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/bti.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <lib/zx/vmar.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/irq-handler-loop-util.h"
#include "src/graphics/display/drivers/amlogic-display/rdma-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpp-regs.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/logging/zxlogf.h"

namespace amlogic_display {

// static
zx::result<std::unique_ptr<RdmaEngine>> RdmaEngine::Create(
    display::DispatcherFactory& dispatcher_factory,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device,
    inspect::Node* video_input_unit_node) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());

  zx::result<zx::bti> bti_result = GetBti(BtiResourceIndex::kDma, platform_device);
  if (bti_result.is_error()) {
    zxlogf(ERROR, "Could not get BTI handle");
    return zx::error(bti_result.error_value());
  }
  zx::bti dma_bti = std::move(bti_result).value();

  // Map RDMA Done Interrupt
  zx::result<zx::interrupt> rdma_done_result =
      GetInterrupt(InterruptResourceIndex::kRdmaDone, platform_device);
  if (rdma_done_result.is_error()) {
    return rdma_done_result.take_error();
  }
  zx::interrupt rdma_done_interrupt = std::move(rdma_done_result).value();

  zx::result<fdf::MmioBuffer> vpu_mmio_result = MapMmio(MmioResourceIndex::kVpu, platform_device);
  if (vpu_mmio_result.is_error()) {
    return vpu_mmio_result.take_error();
  }
  fdf::MmioBuffer vpu_mmio = std::move(vpu_mmio_result).value();

  zx::result<std::unique_ptr<display::Dispatcher>> create_dispatcher_result =
      dispatcher_factory.Create("rdma_irq_thread", /*scheduler_role=*/{});
  if (create_dispatcher_result.is_error()) {
    zxlogf(ERROR, "Failed to create IRQ handler dispatcher: %s",
           create_dispatcher_result.status_string());
    return create_dispatcher_result.take_error();
  }
  std::unique_ptr<display::Dispatcher> dispatcher = std::move(create_dispatcher_result).value();

  fbl::AllocChecker alloc_checker;
  auto rdma = fbl::make_unique_checked<RdmaEngine>(
      &alloc_checker, std::move(vpu_mmio), std::move(dma_bti), std::move(rdma_done_interrupt),
      std::move(dispatcher), video_input_unit_node);
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(rdma));
}

RdmaEngine::RdmaEngine(fdf::MmioBuffer vpu_mmio, zx::bti dma_bti, zx::interrupt rdma_done_interrupt,
                       std::unique_ptr<display::Dispatcher> rdma_irq_handler_dispatcher,
                       inspect::Node* inspect_node)
    : vpu_mmio_(std::move(vpu_mmio)),
      bti_(std::move(dma_bti)),
      rdma_irq_(std::move(rdma_done_interrupt)),
      rdma_irq_handler_dispatcher_(std::move(rdma_irq_handler_dispatcher)),
      rdma_allocation_failures_(inspect_node->CreateUint("rdma_allocation_failures", 0)),
      rdma_irq_count_(inspect_node->CreateUint("rdma_irq_count", 0)),
      rdma_begin_count_(inspect_node->CreateUint("rdma_begin_count", 0)),
      rdma_pending_in_vsync_count_(inspect_node->CreateUint("rdma_pending_in_vsync_count", 0)),
      last_rdma_pending_in_vsync_interval_ns_(
          inspect_node->CreateUint("last_rdma_pending_in_vsync_interval_ns", 0)),
      last_rdma_pending_in_vsync_timestamp_ns_prop_(
          inspect_node->CreateUint("last_rdma_pending_in_vsync_timestamp_ns", 0)) {
  rdma_irq_handler_.set_object(rdma_irq_.get());
}

void RdmaEngine::TryResolvePendingRdma() {
  ZX_DEBUG_ASSERT(rdma_active_);

  zx::time now = zx::clock::get_monotonic();
  auto rdma_status = RdmaStatusReg::Get().ReadFrom(&vpu_mmio_);
  if (!rdma_status.ChannelDone(kRdmaChannel)) {
    // The configs scheduled to apply on the previous vsync have not been processed by the RDMA
    // engine yet. Log some statistics on how often this situation occurs.
    rdma_pending_in_vsync_count_.Add(1);

    zx::duration interval = now - last_rdma_pending_in_vsync_timestamp_;
    last_rdma_pending_in_vsync_timestamp_ = now;
    last_rdma_pending_in_vsync_timestamp_ns_prop_.Set(last_rdma_pending_in_vsync_timestamp_.get());
    last_rdma_pending_in_vsync_interval_ns_.Set(interval.get());
  }

  // If RDMA for AFBC just completed, simply clear the interrupt. We keep RDMA enabled to
  // automatically get triggered on every vsync. FlipOnVsync is responsible for enabling/disabling
  // AFBC-related RDMA based on configs.
  if (rdma_status.ChannelDone(kAfbcRdmaChannel, &vpu_mmio_)) {
    RdmaCtrlReg::ClearInterrupt(kAfbcRdmaChannel, &vpu_mmio_);
  }

  if (rdma_status.ChannelDone(kRdmaChannel)) {
    RdmaCtrlReg::ClearInterrupt(kRdmaChannel, &vpu_mmio_);

    uint32_t regVal = vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO);
    regVal &= ~RDMA_ACCESS_AUTO_INT_EN(kRdmaChannel);  // Remove VSYNC interrupt source
    vpu_mmio_.Write32(regVal, VPU_RDMA_ACCESS_AUTO);

    // Read and store the last applied image handle and drive the RDMA state machine forward.
    ProcessRdmaUsageTable();
  }
}

display::ConfigStamp RdmaEngine::GetLastConfigStampApplied() {
  fbl::AutoLock lock(&rdma_lock_);
  if (rdma_active_) {
    TryResolvePendingRdma();
  }
  return latest_applied_config_;
}

void RdmaEngine::ProcessRdmaUsageTable() {
  ZX_DEBUG_ASSERT(rdma_active_);

  // Find out how far did the RDMA write
  uint64_t val = (static_cast<uint64_t>(vpu_mmio_.Read32(VPP_DUMMY_DATA1)) << 32) |
                 (vpu_mmio_.Read32(VPP_OSD_SC_DUMMY_DATA));
  size_t last_table_index = -1;
  // FIXME: or search until end_index_used_. Either way, end_index_used_ will
  // always be less than kNumberOfTables. So no penalty
  for (size_t i = start_index_used_; i < kNumberOfTables && last_table_index == -1ul; i++) {
    if (val == rdma_usage_table_[i]) {
      // Found the last table that was written to
      last_table_index = i;
      latest_applied_config_ = display::ConfigStamp(rdma_usage_table_[i]);  // save this for vsync
    }
    rdma_usage_table_[i] = kRdmaTableUnavailable;  // mark as unavailable for now
  }
  if (last_table_index == -1ul) {
    zxlogf(ERROR, "RDMA handler could not find last used table index");
    DumpRdmaState();

    // Pretend that all configs have been completed to recover. The next code block will initialize
    // the entire table as ready to consume new configs.
    last_table_index = end_index_used_;
  }

  rdma_active_ = false;

  // Only mark ready if we actually completed all the configs.
  if (last_table_index == end_index_used_) {
    // Clear them up
    for (size_t i = 0; i <= last_table_index; i++) {
      rdma_usage_table_[i] = kRdmaTableReady;
    }
  } else {
    // We have pending configs. Let's schedule it. First,
    // We want to schedule a new RDMA from <last_table_index + 1> to end_index_used
    // Write the start and end address of the table. End address is the last address that
    // the RDMA engine reads from.
    start_index_used_ = static_cast<uint8_t>(last_table_index + 1);
    vpu_mmio_.Write32(static_cast<uint32_t>(rdma_channels_[start_index_used_].phys_offset),
                      VPU_RDMA_AHB_START_ADDR(kRdmaChannel));
    vpu_mmio_.Write32(
        static_cast<uint32_t>(rdma_channels_[start_index_used_].phys_offset + kTableSize - 4),
        VPU_RDMA_AHB_END_ADDR(kRdmaChannel));
    uint32_t regVal = vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO);
    regVal |= RDMA_ACCESS_AUTO_INT_EN(kRdmaChannel);  // VSYNC interrupt source
    regVal |= RDMA_ACCESS_AUTO_WRITE(kRdmaChannel);   // Write
    vpu_mmio_.Write32(regVal, VPU_RDMA_ACCESS_AUTO);
    rdma_active_ = true;
    rdma_begin_count_.Add(1);
  }
}

int RdmaEngine::GetNextAvailableRdmaTableIndex() {
  fbl::AutoLock lock(&rdma_lock_);
  for (uint32_t i = 0; i < kNumberOfTables; i++) {
    if (rdma_usage_table_[i] == kRdmaTableReady) {
      return i;
    }
  }
  rdma_allocation_failures_.Add(1);
  return -1;
}

void RdmaEngine::ResetRdmaTable() {
  for (const RdmaChannelContainer& rdma_channel : rdma_channels_) {
    auto* rdma_table = reinterpret_cast<RdmaTable*>(rdma_channel.virt_offset);
    rdma_table[IDX_BLK0_CFG_W0].reg = (VPU_VIU_OSD1_BLK0_CFG_W0 >> 2);
    rdma_table[IDX_CTRL_STAT].reg = (VPU_VIU_OSD1_CTRL_STAT >> 2);
    rdma_table[IDX_CTRL_STAT2].reg = (VPU_VIU_OSD1_CTRL_STAT2 >> 2);
    rdma_table[IDX_MATRIX_EN_CTRL].reg = (VPU_VPP_POST_MATRIX_EN_CTRL >> 2);
    rdma_table[IDX_MATRIX_COEF00_01].reg = (VPU_VPP_POST_MATRIX_COEF00_01 >> 2);
    rdma_table[IDX_MATRIX_COEF02_10].reg = (VPU_VPP_POST_MATRIX_COEF02_10 >> 2);
    rdma_table[IDX_MATRIX_COEF11_12].reg = (VPU_VPP_POST_MATRIX_COEF11_12 >> 2);
    rdma_table[IDX_MATRIX_COEF20_21].reg = (VPU_VPP_POST_MATRIX_COEF20_21 >> 2);
    rdma_table[IDX_MATRIX_COEF22].reg = (VPU_VPP_POST_MATRIX_COEF22 >> 2);
    rdma_table[IDX_MATRIX_OFFSET0_1].reg = (VPU_VPP_POST_MATRIX_OFFSET0_1 >> 2);
    rdma_table[IDX_MATRIX_OFFSET2].reg = (VPU_VPP_POST_MATRIX_OFFSET2 >> 2);
    rdma_table[IDX_MATRIX_PRE_OFFSET0_1].reg = (VPU_VPP_POST_MATRIX_PRE_OFFSET0_1 >> 2);
    rdma_table[IDX_MATRIX_PRE_OFFSET2].reg = (VPU_VPP_POST_MATRIX_PRE_OFFSET2 >> 2);
    rdma_table[IDX_BLK2_CFG_W4].reg = (VPU_VIU_OSD1_BLK2_CFG_W4 >> 2);
    rdma_table[IDX_MALI_UNPACK_CTRL].reg = (VPU_VIU_OSD1_MALI_UNPACK_CTRL >> 2);
    rdma_table[IDX_PATH_MISC_CTRL].reg = (VPU_OSD_PATH_MISC_CTRL >> 2);
    rdma_table[IDX_AFBC_HEAD_BUF_ADDR_LOW].reg = (VPU_MAFBC_HEADER_BUF_ADDR_LOW_S0 >> 2);
    rdma_table[IDX_AFBC_HEAD_BUF_ADDR_HIGH].reg = (VPU_MAFBC_HEADER_BUF_ADDR_HIGH_S0 >> 2);
    rdma_table[IDX_AFBC_SURFACE_CFG].reg = (VPU_MAFBC_SURFACE_CFG >> 2);
    rdma_table[IDX_RDMA_CFG_STAMP_HIGH].reg = (VPP_DUMMY_DATA1 >> 2);
    rdma_table[IDX_RDMA_CFG_STAMP_LOW].reg = (VPP_OSD_SC_DUMMY_DATA >> 2);
  }
  auto* afbc_rdma_table = reinterpret_cast<RdmaTable*>(afbc_rdma_channel_.virt_offset);
  afbc_rdma_table->reg = (VPU_MAFBC_COMMAND >> 2);
}

void RdmaEngine::SetRdmaTableValue(uint32_t table_index, uint32_t idx, uint32_t val) {
  ZX_DEBUG_ASSERT(idx < IDX_MAX);
  ZX_DEBUG_ASSERT(table_index < kNumberOfTables);
  auto* rdma_table = reinterpret_cast<RdmaTable*>(rdma_channels_[table_index].virt_offset);
  rdma_table[idx].val = val;
}

void RdmaEngine::FlushRdmaTable(uint32_t table_index) {
  zx_status_t status =
      zx_cache_flush(rdma_channels_[table_index].virt_offset, IDX_MAX * sizeof(RdmaTable),
                     ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not clean cache: %s", zx_status_get_string(status));
    return;
  }
}

void RdmaEngine::ExecRdmaTable(uint32_t next_table_idx, display::ConfigStamp config_stamp,
                               bool use_afbc) {
  fbl::AutoLock lock(&rdma_lock_);
  // Write the start and end address of the table. End address is the last address that the
  // RDMA engine reads from.
  // if rdma is already active, just update the end_addr
  if (rdma_active_) {
    end_index_used_ = static_cast<uint8_t>(next_table_idx);
    rdma_usage_table_[next_table_idx] = config_stamp.value();
    vpu_mmio_.Write32(
        static_cast<uint32_t>(rdma_channels_[next_table_idx].phys_offset + kTableSize - 4),
        VPU_RDMA_AHB_END_ADDR(kRdmaChannel));
    return;
  }

  start_index_used_ = static_cast<uint8_t>(next_table_idx);
  end_index_used_ = start_index_used_;

  vpu_mmio_.Write32(static_cast<uint32_t>(rdma_channels_[next_table_idx].phys_offset),
                    VPU_RDMA_AHB_START_ADDR(kRdmaChannel));
  vpu_mmio_.Write32(
      static_cast<uint32_t>(rdma_channels_[next_table_idx].phys_offset + kTableSize - 4),
      VPU_RDMA_AHB_END_ADDR(kRdmaChannel));

  // Enable Auto mode: Non-Increment, VSync Interrupt Driven, Write
  uint32_t regVal = vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO);
  regVal |= RDMA_ACCESS_AUTO_INT_EN(kRdmaChannel);  // VSYNC interrupt source
  regVal |= RDMA_ACCESS_AUTO_WRITE(kRdmaChannel);   // Write
  vpu_mmio_.Write32(regVal, VPU_RDMA_ACCESS_AUTO);
  rdma_usage_table_[next_table_idx] = config_stamp.value();
  rdma_active_ = true;
  rdma_begin_count_.Add(1);
  if (use_afbc) {
    // Enable Auto mode: Non-Increment, VSync Interrupt Driven, Write
    RdmaAccessAuto2Reg::Get().FromValue(0).set_chn7_auto_write(1).WriteTo(&vpu_mmio_);
    RdmaAccessAuto3Reg::Get().FromValue(0).set_chn7_intr(1).WriteTo(&vpu_mmio_);
  } else {
    // Remove interrupt source
    RdmaAccessAuto3Reg::Get().FromValue(0).set_chn7_intr(0).WriteTo(&vpu_mmio_);
  }
}

zx_status_t RdmaEngine::SetupRdma() {
  zx_status_t status = ZX_OK;
  zxlogf(DEBUG, "Setting up Display RDMA");

  // First, clean up any ongoing DMA that a previous incarnation of this driver
  // may have started, and tell the BTI to drop its quarantine list.
  StopRdma();
  bti_.release_quarantine();

  status = zx::vmo::create_contiguous(bti_, kRdmaRegionSize, 0, &rdma_vmo_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not create RDMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  zx_paddr_t rdma_physical_address = 0;
  status = bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, rdma_vmo_, 0, kRdmaRegionSize,
                    &rdma_physical_address, 1, &rdma_pmt_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not create RDMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  zx_vaddr_t rdma_virtual_address = 0;
  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, rdma_vmo_, 0,
                                      kRdmaRegionSize, &rdma_virtual_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map RDMA VMO: %s", zx_status_get_string(status));
    return status;
  }
  const cpp20::span<uint8_t> rdma_region(reinterpret_cast<uint8_t*>(rdma_virtual_address),
                                         kRdmaRegionSize);

  // At this point, we have a table initialized.
  // Initialize each rdma channel container
  fbl::AutoLock l(&rdma_lock_);
  for (uint32_t i = 0; i < kNumberOfTables; i++) {
    rdma_channels_[i].phys_offset = rdma_physical_address + (i * kTableSize);
    const cpp20::span<uint8_t> rdma_table_subregion =
        rdma_region.subspan(i * kTableSize, kTableSize);
    rdma_channels_[i].virt_offset = rdma_table_subregion.data();
    rdma_usage_table_[i] = kRdmaTableReady;
  }

  // Allocate RDMA Table for AFBC engine
  status = zx::vmo::create_contiguous(bti_, kAfbcRdmaRegionSize, 0, &afbc_rdma_vmo_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not create afbc RDMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  zx_paddr_t afbc_rdma_physical_address = 0;
  status = bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE, afbc_rdma_vmo_, 0, kAfbcRdmaRegionSize,
                    &afbc_rdma_physical_address, 1, &afbc_rdma_pmt_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not pin afbc RDMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  zx_vaddr_t afbc_rdma_virtual_address = 0;
  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, afbc_rdma_vmo_, 0,
                                      kAfbcRdmaRegionSize, &afbc_rdma_virtual_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map afbc vmar: %s", zx_status_get_string(status));
    return status;
  }

  // Initialize AFBC rdma channel container
  afbc_rdma_channel_.phys_offset = afbc_rdma_physical_address;
  afbc_rdma_channel_.virt_offset = reinterpret_cast<uint8_t*>(afbc_rdma_virtual_address);

  // Setup RDMA_CTRL:
  // Default: no reset, no clock gating, burst size 4x16B for read and write
  // DDR Read/Write request urgent
  RdmaCtrlReg::Get().FromValue(0).set_write_urgent(1).set_read_urgent(1).WriteTo(&vpu_mmio_);

  ResetRdmaTable();

  return InitializeIrqHandler().status_value();
}

zx::result<> RdmaEngine::InitializeIrqHandler() {
  zx_status_t status = rdma_irq_handler_.Begin(rdma_irq_handler_dispatcher_->async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to begin IRQ handler on the dispatcher: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

void RdmaEngine::SetAfbcRdmaTableValue(uint32_t val) const {
  RdmaTable* afbc_rdma_table = reinterpret_cast<RdmaTable*>(afbc_rdma_channel_.virt_offset);
  afbc_rdma_table->val = val;
}

void RdmaEngine::FlushAfbcRdmaTable() const {
  zx_status_t status = zx_cache_flush(afbc_rdma_channel_.virt_offset, sizeof(RdmaTable),
                                      ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not clean cache: %s", zx_status_get_string(status));
    return;
  }
  // Write the start and end address of the table.  End address is the last address that the
  // RDMA engine reads from.
  vpu_mmio_.Write32(static_cast<uint32_t>(afbc_rdma_channel_.phys_offset),
                    VPU_RDMA_AHB_START_ADDR(kAfbcRdmaChannel));
  vpu_mmio_.Write32(static_cast<uint32_t>(afbc_rdma_channel_.phys_offset + kAfbcTableSize - 4),
                    VPU_RDMA_AHB_END_ADDR(kAfbcRdmaChannel));
}

// TODO(https://fxbug.dev/42135501): stop all channels for safer reloads.
void RdmaEngine::StopRdma() {
  // TODO(https://fxbug.dev/322296668): Make StopRdma() idempotent.
  zxlogf(DEBUG, "Stopping RDMA");

  fbl::AutoLock lock(&rdma_lock_);

  // Grab a copy of active DMA channels before clearing it
  const uint32_t aa = RdmaAccessAutoReg::Get().ReadFrom(&vpu_mmio_).reg_value();
  const uint32_t aa3 = RdmaAccessAuto3Reg::Get().ReadFrom(&vpu_mmio_).reg_value();

  // Disable triggering for channels 0-2.
  RdmaAccessAutoReg::Get()
      .ReadFrom(&vpu_mmio_)
      .set_chn1_intr(0)
      .set_chn2_intr(0)
      .set_chn3_intr(0)
      .WriteTo(&vpu_mmio_);
  // Also disable 7, the dedicated AFBC channel.
  RdmaAccessAuto3Reg::Get().FromValue(0).set_chn7_intr(0).WriteTo(&vpu_mmio_);

  // Wait for all active copies to complete
  constexpr size_t kMaxRdmaWaits = 5;
  uint32_t expected = RdmaStatusReg::DoneFromAccessAuto(aa, 0, aa3);
  for (size_t i = 0; i < kMaxRdmaWaits; i++) {
    if (RdmaStatusReg::Get().ReadFrom(&vpu_mmio_).done() == expected) {
      break;
    }
    zx::nanosleep(zx::deadline_after(zx::usec(5)));
  }

  // Clear interrupt status
  RdmaCtrlReg::Get().ReadFrom(&vpu_mmio_).set_clear_done(0xFF).WriteTo(&vpu_mmio_);
  rdma_active_ = false;
  for (auto& i : rdma_usage_table_) {
    i = kRdmaTableReady;
  }
}

void RdmaEngine::ResetConfigStamp(display::ConfigStamp config_stamp) {
  fbl::AutoLock lock(&rdma_lock_);
  latest_applied_config_ = config_stamp;
}

void RdmaEngine::DumpRdmaRegisters() {
  zxlogf(INFO, "Dumping all RDMA related Registers");
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_MAN = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_MAN));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_MAN = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_MAN));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_1 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_1));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_1 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_1));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_2 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_2));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_2 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_2));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_3 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_3));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_3 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_3));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_4 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_4));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_4 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_4));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_5 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_5));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_5 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_5));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_6 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_6));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_6 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_6));
  zxlogf(INFO, "VPU_RDMA_AHB_START_ADDR_7 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_START_ADDR_7));
  zxlogf(INFO, "VPU_RDMA_AHB_END_ADDR_7 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_AHB_END_ADDR_7));
  zxlogf(INFO, "VPU_RDMA_ACCESS_AUTO = 0x%x", vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO));
  zxlogf(INFO, "VPU_RDMA_ACCESS_AUTO2 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO2));
  zxlogf(INFO, "VPU_RDMA_ACCESS_AUTO3 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_ACCESS_AUTO3));
  zxlogf(INFO, "VPU_RDMA_ACCESS_MAN = 0x%x", vpu_mmio_.Read32(VPU_RDMA_ACCESS_MAN));
  zxlogf(INFO, "VPU_RDMA_CTRL = 0x%x", vpu_mmio_.Read32(VPU_RDMA_CTRL));
  zxlogf(INFO, "VPU_RDMA_STATUS = 0x%x", vpu_mmio_.Read32(VPU_RDMA_STATUS));
  zxlogf(INFO, "VPU_RDMA_STATUS2 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_STATUS2));
  zxlogf(INFO, "VPU_RDMA_STATUS3 = 0x%x", vpu_mmio_.Read32(VPU_RDMA_STATUS3));
  zxlogf(INFO, "Scratch Reg High: 0x%x", vpu_mmio_.Read32(VPP_DUMMY_DATA1));
  zxlogf(INFO, "Scratch Reg Low: 0x%x", vpu_mmio_.Read32(VPP_OSD_SC_DUMMY_DATA));
}

void RdmaEngine::DumpRdmaState() {
  zxlogf(INFO, "\n\n============ RDMA STATE DUMP ============");
  zxlogf(INFO, "Dumping all RDMA related States");
  zxlogf(INFO, "rdma is %s", rdma_active_ ? "Active" : "Not Active");

  DumpRdmaRegisters();

  zxlogf(INFO, "RDMA Table Content:");
  for (uint64_t table_entry : rdma_usage_table_) {
    zxlogf(INFO, "[0x%" PRIx64 "]", table_entry);
  }

  zxlogf(INFO, "start_index = %ld, end_index = %ld", start_index_used_, end_index_used_);
  zxlogf(INFO, "latest applied config stamp = 0x%lx", latest_applied_config_.value());
  zxlogf(INFO, "\n\n=========================================");
}

void RdmaEngine::OnTransactionFinished() { rdma_irq_count_.Add(1); }

void RdmaEngine::InterruptHandler(async_dispatcher_t* dispatcher, async::IrqBase* irq,
                                  zx_status_t status, const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    zxlogf(INFO, "RDMA interrupt wait is cancelled.");
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "RDMA interrupt wait failed: %s", zx_status_get_string(status));
    // A failed async interrupt wait doesn't remove the interrupt from the
    // async loop, so we have to manually cancel it.
    irq->Cancel();
    return;
  }

  OnTransactionFinished();

  // For interrupts bound to ports (including those bound to async loops), the
  // interrupt must be re-armed using zx_interrupt_ack() for each incoming
  // interrupt request. This is best done after the interrupt has been fully
  // processed.
  zx::unowned_interrupt(irq->object())->ack();
}

void RdmaEngine::Release() {
  rdma_irq_.destroy();
  rdma_pmt_.unpin();
  rdma_irq_handler_dispatcher_.reset();
}

}  // namespace amlogic_display
