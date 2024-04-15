// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/pool-mem-config.h>
#include <lib/boot-shim/uart.h>
#include <lib/zbi-format/board.h>
#include <lib/zbitl/view.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <fbl/alloc_checker.h>
#include <ktl/align.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/arch/arch-phys-info.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/new.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

namespace {

using PlatformIdItem = boot_shim::SingleOptionalItem<zbi_platform_id_t, ZBI_TYPE_PLATFORM_ID>;
using BoardInfoItem = boot_shim::SingleOptionalItem<zbi_board_info_t, ZBI_TYPE_DRV_BOARD_INFO>;

constexpr const char* kShimName = "linux-riscv64-boot-shim";

// TODO(https://fxbug.dev/295031359): Once assembly generates this items, remove the hardcoded pair.
constexpr zbi_platform_id_t kQemuPlatformId = {
    .vid = 1,  // fuchsia.platform.BIND_PLATFORM_DEV_VID.QEMU
    .pid = 1,  // fuchsia.platform.BIND_PLATFORM_DEV_PID.QEMU
    .board_name = "qemu-riscv64",
};

constexpr zbi_board_info_t kQemuBoardInfo = {
    .revision = 0x1,
};

struct BootHartIdGetter {
  static uint64_t Get() {
    ZX_DEBUG_ASSERT(gArchPhysInfo);
    return gArchPhysInfo->boot_hart_id;
  }
};

}  // namespace

void PhysMain(void* fdt, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  // Set up gArchPhysInfo as early as possible, as the boot hart ID needs to
  // be supplied to the CPU topology devicetree matcher.
  ArchSetUp(nullptr);

  AddressSpace aspace;
  InitMemory(fdt, &aspace);

  MainSymbolize symbolize(kShimName);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  boot_shim::DevicetreeBootShim<boot_shim::UartItem<>, boot_shim::PoolMemConfigItem,
                                boot_shim::RiscvDevicetreePlicItem,
                                boot_shim::RiscvDevicetreeTimerItem,
                                boot_shim::RiscvDevicetreeCpuTopologyItem<BootHartIdGetter>,
                                boot_shim::DevicetreeDtbItem, PlatformIdItem, BoardInfoItem>
      shim(kShimName, gDevicetreeBoot.fdt);
  shim.set_allocator([](size_t size, size_t align, fbl::AllocChecker& ac) -> void* {
    return new (ktl::align_val_t{align}, gPhysNew<memalloc::Type::kPhysScratch>, ac) uint8_t[size];
  });
  shim.set_cmdline(gDevicetreeBoot.cmdline);
  shim.Get<boot_shim::UartItem<>>().Init(GetUartDriver().uart());
  shim.Get<boot_shim::PoolMemConfigItem>().Init(Allocation::GetPool());
  shim.Get<boot_shim::DevicetreeDtbItem>().set_payload(ktl::as_bytes(gDevicetreeBoot.fdt.fdt()));

  shim.Get<PlatformIdItem>().set_payload(kQemuPlatformId);
  shim.Get<BoardInfoItem>().set_payload(kQemuBoardInfo);

  // Fill DevicetreeItems.
  ZX_ASSERT(shim.Init());

  // Finally we can boot into the kernel image.
  BootZbi::InputZbi zbi_view(gDevicetreeBoot.ramdisk);
  BootZbi boot;

  if (shim.Check("Not a bootable ZBI", boot.Init(zbi_view)) &&
      shim.Check("Failed to load ZBI", boot.Load(static_cast<uint32_t>(shim.size_bytes()))) &&
      shim.Check("Failed to append boot loader items to data ZBI",
                 shim.AppendItems(boot.DataZbi()))) {
    boot.Log();
    boot.Boot();
  }

  abort();
}
