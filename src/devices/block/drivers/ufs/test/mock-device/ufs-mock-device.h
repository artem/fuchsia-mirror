// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_

#include <endian.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <bitset>

#include <zxtest/zxtest.h>

#include "fake-dma-handler.h"
#include "query-request-processor.h"
#include "register-mmio-processor.h"
#include "scsi-command-processor.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/lib/mmio/test-helper.h"
#include "transfer-request-processor.h"
#include "uiccmd-processor.h"

namespace ufs {
namespace ufs_mock_device {

constexpr uint64_t kMockBlockSizeShift = 12;
constexpr uint64_t kMockBlockSize = (1 << kMockBlockSizeShift);
constexpr uint64_t kMockTotalDeviceCapacity = (1 << 24);  // 16MB

constexpr uint32_t kMajorVersion = 3;
constexpr uint32_t kMinorVersion = 1;
constexpr uint32_t kVersionSuffix = 2;

constexpr uint32_t kMaxGear = 1;
constexpr uint32_t kConnectedDataLanes = 1;

class FakeRegisters final {
 public:
  FakeRegisters() {
    ASSERT_OK(zx::vmo::create(RegisterMap::kRegisterSize, /*options=*/0, &registers_vmo_));
    ASSERT_OK(registers_vmo_.set_cache_policy(ZX_CACHE_POLICY_UNCACHED));
    ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                                         /*vmar_offset=*/0, registers_vmo_,
                                         /*vmo_offset=*/0, /*len=*/RegisterMap::kRegisterSize,
                                         reinterpret_cast<zx_vaddr_t *>(&registers_)));
  }

  template <typename T>
  T Read(zx_off_t offs) const {
    ZX_ASSERT(offs + sizeof(T) <= RegisterMap::kRegisterSize);
    return *reinterpret_cast<const T *>(registers_ + offs);
  }

  template <typename T>
  void Write(T val, zx_off_t offs) {
    ZX_ASSERT(offs + sizeof(T) <= RegisterMap::kRegisterSize);
    *reinterpret_cast<T *>(registers_ + offs) = val;
  }

  zx::vmo GetRegistersVmo() {
    zx::vmo vmo;
    zx_status_t status = registers_vmo_.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
    ZX_ASSERT(status == ZX_OK);
    return vmo;
  }

 private:
  zx::vmo registers_vmo_;
  uint8_t *registers_ = nullptr;
};

// Simulates a logical unit and its contents.
class UfsLogicalUnit {
 public:
  UfsLogicalUnit();

  zx_status_t Enable(uint8_t lun, uint64_t block_count);

  zx_status_t BufferWrite(const void *buf, size_t block_count, off_t block_offset);
  zx_status_t BufferRead(void *buf, size_t block_count, off_t block_offset);
  UnitDescriptor &GetUnitDesc() { return unit_desc_; }

 private:
  uint64_t block_count_;
  std::vector<uint8_t> buffer_;
  UnitDescriptor unit_desc_;
};

class UfsMockDevice {
 public:
  static constexpr uint32_t kNutrs = 32;
  static constexpr uint32_t kNutmrs = 8;

  explicit UfsMockDevice(zx::interrupt irq);
  UfsMockDevice(const UfsMockDevice &) = delete;
  UfsMockDevice &operator=(const UfsMockDevice &) = delete;
  UfsMockDevice(const UfsMockDevice &&) = delete;
  UfsMockDevice &operator=(const UfsMockDevice &&) = delete;
  ~UfsMockDevice() = default;

  fdf::MmioBuffer GetMmioBuffer() {
    return fdf_testing::CreateMmioBuffer(registers_.GetRegistersVmo(), ZX_CACHE_POLICY_UNCACHED,
                                         &RegisterMmioProcessor::GetMmioOps(), this);
  }

  zx_status_t AddLun(uint8_t lun);

  zx::bti GetFakeBti() { return dma_handler_.DuplicateFakeBti(); }
  zx::result<zx_vaddr_t> MapDmaPaddr(zx_paddr_t paddr) { return dma_handler_.PhysToVirt(paddr); }

  void TriggerInterrupt() { irq_.trigger(0, zx::clock::get_monotonic()); }

  zx_status_t BufferWrite(uint8_t lun, const void *buf, size_t block_count, off_t block_offset);
  zx_status_t BufferRead(uint8_t lun, void *buf, size_t block_count, off_t block_offset);

  FakeRegisters *GetRegisters() { return &registers_; }
  DeviceDescriptor &GetDeviceDesc() { return device_desc_; }
  GeometryDescriptor &GetGeometryDesc() { return geometry_desc_; }
  PowerParametersDescriptor &GetPowerDesc() { return power_desc_; }
  void SetAttribute(Attributes idn, uint32_t value) {
    attributes_[static_cast<size_t>(idn)] = value;
  }
  void SetFlag(Flags idn, bool value) { flags_[static_cast<size_t>(idn)] = value; }
  uint32_t GetAttribute(Attributes idn) const { return attributes_[static_cast<size_t>(idn)]; }
  bool GetFlag(Flags idn) const { return flags_[static_cast<size_t>(idn)]; }

  void SetUnitAttention(bool value) { unit_attention_ = value; }
  bool GetUnitAttention() const { return unit_attention_; }

  UfsLogicalUnit &GetLogicalUnit(uint8_t lun) { return logical_units_[lun]; }
  RegisterMmioProcessor &GetRegisterMmioProcessor() { return register_mmio_processor_; }
  UicCmdProcessor &GetUicCmdProcessor() { return uiccmd_processor_; }
  TransferRequestProcessor &GetTransferRequestProcessor() { return transfer_request_processor_; }
  QueryRequestProcessor &GetQueryRequestProcessor() { return query_request_processor_; }
  ScsiCommandProcessor &GetScsiCommandProcessor() { return scsi_command_processor_; }

 private:
  std::array<UfsLogicalUnit, kMaxLunCount> logical_units_;
  DeviceDescriptor device_desc_;
  GeometryDescriptor geometry_desc_;
  PowerParametersDescriptor power_desc_;
  std::array<uint32_t, static_cast<size_t>(Attributes::kAttributeCount)> attributes_;
  std::array<bool, static_cast<size_t>(Flags::kFlagCount)> flags_;

  zx::interrupt irq_;
  FakeDmaHandler dma_handler_;
  FakeRegisters registers_;
  RegisterMmioProcessor register_mmio_processor_;
  UicCmdProcessor uiccmd_processor_;
  TransferRequestProcessor transfer_request_processor_;
  QueryRequestProcessor query_request_processor_;
  ScsiCommandProcessor scsi_command_processor_;

  bool unit_attention_ = false;
};

}  // namespace ufs_mock_device
}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_UFS_MOCK_DEVICE_H_
