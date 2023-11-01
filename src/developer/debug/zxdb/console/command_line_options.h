// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_CONSOLE_COMMAND_LINE_OPTIONS_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_CONSOLE_COMMAND_LINE_OPTIONS_H_

#include <lib/cmdline/status.h>

#include <optional>
#include <string>
#include <vector>

#include "src/lib/analytics/cpp/core_dev_tools/command_line_options.h"

namespace zxdb {

struct CommandLineOptions {
 private:
  using AnalyticsOption = ::analytics::core_dev_tools::AnalyticsOption;

 public:
  std::optional<std::string> connect;
  std::optional<std::string> unix_connect;
  bool debug_mode = false;
  std::optional<std::string> core;
  std::vector<std::string> attach;
  std::vector<std::string> script_files;
  std::vector<std::string> execute_commands;
  std::optional<std::string> symbol_cache;
  std::vector<std::string> symbol_index_files;
  std::vector<std::string> symbol_paths;
  std::vector<std::string> build_id_dirs;
  std::vector<std::string> ids_txts;
  std::vector<std::string> symbol_servers;
  AnalyticsOption analytics = AnalyticsOption::kUnspecified;
  bool analytics_show = false;
  bool requested_version = false;
  bool enable_debug_adapter = false;
  uint16_t debug_adapter_port = 15678;
  bool no_auto_attach_limbo = false;
  pid_t signal_when_ready = 0;
};

// Parses the given command line into options and params.
//
// Returns an error if the command-line is badly formed. In addition, --help
// text will be returned as an error.
cmdline::Status ParseCommandLine(int argc, const char* argv[], CommandLineOptions* options,
                                 std::vector<std::string>* params);

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_CONSOLE_COMMAND_LINE_OPTIONS_H_
