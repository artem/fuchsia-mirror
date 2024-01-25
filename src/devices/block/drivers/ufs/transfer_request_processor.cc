// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "transfer_request_processor.h"

#include <lib/trace/event.h>

#include <safemath/checked_math.h>
#include <safemath/safe_conversions.h>

#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"

namespace ufs {

namespace {
void FillPrdt(PhysicalRegionDescriptionTableEntry *prdt,
              const std::vector<zx_paddr_t> &buffer_physical_addresses, uint32_t prdt_count,
              uint32_t data_length) {
  for (uint32_t i = 0; i < prdt_count; ++i) {
    // It only supports 4KB data buffers for each entry in the scatter-gather.
    ZX_ASSERT(buffer_physical_addresses[i] != 0);
    uint32_t byte_count = data_length < kPrdtEntryDataLength ? data_length : kPrdtEntryDataLength;
    prdt->set_data_base_address(static_cast<uint32_t>(buffer_physical_addresses[i] & 0xffffffff));
    prdt->set_data_base_address_upper(static_cast<uint32_t>(buffer_physical_addresses[i] >> 32));
    prdt->set_data_byte_count(byte_count - 1);

    ++prdt;
    data_length -= byte_count;
  }
  ZX_DEBUG_ASSERT(data_length == 0);
}
}  // namespace

template <>
std::tuple<uint16_t, uint32_t> TransferRequestProcessor::PreparePrdt<ScsiCommandUpiu>(
    ScsiCommandUpiu &request, const uint8_t lun, const uint8_t slot,
    const std::vector<zx_paddr_t> &buffer_phys, const uint16_t response_offset,
    const uint16_t response_length) {
  const uint32_t data_transfer_length = std::min(request.GetTransferBytes(), kMaxPrdtDataLength);

  request.GetHeader().lun = lun;
  request.SetExpectedDataTransferLength(data_transfer_length);

  // Prepare PRDT(physical region description table).
  const uint32_t prdt_entry_count =
      fbl::round_up(data_transfer_length, kPrdtEntryDataLength) / kPrdtEntryDataLength;
  ZX_DEBUG_ASSERT(prdt_entry_count <= kMaxPrdtEntryCount);

  uint16_t prdt_offset = response_offset + response_length;
  uint32_t prdt_length_in_bytes = prdt_entry_count * sizeof(PhysicalRegionDescriptionTableEntry);
  const size_t total_length = static_cast<size_t>(prdt_offset) + prdt_length_in_bytes;

  ZX_DEBUG_ASSERT_MSG(total_length <= request_list_.GetDescriptorBufferSize(slot),
                      "Invalid UPIU size for prdt");
  auto prdt =
      request_list_.GetDescriptorBuffer<PhysicalRegionDescriptionTableEntry>(slot, prdt_offset);
  memset(prdt, 0, prdt_length_in_bytes);

  FillPrdt(prdt, buffer_phys, prdt_entry_count, data_transfer_length);

  // TODO(https://fxbug.dev/42075643): Enable unmmap and write buffer command. Umap and writebuffer must set
  // the xfer->count value differently.

  return {prdt_offset, prdt_entry_count};
}

zx::result<std::unique_ptr<TransferRequestProcessor>> TransferRequestProcessor::Create(
    Ufs &ufs, zx::unowned_bti bti, fdf::MmioBuffer &mmio, uint8_t entry_count) {
  if (entry_count > kMaxTransferRequestListSize) {
    zxlogf(ERROR, "Request list size exceeded the maximum size of %d.",
           kMaxTransferRequestListSize);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result<RequestList> request_list =
      RequestList::Create(bti->borrow(), sizeof(TransferRequestDescriptor), entry_count);
  if (request_list.is_error()) {
    return request_list.take_error();
  }

  fbl::AllocChecker ac;
  auto request_processor = fbl::make_unique_checked<TransferRequestProcessor>(
      &ac, std::move(request_list.value()), ufs, std::move(bti), mmio, entry_count);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate transfer request processor.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(request_processor));
}

zx::result<> TransferRequestProcessor::Init() {
  zx_paddr_t paddr =
      request_list_.GetRequestDescriptorPhysicalAddress<TransferRequestDescriptor>(0);
  UtrListBaseAddressReg::Get().FromValue(paddr & 0xffffffff).WriteTo(&register_);
  UtrListBaseAddressUpperReg::Get().FromValue(paddr >> 32).WriteTo(&register_);

  slot_mask_ = static_cast<uint32_t>(1UL << request_list_.GetSlotCount()) - 1;

  if (!HostControllerStatusReg::Get().ReadFrom(&register_).utp_transfer_request_list_ready()) {
    zxlogf(ERROR, "UTP transfer request list is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (UtrListDoorBellReg::Get().ReadFrom(&register_).door_bell() != 0) {
    zxlogf(ERROR, "UTP transfer request list door bell is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  if (UtrListCompletionNotificationReg::Get().ReadFrom(&register_).notification() != 0) {
    zxlogf(ERROR, "UTP transfer request list notification is not ready\n");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Start Utp Transfer Request list.
  UtrListRunStopReg::Get().FromValue(0).set_value(true).WriteTo(&register_);

  return zx::ok();
}

zx::result<uint8_t> TransferRequestProcessor::ReserveAdminSlot() {
  RequestSlot &slot = request_list_.GetSlot(kAdminCommandSlotNumber);
  if (slot.state == SlotState::kFree) {
    slot.state = SlotState::kReserved;
    return zx::ok(kAdminCommandSlotNumber);
  }
  zxlogf(DEBUG, "Failed to reserve a admin request slot");
  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx::result<uint8_t> TransferRequestProcessor::ReserveSlot() {
  for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
    if (slot_num == kAdminCommandSlotNumber) {
      continue;
    }
    RequestSlot &slot = request_list_.GetSlot(slot_num);
    if (slot.state == SlotState::kFree) {
      slot.state = SlotState::kReserved;
      return zx::ok(slot_num);
    }
  }
  zxlogf(DEBUG, "Failed to reserve a request slot");
  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx::result<std::unique_ptr<ResponseUpiu>> TransferRequestProcessor::SendScsiUpiu(
    ScsiCommandUpiu &request, uint8_t lun, std::optional<zx::unowned_vmo> data_vmo,
    IoCommand *io_cmd) {
  const bool is_admin = io_cmd == nullptr;
  zx::result<uint8_t> slot = is_admin ? ReserveAdminSlot() : ReserveSlot();
  if (slot.is_error()) {
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  uint32_t offset = 0;
  uint32_t length = 0;
  if (io_cmd != nullptr) {
    offset = safemath::checked_cast<uint32_t>(io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_TRIM
                                                  ? io_cmd->disk_op.op.trim.offset_dev
                                                  : io_cmd->disk_op.op.rw.offset_dev);
    length = io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_TRIM ? io_cmd->disk_op.op.trim.length
                                                                    : io_cmd->disk_op.op.rw.length;
  }
  TRACE_DURATION("ufs", "SendScsiUpiu", "slot", slot.value(), "offset", offset, "length", length);

  zx::result<void *> response;
  // Admin commands should be performed synchronously and non-admin (data) commands should be
  // performed asynchronously.
  if (response = SendRequestUsingSlot<ScsiCommandUpiu>(
          request, lun, slot.value(), std::move(data_vmo), io_cmd, /*is_sync*/ is_admin);
      response.is_error()) {
    return response.take_error();
  }
  auto response_upiu = std::make_unique<ResponseUpiu>(response.value());

  return zx::ok(std::move(response_upiu));
}

template <class RequestType>
zx::result<void *> TransferRequestProcessor::SendRequestUsingSlot(
    RequestType &request, uint8_t lun, uint8_t slot, std::optional<zx::unowned_vmo> data_vmo,
    IoCommand *io_cmd, bool is_sync) {
  if (is_sync) {
    // Since the completion is handled by the I/O thread, submitting a synchronous command from the
    // I/O thread will cause a deadlock.
    ZX_DEBUG_ASSERT(controller_.GetIoThread() != thrd_current());
  }

  RequestSlot &request_slot = request_list_.GetSlot(slot);
  ZX_DEBUG_ASSERT_MSG(request_slot.state == SlotState::kReserved, "Invalid slot state");

  const uint16_t response_offset = request.GetResponseOffset();
  const uint16_t response_length = request.GetResponseLength();

  request_slot.io_cmd = io_cmd;
  request_slot.is_scsi_command = std::is_base_of<ScsiCommandUpiu, RequestType>::value;
  request_slot.is_sync = is_sync;
  request_slot.response_upiu_offset = response_offset;

  uint16_t prdt_offset = 0;
  uint32_t prdt_entry_count = 0;
  std::vector<zx_paddr_t> data_paddrs;

  if (data_vmo.has_value()) {
    // Assign physical addresses(pin) to data vmo. The return value is the physical address of
    // the pinned memory.
    const uint32_t kPageSize = zx_system_get_page_size();
    uint64_t offset, length;
    uint32_t option;

    if (io_cmd) {
      // Non-admin (data) command.
      if (io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_TRIM) {
        offset = 0;
        length = kPageSize;
        option = ZX_BTI_PERM_READ;
      } else {
        offset = io_cmd->disk_op.op.rw.offset_vmo * io_cmd->block_size_bytes;
        length = static_cast<uint64_t>(io_cmd->disk_op.op.rw.length) * io_cmd->block_size_bytes;
        option = (io_cmd->disk_op.op.command.opcode == BLOCK_OPCODE_READ) ? ZX_BTI_PERM_WRITE
                                                                          : ZX_BTI_PERM_READ;
      }
    } else {
      // Admin command.
      offset = 0;
      length = kPageSize;
      option = ZX_BTI_PERM_WRITE;
    }
    ZX_DEBUG_ASSERT(length % kPageSize == 0);

    data_paddrs.resize(length / kPageSize, 0);
    if (zx_status_t status =
            GetBti()->pin(option, *data_vmo.value(), offset, length, data_paddrs.data(),
                          length / kPageSize, &request_slot.pmt);
        status != ZX_OK) {
      zxlogf(ERROR, "Failed to pin IO buffer: %s", zx_status_get_string(status));

      if (zx::result<> result = ClearSlot(request_slot); result.is_error()) {
        return result.take_error();
      }
      return zx::error(status);
    }
  }

  std::tie(prdt_offset, prdt_entry_count) =
      PreparePrdt<RequestType>(request, lun, slot, data_paddrs, response_offset, response_length);

  // Record the slot number to |task_tag| for debugging.
  request.GetHeader().task_tag = slot;

  // Copy request and prepare response.
  const size_t length = static_cast<size_t>(response_offset) + response_length;
  ZX_DEBUG_ASSERT_MSG(length <= request_list_.GetDescriptorBufferSize(slot), "Invalid UPIU size");

  memcpy(request_list_.GetDescriptorBuffer(slot), request.GetData(), response_offset);
  memset(request_list_.GetDescriptorBuffer<uint8_t>(slot) + response_offset, 0, response_length);
  auto response = request_list_.GetDescriptorBuffer(slot, response_offset);

  if (zx::result<> result =
          FillDescriptorAndSendRequest(slot, request.GetDataDirection(), response_offset,
                                       response_length, prdt_offset, prdt_entry_count);
      result.is_error()) {
    zxlogf(ERROR, "Failed to send upiu: %s", result.status_string());

    if (zx::result<> result = ClearSlot(request_slot); result.is_error()) {
      return result.take_error();
    }
    return result.take_error();
  }

  if (is_sync) {
    // Wait for completion.
    TRACE_DURATION("ufs", "SendRequestUsingSlot::sync_completion_wait", "slot", slot);
    zx_status_t status = sync_completion_wait(&request_slot.complete, ZX_MSEC(GetTimeoutMsec()));
    zx_status_t request_result = request_slot.result;
    if (zx::result<> result = ClearSlot(request_slot); result.is_error()) {
      return result.take_error();
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "SendRequestUsingSlot request timed out: %s", zx_status_get_string(status));
      return zx::error(status);
    }
    if (request_result != ZX_OK) {
      zxlogf(ERROR, "Failed to SendRequestUsingSlot: %s", zx_status_get_string(request_result));
      return zx::error(request_result);
    }

    UtrListCompletionNotificationReg::Get().FromValue(0).set_notification(1 << slot).WriteTo(
        &register_);
  }

  return zx::ok(response);
}

template zx::result<void *> TransferRequestProcessor::SendRequestUsingSlot<QueryRequestUpiu>(
    QueryRequestUpiu &request, uint8_t lun, uint8_t slot, std::optional<zx::unowned_vmo> data_vmo,
    IoCommand *io_cmd, bool is_sync);
template zx::result<void *> TransferRequestProcessor::SendRequestUsingSlot<ScsiCommandUpiu>(
    ScsiCommandUpiu &request, uint8_t lun, uint8_t slot, std::optional<zx::unowned_vmo> data_vmo,
    IoCommand *io_cmd, bool is_sync);
template zx::result<void *> TransferRequestProcessor::SendRequestUsingSlot<NopOutUpiu>(
    NopOutUpiu &request, uint8_t lun, uint8_t slot, std::optional<zx::unowned_vmo> data_vmo,
    IoCommand *io_cmd, bool is_sync);

zx::result<> TransferRequestProcessor::RingRequestDoorbell(uint8_t slot_num) {
  TRACE_DURATION("ufs", "RingRequestDoorbell", "slot", slot_num);

  ZX_DEBUG_ASSERT_MSG(UtrListRunStopReg::Get().ReadFrom(&register_).value(),
                      "Transfer request list is not running");

  if (zx::result<> result = controller_.Notify(NotifyEvent::kSetupTransferRequestList, slot_num);
      result.is_error()) {
    return result.take_error();
  }

  RequestSlot &request_slot = request_list_.GetSlot(slot_num);
  sync_completion_t *complete = &request_slot.complete;
  sync_completion_reset(complete);
  ZX_DEBUG_ASSERT(request_slot.state == SlotState::kReserved);
  request_slot.state = SlotState::kScheduled;

  // TODO(https://fxbug.dev/42075643): Set the UtrInterruptAggregationControlReg.

  UtrListDoorBellReg::Get().FromValue(1 << slot_num).WriteTo(&register_);

  return zx::ok();
}

zx::result<> TransferRequestProcessor::ScsiCompletion(uint8_t slot_num, RequestSlot &request_slot,
                                                      TransferRequestDescriptor *descriptor) {
  TRACE_DURATION("ufs", "ScsiCompletion", "slot", slot_num);

  ResponseUpiu response(
      request_list_.GetDescriptorBuffer<ResponseUpiu>(slot_num, sizeof(CommandUpiuData)));

  // TODO(https://fxbug.dev/42075643): Need to check if response.header.trans_code() is a kCommnad.
  return GetResponseStatus(descriptor, response, UpiuTransactionCodes::kCommand);
}

zx::result<> TransferRequestProcessor::ClearSlot(RequestSlot &request_slot) {
  if (request_slot.pmt.is_valid()) {
    if (zx_status_t status = request_slot.pmt.unpin(); status != ZX_OK) {
      zxlogf(ERROR, "Failed to unpin IO buffer: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  request_slot.state = SlotState::kFree;
  request_slot.io_cmd = nullptr;
  request_slot.is_scsi_command = false;
  request_slot.is_sync = false;
  request_slot.result = ZX_OK;

  return zx::ok();
}

uint32_t TransferRequestProcessor::RequestCompletion() {
  uint32_t completion_count = 0;

  // Search for all pending slots and signed the ones already done.
  for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
    RequestSlot &request_slot = request_list_.GetSlot(slot_num);
    if (request_slot.state == SlotState::kScheduled) {
      if (!(UtrListDoorBellReg::Get().ReadFrom(&register_).door_bell() & (1 << slot_num))) {
        zx::result<> result = zx::ok();
        if (request_slot.is_scsi_command) {
          // Check SCSI command response.
          auto descriptor = request_list_.GetRequestDescriptor<TransferRequestDescriptor>(slot_num);
          result = ScsiCompletion(slot_num, request_slot, descriptor);
        } else {
          // Check request command response.
          ResponseUpiu response(request_list_.GetDescriptorBuffer<ResponseUpiu>(
              slot_num, request_slot.response_upiu_offset));
          if (response.GetHeader().response != UpiuHeaderResponse::kTargetSuccess) {
            zxlogf(ERROR, "Request command failure: response=%x", response.GetHeader().response);
            result = zx::error(ZX_ERR_BAD_STATE);
          }
        }
        if (request_slot.io_cmd) {
          request_slot.io_cmd->data_vmo.reset();
          request_slot.io_cmd->disk_op.Complete(result.status_value());
        } else {
          request_slot.result = result.status_value();
        }

        if (request_slot.is_sync) {
          sync_completion_signal(&request_slot.complete);
        } else {
          UtrListCompletionNotificationReg::Get()
              .FromValue(0)
              .set_notification(1 << slot_num)
              .WriteTo(&register_);

          if (result = ClearSlot(request_slot); result.is_error()) {
            zxlogf(ERROR, "Failed to clear slot[%u]: %s", slot_num, result.status_string());
          }
        }
        ++completion_count;
      }
    }
  }
  return completion_count;
}

zx::result<> TransferRequestProcessor::FillDescriptorAndSendRequest(
    uint8_t slot, const DataDirection data_dir, const uint16_t response_offset,
    const uint16_t response_length, const uint16_t prdt_offset, const uint32_t prdt_entry_count) {
  auto descriptor = request_list_.GetRequestDescriptor<TransferRequestDescriptor>(slot);
  zx_paddr_t paddr = request_list_.GetSlot(slot).command_descriptor_io.phys();

  // Fill up UTP Transfer Request Descriptor.
  memset(descriptor, 0, sizeof(TransferRequestDescriptor));
  descriptor->set_interrupt(true);
  descriptor->set_data_direction(data_dir);
  descriptor->set_command_type(kCommandTypeUfsStorage);
  // If the command was successful, overwrite |overall_command_status| field with |kSuccess|.
  descriptor->set_overall_command_status(OverallCommandStatus::kInvalid);
  descriptor->set_utp_command_descriptor_base_address(static_cast<uint32_t>(paddr & 0xffffffff));
  descriptor->set_utp_command_descriptor_base_address_upper(static_cast<uint32_t>(paddr >> 32));

  constexpr uint16_t kDwordSize = 4;
  descriptor->set_response_upiu_offset(response_offset / kDwordSize);
  descriptor->set_response_upiu_length(response_length / kDwordSize);
  descriptor->set_prdt_offset(prdt_offset / kDwordSize);
  descriptor->set_prdt_length(prdt_entry_count);

  if (zx::result<> result = RingRequestDoorbell(slot); result.is_error()) {
    zxlogf(ERROR, "Failed to send cmd %s", result.status_string());
    return result.take_error();
  }
  return zx::ok();
}

zx::result<> TransferRequestProcessor::GetResponseStatus(TransferRequestDescriptor *descriptor,
                                                         AbstractResponseUpiu &response,
                                                         uint8_t transaction_type) {
  uint8_t status = response.GetHeader().status;
  uint8_t header_response = response.GetHeader().response;

  // TODO(https://fxbug.dev/42075643): Needs refactoring.
  if (transaction_type == UpiuTransactionCodes::kCommand &&
      (descriptor->overall_command_status() != OverallCommandStatus::kSuccess ||
       status != static_cast<uint8_t>(scsi::StatusCode::GOOD) ||
       header_response != UpiuHeaderResponse::kTargetSuccess)) {
    zxlogf(ERROR, "SCSI failure: ocs=0x%x, status=0x%x, header_response=0x%x",
           descriptor->overall_command_status(), status, header_response);
    auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(
        static_cast<ResponseUpiu &>(response).GetSenseData());
    zxlogf(ERROR, "SCSI sense data:sense_key=0x%x, asc=0x%x, ascq=0x%x",
           static_cast<uint8_t>(sense_data->sense_key()), sense_data->additional_sense_code,
           sense_data->additional_sense_code_qualifier);
  } else if (transaction_type == UpiuTransactionCodes::kQueryRequest &&
             (descriptor->overall_command_status() != OverallCommandStatus::kSuccess ||
              header_response != UpiuHeaderResponse::kTargetSuccess)) {
    zxlogf(ERROR, "Query failure: ocs=0x%x, status=0x%x, header_response=0x%x",
           descriptor->overall_command_status(), status, header_response);
  } else if (descriptor->overall_command_status() != OverallCommandStatus::kSuccess) {
    zxlogf(ERROR, "Generic failure: ocs=0x%x", descriptor->overall_command_status());
  } else {
    return zx::ok();
  }

  return zx::error(ZX_ERR_BAD_STATE);
}

}  // namespace ufs
