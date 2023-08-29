// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>

#include <gtest/gtest.h>
#include <trace-reader/file_reader.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/test/test_settings.h"
#include "src/performance/lib/perfmon/controller.h"
#include "src/performance/lib/test_utils/run_program.h"

const char kTracePath[] = "/pkg/bin/trace";

const char kDurationArg[] = "--duration=1";

// Note: /data is no longer large enough in qemu sessions
const char kOutputFile[] = "/tmp/test-trace.fxt";

#if defined(__x86_64__)
const char kCategoriesArg[] = "--categories=cpu:fixed:instructions_retired,cpu:tally";
const char kCategoryName[] = "cpu:perf";
const char kTestEventName[] = "instructions_retired";
#elif defined(__aarch64__)
const char kCategoriesArg[] = "--categories=cpu:fixed:cycle_counter,cpu:tally";
const char kCategoryName[] = "cpu:perf";
const char kTestEventName[] = "cycle_counter";
#else
const char kCategoriesArg[] = "";
const char kCategoryName[] = "";
const char kTestEventName[] = "";
#endif

TEST(CpuperfProvider, IntegrationTest) {
  if (!perfmon::Controller::IsSupported()) {
    FX_LOGS(INFO) << "Exiting, perfmon device not supported";
    return;
  }

  zx::job job;
  ASSERT_EQ(zx::job::create(*zx::job::default_job(), 0, &job), ZX_OK);

  zx::process child;
  std::vector<std::string> argv{kTracePath,     "record",
                                "--binary",     kDurationArg,
                                kCategoriesArg, std::string("--output-file=") + kOutputFile};
  ASSERT_EQ(tracing::test::SpawnProgram(job, argv, ZX_HANDLE_INVALID, &child), ZX_OK);

  int64_t return_code;
  ASSERT_TRUE(tracing::test::WaitAndGetReturnCode(argv[0], child, &return_code));
  EXPECT_EQ(return_code, 0);

  size_t record_count = 0;
  size_t event_count = 0;
  auto record_consumer = [&record_count, &event_count](trace::Record record) {
    ++record_count;
    if (record.type() == trace::RecordType::kEvent) {
      const trace::Record::Event& event = record.GetEvent();
      if (event.type() == trace::EventType::kCounter && event.category == kCategoryName &&
          event.name == kTestEventName) {
        ++event_count;
      }
    }
  };

  bool got_error = false;
  auto error_handler = [&got_error](fbl::String error) {
    FX_LOGS(ERROR) << "While reading records got error: " << error.c_str();
    got_error = true;
  };

  std::unique_ptr<trace::FileReader> reader;
  ASSERT_TRUE(trace::FileReader::Create(kOutputFile, std::move(record_consumer),
                                        std::move(error_handler), &reader));
  reader->ReadFile();
  ASSERT_FALSE(got_error);

  FX_LOGS(INFO) << "Got " << record_count << " records, " << event_count << " instructions";

  ASSERT_GT(event_count, 0u);
}

// Provide our own main so that --verbose,etc. are recognized.
int main(int argc, char** argv) {
  fxl::CommandLine cl = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetTestSettings(cl))
    return EXIT_FAILURE;
  testing::InitGoogleTest(&argc, argv);

  return RUN_ALL_TESTS();
}
