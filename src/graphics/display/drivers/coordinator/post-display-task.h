// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_POST_DISPLAY_TASK_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_POST_DISPLAY_TASK_H_

#include <lib/async/dispatcher.h>
#include <lib/zx/result.h>

#include <cstddef>
#include <utility>

#include "src/graphics/display/drivers/coordinator/post-task.h"

namespace display {

// The maximum capacity for display tasks.
//
// The coordinator only uses this value as the `inline_target_size` argument for
// `PostTask()` and `PostTaskState`. Using a single value trades off a bit of
// dynamic memory consumption for a smaller binary size.
constexpr size_t kDisplayTaskTargetSize = 56;

using DisplayTaskState = PostTaskState<kDisplayTaskTargetSize>;

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_POST_DISPLAY_TASK_H_
