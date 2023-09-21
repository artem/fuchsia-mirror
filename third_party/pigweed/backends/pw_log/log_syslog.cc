// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include "pw_log_fuchsia/log_fuchsia.h"

extern "C" {
namespace {

::fuchsia_logging::LogSeverity ToFuchsiaLevel(int pw_level) {
  switch (pw_level) {
    case PW_LOG_LEVEL_ERROR:
      return fuchsia_logging::LOG_ERROR;
    case PW_LOG_LEVEL_WARN:
      return fuchsia_logging::LOG_WARNING;
    case PW_LOG_LEVEL_INFO:
      return fuchsia_logging::LOG_INFO;
    case PW_LOG_LEVEL_DEBUG:
      return fuchsia_logging::LOG_DEBUG;
    default:
      return fuchsia_logging::LOG_ERROR;
  }
}

}  // namespace

void pw_log_fuchsia_impl(int level, const char* module_name, const char* file_name, int line_number,
                         const char* message) {
  ::fuchsia_logging::LogMessage(ToFuchsiaLevel(level), file_name, line_number, nullptr, module_name)
          .stream()
      << message;
}
}
