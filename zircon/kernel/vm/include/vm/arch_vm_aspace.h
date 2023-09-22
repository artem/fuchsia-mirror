// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_ARCH_VM_ASPACE_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_ARCH_VM_ASPACE_H_

#include <lib/fit/function.h>
#include <sys/types.h>
#include <zircon/types.h>

#include <fbl/macros.h>
#include <vm/page.h>

// Flags
const uint ARCH_MMU_FLAG_CACHED = (0u << 0);
const uint ARCH_MMU_FLAG_UNCACHED = (1u << 0);
const uint ARCH_MMU_FLAG_UNCACHED_DEVICE =
    (2u << 0);  // Only exists on some arches, otherwise UNCACHED
const uint ARCH_MMU_FLAG_WRITE_COMBINING =
    (3u << 0);  // Only exists on some arches, otherwise UNCACHED
const uint ARCH_MMU_FLAG_CACHE_MASK = (3u << 0);
const uint ARCH_MMU_FLAG_PERM_USER = (1u << 2);
const uint ARCH_MMU_FLAG_PERM_READ = (1u << 3);
const uint ARCH_MMU_FLAG_PERM_WRITE = (1u << 4);
const uint ARCH_MMU_FLAG_PERM_EXECUTE = (1u << 5);
const uint ARCH_MMU_FLAG_PERM_RWX_MASK =
    (ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE | ARCH_MMU_FLAG_PERM_EXECUTE);
const uint ARCH_MMU_FLAG_NS = (1u << 6);       // NON-SECURE
const uint ARCH_MMU_FLAG_INVALID = (1u << 7);  // Indicates that flags are not specified

const uint ARCH_ASPACE_FLAG_KERNEL = (1u << 0);
const uint ARCH_ASPACE_FLAG_GUEST = (1u << 1);

// per arch base class api to encapsulate the mmu routines on an aspace
//
// Beyond construction/destruction lifetimes users of this object must ensure that none of the
// main methods are called before calling Init or after calling Destroy. Doing so is allowed to
// cause a panic.
// Aside from Init and Destroy, the main methods are all thread-safe.
class ArchVmAspaceInterface {
 public:
  ArchVmAspaceInterface() = default;
  virtual ~ArchVmAspaceInterface() = default;

  // Function pointer to allocate a single page that the mmu routine uses to allocate
  // page tables.
  using page_alloc_fn_t = zx_status_t (*)(uint alloc_flags, vm_page** p, paddr_t* pa);

  // This method initializes an empty ArchVmAspace.
  virtual zx_status_t Init() = 0;

  // This method initializes an ArchVmAspace just like Init, but also prepopulates the top level
  // page table. Either Init or InitPrepopulated can be called to set up an ArchVmAspace, but not
  // both.
  virtual zx_status_t InitPrepopulated() = 0;

  // This method puts the instance into read-only mode and asserts that it contains no mappings.
  //
  // Note, this method may be a no-op on some architectures.  See fxbug.dev/79118.
  //
  // It is an error to call this method on an instance that contains mappings.  Once called,
  // subsequent operations that modify the page table will trigger a panic.
  //
  // The purpose of this method is to help enforce lifecycle and state transitions of VmAspace and
  // ArchVmAspaceInterface.
  virtual void DisableUpdates() = 0;

  // Destroy expects the aspace to be fully unmapped, as any mapped regions
  // indicate incomplete cleanup at the higher layers.
  //
  // It is safe to call Destroy even if Init failed.
  // Once destroy has been called it is a user error to call any of the other methods on the aspace,
  // unless specifically stated otherwise, and doing so may cause a panic.
  virtual zx_status_t Destroy() = 0;

  // main methods
  // Map a physically contiguous region into the virtual address space
  virtual zx_status_t MapContiguous(vaddr_t vaddr, paddr_t paddr, size_t count, uint mmu_flags,
                                    size_t* mapped) = 0;

  // Map the given array of pages into the virtual address space starting at
  // |vaddr|, in the order they appear in |phys|.
  // If any address in the range [vaddr, vaddr + count * PAGE_SIZE) is already
  // mapped when this is called, and the |existing_action| is |Error| then this
  // returns ZX_ERR_ALREADY_EXISTS, otherwise they are skipped. Skipped pages
  // are stil counted in |mapped|. On failure some pages may still be mapped,
  // the number of which will be reported in |mapped|.
  enum class ExistingEntryAction : bool {
    Skip,
    Error,
  };
  virtual zx_status_t Map(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                          ExistingEntryAction existing_action, size_t* mapped) = 0;

  // Unmap the given virtual address range.
  // EnlargeOperation controls whether the unmap region can be extended to be larger, or if only the
  // exact region may be unmapped. The unmap region might be extended in out of memory scenarios if
  // large pages need to be split.
  enum EnlargeOperation : bool {
    Yes = true,
    No = false,
  };
  virtual zx_status_t Unmap(vaddr_t vaddr, size_t count, EnlargeOperation enlarge,
                            size_t* unmapped) = 0;

  // Change the page protections on the given virtual address range
  //
  // May return ZX_ERR_NO_MEMORY if the operation requires splitting
  // a large page and the next level page table allocation fails. In
  // this case, mappings in the input range may be a mix of the old and
  // new flags.
  virtual zx_status_t Protect(vaddr_t vaddr, size_t count, uint mmu_flags) = 0;

  virtual zx_status_t Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) = 0;

  virtual vaddr_t PickSpot(vaddr_t base, vaddr_t end, vaddr_t align, size_t size,
                           uint mmu_flags) = 0;

  // For HarvestAccessed Terminal and non-terminal get processed based on the following two
  // controls.
  enum class NonTerminalAction : bool {
    // If a non-terminal entry has no accessed information, unmap and free it. If it has accessed
    // information, just remove the flag.
    FreeUnaccessed,
    // Retain both the non-terminal mappings and any accessed information.
    Retain,
  };
  enum class TerminalAction : bool {
    // If the page is accessed update its age in the page queues, and remove the accessed flag.
    UpdateAgeAndHarvest,
    // If the page is accessed update its age in the page queues, but do not clear the flag.
    UpdateAge,
  };
  // Walks the given range of pages and for any pages that are mapped and have their access bit set
  //  * Tells the page queues it has been accessed via PageQueues::MarkAccessed
  //  * Potentially removes the accessed flag.
  //  * Potentially frees unaccessed page tables.
  virtual zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count,
                                      NonTerminalAction non_terminal_action,
                                      TerminalAction terminal_action) = 0;

  // Marks any pages in the given virtual address range as being accessed.
  virtual zx_status_t MarkAccessed(vaddr_t vaddr, size_t count) = 0;

  // Returns whether or not this aspace has been active since the last time this method was called
  // with clear=true.
  //
  // This is intended for use by the harvester to avoid scanning for any accessed or dirty bits if
  // the aspace has not been active in the mmu, since an aspace that has not been active cannot
  // generate new information.
  //
  // The |clear| flag controls whether the aspace having been active should be cleared or not. Not
  // clearing makes this function const and not modify any state.
  virtual bool ActiveSinceLastCheck(bool clear) = 0;

  // Physical address of the backing data structure used for translation.
  //
  // This should be treated as an opaque value outside of
  // architecture-specific components.
  virtual paddr_t arch_table_phys() const = 0;
};

// Per arch base class API to encapsulate routines for maintaining icache consistency.
class ArchVmICacheConsistencyManagerInterface {
 public:
  ArchVmICacheConsistencyManagerInterface() = default;
  virtual ~ArchVmICacheConsistencyManagerInterface() = default;

  // Indicate that the given kernel address range may have modified data. The given range is not
  // actually guaranteed to be synced until |Finish| is called. All aliases of the given range are
  // guaranteed to be consistent after |Finish|.
  virtual void SyncAddr(vaddr_t start, size_t len) = 0;

  // Perform any final synchronization operations. This may be used by an implementation to
  // efficiently batch operations, and no addresses should be considered actually synchronized
  // until this returns.
  // This is automatically called on destruction.
  virtual void Finish() = 0;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_ARCH_VM_ASPACE_H_
