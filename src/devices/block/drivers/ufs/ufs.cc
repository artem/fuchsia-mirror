// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ufs.h"

#include <lib/ddk/binding_driver.h>
#include <lib/fit/defer.h>
#include <lib/trace/event.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <vector>

#include <safemath/safe_conversions.h>

#include "fbl/auto_lock.h"
#include "logical_unit.h"

namespace ufs {

zx::result<> Ufs::NotifyEventCallback(NotifyEvent event, uint64_t data) {
  switch (event) {
    // This should all be done by the bootloader at start up and not reperformed.
    case NotifyEvent::kInit:
    // This is normally done at init, but isn't necessary.
    case NotifyEvent::kReset:
    case NotifyEvent::kPreLinkStartup:
    case NotifyEvent::kPostLinkStartup:
    case NotifyEvent::kDeviceInitDone:
    case NotifyEvent::kSetupTransferRequestList:
      return zx::ok();
    // If these get called we're probably in trouble.
    case NotifyEvent::kPrePowerModeChange:
    case NotifyEvent::kPostPowerModeChange:
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  };
}

zx::result<> Ufs::Notify(NotifyEvent event, uint64_t data) {
  if (!host_controller_callback_) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  return host_controller_callback_(event, data);
}

zx_status_t Ufs::WaitWithTimeout(fit::function<zx_status_t()> wait_for, uint32_t timeout_us,
                                 const fbl::String& timeout_message) {
  uint32_t time_left = timeout_us;
  while (true) {
    if (wait_for()) {
      return ZX_OK;
    }
    if (time_left == 0) {
      zxlogf(ERROR, "%s after %u usecs", timeout_message.begin(), timeout_us);
      return ZX_ERR_TIMED_OUT;
    }
    usleep(1);
    time_left--;
  }
}

void Ufs::ProcessIoSubmissions() {
  while (true) {
    IoCommand* io_cmd;
    {
      fbl::AutoLock lock(&commands_lock_);
      io_cmd = list_remove_head_type(&pending_commands_, IoCommand, node);
    }

    if (io_cmd == nullptr) {
      return;
    }

    zx::pmt pmt;
    std::unique_ptr<ScsiCommandUpiu> upiu;
    std::optional<zx::unowned_vmo> vmo;

    const uint32_t opcode = io_cmd->op.command.opcode;
    switch (opcode) {
      case BLOCK_OPCODE_READ:
      case BLOCK_OPCODE_WRITE: {
        vmo = zx::unowned_vmo(io_cmd->op.rw.vmo);

        // TODO(fxbug.dev/124835): Support the use of READ(16), WRITE(16) CDBs
        if ((io_cmd->op.rw.offset_dev > UINT32_MAX) || (io_cmd->op.rw.length > UINT16_MAX)) {
          zxlogf(ERROR, "Cannot handle block offset(%lu) or length(%u).", io_cmd->op.rw.offset_dev,
                 io_cmd->op.rw.length);
          io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
          return;
        }

        const uint32_t block_offset = safemath::checked_cast<uint32_t>(io_cmd->op.rw.offset_dev);
        const uint16_t block_length = safemath::checked_cast<uint16_t>(io_cmd->op.rw.length);
        const uint32_t block_size = io_cmd->block_size_bytes;

        if (opcode == BLOCK_OPCODE_READ) {
          upiu = std::make_unique<ScsiRead10Upiu>(block_offset, block_length, block_size,
                                                  /*fua=*/false, 0);
        } else {
          upiu = std::make_unique<ScsiWrite10Upiu>(block_offset, block_length, block_size,
                                                   /*fua=*/false, 0);
        }

      } break;
      case BLOCK_OPCODE_TRIM: {
        if (io_cmd->op.trim.length > UINT16_MAX) {
          zxlogf(ERROR, "Cannot handle trim block length(%d).", io_cmd->op.trim.length);
          io_cmd->Complete(ZX_ERR_NOT_SUPPORTED);
          return;
        }
        upiu = std::make_unique<ScsiUnmapUpiu>(static_cast<uint16_t>(io_cmd->op.trim.length));
        break;
      }
      case BLOCK_OPCODE_FLUSH:
        // TODO(fxbug.dev/124835): Use Synchronize Cache (10)
        io_cmd->Complete(ZX_OK);
        // TODO(fxbug.dev/124835): Use break;
        return;
    }

    auto response =
        transfer_request_processor_->SendScsiUpiu(*upiu, io_cmd->lun_id, std::move(vmo), io_cmd);
    if (response.is_error()) {
      if (response.error_value() == ZX_ERR_NO_RESOURCES) {
        fbl::AutoLock lock(&commands_lock_);
        list_add_head(&pending_commands_, &io_cmd->node);
        return;
      }
      zxlogf(ERROR, "Failed to submit SCSI command (command %p): %s", io_cmd,
             response.status_string());
      io_cmd->Complete(response.error_value());
    }
  }
}

void Ufs::ProcessCompletions() { transfer_request_processor_->RequestCompletion(); }

zx::result<> Ufs::Isr() {
  auto interrupt_status = InterruptStatusReg::Get().ReadFrom(&mmio_);

  // TODO(fxbug.dev/124835): implement error handlers
  if (interrupt_status.uic_error()) {
    zxlogf(ERROR, "UFS: UIC error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_uic_error(true).WriteTo(&mmio_);
  }
  if (interrupt_status.device_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Device fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_device_fatal_error_status(true).WriteTo(&mmio_);
  }
  if (interrupt_status.host_controller_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Host controller fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_host_controller_fatal_error_status(true).WriteTo(
        &mmio_);
  }
  if (interrupt_status.system_bus_fatal_error_status()) {
    zxlogf(ERROR, "UFS: System bus fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_system_bus_fatal_error_status(true).WriteTo(&mmio_);
  }
  if (interrupt_status.crypto_engine_fatal_error_status()) {
    zxlogf(ERROR, "UFS: Crypto engine fatal error on ISR");
    InterruptStatusReg::Get().FromValue(0).set_crypto_engine_fatal_error_status(true).WriteTo(
        &mmio_);
  }

  // Handle command completion interrupts.
  if (interrupt_status.utp_transfer_request_completion_status()) {
    InterruptStatusReg::Get().FromValue(0).set_utp_transfer_request_completion_status(true).WriteTo(
        &mmio_);
    sync_completion_signal(&io_signal_);
  }
  if (interrupt_status.utp_task_management_request_completion_status()) {
    // TODO(fxbug.dev/124835): Handle UTMR completion
    zxlogf(ERROR, "UFS: UTMR completion not yet implemented");
    InterruptStatusReg::Get()
        .FromValue(0)
        .set_utp_task_management_request_completion_status(true)
        .WriteTo(&mmio_);
  }
  if (interrupt_status.uic_command_completion_status()) {
    // TODO(fxbug.dev/124835): Handle UIC completion
    zxlogf(ERROR, "UFS: UIC completion not yet implemented");
    InterruptStatusReg::Get().FromValue(0).set_uic_command_completion_status(true).WriteTo(&mmio_);
  }

  return zx::ok();
}

int Ufs::IrqLoop() {
  while (true) {
    if (zx_status_t status = irq_.wait(nullptr); status != ZX_OK) {
      if (status == ZX_ERR_CANCELED) {
        zxlogf(DEBUG, "Interrupt cancelled. Exiting IRQ loop.");
      } else {
        zxlogf(ERROR, "Failed to wait for interrupt: %s", zx_status_get_string(status));
      }
      break;
    }

    if (zx::result<> result = Isr(); result.is_error()) {
      zxlogf(ERROR, "Failed to run interrupt service routine: %s", result.status_string());
    }

    if (irq_mode_ == fuchsia_hardware_pci::InterruptMode::kLegacy) {
      if (zx_status_t status = pci_.AckInterrupt(); status != ZX_OK) {
        zxlogf(ERROR, "Failed to ack interrupt: %s", zx_status_get_string(status));
        break;
      }
    }
  }
  return thrd_success;
}

int Ufs::IoLoop() {
  while (true) {
    if (IsDriverShutdown()) {
      zxlogf(DEBUG, "IO thread exiting.");
      break;
    }

    if (zx_status_t status = sync_completion_wait(&io_signal_, ZX_TIME_INFINITE); status != ZX_OK) {
      zxlogf(ERROR, "Failed to wait for sync completion: %s", zx_status_get_string(status));
      break;
    }
    sync_completion_reset(&io_signal_);

    // TODO(fxbug.dev/124835): Process async completions

    if (!disable_completion_) {
      ProcessCompletions();
    }
    ProcessIoSubmissions();
  }
  return thrd_success;
}

void Ufs::QueueIoCommand(IoCommand* io_cmd) {
  {
    fbl::AutoLock lock(&commands_lock_);
    list_add_tail(&pending_commands_, &io_cmd->node);
  }
  sync_completion_signal(&io_signal_);
}

zx_status_t Ufs::Init() {
  list_initialize(&pending_commands_);

  if (zx::result<> result = InitController(); result.is_error()) {
    zxlogf(ERROR, "Failed to init UFS controller: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = GetControllerDescriptor(); result.is_error()) {
    zxlogf(ERROR, "Failed to get controller descriptor: %s", result.status_string());
    return result.error_value();
  }

  if (zx::result<> result = ScanLogicalUnits(); result.is_error()) {
    zxlogf(ERROR, "Failed to scan logical units: %s", result.status_string());
    return result.error_value();
  }

  uint8_t lun_count = 0;
  for (uint8_t i = 0; i < kMaxLun; ++i) {
    // If |is_present| is true, then the block device has been initialized.
    if (block_devices_[i].is_present) {
      if (zx_status_t status = LogicalUnit::Bind(*this, block_devices_[i], i); status != ZX_OK) {
        zxlogf(ERROR, "Failed to add logical unit %u: %s", i, zx_status_get_string(status));
        return status;
      }
      ++lun_count;
    }
  }

  if (lun_count == 0) {
    zxlogf(ERROR, "Bind Error. There is no available LUN(lun_count = 0).");
    return ZX_ERR_BAD_STATE;
  }
  logical_unit_count_ = lun_count;
  zxlogf(INFO, "Bind Success");

  DumpRegisters();

  return ZX_OK;
}

zx::result<> Ufs::InitController() {
  // Disable all interrupts.
  InterruptEnableReg::Get().FromValue(0).WriteTo(&mmio_);

  if (zx::result<> result = Notify(NotifyEvent::kReset, 0); result.is_error()) {
    return result.take_error();
  }
  // If UFS host controller is already enabled, disable it.
  if (HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable()) {
    DisableHostController();
  }
  if (zx_status_t status = EnableHostController(); status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable host controller %d", status);
    return zx::error(status);
  }

  if (int thrd_status = thrd_create_with_name(
          &irq_thread_, [](void* ctx) { return static_cast<Ufs*>(ctx)->IrqLoop(); }, this,
          "ufs-irq-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IRQ thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  irq_thread_started_ = true;

  // Notify platform UFS that we are going to init the UFS host controller.
  if (zx::result<> result = Notify(NotifyEvent::kInit, 0); result.is_error()) {
    return result.take_error();
  }

  zxlogf(INFO, "Controller version %u.%u found",
         VersionReg::Get().ReadFrom(&mmio_).major_version_number(),
         VersionReg::Get().ReadFrom(&mmio_).minor_version_number());
  zxlogf(DEBUG, "capabilities 0x%x", CapabilityReg::Get().ReadFrom(&mmio_).reg_value());

  uint8_t number_of_task_management_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_task_management_request_slots() + 1);
  zxlogf(DEBUG, "number_of_task_management_request_slots=%d",
         number_of_task_management_request_slots);
  // TODO(fxbug.dev/124835): Create TaskManagementRequestProcessor

  uint8_t number_of_transfer_request_slots = safemath::checked_cast<uint8_t>(
      CapabilityReg::Get().ReadFrom(&mmio_).number_of_utp_transfer_request_slots() + 1);
  zxlogf(DEBUG, "number_of_transfer_request_slots=%d", number_of_transfer_request_slots);

  auto transfer_request_processor = TransferRequestProcessor::Create(
      *this, bti_.borrow(), mmio_,
      safemath::checked_cast<uint8_t>(number_of_transfer_request_slots));
  if (transfer_request_processor.is_error()) {
    zxlogf(ERROR, "Failed to create transfer request processor %s",
           transfer_request_processor.status_string());
    return transfer_request_processor.take_error();
  }
  transfer_request_processor_ = std::move(*transfer_request_processor);

  if (int thrd_status = thrd_create_with_name(
          &io_thread_, [](void* ctx) { return static_cast<Ufs*>(ctx)->IoLoop(); }, this,
          "ufs-io-thread");
      thrd_status) {
    zx_status_t status = thrd_status_to_zx_status(thrd_status);
    zxlogf(ERROR, " Failed to create IO thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  io_thread_started_ = true;

  // TODO(fxbug.dev/124835): We need to check if retry is needed in the real HW and remove it if
  // not.
  // Initialise the device interface. If it fails, retry twice.
  zx::result<> result;
  for (uint32_t retry = 3; retry > 0; --retry) {
    if (result = InitDeviceInterface(); result.is_error()) {
      zxlogf(WARNING, "Device init failed: %s, retrying", result.status_string());
    } else {
      break;
    }
  }
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to initialize device interface: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> Ufs::InitDeviceInterface() {
  // Enable error and UIC/UTP related interrupts.
  InterruptEnableReg::Get()
      .FromValue(0)
      .set_crypto_engine_fatal_error_enable(true)
      .set_system_bus_fatal_error_enable(true)
      .set_host_controller_fatal_error_enable(true)
      .set_utp_error_enable(true)
      .set_device_fatal_error_enable(true)
      .set_uic_command_completion_enable(false)  // The UIC command uses polling mode.
      .set_utp_task_management_request_completion_enable(true)
      .set_uic_link_startup_status_enable(false)  // Ignore link startup interrupt.
      .set_uic_link_lost_status_enable(true)
      .set_uic_hibernate_enter_status_enable(false)  // The hibernate commands use polling mode.
      .set_uic_hibernate_exit_status_enable(false)   // The hibernate commands use polling mode.
      .set_uic_power_mode_status_enable(true)
      .set_uic_test_mode_status_enable(true)
      .set_uic_error_enable(true)
      .set_uic_dme_endpointreset(true)
      .set_utp_transfer_request_completion_enable(true)
      .WriteTo(&mmio_);

  if (!HostControllerStatusReg::Get().ReadFrom(&mmio_).uic_command_ready()) {
    zxlogf(ERROR, "UIC command is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Send Link Startup UIC command to start the link startup procedure.
  DmeLinkStartUpUicCommand link_startup_command(*this);
  if (zx::result<std::optional<uint32_t>> result = link_startup_command.SendCommand();
      result.is_error()) {
    zxlogf(ERROR, "Failed to startup UFS link: %s", result.status_string());
    return result.take_error();
  }

  // TODO(fxbug.dev/124835): Get the max gear level using DME_GET command.

  // The |device_present| bit becomes true if the host controller has successfully received a Link
  // Startup UIC command response and the UFS device has found a physical link to the controller.
  if (!HostControllerStatusReg::Get().ReadFrom(&mmio_).device_present()) {
    zxlogf(ERROR, "UFS device not found");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  zxlogf(INFO, "UFS device found");

  // TODO(fxbug.dev/124835): Init task management request processor

  if (zx::result<> result = transfer_request_processor_->Init(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize transfer request processor %s", result.status_string());
    return result.take_error();
  }

  // TODO(fxbug.dev/124835): Configure interrupt aggregation. (default 0)

  NopOutUpiu nop_upiu;
  auto nop_response = transfer_request_processor_->SendRequestUpiu<NopOutUpiu, NopInUpiu>(nop_upiu);
  if (nop_response.is_error()) {
    zxlogf(ERROR, "Send NopInUpiu failed: %s", nop_response.status_string());
    return nop_response.take_error();
  }

  zx::time device_init_start_time = zx::clock::get_monotonic();
  SetFlagUpiu set_flag_upiu(Flags::fDeviceInit);
  auto query_response =
      transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          set_flag_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to set fDeviceInit flag: %s", query_response.status_string());
    return query_response.take_error();
  }

  zx::time device_init_time_out = device_init_start_time + zx::msec(kDeviceInitTimeoutMs);
  while (true) {
    ReadFlagUpiu read_flag_upiu(Flags::fDeviceInit);
    auto response =
        transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
            read_flag_upiu);
    if (response.is_error()) {
      zxlogf(ERROR, "Failed to read fDeviceInit flag: %s", response.status_string());
      return response.take_error();
    }
    uint8_t flag = response->GetResponse<FlagResponseUpiu>().GetFlag();

    if (!flag)
      break;

    if (zx::clock::get_monotonic() > device_init_time_out) {
      zxlogf(ERROR, "Wait for fDeviceInit timed out (%u ms)", kDeviceInitTimeoutMs);
      return zx::error(ZX_ERR_TIMED_OUT);
    }
    usleep(10000);
  }

  if (zx::result<> result = Notify(NotifyEvent::kDeviceInitDone, 0); result.is_error()) {
    return result.take_error();
  }

  // 26MHz is a default value written in spec.
  // UFS Specification Version 3.1, section 6.4 "Reference Clock".
  WriteAttributeUpiu write_attribute_upiu(Attributes::bRefClkFreq, AttributeReferenceClock::k26MHz);
  query_response =
      transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          write_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to write bRefClkFreq attribute: %s", query_response.status_string());
  }

  // Get connected lanes.
  DmeGetUicCommand dme_get_connected_tx_lanes_command(*this, PA_ConnectedTxDataLanes, 0);
  zx::result<std::optional<uint32_t>> value = dme_get_connected_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  [[maybe_unused]] uint32_t connected_tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_connected_rx_lanes_command(*this, PA_ConnectedRxDataLanes, 0);
  value = dme_get_connected_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  [[maybe_unused]] uint32_t connected_rx_lanes = value.value().value();

  // Update lanes with available TX/RX lanes.
  DmeGetUicCommand dme_get_avail_tx_lanes_command(*this, PA_AvailTxDataLanes, 0);
  value = dme_get_avail_tx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t tx_lanes = value.value().value();

  DmeGetUicCommand dme_get_avail_rx_lanes_command(*this, PA_AvailRxDataLanes, 0);
  value = dme_get_avail_rx_lanes_command.SendCommand();
  if (value.is_error()) {
    return value.take_error();
  }
  uint32_t rx_lanes = value.value().value();
  zxlogf(DEBUG, "tx_lanes_=%d, rx_lanes_=%d", tx_lanes, rx_lanes);

  // Read bBootLunEn to confirm device interface is ok.
  ReadAttributeUpiu read_attribute_upiu(Attributes::bBootLunEn);

  query_response =
      transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
          read_attribute_upiu);
  if (query_response.is_error()) {
    zxlogf(ERROR, "Failed to read bBootLunEn attribute: %s", query_response.status_string());
    return query_response.take_error();
  }
  auto attribute = query_response->GetResponse<AttributeResponseUpiu>().GetAttribute();
  zxlogf(DEBUG, "bBootLunEn 0x%0x", attribute);

  // TODO(fxbug.dev/124835): Set bMaxNumOfRTT (Read-to-transfer)

  return zx::ok();
}

zx::result<> Ufs::GetControllerDescriptor() {
  ReadDescriptorUpiu read_device_desc_upiu(DescriptorType::kDevice);
  auto response = transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
      read_device_desc_upiu);
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to read device descriptor: %s", response.status_string());
    return response.take_error();
  }
  device_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<DeviceDescriptor>();

  // The field definitions for VersionReg and wSpecVersion are the same.
  // wSpecVersion use big-endian byte ordering.
  auto version = VersionReg::Get().FromValue(betoh16(device_descriptor_.wSpecVersion));
  zxlogf(INFO, "UFS device version %u.%u%u", version.major_version_number(),
         version.minor_version_number(), version.version_suffix());

  zxlogf(INFO, "%u enabled LUNs found", device_descriptor_.bNumberLU);

  ReadDescriptorUpiu read_geometry_desc_upiu(DescriptorType::kGeometry);
  response = transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
      read_geometry_desc_upiu);
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to read geometry descriptor: %s", response.status_string());
    return response.take_error();
  }
  geometry_descriptor_ =
      response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<GeometryDescriptor>();

  // The kDeviceDensityUnit is defined in the spec as 512.
  // qTotalRawDeviceCapacity use big-endian byte ordering.
  constexpr uint32_t kDeviceDensityUnit = 512;
  zxlogf(INFO, "UFS device total size is %lu bytes",
         betoh64(geometry_descriptor_.qTotalRawDeviceCapacity) * kDeviceDensityUnit);

  return zx::ok();
}

zx::result<> Ufs::ScanLogicalUnits() {
  uint8_t max_luns = 0;
  if (geometry_descriptor_.bMaxNumberLU == 0) {
    max_luns = 8;
  } else if (geometry_descriptor_.bMaxNumberLU == 1) {
    max_luns = 32;
  } else {
    zxlogf(ERROR, "Invalid Geometry Descriptor bMaxNumberLU value=%d",
           geometry_descriptor_.bMaxNumberLU);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  ZX_ASSERT(max_luns <= kMaxLun);

  // Allocate a response data buffer.
  const uint32_t kPageSize = zx_system_get_page_size();
  zx::vmo data_vmo;
  if (zx_status_t status = zx::vmo::create(kPageSize, 0, &data_vmo); status != ZX_OK) {
    return zx::error(status);
  }

  // Allocate a buffer for SCSI response data.
  fzl::VmoMapper mapper;
  if (zx_status_t status = mapper.Map(data_vmo, 0, kPageSize); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  for (uint8_t i = 0; i < max_luns; ++i) {
    ReadDescriptorUpiu read_unit_desc_upiu(DescriptorType::kUnit, i);
    auto response =
        transfer_request_processor_->SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(
            read_unit_desc_upiu);
    if (response.is_error()) {
      continue;
    }

    auto unit_descriptor =
        response->GetResponse<DescriptorResponseUpiu>().GetDescriptor<UnitDescriptor>();

    if (unit_descriptor.bLUEnable != 1) {
      continue;
    }

    BlockDevice& block_device = block_devices_[i];
    block_device.is_present = true;
    block_device.lun = i;

    block_device.name = std::string("ufs") + std::to_string(i);

    if (unit_descriptor.bLogicalBlockSize >= sizeof(size_t) * 8) {
      zxlogf(ERROR, "Cannot handle the unit descriptor bLogicalBlockSize = %d.",
             unit_descriptor.bLogicalBlockSize);
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    block_device.block_size = 1 << unit_descriptor.bLogicalBlockSize;
    block_device.block_count = betoh64(unit_descriptor.qLogicalBlockCount);

    if (block_device.block_size < 4096 ||
        block_device.block_size <
            static_cast<size_t>(geometry_descriptor_.bMinAddrBlockSize) * 512 ||
        block_device.block_size >
            static_cast<size_t>(geometry_descriptor_.bMaxInBufferSize) * 512 ||
        block_device.block_size >
            static_cast<size_t>(geometry_descriptor_.bMaxOutBufferSize) * 512) {
      zxlogf(ERROR, "Cannot handle logical block size of %zu.", block_device.block_size);
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    ZX_ASSERT_MSG(block_device.block_size == 4096, "Currently, it only supports a 4KB block size.");

    // Checks for block size consistency.
    ScsiReadCapacity10Upiu read_capacity_upiu;
    if (auto response = transfer_request_processor_->SendScsiUpiu(read_capacity_upiu, i,
                                                                  zx::unowned_vmo(data_vmo));
        response.is_error()) {
      zxlogf(ERROR, "Failed to send SCSI READ CAPACITY 10 command: %s", response.status_string());
      return response.take_error();
    }
    auto* read_capacity_data = reinterpret_cast<scsi::ReadCapacity10ParameterData*>(mapper.start());
    const uint32_t block_length_in_bytes = betoh32(read_capacity_data->block_length_in_bytes);
    if (block_device.block_size != block_length_in_bytes) {
      zxlogf(WARNING,
             "The block size(%ld) from the unit descriptor and the block size(%d) from the READ "
             "CAPACITY are different.",
             block_device.block_size, block_length_in_bytes);
    }
    // TODO(fxbug.dev/124835): If the value of |returned_logical_block_address| is UINT32_MAX, READ
    // CAPACITY 16 should be used instead of READ CAPACITY 10.
    const uint32_t returned_logical_block_address =
        betoh32(read_capacity_data->returned_logical_block_address);
    if ((block_device.block_count != returned_logical_block_address + 1) &&
        (returned_logical_block_address != UINT32_MAX)) {
      zxlogf(WARNING,
             "The block count(%ld) from the unit descriptor and the block count(%d) from the READ "
             "CAPACITY are different.",
             block_device.block_count, returned_logical_block_address + 1);
    }
    zxlogf(INFO, "LUN-%d block_size=%zu, block_count=%ld", i, block_device.block_size,
           block_device.block_count);

    // Verify that the Lun is ready. This command expects a unit attention error.
    ScsiTestUnitReadyUpiu unit_ready_upiu;
    if (auto response = transfer_request_processor_->SendScsiUpiu(unit_ready_upiu, i,
                                                                  zx::unowned_vmo(data_vmo));
        response.is_error()) {
      auto* response_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader*>(mapper.start());
      if (response_data->sense_key() == static_cast<uint8_t>(scsi::SenseKey::UNIT_ATTENTION)) {
        zxlogf(DEBUG, "Expected Unit Attention error: %s", response.status_string());
      } else {
        zxlogf(ERROR, "Failed to send SCSI command: %s", response.status_string());
        return response.take_error();
      }
    }

    // Send request sense commands to clear the Unit Attention Condition(UAC) of LUs. UAC is a
    // condition which needs to be serviced before the logical unit can process commands.
    // This command will get sense data, but ignore it for now because our goal is to clear the
    // UAC.
    ScsiRequestSenseUpiu request_sense_upiu;
    if (auto response = transfer_request_processor_->SendScsiUpiu(request_sense_upiu, i,
                                                                  zx::unowned_vmo(data_vmo));
        response.is_error()) {
      zxlogf(ERROR, "Failed to send SCSI command: %s", response.status_string());
      return response.take_error();
    }

    // Verify that the Lun is ready. This command expects a success.
    if (auto response = transfer_request_processor_->SendScsiUpiu(unit_ready_upiu, i,
                                                                  zx::unowned_vmo(data_vmo));
        response.is_error()) {
      zxlogf(ERROR, "Failed to send SCSI command: %s", response.status_string());
      return response.take_error();
    }
  }

  // TODO(fxbug.dev/124835): Send a request sense command to clear the UAC of a well-known LU.
  // TODO(fxbug.dev/124835): We need to implement the processing of a well-known LU.

  return zx::ok();
}

void Ufs::DumpRegisters() {
  CapabilityReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "CapabilityReg::%s", arg); });
  VersionReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "VersionReg::%s", arg); });

  InterruptStatusReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "InterruptStatusReg::%s", arg); });
  InterruptEnableReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "InterruptEnableReg::%s", arg); });

  HostControllerStatusReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "HostControllerStatusReg::%s", arg); });
  HostControllerEnableReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "HostControllerEnableReg::%s", arg); });

  UtrListBaseAddressReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListBaseAddressReg::%s", arg); });
  UtrListBaseAddressUpperReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListBaseAddressUpperReg::%s", arg); });
  UtrListDoorBellReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListDoorBellReg::%s", arg); });
  UtrListClearReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListClearReg::%s", arg); });
  UtrListRunStopReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListRunStopReg::%s", arg); });
  UtrListCompletionNotificationReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtrListCompletionNotificationReg::%s", arg); });

  UtmrListBaseAddressReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListBaseAddressReg::%s", arg); });
  UtmrListBaseAddressUpperReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListBaseAddressUpperReg::%s", arg); });
  UtmrListDoorBellReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListDoorBellReg::%s", arg); });
  UtmrListRunStopReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UtmrListRunStopReg::%s", arg); });

  UicCommandReg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandReg::%s", arg); });
  UicCommandArgument1Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument1Reg::%s", arg); });
  UicCommandArgument2Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument2Reg::%s", arg); });
  UicCommandArgument3Reg::Get().ReadFrom(&mmio_).Print(
      [](const char* arg) { zxlogf(DEBUG, "UicCommandArgument3Reg::%s", arg); });
}

zx_status_t Ufs::EnableHostController() {
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(true).WriteTo(&mmio_);

  auto wait_for = [&]() -> bool {
    return HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for EnableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx_status_t Ufs::DisableHostController() {
  HostControllerEnableReg::Get().FromValue(0).set_host_controller_enable(false).WriteTo(&mmio_);

  auto wait_for = [&]() -> bool {
    return !HostControllerEnableReg::Get().ReadFrom(&mmio_).host_controller_enable();
  };
  fbl::String timeout_message = "Timeout waiting for DisableHostController";
  return WaitWithTimeout(wait_for, kHostControllerTimeoutUs, timeout_message);
}

zx_status_t Ufs::AddDevice() {
  if (zx_status_t status = DdkAdd(ddk::DeviceAddArgs(kDriverName)
                                      .set_flags(DEVICE_ADD_NON_BINDABLE)
                                      .set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to run DdkAdd: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Ufs::Bind(void* ctx, zx_device_t* parent) {
  ddk::Pci pci(parent, "pci");
  if (!pci.is_valid()) {
    zxlogf(ERROR, "Failed to find PCI fragment");
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::optional<fdf::MmioBuffer> mmio;
  if (zx_status_t status = pci.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to map registers: %s", zx_status_get_string(status));
    return status;
  }

  fuchsia_hardware_pci::InterruptMode irq_mode;
  if (zx_status_t status = pci.ConfigureInterruptMode(1, &irq_mode); status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure interrupt: %s", zx_status_get_string(status));
    return status;
  }
  zxlogf(DEBUG, "Interrupt mode: %u", static_cast<uint8_t>(irq_mode));

  zx::interrupt irq;
  if (zx_status_t status = pci.MapInterrupt(0, &irq); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map interrupt: %s", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = pci.SetBusMastering(true); status != ZX_OK) {
    zxlogf(ERROR, "Failed to enable bus mastering: %s", zx_status_get_string(status));
    return status;
  }
  auto cleanup = fit::defer([&] { pci.SetBusMastering(false); });

  zx::bti bti;
  if (zx_status_t status = pci.GetBti(0, &bti); status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  auto driver = fbl::make_unique_checked<Ufs>(&ac, parent, std::move(pci), std::move(*mmio),
                                              irq_mode, std::move(irq), std::move(bti));
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate memory for UFS driver.");
    return ZX_ERR_NO_MEMORY;
  }
  driver->SetHostControllerCallback(NotifyEventCallback);

  if (zx_status_t status = driver->AddDevice(); status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  [[maybe_unused]] auto placeholder = driver.release();
  cleanup.cancel();
  return ZX_OK;
}

void Ufs::DdkInit(ddk::InitTxn txn) {
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
    DumpRegisters();
  }
  txn.Reply(status);
}

void Ufs::DdkRelease() {
  zxlogf(DEBUG, "Releasing driver.");
  driver_shutdown_ = true;
  if (pci_.is_valid()) {
    pci_.SetBusMastering(false);
  }
  irq_.destroy();  // Make irq_.wait() in IrqLoop() return ZX_ERR_CANCELED.
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }
  if (io_thread_started_) {
    sync_completion_signal(&io_signal_);
    thrd_join(io_thread_, nullptr);
  }

  delete this;
}

static zx_driver_ops_t driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = Ufs::Bind,
};

}  // namespace ufs

ZIRCON_DRIVER(Ufs, ufs::driver_ops, "zircon", "0.1");
