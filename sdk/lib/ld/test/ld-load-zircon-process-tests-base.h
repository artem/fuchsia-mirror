// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_

#include <lib/ld/testing/test-processargs.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>

#include "ld-load-zircon-ldsvc-tests-base.h"

namespace ld::testing {

// This is the common base class for test fixtures to launch a Zircon process.
class LdLoadZirconProcessTestsBase : public LdLoadZirconLdsvcTestsBase {
 public:
  static constexpr int64_t kRunFailureForTrap = ZX_TASK_RETCODE_EXCEPTION_KILL;
  static constexpr int64_t kRunFailureForBadPointer = ZX_TASK_RETCODE_EXCEPTION_KILL;

  ~LdLoadZirconProcessTestsBase();

  const char* process_name() const;

 protected:
  const zx::process& process() const { return process_; }

  void set_process(zx::process process);

  int64_t Run(TestProcessArgs* bootstrap, std::optional<size_t> stack_size,
              const zx::thread& thread, uintptr_t entry, uintptr_t vdso_base,
              const zx::vmar& root_vmar);

  // Wait for the process to die and collect its exit code.
  int64_t Wait();

 private:
  zx::process process_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_
