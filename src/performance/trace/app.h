// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_APP_H_
#define SRC_PERFORMANCE_TRACE_APP_H_

#include <map>
#include <memory>
#include <string>

#include "src/performance/trace/command.h"

namespace tracing {

class App : public Command {
 public:
  App();

 protected:
  void Start(const fxl::CommandLine& command_line) override;

 private:
  void RegisterCommand(Command::Info info);
  void PrintHelp();

  std::map<std::string, Command::Info> known_commands_;
  std::unique_ptr<Command> command_;

  App(const App&) = delete;
  App(App&&) = delete;
  App& operator=(const App&) = delete;
  App& operator=(App&&) = delete;
};

}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_APP_H_
