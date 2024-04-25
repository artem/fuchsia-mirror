// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_TRACE_COMMANDS_TIME_H_
#define SRC_PERFORMANCE_TRACE_COMMANDS_TIME_H_

#include "src/performance/trace/command.h"

namespace tracing {

class TimeCommand : public Command {
 public:
  static Info Describe();

  explicit TimeCommand();

 protected:
  void Start(const fxl::CommandLine& command_line) override;
};

}  // namespace tracing

#endif  // SRC_PERFORMANCE_TRACE_COMMANDS_TIME_H_
