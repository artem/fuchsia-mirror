// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/types.h"

#include <string>

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/sm/smp.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/inspect_util.h"

namespace bt::sm {
namespace {

using namespace inspect::testing;
using bt::testing::ReadInspect;

TEST(TypesTest, LinkKeyTypeToSecurityProperties) {
  SecurityProperties props(hci_spec::LinkKeyType::kCombination);
  EXPECT_EQ(SecurityLevel::kNoSecurity, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kLocalUnit);
  EXPECT_EQ(SecurityLevel::kNoSecurity, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kRemoteUnit);
  EXPECT_EQ(SecurityLevel::kNoSecurity, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kDebugCombination);
  EXPECT_EQ(SecurityLevel::kEncrypted, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kUnauthenticatedCombination192);
  EXPECT_EQ(SecurityLevel::kEncrypted, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kAuthenticatedCombination192);
  EXPECT_EQ(SecurityLevel::kAuthenticated, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(true, props.authenticated());
  EXPECT_EQ(false, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kUnauthenticatedCombination256);
  EXPECT_EQ(SecurityLevel::kEncrypted, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(false, props.authenticated());
  EXPECT_EQ(true, props.secure_connections());

  props = SecurityProperties(hci_spec::LinkKeyType::kAuthenticatedCombination256);
  EXPECT_EQ(SecurityLevel::kSecureAuthenticated, props.level());
  EXPECT_EQ(16UL, props.enc_key_size());
  EXPECT_EQ(true, props.authenticated());
  EXPECT_EQ(true, props.secure_connections());
}

TEST(TypesTest, SecurityPropertiesToLinkKeyType) {
  SecurityProperties props(hci_spec::LinkKeyType::kCombination);
  EXPECT_EQ(std::nullopt, props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kLocalUnit);
  EXPECT_EQ(std::nullopt, props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kRemoteUnit);
  EXPECT_EQ(std::nullopt, props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kDebugCombination);
  ASSERT_TRUE(props.GetLinkKeyType().has_value());
  EXPECT_EQ(hci_spec::LinkKeyType::kUnauthenticatedCombination192, *props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kUnauthenticatedCombination192);
  ASSERT_TRUE(props.GetLinkKeyType().has_value());
  EXPECT_EQ(hci_spec::LinkKeyType::kUnauthenticatedCombination192, *props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kAuthenticatedCombination192);
  ASSERT_TRUE(props.GetLinkKeyType().has_value());
  EXPECT_EQ(hci_spec::LinkKeyType::kAuthenticatedCombination192, *props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kUnauthenticatedCombination256);
  ASSERT_TRUE(props.GetLinkKeyType().has_value());
  EXPECT_EQ(hci_spec::LinkKeyType::kUnauthenticatedCombination256, *props.GetLinkKeyType());

  props = SecurityProperties(hci_spec::LinkKeyType::kAuthenticatedCombination256);
  ASSERT_TRUE(props.GetLinkKeyType().has_value());
  EXPECT_EQ(hci_spec::LinkKeyType::kAuthenticatedCombination256, *props.GetLinkKeyType());
}

TEST(TypesTest, CorrectPropertiesToLevelMapping) {
  for (auto sc : {true, false}) {
    SCOPED_TRACE("secure connections: " + std::to_string(sc));
    for (auto key_sz : {kMinEncryptionKeySize, kMaxEncryptionKeySize}) {
      SCOPED_TRACE("encryption key size: " + std::to_string(key_sz));
      ASSERT_EQ(SecurityLevel::kEncrypted, SecurityProperties(true, false, sc, key_sz).level());

      for (auto auth : {true, false}) {
        SCOPED_TRACE("authenticated: " + std::to_string(auth));
        ASSERT_EQ(SecurityLevel::kNoSecurity, SecurityProperties(false, auth, sc, key_sz).level());
      }
    }
  }
  ASSERT_EQ(SecurityLevel::kAuthenticated,
            SecurityProperties(true, true, false, kMaxEncryptionKeySize).level());
  ASSERT_EQ(SecurityLevel::kAuthenticated,
            SecurityProperties(true, true, true, kMinEncryptionKeySize).level());
  ASSERT_EQ(SecurityLevel::kSecureAuthenticated,
            SecurityProperties(true, true, true, kMaxEncryptionKeySize).level());
}

TEST(TypesTest, PropertiesLevelConstructorWorks) {
  for (auto enc_key_size : {kMinEncryptionKeySize, kMaxEncryptionKeySize}) {
    SCOPED_TRACE("Enc key size: " + std::to_string(enc_key_size));
    for (auto sc : {true, false}) {
      SCOPED_TRACE("Secure Connections: " + std::to_string(sc));
      ASSERT_EQ(SecurityLevel::kNoSecurity,
                SecurityProperties(SecurityLevel::kNoSecurity, enc_key_size, sc).level());
      ASSERT_EQ(SecurityLevel::kEncrypted,
                SecurityProperties(SecurityLevel::kEncrypted, enc_key_size, sc).level());
      if (sc && enc_key_size == kMaxEncryptionKeySize) {
        ASSERT_EQ(SecurityLevel::kSecureAuthenticated,
                  SecurityProperties(SecurityLevel::kAuthenticated, enc_key_size, sc).level());
        ASSERT_EQ(
            SecurityLevel::kSecureAuthenticated,
            SecurityProperties(SecurityLevel::kSecureAuthenticated, enc_key_size, sc).level());
      } else {
        ASSERT_EQ(SecurityLevel::kAuthenticated,
                  SecurityProperties(SecurityLevel::kAuthenticated, enc_key_size, sc).level());
      }
    }
  }
}

TEST(TypesTest, HasKeysToDistribute) {
  PairingFeatures local_link_key_and_others;
  local_link_key_and_others.local_key_distribution = KeyDistGen::kLinkKey | KeyDistGen::kEncKey;
  EXPECT_TRUE(HasKeysToDistribute(local_link_key_and_others));

  PairingFeatures remote_link_key_and_others;
  remote_link_key_and_others.remote_key_distribution = KeyDistGen::kLinkKey | KeyDistGen::kIdKey;
  EXPECT_TRUE(HasKeysToDistribute(remote_link_key_and_others));

  PairingFeatures remote_link_key_only;
  remote_link_key_only.remote_key_distribution = KeyDistGen::kLinkKey;
  EXPECT_FALSE(HasKeysToDistribute(remote_link_key_only));

  // No keys set.
  EXPECT_FALSE(HasKeysToDistribute(PairingFeatures{}));
}

TEST(TypesTest, SecurityPropertiesComparisonWorks) {
  const SecurityProperties kInsecure(SecurityLevel::kNoSecurity, kMinEncryptionKeySize,
                                     /*secure_connections=*/false),
      kEncryptedLegacy(SecurityLevel::kEncrypted, kMaxEncryptionKeySize,
                       /*secure_connections=*/false),
      kEncryptedSecure(SecurityLevel::kEncrypted, kMaxEncryptionKeySize,
                       /*secure_connections=*/true),
      kAuthenticatedLegacy(SecurityLevel::kAuthenticated, kMaxEncryptionKeySize,
                           /*secure_connections=*/false),
      kAuthenticatedSecure(SecurityLevel::kAuthenticated, kMaxEncryptionKeySize,
                           /*secure_connections=*/true),
      kAuthenticatedSecureShortKey(SecurityLevel::kAuthenticated, kMinEncryptionKeySize,
                                   /*secure_connections=*/true);

  const std::array kTestProperties{kInsecure,
                                   kEncryptedLegacy,
                                   kEncryptedSecure,
                                   kAuthenticatedLegacy,
                                   kAuthenticatedSecure,
                                   kAuthenticatedSecureShortKey};
  // Tests that are true for all of kTestProperties
  for (auto props : kTestProperties) {
    SCOPED_TRACE(props.ToString());
    ASSERT_TRUE(props.IsAsSecureAs(props));
    // kAuthenticatedSecure is the "most secure" possible properties.
    ASSERT_TRUE(kAuthenticatedSecure.IsAsSecureAs(props));
  }

  // Test based on the least secure SecurityProperties in kTestProperties
  for (auto props : kTestProperties) {
    SCOPED_TRACE(props.ToString());
    if (props != kInsecure) {
      ASSERT_FALSE(kInsecure.IsAsSecureAs(props));
      ASSERT_TRUE(props.IsAsSecureAs(kInsecure));
      if (props != kAuthenticatedSecureShortKey) {
        ASSERT_FALSE(kAuthenticatedSecureShortKey.IsAsSecureAs(props));
      }
    }
  }

  // Test Encrypted Legacy properties
  for (auto props : kTestProperties) {
    SCOPED_TRACE(props.ToString());
    if (props != kInsecure && props != kEncryptedLegacy) {
      ASSERT_FALSE(kEncryptedLegacy.IsAsSecureAs(props));
    }
  }

  // Test Encrypted Secure properties
  ASSERT_TRUE(kEncryptedSecure.IsAsSecureAs(kEncryptedLegacy));
  for (auto props :
       std::array{kAuthenticatedLegacy, kAuthenticatedSecure, kAuthenticatedSecureShortKey}) {
    SCOPED_TRACE(props.ToString());
    ASSERT_FALSE(kEncryptedSecure.IsAsSecureAs(props));
  }

  // Test Authenticated Legacy properties
  ASSERT_TRUE(kAuthenticatedLegacy.IsAsSecureAs(kEncryptedLegacy));
  for (auto props :
       std::array{kEncryptedSecure, kAuthenticatedSecure, kAuthenticatedSecureShortKey}) {
    SCOPED_TRACE(props.ToString());
    ASSERT_FALSE(kAuthenticatedLegacy.IsAsSecureAs(props));
  }
}

#ifndef NINSPECT
TEST(TypesTest, InspectSecurityProperties) {
  inspect::Inspector inspector;

  SecurityProperties kInsecure(SecurityLevel::kNoSecurity, kMinEncryptionKeySize,
                               /*secure_connections=*/false),
      kEncryptedLegacy(SecurityLevel::kEncrypted, kMaxEncryptionKeySize,
                       /*secure_connections=*/false),
      kEncryptedSecure(SecurityLevel::kEncrypted, kMaxEncryptionKeySize,
                       /*secure_connections=*/true),
      kAuthenticatedLegacy(SecurityLevel::kAuthenticated, kMaxEncryptionKeySize,
                           /*secure_connections=*/false),
      kAuthenticatedSecure(SecurityLevel::kAuthenticated, kMaxEncryptionKeySize,
                           /*secure_connections=*/true);

  // kInsecure
  kInsecure.AttachInspect(inspector.GetRoot(), "security_properties");
  auto insecure_matcher = AllOf(NodeMatches(AllOf(
      NameMatches("security_properties"),
      PropertyList(UnorderedElementsAre(StringIs("level", "not secure"), BoolIs("encrypted", false),
                                        BoolIs("secure_connections", false),
                                        BoolIs("authenticated", false))))));

  // kEncryptedLegacy
  kEncryptedLegacy.AttachInspect(inspector.GetRoot(), "security_properties");
  auto encrypted_legacy_matcher = AllOf(
      NodeMatches(AllOf(NameMatches("security_properties"),
                        PropertyList(UnorderedElementsAre(
                            StringIs("level", "encrypted"), BoolIs("encrypted", true),
                            BoolIs("secure_connections", false), BoolIs("authenticated", false),
                            StringIs("key_type", "kUnauthenticatedCombination192"))))));

  // kEncryptedSecure
  kEncryptedSecure.AttachInspect(inspector.GetRoot(), "security_properties");
  auto encrypted_secure_matcher = AllOf(
      NodeMatches(AllOf(NameMatches("security_properties"),
                        PropertyList(UnorderedElementsAre(
                            StringIs("level", "encrypted"), BoolIs("encrypted", true),
                            BoolIs("secure_connections", true), BoolIs("authenticated", false),
                            StringIs("key_type", "kUnauthenticatedCombination256"))))));

  // kAuthenticatedLegacy
  kAuthenticatedLegacy.AttachInspect(inspector.GetRoot(), "security_properties");
  auto authenticated_legacy_matcher = AllOf(
      NodeMatches(AllOf(NameMatches("security_properties"),
                        PropertyList(UnorderedElementsAre(
                            StringIs("level", "Authenticated"), BoolIs("encrypted", true),
                            BoolIs("secure_connections", false), BoolIs("authenticated", true),
                            StringIs("key_type", "kAuthenticatedCombination192"))))));

  // kAuthenticatedSecure
  kAuthenticatedSecure.AttachInspect(inspector.GetRoot(), "security_properties");
  auto authenticated_secure_matcher = AllOf(NodeMatches(AllOf(
      NameMatches("security_properties"),
      PropertyList(UnorderedElementsAre(
          StringIs("level", "Authenticated with Secure Connections and 128-bit key"),
          BoolIs("encrypted", true), BoolIs("secure_connections", true),
          BoolIs("authenticated", true), StringIs("key_type", "kAuthenticatedCombination256"))))));

  inspect::Hierarchy hierarchy = ReadInspect(inspector);
  EXPECT_THAT(hierarchy, AllOf(ChildrenMatch(UnorderedElementsAre(
                             insecure_matcher, encrypted_legacy_matcher, encrypted_secure_matcher,
                             authenticated_legacy_matcher, authenticated_secure_matcher))));
}
#endif  // NINSPECT

}  // namespace
}  // namespace bt::sm
