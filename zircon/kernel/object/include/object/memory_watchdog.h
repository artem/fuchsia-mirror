// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MEMORY_WATCHDOG_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MEMORY_WATCHDOG_H_

#include <zircon/boot/crash-reason.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <object/diagnostics.h>
#include <object/event_dispatcher.h>
#include <object/job_dispatcher.h>
#include <object/port_dispatcher.h>

class Executor;

// Object is thread safe.
class MemoryWatchdog {
 public:
  enum PressureLevel : uint8_t {
    kOutOfMemory = 0,
    kImminentOutOfMemory,
    kCritical,
    kWarning,
    kNormal,
    kNumLevels,
  };

  // Init must be called before any other methods.
  void Init(Executor* executor);

  fbl::RefPtr<EventDispatcher> GetMemPressureEvent(uint32_t kind);

  uint64_t DebugNumBytesTillPressureLevel(PressureLevel level);
  void Dump();

 private:
  // The callback provided to the |eviction_trigger_| timer.
  static void EvictionTriggerCallback(Timer* timer, zx_time_t now, void* arg);
  void EvictionTrigger();

  void WorkerThread() __NO_RETURN;

  // Helper called by the WorkerThread when OOM conditions are hit.
  void OnOom();

  // Called by the WorkerThread to determine if a kernel event needs to be signaled corresponding to
  // pressure change to level |idx|.
  inline bool IsSignalDue(PressureLevel idx, zx_time_t time_now) const;

  // Called by the WorkerThread to determine if kernel eviction (asynchronous) needs to be triggered
  // in response to pressure change to level |idx|.
  inline bool IsEvictionRequired(PressureLevel idx) const;

  void WaitForMemChange(const Deadline& deadline);
  ktl::pair<uint64_t, uint64_t> FreeMemBoundsForLevel(PressureLevel level) const;

  PressureLevel CalculatePressureLevel() const;

  // Kernel-owned events used to signal userspace at different levels of memory pressure.
  ktl::array<fbl::RefPtr<EventDispatcher>, PressureLevel::kNumLevels> mem_pressure_events_;

  // Event used for communicating memory state between the mem_avail_state_updated_cb callback and
  // the WorkerThread.
  AutounsignalEvent mem_state_signal_;

  // Relaxed atomic so that debug methods can safely read it.
  RelaxedAtomic<PressureLevel> mem_event_idx_ = PressureLevel::kNormal;
  PressureLevel prev_mem_event_idx_ = mem_event_idx_;

  // Watermark information is not modified after Init and so is safe for multiple threads to access.
  static constexpr uint8_t kNumWatermarks = PressureLevel::kNumLevels - 1;
  ktl::array<uint64_t, kNumWatermarks> mem_watermarks_;
  uint64_t watermark_debounce_ = 0;

  // Used to delay signaling memory level transitions in the case of rapid changes.
  zx_time_t hysteresis_seconds_ = ZX_SEC(10);

  // Tracks last time the memory state was evaluated (and signaled if required).
  zx_time_t prev_mem_state_eval_time_ = ZX_TIME_INFINITE_PAST;

  // The highest pressure level we trigger eviction at, OOM being the lowest pressure level (0).
  PressureLevel max_eviction_level_ = PressureLevel::kCritical;

  // The free memory target to aim for when we trigger eviction.
  uint64_t free_mem_target_ = 0;

  // Current minimum amount of memory we want the triggered eviction to reclaim.
  uint64_t min_free_target_ = 0;

  // A timer is used to trigger eviction so that user space is given a chance to act upon a memory
  // pressure signal first.
  Timer eviction_trigger_;

  // OneShot eviction strategy only triggers eviction events at memory pressure state transitions.
  // Continuous eviction strategy enables continuous background eviction as long the system remains
  // under memory pressure (i.e. at a memory pressure level that is eligible for eviction).
  enum EvictionStrategy : uint8_t { OneShot, Continuous };
  EvictionStrategy eviction_strategy_ = EvictionStrategy::OneShot;

  Executor* executor_;
};

MemoryWatchdog& GetMemoryWatchdog();

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_MEMORY_WATCHDOG_H_
