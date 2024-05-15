// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/function.h>
#include <lib/zx/bti.h>
#include <lib/zx/time.h>
#include <threads.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

namespace display {

class TestBase : public testing::Test {
 public:
  TestBase() : loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  void SetUp() override;
  void TearDown() override;

  Controller* controller() { return tree_->coordinator_controller(); }
  fake_display::FakeDisplay* display() { return tree_->display(); }

  fidl::ClientEnd<fuchsia_sysmem2::Allocator> ConnectToSysmemAllocatorV2();
  const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& display_fidl();

  async_dispatcher_t* dispatcher() { return loop_.dispatcher(); }

  // Waits until `predicate` returns true.
  //
  // `predicate` will only be evaluated on `loop_`.
  //
  // Returns true if the last evaluation of`predicate` returned true. Returns
  // false when the predicate can no longer be evaluated, such as when the
  // loop is destroyed.
  bool PollUntilOnLoop(fit::function<bool()> predicate, zx::duration poll_interval = zx::msec(10));

 private:
  async::Loop loop_;
  thrd_t loop_thrd_ = 0;

  std::unique_ptr<FakeDisplayStack> tree_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_
