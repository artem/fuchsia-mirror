// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>

#include <algorithm>

#include "ufs-mock-device.h"

namespace ufs {
namespace ufs_mock_device {

namespace {
zx::result<uint8_t *> PrdtMapAndGetVirtualAddress(
    UfsMockDevice &mock_device, const PhysicalRegionDescriptionTableEntry &prdt_upiu) {
  zx_paddr_t data_buffer_paddr =
      (static_cast<zx_paddr_t>(prdt_upiu.data_base_address_upper()) << 32) |
      prdt_upiu.data_base_address();

  uint8_t *data_buffer;
  if (auto result = mock_device.MapDmaPaddr(data_buffer_paddr); result.is_error()) {
    zxlogf(ERROR, "UFS MOCK: scsi command could not get virtual address");
    return result.take_error();
  } else {
    data_buffer = reinterpret_cast<uint8_t *>(*result);
  }
  return zx::ok(data_buffer);
}

zx_status_t CopyBufferToPhysicalRegion(UfsMockDevice &mock_device,
                                       cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius,
                                       const std::vector<uint8_t> &buffer) {
  uint64_t cur_pos = 0;
  for (auto &prdt_upiu : prdt_upius) {
    if (cur_pos >= buffer.size()) {
      break;
    }

    auto data_buffer = PrdtMapAndGetVirtualAddress(mock_device, prdt_upiu);
    if (data_buffer.is_error()) {
      zxlogf(ERROR, "UFS MOCK: Faild to map the data buffer.");
      return data_buffer.status_value();
    }

    uint64_t data_buffer_size = prdt_upiu.data_byte_count() + 1;
    uint64_t transfer_count = std::min(data_buffer_size, buffer.size() - cur_pos);
    std::memcpy(data_buffer.value(), buffer.data() + cur_pos, transfer_count);

    cur_pos += transfer_count;
  }

  if (cur_pos != buffer.size()) {
    zxlogf(ERROR, "UFS MOCK: scsi command buffer size is too small");
    return ZX_ERR_NO_SPACE;
  }

  return ZX_OK;
}

zx_status_t CopyPhysicalRegionToBuffer(
    UfsMockDevice &mock_device, std::vector<uint8_t> &buffer,
    const cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  uint64_t cur_pos = 0;
  for (auto &prdt_upiu : prdt_upius) {
    if (cur_pos >= buffer.size()) {
      break;
    }

    auto data_buffer = PrdtMapAndGetVirtualAddress(mock_device, prdt_upiu);
    if (data_buffer.is_error()) {
      return data_buffer.status_value();
    }

    uint64_t data_buffer_size = prdt_upiu.data_byte_count() + 1;
    uint64_t transfer_count = std::min(data_buffer_size, buffer.size() - cur_pos);
    std::memcpy(buffer.data() + cur_pos, data_buffer.value(), transfer_count);

    cur_pos += transfer_count;
  }

  if (cur_pos != buffer.size()) {
    zxlogf(ERROR, "UFS MOCK: scsi command buffer size is too small");
    return ZX_ERR_NO_SPACE;
  }

  return ZX_OK;
}
}  // namespace

void ScsiCommandProcessor::BuildSenseData(ResponseUpiuData &response_upiu, uint8_t sense_key) {
  response_upiu.header.data_segment_length = htobe16(sizeof(scsi::FixedFormatSenseDataHeader));
  response_upiu.sense_data_len = htobe16(sizeof(scsi::FixedFormatSenseDataHeader));
  auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(response_upiu.sense_data);
  sense_data->set_response_code(0x70);
  sense_data->set_valid(0);
  sense_data->set_sense_key(sense_key);
}

zx_status_t ScsiCommandProcessor::HandleScsiCommand(
    CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  constexpr uint8_t kIllegalRequest = 0x05;
  scsi::Opcode opcode = static_cast<scsi::Opcode>(command_upiu.cdb[0]);
  std::vector<uint8_t> data;
  if (auto it = handlers_.find(opcode); it != handlers_.end()) {
    if (auto result = (it->second)(mock_device_, command_upiu, response_upiu, prdt_upius);
        result.is_error()) {
      BuildSenseData(response_upiu, kIllegalRequest);
      return result.status_value();
    } else {
      data = std::move(*result);
    }
  } else {
    zxlogf(ERROR, "UFS MOCK: scsi command opcode: 0x%hhx is not supported",
           static_cast<uint8_t>(opcode));
    BuildSenseData(response_upiu, kIllegalRequest);
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t status = ZX_OK;
  if (command_upiu.header_flags_r()) {
    status = CopyBufferToPhysicalRegion(mock_device_, prdt_upius, data);
  }
  return status;
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultRequestSenseHandler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  ScsiRequestSenseUpiu request_sense_command(command_upiu);
  if (request_sense_command.desc() != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<uint8_t> data_buffer(request_sense_command.allocation_length());

  auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(data_buffer.data());
  sense_data->set_response_code(0x70);
  sense_data->set_valid(0);
  sense_data->set_sense_key(0);

  ZX_DEBUG_ASSERT(command_upiu.header_flags_r());
  if (auto status = CopyBufferToPhysicalRegion(mock_device, prdt_upius, data_buffer);
      status != ZX_OK) {
    zxlogf(ERROR, "UFS MOCK: scsi command, Failed to CopyBufferToPhysicalRegion");
    return zx::error(status);
  }

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultRead10Handler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  uint8_t lun = command_upiu.header.lun;
  ScsiRead10Upiu read10_command(command_upiu, kMockBlockSize);

  if (betoh32(command_upiu.expected_data_transfer_length) != read10_command.GetTransferBytes()) {
    zxlogf(ERROR,
           "UFS MOCK: scsi READ(10) command, expected_data_transfer_length(%d) and "
           "transfer_length(%d) are different.",
           betoh32(command_upiu.expected_data_transfer_length), read10_command.GetTransferBytes());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<uint8_t> data_buffer(read10_command.GetTransferBytes());

  if (auto status = mock_device.BufferRead(lun, data_buffer.data(),
                                           read10_command.GetTransferBytes() / kMockBlockSize,
                                           read10_command.GetStartLba().value());
      status != ZX_OK) {
    return zx::error(status);
  }

  ZX_DEBUG_ASSERT(command_upiu.header_flags_r());
  if (auto status = CopyBufferToPhysicalRegion(mock_device, prdt_upius, data_buffer);
      status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultWrite10Handler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  uint8_t lun = command_upiu.header.lun;
  ScsiWrite10Upiu write10_command(command_upiu, kMockBlockSize);

  if (betoh32(command_upiu.expected_data_transfer_length) != write10_command.GetTransferBytes()) {
    zxlogf(ERROR,
           "UFS MOCK: scsi WRTIE(10) command, expected_data_transfer_length(%d) and "
           "transfer_length(%d) are different.",
           betoh32(command_upiu.expected_data_transfer_length), write10_command.GetTransferBytes());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<uint8_t> data_buffer(write10_command.GetTransferBytes());

  ZX_DEBUG_ASSERT(command_upiu.header_flags_w());
  if (auto status = CopyPhysicalRegionToBuffer(mock_device, data_buffer, prdt_upius);
      status != ZX_OK) {
    return zx::error(status);
  }

  if (auto status = mock_device.BufferWrite(lun, data_buffer.data(),
                                            write10_command.GetTransferBytes() / kMockBlockSize,
                                            write10_command.GetStartLba().value());
      status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultReadCapacity10Handler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  std::vector<uint8_t> data_buffer(sizeof(scsi::ReadCapacity10ParameterData));
  auto *read_capacity_data =
      reinterpret_cast<scsi::ReadCapacity10ParameterData *>(data_buffer.data());

  // |returned_logical_block_address| is a 0-based value.
  read_capacity_data->returned_logical_block_address =
      htobe32((kMockTotalDeviceCapacity / kMockBlockSize) - 1);
  read_capacity_data->block_length_in_bytes = htobe32(kMockBlockSize);

  ZX_DEBUG_ASSERT(command_upiu.header_flags_r());
  if (auto status = CopyBufferToPhysicalRegion(mock_device, prdt_upius, data_buffer);
      status != ZX_OK) {
    zxlogf(ERROR, "UFS MOCK: scsi command, Failed to CopyBufferToPhysicalRegion");
    return zx::error(status);
  }

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultSynchronizeCache10Handler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  std::vector<uint8_t> data_buffer;

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultTestUnitReadyHandler(
    UfsMockDevice &mock_device, CommandUpiuData &command_upiu, ResponseUpiuData &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  std::vector<uint8_t> data_buffer;

  return zx::ok(std::move(data_buffer));
}

}  // namespace ufs_mock_device
}  // namespace ufs
