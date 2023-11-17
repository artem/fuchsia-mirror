// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/cobalt/bin/app/configuration_data.h"

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "third_party/abseil-cpp/absl/strings/match.h"

namespace cobalt::test {

using testing::ContainsRegex;

const char kTestDir[] = "/tmp/cobalt_config_test";

bool WriteFile(const std::string& file, const std::string& to_write) {
  return files::WriteFile(std::string(kTestDir) + std::string("/") + file, to_write.c_str(),
                          to_write.length());
}

// Tests behavior when there are no config files.
TEST(ConfigTest, Empty) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_TRUE(absl::StrContains(config_data.AnalyzerPublicKeyPath(), "prod"));
  auto env = config_data.GetBackendEnvironment();
  EXPECT_EQ(config::Environment::PROD, env);
  EXPECT_TRUE(absl::StrContains(config_data.ShufflerPublicKeyPath(), "prod"));
  EXPECT_EQ(cobalt::ReleaseStage::GA, config_data.GetReleaseStage());
}

TEST(ConfigTest, DefaultEnvironment) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"default_environment\": \"DEVEL\"}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  auto env = config_data.GetBackendEnvironment();
  EXPECT_EQ(config::Environment::DEVEL, env);
}

// Tests behavior when there is one valid config file.
TEST(ConfigTest, OneValidFile) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("cobalt_environment", "PROD"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_TRUE(absl::StrContains(config_data.AnalyzerPublicKeyPath(), "prod"));
  auto env = config_data.GetBackendEnvironment();
  EXPECT_EQ(config::Environment::PROD, env);
  EXPECT_TRUE(absl::StrContains(config_data.ShufflerPublicKeyPath(), "prod"));
  EXPECT_EQ(cobalt::ReleaseStage::GA, config_data.GetReleaseStage());
}

// Tests behavior when there is one invalid config file.
TEST(ConfigTest, OneInvalidFile) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("cobalt_environment", "INVALID"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_TRUE(absl::StrContains(config_data.AnalyzerPublicKeyPath(), "prod"));
  auto env = config_data.GetBackendEnvironment();
  EXPECT_EQ(config::Environment::PROD, env);
  EXPECT_TRUE(absl::StrContains(config_data.ShufflerPublicKeyPath(), "prod"));
  EXPECT_EQ(cobalt::ReleaseStage::GA, config_data.GetReleaseStage());
}

TEST(ConfigTest, ReleaseStageGA) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"release_stage\": \"GA\"}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(cobalt::ReleaseStage::GA, config_data.GetReleaseStage());
}

TEST(ConfigTest, ReleaseStageDEBUG) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"release_stage\": \"DEBUG\"}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(cobalt::ReleaseStage::DEBUG, config_data.GetReleaseStage());
}

TEST(ConfigTest, DataCollectionPolicyDoNotUpload) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"default_data_collection_policy\": \"DO_NOT_UPLOAD\"}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(cobalt::CobaltServiceInterface::DataCollectionPolicy::DO_NOT_UPLOAD,
            config_data.GetDataCollectionPolicy());
}

TEST(ConfigTest, DataCollectionPolicyCollectAndUpload) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(
      WriteFile("config.json", "{\"default_data_collection_policy\": \"COLLECT_AND_UPLOAD\"}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(cobalt::CobaltServiceInterface::DataCollectionPolicy::COLLECT_AND_UPLOAD,
            config_data.GetDataCollectionPolicy());
}

TEST(ConfigTest, WatchForUserConsentDefault) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(true, config_data.GetWatchForUserConsent());
}

TEST(ConfigTest, WatchForUserConsentTrue) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"watch_for_user_consent\":true}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(true, config_data.GetWatchForUserConsent());
}

TEST(ConfigTest, WatchForUserConsentFalse) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"watch_for_user_consent\":false}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(false, config_data.GetWatchForUserConsent());
}

TEST(ConfigTest, EnableReplacementMetricsDefault) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(false, config_data.GetEnableReplacementMetrics());
}

TEST(ConfigTest, EnableReplacementMetricsTrue) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"enable_replacement_metrics\":true}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(true, config_data.GetEnableReplacementMetrics());
}

TEST(ConfigTest, EnableReplacementMetricsFalse) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"enable_replacement_metrics\":false}"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);

  EXPECT_EQ(false, config_data.GetEnableReplacementMetrics());
}

TEST(ConfigTest, GetApiKeyNotEmpty) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);
  auto api_key = config_data.GetApiKey();
  EXPECT_EQ(api_key, "cobalt-default-api-key");
}

TEST(ConfigTest, GetApiKey) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("api_key.hex", "deadbeef"));

  FuchsiaConfigurationData config_data(kTestDir, kTestDir);
  auto api_key = config_data.GetApiKey();
  EXPECT_EQ(api_key, "\xDE\xAD\xBE\xEF");
}

TEST(ConfigTest, GetBuildType) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));

  EXPECT_TRUE(WriteFile("type", "eng\n"));
  FuchsiaConfigurationData config_data(kTestDir, kTestDir, kTestDir);
  auto build_type = config_data.GetBuildType();
  EXPECT_EQ(build_type, SystemProfile::ENG);
}

TEST(ConfigTest, GetBuildTypeMissingFile) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  FuchsiaConfigurationData config_data(kTestDir, kTestDir, kTestDir);
  auto build_type = config_data.GetBuildType();
  EXPECT_EQ(build_type, SystemProfile::UNKNOWN_TYPE);
}

TEST(ConfigTest, GetBuildTypeInvalidType) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));

  EXPECT_TRUE(WriteFile("type", "invalid"));
  FuchsiaConfigurationData config_data(kTestDir, kTestDir, kTestDir);
  auto build_type = config_data.GetBuildType();
  EXPECT_EQ(build_type, SystemProfile::OTHER_TYPE);
}

TEST(JSONHelper, FailsToReadInvalidConfig) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{"));

  JSONHelper helper(files::JoinPath(kTestDir, "config.json"));
  EXPECT_FALSE(helper.GetString("invalid_config").ok());
  EXPECT_THAT(helper.GetString("invalid_config").status().error_message(),
              ContainsRegex("Failed to parse"));

  EXPECT_FALSE(helper.GetBool("invalid_config").ok());
  EXPECT_THAT(helper.GetBool("invalid_config").status().error_message(),
              ContainsRegex("Failed to parse"));
}

TEST(JSONHelper, FailsToReadAbsentKeys) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{}"));

  JSONHelper helper(files::JoinPath(kTestDir, "config.json"));
  EXPECT_FALSE(helper.GetString("not_present").ok());
  EXPECT_THAT(helper.GetString("not_present").status().error_message(),
              ContainsRegex("not present"));

  EXPECT_FALSE(helper.GetBool("not_present").ok());
  EXPECT_THAT(helper.GetBool("not_present").status().error_message(), ContainsRegex("not present"));
}

TEST(JSONHelper, FailsToReadWrongType) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"not_string\":null,\"not_bool\":\"test\"}"));

  JSONHelper helper(files::JoinPath(kTestDir, "config.json"));

  EXPECT_FALSE(helper.GetString("not_string").ok());
  EXPECT_THAT(helper.GetString("not_string").status().error_message(),
              ContainsRegex("is not of type string"));
  EXPECT_THAT(helper.GetString("not_string").status().error_details(),
              ContainsRegex("is expected to be a string"));

  EXPECT_FALSE(helper.GetBool("not_bool").ok());
  EXPECT_THAT(helper.GetBool("not_bool").status().error_message(),
              ContainsRegex("is not of type bool"));
  EXPECT_THAT(helper.GetBool("not_bool").status().error_details(),
              ContainsRegex("is expected to be a bool"));
}

TEST(JSONHelper, CanRead) {
  EXPECT_TRUE(files::DeletePath(kTestDir, true));
  EXPECT_TRUE(files::CreateDirectory(kTestDir));
  EXPECT_TRUE(WriteFile("config.json", "{\"a_string\":\"a value\",\"a_bool\":true}"));

  JSONHelper helper(files::JoinPath(kTestDir, "config.json"));

  {
    auto value = helper.GetString("a_string");
    EXPECT_TRUE(value.ok());
    EXPECT_EQ(value.value(), "a value");
  }

  {
    auto value = helper.GetBool("a_bool");
    EXPECT_TRUE(value.ok());
    EXPECT_EQ(value.value(), true);
  }
}

}  // namespace cobalt::test
