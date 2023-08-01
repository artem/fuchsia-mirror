// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fidl/cpp/wire/server.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

void LifecycleServer::Create(async_dispatcher_t* dispatcher, ShutdownCallback shutdown,
                             fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> request) {
  if (!request.is_valid()) {
    FX_LOGS(INFO)
        << "Invalid handle for lifecycle events, assuming test environment and continuing";
    return;
  }
  fidl::BindServer(dispatcher, std::move(request),
                   std::make_unique<LifecycleServer>(std::move(shutdown)));
}

void LifecycleServer::Stop(StopCompleter::Sync& completer) {
  FX_LOGS(INFO) << "received shutdown command over lifecycle interface";
  shutdown_([completer = completer.ToAsync()](zx_status_t status) mutable {
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "shutdown failed: " << zx_status_get_string(status);
    } else {
      FX_LOGS(INFO) << "shutdown complete";
    }
    completer.Close(status);
  });
}

}  // namespace f2fs
