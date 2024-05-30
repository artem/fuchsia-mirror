// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOGGING_BACKEND_H_
#define LIB_SYSLOG_CPP_LOGGING_BACKEND_H_

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

namespace syslog_runtime {

void SetLogSettings(const fuchsia_logging::LogSettings& settings);

void SetLogSettings(const fuchsia_logging::LogSettings& settings,
                    const std::initializer_list<std::string>& tags);

fuchsia_logging::LogSeverity GetMinLogLevel();

fuchsia_logging::LogSeverity GetMinLogSeverity();

}  // namespace syslog_runtime

#endif  // LIB_SYSLOG_CPP_LOGGING_BACKEND_H_
