// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/unwind.h"

#include <lib/zx/process.h>
#include <lib/zx/suspend_token.h>

#include <condition_variable>
#include <thread>

#include <gtest/gtest.h>

#include "src/developer/debug/debug_agent/module_list.h"
#include "src/developer/debug/debug_agent/zircon_process_handle.h"
#include "src/developer/debug/debug_agent/zircon_thread_handle.h"

namespace debug_agent {

namespace {

// This would be simpler using a mutex instead of the condition variable since there are only two
// threads, but the Clang lock checker gets very upset.
struct ThreadData {
  std::mutex mutex;

  // Set by thread itself before thread_ready is signaled. zx::thread::native_handle doesn't seem to
  // do what we want.
  std::unique_ptr<ThreadHandle> thread;

  bool thread_ready = false;
  std::condition_variable thread_ready_cv;

  bool backtrace_done = false;
  std::condition_variable backtrace_done_cv;
};

void __attribute__((noinline)) ThreadFunc2(ThreadData* data) {
  // Tell the main thread we're ready for backtrace computation.
  std::unique_lock<std::mutex> lock(data->mutex);
  data->thread_ready = true;
  data->thread_ready_cv.notify_one();

  // Block until the backtrace is done being completed.
  if (!data->backtrace_done) {
    data->backtrace_done_cv.wait(lock, [data]() { return data->backtrace_done; });
  }
}

void __attribute__((noinline)) ThreadFunc1(ThreadData* data) {
  // Fill in our thread handle.
  zx::thread handle;
  zx::thread::self()->duplicate(ZX_RIGHT_SAME_RIGHTS, &handle);

  // Here we use fake koids for the process and thread because those aren't necessary for the test.
  data->thread = std::make_unique<ZirconThreadHandle>(std::move(handle));

  // Put another function on the stack.
  ThreadFunc2(data);

  // This doesn't do anything useful but we need some code the compiler can't remove after the
  // ThreadFunc2 call to ensure the compiler doesn't optimize out the return.
  data->thread_ready_cv.notify_one();
}

// Synchronously suspends the thread. Returns a valid suspend token on success.
std::unique_ptr<SuspendHandle> SyncSuspendThread(ThreadHandle& thread) {
  auto suspend_handle = thread.Suspend();

  // Need long timeout when running on shared bots on QEMU.
  zx_signals_t observed = 0;
  zx_status_t status = thread.GetNativeHandle().wait_one(
      ZX_THREAD_SUSPENDED, zx::deadline_after(zx::sec(10)), &observed);
  EXPECT_TRUE(observed & ZX_THREAD_SUSPENDED);
  if (status != ZX_OK)
    return nullptr;

  return suspend_handle;
}

bool FrameHasRegister(const debug_ipc::StackFrame& frame, debug::RegisterID id) {
  for (const auto& reg : frame.regs) {
    if (reg.id == id)
      return true;
  }
  return false;
}

// Returns true if the given stack frame has values for the thread-specific registers for the
// current platform. This does not validate the actual values.
bool FrameHasThreadRegisters(const debug_ipc::StackFrame& frame) {
#if defined(__x86_64__)
  return FrameHasRegister(frame, debug::RegisterID::kX64_fsbase) &&
         FrameHasRegister(frame, debug::RegisterID::kX64_gsbase);
#elif defined(__aarch64__)
  return FrameHasRegister(frame, debug::RegisterID::kARMv8_tpidr);
#else
#error Write for your platform
#endif
}

void DoUnwindTest() {
  zx::process handle;
  zx::process::self()->duplicate(ZX_RIGHT_SAME_RIGHTS, &handle);
  ZirconProcessHandle process(std::move(handle));

  ThreadData data;
  std::thread background(ThreadFunc1, &data);

  // Wait until the background thread is ready for the backtrace.
  std::vector<debug_ipc::StackFrame> stack;
  {
    std::unique_lock<std::mutex> lock(data.mutex);
    if (!data.thread_ready)
      data.thread_ready_cv.wait(lock, [&data]() { return data.thread_ready; });

    // Thread query functions require it to be suspended.
    auto suspend = SyncSuspendThread(*data.thread);

    // Get the registers for the unwinder.
    std::optional<GeneralRegisters> regs = data.thread->GetGeneralRegisters();
    ASSERT_TRUE(regs);

    // Find the module information.
    ModuleList modules;
    modules.Update(process);

    // Do the unwinding.
    zx_status_t status = UnwindStack(process, modules, *data.thread, *regs, 16, &stack);
    ASSERT_EQ(ZX_OK, status);

    data.backtrace_done = true;
  }

  // Tell the background thread it can complete.
  data.backtrace_done_cv.notify_one();
  background.join();

  // Validate the stack. It's really hard to say what these values will be without symbols given the
  // few guarantees C++ can provide. But we should have "several" entries, and each one should have
  // "a bunch" of registers.
  ASSERT_TRUE(stack.size() >= 3) << "Only got " << stack.size() << " stack entries";

  // Don't check the bottom stack frame because it sometimes has weird initial state.
  for (size_t i = 0; i < stack.size() - 1; i++) {
    EXPECT_TRUE(stack[i].ip != 0);
    EXPECT_TRUE(stack[i].regs.size() >= 8)
        << "Only got " << stack[i].regs.size() << " regs for frame " << i;

    // Each stack frame should have the thread-specific registers that don't change across frames.
    EXPECT_TRUE(FrameHasThreadRegisters(stack[i]));
  }

  // TODO: It might be nice to write the thread functions in assembly so we can know what the
  // addresses are supposed to be.
}

}  // namespace

TEST(Unwind, Unwind) { DoUnwindTest(); }

}  // namespace debug_agent
