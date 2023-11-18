// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/linux_exception_handle.h"

#include <string.h>
#include <zircon/errors.h>

#include "src/developer/debug/debug_agent/linux_arch.h"
#include "src/developer/debug/debug_agent/linux_task.h"
#include "src/developer/debug/debug_agent/linux_thread_handle.h"

namespace debug_agent {

LinuxExceptionHandle::LinuxExceptionHandle(fxl::RefPtr<LinuxTask> task, const siginfo_t& info)
    : task_(std::move(task)),
      type_(arch::DecodeExceptionType(info.si_signo, info.si_code)),
      info_(info) {}

// This contructor creates thread start/exit exceptions. There is no siginfo_t since it doesn't
// correspond to a real Linux exception.
LinuxExceptionHandle::LinuxExceptionHandle(debug_ipc::ExceptionType type,
                                           fxl::RefPtr<LinuxTask> thread)
    : task_(std::move(thread)), type_(type) {
  // This contructor is only used for thread fake-"exception" notifications.
  FX_DCHECK(type == debug_ipc::ExceptionType::kThreadStarting ||
            type == debug_ipc::ExceptionType::kThreadExiting);

  // siginfo_t has a union so 0-initialization requires memset.
  memset(&info_, 0, sizeof(info_));
}

LinuxExceptionHandle::~LinuxExceptionHandle() {
  // Resume the thread.
  task_->DecrementSuspendCount();
}

std::unique_ptr<ThreadHandle> LinuxExceptionHandle::GetThreadHandle() const {
  return std::make_unique<LinuxThreadHandle>(task_);
}

debug_ipc::ExceptionType LinuxExceptionHandle::GetType(const ThreadHandle& thread) const {
  return type_;
}

fit::result<debug::Status, ExceptionHandle::Resolution> LinuxExceptionHandle::GetResolution()
    const {
  // TODO(brettw) implement exception forwarding on Linux.
  return fit::error(debug::Status("Unimplemented on Linux"));
}

debug::Status LinuxExceptionHandle::SetResolution(Resolution) {
  // TODO(brettw) implement exception forwarding on Linux.
  return debug::Status();
}

fit::result<debug::Status, debug_ipc::ExceptionStrategy> LinuxExceptionHandle::GetStrategy() const {
  return fit::success(debug_ipc::ExceptionStrategy::kFirstChance);
}

debug::Status LinuxExceptionHandle::SetStrategy(debug_ipc::ExceptionStrategy strategy) {
  return debug::Status("Linux does not support exception strategies.");
}

}  // namespace debug_agent
