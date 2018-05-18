// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Token manager unit tests using DEV auth provider.

#include <memory>
#include <string>

#include <auth/cpp/fidl.h>

#include "garnet/bin/auth/store/auth_db_file_impl.h"
#include "gtest/gtest.h"
#include "lib/app/cpp/application_context.h"
#include "lib/app/cpp/connect.h"
#include "lib/fidl/cpp/binding.h"
#include "lib/fsl/tasks/message_loop.h"
#include "lib/fxl/command_line.h"
#include "lib/fxl/files/file.h"
#include "lib/fxl/files/path.h"
#include "lib/fxl/files/scoped_temp_dir.h"
#include "lib/fxl/log_settings_command_line.h"
#include "lib/fxl/logging.h"
#include "lib/fxl/macros.h"
#include "lib/fxl/strings/string_view.h"
#include "lib/svc/cpp/services.h"
#include "lib/test_runner/cpp/reporting/gtest_listener.h"
#include "lib/test_runner/cpp/reporting/reporter.h"

namespace google_oauth_demo {
namespace {

const std::string kTestUserId = "tq_user_1";
const std::string kTestAppUrl = "/system/test/google_oauth_demo";
constexpr fxl::StringView kRefreshTokenFlag = "refresh-token";
constexpr fxl::StringView kUserProfileIdFlag = "user-profile-id";

void PrintUsage(const char* executable_name) {
  std::cout << "Usage: " << executable_name << " --" << kUserProfileIdFlag
            << "=<string>"
            << " --" << kRefreshTokenFlag << "=<string>" << std::endl;
}

auth::AppConfig MakeGoogleAppConfig(const std::string& client_id,
                                    const std::string& client_secret) {
  auth::AppConfig google_app_config;
  google_app_config.auth_provider_type = auth::AuthProviderType::GOOGLE;
  google_app_config.client_id = client_id;
  google_app_config.client_secret = client_secret;
  return google_app_config;
}

// This is a sample app demonstrating Google OAuth handshake for minting
// OAuth tokens.
class GoogleTokenManagerApp : auth::AuthenticationContextProvider {
 public:
  GoogleTokenManagerApp(const std::string& user_profile_id,
                        const std::string& refresh_token)
      : user_profile_id_(user_profile_id),
        refresh_token_(refresh_token),
        application_context_(
            component::ApplicationContext::CreateFromStartupInfo()),
        auth_context_provider_binding_(this) {}

  ~GoogleTokenManagerApp() {}

  void Run() {
    Initialize();
    SetupDb();
    FetchAndVerifyAccessToken();
    FetchAndVerifyIdToken();
    FetchAndVerifyFirebaseToken();
    VerifyRevokeToken();
  }

 private:
  // |AuthenticationContextProvider|
  void GetAuthenticationUIContext(
      fidl::InterfaceRequest<auth::AuthenticationUIContext> request) override {
    FXL_LOG(INFO) << "DevTokenManagerAppTest::GetAuthenticationUIContext() is "
                     "unimplemented.";
  }

  void Initialize() {
    component::Services services;
    component::LaunchInfo launch_info;
    launch_info.url = "token_manager";
    launch_info.directory_request = services.NewRequest();
    {
      std::ostringstream stream;
      stream << "--verbose=" << fxl::GetVlogVerbosity();
      launch_info.arguments.push_back(stream.str());
    }
    application_context_->launcher()->CreateApplication(
        std::move(launch_info), app_controller_.NewRequest());
    app_controller_.set_error_handler([] {
      FXL_LOG(ERROR) << "Error in connecting to TokenManagerFactory service.";
    });

    services.ConnectToService(token_mgr_factory_.NewRequest());

    auth::AuthProviderConfig google_config;
    google_config.auth_provider_type = auth::AuthProviderType::GOOGLE;
    google_config.url = "google_auth_provider";

    fidl::VectorPtr<auth::AuthProviderConfig> auth_provider_configs;
    auth_provider_configs.push_back(std::move(google_config));

    token_mgr_factory_->GetTokenManager(
        kTestUserId, kTestAppUrl, std::move(auth_provider_configs),
        auth_context_provider_binding_.NewBinding(), token_mgr_.NewRequest());
  }

  // This step is equivalent to calling Authorize(), until we can figure out how
  // to automate the UI flow. Manually creates the creds db file using the
  // passed in values for "refresh-token" and "user-profile-id" flags.
  void SetupDb() {
    auto file_name = "/data/auth" + kTestUserId + "token_store.db";
    auto auth_db = std::make_unique<auth::store::AuthDbFileImpl>(file_name);
    if (auth_db->Load() != auth::store::Status::kOK) {
      FXL_LOG(ERROR) << "Auth DB failed to load file: " << file_name
                     << " ,exiting...";
    }

    auto cred_id = auth::store::CredentialIdentifier(
        user_profile_id_, auth::store::IdentityProvider::GOOGLE);

    if (auth_db->AddCredential(auth::store::CredentialValue(
            cred_id, refresh_token_)) != auth::store::Status::kOK) {
      FXL_LOG(ERROR) << "Auth DB failed to load file: " << file_name
                     << " ,exiting...";
    }
  }

  void FetchAndVerifyAccessToken() {
    fidl::VectorPtr<fidl::StringPtr> scopes;
    scopes.push_back("https://www.googleapis.com/auth/plus.me");
    scopes.push_back("https://www.googleapis.com/auth/userinfo.email");

    auth::Status status;
    fidl::StringPtr access_token;

    token_mgr_->GetAccessToken(MakeGoogleAppConfig("", ""), user_profile_id_,
                               std::move(scopes), &status, &access_token);
    ASSERT_EQ(auth::Status::OK, status);
    EXPECT_TRUE(access_token.get().find(":at_") != std::string::npos);
  }

  void FetchAndVerifyIdToken() {
    auth::Status status;
    fidl::StringPtr id_token;

    token_mgr_->GetIdToken(MakeGoogleAppConfig("", ""), user_profile_id_, "",
                           &status, &id_token);
    ASSERT_EQ(auth::Status::OK, status);
    EXPECT_TRUE(id_token.get().find(":idt_") != std::string::npos);
  }

  void FetchAndVerifyFirebaseToken() {
    auth::Status status;
    auth::FirebaseTokenPtr firebase_token;

    // TODO: Wire test firebase api key
    token_mgr_->GetFirebaseToken(MakeGoogleAppConfig("", ""), user_profile_id_,
                                 "", "", &status, &firebase_token);
    ASSERT_EQ(auth::Status::OK, status);
    EXPECT_FALSE(firebase_token);
  }

  void VerifyRevokeToken() {
    auth::Status status;
    auth::FirebaseTokenPtr firebase_token;

    token_mgr_->DeleteAllTokens(MakeGoogleAppConfig("", ""), user_profile_id_,
                                &status);
    ASSERT_EQ(auth::Status::OK, status);
  }

  const std::string user_profile_id_;
  const std::string refresh_token_;
  std::unique_ptr<component::ApplicationContext> application_context_;
  component::ApplicationControllerPtr app_controller_;

  fidl::Binding<auth::AuthenticationContextProvider>
      auth_context_provider_binding_;

  auth::TokenManagerSyncPtr token_mgr_;
  auth::TokenManagerFactorySyncPtr token_mgr_factory_;
  fidl::BindingSet<auth::AuthenticationUIContext> ui_bindings_;

  FXL_DISALLOW_COPY_AND_ASSIGN(GoogleTokenManagerApp);
};

}  // namespace
}  // namespace google_oauth_demo

int main(int argc, char** argv) {
  fxl::CommandLine command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  std::string refresh_token;
  if (!command_line.GetOptionValue(
          google_oauth_demo::kRefreshTokenFlag.ToString(), &refresh_token)) {
    google_oauth_demo::PrintUsage(argv[0]);
    return -1;
  }

  std::string user_profile_id_;
  if (!command_line.GetOptionValue(
          google_oauth_demo::kUserProfileIdFlag.ToString(),
          &user_profile_id_)) {
    google_oauth_demo::PrintUsage(argv[0]);
    return -1;
  }

  fsl::MessageLoop loop;
  google_oauth_demo::GoogleTokenManagerApp gtm(std::move(refresh_token),
                                               std::move(user_profile_id_));
  gtm.Run();
  return 0;
}
