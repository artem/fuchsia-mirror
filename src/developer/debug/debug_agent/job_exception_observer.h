// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_OBSERVER_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_OBSERVER_H_

#include <memory>

namespace debug_agent {

class ProcessHandle;

class JobExceptionObserver {
 public:
  virtual void OnProcessStarting(std::unique_ptr<ProcessHandle> process) = 0;
  virtual void OnProcessNameChanged(std::unique_ptr<ProcessHandle> process) = 0;
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_JOB_EXCEPTION_OBSERVER_H_
