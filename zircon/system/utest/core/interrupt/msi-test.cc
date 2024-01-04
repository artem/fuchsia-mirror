// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/interrupt.h>
#include <lib/zx/msi.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/vmar.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>

#include <zxtest/zxtest.h>

#include "fixture.h"
#include "zircon/system/utest/core/vmo/helpers.h"

using MsiTest = RootResourceFixture;

namespace {

class MsiTestVmo {
 public:
  static zx::result<std::unique_ptr<MsiTestVmo>> Create() {
    const size_t vmo_size = zx_system_get_page_size();
    auto result = vmo_test::GetTestPhysVmo(vmo_size);
    if (result.is_error()) {
      return result.take_error();
    }

    zx::vmo vmo = std::move(result.value().vmo);
    zx_status_t status = vmo.set_cache_policy(ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    zx_vaddr_t mapped_addr = 0;
    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, vmo_size,
                                        &mapped_addr);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    std::unique_ptr<MsiTestVmo> res(new MsiTestVmo(std::move(vmo), mapped_addr, vmo_size));
    return zx::ok(std::move(res));
  }

  const zx::vmo& vmo() const { return vmo_; }
  zx_vaddr_t addr() const { return addr_; }

  ~MsiTestVmo() { zx::vmar::root_self()->unmap(addr_, size_); }

 private:
  MsiTestVmo(zx::vmo vmo, zx_vaddr_t addr, size_t size)
      : vmo_(std::move(vmo)), addr_(addr), size_(size) {}
  const zx::vmo vmo_;
  const zx_vaddr_t addr_;
  const size_t size_;
};

// Differentiate the two test categories while still allowing the use of the helper
// functions in this file.
TEST_F(MsiTest, AllocateSyscall) {
  if (!MsiTestsSupported()) {
    return;
  }

  // clang-format off
  constexpr std::pair<zx_status_t, uint32_t> kTests[] = {
    { ZX_ERR_INVALID_ARGS, 0 },
    { ZX_OK, 1 },
    { ZX_OK, 2 },
    { ZX_OK, 4 },
    { ZX_ERR_INVALID_ARGS, 5 },  // platform allocations need to be pow2.
    { ZX_OK, 8 },
    { ZX_OK, 16 },
    { ZX_OK, 32 },
    { ZX_ERR_INVALID_ARGS, 64 }, // 64 exceeds the present platform max of 32.
    { ZX_ERR_INVALID_ARGS, std::numeric_limits<uint32_t>::max() },
  };
  // clang-format on

  for (const auto& test : kTests) {
    zx::msi msi = {};
    EXPECT_EQ(test.first, zx::msi::allocate(*root_resource(), test.second, &msi),
              "irq_cnt = %u failed.", test.second);
  }
}

struct MsiCreateTestCase {
  zx_handle_t msi;
  uint32_t opt;
  uint32_t id;
  zx_handle_t vmo;
  uint32_t off;
  zx_status_t status;
};

// Copied from MsiInterruptDispatcher's MsiCapability, but we can't include that kernel header.
namespace FakeMsi {
// All of these values are sourced from the PCI Local Bus Specification rev 3.0 figure 6-9
// and the header msi_interrupt_dispatcher.h which cannot be included due to being kernel-side.
// The intent is to mock the bare minimum functionality of an MSI capability so that the
// dispatcher behavior can be controlled and observed.
// TODO(https://fxbug.dev/32978): The maximum size for this capability can vary based on PVM and bit
// count, so add tests to validate the 4 possible sizes against the VMO.
struct Capability {
  uint8_t id;
  uint8_t next;
  uint16_t control;
  uint64_t reserved1_;    // For 32 bit this is Address, Data, and a reserved field.
                          // For 64 bit this is Address and Address Upper.
  uint32_t mask_bits_32;  // For 64 bit this is Data and a reserved field.
  uint32_t mask_bits_64;
  uint32_t reserved2_;  // Pending Bits
} __PACKED;
static_assert(offsetof(Capability, mask_bits_32) == 0x0C);
static_assert(offsetof(Capability, mask_bits_64) == 0x10);
static_assert(sizeof(Capability) == 24);

const uint8_t Id = 0x5;
const uint16_t CtrlPvmSupported = (1u << 8);

struct TableEntry {
  uint32_t msg_addr;
  uint32_t msg_upper_addr;
  uint32_t msg_data;
  uint32_t vector_control;
};
uint32_t kVectorControlMasked = (1u << 0);

}  // namespace FakeMsi

TEST_F(MsiTest, CreateSyscallArgs) {
  auto result = MsiTestVmo::Create();
  if (!MsiTestsSupported() || result.is_error()) {
    return;
  }

  zx::msi msi;
  constexpr uint32_t msi_cnt = 8;
  auto msi_test_vmo = std::move(result.value());
  auto& vmo = msi_test_vmo->vmo();
  ASSERT_OK(zx::msi::allocate(*root_resource(), msi_cnt, &msi));
  zx_info_msi_t msi_info;
  ASSERT_OK(msi.get_info(ZX_INFO_MSI, &msi_info, sizeof(msi_info), nullptr, nullptr));
  zx_info_vmo_t vmo_info;
  ASSERT_OK(vmo.get_info(ZX_INFO_VMO, &vmo_info, sizeof(vmo_info), nullptr, nullptr));

  uint32_t vmo_size = static_cast<uint32_t>(vmo_info.size_bytes);
  ASSERT_LE(vmo_size, std::numeric_limits<uint32_t>::max());

  // clang-format off
  const struct MsiCreateTestCase kTests[] = {
      // Bad handle.
      { .msi = 123456,    .opt = 0, .id = 0,       .vmo = vmo.get(), .off = 0, .status = ZX_ERR_BAD_HANDLE },
      // Valid handle but wrong type for MSI.
      { .msi = vmo.get(), .opt = 0, .id = 0,       .vmo = vmo.get(), .off = 0, .status = ZX_ERR_WRONG_TYPE },
      // |vmo| is invalid.
      { .msi = msi.get(), .opt = 0, .id = 0,       .vmo = 123456,    .off = 0, .status = ZX_ERR_BAD_HANDLE },
      // |msi_id| exceeds number of allocated interrupts.
      { .msi = msi.get(), .opt = 0, .id = msi_cnt, .vmo = vmo.get(), .off = 0, .status = ZX_ERR_INVALID_ARGS },
      // |options| must be zero, or ZX_MSI_MODE_MSI_X
      { .msi = msi.get(), .opt = ~ZX_MSI_MODE_MSI_X, .id = 0,       .vmo = vmo.get(), .off = 0, .status = ZX_ERR_INVALID_ARGS },
      // |vmo_offset| is past the end of the VMO.
      { .msi = msi.get(), .opt = 0, .id = 0,       .vmo = vmo.get(), .off = vmo_size, .status = ZX_ERR_INVALID_ARGS},
      // |vmo_offset| doesn't provide enough space for the capability.
      { .msi = msi.get(), .opt = 0, .id = 0,       .vmo = vmo.get(), .off = static_cast<uint32_t>(vmo_size - sizeof(FakeMsi::Capability)), .status = ZX_ERR_INVALID_ARGS },
      // |vmo_offset| is the max size possible.
      { .msi = msi.get(), .opt = 0, .id = 0,       .vmo = vmo.get(), .off = std::numeric_limits<uint32_t>::max(), .status = ZX_ERR_INVALID_ARGS },
  };
  // clang-format on

  for (size_t i = 0; i < std::size(kTests); i++) {
    auto& test = kTests[i];
    zx::interrupt interrupt;
    EXPECT_EQ(test.status,
              zx::msi::create(*zx::unowned_msi(test.msi), test.opt, test.id,
                              *zx::unowned_vmo(test.vmo), test.off, &interrupt),
              "kTests[%zu] failed.", i);
  }
}

TEST_F(MsiTest, Msi) {
  auto result = MsiTestVmo::Create();
  if (!MsiTestsSupported() || result.is_error()) {
    return;
  }

  zx::msi msi;
  constexpr uint32_t msi_cnt = 8;
  ASSERT_OK(zx::msi::allocate(*root_resource(), msi_cnt, &msi));
  zx::interrupt interrupt, interrupt_dup;

  auto msi_test_vmo = std::move(result.value());
  auto& vmo = msi_test_vmo->vmo();
  auto ptr = msi_test_vmo->addr();
  auto cap = reinterpret_cast<volatile FakeMsi::Capability*>(ptr);

  // With no options the syscall should check if the Capability's ID matches MSI's.
  ASSERT_STATUS(
      zx::msi::create(msi, /*options=*/0, /*msi_id=*/0, vmo, /*vmo_offset=*/0, &interrupt),
      ZX_ERR_INVALID_ARGS);
  cap->id = FakeMsi::Id;
  cap->control = FakeMsi::CtrlPvmSupported;
  cap->mask_bits_32 = std::numeric_limits<uint32_t>::max();
  ASSERT_OK(zx::msi::create(msi, /*options=*/0, /*msi_id=*/0, vmo, /*vmo_offset=*/0, &interrupt));

  zx_info_msi_t msi_info;
  ASSERT_OK(msi.get_info(ZX_INFO_MSI, &msi_info, sizeof(msi_info), nullptr, nullptr));
  ASSERT_EQ(msi_info.interrupt_count, 1);
  ASSERT_EQ(cap->mask_bits_32 & 0x1, 0);
  ASSERT_STATUS(
      zx::msi::create(msi, /*options=*/0, /*msi_id=*/0, vmo, /*vmo_offset=*/0, &interrupt_dup),
      ZX_ERR_ALREADY_BOUND);
  ASSERT_OK(
      zx::msi::create(msi, /*options=*/0, /*msi_id=*/1, vmo, /*vmo_offset=*/0, &interrupt_dup));
  ASSERT_OK(msi.get_info(ZX_INFO_MSI, &msi_info, sizeof(msi_info), nullptr, nullptr));
  ASSERT_EQ(msi_info.interrupt_count, 2);
  interrupt.reset();
  interrupt_dup.reset();
  ASSERT_OK(msi.get_info(ZX_INFO_MSI, &msi_info, sizeof(msi_info), nullptr, nullptr));
  ASSERT_EQ(msi_info.interrupt_count, 0);
}

constexpr uint32_t SizeNeededForMsi(size_t vmo_size, uint32_t msi_id) {
  return static_cast<uint32_t>(vmo_size - (sizeof(FakeMsi::TableEntry) * (msi_id + 1)));
}

TEST_F(MsiTest, Msix) {
  auto result = MsiTestVmo::Create();
  if (!MsiTestsSupported() || result.is_error()) {
    return;
  }

  const uint32_t msi_cnt = 8;
  zx::msi msi = {};
  ASSERT_OK(zx::msi::allocate(*root_resource(), msi_cnt, &msi));

  auto msi_test_vmo = std::move(result.value());
  auto& vmo = msi_test_vmo->vmo();
  auto ptr = msi_test_vmo->addr();
  zx_info_vmo_t vmo_info = {};
  zx_info_msi_t msi_info = {};
  ASSERT_OK(vmo.get_info(ZX_INFO_VMO, &vmo_info, sizeof(vmo_info), nullptr, nullptr));
  ASSERT_OK(msi.get_info(ZX_INFO_MSI, &msi_info, sizeof(msi_info), nullptr, nullptr));
  uint32_t vmo_size = static_cast<uint32_t>(vmo_info.size_bytes);
  ASSERT_LE(vmo_size, std::numeric_limits<uint32_t>::max());
  [[maybe_unused]] auto msix_table = reinterpret_cast<volatile FakeMsi::TableEntry*>(ptr);

  // clang-format off
  const struct MsiCreateTestCase kTests[] = {
      { .msi = msi.get(), .opt = ZX_MSI_MODE_MSI_X, .id = 0, .vmo = vmo.get(), .off = SizeNeededForMsi(vmo_size, 1), .status = ZX_OK },
      { .msi = msi.get(), .opt = ZX_MSI_MODE_MSI_X, .id = 1, .vmo = vmo.get(), .off = SizeNeededForMsi(vmo_size, 1), .status = ZX_OK },
      { .msi = msi.get(), .opt = ZX_MSI_MODE_MSI_X, .id = 2, .vmo = vmo.get(), .off = SizeNeededForMsi(vmo_size, 1), .status = ZX_ERR_INVALID_ARGS },
      { .msi = msi.get(), .opt = ZX_MSI_MODE_MSI_X, .id = 2, .vmo = vmo.get(), .off = SizeNeededForMsi(vmo_size, 2), .status = ZX_OK },
  };
  // clang-format on
  for (size_t i = 0; i < std::size(kTests); i++) {
    auto& test = kTests[i];
    zx::interrupt interrupt;
    EXPECT_EQ(test.status,
              zx::msi::create(*zx::unowned_msi(test.msi), test.opt, test.id,
                              *zx::unowned_vmo(test.vmo), test.off, &interrupt),
              "kTests[%zu] failed.", i);
  }

  // Verify the table entry is correctly configured in the right location by the new dispatcher.
  for (uint32_t id = 0; id < msi_info.num_irq; id++) {
    {
      zx::interrupt interrupt;
      msix_table[id].vector_control = FakeMsi::kVectorControlMasked;
      ASSERT_OK(zx::msi::create(msi, ZX_MSI_MODE_MSI_X, id, vmo, 0, &interrupt));
      EXPECT_EQ(static_cast<uint32_t>(msi_info.target_addr), msix_table[id].msg_addr);
      EXPECT_EQ(static_cast<uint32_t>(msi_info.target_addr >> 32), msix_table[id].msg_upper_addr);
      EXPECT_EQ(msi_info.target_data + id, msix_table[id].msg_data);
      EXPECT_EQ(0, msix_table[id].vector_control & FakeMsi::kVectorControlMasked);
    }

    // When freed an MsixDispatcherImpl should clear out the table and mask the interrupt.
    EXPECT_EQ(0u, msix_table[id].msg_addr);
    EXPECT_EQ(0u, msix_table[id].msg_upper_addr);
    EXPECT_EQ(0u, msix_table[id].msg_data);
    EXPECT_EQ(1u, msix_table[id].vector_control & FakeMsi::kVectorControlMasked);
  }
}

TEST_F(MsiTest, ContiguousNotSupported) {
  if (!MsiTestsSupported()) {
    return;
  }

  zx::vmo vmo;
  const size_t msi_cnt = 1;
  ASSERT_OK(zx::vmo::create_contiguous(*bti(), zx_system_get_page_size(), 0, &vmo));
  zx::msi msi;
  ASSERT_OK(zx::msi::allocate(*root_resource(), msi_cnt, &msi));

  zx::interrupt interrupt;
  ASSERT_NE(ZX_OK, zx::msi::create(msi, 0, 0, vmo, 0, &interrupt));
}

}  // namespace
