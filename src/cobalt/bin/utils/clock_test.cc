// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/cobalt/bin/utils/clock.h"

#include <fuchsia/time/cpp/fidl.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/clock.h>

#include <future>
#include <optional>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "lib/fidl/cpp/binding_set.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace cobalt {

using inspect::testing::BoolIs;
using inspect::testing::ChildrenMatch;
using inspect::testing::IntIs;
using inspect::testing::NameMatches;
using inspect::testing::NodeMatches;
using inspect::testing::PropertyList;
using ::testing::UnorderedElementsAre;

class FuchsiaSystemClockTest : public ::gtest::TestLoopFixture {
 public:
  ~FuchsiaSystemClockTest() override = default;

 protected:
  void SetUp() override {
    EXPECT_EQ(ZX_OK, zx::clock::create(0, nullptr, &zircon_clock_));
    clock_.reset(new FuchsiaSystemClock(
        dispatcher(), inspector_.GetRoot().CreateChild("system_clock"), zircon_clock_.borrow()));
  }

  void TearDown() override { clock_.reset(); }

  void SignalClockStarted() {
    zx::clock::update_args args;
    args.set_value(zx::time(3000));
    EXPECT_EQ(ZX_OK, zircon_clock_.update(args));
  }

  void SignalClockSynced() {
    if (const zx_status_t status =
            zircon_clock_.signal(/*clear_mask=*/0,
                                 /*set_mask=*/fuchsia::time::SIGNAL_UTC_CLOCK_SYNCHRONIZED);
        status != ZX_OK) {
      FX_PLOGS(FATAL, status) << "Failed to sync clock";
    }
  }

  zx::clock zircon_clock_;
  inspect::Inspector inspector_;
  std::unique_ptr<FuchsiaSystemClock> clock_;
};

TEST_F(FuchsiaSystemClockTest, AwaitExternalSourceNotTriggeredOnClockReady) {
  SignalClockStarted();

  bool called = false;
  clock_->AwaitExternalSource([&called]() { called = true; });
  RunLoopUntilIdle();

  EXPECT_FALSE(called);
}

TEST_F(FuchsiaSystemClockTest, AwaitExternalSourceInitiallyAccurate) {
  SignalClockSynced();

  bool called = false;
  clock_->AwaitExternalSource([&called]() { called = true; });
  RunLoopUntilIdle();

  EXPECT_TRUE(called);
}

TEST_F(FuchsiaSystemClockTest, AwaitExternalSourceInitiallyInaccurate) {
  bool called = false;
  clock_->AwaitExternalSource([&called]() { called = true; });
  RunLoopUntilIdle();

  EXPECT_FALSE(called);

  SignalClockSynced();
  RunLoopUntilIdle();

  EXPECT_TRUE(called);

  fpromise::result<inspect::Hierarchy> result = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  ASSERT_TRUE(result.is_ok());
  EXPECT_THAT(
      result.take_value(),
      AllOf(NodeMatches(NameMatches("root")),
            ChildrenMatch(UnorderedElementsAre(NodeMatches(AllOf(
                NameMatches("system_clock"),
                PropertyList(UnorderedElementsAre(IntIs("start_waiting_time", testing::Gt(0)),
                                                  IntIs("clock_accurate_time", testing::Gt(0)),
                                                  BoolIs("is_accurate", true)))))))));
}

TEST_F(FuchsiaSystemClockTest, NowBeforeInitialized) { EXPECT_EQ(clock_->now(), std::nullopt); }

TEST_F(FuchsiaSystemClockTest, NowAfterInitialized) {
  SignalClockSynced();

  clock_->AwaitExternalSource([]() {});

  RunLoopUntilIdle();

  EXPECT_NE(clock_->now(), std::nullopt);
}

// Tests our use of an atomic_bool. We can set |accurate_| true in
// one thread and read it as true in another thread.
TEST_F(FuchsiaSystemClockTest, NowFromAnotherThread) {
  SignalClockSynced();
  clock_->AwaitExternalSource([]() {});

  RunLoopUntilIdle();

  EXPECT_NE(std::async(std::launch::async, [this]() { return clock_->now(); }).get(), std::nullopt);
}

}  // namespace cobalt
