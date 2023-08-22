// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

#include <lib/fake-bti/bti.h>

#include <memory>

#include "fuchsia/hardware/block/driver/cpp/banjo.h"

namespace ufs {

void UfsTest::SetUp() {
  fake_root_ = MockDevice::FakeRootParent();

  // Set up an interrupt.
  zx::interrupt irq;
  ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq));
  zx::interrupt irq_duplicated;
  ASSERT_OK(irq.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_duplicated));

  mock_device_ = std::make_unique<ufs_mock_device::UfsMockDevice>(std::move(irq));
  ASSERT_OK(mock_device_->AddLun(0));

  // Set up the driver.
  auto driver = std::make_unique<Ufs>(fake_root_.get(), ddk::Pci{}, mock_device_->GetMmioBuffer(),
                                      fuchsia_hardware_pci::InterruptMode::kMsiX,
                                      std::move(irq_duplicated), mock_device_->GetFakeBti());
  driver->SetHostControllerCallback(Ufs::NotifyEventCallback);
  ASSERT_OK(driver->AddDevice());
  [[maybe_unused]] auto unused = driver.release();

  device_ = fake_root_->GetLatestChild();
  ufs_ = device_->GetDeviceContext<Ufs>();
}

void UfsTest::RunInit() {
  device_->InitOp();
  ASSERT_OK(device_->WaitUntilInitReplyCalled(zx::time::infinite()));
  ASSERT_OK(device_->InitReplyCallStatus());
}

void UfsTest::TearDown() {
  device_async_remove(device_);
  EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(device_));
}

zx::result<> UfsTest::FillDescriptorAndSendRequest(uint8_t slot,
                                                   TransferRequestDescriptorDataDirection ddir,
                                                   uint16_t resp_offset, uint16_t resp_len,
                                                   uint16_t prdt_offset, uint16_t prdt_entry_count,
                                                   bool sync) {
  return ufs_->GetTransferRequestProcessor().FillDescriptorAndSendRequest(
      slot, ddir, resp_offset, resp_len, prdt_offset, prdt_entry_count, sync);
}

zx::result<std::vector<zx_paddr_t>> UfsTest::MapAndPinVmo(uint32_t option, zx::unowned_vmo &vmo,
                                                          fzl::VmoMapper &mapper, zx::pmt &pmt,
                                                          uint64_t offset, uint64_t length) {
  if (zx_status_t status = mapper.Map(*vmo, offset, length); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  const uint32_t kPageSize = zx_system_get_page_size();
  std::vector<zx_paddr_t> paddrs;
  ZX_DEBUG_ASSERT(length % kPageSize == 0);
  paddrs.resize(length / kPageSize, 0);
  if (zx_status_t status = mock_device_->GetFakeBti().pin(option, *vmo, offset, length,
                                                          paddrs.data(), length / kPageSize, &pmt);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin IO buffer: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(paddrs));
}

}  // namespace ufs
