// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCENIC_TESTS_SCENIC_TEST_H_
#define SRC_UI_SCENIC_LIB_SCENIC_TESTS_SCENIC_TEST_H_

#include <lib/async-testing/test_loop.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/ui/scenic/cpp/session.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/ui/scenic/lib/scenic/scenic.h"
#include "src/ui/scenic/lib/scheduling/default_frame_scheduler.h"

namespace scenic_impl::test {

// Base class that can be specialized to configure a Scenic with the systems
// required for a set of tests.
class ScenicTest : public ::gtest::TestLoopFixture {
 public:
  ScenicTest() = default;
  ~ScenicTest() override = default;

  Scenic* scenic() { return scenic_.get(); }
  std::unique_ptr<::scenic::Session> CreateSession();

 protected:
  // ::testing::Test virtual method.
  void SetUp() override;

  // ::testing::Test virtual method.
  void TearDown() override;

  // Subclasses may override this to install any systems required by the test;
  // none are installed by default.
  virtual void InitializeScenic(std::shared_ptr<Scenic> scenic);

  std::unique_ptr<sys::ComponentContext> context_;
  std::unique_ptr<scheduling::DefaultFrameScheduler> frame_scheduler_;
  inspect::Node inspect_node_;
  std::shared_ptr<Scenic> scenic_;
};

}  // namespace scenic_impl::test

#endif  // SRC_UI_SCENIC_LIB_SCENIC_TESTS_SCENIC_TEST_H_
