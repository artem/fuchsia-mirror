// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>

#include <gtest/gtest.h>
#include <trace-reader/file_reader.h>

#include "src/performance/trace/tests/integration_test_utils.h"
#include "src/performance/trace/tests/run_test.h"

namespace tracing {
namespace test {

namespace {

const char kChildPath[] = "/pkg/bin/shared_provider_app";

// We don't enable all categories, we just need a kernel category we know we'll
// receive. Syscalls are a good choice. We also need the sched category to get
// syscall events (syscall enter/exit tracking requires thread tracking).
// And we also need irq events because syscalls are mapped to the "irq" group
// in the kernel.
// TODO(dje): This could use some cleanup.
const char kCategoriesArg[] = "--categories=" CATEGORY_NAME;

TEST(SharedProvider, IntegrationTest) {
  zx::job job{};  // -> default job
  std::vector<std::string> args{
      "record", kCategoriesArg,
      std::string("--output-file=") + kSpawnedTestTmpPath + "/" + kRelativeOutputFilePath,
      "--spawn", kChildPath};
  ASSERT_TRUE(RunTraceAndWait(job, args));

  size_t num_events;
  ASSERT_TRUE(VerifyTestEventsFromJson(std::string(kTestTmpPath) + "/" + kRelativeOutputFilePath,
                                       &num_events));
  FX_LOGS(DEBUG) << "Got " << num_events << " events";
}

}  // namespace

}  // namespace test
}  // namespace tracing
