// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_MAIN_SERVICE_H_
#define SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_MAIN_SERVICE_H_

#include <fuchsia/feedback/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/service_directory.h>

#include <memory>
#include <string>

#include "src/developer/forensics/crash_reports/config.h"
#include "src/developer/forensics/crash_reports/crash_register.h"
#include "src/developer/forensics/crash_reports/crash_reporter.h"
#include "src/developer/forensics/crash_reports/info/info_context.h"
#include "src/developer/forensics/crash_reports/info/main_service_info.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/lib/fxl/macros.h"
#include "src/lib/timekeeper/clock.h"

namespace forensics {
namespace crash_reports {

// Main class that handles incoming CrashReporter requests, manages the component's Inspect state,
// etc.
class MainService {
 public:
  MainService(async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
              inspect::Node* inspect_root, timekeeper::Clock* clock, Config config,
              ErrorOr<std::string> build_version, AnnotationMap default_annotations);

  // Place the component in a state where it expects to be stopped soon. This includes:
  //  * Immediately persisting all future and pending crash reports without snapshots.
  void ShutdownImminent();

  // FIDL protocol handlers.
  //
  // fuchsia.feedback.CrashReportingProductRegister
  void HandleCrashRegisterRequest(
      ::fidl::InterfaceRequest<fuchsia::feedback::CrashReportingProductRegister> request);
  // fuchsia.feedback.CrashReporter
  void HandleCrashReporterRequest(
      ::fidl::InterfaceRequest<fuchsia::feedback::CrashReporter> request);

 private:
  async_dispatcher_t* dispatcher_;
  std::shared_ptr<InfoContext> info_context_;
  MainServiceInfo info_;

  LogTags tags_;
  CrashServer crash_server_;
  SnapshotManager snapshot_manager_;

  CrashRegister crash_register_;
  ::fidl::BindingSet<fuchsia::feedback::CrashReportingProductRegister> crash_register_connections_;

  CrashReporter crash_reporter_;
  ::fidl::BindingSet<fuchsia::feedback::CrashReporter> crash_reporter_connections_;
};

}  // namespace crash_reports
}  // namespace forensics

#endif  // SRC_DEVELOPER_FORENSICS_CRASH_REPORTS_MAIN_SERVICE_H_
