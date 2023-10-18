// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree.h>
#include <lib/fit/defer.h>

#include "devicetree-test-fixture.h"

namespace {

using devicetree_test::LoadDtb;
using devicetree_test::LoadedDtb;

class RiscvDevicetreeTimerItemTest
    : public devicetree_test::TestMixin<devicetree_test::RiscvDevicetreeTest> {
 public:
  static void SetUpTestSuite() {
    Mixin::SetUpTestSuite();
    auto loaded_dtb = LoadDtb("cpus_riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    cpus_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    cpus_ = std::nullopt;
    Mixin::TearDownTestSuite();
  }

  devicetree::Devicetree cpus() { return cpus_->fdt(); }

 private:
  static std::optional<LoadedDtb> cpus_;
};

std::optional<LoadedDtb> RiscvDevicetreeTimerItemTest::cpus_ = std::nullopt;

TEST_F(RiscvDevicetreeTimerItemTest, TimerFromCpus) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = cpus();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x989680);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, Qemu) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_riscv();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x989680);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, VisionFive2) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = vision_five_2();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x3D0900);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, SifiveHifiveUnmatched) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = sifive_hifive_unmatched();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0xF4240);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

}  // namespace
