// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_SCHEDULER_PROFILE_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_SCHEDULER_PROFILE_H_

#include <zircon/types.h>

namespace internal {
zx_status_t connect_scheduler_profile_provider();

zx_status_t set_scheduler_profile_by_role(zx_handle_t thread, const char* role, size_t role_size);
}  // namespace internal

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_SCHEDULER_PROFILE_H_
