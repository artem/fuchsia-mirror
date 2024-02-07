// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/riscv64/feature.h>

#include <string_view>

#include <gtest/gtest.h>

namespace {

TEST(RiscvFeatureTests, GetAndSet) {
  arch::RiscvFeatures features;
  EXPECT_FALSE(features[arch::RiscvFeature::kVector]);
  features.Set(arch::RiscvFeature::kVector);
  EXPECT_TRUE(features[arch::RiscvFeature::kVector]);
  features.Set(arch::RiscvFeature::kVector, false);
  EXPECT_FALSE(features[arch::RiscvFeature::kVector]);
}

TEST(RiscvFeatureTests, And) {
  arch::RiscvFeatures a, b;

  a.Set(arch::RiscvFeature::kVector, false);
  b.Set(arch::RiscvFeature::kVector, false);

  a.Set(arch::RiscvFeature::kSvpbmt, false);
  b.Set(arch::RiscvFeature::kSvpbmt, true);

  a.Set(arch::RiscvFeature::kZicbom, true);
  b.Set(arch::RiscvFeature::kZicbom, false);

  a.Set(arch::RiscvFeature::kZicboz, true);
  b.Set(arch::RiscvFeature::kZicboz, true);

  a &= b;
  EXPECT_FALSE(a[arch::RiscvFeature::kVector]);  // 0 & 0
  EXPECT_FALSE(a[arch::RiscvFeature::kSvpbmt]);  // 0 & 1
  EXPECT_FALSE(a[arch::RiscvFeature::kZicbom]);  // 1 & 0
  EXPECT_TRUE(a[arch::RiscvFeature::kZicboz]);   // 1 & 1
}

TEST(RiscvFeatureTests, SetMany) {
  constexpr std::string_view kIsaString1 =
      "rv64imafdch_zicsr_zifencei_zihintpause_zba_zbb_zbc_zbs_sstc";
  constexpr std::string_view kIsaString2 =
      "rv64imafdcvh_zicbom_zicboz_zicsr_zifencei_zihintpause_zawrs_zba_zbb_zbc_zbs_zve32f_zve64f_zve64d_sstc_svadu_svpbmt";

  arch::RiscvFeatures features;

  features.SetMany(kIsaString1);
  EXPECT_FALSE(features[arch::RiscvFeature::kVector]);
  EXPECT_FALSE(features[arch::RiscvFeature::kSvpbmt]);
  EXPECT_FALSE(features[arch::RiscvFeature::kZicbom]);
  EXPECT_FALSE(features[arch::RiscvFeature::kZicboz]);

  features.SetMany(kIsaString2);
  EXPECT_TRUE(features[arch::RiscvFeature::kVector]);
  EXPECT_TRUE(features[arch::RiscvFeature::kSvpbmt]);
  EXPECT_TRUE(features[arch::RiscvFeature::kZicbom]);
  EXPECT_TRUE(features[arch::RiscvFeature::kZicboz]);
}

}  // namespace
