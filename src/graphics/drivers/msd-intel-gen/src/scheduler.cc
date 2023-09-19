// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "scheduler.h"

#include <lib/magma/platform/platform_trace.h>

#include "msd_intel_connection.h"
#include "msd_intel_context.h"

class FifoScheduler : public Scheduler {
 public:
  FifoScheduler() {}

  void CommandBufferQueued(std::weak_ptr<MsdIntelContext> context) override;
  void CommandBufferCompleted(std::shared_ptr<MsdIntelContext> context) override;

  std::shared_ptr<MsdIntelContext> ScheduleContext() override;

 private:
  std::queue<std::weak_ptr<MsdIntelContext>> fifo_;
  std::shared_ptr<MsdIntelContext> current_context_;
  uint32_t current_count_{};
};

void FifoScheduler::CommandBufferQueued(std::weak_ptr<MsdIntelContext> context) {
  fifo_.push(context);
}

std::shared_ptr<MsdIntelContext> FifoScheduler::ScheduleContext() {
  std::shared_ptr<MsdIntelContext> context;

  while (!context) {
    if (fifo_.empty())
      return nullptr;

    context = fifo_.front().lock();
    if (!context) {
      fifo_.pop();
      continue;
    }

    if (context->killed()) {
      DLOG("context killed");
      fifo_.pop();
      context = nullptr;
    }
  }

  if (current_context_ == nullptr || current_context_ == context) {
    if (current_context_ == nullptr) {
      auto connection = context->connection().lock();
      uint64_t ATTRIBUTE_UNUSED current_id = connection ? connection->client_id() : 0;
      uint64_t ATTRIBUTE_UNUSED current_ticks = magma::PlatformTrace::GetCurrentTicks();

      TRACE_VTHREAD_DURATION_BEGIN("magma", "Context Exec", "GPU", current_id, current_ticks, "id",
                                   current_id);
    }

    fifo_.pop();
    current_context_ = context;
    current_count_++;
    return context;
  }

  return nullptr;
}

void FifoScheduler::CommandBufferCompleted(std::shared_ptr<MsdIntelContext> context) {
  DASSERT(current_count_);
  if (--current_count_ == 0) {
    auto connection = context->connection().lock();
    uint64_t ATTRIBUTE_UNUSED current_id = connection ? connection->client_id() : 0;
    uint64_t ATTRIBUTE_UNUSED current_ticks = magma::PlatformTrace::GetCurrentTicks();

    TRACE_VTHREAD_DURATION_END("magma", "Context Exec", "GPU", current_id, current_ticks, "id",
                               current_id);
    current_context_.reset();
  }
}

//////////////////////////////////////////////////////////////////////////////////////////////////

std::unique_ptr<Scheduler> Scheduler::CreateFifoScheduler() {
  return std::make_unique<FifoScheduler>();
}
