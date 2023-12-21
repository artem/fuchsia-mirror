// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tracing.h"

#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/trace-provider/fdio_connect.h>
#include <lib/trace-provider/provider.h>
#include <zircon/status.h>

#include "src/devices/lib/log/log.h"

namespace internal {

static trace_provider_t* g_trace_provider;
static async_loop_t* g_loop;

zx_status_t start_trace_provider() {
  if (g_trace_provider != nullptr || g_loop != nullptr) {
    LOGF(ERROR, "Trace provider already started");
    return ZX_ERR_ALREADY_BOUND;
  }
  async_loop_t* loop;
  zx_status_t status = async_loop_create(&kAsyncLoopConfigNoAttachToCurrentThread, &loop);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to create async loop: %s", zx_status_get_string(status));
    return status;
  }

  status = async_loop_start_thread(loop, "driver_host-tracer", nullptr);
  if (status != ZX_OK) {
    async_loop_destroy(loop);
    LOGF(ERROR, "Failed to start thread for async loop: %s", zx_status_get_string(status));
    return status;
  }

  async_dispatcher_t* dispatcher = async_loop_get_dispatcher(loop);
  zx_handle_t to_service;
  status = trace_provider_connect_with_fdio(&to_service);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to connect to trace provider registry: %s", zx_status_get_string(status));
    return status;
  }
  trace_provider_t* trace_provider = trace_provider_create(to_service, dispatcher);
  if (!trace_provider) {
    async_loop_destroy(loop);
    LOGF(WARNING, "Failed to register trace provider");
    return ZX_ERR_INTERNAL;
  }

  g_trace_provider = trace_provider;
  g_loop = loop;

  // N.B. The registry has begun, but these things are async. TraceManager
  // may not even be running yet (and likely isn't).
  VLOGF(1, "Started trace provider");
  return ZX_OK;
}

void stop_trace_provider() {
  if (g_loop != nullptr) {
    async_loop_destroy(g_loop);
    g_loop = nullptr;
  }
  if (g_trace_provider != nullptr) {
    trace_provider_destroy(g_trace_provider);
    g_trace_provider = nullptr;
  }
}

}  // namespace internal
