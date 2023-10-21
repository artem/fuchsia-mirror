// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/x86/boot-cpuid.h>
#include <lib/fit/defer.h>
#include <lib/memalloc/pool.h>
#include <stdio.h>
#include <zircon/limits.h>

#include <fbl/algorithm.h>
#include <hwreg/x86msr.h>
#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <phys/allocation.h>

#include "phys/address-space.h"

#include <ktl/enforce.h>

namespace {

// On x86-64, we don't have any guarantee that all the memory in our address
// space is actually mapped in.
//
// We use a bootstrap allocator consisting of memory from ".bss" to construct a
// real page table with.  Unused memory will be returned to the heap after
// initialisation is complete.
//
// Amount of memory reserved in .bss for allocation of page table data
// structures: We reserve 512kiB. On machines which only support at most 2 MiB
// page sizes, we need ~8 bytes per 2 MiB, allowing us to map ~128 GiB of
// RAM. On machines with 1 GiB page sizes, we can support ~64 TiB of RAM.
//
constexpr size_t kBootstrapMemoryBytes = 512 * 1024;

// Bootstrap memory pool.
alignas(ZX_MIN_PAGE_SIZE) ktl::array<ktl::byte, kBootstrapMemoryBytes> gBootstrapMemory;

void SetUpAddressSpace(AddressSpace& aspace) {
  // Ensure that executable pages are allowed.
  hwreg::X86MsrIo msr;
  arch::X86ExtendedFeatureEnableRegisterMsr::Get().ReadFrom(&msr).set_nxe(1).WriteTo(&msr);
  aspace.Init();
  aspace.SetUpIdentityMappings();
  aspace.Install();
}

}  // namespace

void ArchSetUpAddressSpaceEarly(AddressSpace& aspace) {
  memalloc::Pool& pool = Allocation::GetPool();

  uint64_t bootstrap_start = reinterpret_cast<uintptr_t>(gBootstrapMemory.data());
  uint64_t bootstrap_end = bootstrap_start + gBootstrapMemory.size();

  // Per the above, we Free() the .bss bootstrap region to be able to allocate
  // from it, and then clamp the global page table allocation bounds to it.

  if (pool.Free(bootstrap_start, gBootstrapMemory.size()).is_error()) {
    ZX_PANIC("Failed to free .bss page table bootstrap region [%#" PRIx64 ", %#" PRIx64 ")",
             bootstrap_start, bootstrap_end);
  }

  aspace.SetPageTableAllocationBounds(bootstrap_start, bootstrap_end);
  SetUpAddressSpace(aspace);

  // Now that all RAM is mapped in, we no longer have any allocation
  // restrictions.
  aspace.SetPageTableAllocationBounds(ktl::nullopt, ktl::nullopt);
}

void ArchSetUpAddressSpaceLate(AddressSpace& aspace) { SetUpAddressSpace(aspace); }
