// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "garnet/bin/debug_agent/integration_tests/message_loop_wrapper.h"

#include "garnet/lib/debug_ipc/helper/message_loop_async.h"

namespace debug_agent {

MessageLoopWrapper::MessageLoopWrapper() {
  loop_ = std::make_unique<debug_ipc::MessageLoopAsync>();
  loop_->Init();
}

MessageLoopWrapper::~MessageLoopWrapper() {
  loop_->Cleanup();
}

}  // namespace debug_agent
