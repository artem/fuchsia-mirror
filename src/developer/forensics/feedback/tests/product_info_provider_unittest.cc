// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/product_info_provider.h"

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "fuchsia/intl/cpp/fidl.h"
#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/feedback/annotations/types.h"

namespace forensics::feedback {
namespace {

using ::testing::Pair;
using ::testing::UnorderedElementsAreArray;

TEST(ProductInfoToAnnotationsTest, Convert) {
  ProductInfoToAnnotations convert;

  fuchsia::hwinfo::ProductInfo info;
  EXPECT_THAT(convert(info), UnorderedElementsAreArray({
                                 Pair(kHardwareProductSKUKey, Error::kMissingValue),
                                 Pair(kHardwareProductLanguageKey, Error::kMissingValue),
                                 Pair(kHardwareProductRegulatoryDomainKey, Error::kMissingValue),
                                 Pair(kHardwareProductLocaleListKey, Error::kMissingValue),
                                 Pair(kHardwareProductNameKey, Error::kMissingValue),
                                 Pair(kHardwareProductModelKey, Error::kMissingValue),
                                 Pair(kHardwareProductManufacturerKey, Error::kMissingValue),
                             }));

  info.set_sku("sku");
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductNameKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductModelKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  info.set_language("language");
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductNameKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductModelKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  fuchsia::intl::RegulatoryDomain regulatory_domain;
  info.set_regulatory_domain(std::move(regulatory_domain.set_country_code("country")));
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString("country")),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductNameKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductModelKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  info.set_locale_list({
      fuchsia::intl::LocaleId{.id = "locale1"},
      fuchsia::intl::LocaleId{.id = "locale2"},
      fuchsia::intl::LocaleId{.id = "locale3"},
  });
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString("country")),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString("locale1, locale2, locale3")),
                  Pair(kHardwareProductNameKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductModelKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  info.set_name("name");
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString("country")),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString("locale1, locale2, locale3")),
                  Pair(kHardwareProductNameKey, ErrorOrString("name")),
                  Pair(kHardwareProductModelKey, ErrorOrString(Error::kMissingValue)),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  info.set_model("model");
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString("country")),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString("locale1, locale2, locale3")),
                  Pair(kHardwareProductNameKey, ErrorOrString("name")),
                  Pair(kHardwareProductModelKey, ErrorOrString("model")),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString(Error::kMissingValue)),
              }));

  info.set_manufacturer("manufacturer");
  EXPECT_THAT(convert(info),
              UnorderedElementsAreArray({
                  Pair(kHardwareProductSKUKey, ErrorOrString("sku")),
                  Pair(kHardwareProductLanguageKey, ErrorOrString("language")),
                  Pair(kHardwareProductRegulatoryDomainKey, ErrorOrString("country")),
                  Pair(kHardwareProductLocaleListKey, ErrorOrString("locale1, locale2, locale3")),
                  Pair(kHardwareProductNameKey, ErrorOrString("name")),
                  Pair(kHardwareProductModelKey, ErrorOrString("model")),
                  Pair(kHardwareProductManufacturerKey, ErrorOrString("manufacturer")),
              }));
}

TEST(ProductInforProvider, Keys) {
  // Safe to pass nullptrs b/c objects are never used.
  ProductInfoProvider provider(nullptr, nullptr, nullptr);

  EXPECT_THAT(provider.GetKeys(), UnorderedElementsAreArray({
                                      kHardwareProductSKUKey,
                                      kHardwareProductLanguageKey,
                                      kHardwareProductRegulatoryDomainKey,
                                      kHardwareProductLocaleListKey,
                                      kHardwareProductNameKey,
                                      kHardwareProductModelKey,
                                      kHardwareProductManufacturerKey,
                                  }));
}

}  // namespace
}  // namespace forensics::feedback
