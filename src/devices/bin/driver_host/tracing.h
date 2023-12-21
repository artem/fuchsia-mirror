// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_TRACING_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_TRACING_H_

#include <zircon/types.h>

namespace internal {
// Register the driver_host as a "trace provider" with the trace manager.
// This method is *NOT* thread-safe.
zx_status_t start_trace_provider();

// Unregister the driver_host as a "trace provider" with the trace manager.
// This method is *NOT* thread-safe.
void stop_trace_provider();

}

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_TRACING_H_
