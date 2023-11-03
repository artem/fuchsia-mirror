// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/unwind.h"

#include "src/developer/debug/debug_agent/arch.h"
#include "src/developer/debug/debug_agent/general_registers.h"
#include "src/developer/debug/debug_agent/module_list.h"
#include "src/developer/debug/debug_agent/process_handle.h"
#include "src/developer/debug/debug_agent/thread_handle.h"
#include "src/developer/debug/ipc/unwinder_support.h"
#include "src/lib/unwinder/platform.h"
#include "src/lib/unwinder/unwind.h"

namespace debug_agent {

namespace {

using debug::RegisterID;

// The general registers include thread-specific information (fsbase/gsbase on x64, and tpidr on
// ARM64). The unwinders don't deal with these registers because unwinding shouldn't affect them.
// This function copies the current platform's thread-specific registers to the stack frame record
// under the assumption that they never change across stack frames.
void AddThreadRegs(const GeneralRegisters& source, debug_ipc::StackFrame* dest) {
  const auto& native_regs = source.GetNativeRegisters();

#if defined(__x86_64__)
  // Linux defines fs_base and gs_base as "unsigned long long" which requires a cast to be
  // unambiguous given the many Register constructor variants.
  dest->regs.emplace_back(RegisterID::kX64_fsbase, static_cast<uint64_t>(native_regs.fs_base));
  dest->regs.emplace_back(RegisterID::kX64_gsbase, static_cast<uint64_t>(native_regs.gs_base));
#elif defined(__aarch64__)
  dest->regs.emplace_back(RegisterID::kARMv8_tpidr, native_regs.tpidr);
#else
#error Write for your platform
#endif
}

}  // namespace

zx_status_t UnwindStack(const ProcessHandle& process, const ModuleList& modules,
                        const ThreadHandle& thread, const GeneralRegisters& regs, size_t max_depth,
                        std::vector<debug_ipc::StackFrame>* stack) {
  // Prepare arguments for unwinder::Unwind.
#if defined(__Fuchsia__)
  unwinder::FuchsiaMemory memory(process.GetNativeHandle().get());
#elif defined(__linux__)
  unwinder::LinuxMemory memory(process.GetKoid());
#else
#error Need unwinder memory for this platform.
#endif

  std::vector<uint64_t> module_bases;
  module_bases.reserve(modules.modules().size());
  for (const auto& module : modules.modules()) {
    module_bases.push_back(module.base);
  }
  auto registers = unwinder::FromPlatformRegisters(regs.GetNativeRegisters());

  // Request one more frame for the CFA of the last frame.
  auto frames = unwinder::Unwind(&memory, module_bases, registers, max_depth + 1);

  // Convert from unwinder::Frame to debug_ipc::StackFrame.
  *stack = debug_ipc::ConvertFrames(frames);
  if (stack->size() > max_depth) {
    stack->resize(max_depth);
  }
  for (auto& frame : *stack) {
    AddThreadRegs(regs, &frame);
  }
  return ZX_OK;
}

}  // namespace debug_agent
