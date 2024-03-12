// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../aml-usb-phy-visitor.h"

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <gtest/gtest.h>
#include <soc/aml-common/aml-usb-phy.h>
#include <usb/usb.h>

#include "dts/aml-usb-phy-test.h"

namespace aml_usb_phy_visitor_dt {

class AmlUsbPhyVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<AmlUsbPhyVisitor> {
 public:
  AmlUsbPhyVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<AmlUsbPhyVisitor>(dtb_path,
                                                                     "AmlUsbPhyVisitorTest") {}
};

TEST(AmlUsbPhyVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<AmlUsbPhyVisitorTester>("/pkg/test-data/aml-usb-phy.dtb");
  AmlUsbPhyVisitorTester* aml_usb_phy_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, aml_usb_phy_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(aml_usb_phy_visitor_tester->DoPublish().is_ok());

  auto node_count = aml_usb_phy_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = aml_usb_phy_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("phy-ffe00000") != std::string::npos) {
      node_tested_count++;
      auto metadata = aml_usb_phy_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(3lu, metadata->size());

      // PLL settings metadata
      std::vector<uint8_t> metadata_blob_1 = std::move(*(*metadata)[0].data());
      auto metadata_start_1 = reinterpret_cast<uint32_t*>(metadata_blob_1.data());
      std::vector<uint32_t> pll_settings(
          metadata_start_1, metadata_start_1 + (metadata_blob_1.size() / sizeof(uint32_t)));
      ASSERT_EQ(pll_settings.size(), 8lu);
      EXPECT_EQ(pll_settings[0], static_cast<uint32_t>(PLL_SETTING_1));
      EXPECT_EQ(pll_settings[1], static_cast<uint32_t>(PLL_SETTING_2));
      EXPECT_EQ(pll_settings[2], static_cast<uint32_t>(PLL_SETTING_3));
      EXPECT_EQ(pll_settings[3], static_cast<uint32_t>(PLL_SETTING_4));
      EXPECT_EQ(pll_settings[4], static_cast<uint32_t>(PLL_SETTING_5));
      EXPECT_EQ(pll_settings[5], static_cast<uint32_t>(PLL_SETTING_6));
      EXPECT_EQ(pll_settings[6], static_cast<uint32_t>(PLL_SETTING_7));
      EXPECT_EQ(pll_settings[7], static_cast<uint32_t>(PLL_SETTING_8));

      // PhyType metadata
      std::vector<uint8_t> metadata_blob_2 = std::move(*(*metadata)[1].data());
      auto phy_type = reinterpret_cast<PhyType*>(metadata_blob_2.data());
      EXPECT_EQ(*phy_type, kG12B);

      // Drive mode metadata
      std::vector<uint8_t> metadata_blob_3 = std::move(*(*metadata)[2].data());
      auto metadata_start_3 = reinterpret_cast<UsbPhyMode*>(metadata_blob_3.data());
      std::vector<UsbPhyMode> phy_modes(
          metadata_start_3, metadata_start_3 + (metadata_blob_3.size() / sizeof(UsbPhyMode)));
      ASSERT_EQ(phy_modes.size(), 3lu);
      EXPECT_EQ(phy_modes[0].protocol, UsbProtocol::Usb2_0);
      EXPECT_EQ(phy_modes[0].dr_mode, USB_MODE_HOST);
      EXPECT_EQ(phy_modes[0].is_otg_capable, false);
      EXPECT_EQ(phy_modes[1].protocol, UsbProtocol::Usb2_0);
      EXPECT_EQ(phy_modes[1].dr_mode, USB_MODE_PERIPHERAL);
      EXPECT_EQ(phy_modes[1].is_otg_capable, true);
      EXPECT_EQ(phy_modes[2].protocol, UsbProtocol::Usb3_0);
      EXPECT_EQ(phy_modes[2].dr_mode, USB_MODE_HOST);
      EXPECT_EQ(phy_modes[2].is_otg_capable, false);
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace aml_usb_phy_visitor_dt
