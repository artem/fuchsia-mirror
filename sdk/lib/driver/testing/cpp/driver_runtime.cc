// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdf/env.h>
#include <lib/zx/clock.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <latch>
#include <unordered_set>

namespace fdf_testing {

namespace {

thread_local DriverRuntime* g_instance = nullptr;

}  // namespace

namespace internal {

DriverRuntimeEnv::DriverRuntimeEnv() {
  zx_status_t status = fdf_env_start();
  ZX_ASSERT_MSG(ZX_OK == status, "Failed to initialize driver runtime env: %s",
                zx_status_get_string(status));
}

DriverRuntimeEnv::~DriverRuntimeEnv() {
  fdf_env_destroy_all_dispatchers();
  fdf_env_reset();
}

}  // namespace internal

DriverRuntime::DriverRuntime()
    : initial_thread_id(std::this_thread::get_id()),
      foreground_dispatcher_(fdf_internal::kDispatcherDefault) {
  ZX_ASSERT_MSG(g_instance == nullptr,
                "Cannot create more than one instance of DriverRuntime at a time.");
  g_instance = this;
}

DriverRuntime::~DriverRuntime() {
  AssertCurrentThreadIsInitialThread();
  ShutdownAllDispatchers(nullptr);
  g_instance = nullptr;
}

DriverRuntime* DriverRuntime::GetInstance() { return g_instance; }

fdf::UnownedSynchronizedDispatcher DriverRuntime::StartBackgroundDispatcher() {
  AssertCurrentThreadIsInitialThread();
  ZX_ASSERT_MSG(!is_shutdown_,
                "Cannot start any more dispatchers after calling ShutdownAllDispatchers.");

  fdf_internal::TestSynchronizedDispatcher& dispatcher =
      background_dispatchers_.emplace_back(fdf_internal::kDispatcherManaged);
  return dispatcher.driver_dispatcher().borrow();
}

void DriverRuntime::ShutdownAllDispatchers(fdf_dispatcher_t* dut_initial_dispatcher) {
  AssertCurrentThreadIsInitialThread();

  if (is_shutdown_) {
    fdf_testing_set_default_dispatcher(dut_initial_dispatcher);
    return;
  }

  std::unordered_set<const void*> dispatcher_owners;
  for (auto& owner : background_dispatchers_) {
    dispatcher_owners.insert(&owner);
  }
  dispatcher_owners.insert(&foreground_dispatcher_);

// TODO(https://fxbug.dev/336633211): Stop using std::latch or switch to C++20 before LLVM 20.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
  std::latch latch(static_cast<int64_t>(dispatcher_owners.size()));
#pragma clang diagnostic pop
  for (const void* owner : dispatcher_owners) {
    auto shutdown = std::make_unique<fdf_env::DriverShutdown>();
    auto shutdown_ptr = shutdown.get();
    auto shutdown_callback = [shutdown = std::move(shutdown), &latch](const void* shutdown_driver) {
      latch.count_down();
    };
    ZX_ASSERT(ZX_OK == shutdown_ptr->Begin(owner, std::move(shutdown_callback)));
  }

  while (!latch.try_wait()) {
    RunUntilIdleInternal();
    ResetQuit();
  }

  fdf_testing_set_default_dispatcher(dut_initial_dispatcher);
  is_shutdown_ = true;
}

void DriverRuntime::Run() {
  AssertCurrentThreadIsInitialThread();
  RunInternal();
  ResetQuit();
}

bool DriverRuntime::RunWithTimeout(zx::duration timeout) {
  AssertCurrentThreadIsInitialThread();
  // This cannot be a local variable because the delayed task below can execute
  // after this function returns.
  auto canceled = std::make_shared<bool>(false);
  bool timed_out = false;
  async::PostDelayedTask(
      foreground_dispatcher_.dispatcher(),
      [this, canceled, &timed_out] {
        if (*canceled) {
          return;
        }
        timed_out = true;
        Quit();
      },
      timeout);
  RunInternal();
  ResetQuit();
  // Another task can call Quit() on the message loop, which exits the
  // message loop before the delayed task executes, in which case |timed_out| is
  // still false here because the delayed task hasn't run yet.
  // Since the message loop isn't destroyed then (as it usually would after
  // Quit()), and presumably can be reused after this function returns we
  // still need to prevent the delayed task to quit it again at some later time
  // using the canceled pointer.
  if (!timed_out) {
    *canceled = true;
  }
  return timed_out;
}

void DriverRuntime::RunUntil(fit::function<bool()> condition, zx::duration step) {
  AssertCurrentThreadIsInitialThread();
  RunWithTimeoutOrUntil(std::move(condition), zx::duration::infinite(), step);
}

bool DriverRuntime::RunWithTimeoutOrUntil(fit::function<bool()> condition, zx::duration timeout,
                                          zx::duration step) {
  AssertCurrentThreadIsInitialThread();
  const zx::time timeout_deadline = zx::deadline_after(timeout);

  while (zx::clock::get_monotonic() < timeout_deadline) {
    if (condition()) {
      ResetQuit();
      return true;
    }

    if (step == zx::duration::infinite()) {
      // Performs a single unit of work, possibly blocking until there is work
      // to do or the timeout deadline arrives.
      RunInternal(timeout_deadline, true);
    } else {
      // Performs work until the step deadline arrives.
      RunWithTimeout(step);
    }
  }

  ResetQuit();
  return condition();
}

void DriverRuntime::RunUntilIdle() {
  AssertCurrentThreadIsInitialThread();
  RunUntilIdleInternal();
  ResetQuit();
}

void DriverRuntime::Quit() { fdf_testing_quit(); }

zx_status_t DriverRuntime::ResetQuit() { return fdf_testing_reset_quit(); }

fit::closure DriverRuntime::QuitClosure() {
  return []() { fdf_testing_quit(); };
}

zx_status_t DriverRuntime::RunUntilIdleInternal() {
  AssertCurrentThreadIsInitialThread();
  ZX_ASSERT_MSG(!is_shutdown_, "Cannot Run after calling ShutdownAllDispatchers.");
  return fdf_testing_run_until_idle();
}

zx_status_t DriverRuntime::RunInternal(zx::time deadline, bool once) {
  AssertCurrentThreadIsInitialThread();
  ZX_ASSERT_MSG(!is_shutdown_, "Cannot Run after calling ShutdownAllDispatchers.");
  return fdf_testing_run(deadline.get(), once);
}

void DriverRuntime::AssertCurrentThreadIsInitialThread() {
  ZX_ASSERT_MSG(std::this_thread::get_id() == initial_thread_id, "%s",
                "This class is thread-unsafe. Call only allowed from the main test thread.");
}

}  // namespace fdf_testing
