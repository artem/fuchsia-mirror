// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUGGED_THREAD_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUGGED_THREAD_H_

#include "src/developer/debug/debug_agent/arch.h"
#include "src/developer/debug/debug_agent/automation_handler.h"
#include "src/developer/debug/debug_agent/exception_handle.h"
#include "src/developer/debug/debug_agent/general_registers.h"
#include "src/developer/debug/debug_agent/thread_handle.h"
#include "src/developer/debug/debug_agent/time.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/lib/fxl/macros.h"
#include "src/lib/fxl/memory/ref_ptr.h"
#include "src/lib/fxl/memory/weak_ptr.h"

namespace debug_agent {

class DebugAgent;
class DebuggedProcess;
class ProcessBreakpoint;
class Watchpoint;

class DebuggedThread {
 public:
  DebuggedThread(DebugAgent*, DebuggedProcess* process, std::unique_ptr<ThreadHandle> handle);

  virtual ~DebuggedThread();

  const DebuggedProcess* process() const { return process_; }

  zx_koid_t koid() const { return thread_handle_->GetKoid(); }

  const ThreadHandle& thread_handle() const { return *thread_handle_; }
  ThreadHandle& thread_handle() { return *thread_handle_; }

  // Returns true if this thread is currently suspended from the perspective of the client. See
  // ClientSuspend().
  bool is_client_suspended() const { return client_suspend_handle_.get(); }

  // TODO(brettw) remove this and have all callers use thread_handle().
  NativeThreadHandle& handle() { return thread_handle_->GetNativeHandle(); }
  const NativeThreadHandle& handle() const { return thread_handle_->GetNativeHandle(); }

  ExceptionHandle* exception_handle() { return exception_handle_.get(); }
  const ExceptionHandle* exception_handle() const { return exception_handle_.get(); }
  void set_exception_handle(std::unique_ptr<ExceptionHandle> exception) {
    exception_handle_ = std::move(exception);
  }

  fxl::WeakPtr<DebuggedThread> GetWeakPtr();

  void OnException(std::unique_ptr<ExceptionHandle> exception_handle);

  // Resumes execution of the thread from the perspective of the client. The thread should currently
  // be in a stopped state. If it's not stopped from the client's perspective (client suspend or in
  // an exception), this will be ignored.
  void ClientResume(const debug_ipc::ResumeRequest& request);

  // Low-level resume the thread from an exception. Most callers will want ClientResume or
  // ResumeFromException(). Calling this will bypass single step requests and stepping over
  // breakpoint logic. Will be a no-op if the thread is not in an exception. This is public because
  // it needs to be called by the breakpoint code when stepping over breakpoints.
  void InternalResumeException();

  // Pauses execution of the thread.
  //
  //  - ClientSuspend() pauses from the perspective of the client. This means that the client will
  //    be in charge of resuming this thread. It does nothing if the thread is already suspended
  //    from the perspective of the client.
  //
  //  - InternalSuspend() pauses for internal users (like breakpoints being stepped over) and the
  //    thread will remain suspended as long as the returned SuspendHandle is alive.
  //
  // For the thread to be running, there must be no client suspension nor any SuspendHandles
  // live.
  //
  // Suspending happens asynchronously so the thread will not necessarily have stopped when this
  // returns. Set the |synchronous| flag for blocking on the suspended signal and make this call
  // block until the thread is suspended. See also ThreadHandle::WaitForSuspension().
  void ClientSuspend(bool synchronous = false);
  [[nodiscard]] std::unique_ptr<SuspendHandle> InternalSuspend(bool synchronous = false);

  // Pauses execution of the thread. Pausing happens asynchronously so the thread will not
  // necessarily have stopped when this returns. Set the |synchronous| flag for blocking on the
  // suspended signal and make this call block.
  //
  // Suspension is ref-counted on the thread. This is done by returning a suspend token that will
  // keep track of how many suspensions this thread has. As long as there is a valid one, the
  // thread will remain suspended.

  // The typical suspend deadline users should use when suspending from now.
  static TickTimePoint DefaultSuspendDeadline();

  // Fills the thread status record. If full_stack is set, a full backtrace will be generated,
  // otherwise a minimal one will be generated.
  //
  // If the optional registers is set, will contain the current registers of the thread. If null,
  // these will be fetched automatically (this is an optimization for cases where the caller has
  // already requested registers).
  debug_ipc::ThreadRecord GetThreadRecord(
      debug_ipc::ThreadRecord::StackAmount stack_amount,
      std::optional<GeneralRegisters> regs = std::nullopt) const;

  // Register reading and writing. The "write" command also returns the contents of the register
  // categories written do.
  std::vector<debug::RegisterValue> ReadRegisters(
      const std::vector<debug::RegisterCategory>& cats_to_get) const;
  std::vector<debug::RegisterValue> WriteRegisters(const std::vector<debug::RegisterValue>& regs);

  // Sends a notification to the client about the state of this thread.
  void SendThreadNotification() const;

  // Notification that a ProcessBreakpoint is about to be deleted.
  void WillDeleteProcessBreakpoint(ProcessBreakpoint* bp);

  bool in_exception() const { return !!exception_handle_; }

  bool stepping_over_breakpoint() const { return stepping_over_breakpoint_; }
  void set_stepping_over_breakpoint(bool so) { stepping_over_breakpoint_ = so; }

 private:
  enum class OnStop {
    kNotify,  // Send client notification like normal.
    kResume,  // The thread should be resumed from this exception.
  };

  // Resumes the thread according to the current run mode. This handles stepping over breakpoints
  // and will resolve any exceptions.
  void ResumeFromException();

  // Some of these need to update the general registers in response to handling the exception. These
  // ones take a non-const GeneralRegisters reference.
  void HandleSingleStep(debug_ipc::NotifyException*, const GeneralRegisters& regs);
  void HandleGeneralException(debug_ipc::NotifyException*, const GeneralRegisters& regs);
  void HandleSoftwareBreakpoint(debug_ipc::NotifyException*, GeneralRegisters& regs);
  void HandleHardwareBreakpoint(debug_ipc::NotifyException*, GeneralRegisters& regs);
  void HandleWatchpoint(debug_ipc::NotifyException*, const GeneralRegisters& regs);

  void SendExceptionNotification(debug_ipc::NotifyException*, const GeneralRegisters& regs);

  // Updates the registers and the thread state for hitting the breakpoint, and fills in the
  // given breakpoint array for all matches.
  OnStop UpdateForSoftwareBreakpoint(GeneralRegisters& regs,
                                     std::vector<debug_ipc::BreakpointStats>& hit_breakpoints,
                                     std::vector<debug_ipc::ThreadRecord>& other_affected_threads);

  // Handles an exception corresponding to a ProcessBreakpoint. All Breakpoints affected will have
  // their updated stats added to *hit_breakpoints.
  //
  // WARNING: The ProcessBreakpoint argument could be deleted in this call if it was a one-shot
  //          breakpoint, so it must not be used after this call.
  void UpdateForHitProcessBreakpoint(debug_ipc::BreakpointType exception_type,
                                     ProcessBreakpoint* process_breakpoint,
                                     std::vector<debug_ipc::BreakpointStats>& hit_breakpoints,
                                     std::vector<debug_ipc::ThreadRecord>& stopped_threads);

  // Returns true if there is a software breakpoint instruction at the given address.
  bool IsBreakpointInstructionAtAddress(uint64_t address) const;

  // Sets the current single step flag for the current run mode.
  void SetSingleStepForRunMode();

  std::unique_ptr<ThreadHandle> thread_handle_;

  DebugAgent* debug_agent_;   // Non-owning.
  DebuggedProcess* process_;  // Non-owning.

  // The main thing we're doing. Possibly overridden by stepping_over_breakpoint_.
  debug_ipc::ResumeRequest::How run_mode_ = debug_ipc::ResumeRequest::How::kResolveAndContinue;

  // When run_mode_ == kStepInstruction, the number of instructions to step. Must be larger than 0.
  uint64_t step_count_ = 1;

  // When run_mode_ == kStepInRange, this defines the range (end non-inclusive).
  uint64_t step_in_range_begin_ = 0;
  uint64_t step_in_range_end_ = 0;

  // The client doesn't have reference-counted suspends, just the current state. This suspend handle
  // is active when the thread should be suspended from the client's perspective. Debug agent code
  // suspending for its own purpose should maintain its own suspend handle.
  std::unique_ptr<SuspendHandle> client_suspend_handle_;

  // Active if the thread is currently on an exception.
  std::unique_ptr<ExceptionHandle> exception_handle_;

  // Indicates when we're single-stepping over a breakpoint. This is required because it's
  // internally generated and needs to override the run_mode_.
  bool stepping_over_breakpoint_ = false;

  // This can be set in two cases:
  // - When suspended after hitting a breakpoint, this will be the breakpoint
  //   that was hit.
  // - When single-stepping over a breakpoint, this will be the breakpoint
  //   being stepped over.
  ProcessBreakpoint* current_breakpoint_ = nullptr;

  fxl::WeakPtrFactory<DebuggedThread> weak_factory_;

  AutomationHandler automation_handler_;

  FXL_DISALLOW_COPY_AND_ASSIGN(DebuggedThread);
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_DEBUGGED_THREAD_H_
