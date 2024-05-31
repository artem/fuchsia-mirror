// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/debuglog.h>
#include <lib/zircon-internal/macros.h>

#include <object/executor.h>
#include <object/memory_watchdog.h>
#include <platform/halt_helper.h>
#include <platform/halt_token.h>
#include <pretty/cpp/sizes.h>
#include <vm/scanner.h>

using pretty::FormattedBytes;

namespace {

KCOUNTER(pressure_level_oom, "memory_watchdog.pressure.oom")
KCOUNTER(pressure_level_imminent_oom, "memory_watchdog.pressure.imminent_oom")
KCOUNTER(pressure_level_critical, "memory_watchdog.pressure.critical")
KCOUNTER(pressure_level_warning, "memory_watchdog.pressure.warning")
KCOUNTER(pressure_level_normal, "memory_watchdog.pressure.normal")

void CountPressureEvent(MemoryWatchdog::PressureLevel level) {
  switch (level) {
    case MemoryWatchdog::PressureLevel::kOutOfMemory:
      pressure_level_oom.Add(1);
      break;
    case MemoryWatchdog::PressureLevel::kImminentOutOfMemory:
      pressure_level_imminent_oom.Add(1);
      break;
    case MemoryWatchdog::PressureLevel::kCritical:
      pressure_level_critical.Add(1);
      break;
    case MemoryWatchdog::PressureLevel::kWarning:
      pressure_level_warning.Add(1);
      break;
    case MemoryWatchdog::PressureLevel::kNormal:
      pressure_level_normal.Add(1);
      break;
    default:
      break;
  }
}

const char* PressureLevelToString(MemoryWatchdog::PressureLevel level) {
  switch (level) {
    case MemoryWatchdog::PressureLevel::kOutOfMemory:
      return "OutOfMemory";
    case MemoryWatchdog::PressureLevel::kImminentOutOfMemory:
      return "ImminentOutOfMemory";
    case MemoryWatchdog::PressureLevel::kCritical:
      return "Critical";
    case MemoryWatchdog::PressureLevel::kWarning:
      return "Warning";
    case MemoryWatchdog::PressureLevel::kNormal:
      return "Normal";
    default:
      return "Unknown";
  }
}

void HandleOnOomReboot() {
  // Notify the pmm that although we are out of memory, we would like to never wait for memory.
  // This ensures that if userspace needs to allocate to do a graceful shutdown it is able to.
  pmm_stop_returning_should_wait();

  if (!HaltToken::Get().Take()) {
    // We failed to acquire the token.  Someone else must have it.  That's OK.  We'll rely on them
    // to halt/reboot.  Nothing left for us to do but wait.
    printf("memory-pressure: halt/reboot already in progress; sleeping forever\n");
    Thread::Current::Sleep(ZX_TIME_INFINITE);
  }
  // We now have the halt token so we're committed.  To ensure we record the true cause of the
  // reboot, we must ensure nothing (aside from a panic) prevents us from halting with reason OOM.

  // We are out of or nearly out of memory so future attempts to allocate may fail.  From this
  // point on, avoid performing any allocation.  Establish a "no allocation allowed" scope to
  // detect (assert) if we attempt to allocate.
  ScopedMemoryAllocationDisabled allocation_disabled;

  printf("memory-pressure: pausing for %ums after OOM mem signal\n", gBootOptions->oom_timeout_ms);
  zx_status_t status =
      HaltToken::Get().WaitForAck(Deadline::after(ZX_MSEC(gBootOptions->oom_timeout_ms)));

  switch (status) {
    case ZX_OK:
      printf("memory-pressure: rebooting due to OOM. received user-mode acknowledgement.\n");
      break;

    case ZX_ERR_TIMED_OUT:
      // User mode code should have acked by now, since it hasn't, reboot the system.
      printf(
          "memory-pressure: rebooting due to OOM. timed out after waiting %ums for user-mode "
          "ack.\n",
          gBootOptions->oom_timeout_ms);
      break;

    default:
      printf(
          "memory-pressure: rebooting due to OOM. unexpected error while waiting for user-mode "
          "acknowledgement (status %d).\n",
          status);
      break;
  }

  // Tell the oom_tests host test that we are about to generate an OOM
  // crashlog to keep it happy.  Without these messages present in a
  // specific order in the log, the test will fail.
  printf("memory-pressure: stowing crashlog\nZIRCON REBOOT REASON (OOM)\n");

  // The debuglog could contain diagnostic messages that would assist in debugging the cause of
  // the OOM.  Shutdown debuglog before rebooting in order to flush any queued messages.
  //
  // It is important that we don't hang during this process so set a deadline for the debuglog
  // to shutdown.
  //
  // How long should we wait?  Shutting down the debuglog includes flushing any buffered
  // messages to the serial port (if present).  Writing to a serial port can be slow.  Assuming
  // we have a full debuglog buffer of 128KB, at 115200 bps, with 8-N-1, it will take roughly
  // 11.4 seconds to drain the buffer.  The timeout should be long enough to allow a full DLOG
  // buffer to be drained.
  zx_time_t deadline = current_time() + ZX_SEC(20);
  status = dlog_shutdown(deadline);
  if (status != ZX_OK) {
    // If `dlog_shutdown` failed, there's not much we can do besides print an error (which
    // probably won't make it out anyway since we've already called `dlog_shutdown`) and
    // continue on to `platform_halt`.
    printf("ERROR: dlog_shutdown failed: %d\n", status);
  }
  platform_halt(HALT_ACTION_REBOOT, ZirconCrashReason::Oom);
}

}  // namespace

fbl::RefPtr<EventDispatcher> MemoryWatchdog::GetMemPressureEvent(uint32_t kind) {
  switch (kind) {
    case ZX_SYSTEM_EVENT_OUT_OF_MEMORY:
      return mem_pressure_events_[PressureLevel::kOutOfMemory];
    case ZX_SYSTEM_EVENT_IMMINENT_OUT_OF_MEMORY:
      return mem_pressure_events_[PressureLevel::kImminentOutOfMemory];
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_CRITICAL:
      return mem_pressure_events_[PressureLevel::kCritical];
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_WARNING:
      return mem_pressure_events_[PressureLevel::kWarning];
    case ZX_SYSTEM_EVENT_MEMORY_PRESSURE_NORMAL:
      return mem_pressure_events_[PressureLevel::kNormal];
    default:
      return nullptr;
  }
}

void MemoryWatchdog::EvictionTriggerCallback(Timer* timer, zx_time_t now, void* arg) {
  MemoryWatchdog* watchdog = reinterpret_cast<MemoryWatchdog*>(arg);
  watchdog->EvictionTrigger();
}

void MemoryWatchdog::EvictionTrigger() {
  // This runs from a timer interrupt context, as such we do not want to be performing synchronous
  // eviction and blocking some random thread. Therefore we use the asynchronous eviction trigger
  // that will cause the eviction thread to perform the actual eviction work.
  if (eviction_strategy_ == EvictionStrategy::Continuous) {
    pmm_evictor()->EnableContinuousEviction(min_free_target_, free_mem_target_,
                                            Evictor::EvictionLevel::OnlyOldest,
                                            Evictor::Output::Print);
  } else {
    pmm_evictor()->EvictOneShotAsynchronous(min_free_target_, free_mem_target_,
                                            Evictor::EvictionLevel::OnlyOldest,
                                            Evictor::Output::Print);
  }
}

// Helper called by the memory pressure thread when OOM state is entered.
void MemoryWatchdog::OnOom() {
  switch (gBootOptions->oom_behavior) {
    case OomBehavior::kJobKill:
      if (!executor_->GetRootJobDispatcher()->KillJobWithKillOnOOM()) {
        printf("memory-pressure: no alive job has a kill bit\n");
      }

      // Since killing is asynchronous, sleep for a short period for the system to quiesce. This
      // prevents us from rapidly killing more jobs than necessary. And if we don't find a
      // killable job, don't just spin since the next iteration probably won't find a one either.
      Thread::Current::SleepRelative(ZX_MSEC(500));
      break;

    case OomBehavior::kReboot:
      HandleOnOomReboot();
  }
}

bool MemoryWatchdog::IsSignalDue(PressureLevel idx, zx_time_t time_now) const {
  // We signal a memory state change immediately if any of these conditions are met:
  // 1) The current index is lower than the previous one signaled (i.e. available memory is lower
  // now), so that clients can act on the signal quickly.
  // 2) |hysteresis_seconds_| have elapsed since the last time we examined the state.
  return idx < prev_mem_event_idx_ ||
         zx_time_sub_time(time_now, prev_mem_state_eval_time_) >= hysteresis_seconds_;
}

bool MemoryWatchdog::IsEvictionRequired(PressureLevel idx) const {
  // Trigger asynchronous eviction if:
  // 1) the memory availability state is more critical than the previous one
  // AND
  // 2) we're configured to evict at that level.
  //
  // Do not trigger asynchronous eviction at the OOM level, as we have already performed synchronous
  // eviction to attempt a quick recovery before reaching here. At this point we are about to signal
  // filesystems to shut down on OOM, after which eviction will be a no-op anyway, since there will
  // no longer be any pager-backed memory to evict.
  return idx < prev_mem_event_idx_ && idx <= max_eviction_level_ &&
         idx != PressureLevel::kOutOfMemory;
}

void MemoryWatchdog::WorkerThread() {
  while (true) {
    // If we've hit OOM level perform some immediate synchronous eviction to attempt to avoid OOM.
    if (mem_event_idx_ == PressureLevel::kOutOfMemory) {
      CountPressureEvent(mem_event_idx_);
      printf("memory-pressure: free memory is %zuMB, evicting pages to prevent OOM...\n",
             pmm_count_free_pages() * PAGE_SIZE / MB);
      pmm_page_queues()->Dump();
      // Keep trying to perform eviction for as long as we are evicting non-zero pages and we remain
      // in the out of memory state.
      while (mem_event_idx_ == PressureLevel::kOutOfMemory) {
        uint64_t evicted_pages = pmm_evictor()->EvictOneShotSynchronous(
            MB * 10, Evictor::EvictionLevel::IncludeNewest, Evictor::Output::Print,
            Evictor::TriggerReason::OOM);
        if (evicted_pages == 0) {
          printf("memory-pressure: found no pages to evict\n");
          break;
        }
        mem_event_idx_ = CalculatePressureLevel();
      }
      printf("memory-pressure: free memory after OOM eviction is %zuMB\n",
             pmm_count_free_pages() * PAGE_SIZE / MB);
      pmm_page_queues()->Dump();
      PageQueues::ReclaimCounts pager_counts = pmm_page_queues()->GetReclaimQueueCounts();
      printf(
          "memory-pressure: reclaimable working set immediately after OOM eviction is: "
          "total: %zu MiB newest: %zu MiB oldest: %zu MiB\n",
          pager_counts.total * PAGE_SIZE / MB, pager_counts.newest * PAGE_SIZE / MB,
          pager_counts.oldest * PAGE_SIZE / MB);
      pmm_print_physical_page_borrowing_stats();
    }

    // Check to see if the PMM has failed any allocations.  If the PMM has ever failed to allocate
    // because it was out of memory, then escalate the pressure level to trigger an OOM response
    // immediately.  The idea here is that usermode processes may not be able to handle allocation
    // failure and therefore could have become wedged in some way.
    if (gBootOptions->oom_trigger_on_alloc_failure && pmm_has_alloc_failed_no_mem()) {
      printf("memory-pressure: pmm failed one or more alloc calls, escalating to oom...\n");
      mem_event_idx_ = PressureLevel::kOutOfMemory;
    }

    auto time_now = current_time();

    if (IsSignalDue(mem_event_idx_, time_now)) {
      CountPressureEvent(mem_event_idx_);
      printf("memory-pressure: memory availability state - %s\n",
             PressureLevelToString(mem_event_idx_));
      pmm_page_queues()->Dump();

      if (IsEvictionRequired(mem_event_idx_)) {
        // Clear any previous eviction trigger. Once Cancel completes we know that we will not race
        // with the callback and are free to update the targets. Cancel will return true if the
        // timer was canceled before it was scheduled on a cpu, i.e. an eviction was outstanding.
        bool eviction_was_outstanding = eviction_trigger_.Cancel();

        const uint64_t free_mem = pmm_count_free_pages() * PAGE_SIZE;
        // Set the minimum amount to free as half the amount required to reach our desired free
        // memory level. This minimum ensures that even if the user reduces memory in reaction to
        // this signal we will always attempt to free a bit.
        // TODO: measure and fine tune this over time as user space evolves.
        min_free_target_ = free_mem < free_mem_target_ ? (free_mem_target_ - free_mem) / 2 : 0;

        // If eviction was outstanding when we canceled the eviction trigger, trigger eviction
        // immediately without any delay. We are here because of a rapid allocation spike which
        // caused the memory pressure to become more critical in a very short interval, so it might
        // be better to evict pages as soon as possible to try and counter the allocation spike.
        // Otherwise if eviction was not outstanding, trigger the eviction for slightly in the
        // future. Half the hysteresis time here is a balance between giving user space time to
        // release memory and the eviction running before the end of the hysteresis period.
        eviction_trigger_.SetOneshot(
            (eviction_was_outstanding ? time_now
                                      : zx_time_add_duration(time_now, hysteresis_seconds_ / 2)),
            EvictionTriggerCallback, this);
        printf("memory-pressure: set target memory to evict %zuMB (free memory is %zuMB)\n",
               min_free_target_ / MB, free_mem / MB);
      } else if (eviction_strategy_ == EvictionStrategy::Continuous &&
                 mem_event_idx_ > max_eviction_level_) {
        // If we're out of the max configured eviction-eligible memory pressure level, disable
        // continuous eviction.

        // Cancel any outstanding eviction trigger, so that eviction is not accidentally enabled
        // *after* we disable it here.
        eviction_trigger_.Cancel();
        // Disable continuous eviction.
        pmm_evictor()->DisableContinuousEviction();
      }

      // Unsignal the last event that was signaled.
      zx_status_t status =
          mem_pressure_events_[prev_mem_event_idx_]->user_signal_self(ZX_EVENT_SIGNALED, 0);
      if (status != ZX_OK) {
        panic("memory-pressure: unsignal memory event %s failed: %d\n",
              PressureLevelToString(prev_mem_event_idx_), status);
      }

      // Signal event corresponding to the new memory state.
      status = mem_pressure_events_[mem_event_idx_]->user_signal_self(0, ZX_EVENT_SIGNALED);
      if (status != ZX_OK) {
        panic("memory-pressure: signal memory event %s failed: %d\n",
              PressureLevelToString(mem_event_idx_), status);
      }
      prev_mem_event_idx_ = mem_event_idx_;
      prev_mem_state_eval_time_ = time_now;

      // If we're below the out-of-memory watermark, trigger OOM behavior.
      if (mem_event_idx_ == PressureLevel::kOutOfMemory) {
        pmm_page_queues()->Dump();
        OnOom();
      }

      // Wait for the memory state to change again.
      WaitForMemChange(Deadline::infinite());

    } else {
      prev_mem_state_eval_time_ = time_now;

      // We are ignoring this memory state transition. Wait for only |hysteresis_seconds_|, and then
      // re-evaluate the memory state. Otherwise we could remain stuck at the lower memory state if
      // mem_state_signal_ is not signaled.
      WaitForMemChange(Deadline::no_slack(zx_time_add_duration(time_now, hysteresis_seconds_)));
    }
  }
}

void MemoryWatchdog::WaitForMemChange(const Deadline& deadline) {
  const PressureLevel prev = mem_event_idx_;
  zx_status_t status = ZX_OK;
  int iterations = 0;
  do {
    if (iterations++ == 10) {
      printf("WARNING: %s has iterated %d times, possible bug", __FUNCTION__, iterations);
    }
    auto [lower, upper] = FreeMemBoundsForLevel(mem_event_idx_);
    const uint64_t delay_alloc_level =
        mem_event_idx_ == PressureLevel::kOutOfMemory
            ? UINT64_MAX
            : (mem_watermarks_[PressureLevel::kOutOfMemory] - watermark_debounce_) / PAGE_SIZE;
    if (pmm_set_free_memory_signal(lower / PAGE_SIZE, upper / PAGE_SIZE, delay_alloc_level,
                                   &mem_state_signal_)) {
      // After having successfully set the event check again for any allocation failures. This is to
      // ensure that if an allocation failure happened while we did not have an event set that it is
      // not missed.
      if (gBootOptions->oom_trigger_on_alloc_failure && pmm_has_alloc_failed_no_mem()) {
        return;
      }
      status = mem_state_signal_.Wait(deadline);
    } else {
      // Setting failed, must've raced. Fall through and compute the new pressure level.
    }
    mem_event_idx_ = CalculatePressureLevel();
    // In the case where we raced with additional pmm actions keep looping unless the deadline was
    // reached.
  } while (mem_event_idx_ == prev && status == ZX_OK);
}

ktl::pair<uint64_t, uint64_t> MemoryWatchdog::FreeMemBoundsForLevel(PressureLevel level) const {
  // Calculate the range, including debounce, for the current memory level.
  uint64_t lower = 0;
  uint64_t upper = UINT64_MAX;
  if (level > PressureLevel::kOutOfMemory) {
    lower = mem_watermarks_[level - 1] - watermark_debounce_;
  }
  if (level < kNumWatermarks) {
    upper = mem_watermarks_[level] + watermark_debounce_;
  }
  return {lower, upper};
}

MemoryWatchdog::PressureLevel MemoryWatchdog::CalculatePressureLevel() const {
  // Get the bounds for the current level, this is inclusive of the debounce.
  auto [lower, upper] = FreeMemBoundsForLevel(mem_event_idx_);

  // Retrieve current free memory.
  const uint64_t free_mem = pmm_count_free_pages() * PAGE_SIZE;

  // Check if still inside the bounds.
  if (free_mem >= lower && free_mem <= upper) {
    // No change.
    return mem_event_idx_;
  }

  // Determine the new level using a simple O(N).
  uint8_t new_level = PressureLevel::kOutOfMemory;
  while (new_level < kNumWatermarks && free_mem > mem_watermarks_[new_level]) {
    new_level++;
  }
  DEBUG_ASSERT(new_level != mem_event_idx_);
  return static_cast<PressureLevel>(new_level);
}

uint64_t MemoryWatchdog::DebugNumBytesTillPressureLevel(PressureLevel level) {
  // Check we have been initialized.
  DEBUG_ASSERT(executor_);
  if (mem_event_idx_ <= level) {
    // Already in level, or in a state with less available memory than level
    return 0;
  }
  // We need to either get free_pages below mem_watermarks[level] or, if we are
  // in state (level + 1), we also need to clear the debounce amount. For simplicity we just
  // always allocate the debounce amount as well.
  uint64_t trigger = mem_watermarks_[level] - watermark_debounce_;
  uint64_t free_count = pmm_count_free_pages() * PAGE_SIZE;
  // Handle races in the current pressure level.
  if (free_count < trigger) {
    return 0;
  }
  return free_count - trigger;
}

void MemoryWatchdog::Dump() {
  printf("watermarks: [");
  for (uint8_t i = 0; i < kNumWatermarks; i++) {
    printf("%s%s", FormattedBytes(mem_watermarks_[i]).c_str(),
           i + 1 == kNumWatermarks ? "]\n" : ", ");
  }
  const PressureLevel current = mem_event_idx_;
  auto [lower, upper] = FreeMemBoundsForLevel(current);
  printf("debounce: %s\n", FormattedBytes(watermark_debounce_).c_str());
  printf("current state: %u\n", mem_event_idx_.load());
  printf("current bounds: [%s, %s]\n", FormattedBytes(lower).c_str(),
         FormattedBytes(upper).c_str());
  printf("free memory: %s\n", FormattedBytes(pmm_count_free_pages() * PAGE_SIZE).c_str());
}

void MemoryWatchdog::Init(Executor* executor) {
  DEBUG_ASSERT(executor_ == nullptr);

  executor_ = executor;

  for (uint8_t i = 0; i < PressureLevel::kNumLevels; i++) {
    auto level = PressureLevel(i);
    KernelHandle<EventDispatcher> event;
    zx_rights_t rights;
    zx_status_t status = EventDispatcher::Create(0, &event, &rights);
    if (status != ZX_OK) {
      panic("memory-pressure: create memory event %s failed: %d\n", PressureLevelToString(level),
            status);
    }
    mem_pressure_events_[i] = event.release();
  }

  if (gBootOptions->oom_enabled) {
    // TODO(rashaeqbal): The watermarks chosen below are arbitrary. Tune them based on memory usage
    // patterns. Consider moving to percentages of total memory instead of absolute numbers - will
    // be easier to maintain across platforms.
    mem_watermarks_[PressureLevel::kOutOfMemory] =
        (gBootOptions->oom_out_of_memory_threshold_mb) * MB;
    mem_watermarks_[PressureLevel::kImminentOutOfMemory] =
        mem_watermarks_[PressureLevel::kOutOfMemory] +
        (gBootOptions->oom_imminent_oom_delta_mb) * MB;
    mem_watermarks_[PressureLevel::kCritical] = (gBootOptions->oom_critical_threshold_mb) * MB;
    mem_watermarks_[PressureLevel::kWarning] = (gBootOptions->oom_warning_threshold_mb) * MB;

    watermark_debounce_ = gBootOptions->oom_debounce_mb * MB;
    if (gBootOptions->oom_evict_at_warning) {
      max_eviction_level_ = PressureLevel::kWarning;
    }

    // Validate our watermarks and debounce settings makes sense.
    for (uint8_t j = 0; j < kNumWatermarks; j++) {
      uint64_t prev = j == 0 ? 0 : mem_watermarks_[j - 1];
      uint64_t next = (j == kNumWatermarks - 1) ? UINT64_MAX : mem_watermarks_[j + 1];
      ASSERT(mem_watermarks_[j] > prev);
      ASSERT(mem_watermarks_[j] < next);
      ASSERT(mem_watermarks_[j] - prev > watermark_debounce_);
      ASSERT(next - mem_watermarks_[j] > watermark_debounce_);
    }

    // Set our eviction target to be such that we try to get completely out of the max eviction
    // level, taking into account the debounce.
    free_mem_target_ = mem_watermarks_[max_eviction_level_] + watermark_debounce_;

    hysteresis_seconds_ = ZX_SEC(gBootOptions->oom_hysteresis_seconds);

    printf(
        "memory-pressure: memory watermarks - OutOfMemory: %zuMB, Critical: %zuMB, Warning: %zuMB, "
        "Debounce: %zuMB\n",
        mem_watermarks_[PressureLevel::kOutOfMemory] / MB,
        mem_watermarks_[PressureLevel::kCritical] / MB,
        mem_watermarks_[PressureLevel::kWarning] / MB, watermark_debounce_ / MB);

    printf("memory-pressure: eviction trigger level - %s\n",
           PressureLevelToString(max_eviction_level_));

    if (gBootOptions->oom_evict_continuous) {
      eviction_strategy_ = EvictionStrategy::Continuous;
      printf("memory-pressure: eviction strategy - continuous\n");
    } else {
      eviction_strategy_ = EvictionStrategy::OneShot;
      printf("memory-pressure: eviction strategy - one-shot\n");
    }

    printf("memory-pressure: hysteresis interval - %ld seconds\n", hysteresis_seconds_ / ZX_SEC(1));

    printf("memory-pressure: ImminentOutOfMemory watermark - %zuMB\n",
           mem_watermarks_[PressureLevel::kImminentOutOfMemory] / MB);

    auto memory_worker_thread = [](void* arg) -> int {
      MemoryWatchdog* watchdog = reinterpret_cast<MemoryWatchdog*>(arg);
      watchdog->WorkerThread();
    };
    auto thread =
        Thread::Create("memory-pressure-thread", memory_worker_thread, this, HIGHEST_PRIORITY);
    DEBUG_ASSERT(thread);
    thread->Detach();
    thread->Resume();
  }
}
