// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/developer/forensics/exceptions/process_handler.h"

#include <lib/fdio/spawn.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/processargs.h>

#include <array>

#include "src/developer/forensics/exceptions/constants.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace forensics {
namespace exceptions {
namespace {

bool SpawnSubprocess(zx::channel* client, zx::process* subprocess, bool suspend_enabled) {
  static size_t subprocess_num{1};
  const std::string subprocess_name(fxl::StringPrintf("exception_handler_%03zu", subprocess_num++));

  const std::array<const char*, 3> args = {
      subprocess_name.c_str(),
      suspend_enabled ? kSuspendEnabledFlag : kNoSuspendEnabledFlag,
      nullptr,
  };

  // Create a channel to pass as a startup handle.
  zx::channel channel, server;
  if (const zx_status_t status = zx::channel::create(0u, &channel, &server); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to create channel";
    return false;
  }
  const std::array actions = {
      fdio_spawn_action_t{
          .action = FDIO_SPAWN_ACTION_ADD_HANDLE,
          .h =
              {
                  .id = PA_HND(PA_USER0, 0),
                  .handle = server.release(),
              },
      },
  };

  // Create the sub-process.
  char err_msg[FDIO_SPAWN_ERR_MSG_MAX_LENGTH] = {};
  zx::process process;
  if (const zx_status_t status = fdio_spawn_etc(ZX_HANDLE_INVALID, FDIO_SPAWN_CLONE_ALL,
                                                "/pkg/bin/exception_handler", args.data(),
                                                /*environ=*/nullptr, actions.size(), actions.data(),
                                                process.reset_and_get_address(), err_msg);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to launch exception handler process: " << err_msg;
    return false;
  }

  *client = std::move(channel);
  *subprocess = std::move(process);

  return true;
}

}  // namespace

ProcessHandler::ProcessHandler(async_dispatcher_t* dispatcher, bool suspend_enabled,
                               LogMonikerFn log_moniker, fit::closure on_available)
    : dispatcher_(dispatcher),
      suspend_enabled_(suspend_enabled),
      log_moniker_(std::move(log_moniker)),
      on_available_(std::move(on_available)) {
  crash_reporter_.set_error_handler([this](const zx_status_t status) {
    FX_PLOGS(WARNING, status) << "Lost connection to subprocess";
    on_available_();
  });
}

ProcessHandler::~ProcessHandler() {
  if (subprocess_.is_valid()) {
    subprocess_.kill();
  }
}

void ProcessHandler::Handle(zx::exception exception, zx::process process, zx::thread thread) {
  if (!crash_reporter_.is_bound()) {
    zx::channel client;

    // If we are not able to spawn a sub-process, we will have to lose the exception.
    if (!SpawnSubprocess(&client, &subprocess_, suspend_enabled_)) {
      const std::string thread_name =
          (thread.is_valid()) ? fsl::GetObjectName(thread.get()) : "unknown";
      const std::string process_name =
          (process.is_valid()) ? fsl::GetObjectName(process.get()) : "unknown";
      FX_LOGS(WARNING) << "Dropping the exception for thread '" << thread_name << "' in process '"
                       << process_name << "'";
      on_available_();
      return;
    }

    crash_reporter_.Bind(std::move(client), dispatcher_);
  }

  crash_reporter_->Send(std::move(exception), std::move(process), std::move(thread),
                        [this](const ::fidl::StringPtr moniker) {
                          // Log |moniker| before calling |on_available_| because the lambda may
                          // recurse.
                          if (moniker) {
                            log_moniker_(*moniker);
                          }

                          on_available_();
                        });
}

}  // namespace exceptions
}  // namespace forensics
