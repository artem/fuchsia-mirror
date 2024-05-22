// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../performance-domain-visitor.h"

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "dts/performance-domain-test.h"

namespace performance_domain_visitor_dt {

class PerformanceDomainVisitorTester
    : public fdf_devicetree::testing::VisitorTestHelper<PerformanceDomainVisitor> {
 public:
  PerformanceDomainVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<PerformanceDomainVisitor>(
            dtb_path, "PerformanceDomainVisitorTest") {}
};

TEST(PerformanceDomainVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester =
      std::make_unique<PerformanceDomainVisitorTester>("/pkg/test-data/performance-domain.dtb");
  PerformanceDomainVisitorTester* performance_domain_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, performance_domain_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(performance_domain_visitor_tester->DoPublish().is_ok());

  // Test performance domain and  operating points metadata.
  auto cpu_controller = performance_domain_visitor_tester->GetPbusNodes("cpu-controller");
  ASSERT_EQ(cpu_controller.size(), 1u);
  auto metadata = cpu_controller[0].metadata();
  ASSERT_TRUE(metadata);
  ASSERT_EQ(metadata->size(), 2u);
  std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
  auto metadata_start0 = reinterpret_cast<amlogic_cpu::perf_domain_t*>(metadata_blob0.data());
  std::vector<amlogic_cpu::perf_domain_t> performance_domains(
      metadata_start0,
      metadata_start0 + (metadata_blob0.size() / sizeof(amlogic_cpu::perf_domain_t)));
  EXPECT_EQ(performance_domains.size(), 2u);
  EXPECT_EQ(performance_domains[0].id, static_cast<uint32_t>(TEST_DOMAIN_1));
  EXPECT_EQ(strcmp(performance_domains[0].name, "arm-a73"), 0);
  EXPECT_EQ(performance_domains[0].core_count, 4u);
  EXPECT_EQ(performance_domains[0].relative_performance,
            static_cast<uint32_t>(TEST_DOMAIN_1_PERFORMANCE));
  EXPECT_EQ(performance_domains[1].id, static_cast<uint32_t>(TEST_DOMAIN_2));
  EXPECT_EQ(strcmp(performance_domains[1].name, "arm-a53"), 0);
  EXPECT_EQ(performance_domains[1].core_count, 2u);
  EXPECT_EQ(performance_domains[1].relative_performance,
            static_cast<uint32_t>(TEST_DOMAIN_2_PERFORMANCE));

  std::vector<uint8_t> metadata_blob1 = std::move(*(*metadata)[1].data());
  auto metadata_start1 = reinterpret_cast<amlogic_cpu::operating_point_t*>(metadata_blob1.data());
  std::vector<amlogic_cpu::operating_point_t> opp_points(
      metadata_start1,
      metadata_start1 + (metadata_blob1.size() / sizeof(amlogic_cpu::operating_point_t)));
  EXPECT_EQ(opp_points.size(), 8u);

#define TEST_OPP_POINT(DOMAIN, INDEX)                                          \
  {                                                                            \
    EXPECT_EQ(opp_points[(INDEX - 1) + (4 * (DOMAIN - 1))].freq_hz,            \
              static_cast<uint32_t>(TEST_DOMAIN_##DOMAIN##_OPP_##INDEX##_HZ)); \
    EXPECT_EQ(opp_points[(INDEX - 1) + (4 * (DOMAIN - 1))].volt_uv,            \
              static_cast<uint32_t>(TEST_DOMAIN_##DOMAIN##_OPP_##INDEX##_UV)); \
    EXPECT_EQ(opp_points[(INDEX - 1) + (4 * (DOMAIN - 1))].pd_id,              \
              static_cast<uint32_t>(TEST_DOMAIN_##DOMAIN));                    \
  }

  TEST_OPP_POINT(1, 1);
  TEST_OPP_POINT(1, 2);
  TEST_OPP_POINT(1, 3);
  TEST_OPP_POINT(1, 4);
  TEST_OPP_POINT(2, 1);
  TEST_OPP_POINT(2, 2);
  TEST_OPP_POINT(2, 3);
  TEST_OPP_POINT(2, 4);
#undef TEST_OPP_POINT
}

}  // namespace performance_domain_visitor_dt
