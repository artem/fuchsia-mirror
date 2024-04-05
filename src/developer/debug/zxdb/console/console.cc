// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/console.h"

#include "src/developer/debug/zxdb/console/async_output_buffer.h"
#include "src/developer/debug/zxdb/console/command_parser.h"
#include "src/developer/debug/zxdb/console/output_buffer.h"

namespace zxdb {

Console* Console::singleton_ = nullptr;

Console::Console(Session* session) : context_(session), weak_factory_(this) {
  FX_DCHECK(!singleton_);
  singleton_ = this;
  debug::LogBackend::Set(this, false);
}

Console::~Console() {
  FX_DCHECK(singleton_ == this);
  singleton_ = nullptr;
  debug::LogBackend::Unset();

  // Clear backpointers bound with the callbacks for any pending async buffers.
  for (auto& pair : async_output_)
    pair.second->SetCompletionCallback({});
}

fxl::WeakPtr<Console> Console::GetWeakPtr() { return weak_factory_.GetWeakPtr(); }

void Console::Output(const OutputBuffer& output, bool add_newline) {
  if (OutputEnabled()) {
    Write(output, add_newline);
  }
}

void Console::Output(const std::string& s) {
  OutputBuffer buffer;
  buffer.Append(s);
  Output(buffer);
}

void Console::Output(const Err& err) {
  OutputBuffer buffer;
  buffer.Append(err);
  Output(buffer);
}

void Console::Output(fxl::RefPtr<AsyncOutputBuffer> output) {
  if (output->is_complete()) {
    // Synchronously available.
    Output(output->DestructiveFlatten());
  } else {
    // Listen for completion.
    //
    // Binds |this|. On our destruction we'll clear all the callbacks to prevent dangling pointers
    // for anything not completed yet.
    AsyncOutputBuffer* output_ptr = output.get();
    output->SetCompletionCallback([this, output_ptr]() {
      Output(output_ptr->DestructiveFlatten());

      auto found = async_output_.find(output_ptr);
      FX_DCHECK(found != async_output_.end());
      async_output_.erase(found);
    });
    async_output_[output_ptr] = std::move(output);
  }
}

void Console::Stream(const OutputBuffer& output) {
  if (StreamingEnabled()) {
    Write(output, false);
  }
}

bool Console::OutputEnabled() { return output_enabled_ > 0; }

void Console::EnableOutput() { ++output_enabled_; }

void Console::DisableOutput() { --output_enabled_; }

void Console::EnableStreaming() { ++streaming_enabled_; }

void Console::DisableStreaming() { --streaming_enabled_; }

void Console::WriteLog(debug::LogSeverity severity, const debug::FileLineFunction& location,
                       std::string log) {
  Syntax syntax;
  switch (severity) {
    case debug::LogSeverity::kInfo:
      syntax = Syntax::kComment;
      break;
    case debug::LogSeverity::kWarn:
      syntax = Syntax::kWarning;
      break;
    case debug::LogSeverity::kError:
      syntax = Syntax::kError;
      break;
  }
  Output(OutputBuffer(syntax, std::move(log)));
}

}  // namespace zxdb
