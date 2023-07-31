// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "job_watcher.h"

#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/exception.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/syscalls/exception.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

zx::result<> profiler::JobWatcher::Watch(async_dispatcher_t* dispatcher) {
  zx_status_t status =
      job_->create_exception_channel(ZX_EXCEPTION_CHANNEL_DEBUGGER, &exception_channel_);

  if (status != ZX_OK) {
    return zx::error(status);
  }

  wait_.emplace(this, exception_channel_.get(), ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED,
                /*options=*/0);
  wait_->Begin(dispatcher);
  return zx::ok();
}

void profiler::JobWatcher::HandleException(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                           zx_status_t status, const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to wait on exception";
    return;
  }
  if (signal->observed & ZX_CHANNEL_READABLE) {
    zx_exception_info_t info;
    zx::exception exception;
    zx_status_t status = exception_channel_.read(0, &info, exception.reset_and_get_address(),
                                                 sizeof(info), 1, nullptr, nullptr);
    if (status != ZX_OK) {
      return;
    }
    zx::process p;
    status = exception.get_process(&p);
    if (status != ZX_OK) {
      return;
    }
    handler_(info.pid, std::move(p));
    uint32_t state = ZX_EXCEPTION_STATE_HANDLED;
    exception.set_property(ZX_PROP_EXCEPTION_STATE, &state, sizeof(state));
  } else {
    // ZX_CHANNEL_PEER_CLOSED:
    exception_channel_.reset();
    return;
  }
  if (wait_) {
    wait_->Begin(dispatcher);
  }
}
