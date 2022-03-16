// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/fuzzing/common/runner.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include "src/sys/fuzzing/common/status.h"

namespace fuzzing {
namespace {

// This enum is used with |Pend| to schedule specific actions.
enum Action : uint8_t {
  kExecute,
  kMinimize,
  kCleanse,
  kFuzz,
  kMerge,
  kIdle,
  kStop,
};

}  // namespace

Runner::Runner(ExecutorPtr executor)
    : executor_(executor),
      action_(kIdle),
      monitors_(executor),
      close_([this]() { CloseImpl(); }),
      interrupt_([this] { InterruptImpl(); }),
      join_([this]() { JoinImpl(); }) {
  worker_ = std::thread([this]() { Worker(); });
}

Runner::~Runner() {
  close_.Run();
  interrupt_.Run();
  join_.Run();
}

zx_status_t Runner::Configure(const OptionsPtr& options) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!idle_) {
    FX_LOGS(WARNING) << "Attempted to re-configure when fuzzing already in progress.";
    return ZX_ERR_BAD_STATE;
  }
  ConfigureImpl(options);
  return ZX_OK;
}

///////////////////////////////////////////////////////////////
// Worker methods

void Runner::Pend(uint8_t action, Input input, fit::function<void(zx_status_t)> callback) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (action_ == kStop) {
    FX_LOGS(WARNING) << "Attempted to perform action when engine is stopping.";
    callback(ZX_ERR_BAD_STATE);
    return;
  }
  if (!idle_) {
    FX_LOGS(WARNING) << "Attempted to perform action when fuzzing already in progress.";
    callback(ZX_ERR_BAD_STATE);
    return;
  }
  idle_ = false;
  action_ = action;
  input_ = std::move(input);
  callback_ = std::move(callback);
  worker_sync_.Signal();
}

ZxPromise<> Runner::PendAsync(uint8_t action, Input&& input) {
  ZxBridge<> bridge;
  Pend(action, std::move(input),
       [completer = std::move(bridge.completer)](zx_status_t status) mutable {
         if (status != ZX_OK) {
           completer.complete_error(status);
           return;
         }
         completer.complete_ok();
       });
  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_CANCELED));
}

ZxPromise<FuzzResult> Runner::Execute(Input input) {
  return PendAsync(kExecute, std::move(input))
      .and_then([this]() { return fpromise::ok(result_); })
      .wrap_with(scope_);
}

ZxPromise<Input> Runner::Minimize(Input input) {
  return PendAsync(kMinimize, std::move(input))
      .and_then([this]() { return fpromise::ok(result_input_.Duplicate()); })
      .wrap_with(scope_);
}

ZxPromise<Input> Runner::Cleanse(Input input) {
  return PendAsync(kCleanse, std::move(input))
      .and_then([this]() { return fpromise::ok(result_input_.Duplicate()); })
      .wrap_with(scope_);
}

ZxPromise<Artifact> Runner::Fuzz() {
  Input input;
  return PendAsync(kFuzz, std::move(input))
      .and_then([this]() { return fpromise::ok(Artifact(result_, result_input_.Duplicate())); })
      .wrap_with(scope_);
}

void Runner::Merge(fit::function<void(zx_status_t)> callback) {
  Input input;
  Pend(kMerge, std::move(input), std::move(callback));
}

void Runner::Worker() {
  while (true) {
    // Wait indefinitely. Destroying this object will call |StopImpl|.
    worker_sync_.WaitFor("more work to do");
    worker_sync_.Reset();
    uint8_t action;
    Input input;
    fit::function<void(zx_status_t)> callback;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (action_ == kStop) {
        return;
      }
      action = action_;
      input = std::move(input_);
      callback = std::move(callback_);
    }
    zx_status_t status = ZX_OK;
    ClearErrors();
    switch (action) {
      case kExecute:
        status = SyncExecute(input);
        break;
      case kMinimize:
        status = SyncMinimize(input);
        break;
      case kCleanse:
        status = SyncCleanse(input);
        break;
      case kFuzz:
        status = SyncFuzz();
        break;
      case kMerge:
        status = SyncMerge();
        break;
      default:
        FX_NOTREACHED();
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      callback(status);
      idle_ = true;
    }
  }
}

///////////////////////////////////////////////////////////////
// Status-related methods.

void Runner::AddMonitor(fidl::InterfaceHandle<Monitor> monitor) {
  monitors_.Add(std::move(monitor));
}

void Runner::UpdateMonitors(UpdateReason reason) {
  monitors_.set_status(CollectStatus());
  monitors_.Update(reason);
}

void Runner::ClearErrors() {
  result_ = FuzzResult::NO_ERRORS;
  result_input_.Clear();
}

///////////////////////////////////////////////////////////////
// Stop-related methods.

void Runner::CloseImpl() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    idle_ = false;
    action_ = kStop;
    worker_sync_.Signal();
  }
}

void Runner::InterruptImpl() {
  // no-op in the base class.
}

void Runner::JoinImpl() {
  FX_DCHECK(worker_.joinable());
  worker_.join();
}

}  // namespace fuzzing
