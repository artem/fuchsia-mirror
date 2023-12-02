// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_

#include <zxtest/zxtest.h>

#include "mock-device/ufs-mock-device.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace ufs {

class UfsTest : public zxtest::Test {
 public:
  void SetUp() override;

  void RunInit();

  void TearDown() override;

  void CheckControllerDescriptor();

  zx_status_t DisableController() { return ufs_->DisableHostController(); }
  zx_status_t EnableController() { return ufs_->EnableHostController(); }

  // Helper functions for accessing private functions.
  zx::result<> FillDescriptorAndSendRequest(uint8_t slot, DataDirection ddir, uint16_t resp_offset,
                                            uint16_t resp_len, uint16_t prdt_offset,
                                            uint16_t prdt_entry_count);

  // Map the data vmo to the address space and assign physical addresses. Currently, it only
  // supports 8KB vmo. So, we get two physical addresses. The return value is the physical address
  // of the pinned memory.
  zx::result<> MapVmo(zx::unowned_vmo &vmo, fzl::VmoMapper &mapper, uint64_t offset_vmo,
                      uint64_t length);

  uint8_t GetSlotStateCount(SlotState slot_state);

 protected:
  std::shared_ptr<zx_device> fake_root_;
  zx_device *device_;
  std::unique_ptr<ufs_mock_device::UfsMockDevice> mock_device_;
  Ufs *ufs_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
