// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/feedback/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include <utility>

#include "src/developer/forensics/crash_reports/config.h"
#include "src/developer/forensics/crash_reports/constants.h"
#include "src/developer/forensics/crash_reports/info/info_context.h"
#include "src/developer/forensics/crash_reports/main_service.h"
#include "src/developer/forensics/utils/component/component.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/trim.h"

namespace forensics {
namespace crash_reports {
namespace {

const char kDefaultConfigPath[] = "/pkg/data/crash_reports/default_config.json";
const char kOverrideConfigPath[] = "/config/data/crash_reports/override_config.json";

std::optional<Config> GetConfig() {
  std::optional<Config> config;
  if (files::IsFile(kOverrideConfigPath)) {
    if (config = ParseConfig(kOverrideConfigPath); !config) {
      FX_LOGS(ERROR) << "Failed to read override config file at " << kOverrideConfigPath
                     << " - falling back to default config file";
    }
  }

  if (!config) {
    if (config = ParseConfig(kDefaultConfigPath); !config) {
      FX_LOGS(ERROR) << "Failed to read default config file at " << kDefaultConfigPath;
    }
  }

  return config;
}

ErrorOr<std::string> ReadStringFromFile(const std::string& filepath) {
  std::string content;
  if (!files::ReadFileToString(filepath, &content)) {
    FX_LOGS(ERROR) << "Failed to read content from " << filepath;
    return Error::kFileReadFailure;
  }
  return std::string(fxl::TrimString(content, "\r\n"));
}

}  // namespace

int main() {
  syslog::SetTags({"forensics", "crash"});

  forensics::component::Component component;

  auto config = GetConfig();
  if (!config) {
    FX_LOGS(FATAL) << "Failed to set up crash reporter";
    return EXIT_FAILURE;
  }

  const ErrorOr<std::string> build_version = ReadStringFromFile("/config/build-info/version");

  AnnotationMap default_annotations;
  default_annotations.Set("osName", "Fuchsia")
      .Set("osVersion", build_version)
      // TODO(fxbug.dev/70398): These keys are duplicates from feedback data, find a better way to
      // share them.
      .Set("build.version", build_version)
      .Set("build.board", ReadStringFromFile("/config/build-info/board"))
      .Set("build.product", ReadStringFromFile("/config/build-info/product"))
      .Set("build.latest-commit-date", ReadStringFromFile("/config/build-info/latest-commit-date"));

  MainService main_service(component.Dispatcher(), component.Services(), component.InspectRoot(),
                           component.Clock(), std::move(*config), build_version,
                           default_annotations);

  // fuchsia.feedback.CrashReporter
  component.AddPublicService(::fidl::InterfaceRequestHandler<fuchsia::feedback::CrashReporter>(
      [&main_service](::fidl::InterfaceRequest<fuchsia::feedback::CrashReporter> request) {
        main_service.HandleCrashReporterRequest(std::move(request));
      }));
  // fuchsia.feedback.CrashReportingProductRegister
  component.AddPublicService(
      ::fidl::InterfaceRequestHandler<fuchsia::feedback::CrashReportingProductRegister>(
          [&main_service](
              ::fidl::InterfaceRequest<fuchsia::feedback::CrashReportingProductRegister> request) {
            main_service.HandleCrashRegisterRequest(std::move(request));
          }));

  component.OnStopSignal([&](::fit::deferred_callback) {
    FX_LOGS(INFO) << "Received stop signal; stopping upload and snapshot request, but not exiting "
                     "to continue persisting new reports.";
    main_service.ShutdownImminent();
    // Don't stop the loop so incoming crash reports can be persisted while appmgr is waiting to
    // terminate v1 components.
  });

  component.RunLoop();

  return EXIT_SUCCESS;
}

}  // namespace crash_reports
}  // namespace forensics
