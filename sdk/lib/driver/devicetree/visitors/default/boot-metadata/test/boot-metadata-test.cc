// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/boot-metadata/boot-metadata.h>
#include <lib/driver/devicetree/visitors/default/boot-metadata/test/dts/boot-metadata-test.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace fdf_devicetree {
namespace {

class BootMetadataVisitorTester : public testing::VisitorTestHelper<BootMetadataVisitor> {
 public:
  BootMetadataVisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<BootMetadataVisitor>(dtb_path, "BootMetadataVisitorTest") {}
};

TEST(BootMetadataVisitorTest, TestBootMetadataProperty) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<BootMetadataVisitorTester>("/pkg/test-data/boot-metadata.dtb");
  BootMetadataVisitorTester* boot_metadata_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, boot_metadata_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(boot_metadata_tester->DoPublish().is_ok());

  auto node_count = boot_metadata_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = boot_metadata_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name() == "sample-device") {
      auto boot_metadata = node.boot_metadata();

      // Test boot-metadata properties.
      ASSERT_TRUE(boot_metadata);
      ASSERT_EQ(2lu, boot_metadata->size());
      EXPECT_EQ(*(*boot_metadata)[0].zbi_type(), static_cast<uint64_t>(TEST_ZBI_TYPE1));
      EXPECT_EQ(*(*boot_metadata)[0].zbi_extra(), static_cast<uint64_t>(TEST_ZBI_EXTRA1));
      EXPECT_EQ(*(*boot_metadata)[1].zbi_type(), static_cast<uint64_t>(TEST_ZBI_TYPE2));
      EXPECT_EQ(*(*boot_metadata)[1].zbi_extra(), static_cast<uint64_t>(TEST_ZBI_EXTRA2));

      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace
}  // namespace fdf_devicetree
