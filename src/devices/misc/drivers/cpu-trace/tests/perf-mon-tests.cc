// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.perfmon.cpu/cpp/wire.h>
#include <lib/zircon-internal/device/cpu-trace/perf-mon.h>

#include <gtest/gtest.h>

#include "../perf-mon.h"

namespace perfmon {

namespace fidl_perfmon = fuchsia_perfmon_cpu;

// Shorten some long FIDL names.
using FidlPerfmonAllocation = fidl_perfmon::wire::Allocation;
using FidlPerfmonConfig = fidl_perfmon::wire::Config;
using FidlPerfmonConfigEventFlags = fidl_perfmon::wire::EventConfigFlags;

// List of events we need. This is a minimal version of
// src/performance/lib/perfmon/event-registry.{h,cc}.
// TODO(dje): Move this DB so we can use it too (after unified build?).

#ifdef __aarch64__

enum class Arm64FixedEvent : perfmon::EventId {
#define DEF_FIXED_EVENT(symbol, event_name, id, regnum, flags, readable_name, description) \
  symbol = MakeEventId(kGroupFixed, id),
#include <lib/zircon-internal/device/cpu-trace/arm64-pm-events.inc>
};

#elif defined(__x86_64__)

enum class X64FixedEvent : perfmon::EventId {
#define DEF_FIXED_EVENT(symbol, event_name, id, regnum, flags, readable_name, description) \
  symbol = MakeEventId(kGroupFixed, id),
#include <lib/zircon-internal/device/cpu-trace/intel-pm-events.inc>
};

enum class SklMiscEvent : perfmon::EventId {
#define DEF_MISC_SKL_EVENT(symbol, event_name, id, offset, size, flags, readable_name, \
                           description)                                                \
  symbol = MakeEventId(kGroupMisc, id),
#include <lib/zircon-internal/device/cpu-trace/skylake-misc-events.inc>
};

#elif defined(__riscv)

enum class RiscvFixedEvent : perfmon::EventId {
  FIXED_RDCYCLE = 0,
  FIXED_RDTIME = 1,
  FIXED_INSTRET = 2,
};

#else
#error "unsupported arch"
#endif

// A version of |zx_mtrace_control()| that always returns ZX_OK.
static zx_status_t mtrace_control_always_ok(zx_handle_t handle, uint32_t kind, uint32_t action,
                                            uint32_t options, void* buf, size_t buf_size) {
  return ZX_OK;
}

// Return a fake set of hw properties suitable for most tests.
static perfmon::PmuHwProperties GetFakeHwProperties() {
  perfmon::PmuHwProperties props{};

#if defined(__aarch64__)
  // VIM2 supports version 3, begin with that.
  props.common.pm_version = 3;
  // ARM has one fixed event, the cycle counter.
  props.common.max_num_fixed_events = 1;
  // The widths here aren't too important.
  props.common.max_fixed_counter_width = 64;
  props.common.max_num_programmable_events = 1;
  props.common.max_programmable_counter_width = 32;
  props.common.max_num_misc_events = 0;
  props.common.max_misc_counter_width = 0;
#elif defined(__x86_64__)
  // Skylake supports version 4, begin with that.
  props.common.pm_version = 4;
  // Intel has 3 fixed events: instruction, unhalted reference cycles, and unhalted core cycles.
  props.common.max_num_fixed_events = 3;
  // The widths here aren't too important.
  props.common.max_fixed_counter_width = 32;
  props.common.max_num_programmable_events = 1;
  props.common.max_programmable_counter_width = 32;
  props.common.max_num_misc_events = 1;
  props.common.max_misc_counter_width = 32;
  props.perf_capabilities = 0;
  props.lbr_stack_size = 0;
#elif defined(__riscv)
  // Based on riscv zicntr and zihpm extensions
  props.common.pm_version = 2;
  props.common.max_num_fixed_events = 3;
  props.common.max_fixed_counter_width = 64;
  props.common.max_num_programmable_events = 29;
  props.common.max_programmable_counter_width = 64;
#else
#error "unsupported arch"
#endif

  return props;
}

class Perfmon : public ::testing::Test {
 public:
  void SetUp() override {
    perfmon::PmuHwProperties props{GetFakeHwProperties()};
    device_ = std::make_unique<perfmon::PerfmonController>(props, mtrace_control_always_ok,
                                                           zx::unowned_resource{ZX_HANDLE_INVALID});
  }

  perfmon::PerfmonController* device() const { return device_.get(); }

 private:
  std::unique_ptr<perfmon::PerfmonController> device_;
};

TEST_F(Perfmon, BasicCycles) {
  FidlPerfmonAllocation allocation{};
  allocation.num_buffers = zx_system_get_num_cpus();
  allocation.buffer_size_in_pages = 1;
  ASSERT_EQ(ZX_OK, device()->PmuInitialize(&allocation));

  FidlPerfmonConfig config;
#if defined(__aarch64__)
  auto cycle_event_id = static_cast<perfmon::EventId>(Arm64FixedEvent::FIXED_CYCLE_COUNTER);
#elif defined(__x86_64__)
  auto cycle_event_id =
      static_cast<perfmon::EventId>(X64FixedEvent::FIXED_UNHALTED_REFERENCE_CYCLES);
#elif defined(__riscv)
  auto cycle_event_id = static_cast<perfmon::EventId>(RiscvFixedEvent::FIXED_RDCYCLE);
#else
#error "unsupported arch"
#endif
  config.events[0].event = cycle_event_id;
  config.events[0].rate = 0;
  config.events[0].flags |= FidlPerfmonEventConfigFlags::kCollectOs;

#if defined(__riscv)
  // Not yet implemented for riscv
  ASSERT_EQ(ZX_ERR_INVALID_ARGS, device()->PmuStageConfig(&config));
  return;
#else
  ASSERT_EQ(ZX_OK, device()->PmuStageConfig(&config));
#endif

  ASSERT_EQ(ZX_OK, device()->PmuStart());
  device()->PmuStop();
}

#ifdef __x86_64__

// It's possible to ask for only non-cpu related counters on x86.
// Verify this doesn't crash.
TEST_F(Perfmon, OnlyNonCpuCountersSelected) {
  FidlPerfmonAllocation allocation{};
  allocation.num_buffers = zx_system_get_num_cpus();
  allocation.buffer_size_in_pages = 1;
  ASSERT_EQ(ZX_OK, device()->PmuInitialize(&allocation));

  FidlPerfmonConfig config{};
  config.events[0].event = static_cast<perfmon::EventId>(SklMiscEvent::MISC_PKG_EDRAM_TEMP);
  config.events[0].rate = 0;
  config.events[0].flags |= FidlPerfmonEventConfigFlags::kCollectOs;
  ASSERT_EQ(ZX_OK, device()->PmuStageConfig(&config));

  ASSERT_EQ(ZX_OK, device()->PmuStart());
  device()->PmuStop();
}

#endif  // __x86_64__

}  // namespace perfmon
