// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler_manager.h"

namespace forensics {
namespace exceptions {

HandlerManager::HandlerManager(async_dispatcher_t* dispatcher, CrashCounter crash_counter,
                               size_t max_num_handlers, zx::duration exception_ttl,
                               bool suspend_enabled)
    : dispatcher_(dispatcher),
      crash_counter_(std::move(crash_counter)),
      exception_ttl_(exception_ttl) {
  handlers_.reserve(max_num_handlers);
  for (size_t i = 0; i < max_num_handlers; ++i) {
    handlers_.emplace_back(
        dispatcher_, suspend_enabled,
        /*log_moniker=*/
        [crash_counter = &crash_counter_](const std::string& moniker) {
          crash_counter->Increment(moniker);
        },
        /*on_available=*/
        [i, this] {
          // Push to the front so already initialized handlers are used first.
          available_handlers_.push_front(i);
          HandleNextPendingException();
        });
    available_handlers_.emplace_back(i);
  }
}

void HandlerManager::Handle(zx::exception exception) {
  pending_exceptions_.emplace_back(dispatcher_, exception_ttl_, std::move(exception));
  HandleNextPendingException();
}

void HandlerManager::HandleNextPendingException() {
  if (pending_exceptions_.empty() || available_handlers_.empty()) {
    return;
  }

  // We must reserve all state needed to handle the exception (the handler and the exception) and
  // remove it from the queues prior to actually handling the exception. This is done to prevent
  // that state from being erroneously being reused when ProcessHandler::Handle ends up calling
  // HandleNextPendingException on a failure.
  const size_t handler_idx = available_handlers_.front();
  available_handlers_.pop_front();

  zx::exception exception = pending_exceptions_.front().TakeException();
  zx::process process = pending_exceptions_.front().TakeProcess();
  zx::thread thread = pending_exceptions_.front().TakeThread();

  pending_exceptions_.pop_front();

  handlers_[handler_idx].Handle(std::move(exception), std::move(process), std::move(thread));
}

}  // namespace exceptions
}  // namespace forensics
