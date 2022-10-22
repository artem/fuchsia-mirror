// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/expr/vm_exec.h"

#include <lib/syslog/cpp/macros.h>

#include <optional>
#include <stack>
#include <vector>

#include "src/developer/debug/zxdb/common/ref_ptr_to.h"
#include "src/developer/debug/zxdb/expr/cast.h"
#include "src/developer/debug/zxdb/expr/eval_operators.h"
#include "src/developer/debug/zxdb/expr/resolve_ptr_ref.h"
#include "src/lib/fxl/memory/ref_counted.h"

namespace zxdb {

namespace {

// Identifies how each operation can complete.
enum class Completion {
  kSync,   // Operation completed synchronously and execution can continue.
  kAsync,  // Operation will complete asynchronously via a subsequent call to Exec().
  kError,  // Operation completed asynchronously and an error was reported (nothing more to do).
           // By convention, if code returns this value, it should have already set the error
           // message.
};

// This class wants to run everything sequentially until an asynchronous operation happens. It
// needs to integrate with the rest of the expression system which takes EvalCallbacks that can
// execute synchronously (from within the current stack frame) or asynchronously (from the message
// loop in the future).
//
// To bridge these two models, this struct is created as a shared_ptr so the caller and the callback
// (created by MakeContinueCallback()) can communicate about how the callback is issued.
//
// The "current" state starts off as synchronous. If the callback is executed during this time,
// the callback will set the "issued" state to kSync or kError.
//
// When the caller completes, it calls SynchronousDone() to indicate the end of the synchronous
// phase of the callback. This returns what the loop should do based on whether the callback
// was already issued or not.
struct CallbackInfo {
  // Indicates the current execution completion kind.
  Completion current = Completion::kSync;

  // Set to the "current" state or error when the callback is issued.
  std::optional<Completion> issued;

  Completion SynchronousDone() {
    // Any callbacks issued from here on will be "asynchronous".
    FX_DCHECK(current == Completion::kSync);
    current = Completion::kAsync;

    if (issued) {
      // Callback has been issued already. Since we just finished the synchronous phase, it should
      // have been marked as an error or synchronous completion.
      FX_DCHECK(*issued != Completion::kAsync);
      return *issued;
    }

    // Callback hasn't been issued yet, it must be async completion in the future.
    return Completion::kAsync;
  }
};

// Holds the machine state for a running bytecode program.
//
// This is a simple stack-based machine where the various operations operate on the value stack
// stored in stack_.
//
// A great book on this topic is "Crafting Interpreters" by Robert Nystrom.
class VmExecState : public fxl::RefCountedThreadSafe<VmExecState> {
 public:
  VmExecState(const fxl::RefPtr<EvalContext>& eval_context, VmStream stream, EvalCallback cb)
      : eval_context_(eval_context), stream_(std::move(stream)), cb_(std::move(cb)) {}

  static void Exec(fxl::RefPtr<VmExecState> state);

 private:
  // Executes one operation.
  Completion ExecOp(const VmOp& op);

  Completion ExecError(const VmOp& op);
  Completion ExecUnary(const VmOp& op);
  Completion ExecBinary(const VmOp& op);
  Completion ExecExpandRef(const VmOp& op);
  Completion ExecDrop(const VmOp& op);
  Completion ExecDup(const VmOp& op);
  Completion ExecLiteral(const VmOp& op);
  Completion ExecJump(const VmOp& op);
  Completion ExecJumpIfFalse(const VmOp& op);
  Completion ExecCallback0(const VmOp& op);
  Completion ExecCallback1(const VmOp& op);
  Completion ExecCallback2(const VmOp& op);
  Completion ExecCallbackN(const VmOp& op);
  Completion ExecAsyncCallback0(const VmOp& op);
  Completion ExecAsyncCallback1(const VmOp& op);
  Completion ExecAsyncCallback2(const VmOp& op);
  Completion ExecAsyncCallbackN(const VmOp& op);

  // Pushes the given value to the stack.
  void Push(ExprValue v);

  // Pops the top stack value and puts it in *popped. Returns either kSync or kError depending on
  // whether the value could be popped.
  Completion Pop(ExprValue* popped);

  void ReportDone(ErrOrValue result);

  // Issues the callback with the given error message.
  //
  // Always returns Completion::kError (for convenience so one can do:
  //
  //   return ReportError("...");
  //
  Completion ReportError(const std::string& msg);
  Completion ReportError(const Err& err);

  // See CallbackInfo above.
  EvalCallback MakeContinueCallback(std::shared_ptr<CallbackInfo> cb_info);

  fxl::RefPtr<EvalContext> eval_context_;
  VmStream stream_;
  EvalCallback cb_;

  size_t stream_index_ = 0;

  std::stack<ExprValue, std::vector<ExprValue>> stack_;
};

// static
void VmExecState::Exec(fxl::RefPtr<VmExecState> state) {
  while (state->stream_index_ < state->stream_.size()) {
    const auto& op = state->stream_[state->stream_index_];
    state->stream_index_++;  // Advance to next instruction.

    switch (state->ExecOp(op)) {
      case Completion::kSync:
        continue;
      case Completion::kAsync:
        // Exec() will be called back in the future to resume.
        return;
      case Completion::kError:
        // Error callback should already have been issued.
        FX_DCHECK(!state->cb_);
        return;
    }
  }

  // Successful completion.
  if (state->stack_.empty()) {
    // Every operation should push a value on the stack, so an empty stack should only happen for
    // empty programs.
    FX_DCHECK(state->stream_.empty());
    state->ReportDone(ExprValue());
  } else {
    // Correct programs should have only one result.
    FX_DCHECK(state->stack_.size() == 1u);
    state->ReportDone(state->stack_.top());
  }
}

Completion VmExecState::ExecOp(const VmOp& op) {
  switch (op.op) {
    // clang-format off
    case VmOpType::kError:          return ExecError(op);
    case VmOpType::kUnary:          return ExecUnary(op);
    case VmOpType::kBinary:         return ExecBinary(op);
    case VmOpType::kExpandRef:      return ExecExpandRef(op);
    case VmOpType::kDrop:           return ExecDrop(op);
    case VmOpType::kDup:            return ExecDup(op);
    case VmOpType::kLiteral:        return ExecLiteral(op);
    case VmOpType::kJump:           return ExecJump(op);
    case VmOpType::kJumpIfFalse:    return ExecJumpIfFalse(op);
    case VmOpType::kCallback0:      return ExecCallback0(op);
    case VmOpType::kCallback1:      return ExecCallback1(op);
    case VmOpType::kCallback2:      return ExecCallback2(op);
    case VmOpType::kCallbackN:      return ExecCallbackN(op);
    case VmOpType::kAsyncCallback0: return ExecAsyncCallback0(op);
    case VmOpType::kAsyncCallback1: return ExecAsyncCallback1(op);
    case VmOpType::kAsyncCallback2: return ExecAsyncCallback2(op);
    case VmOpType::kAsyncCallbackN: return ExecAsyncCallbackN(op);
      // clang-format on

    case VmOpType::kLast:
      FX_DCHECK(false);  // Shouldn't reach.
      return Completion::kSync;
  }
}

Completion VmExecState::ExecError(const VmOp& op) {
  // The error is optional because this instruction is used both to throw explicit errors and to
  // indicate an uninitialized operation.
  if (std::holds_alternative<Err>(op.info))
    return ReportError(std::get<Err>(op.info));
  return ReportError("Invalid bytecode operation.");
}

Completion VmExecState::ExecUnary(const VmOp& op) {
  ExprValue param;
  if (Pop(&param) == Completion::kError)
    return Completion::kError;

  auto cb_info = std::make_shared<CallbackInfo>();
  EvalUnaryOperator(eval_context_, op.token, param, MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecBinary(const VmOp& op) {
  // The "left" side expression on the binary operator will be pushed on the stack first, leaving
  // the "right" side at the top of the stack to pop first when we execute the operator.
  ExprValue right_param;
  if (Pop(&right_param) == Completion::kError)
    return Completion::kError;

  ExprValue left_param;
  if (Pop(&left_param) == Completion::kError)
    return Completion::kError;

  auto cb_info = std::make_shared<CallbackInfo>();
  EvalBinaryOperator(eval_context_, left_param, op.token, right_param,
                     MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecExpandRef(const VmOp& op) {
  ExprValue param;
  if (Pop(&param) == Completion::kError)
    return Completion::kError;

  // This is executed a lot. It may be worth checking if it's a reference in-place and otherwise
  // just continuing without all of the completion callback dance.
  auto cb_info = std::make_shared<CallbackInfo>();
  EnsureResolveReference(eval_context_, std::move(param), MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecDrop(const VmOp& op) {
  ExprValue popped;
  return Pop(&popped);
}

Completion VmExecState::ExecDup(const VmOp& op) {
  if (stack_.empty())
    return ReportError("VM stack underflow in 'dup' operation.");
  stack_.push(stack_.top());
  return Completion::kSync;
}

Completion VmExecState::ExecLiteral(const VmOp& op) {
  const auto& literal_info = std::get<VmOp::LiteralInfo>(op.info);
  Push(literal_info.value);
  return Completion::kSync;
}

Completion VmExecState::ExecJump(const VmOp& op) {
  const auto& jump_info = std::get<VmOp::JumpInfo>(op.info);
  FX_DCHECK(jump_info.dest != VmOp::kBadJumpDest);
  stream_index_ = jump_info.dest;
  return Completion::kSync;
}

Completion VmExecState::ExecJumpIfFalse(const VmOp& op) {
  ExprValue param;
  if (Pop(&param) == Completion::kError)
    return Completion::kError;

  ErrOr<bool> result = CastNumericExprValueToBool(eval_context_, param);
  if (result.has_error())
    return ReportError(result.err());

  if (!result.value()) {
    // Take jump.
    const auto& jump_info = std::get<VmOp::JumpInfo>(op.info);
    FX_DCHECK(jump_info.dest != VmOp::kBadJumpDest);
    stream_index_ = jump_info.dest;
  }
  // Otherwise just continue at next instruction.
  return Completion::kSync;
}

Completion VmExecState::ExecCallback0(const VmOp& op) {
  const auto& cb = std::get<VmOp::Callback0>(op.info);

  ErrOrValue result = cb(eval_context_);
  if (result.has_error())
    return ReportError(result.err());

  Push(result.value());
  return Completion::kSync;
}

Completion VmExecState::ExecCallback1(const VmOp& op) {
  const auto& cb = std::get<VmOp::Callback1>(op.info);

  ExprValue param;
  if (Pop(&param) == Completion::kError)
    return Completion::kError;

  ErrOrValue result = cb(eval_context_, std::move(param));
  if (result.has_error())
    return ReportError(result.err());

  Push(result.value());
  return Completion::kSync;
}

Completion VmExecState::ExecCallback2(const VmOp& op) {
  const auto& cb = std::get<VmOp::Callback2>(op.info);

  ExprValue param2;
  if (Pop(&param2) == Completion::kError)
    return Completion::kError;

  ExprValue param1;
  if (Pop(&param1) == Completion::kError)
    return Completion::kError;

  ErrOrValue result = cb(eval_context_, std::move(param1), std::move(param2));
  if (result.has_error())
    return ReportError(result.err());

  Push(result.value());
  return Completion::kSync;
}

Completion VmExecState::ExecCallbackN(const VmOp& op) {
  const auto& info = std::get<VmOp::CallbackNInfo>(op.info);

  std::vector<ExprValue> params;
  params.resize(info.num_params);
  for (int i = info.num_params - 1; i >= 0; i--) {
    if (Pop(&params[i]) == Completion::kError)
      return Completion::kError;
  }

  ErrOrValue result = info.cb(eval_context_, std::move(params));
  if (result.has_error())
    return ReportError(result.err());

  Push(result.value());
  return Completion::kSync;
}

Completion VmExecState::ExecAsyncCallback0(const VmOp& op) {
  const auto& cb = std::get<VmOp::AsyncCallback0>(op.info);

  auto cb_info = std::make_shared<CallbackInfo>();
  cb(eval_context_, MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecAsyncCallback1(const VmOp& op) {
  const auto& cb = std::get<VmOp::AsyncCallback1>(op.info);

  ExprValue param;
  if (Pop(&param) == Completion::kError)
    return Completion::kError;

  auto cb_info = std::make_shared<CallbackInfo>();
  cb(eval_context_, std::move(param), MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecAsyncCallback2(const VmOp& op) {
  const auto& cb = std::get<VmOp::AsyncCallback2>(op.info);

  ExprValue param2;
  if (Pop(&param2) == Completion::kError)
    return Completion::kError;

  ExprValue param1;
  if (Pop(&param1) == Completion::kError)
    return Completion::kError;

  auto cb_info = std::make_shared<CallbackInfo>();
  cb(eval_context_, std::move(param1), std::move(param2), MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

Completion VmExecState::ExecAsyncCallbackN(const VmOp& op) {
  const auto& info = std::get<VmOp::AsyncCallbackNInfo>(op.info);

  std::vector<ExprValue> params;
  params.resize(info.num_params);
  for (int i = info.num_params - 1; i >= 0; i--) {
    if (Pop(&params[i]) == Completion::kError)
      return Completion::kError;
  }

  auto cb_info = std::make_shared<CallbackInfo>();
  info.cb(eval_context_, std::move(params), MakeContinueCallback(cb_info));
  return cb_info->SynchronousDone();
}

void VmExecState::Push(ExprValue v) { stack_.push(std::move(v)); }

Completion VmExecState::Pop(ExprValue* popped) {
  if (stack_.empty()) {
    // TODO report source of error.
    return ReportError("Stack underflow");
  }
  *popped = std::move(stack_.top());
  stack_.pop();
  return Completion::kSync;
}

void VmExecState::ReportDone(ErrOrValue result) {
  cb_(std::move(result));
  cb_ = {};  // Clear out to catch accidental re-use.
}

Completion VmExecState::ReportError(const std::string& msg) { return ReportError(Err(msg)); }

Completion VmExecState::ReportError(const Err& err) {
  ReportDone(err);
  return Completion::kError;
}

EvalCallback VmExecState::MakeContinueCallback(std::shared_ptr<CallbackInfo> cb_info) {
  return [state = RefPtrTo(this), cb_info = std::move(cb_info)](ErrOrValue result) {
    if (result.has_error()) {
      cb_info->issued = Completion::kError;
      state->ReportDone(std::move(result));
      return;
    }

    // Mark this as complete.
    cb_info->issued = cb_info->current;

    state->Push(result.take_value());
    if (cb_info->current == Completion::kAsync) {
      // Need to explicitly continue evaluation.
      VmExecState::Exec(std::move(state));
    }
    // In the synchronous case, the caller will just resume.
  };
}

}  // namespace

void VmExec(const fxl::RefPtr<EvalContext>& eval_context, VmStream stream, EvalCallback cb) {
  VmExecState::Exec(
      fxl::MakeRefCounted<VmExecState>(eval_context, std::move(stream), std::move(cb)));
}

}  // namespace zxdb
