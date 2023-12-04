// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/testing/devicetree-test-fixture.h>
#include <lib/stdcompat/array.h>
#include <lib/zbitl/image.h>

namespace {

using boot_shim::testing::LoadDtb;
using boot_shim::testing::LoadedDtb;

class ArmDevicetreeGicItemTest
    : public boot_shim::testing::TestMixin<boot_shim::testing::ArmDevicetreeTest,
                                           boot_shim::testing::SyntheticDevicetreeTest> {
 public:
  using Mixin::SetUpTestSuite;
  using Mixin::TearDownTestSuite;

  auto get_mmio_observer() {
    return [this](const boot_shim::DevicetreeMmioRange& r) { ranges_.push_back(r); };
  }

  cpp20::span<const boot_shim::DevicetreeMmioRange> mmio_ranges() const { return ranges_; }

 private:
  std::vector<boot_shim::DevicetreeMmioRange> ranges_;
};

TEST_F(ArmDevicetreeGicItemTest, ParseQemuGicV2WithMsi) {
  constexpr auto kExpectedMmio = cpp20::to_array<boot_shim::DevicetreeMmioRange>({
      {
          .address = 0x8000000,
          .size = 0x10000,
      },
      {
          .address = 0x8010000,
          .size = 0x10000,
      },
      {
          .address = 0x8030000,
          .size = 0x10000,
      },
      {
          .address = 0x8040000,
          .size = 0x10000,
      },
      {
          .address = 0x8020000,
          .size = 0x1000,
      },
  });

  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_arm_gic2();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);
  shim.set_mmio_observer(get_mmio_observer());

  ASSERT_TRUE(shim.Init());
  boot_shim::testing::CheckMmioRanges(mmio_ranges(), kExpectedMmio);
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->msi_frame_phys, 0x08020000);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicc_offset, 0x10000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_TRUE(dcfg->use_msi);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, GicV2NoMsi) {
  constexpr auto kExpectedMmio = cpp20::to_array<boot_shim::DevicetreeMmioRange>({
      {
          .address = 0x8000000,
          .size = 0x10000,
      },
      {
          .address = 0x8010000,
          .size = 0x10000,
      },
      {
          .address = 0x8030000,
          .size = 0x10000,
      },
      {
          .address = 0x8040000,
          .size = 0x10000,
      },
  });

  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = arm_gic2_no_msi();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);
  shim.set_mmio_observer(get_mmio_observer());

  ASSERT_TRUE(shim.Init());
  boot_shim::testing::CheckMmioRanges(mmio_ranges(), kExpectedMmio);
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->msi_frame_phys, 0);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicc_offset, 0x10000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->use_msi);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

// We dont support GicV3 with MSI yet, not reflected in the driver configuration.
TEST_F(ArmDevicetreeGicItemTest, ParseQemuGicV3) {
  constexpr auto kExpectedMmio = cpp20::to_array<boot_shim::DevicetreeMmioRange>({
      {
          .address = 0x8000000,
          .size = 0x10000,
      },
      {
          .address = 0x80a0000,
          .size = 0xf60000,
      },
  });

  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_arm_gic3();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);
  shim.set_mmio_observer(get_mmio_observer());

  ASSERT_TRUE(shim.Init());
  boot_shim::testing::CheckMmioRanges(mmio_ranges(), kExpectedMmio);
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v3_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v3_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicr_offset, 0xa0000);
      EXPECT_EQ(dcfg->gicr_stride, 0x20000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V3 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, ParseCrosvm) {
  constexpr auto kExpectedMmio = cpp20::to_array<boot_shim::DevicetreeMmioRange>({
      {
          .address = 0x3fff0000,
          .size = 0x10000,
      },
      {
          .address = 0x3ffd0000,
          .size = 0x20000,
      },
  });
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = crosvm_arm();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);
  shim.set_mmio_observer(get_mmio_observer());

  ASSERT_TRUE(shim.Init());
  boot_shim::testing::CheckMmioRanges(mmio_ranges(), kExpectedMmio);
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v3_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v3_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x3ffd0000);
      EXPECT_EQ(dcfg->gicd_offset, 0x20000);
      EXPECT_EQ(dcfg->gicr_offset, 0x00000);
      EXPECT_EQ(dcfg->gicr_stride, 0x20000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V3 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, KhadasVim3) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = khadas_vim3();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  shim.Init();
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0xffc01000);

      EXPECT_EQ(dcfg->gicd_offset, 0);
      EXPECT_EQ(dcfg->gicc_offset, 0x1000);
      EXPECT_EQ(dcfg->gich_offset, 0x3000);
      EXPECT_EQ(dcfg->gicv_offset, 0x5000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, MissingNode) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = empty_fdt();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  // shim completes successfully even when nothing is matching.
  EXPECT_TRUE(shim.Init());
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  for (auto [header, payload] : image) {
    EXPECT_FALSE(header->type == ZBI_TYPE_KERNEL_DRIVER &&
                 (header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3 ||
                  header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2));
  }
  image.ignore_error();
}

}  // namespace
