// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_MAIN_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_MAIN_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/zx/result.h>

#include "src/devices/bin/driver_manager/driver_manager_config.h"

// Get the root job from the root job service.
zx::result<zx::job> get_root_job();

// Get the mexec resource from the mexec resource service. Not receiving the
// startup handle is logged, but not fatal.  In test environments, it would not
// be present.
zx::result<zx::resource> get_mexec_resource();

int RunDfv2(driver_manager_config::Config config);

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_MAIN_H_
