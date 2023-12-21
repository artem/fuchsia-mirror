// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/ipc/protocol.h"

#include <lib/syslog/cpp/macros.h>

namespace debug_ipc {

const char* MsgHeader::TypeToString(MsgHeader::Type type) {
  switch (type) {
    case Type::kNone:
      return "None";
#define FN(type)                 \
  case MsgHeader::Type::k##type: \
    return #type;
      FOR_EACH_REQUEST_TYPE(FN)
      FOR_EACH_NOTIFICATION_TYPE(FN)
#undef FN
  }
  return "<Unknown>";
}

const char* NotifyIO::TypeToString(Type type) {
  switch (type) {
    case Type::kStderr:
      return "Stderr";
    case Type::kStdout:
      return "Stdout";
    case Type::kLast:
      return "Last";
  }

  FX_NOTREACHED();
  return "<invalid>";
}

const char* ResumeRequest::HowToString(How how) {
  switch (how) {
    case How::kResolveAndContinue:
      return "Resolve and Continue";
    case How::kForwardAndContinue:
      return "Forward and Continue";
    case How::kStepInstruction:
      return "Step Instruction";
    case How::kStepInRange:
      return "Step In Range";
    case How::kLast:
      return "Last";
  }

  FX_NOTREACHED();
  return "<unknown>";
}

const char* NotifyProcessStarting::TypeToString(Type type) {
  // clang-format off
  switch (type) {
    case Type::kAttach: return "Attach";
    case Type::kLaunch: return "Launch";
    case Type::kLimbo: return "Limbo";
    case Type::kLast: return "<last>";
  }
  // clang-format on

  FX_NOTREACHED();
  return "<unknown>";
}

}  // namespace debug_ipc
