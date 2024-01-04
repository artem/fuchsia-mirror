// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/ticks.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/zbitl/view.h>
#include <platform.h>

#include <lk/init.h>
#include <phys/handoff.h>
#include <platform/boot_timestamps.h>
#include <platform/timer.h>
#include <vm/physmap.h>

PhysHandoff* gPhysHandoff;

namespace {

// TODO(https://fxbug.dev/84107): Eventually physboot will hand off a permanent pointer
// we can store in gBootOptions.  For now, handoff only provides temporary
// pointers that we must copy out of.
BootOptions gBootOptionsInstance;

// When using physboot, other samples are available in the handoff data too.
//
// **NOTE** Each sample here is represented in the userland test code in
// //src/tests/benchmarks/kernel_boot_stats.cc that knows the order of the
// steps and gives names to the intervals between the steps (as well as
// tracking the first-to-last total elapsed time across the first to last
// boot.timeline.* samples, not all recorded right here).  Any time a new time
// sample is added to PhysBootTimes, a kcounter should be added here and
// kernel_boot_stats.cc should be updated to give the new intervals appropriate
// names for the performance tracking infrastructure (see the pages at
// https://chromeperf.appspot.com/report and look for "fuchsia.kernel.boot").
KCOUNTER(timeline_hw_startup, "boot.timeline.hw")
KCOUNTER(timeline_zbi_entry, "boot.timeline.zbi")
KCOUNTER(timeline_physboot_setup, "boot.timeline.physboot-setup")
KCOUNTER(timeline_decompress_start, "boot.timeline.decompress-start")
KCOUNTER(timeline_decompress_end, "boot.timeline.decompress-end")
KCOUNTER(timeline_zbi_done, "boot.timeline.zbi-done")
KCOUNTER(timeline_physboot_handoff, "boot.timeline.physboot-handoff")
KCOUNTER(timeline_virtual_entry, "boot.timeline.virtual")

void Set(const Counter& counter, arch::EarlyTicks sample) {
  counter.Set(platform_convert_early_ticks(sample));
}

void Set(const Counter& counter, PhysBootTimes::Index i) {
  counter.Set(platform_convert_early_ticks(gPhysHandoff->times.Get(i)));
}

// Convert early boot timeline points into zx_ticks_t values in kcounters.
void TimelineCounters(unsigned int level) {
  // This isn't really a loop in any meaningful sense, but structuring it
  // this way gets the compiler to warn about any forgotten enum entry.
  for (size_t i = 0; i <= PhysBootTimes::kCount; ++i) {
    const PhysBootTimes::Index when = static_cast<PhysBootTimes::Index>(i);
    switch (when) {
      case PhysBootTimes::kZbiEntry:
        Set(timeline_zbi_entry, when);
        break;
      case PhysBootTimes::kPhysSetup:
        Set(timeline_physboot_setup, when);
        break;
      case PhysBootTimes::kDecompressStart:
        Set(timeline_decompress_start, when);
        break;
      case PhysBootTimes::kDecompressEnd:
        Set(timeline_decompress_end, when);
        break;
      case PhysBootTimes::kZbiDone:
        Set(timeline_zbi_done, when);
        break;
      case PhysBootTimes::kCount:
        // There is no PhysBootTimes entry corresponding to kCount.
        // This is the first sample taken by the kernel proper after physboot handed off.
        Set(timeline_physboot_handoff, kernel_entry_ticks);
        break;
    }
  }
  Set(timeline_virtual_entry, kernel_virtual_entry_ticks);
  Set(timeline_hw_startup, arch::EarlyTicks::Zero());
}

// This can happen really any time after the platform clock is configured.
LK_INIT_HOOK(TimelineCounters, TimelineCounters, LK_INIT_LEVEL_PLATFORM)

}  // namespace

template <>
void* PhysHandoffPtrImportPhysAddr<PhysHandoffPtrEncoding::PhysAddr>(uintptr_t ptr) {
  return paddr_to_physmap(ptr);
}

void HandoffFromPhys(paddr_t handoff_paddr) {
  gPhysHandoff = static_cast<PhysHandoff*>(paddr_to_physmap(handoff_paddr));

  gBootOptionsInstance = *gPhysHandoff->boot_options;
  gBootOptions = &gBootOptionsInstance;

  if (gPhysHandoff->reboot_reason) {
    platform_set_hw_reboot_reason(gPhysHandoff->reboot_reason.value());
  }
  gAcpiRsdp = gPhysHandoff->acpi_rsdp.value_or(0);
  gSmbiosPhys = gPhysHandoff->smbios_phys.value_or(0);
}

ktl::span<ktl::byte> ZbiInPhysmap(bool own) {
  ZX_ASSERT(gPhysHandoff->zbi != 0);
  void* data = paddr_to_physmap(gPhysHandoff->zbi);
  if (own) {
    gPhysHandoff->zbi = 0;
  }

  zbitl::ByteView zbi = zbitl::StorageFromRawHeader(static_cast<zbi_header_t*>(data));
  ZX_ASSERT(!zbi.empty());
  return {const_cast<ktl::byte*>(zbi.data()), zbi.size()};
}
