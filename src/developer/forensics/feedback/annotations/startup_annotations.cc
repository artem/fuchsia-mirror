// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/startup_annotations.h"

#include <fuchsia/sysinfo/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <lib/fidl/cpp/string.h>
#include <lib/fidl/cpp/synchronous_interface_ptr.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/syscalls.h>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/feedback/constants.h"
#include "src/developer/forensics/feedback/reboot_log/annotations.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/trim.h"

namespace forensics::feedback {
namespace {

ErrorOrString ReadAnnotation(const std::string& filepath) {
  std::string content;
  if (!files::ReadFileToString(filepath, &content)) {
    FX_LOGS(WARNING) << "Failed to read content from " << filepath;
    return ErrorOrString(Error::kFileReadFailure);
  }
  return ErrorOrString(std::string(fxl::TrimString(content, "\r\n")));
}

ErrorOrString BoardName() {
  fuchsia::sysinfo::SysInfoSyncPtr sysinfo;

  if (const zx_status_t status = fdio_service_connect("/svc/fuchsia.sysinfo.SysInfo",
                                                      sysinfo.NewRequest().TakeChannel().release());
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Error connecting to sysinfo";
    return ErrorOrString(Error::kConnectionError);
  }

  ::fidl::StringPtr out_board_name;
  zx_status_t out_status;
  if (const zx_status_t status = sysinfo->GetBoardName(&out_status, &out_board_name);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to get device board name";
    return ErrorOrString(Error::kConnectionError);
  }
  if (out_status != ZX_OK) {
    FX_PLOGS(ERROR, out_status) << "Failed to get device board name";
    return ErrorOrString(Error::kBadValue);
  }
  if (!out_board_name) {
    FX_PLOGS(ERROR, out_status) << "Failed to get device board name";
    return ErrorOrString(Error::kMissingValue);
  }

  return ErrorOrString(out_board_name.value());
}

std::string IsDebug() {
#ifndef NDEBUG
  return "true";
#else
  return "false";
#endif
}

std::string NumCPUs() { return std::to_string(zx_system_get_num_cpus()); }

}  // namespace

Annotations GetStartupAnnotations(const RebootLog& reboot_log) {
  return {
      {kBuildBoardKey, ReadAnnotation(kBuildBoardPath)},
      {kBuildProductKey, ReadAnnotation(kBuildProductPath)},
      {kBuildLatestCommitDateKey, ReadAnnotation(kBuildCommitDatePath)},
      {kBuildVersionKey, ReadAnnotation(kCurrentBuildVersionPath)},
      {kBuildVersionPreviousBootKey, ReadAnnotation(kPreviousBuildVersionPath)},
      {kBuildIsDebugKey, ErrorOrString(IsDebug())},
      {kDeviceBoardNameKey, BoardName()},
      {kDeviceNumCPUsKey, ErrorOrString(NumCPUs())},
      {kSystemBootIdCurrentKey, ReadAnnotation(kCurrentBootIdPath)},
      {kSystemBootIdPreviousKey, ReadAnnotation(kPreviousBootIdPath)},
      {kSystemLastRebootReasonKey, ErrorOrString(LastRebootReasonAnnotation(reboot_log))},
      {kSystemLastRebootUptimeKey, LastRebootUptimeAnnotation(reboot_log)},
  };
}

}  // namespace forensics::feedback
