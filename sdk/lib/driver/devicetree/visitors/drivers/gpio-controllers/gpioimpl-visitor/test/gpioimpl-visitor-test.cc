// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../gpioimpl-visitor.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <ddk/metadata/gpio.h>
#include <gtest/gtest.h>

#include "dts/gpio.h"

namespace gpio_impl_dt {

class GpioImplVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<GpioImplVisitor> {
 public:
  GpioImplVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<GpioImplVisitor>(dtb_path,
                                                                    "GpioImplVisitorTest") {}
};

TEST(GpioImplVisitorTest, TestGpiosProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<GpioImplVisitorTester>("/pkg/test-data/gpio.dtb");
  GpioImplVisitorTester* gpio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, gpio_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(gpio_tester->DoPublish().is_ok());

  auto node_count =
      gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("gpio-controller-ffffa000") != std::string::npos) {
      auto metadata = gpio_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(3lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_gpioimpl::ControllerMetadata>(metadata_blob0);
      ASSERT_TRUE(controller_metadata.is_ok());

      // Init metadata.
      std::vector<uint8_t> metadata_blob1 = std::move(*(*metadata)[1].data());
      fit::result init_metadata =
          fidl::Unpersist<fuchsia_hardware_gpioimpl::InitMetadata>(metadata_blob1);
      ASSERT_TRUE(init_metadata.is_ok());
      ASSERT_EQ((*init_metadata).steps().size(), 3u /*from gpio hog*/ + 7u /*pincfg groups*/);

      // GPIO Hog init steps.
      ASSERT_EQ((*init_metadata).steps()[0].index(), static_cast<uint32_t>(HOG_PIN1));
      ASSERT_EQ((*init_metadata).steps()[0].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithOutputValue(0));
      ASSERT_EQ((*init_metadata).steps()[1].index(), static_cast<uint32_t>(HOG_PIN2));
      ASSERT_EQ((*init_metadata).steps()[1].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    static_cast<fuchsia_hardware_gpio::GpioFlags>(HOG_PIN2_FLAG)));
      ASSERT_EQ((*init_metadata).steps()[2].index(), static_cast<uint32_t>(HOG_PIN3));
      ASSERT_EQ((*init_metadata).steps()[2].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    static_cast<fuchsia_hardware_gpio::GpioFlags>(HOG_PIN3_FLAG)));

      // Pin controller config init steps.
      ASSERT_EQ((*init_metadata).steps()[3].index(), static_cast<uint32_t>(GROUP1_PIN1));
      ASSERT_EQ((*init_metadata).steps()[3].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithAltFunction(GROUP1_FUNCTION));

      ASSERT_EQ((*init_metadata).steps()[4].index(), static_cast<uint32_t>(GROUP1_PIN1));
      ASSERT_EQ((*init_metadata).steps()[4].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithDriveStrengthUa(GROUP1_DRIVE_STRENGTH));

      ASSERT_EQ((*init_metadata).steps()[5].index(), static_cast<uint32_t>(GROUP1_PIN2));
      ASSERT_EQ((*init_metadata).steps()[5].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithAltFunction(GROUP1_FUNCTION));

      ASSERT_EQ((*init_metadata).steps()[6].index(), static_cast<uint32_t>(GROUP1_PIN2));
      ASSERT_EQ((*init_metadata).steps()[6].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithDriveStrengthUa(GROUP1_DRIVE_STRENGTH));

      ASSERT_EQ((*init_metadata).steps()[7].index(), static_cast<uint32_t>(GROUP3_PIN1));
      ASSERT_EQ((*init_metadata).steps()[7].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    fuchsia_hardware_gpio::GpioFlags::kNoPull));

      ASSERT_EQ((*init_metadata).steps()[8].index(), static_cast<uint32_t>(GROUP2_PIN1));
      ASSERT_EQ((*init_metadata).steps()[8].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithOutputValue(0));

      ASSERT_EQ((*init_metadata).steps()[9].index(), static_cast<uint32_t>(GROUP2_PIN2));
      ASSERT_EQ((*init_metadata).steps()[9].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithOutputValue(0));

      // Pin metadata.
      std::vector<uint8_t> metadata_blob2 = std::move(*(*metadata)[2].data());
      auto metadata_start = reinterpret_cast<gpio_pin_t*>(metadata_blob2.data());
      std::vector<gpio_pin_t> gpio_pins(
          metadata_start, metadata_start + (metadata_blob2.size() / sizeof(gpio_pin_t)));
      ASSERT_EQ(gpio_pins.size(), 2lu);
      EXPECT_EQ(gpio_pins[0].pin, static_cast<uint32_t>(PIN1));
      EXPECT_EQ(strcmp(gpio_pins[0].name, PIN1_NAME), 0);
      EXPECT_EQ(gpio_pins[1].pin, static_cast<uint32_t>(PIN2));
      EXPECT_EQ(strcmp(gpio_pins[1].name, PIN2_NAME), 0);

      node_tested_count++;
    }

    if (node.name()->find("gpio-controller-ffffb000") != std::string::npos) {
      auto metadata = gpio_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(2lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_gpioimpl::ControllerMetadata>(metadata_blob0);
      ASSERT_TRUE(controller_metadata.is_ok());

      // Init metadata.
      std::vector<uint8_t> metadata_blob1 = std::move(*(*metadata)[1].data());
      fit::result init_metadata =
          fidl::Unpersist<fuchsia_hardware_gpioimpl::InitMetadata>(metadata_blob1);
      ASSERT_TRUE(init_metadata.is_ok());
      ASSERT_EQ((*init_metadata).steps().size(), 1u);

      // Pin controller config init steps.
      ASSERT_EQ((*init_metadata).steps()[0].index(), static_cast<uint32_t>(GROUP4_PIN1));
      ASSERT_EQ((*init_metadata).steps()[0].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    fuchsia_hardware_gpio::GpioFlags::kPullUp));
      node_tested_count++;
    }

    if (node.name()->find("audio") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, gpio_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = gpio_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(4lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd parent is GPIO PIN1.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
            fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION,
                              "fuchsia.gpio.FUNCTION." + std::string(PIN1_NAME))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                    bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(PIN1))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      // 3rd parent is GPIO PIN2.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
            fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_gpio::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION,
                              "fuchsia.gpio.FUNCTION." + std::string(PIN2_NAME))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                    bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(PIN2))}},
          (*mgr_request.parents())[2].bind_rules(), false));

      // 4th parent is GPIO INIT.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)}},
          (*mgr_request.parents())[3].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                    bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)}},
          (*mgr_request.parents())[3].bind_rules(), false));
    }

    if (node.name()->find("video") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, gpio_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = gpio_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd and 3rd parents are GPIO INIT of different gpio controllers.
      for (uint32_t i = 1; i < 3; i++) {
        EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
            {{fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)}},
            (*mgr_request.parents())[i].properties(), false));
        EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
            {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                      bind_fuchsia_gpio::BIND_INIT_STEP_GPIO)}},
            (*mgr_request.parents())[i].bind_rules(), false));
      }
    }
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace gpio_impl_dt
