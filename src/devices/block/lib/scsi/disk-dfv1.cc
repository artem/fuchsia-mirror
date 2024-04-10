// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/scsi/disk-dfv1.h>
#include <netinet/in.h>
#include <zircon/process.h>

#include <fbl/alloc_checker.h>

#include "src/devices/block/lib/common/include/common-dfv1.h"

namespace scsi {

zx::result<fbl::RefPtr<Disk>> Disk::Bind(zx_device_t* parent, Controller* controller,
                                         uint8_t target, uint16_t lun, uint32_t max_transfer_bytes,
                                         DiskOptions disk_options) {
  fbl::AllocChecker ac;
  auto disk = fbl::MakeRefCountedChecked<Disk>(&ac, parent, controller, target, lun, disk_options);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  auto status = disk->AddDisk(max_transfer_bytes);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(disk);
}

zx_status_t Disk::AddDisk(uint32_t max_transfer_bytes) {
  zx::result inquiry_data = controller_->Inquiry(target_, lun_);
  if (inquiry_data.is_error()) {
    return inquiry_data.status_value();
  }
  // Check that its a direct access block device first.
  if (inquiry_data.value().peripheral_device_type != 0) {
    zxlogf(ERROR, "Device is not direct access block device!, device type: 0x%x",
           inquiry_data.value().peripheral_device_type);
    return ZX_ERR_IO;
  }

  // Print T10 Vendor ID/Product ID
  auto vendor_id =
      std::string(inquiry_data.value().t10_vendor_id, sizeof(inquiry_data.value().t10_vendor_id));
  auto product_id =
      std::string(inquiry_data.value().product_id, sizeof(inquiry_data.value().product_id));
  // Some vendors don't pad the strings with spaces (0x20). Null-terminate strings to avoid printing
  // illegal characters.
  vendor_id = std::string(vendor_id.c_str());
  product_id = std::string(product_id.c_str());
  zxlogf(INFO, "Target %u LUN %u: Vendor ID = %s, Product ID = %s", target_, lun_,
         vendor_id.c_str(), product_id.c_str());

  removable_ = inquiry_data.value().removable_media();

  // Verify that the Lun is ready. This command expects a unit attention error.
  if (zx_status_t status = controller_->TestUnitReady(target_, lun_); status != ZX_OK) {
    // TestUnitReady returns ZX_ERR_UNAVAILABLE status if a unit attention error occurred.
    if (status != ZX_ERR_UNAVAILABLE) {
      zxlogf(ERROR, "Failed SCSI TEST UNIT READY command: %s", zx_status_get_string(status));
      return status;
    }
    zxlogf(DEBUG, "Expected Unit Attention error: %s", zx_status_get_string(status));

    // Send request sense commands to clear the Unit Attention Condition(UAC) of LUs. UAC is a
    // condition which needs to be serviced before the logical unit can process commands.
    // This command will get sense data, but ignore it for now because our goal is to clear the
    // UAC.
    scsi::FixedFormatSenseDataHeader request_sense_data;
    zx_status_t clear_uac_status =
        controller_->RequestSense(target_, lun_, {&request_sense_data, sizeof(request_sense_data)});
    if (clear_uac_status != ZX_OK) {
      zxlogf(ERROR, "Failed SCSI REQUEST SENSE command: %s",
             zx_status_get_string(clear_uac_status));
      return clear_uac_status;
    }

    // Verify that the Lun is ready. This command expects a success.
    clear_uac_status = controller_->TestUnitReady(target_, lun_);
    if (clear_uac_status != ZX_OK) {
      zxlogf(ERROR, "Failed SCSI TEST UNIT READY command: %s",
             zx_status_get_string(clear_uac_status));
      return clear_uac_status;
    }
  }

  zx::result<std::tuple<bool, bool>> parameter =
      controller_->ModeSenseDpoFuaAndWriteProtectedEnabled(target_, lun_,
                                                           disk_options_.use_mode_sense_6);
  if (parameter.is_error()) {
    zxlogf(WARNING,
           "Failed to get DPO FUA and write protected parameter for target %u, lun %u: %s.",
           target_, lun_, zx_status_get_string(parameter.status_value()));
    return parameter.error_value();
  }
  std::tie(dpo_fua_available_, write_protected_) = parameter.value();

  zx::result write_cache_enabled =
      controller_->ModeSenseWriteCacheEnabled(target_, lun_, disk_options_.use_mode_sense_6);
  if (write_cache_enabled.is_error()) {
    zxlogf(WARNING, "Failed to get write cache status for target %u, lun %u: %s.", target_, lun_,
           zx_status_get_string(write_cache_enabled.status_value()));
    // Assume write cache is enabled so that flush operations are not ignored.
    write_cache_enabled_ = true;
  } else {
    write_cache_enabled_ = write_cache_enabled.value();
  }

  zx_status_t status = controller_->ReadCapacity(target_, lun_, &block_count_, &block_size_bytes_);
  if (status != ZX_OK) {
    return status;
  }
  zxlogf(INFO, "%ld blocks of %d bytes", block_count_, block_size_bytes_);

  zx::result<VPDBlockLimits> block_limits = controller_->InquiryBlockLimits(target_, lun_);
  if (block_limits.is_ok()) {
    // A maximum_transfer_length field set to 0 indicates that the device server does not
    // report a limit on the transfer length.
    if (betoh32(block_limits->maximum_transfer_blocks) == 0) {
      max_transfer_bytes_ = max_transfer_bytes;
    } else {
      max_transfer_bytes_ = std::min(
          max_transfer_bytes, betoh32(block_limits->maximum_transfer_blocks) * block_size_bytes_);
    }
  } else {
    max_transfer_bytes_ = max_transfer_bytes;
  }

  if (max_transfer_bytes_ == fuchsia_hardware_block::wire::kMaxTransferUnbounded) {
    max_transfer_blocks_ = UINT32_MAX;
  } else {
    if (max_transfer_bytes_ % block_size_bytes_ != 0) {
      zxlogf(ERROR, "Max transfer size (%u bytes) is not a multiple of the block size (%u bytes).",
             max_transfer_bytes_, block_size_bytes_);
      return ZX_ERR_BAD_STATE;
    }
    max_transfer_blocks_ = max_transfer_bytes_ / block_size_bytes_;
  }

  // If we only need to use the read(10)/write(10) commands, then limit max_transfer_blocks_ and
  // max_transfer_bytes_.
  if (block_count_ <= UINT32_MAX && !disk_options_.use_read_write_12 &&
      max_transfer_blocks_ > UINT16_MAX) {
    max_transfer_blocks_ = UINT16_MAX;
    max_transfer_bytes_ = max_transfer_blocks_ * block_size_bytes_;
  }

  if (disk_options_.check_unmap_support && block_limits.is_ok()) {
    zx::result unmap_command_supported = controller_->InquirySupportUnmapCommand(target_, lun_);
    if (unmap_command_supported.is_ok()) {
      unmap_command_supported_ =
          unmap_command_supported.value() && betoh32(block_limits->maximum_unmap_lba_count);
    }
  }

  status = DdkAdd(DiskName().c_str());
  if (status == ZX_OK) {
    AddRef();
  }
  return status;
}

void Disk::DdkRelease() {
  if (Release()) {
    delete this;
  }
}

void Disk::BlockImplQuery(block_info_t* info_out, size_t* block_op_size_out) {
  info_out->block_size = block_size_bytes_;
  info_out->block_count = block_count_;
  info_out->max_transfer_size = max_transfer_bytes_;
  info_out->flags = (write_protected_ ? FLAG_READONLY : 0) | (removable_ ? FLAG_REMOVABLE : 0) |
                    (dpo_fua_available_ ? FLAG_FUA_SUPPORT : 0);
  *block_op_size_out = controller_->BlockOpSize();
}

void Disk::BlockImplQueue(block_op_t* op, block_impl_queue_callback completion_cb, void* cookie) {
  DiskOp* disk_op = containerof(op, DiskOp, op);
  disk_op->completion_cb = completion_cb;
  disk_op->cookie = cookie;

  switch (op->command.opcode) {
    case BLOCK_OPCODE_READ:
    case BLOCK_OPCODE_WRITE: {
      if (zx_status_t status = block::CheckIoRange(op->rw, block_count_, max_transfer_blocks_);
          status != ZX_OK) {
        completion_cb(cookie, status, op);
        return;
      }
      const bool is_write = op->command.opcode == BLOCK_OPCODE_WRITE;
      const bool is_fua = op->command.flags & BLOCK_IO_FLAG_FORCE_ACCESS;
      if (!dpo_fua_available_ && is_fua) {
        completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
        return;
      }

      uint8_t cdb_buffer[16] = {};
      uint8_t cdb_length;
      if (block_count_ > UINT32_MAX) {
        auto cdb = reinterpret_cast<Read16CDB*>(cdb_buffer);  // Struct-wise equiv. to Write16CDB.
        cdb_length = 16;
        cdb->opcode = is_write ? Opcode::WRITE_16 : Opcode::READ_16;
        cdb->logical_block_address = htobe64(op->rw.offset_dev);
        cdb->transfer_length = htobe32(op->rw.length);
        cdb->set_force_unit_access(is_fua);
      } else if (disk_options_.use_read_write_12) {
        auto cdb = reinterpret_cast<Read12CDB*>(cdb_buffer);  // Struct-wise equiv. to Write12CDB.
        cdb_length = 12;
        cdb->opcode = is_write ? Opcode::WRITE_12 : Opcode::READ_12;
        cdb->logical_block_address = htobe32(static_cast<uint32_t>(op->rw.offset_dev));
        cdb->transfer_length = htobe32(op->rw.length);
        cdb->set_force_unit_access(is_fua);
      } else {
        auto cdb = reinterpret_cast<Read10CDB*>(cdb_buffer);  // Struct-wise equiv. to Write10CDB.
        cdb_length = 10;
        cdb->opcode = is_write ? Opcode::WRITE_10 : Opcode::READ_10;
        cdb->logical_block_address = htobe32(static_cast<uint32_t>(op->rw.offset_dev));
        cdb->transfer_length = htobe16(static_cast<uint16_t>(op->rw.length));
        cdb->set_force_unit_access(is_fua);
      }
      ZX_ASSERT(cdb_length <= sizeof(cdb_buffer));
      controller_->ExecuteCommandAsync(target_, lun_, {cdb_buffer, cdb_length}, is_write,
                                       block_size_bytes_, disk_op, {nullptr, 0});
      return;
    }
    case BLOCK_OPCODE_FLUSH: {
      if (zx_status_t status = block::CheckFlushValid(op->rw); status != ZX_OK) {
        completion_cb(cookie, status, op);
        return;
      }
      if (!write_cache_enabled_) {
        completion_cb(cookie, ZX_OK, op);
        return;
      }
      SynchronizeCache10CDB cdb = {};
      cdb.opcode = Opcode::SYNCHRONIZE_CACHE_10;
      // Prefer writing to storage medium (instead of nv cache) and return only
      // after completion of operation.
      cdb.reserved_and_immed = 0;
      // Ideally this would flush specific blocks, but several platforms don't
      // support this functionality, so just synchronize the whole disk.
      cdb.logical_block_address = 0;
      cdb.number_of_logical_blocks = 0;
      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)},
                                       /*is_write=*/false, block_size_bytes_, disk_op,
                                       {nullptr, 0});
      return;
    }
    case BLOCK_OPCODE_TRIM: {
      if (!unmap_command_supported_) {
        completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
        return;
      }
      if (zx_status_t status = block::CheckIoRange(op->trim, block_count_, max_transfer_blocks_);
          status != ZX_OK) {
        completion_cb(cookie, status, op);
        return;
      }
      UnmapCDB cdb = {};
      cdb.opcode = Opcode::UNMAP;

      // block_trim can only pass a single block slice.
      constexpr uint32_t block_descriptor_count = 1;
      // The SCSI UNMAP command requires separate data to be sent for the UNMAP parameter list.
      uint8_t data[sizeof(UnmapParameterListHeader) +
                   (sizeof(UnmapBlockDescriptor) * block_descriptor_count)] = {};
      cdb.parameter_list_length = htobe16(sizeof(data));

      UnmapParameterListHeader* patameter_list_header =
          reinterpret_cast<UnmapParameterListHeader*>(data);
      patameter_list_header->data_length =
          htobe16(sizeof(UnmapParameterListHeader) - sizeof(patameter_list_header->data_length) +
                  sizeof(UnmapBlockDescriptor));
      patameter_list_header->block_descriptor_data_length = htobe16(sizeof(UnmapBlockDescriptor));

      UnmapBlockDescriptor* block_descriptor =
          reinterpret_cast<UnmapBlockDescriptor*>(data + sizeof(UnmapParameterListHeader));
      block_descriptor->logical_block_address = htobe64(op->trim.offset_dev);
      block_descriptor->blocks = htobe32(op->trim.length);

      controller_->ExecuteCommandAsync(target_, lun_, {&cdb, sizeof(cdb)}, /*is_write=*/true,
                                       block_size_bytes_, disk_op, {&data, sizeof(data)});
      return;
    }
    default:
      completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, op);
      return;
  }
}

}  // namespace scsi
