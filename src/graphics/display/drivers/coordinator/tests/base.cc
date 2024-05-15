// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/tests/base.h"

#include <lib/async/cpp/task.h>
#include <zircon/compiler.h>
#include <zircon/status.h>

#include <memory>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/fake/fake-display.h"

namespace display {

void TestBase::SetUp() {
  std::shared_ptr<zx_device> mock_root = MockDevice::FakeRootParent();
  loop_.StartThread("display::TestBase::loop_", &loop_thrd_);
  auto sysmem =
      std::make_unique<GenericSysmemDeviceWrapper<sysmem_driver::Device>>(mock_root.get());
  static constexpr fake_display::FakeDisplayDeviceConfig kDeviceConfig = {
      .manual_vsync_trigger = true,
      .no_buffer_access = false,
  };
  tree_ =
      std::make_unique<FakeDisplayStack>(std::move(mock_root), std::move(sysmem), kDeviceConfig);
}

void TestBase::TearDown() {
  tree_->SyncShutdown();
  async::PostTask(loop_.dispatcher(), [this]() { loop_.Quit(); });

  // Wait for loop_.Quit() to execute.
  loop_.JoinThreads();

  tree_.reset();
}

bool TestBase::PollUntilOnLoop(fit::function<bool()> predicate, zx::duration poll_interval) {
  struct PredicateEvalState {
    // Immutable after construction.
    fit::function<bool()> predicate __TA_GUARDED(mutex);

    fbl::Mutex mutex;

    // Signaled after `result` has a value.
    fbl::ConditionVariable signal;

    // nullopt while waiting for the predicate to be evaluated.
    std::optional<bool> result __TA_GUARDED(mutex);
  };
  PredicateEvalState predicate_eval_state = {
      .predicate = std::move(predicate),
  };

  while (true) {
    auto post_task_state = std::make_unique<DisplayTaskState>();

    {
      fbl::AutoLock lock(&predicate_eval_state.mutex);
      predicate_eval_state.result = std::nullopt;
    }

    // Retaining `predicate_eval_state` by reference is safe because this method
    // will block on a ConditionVariable::Wait() call, which is only fired at
    // the end of the task handler.
    zx::result post_task_result =
        PostTask(std::move(post_task_state), *loop_.dispatcher(), [&predicate_eval_state]() {
          fbl::AutoLock lock(&predicate_eval_state.mutex);

          predicate_eval_state.result = predicate_eval_state.predicate();

          // Once `result` is set to true above, PollUntilOnLoop() may return as
          // soon as it acquires `mutex`. So, `predicate_eval_state` can only be
          // used while `mutex` remains held.
          //
          // This means that we must call ConditionVariable::Signal() while
          // holding `mutex`.
          predicate_eval_state.signal.Signal();
        });
    if (post_task_result.is_error()) {
      zxlogf(ERROR, "Failed to post task: %s", post_task_result.status_string());
      return false;
    }

    {
      fbl::AutoLock lock(&predicate_eval_state.mutex);
      while (!predicate_eval_state.result.has_value()) {
        predicate_eval_state.signal.Wait(&predicate_eval_state.mutex);
      }
      if (predicate_eval_state.result.value()) {
        return true;
      }
    }

    zx_status_t sleep_status = zx::nanosleep(zx::deadline_after(poll_interval));
    if (sleep_status != ZX_OK) {
      zxlogf(ERROR, "Failed to sleep: %s", zx_status_get_string(sleep_status));
      return false;
    }
  }
}

fidl::ClientEnd<fuchsia_sysmem2::Allocator> TestBase::ConnectToSysmemAllocatorV2() {
  return tree_->ConnectToSysmemAllocatorV2();
}
const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& TestBase::display_fidl() {
  return tree_->display_client();
}

}  // namespace display
