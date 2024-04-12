// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_
#define ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_

#include <align.h>
#include <lib/arch/x86/boot-cpuid.h>
#include <lib/fit/defer.h>
#include <lib/zx/result.h>

#include <arch/x86/page_tables/constants.h>
#include <fbl/canary.h>
#include <hwreg/bitfields.h>
#include <kernel/mutex.h>
#include <page_tables/x86/constants.h>
#include <vm/arch_vm_aspace.h>
#include <vm/mapping_cursor.h>
#include <vm/physmap.h>
#include <vm/pmm.h>

typedef uint64_t pt_entry_t;
#define PRIxPTE PRIx64

// Different page table levels in the page table mgmt hirerachy
enum class PageTableLevel {
  PT_L = 0,
  PD_L = 1,
  PDP_L = 2,
  PML4_L = 3,
};

// Different roles a page table can fulfill when running with unified aspaces.
enum class PageTableRole : uint8_t {
  kIndependent,
  kRestricted,
  kShared,
  kUnified,
};

// Type for flags used in the hardware page tables, for terminal entries.
// Note that some flags here may have meanings that depend on the level
// at which they occur (e.g. page size and PAT).
using PtFlags = uint64_t;

// Type for flags used in the hardware page tables, for non-terminal
// entries.
using IntermediatePtFlags = uint64_t;

namespace internal {

// Utility for coalescing cache line flushes when modifying page tables.  This
// allows us to mutate adjacent page table entries without having to flush for
// each cache line multiple times.
class CacheLineFlusher {
 public:
  // If |perform_invalidations| is false, this class acts as a no-op.
  explicit CacheLineFlusher(bool perform_invalidations)
      : dirty_line_(0),
        cl_mask_(~(arch::BootCpuid<arch::CpuidProcessorInfo>().cache_line_size_bytes() - 1ull)),
        perform_invalidations_(perform_invalidations) {}
  ~CacheLineFlusher() { ForceFlush(); }
  void FlushPtEntry(const volatile pt_entry_t* entry) {
    uintptr_t entry_line = reinterpret_cast<uintptr_t>(entry) & cl_mask_;
    if (entry_line != dirty_line_) {
      ForceFlush();
      dirty_line_ = entry_line;
    }
  }

  void ForceFlush() {
    if (dirty_line_ && perform_invalidations_) {
      __asm__ volatile("clflush %0\n" : : "m"(*reinterpret_cast<char*>(dirty_line_)) : "memory");
      dirty_line_ = 0;
    }
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(CacheLineFlusher);

  // The cache-aligned address that currently dirty.  If 0, no dirty line.
  uintptr_t dirty_line_;

  const uintptr_t cl_mask_;
  const bool perform_invalidations_;
};

// Structure for tracking an upcoming TLB invalidation
struct PendingTlbInvalidation {
  struct Item {
    uint64_t raw;
    DEF_SUBFIELD(raw, 2, 0, page_level);
    DEF_SUBBIT(raw, 3, is_global);
    DEF_SUBBIT(raw, 4, is_terminal);
    DEF_SUBFIELD(raw, 63, 12, encoded_addr);

    vaddr_t addr() const { return encoded_addr() << PAGE_SIZE_SHIFT; }
  };
  static_assert(sizeof(Item) == 8, "");

  // If true, ignore |vaddr| and perform a full invalidation for this context.
  bool full_shootdown = false;
  // If true, at least one enqueued entry was for a global page.
  bool contains_global = false;
  // Number of valid elements in |item|
  uint count = 0;
  // List of addresses queued for invalidation.
  // Explicitly uninitialized since the size is fairly large.
  Item item[32];

  // Add address |v|, translated at depth |level|, to the set of addresses to be invalidated.
  // |is_terminal| should be true iff this invalidation is targeting the final step of the
  // translation rather than a higher page table entry. |is_global_page| should be true iff this
  // page was mapped with the global bit set.
  void enqueue(vaddr_t v, PageTableLevel level, bool is_global_page, bool is_terminal) {
    if (is_global_page) {
      contains_global = true;
    }

    // We mark PML4_L entries as full shootdowns, since it's going to be
    // expensive one way or another.
    if (count >= ktl::size(item) || level == PageTableLevel::PML4_L) {
      full_shootdown = true;
      return;
    }
    item[count].set_page_level(static_cast<uint64_t>(level));
    item[count].set_is_global(is_global_page);
    item[count].set_is_terminal(is_terminal);
    item[count].set_encoded_addr(v >> PAGE_SIZE_SHIFT);
    count++;
  }

  // Clear the list of pending invalidations
  void clear() {
    count = 0;
    full_shootdown = false;
    contains_global = false;
  }

  ~PendingTlbInvalidation() { DEBUG_ASSERT(count == 0); }
};

}  // namespace internal

class X86PageTableBase {
 public:
  X86PageTableBase() {}
  virtual ~X86PageTableBase() {
    DEBUG_ASSERT_MSG(!phys_, "page table dtor called before Destroy()");
  }

  paddr_t phys() const { return phys_; }
  void* virt() const { return virt_; }

  size_t pages() {
    Guard<Mutex> al{AssertOrderedLock, &lock_, LockOrder()};
    return pages_;
  }
  void* ctx() const { return ctx_; }

  using PendingTlbInvalidation = internal::PendingTlbInvalidation;
  using CacheLineFlusher = internal::CacheLineFlusher;

  using ExistingEntryAction = ArchVmAspaceInterface::ExistingEntryAction;
  using EnlargeOperation = ArchVmAspaceInterface::EnlargeOperation;

  // Returns whether this page table is restricted.
  // We do so by verifying that it was created with `InitRestricted` and has been linked to a
  // unified page table.
  bool IsRestricted() const { return role_ == PageTableRole::kRestricted; }

  // Returns whether this page table is shared.
  bool IsShared() const { return role_ == PageTableRole::kShared; }

  // Returns whether this page table is unified.
  bool IsUnified() const { return role_ == PageTableRole::kUnified; }

  virtual zx_status_t MapPages(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                               ExistingEntryAction existing_action, size_t* mapped) = 0;
  virtual zx_status_t MapPagesContiguous(vaddr_t vaddr, paddr_t paddr, const size_t count,
                                         uint mmu_flags, size_t* mapped) = 0;
  virtual zx_status_t UnmapPages(vaddr_t vaddr, const size_t count, EnlargeOperation enlarge,
                                 size_t* unmapped) = 0;
  virtual zx_status_t ProtectPages(vaddr_t vaddr, size_t count, uint mmu_flags) = 0;
  virtual zx_status_t QueryVaddr(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) = 0;

  using NonTerminalAction = ArchVmAspaceInterface::NonTerminalAction;
  using TerminalAction = ArchVmAspaceInterface::TerminalAction;
  virtual zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count,
                                      NonTerminalAction non_terminal_action,
                                      TerminalAction terminal_action) = 0;

  // Returns 1 for unified page tables and 0 for all other page tables. This establishes an
  // ordering that is used when the lock_ is acquired. The restricted page table lock is acquired
  // first, and the unified page table lock is acquired afterwards.
  uint32_t LockOrder() const { return IsUnified() ? 1 : 0; }

 protected:
  using page_alloc_fn_t = ArchVmAspaceInterface::page_alloc_fn_t;

  // Initialize an empty page table, assigning this given context to it.
  zx_status_t Init(void* ctx, page_alloc_fn_t test_paf = nullptr) TA_NO_THREAD_SAFETY_ANALYSIS {
    test_page_alloc_func_ = test_paf;

    /* allocate a top level page table for the new address space */
    virt_ = AllocatePageTable();
    if (!virt_) {
      return ZX_ERR_NO_MEMORY;
    }

    phys_ = physmap_to_paddr(virt_);
    DEBUG_ASSERT(phys_ != 0);

    ctx_ = ctx;
    pages_ = 1;
    return ZX_OK;
  }

  pt_entry_t* AllocatePageTable() {
    paddr_t pa;
    vm_page* p;
    zx_status_t status;

    // The default allocation routine is pmm_alloc_page so test and explicitly call it
    // to avoid any unnecessary virtual function calls.
    if (likely(!test_page_alloc_func_)) {
      status = pmm_alloc_page(0, &p, &pa);
    } else {
      status = test_page_alloc_func_(0, &p, &pa);
    }
    if (status != ZX_OK) {
      return nullptr;
    }
    p->set_state(vm_page_state::MMU);

    pt_entry_t* page_ptr = static_cast<pt_entry_t*>(paddr_to_physmap(pa));
    DEBUG_ASSERT(page_ptr);

    arch_zero_page(page_ptr);

    return page_ptr;
  }

  DISALLOW_COPY_ASSIGN_AND_MOVE(X86PageTableBase);

  fbl::Canary<fbl::magic("X86P")> canary_;

  // The number of times entries in the pml4 are referenced by other page tables.
  // Unified page tables increment and decrement this value on their associated shared and
  // restricted page tables, so we must hold the lock_ when doing so.
  uint32_t num_references_ TA_GUARDED(lock_) = 0;

  // The role this page table plays in unified aspaces, if any. This should only be set by the
  // Init* functions, and should not be modified anywhere else.
  PageTableRole role_ = PageTableRole::kIndependent;

  // Page allocate function, overridable for testing.
  page_alloc_fn_t test_page_alloc_func_ = nullptr;

  // Pointer to the translation table.
  paddr_t phys_ = 0;
  pt_entry_t* virt_ = nullptr;

  // Counter of pages allocated to back the translation table.
  size_t pages_ TA_GUARDED(lock_) = 0;

  // A context structure that may used by a PageTable type above as part of
  // invalidation.
  void* ctx_ = nullptr;

  // Lock to protect the mmu code.
  DECLARE_MUTEX(X86PageTableBase, lockdep::LockFlagsNestable) lock_;
};

// Implementation of the X86 page table code, that is expected to be derived using the recursive
// template pattern. The derived class T is expected to implement the following methods:
//
// Returns the highest level of the page tables
// PageTableLevel top_level();
//
// Returns true if the given ARCH_MMU_FLAG_* flag combination is valid.
// bool allowed_flags(uint flags);
//
// Returns true if the given paddr is valid
// bool check_paddr(paddr_t paddr);
//
// Returns true if the given vaddr is valid
// bool check_vaddr(vaddr_t vaddr);
//
// Whether the processor supports the page size of this level
// bool supports_page_size(PageTableLevel level);
//
// Return the hardware flags to use on intermediate page tables entries
// IntermediatePtFlags intermediate_flags();
//
// Return the hardware flags to use on terminal page table entries
// PtFlags terminal_flags(PageTableLevel level, uint flags);
//
// Return the hardware flags to use on smaller pages after a splitting a
// large page with flags |flags|.
// PtFlags split_flags(PageTableLevel level, PtFlags flags);
//
// Execute the given pending invalidation
// void TlbInvalidate(const PendingTlbInvalidation* pending);
//
// Convert PtFlags to ARCH_MMU_* flags.
// uint pt_flags_to_mmu_flags(PtFlags flags, PageTableLevel level);
//
// Returns true if a cache flush is necessary for pagetable changes to be
// visible to hardware page table walkers. On x86, this is only true for Intel IOMMU page
// tables when the IOMMU 'caching mode' bit is true.
// bool needs_cache_flushes();
template <class T>
class X86PageTableImpl : public X86PageTableBase {
 public:
  X86PageTableImpl() {}

  // Accessors for the shared and restricted page tables on a unified page table.
  // We can turn off thread safety analysis as these accessors should only be used on unified page
  // tables, for which both the shared and restricted page table pointers are notionally const.
  X86PageTableBase* get_shared_pt() TA_NO_THREAD_SAFETY_ANALYSIS {
    DEBUG_ASSERT(IsUnified());
    return shared_pt_;
  }
  X86PageTableBase* get_restricted_pt() TA_NO_THREAD_SAFETY_ANALYSIS {
    DEBUG_ASSERT(IsUnified());
    return referenced_pt_;
  }

  zx_status_t MapPages(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                       ExistingEntryAction existing_action, size_t* mapped) override final {
    canary_.Assert();

    if (!static_cast<T*>(this)->check_vaddr(vaddr))
      return ZX_ERR_INVALID_ARGS;
    for (size_t i = 0; i < count; ++i) {
      if (!static_cast<T*>(this)->check_paddr(phys[i]))
        return ZX_ERR_INVALID_ARGS;
    }
    if (count == 0)
      return ZX_OK;

    if (!static_cast<T*>(this)->allowed_flags(mmu_flags))
      return ZX_ERR_INVALID_ARGS;

    __UNINITIALIZED ConsistencyManager cm(this);
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      DEBUG_ASSERT(virt_);

      MappingCursor cursor(/*paddrs=*/phys, /*paddr_count=*/count, /*page_size=*/PAGE_SIZE,
                           /*vaddr=*/vaddr);
      zx_status_t status = AddMapping(virt_, mmu_flags, static_cast<T*>(this)->top_level(),
                                      existing_action, cursor, &cm);
      cm.Finish();
      if (status != ZX_OK) {
        dprintf(SPEW, "Add mapping failed with err=%d\n", status);
        return status;
      }
      DEBUG_ASSERT(cursor.size() == 0);
    }

    if (mapped) {
      *mapped = count;
    }
    return ZX_OK;
  }
  zx_status_t MapPagesContiguous(vaddr_t vaddr, paddr_t paddr, const size_t count, uint mmu_flags,
                                 size_t* mapped) override final {
    canary_.Assert();

    if (!static_cast<T*>(this)->check_paddr(paddr))
      return ZX_ERR_INVALID_ARGS;
    if (!static_cast<T*>(this)->check_vaddr(vaddr))
      return ZX_ERR_INVALID_ARGS;
    if (count == 0)
      return ZX_OK;

    if (!static_cast<T*>(this)->allowed_flags(mmu_flags))
      return ZX_ERR_INVALID_ARGS;

    MappingCursor cursor(/*paddrs=*/&paddr, /*paddr_count=*/1, /*page_size=*/count * PAGE_SIZE,
                         /*vaddr=*/vaddr);
    __UNINITIALIZED ConsistencyManager cm(this);
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      DEBUG_ASSERT(virt_);
      zx_status_t status = AddMapping(virt_, mmu_flags, static_cast<T*>(this)->top_level(),
                                      ExistingEntryAction::Error, cursor, &cm);
      cm.Finish();
      if (status != ZX_OK) {
        dprintf(SPEW, "Add mapping failed with err=%d\n", status);
        return status;
      }
    }
    DEBUG_ASSERT(cursor.size() == 0);

    if (mapped)
      *mapped = count;

    return ZX_OK;
  }
  zx_status_t UnmapPages(vaddr_t vaddr, const size_t count, EnlargeOperation enlarge,
                         size_t* unmapped) override final {
    canary_.Assert();

    if (!static_cast<T*>(this)->check_vaddr(vaddr))
      return ZX_ERR_INVALID_ARGS;
    if (count == 0)
      return ZX_OK;

    MappingCursor cursor(/*vaddr=*/vaddr, /*size=*/count * PAGE_SIZE);

    __UNINITIALIZED ConsistencyManager cm(this);
    // This needs to be initialized to some value as gcc cannot work out that it can elide the
    // default constructor.
    zx::result<bool> status = zx::ok(true);
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      DEBUG_ASSERT(virt_);
      status = RemoveMapping(virt_, static_cast<T*>(this)->top_level(), enlarge, cursor, &cm);
      cm.Finish();
    }
    DEBUG_ASSERT(cursor.size() == 0 || status.is_error());

    if (unmapped)
      *unmapped = count;

    return status.status_value();
  }
  zx_status_t ProtectPages(vaddr_t vaddr, size_t count, uint mmu_flags) override final {
    canary_.Assert();

    if (!static_cast<T*>(this)->check_vaddr(vaddr))
      return ZX_ERR_INVALID_ARGS;
    if (count == 0)
      return ZX_OK;

    if (!static_cast<T*>(this)->allowed_flags(mmu_flags))
      return ZX_ERR_INVALID_ARGS;

    MappingCursor cursor(/*vaddr=*/vaddr, /*size=*/count * PAGE_SIZE);
    __UNINITIALIZED ConsistencyManager cm(this);
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      zx_status_t status =
          UpdateMapping(virt_, mmu_flags, static_cast<T*>(this)->top_level(), cursor, &cm);
      cm.Finish();
      if (status != ZX_OK) {
        return status;
      }
    }
    DEBUG_ASSERT(cursor.size() == 0);
    return ZX_OK;
  }

  zx_status_t QueryVaddr(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) override final {
    canary_.Assert();

    PageTableLevel ret_level;

    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

    volatile pt_entry_t* last_valid_entry;
    zx_status_t status =
        GetMapping(virt_, vaddr, static_cast<T*>(this)->top_level(), &ret_level, &last_valid_entry);
    if (status != ZX_OK)
      return status;

    DEBUG_ASSERT(last_valid_entry);

    /* based on the return level, parse the page table entry */
    if (paddr) {
      switch (ret_level) {
        case PageTableLevel::PDP_L: /* 1GB page */
          *paddr = paddr_from_pte(PageTableLevel::PDP_L, *last_valid_entry);
          *paddr |= vaddr & PAGE_OFFSET_MASK_HUGE;
          break;
        case PageTableLevel::PD_L: /* 2MB page */
          *paddr = paddr_from_pte(PageTableLevel::PD_L, *last_valid_entry);
          *paddr |= vaddr & PAGE_OFFSET_MASK_LARGE;
          break;
        case PageTableLevel::PT_L: /* 4K page */
          *paddr = paddr_from_pte(PageTableLevel::PT_L, *last_valid_entry);
          *paddr |= vaddr & PAGE_OFFSET_MASK_4KB;
          break;
        default:
          panic("arch_mmu_query: unhandled frame level\n");
      }
    }

    /* converting arch-specific flags to mmu flags */
    if (mmu_flags) {
      *mmu_flags = static_cast<T*>(this)->pt_flags_to_mmu_flags(*last_valid_entry, ret_level);
    }

    return ZX_OK;
  }

  zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count, NonTerminalAction non_terminal_action,
                              TerminalAction terminal_action) override final {
    canary_.Assert();

    if (!static_cast<T*>(this)->check_vaddr(vaddr)) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (count == 0) {
      return ZX_OK;
    }

    MappingCursor cursor(/*vaddr=*/vaddr, /*size=*/count * PAGE_SIZE);
    __UNINITIALIZED ConsistencyManager cm(this);
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      HarvestMapping(virt_, non_terminal_action, terminal_action,
                     static_cast<T*>(this)->top_level(), cursor, &cm);
      cm.Finish();
    }
    DEBUG_ASSERT(cursor.size() == 0);
    return ZX_OK;
  }

 protected:
  // We disable analysis due to the write to |pages_| tripping it up.  It is safe
  // to write to |pages_| since this is part of object construction.
  // Initialize an empty page table and mark it as restricted.
  zx_status_t InitRestricted(void* ctx, page_alloc_fn_t test_paf = nullptr) {
    role_ = PageTableRole::kRestricted;
    return Init(ctx, test_paf);
  }
  // Initialize a page table, assign the given context, and prepopulate the top level page table
  // entries.
  // We disable analysis due to the write to |pages_| tripping it up.  It is safe
  // to write to |pages_| since this is part of object construction.
  zx_status_t InitShared(void* ctx, vaddr_t base, size_t size,
                         page_alloc_fn_t test_paf = nullptr) TA_NO_THREAD_SAFETY_ANALYSIS {
    zx_status_t status = Init(ctx, test_paf);
    if (status != ZX_OK) {
      return status;
    }
    role_ = PageTableRole::kShared;

    PageTableLevel top = static_cast<T*>(this)->top_level();
    const uint start = vaddr_to_index(top, base);
    uint end = vaddr_to_index(top, base + size - 1);
    // Check the end if it fills out the table entry.
    if (page_aligned(top, base + size)) {
      end += 1;
    }
    IntermediatePtFlags flags = static_cast<T*>(this)->intermediate_flags();

    for (uint i = start; i < end; i++) {
      pt_entry_t* pdp = AllocatePageTable();
      if (pdp == nullptr) {
        return ZX_ERR_NO_MEMORY;
      }
      pages_ += 1;
      virt_[i] = X86_VIRT_TO_PHYS(pdp) | flags | X86_MMU_PG_P;
    }
    return ZX_OK;
  }

  // Initialize a page table, assign the given context, and set it up as a unified page table with
  // entries from the given page tables.
  //
  // The shared and restricted page tables must satisfy the following requirements:
  // 1) The shared page table must set only |is_shared_| to true.
  // 2) The restricted page table must set only |is_restricted_| to true.
  // 3) Both the shared and restricted page tables must have been initialized prior to this call.
  zx_status_t InitUnified(void* ctx, X86PageTableImpl<T>* shared, vaddr_t shared_base,
                          size_t shared_size, X86PageTableImpl<T>* restricted,
                          vaddr_t restricted_base, size_t restricted_size,
                          page_alloc_fn_t test_paf = nullptr) {
    DEBUG_ASSERT(restricted->IsRestricted());
    DEBUG_ASSERT(shared->IsShared());
    // Validate that the shared and restricted page tables do not overlap and do not share a PML4
    // entry.
    PageTableLevel top = static_cast<T*>(this)->top_level();
    const uint restricted_start = vaddr_to_index(top, restricted_base);
    uint restricted_end = vaddr_to_index(top, restricted_base + restricted_size - 1);
    if (page_aligned(top, restricted_base + restricted_size)) {
      restricted_end += 1;
    }
    const uint shared_start = vaddr_to_index(top, shared_base);
    uint shared_end = vaddr_to_index(top, shared_base + shared_size - 1);
    if (page_aligned(top, shared_base + shared_size)) {
      shared_end += 1;
    }
    DEBUG_ASSERT(restricted_end <= shared_start);

    zx_status_t status = Init(ctx, test_paf);
    if (status != ZX_OK) {
      return status;
    }
    role_ = PageTableRole::kUnified;

    // Validate the restricted page table and set its metadata.
    {
      Guard<Mutex> a{AssertOrderedLock, &restricted->lock_, restricted->LockOrder()};
      DEBUG_ASSERT(restricted->virt_);
      DEBUG_ASSERT(restricted->referenced_pt_ == nullptr);

      // Assert that there are no entries in the restricted page table.
      for (uint i = restricted_start; i < restricted_end; i++) {
        DEBUG_ASSERT(!IS_PAGE_PRESENT(restricted->virt_[i]));
      }

      restricted->referenced_pt_ = this;
      restricted->num_references_++;
    }

    // Copy all mappings from the shared page table and set its metadata.
    {
      Guard<Mutex> a{AssertOrderedLock, &shared->lock_, shared->LockOrder()};
      DEBUG_ASSERT(shared->virt_);
      DEBUG_ASSERT(shared->referenced_pt_ == nullptr);

      // Set up the PML4 so we capture any mappings created prior to creation of this unified page
      // table.
      pt_entry_t curr_entry = 0;
      for (uint i = shared_start; i < shared_end; i++) {
        curr_entry = shared->virt_[i];
        if (IS_PAGE_PRESENT(curr_entry)) {
          virt_[i] = curr_entry;
        }
      }
      shared->num_references_++;
    }

    // Update this page table's bookkeeping.
    {
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      referenced_pt_ = restricted;
      shared_pt_ = shared;
    }
    return ZX_OK;
  }

  // Calls DestroyUnified if this is a unified page table and DestroyIndividual if it is not.
  void Destroy(vaddr_t base, size_t size) {
    canary_.Assert();
    if (IsUnified()) {
      return DestroyUnified();
    }
    return DestroyIndividual(base, size);
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(X86PageTableImpl);

  // Utility for managing consistency of the page tables from a cache and TLB
  // point-of-view.  It ensures that memory is not freed while a TLB entry may
  // refer to it, and that changes to the page tables have appropriate visiblity
  // to the hardware interpreting them.  Finish MUST be called on this
  // class, even if the page table change failed.
  // The aspace lock *must* be held over the full operation of the ConsistencyManager, from
  // queue_free to Flush. The lock must be held continuously, due to strategy employed here of only
  // invalidating actual vaddrs with changing entries, and not all vaddrs an operation applies to.
  // Otherwise the following scenario is possible
  //  1. Thread 1 performs an Unmap and removes PTE entries, but drops the lock prior to
  //  invalidation.
  //  2. Thread 2 performs an Unmap, no PTE entries are removed, no invalidations occur
  //  3. Thread 2 now believes the resources (pages) for the region are no longer accessible, and
  //     returns them to the pmm.
  //  4. Thread 3 attempts to access this region and is now able to read/write to returned pages as
  //     invalidations have not occurred.
  // This scenario is possible as the mappings here are not the source of truth of resource
  // management, but a cache of information from other parts of the system. If thread 2 wanted to
  // guarantee that the pages were free it could issue it's own TLB invalidations for the vaddr
  // range, even though it found no entries. However this is not the trategy employed here at the
  // moment.
  class ConsistencyManager {
   public:
    explicit ConsistencyManager(X86PageTableImpl<T>* pt)
        : pt_(pt), clf_(static_cast<T*>(pt)->needs_cache_flushes()) {}
    ~ConsistencyManager() {
      DEBUG_ASSERT(pt_ == nullptr);

      // We free the paging structures here rather than in Finish(), to allow
      // support deferring invoking pmm_free() until after we've left the page
      // table lock.
      vm_page_t* p;
      list_for_every_entry (&to_free_, p, vm_page_t, queue_node) {
        DEBUG_ASSERT(p->state() == vm_page_state::MMU);
      }
      if (!list_is_empty(&to_free_)) {
        pmm_free(&to_free_);
      }
    }

    void queue_free(vm_page_t* page) {
      AssertHeld(pt_->lock_);
      DEBUG_ASSERT(page->state() == vm_page_state::MMU);
      list_add_tail(&to_free_, &page->queue_node);
      DEBUG_ASSERT(pt_->pages_ > 0);
      pt_->pages_--;
    }

    CacheLineFlusher* cache_line_flusher() { return &clf_; }
    PendingTlbInvalidation* pending_tlb() { return &tlb_; }

    // This function must be called while holding pt_->lock_.
    void Finish() {
      AssertHeld(pt_->lock_);

      clf_.ForceFlush();
      if (static_cast<T*>(pt_)->needs_cache_flushes()) {
        // If the hardware needs cache flushes for the tables to be visible,
        // make sure we serialize the flushes before issuing the TLB
        // invalidations.
        arch::DeviceMemoryBarrier();
      }
      static_cast<T*>(pt_)->TlbInvalidate(&tlb_);
      if (pt_->IsRestricted() && pt_->referenced_pt_ != nullptr) {
        // TODO(https://fxbug.dev/42083004): This TLB invalidation could be wrapped into the
        // preceding one so long as we built the target mask correctly.
        Guard<Mutex> a{AssertOrderedLock, &pt_->referenced_pt_->lock_,
                       pt_->referenced_pt_->LockOrder()};
        static_cast<T*>(pt_->referenced_pt_)->TlbInvalidate(&tlb_);
      }
      // Clear out the pending TLB invalidations.
      tlb_.clear();
      pt_ = nullptr;
    }

    void SetFullShootdown() { tlb_.full_shootdown = true; }

   private:
    X86PageTableImpl<T>* pt_;

    // Cache line to flush prior to TLB invalidations
    CacheLineFlusher clf_;

    // TLB invalidations that need to occur
    PendingTlbInvalidation tlb_;

    // vm_page_t's to relese to the PMM after the TLB invalidation occurs
    list_node to_free_ = LIST_INITIAL_VALUE(to_free_);
  };

  // given a page table entry, return a pointer to the next page table one level down
  static inline volatile pt_entry_t* get_next_table_from_entry(pt_entry_t entry) {
    if (!IS_PAGE_PRESENT(entry) || IS_LARGE_PAGE(entry))
      return nullptr;

    return reinterpret_cast<volatile pt_entry_t*>(X86_PHYS_TO_VIRT(entry & X86_PG_FRAME));
  }

  // Return the page size for this level
  static size_t page_size(PageTableLevel level) {
    switch (level) {
      case PageTableLevel::PT_L:
        return 1ULL << PT_SHIFT;
      case PageTableLevel::PD_L:
        return 1ULL << PD_SHIFT;
      case PageTableLevel::PDP_L:
        return 1ULL << PDP_SHIFT;
      case PageTableLevel::PML4_L:
        return 1ULL << PML4_SHIFT;
      default:
        panic("page_size: invalid level\n");
    }
  }

  // Whether an address is aligned to the page size of this level
  static bool page_aligned(PageTableLevel level, vaddr_t vaddr) {
    return (vaddr & (page_size(level) - 1)) == 0;
  }

  // Extract the index needed for finding |vaddr| for the given level
  static uint vaddr_to_index(PageTableLevel level, vaddr_t vaddr) {
    switch (level) {
      case PageTableLevel::PML4_L:
        return VADDR_TO_PML4_INDEX(vaddr);
      case PageTableLevel::PDP_L:
        return VADDR_TO_PDP_INDEX(vaddr);
      case PageTableLevel::PD_L:
        return VADDR_TO_PD_INDEX(vaddr);
      case PageTableLevel::PT_L:
        return VADDR_TO_PT_INDEX(vaddr);
      default:
        panic("vaddr_to_index: invalid level\n");
    }
  }

  // Convert a PTE to a physical address
  static paddr_t paddr_from_pte(PageTableLevel level, pt_entry_t pte) {
    DEBUG_ASSERT(IS_PAGE_PRESENT(pte));

    paddr_t pa;
    switch (level) {
      case PageTableLevel::PDP_L:
        pa = (pte & X86_HUGE_PAGE_FRAME);
        break;
      case PageTableLevel::PD_L:
        pa = (pte & X86_LARGE_PAGE_FRAME);
        break;
      case PageTableLevel::PT_L:
        pa = (pte & X86_PG_FRAME);
        break;
      default:
        panic("paddr_from_pte at unhandled level %d\n", static_cast<int>(level));
    }

    return pa;
  }

  static PageTableLevel lower_level(PageTableLevel level) {
    DEBUG_ASSERT(level != PageTableLevel::PT_L);
    return static_cast<PageTableLevel>(static_cast<int>(level) - 1);
  }

  static bool page_table_is_clear(const volatile pt_entry_t* page_table) {
    uint lower_idx;
    for (lower_idx = 0; lower_idx < NO_OF_PT_ENTRIES; ++lower_idx) {
      if (IS_PAGE_PRESENT(page_table[lower_idx])) {
        return false;
      }
    }
    return true;
  }

  /**
   * @brief Creates mappings for the range specified by start_cursor
   *
   * `level` must be top_level() when invoked from external code.
   *
   * @param table The current paging structure's virtual address.
   * @param mmu_flags MMU flags describing attributes of the mapping.
   * @param level Page table level which the current `table` is located at.
   * @param existing_action Action to take if a mapping is already present.
   * @param cursor A cursor describing the range of address space to act on.
   * @param cm Object to manage consistency of page table entries and cache+TLB.
   *
   * @return ZX_OK if successful
   * @return ZX_ERR_ALREADY_EXISTS if the range overlaps an existing mapping and
   *         `existing_action` is set to `Error`
   * @return ZX_ERR_NO_MEMORY if intermediate page tables could not be allocated
   */
  zx_status_t AddMapping(volatile pt_entry_t* table, uint mmu_flags, PageTableLevel level,
                         ExistingEntryAction existing_action, MappingCursor& cursor,
                         ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(table);
    DEBUG_ASSERT(static_cast<T*>(this)->check_vaddr(cursor.vaddr()));
    DEBUG_ASSERT(static_cast<T*>(this)->check_paddr(cursor.paddr()));
    const vaddr_t start_vaddr = cursor.vaddr();
    // Unified page tables should never be mapping entries directly; rather, their constituent page
    // tables should be mapping entries on their behalf.
    DEBUG_ASSERT(!IsUnified());

    zx_status_t ret = ZX_OK;

    if (level == PageTableLevel::PT_L) {
      return AddMappingL0(table, mmu_flags, existing_action, cursor, cm);
    }

    auto abort = fit::defer([&]() {
      AssertHeld(lock_);
      if (level == static_cast<T*>(this)->top_level()) {
        // Build an unmap cursor. cursor.size should be how much is left to be mapped still.
        MappingCursor unmap_cursor(/*vaddr=*/start_vaddr,
                                   /*size=*/cursor.vaddr() - start_vaddr);
        if (unmap_cursor.size() > 0) {
          auto status = RemoveMapping(table, level, EnlargeOperation::No, unmap_cursor, cm);
          // Removing the exact mappings we just added should never be able to fail.
          ASSERT(status.is_ok());
          DEBUG_ASSERT(unmap_cursor.size() == 0);
        }
      }
    });

    IntermediatePtFlags interm_flags = static_cast<T*>(this)->intermediate_flags();
    PtFlags term_flags = static_cast<T*>(this)->terminal_flags(level, mmu_flags);

    size_t ps = page_size(level);
    bool level_supports_large_pages = static_cast<T*>(this)->supports_page_size(level);
    uint index = vaddr_to_index(level, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;

      // See if there's a large page in our way
      if (IS_PAGE_PRESENT(pt_val) && IS_LARGE_PAGE(pt_val)) {
        if (existing_action == ExistingEntryAction::Error) {
          return ZX_ERR_ALREADY_EXISTS;
        }
        cursor.ConsumePAddr(ps);
        continue;
      }

      // Check if this is a candidate for a new large page
      bool level_valigned = page_aligned(level, cursor.vaddr());
      bool level_paligned = page_aligned(level, cursor.paddr());
      if (level_supports_large_pages && !IS_PAGE_PRESENT(pt_val) && level_valigned &&
          level_paligned && cursor.PageRemaining() >= ps) {
        UpdateEntry(cm, level, cursor.vaddr(), table + index, cursor.paddr(),
                    term_flags | X86_MMU_PG_PS, /*was_terminal=*/false);
        cursor.ConsumePAddr(ps);
      } else {
        // See if we need to create a new table.
        if (!IS_PAGE_PRESENT(pt_val)) {
          // We should never need to do this in a shared PML4.
          if (level == PageTableLevel::PML4_L) {
            DEBUG_ASSERT(!IsShared());
          }
          volatile pt_entry_t* m = AllocatePageTable();
          if (m == nullptr) {
            // The mapping wasn't fully updated, but there is work here
            // that might need to be undone.
            size_t partial_update = ktl::min(ps, cursor.size());
            // Cancel paddr tracking so we account for the virtual range we need to
            // unmap without needing to increment in page appropriate amounts.
            cursor.DropPAddrs();
            cursor.ConsumeVAddr(partial_update);
            return ZX_ERR_NO_MEMORY;
          }

          if (level == PageTableLevel::PML4_L && IsRestricted() && referenced_pt_ != nullptr) {
            Guard<Mutex> a{AssertOrderedLock, &referenced_pt_->lock_, referenced_pt_->LockOrder()};
            pt_entry_t* referenced_entry = (pt_entry_t*)referenced_pt_->virt() + index;
            DEBUG_ASSERT(check_equal_ignore_flags(*referenced_entry, *e));

            ConsistencyManager cm_referenced(referenced_pt_);
            referenced_pt_->UpdateEntry(&cm_referenced, level, cursor.vaddr(), referenced_entry,
                                        X86_VIRT_TO_PHYS(m), interm_flags,
                                        /*was_terminal=*/false);
            cm_referenced.Finish();
          }

          UpdateEntry(cm, level, cursor.vaddr(), e, X86_VIRT_TO_PHYS(m), interm_flags,
                      /*was_terminal=*/false);

          pt_val = *e;
          pages_++;
        }

        ret = AddMapping(get_next_table_from_entry(pt_val), mmu_flags, lower_level(level),
                         existing_action, cursor, cm);

        if (ret != ZX_OK) {
          return ret;
        }
      }
    }
    abort.cancel();
    return ZX_OK;
  }

  // Base case of AddMapping for smallest page size.
  zx_status_t AddMappingL0(volatile pt_entry_t* table, uint mmu_flags,
                           ExistingEntryAction existing_action, MappingCursor& cursor,
                           ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(IS_PAGE_ALIGNED(cursor.size()));

    const PtFlags term_flags =
        static_cast<T*>(this)->terminal_flags(PageTableLevel::PT_L, mmu_flags);
    uint index = vaddr_to_index(PageTableLevel::PT_L, cursor.vaddr());

    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* existing_entry = table + index;

      if (IS_PAGE_PRESENT(*existing_entry)) {
        if (existing_action == ExistingEntryAction::Upgrade) {
          const paddr_t existing_paddr = (*existing_entry) & X86_PG_FRAME;
          const bool remapping_same_address = existing_paddr == cursor.paddr();
          const bool mmu_flags_ro =
              (mmu_flags & ARCH_MMU_FLAG_PERM_RWX_MASK) == ARCH_MMU_FLAG_PERM_READ;

          // If the physical page we are trying to map is already present, and
          // we would be marking it read only, then don't.
          // Either:
          //   1. it is already read-only - we can skip the work.
          //   2. it is already writable - we shouldn't downgrade permissions.
          if (!remapping_same_address || !mmu_flags_ro) {
            UpdateEntry(cm, PageTableLevel::PT_L, cursor.vaddr(), existing_entry, cursor.paddr(),
                        term_flags, /*was_terminal=*/false);
          }
        } else if (existing_action == ExistingEntryAction::Error) {
          return ZX_ERR_ALREADY_EXISTS;
        }
      } else {
        UpdateEntry(cm, PageTableLevel::PT_L, cursor.vaddr(), existing_entry, cursor.paddr(),
                    term_flags, /*was_terminal=*/false);
      }
      cursor.ConsumePAddr(PAGE_SIZE);
    }

    return ZX_OK;
  }

  /**
   * @brief Unmaps the range specified by start_cursor.
   *
   * Level must be top_level() when invoked.  The caller must, even on
   * failure, free all pages in the |to_free| list and adjust the |pages_| count.
   *
   * @param table The top-level paging structure's virtual address.
   * @param start_cursor A cursor describing the range of address space to
   * unmap within table
   * @param new_cursor A returned cursor describing how much work was not
   * completed.  Must be non-null.
   *
   * @return true if the caller (i.e. the next level up page table) might need to
   * free this page table.
   */
  zx::result<bool> RemoveMapping(volatile pt_entry_t* table, PageTableLevel level,
                                 EnlargeOperation enlarge, MappingCursor& cursor,
                                 ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(table);
    DEBUG_ASSERT(static_cast<T*>(this)->check_vaddr(cursor.vaddr()));
    // Unified page tables should never be unmapping entries directly; rather, their constituent
    // page tables should be unmapping entries on their behalf.
    DEBUG_ASSERT(!IsUnified());

    if (level == PageTableLevel::PT_L) {
      return zx::ok(RemoveMappingL0(table, cursor, cm));
    }

    bool unmapped = false;
    // Track if there are any entries at all. This is necessary to properly rollback if an
    // attempt to map a page fails to allocate a page table, as that case can result in an
    // empty non-last-level page table.
    bool any_pages = false;
    size_t ps = page_size(level);
    uint index = vaddr_to_index(level, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;
      // If the page isn't even mapped, just skip it
      if (!IS_PAGE_PRESENT(pt_val)) {
        cursor.SkipEntry(ps);
        continue;
      }
      any_pages = true;

      if (IS_LARGE_PAGE(pt_val)) {
        bool vaddr_level_aligned = page_aligned(level, cursor.vaddr());
        // If the request covers the entire large page, just unmap it
        if (vaddr_level_aligned && cursor.size() >= ps) {
          UnmapEntry(cm, level, cursor.vaddr(), e, /*was_terminal=*/true);
          unmapped = true;

          cursor.ConsumeVAddr(ps);
          continue;
        }
        // Otherwise, we need to split it
        vaddr_t page_vaddr = cursor.vaddr() & ~(ps - 1);
        zx_status_t status = SplitLargePage(level, page_vaddr, e, cm);
        if (status != ZX_OK) {
          // If split fails, just unmap the whole thing, and let a
          // subsequent page fault clean it up.
          if (enlarge == EnlargeOperation::Yes) {
            UnmapEntry(cm, level, cursor.vaddr(), e, /*was_terminal=*/true);
            unmapped = true;

            cursor.SkipEntry(ps);
            continue;
          } else {
            return zx::error(status);
          }
        }
        pt_val = *e;
      }

      volatile pt_entry_t* next_table = get_next_table_from_entry(pt_val);
      // Remember where we are unmapping from in case we need to do a second pass to remove a PT.
      const vaddr_t unmap_vaddr = cursor.vaddr();
      auto status = RemoveMapping(next_table, lower_level(level), enlarge, cursor, cm);
      if (status.is_error()) {
        return status;
      }
      const size_t unmap_size = cursor.vaddr() - unmap_vaddr;
      bool lower_unmapped = status.value();

      // If we were requesting to unmap everything in the lower page table,
      // we know we can unmap the lower level page table.  Otherwise, if
      // we unmapped anything in the lower level, check to see if that
      // level is now empty.
      bool unmap_page_table = page_aligned(level, unmap_vaddr) && unmap_size >= ps;
      // If the top level page is shared, we cannot unmap it here as other page tables may be
      // referencing its entries.
      if (IsShared() && level == PageTableLevel::PML4_L) {
        unmap_page_table = false;
      } else if (!unmap_page_table && lower_unmapped) {
        unmap_page_table = page_table_is_clear(next_table);
      }
      if (unmap_page_table) {
        paddr_t ptable_phys = X86_VIRT_TO_PHYS(next_table);

        vm_page_t* page = paddr_to_vm_page(ptable_phys);
        if (level == PageTableLevel::PML4_L && IsRestricted() && referenced_pt_ != nullptr) {
          Guard<Mutex> a{AssertOrderedLock, &referenced_pt_->lock_, referenced_pt_->LockOrder()};
          pt_entry_t* referenced_entry = (pt_entry_t*)referenced_pt_->virt() + index;
          DEBUG_ASSERT(check_equal_ignore_flags(*referenced_entry, *e));

          ConsistencyManager cm_referenced(referenced_pt_);
          referenced_pt_->UnmapEntry(&cm_referenced, level, unmap_vaddr, referenced_entry, false);
          cm_referenced.Finish();
        }
        UnmapEntry(cm, level, unmap_vaddr, e, /*was_terminal=*/false);

        DEBUG_ASSERT(page);
        DEBUG_ASSERT_MSG(page->state() == vm_page_state::MMU,
                         "page %p state %u, paddr %#" PRIxPTR "\n", page,
                         static_cast<uint32_t>(page->state()), X86_VIRT_TO_PHYS(next_table));
        DEBUG_ASSERT(!list_in_list(&page->queue_node));

        cm->queue_free(page);
        unmapped = true;
      }

      DEBUG_ASSERT(cursor.size() == 0 || page_aligned(level, cursor.vaddr()));
    }

    return zx::ok(unmapped || !any_pages);
  }
  // Base case of RemoveMapping for smallest page size.
  bool RemoveMappingL0(volatile pt_entry_t* table, MappingCursor& cursor, ConsistencyManager* cm)
      TA_REQ(lock_) {
    DEBUG_ASSERT(IS_PAGE_ALIGNED(cursor.size()));

    bool unmapped = false;
    uint index = vaddr_to_index(PageTableLevel::PT_L, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      if (IS_PAGE_PRESENT(*e)) {
        UnmapEntry(cm, PageTableLevel::PT_L, cursor.vaddr(), e, /*was_terminal=*/true);
        unmapped = true;
      }

      cursor.ConsumeVAddr(PAGE_SIZE);
    }
    return unmapped;
  }

  /**
   * @brief Changes the permissions/caching of the range specified by start_cursor
   *
   * Level must be top_level() when invoked.  The caller must, even on
   * failure, free all pages in the |to_free| list and adjust the |pages_| count.
   *
   * @param table The top-level paging structure's virtual address.
   * @param start_cursor A cursor describing the range of address space to
   * act on within table
   * @param new_cursor A returned cursor describing how much work was not
   * completed.  Must be non-null.
   */
  zx_status_t UpdateMapping(volatile pt_entry_t* table, uint mmu_flags, PageTableLevel level,
                            MappingCursor& cursor, ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(table);
    DEBUG_ASSERT(static_cast<T*>(this)->check_vaddr(cursor.vaddr()));

    if (level == PageTableLevel::PT_L) {
      return UpdateMappingL0(table, mmu_flags, cursor, cm);
    }

    zx_status_t ret = ZX_OK;

    PtFlags term_flags = static_cast<T*>(this)->terminal_flags(level, mmu_flags);

    size_t ps = page_size(level);
    uint index = vaddr_to_index(level, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;
      // Skip unmapped pages (we may encounter these due to demand paging)
      if (!IS_PAGE_PRESENT(pt_val)) {
        cursor.SkipEntry(ps);
        continue;
      }

      if (IS_LARGE_PAGE(pt_val)) {
        bool vaddr_level_aligned = page_aligned(level, cursor.vaddr());
        // If the request covers the entire large page, just change the
        // permissions
        if (vaddr_level_aligned && cursor.size() >= ps) {
          UpdateEntry(cm, level, cursor.vaddr(), e, paddr_from_pte(level, pt_val),
                      term_flags | X86_MMU_PG_PS, /*was_terminal=*/true);
          cursor.ConsumeVAddr(ps);
          continue;
        }
        // Otherwise, we need to split it
        vaddr_t page_vaddr = cursor.vaddr() & ~(ps - 1);
        ret = SplitLargePage(level, page_vaddr, e, cm);
        if (ret != ZX_OK) {
          return ret;
        }
        pt_val = *e;
      }

      volatile pt_entry_t* next_table = get_next_table_from_entry(pt_val);
      ret = UpdateMapping(next_table, mmu_flags, lower_level(level), cursor, cm);
      if (ret != ZX_OK) {
        return ret;
      }
      DEBUG_ASSERT(cursor.size() == 0 || page_aligned(level, cursor.vaddr()));
    }
    return ZX_OK;
  }
  // Base case of UpdateMapping for smallest page size.
  zx_status_t UpdateMappingL0(volatile pt_entry_t* table, uint mmu_flags, MappingCursor& cursor,
                              ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(IS_PAGE_ALIGNED(cursor.size()));

    PtFlags term_flags = static_cast<T*>(this)->terminal_flags(PageTableLevel::PT_L, mmu_flags);

    uint index = vaddr_to_index(PageTableLevel::PT_L, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;
      // Skip unmapped pages (we may encounter these due to demand paging)
      if (IS_PAGE_PRESENT(pt_val)) {
        UpdateEntry(cm, PageTableLevel::PT_L, cursor.vaddr(), e,
                    paddr_from_pte(PageTableLevel::PT_L, pt_val), term_flags,
                    /*was_terminal=*/true);
      }

      cursor.ConsumeVAddr(PAGE_SIZE);
    }
    DEBUG_ASSERT(cursor.size() == 0 || page_aligned(PageTableLevel::PT_L, cursor.vaddr()));
    return ZX_OK;
  }
  /**
   * @brief Removes the accessed flag on any terminal entries and calls
   * pmm_page_queues()->MarkAccessed on them. For non-terminal entries any accessed bits are
   * harvested, and unaccessed non-terminal entries are unmapped or retained based on the passed in
   * action.
   *
   * Level must be top_level() when invoked.  The caller must, even on
   * failure, free all pages in the |to_free| list and adjust the |pages_| count.
   *
   * @param table The top-level paging structure's virtual address.
   * @param start_cursor A cursor describing the range of address space to
   * act on within table
   * @param new_cursor A returned cursor describing how much work was not
   * completed.  Must be non-null.
   *
   * @return true if the caller (i.e. the next level up page table) might need to
   * free this page table.
   */
  bool HarvestMapping(volatile pt_entry_t* table, NonTerminalAction non_terminal_action,
                      TerminalAction terminal_action, PageTableLevel level, MappingCursor& cursor,
                      ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(table);
    DEBUG_ASSERT(static_cast<T*>(this)->check_vaddr(cursor.vaddr()));

    if (level == PageTableLevel::PT_L) {
      HarvestMappingL0(table, terminal_action, cursor, cm);
      // HarvestMappingL0 never actually unmaps any entries, so this is always false.
      return false;
    }

    // Track if we perform any unmappings. We propagate this up to our caller, since if we performed
    // any unmappings then we could now be empty, and if so our caller needs to free us.
    bool unmapped = false;
    size_t ps = page_size(level);
    uint index = vaddr_to_index(level, cursor.vaddr());
    bool always_recurse = level == PageTableLevel::PML4_L && (IsShared() || IsRestricted());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;
      // If the page isn't even mapped, just skip it
      if (!IS_PAGE_PRESENT(pt_val)) {
        cursor.SkipEntry(ps);
        continue;
      }

      if (IS_LARGE_PAGE(pt_val)) {
        bool vaddr_level_aligned = page_aligned(level, cursor.vaddr());
        // If the request covers the entire large page then harvest the accessed bit, otherwise we
        // just skip it.
        if (vaddr_level_aligned && cursor.size() >= ps) {
          const uint mmu_flags = static_cast<T*>(this)->pt_flags_to_mmu_flags(pt_val, level);
          const PtFlags term_flags = static_cast<T*>(this)->terminal_flags(level, mmu_flags);
          UpdateEntry(cm, level, cursor.vaddr(), e, paddr_from_pte(level, pt_val),
                      term_flags | X86_MMU_PG_PS, /*was_terminal=*/true, /*exact_flags=*/true);
        }
        cursor.ConsumeVAddr(ps);
        continue;
      }

      volatile pt_entry_t* next_table = get_next_table_from_entry(pt_val);
      paddr_t ptable_phys = X86_VIRT_TO_PHYS(next_table);
      bool lower_unmapped;
      bool unmap_page_table = false;
      // Remember where we are unmapping from in case we need to do a second pass to remove a PT.
      const vaddr_t unmap_vaddr = cursor.vaddr();
      // We should recurse and HarvestMappings at the next level if:
      // 1. This page table entry is in the PML4 of a shared or restricted page table. We must
      //    always recurse in this case because entries in these page tables may have been accessed
      //    via an associated unified page table, which in turn would not set the accessed bits on
      //    the corresponding PML4 entries in this table.
      // 2. The page table entry has been accessed. We unset the AF later should we end up not
      //    unmapping the page table.
      bool should_recurse = always_recurse || (pt_val & X86_MMU_PG_A);
      if (should_recurse) {
        lower_unmapped = HarvestMapping(next_table, non_terminal_action, terminal_action,
                                        lower_level(level), cursor, cm);
      } else if (non_terminal_action == NonTerminalAction::FreeUnaccessed) {
        auto status =
            RemoveMapping(next_table, lower_level(level), EnlargeOperation::No, cursor, cm);
        // Although we pass in EnlargeOperation::No, the unmap should never fail since we are
        // unmapping an entire block and never a sub part of a page.
        ASSERT(status.is_ok());
        lower_unmapped = status.value();
        const vaddr_t unmap_size = cursor.vaddr() - unmap_vaddr;
        // If we processed the entire next level then we can ignore lower_unmapped and just directly
        // assume that the whole page table is empty/unaccessed and that we can unmap it.
        unmap_page_table = page_aligned(level, unmap_vaddr) && unmap_size >= ps;
      } else {
        // No accessed flag and no request to unmap means we are done with this entry.
        cursor.SkipEntry(ps);
        continue;
      }

      // If the lower page table was accessed and there is uncertainty around whether it might now
      // be empty, then we have to just scan it and see.
      if (!unmap_page_table && lower_unmapped) {
        unmap_page_table = page_table_is_clear(next_table);
      }
      // If the top level page is shared, we cannot unmap it here as other page tables may be
      // referencing its entries.
      if (IsShared() && level == PageTableLevel::PML4_L) {
        unmap_page_table = false;
      }
      if (unmap_page_table) {
        vm_page_t* page = paddr_to_vm_page(ptable_phys);
        if (level == PageTableLevel::PML4_L && IsRestricted() && referenced_pt_ != nullptr) {
          Guard<Mutex> a{AssertOrderedLock, &referenced_pt_->lock_, referenced_pt_->LockOrder()};
          pt_entry_t* referenced_entry = (pt_entry_t*)referenced_pt_->virt() + index;
          DEBUG_ASSERT(check_equal_ignore_flags(*referenced_entry, *e));

          ConsistencyManager cm_referenced(referenced_pt_);
          referenced_pt_->UnmapEntry(&cm_referenced, level, unmap_vaddr, referenced_entry, false);
          cm_referenced.Finish();
        }
        UnmapEntry(cm, level, unmap_vaddr, e, /*was_terminal=*/false);

        DEBUG_ASSERT(page);
        DEBUG_ASSERT_MSG(page->state() == vm_page_state::MMU,
                         "page %p state %u, paddr %#" PRIxPTR "\n", page,
                         static_cast<uint32_t>(page->state()), X86_VIRT_TO_PHYS(next_table));
        DEBUG_ASSERT(!list_in_list(&page->queue_node));

        cm->queue_free(page);
        unmapped = true;
      } else if ((pt_val & X86_MMU_PG_A) && non_terminal_action != NonTerminalAction::Retain) {
        // Since we didn't unmap, we need to unset the accessed flag.
        const IntermediatePtFlags flags = static_cast<T*>(this)->intermediate_flags();
        UpdateEntry(cm, level, unmap_vaddr, e, ptable_phys, flags, /*was_terminal=*/false,
                    /*exact_flags=*/true);
        // For the accessed flag to reliably reset we need to ensure that any leaf pages from here
        // are not in the TLB so that a re-walk occurs. To avoid having to find every leaf page,
        // which will probably exceed the consistency managers into count anyway, force trigger a
        // full shootdown.
        cm->SetFullShootdown();
      }
      DEBUG_ASSERT(cursor.size() == 0 || page_aligned(level, cursor.vaddr()));
    }
    return unmapped;
  }
  // Base case of HarvestMapping for smallest page size.
  void HarvestMappingL0(volatile pt_entry_t* table, TerminalAction terminal_action,
                        MappingCursor& cursor, ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT(IS_PAGE_ALIGNED(cursor.size()));

    uint index = vaddr_to_index(PageTableLevel::PT_L, cursor.vaddr());
    for (; index != NO_OF_PT_ENTRIES && cursor.size() != 0; ++index) {
      volatile pt_entry_t* e = table + index;
      pt_entry_t pt_val = *e;
      if (IS_PAGE_PRESENT(pt_val) && (pt_val & X86_MMU_PG_A)) {
        const paddr_t paddr = paddr_from_pte(PageTableLevel::PT_L, pt_val);
        const uint mmu_flags =
            static_cast<T*>(this)->pt_flags_to_mmu_flags(pt_val, PageTableLevel::PT_L);
        const PtFlags term_flags =
            static_cast<T*>(this)->terminal_flags(PageTableLevel::PT_L, mmu_flags);

        vm_page_t* page = paddr_to_vm_page(paddr);
        // Mappings for physical VMOs do not have pages associated with them and so there's no state
        // to update on an access. As the hardware will update any higher level accessed bits for us
        // we do not even ned to remove the accessed bit in that case.
        if (likely(page)) {
          pmm_page_queues()->MarkAccessedDeferredCount(page);

          if (terminal_action == TerminalAction::UpdateAgeAndHarvest) {
            UpdateEntry(cm, PageTableLevel::PT_L, cursor.vaddr(), e,
                        paddr_from_pte(PageTableLevel::PT_L, pt_val), term_flags,
                        /*was_terminal=*/true, /*exact_flags=*/true);
          }
        }
      }

      cursor.ConsumeVAddr(PAGE_SIZE);
    }
    DEBUG_ASSERT(cursor.size() == 0 || page_aligned(PageTableLevel::PT_L, cursor.vaddr()));
  }

  /**
   * @brief  Walk the page table structures returning the entry and level that maps the address.
   *
   * @param table The top-level paging structure's virtual address
   * @param vaddr The virtual address to retrieve the mapping for
   * @param ret_level The level of the table that defines the found mapping
   * @param mapping The mapping that was found
   *
   * @return ZX_OK if mapping is found
   * @return ZX_ERR_NOT_FOUND if mapping is not found
   */
  zx_status_t GetMapping(volatile pt_entry_t* table, vaddr_t vaddr, PageTableLevel level,
                         PageTableLevel* ret_level, volatile pt_entry_t** mapping) TA_REQ(lock_) {
    DEBUG_ASSERT(table);
    DEBUG_ASSERT(ret_level);
    DEBUG_ASSERT(mapping);

    if (level == PageTableLevel::PT_L) {
      return GetMappingL0(table, vaddr, ret_level, mapping);
    }

    uint index = vaddr_to_index(level, vaddr);
    volatile pt_entry_t* e = table + index;
    pt_entry_t pt_val = *e;
    if (!IS_PAGE_PRESENT(pt_val))
      return ZX_ERR_NOT_FOUND;

    /* if this is a large page, stop here */
    if (IS_LARGE_PAGE(pt_val)) {
      *mapping = e;
      *ret_level = level;
      return ZX_OK;
    }

    volatile pt_entry_t* next_table = get_next_table_from_entry(pt_val);
    return GetMapping(next_table, vaddr, lower_level(level), ret_level, mapping);
  }
  zx_status_t GetMappingL0(volatile pt_entry_t* table, vaddr_t vaddr,
                           enum PageTableLevel* ret_level, volatile pt_entry_t** mapping)
      TA_REQ(lock_) {
    /* do the final page table lookup */
    uint index = vaddr_to_index(PageTableLevel::PT_L, vaddr);
    volatile pt_entry_t* e = table + index;
    if (!IS_PAGE_PRESENT(*e))
      return ZX_ERR_NOT_FOUND;

    *mapping = e;
    *ret_level = PageTableLevel::PT_L;
    return ZX_OK;
  }

  // Split the given large page into smaller pages
  zx_status_t SplitLargePage(PageTableLevel level, vaddr_t vaddr, volatile pt_entry_t* pte,
                             ConsistencyManager* cm) TA_REQ(lock_) {
    DEBUG_ASSERT_MSG(level != PageTableLevel::PT_L, "tried splitting PT_L");

    DEBUG_ASSERT(IS_PAGE_PRESENT(*pte) && IS_LARGE_PAGE(*pte));
    volatile pt_entry_t* m = AllocatePageTable();
    if (m == nullptr) {
      return ZX_ERR_NO_MEMORY;
    }

    paddr_t paddr_base = paddr_from_pte(level, *pte);
    PtFlags flags = static_cast<T*>(this)->split_flags(level, *pte & X86_LARGE_FLAGS_MASK);

    DEBUG_ASSERT(page_aligned(level, vaddr));
    vaddr_t new_vaddr = vaddr;
    paddr_t new_paddr = paddr_base;
    size_t ps = page_size(lower_level(level));
    for (int i = 0; i < NO_OF_PT_ENTRIES; i++) {
      volatile pt_entry_t* e = m + i;
      // If this is a PDP_L (i.e. huge page), flags will include the
      // PS bit still, so the new PD entries will be large pages.
      UpdateEntry(cm, lower_level(level), new_vaddr, e, new_paddr, flags, /*was_terminal=*/false);
      new_vaddr += ps;
      new_paddr += ps;
    }
    DEBUG_ASSERT(new_vaddr == vaddr + page_size(level));

    flags = static_cast<T*>(this)->intermediate_flags();
    UpdateEntry(cm, level, vaddr, pte, X86_VIRT_TO_PHYS(m), flags, /*was_terminal=*/true);
    pages_++;
    return ZX_OK;
  }

  void UpdateEntry(ConsistencyManager* cm, PageTableLevel level, vaddr_t vaddr,
                   volatile pt_entry_t* pte, paddr_t paddr, PtFlags flags, bool was_terminal,
                   bool exact_flags = false) TA_REQ(lock_) {
    DEBUG_ASSERT(pte);
    DEBUG_ASSERT(IS_PAGE_ALIGNED(paddr));

    pt_entry_t olde = *pte;
    pt_entry_t newe = paddr | flags | X86_MMU_PG_P;

    // Check if we are actually changing anything, ignoring the accessed and dirty bits unless
    // exact_flags has been requested to allow for those bits to be explicitly unset.
    if ((olde & ~(exact_flags ? 0 : (X86_MMU_PG_A | X86_MMU_PG_D))) == newe) {
      return;
    }

    if (level == PageTableLevel::PML4_L && IsShared()) {
      // If this is a shared page table, the only possible modification should be removal of
      // the accessed flag.
      DEBUG_ASSERT(olde == (newe | X86_MMU_PG_A));
    }
    /* set the new entry */
    *pte = newe;
    cm->cache_line_flusher()->FlushPtEntry(pte);

    /* attempt to invalidate the page */
    if (IS_PAGE_PRESENT(olde)) {
      cm->pending_tlb()->enqueue(vaddr, level, /*is_global_page=*/olde & X86_MMU_PG_G,
                                 was_terminal);
    }
  }
  void UnmapEntry(ConsistencyManager* cm, PageTableLevel level, vaddr_t vaddr,
                  volatile pt_entry_t* pte, bool was_terminal) TA_REQ(lock_) {
    DEBUG_ASSERT(pte);
    if (level == PageTableLevel::PML4_L) {
      DEBUG_ASSERT(!IsShared());
    }

    pt_entry_t olde = *pte;

    *pte = 0;
    cm->cache_line_flusher()->FlushPtEntry(pte);

    /* attempt to invalidate the page */
    if (IS_PAGE_PRESENT(olde)) {
      cm->pending_tlb()->enqueue(vaddr, level, /*is_global_page=*/olde & X86_MMU_PG_G,
                                 was_terminal);
    }
  }

  // Allocating a new page table
  // Release the resources associated with this page table.  |base| and |size|
  // are only used for debug checks that the page tables have no more mappings.
  void DestroyIndividual(vaddr_t base, size_t size) {
    DEBUG_ASSERT(!IsUnified());

    // This lock should be uncontended since Destroy is not supposed to be called in parallel with
    // any other operation, but hold it anyway so we can clear virt_ and attempt to surface any
    // bugs.
    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
    DEBUG_ASSERT(num_references_ == 0);

    // If this page table has a shared top level page, we need to manually clean up the entries we
    // created in InitShared. We know for sure that these entries are no longer referenced by
    // other page tables because we expect those page tables to have been destroyed before this one.
    if (IsShared()) {
      DEBUG_ASSERT(virt_ != nullptr);

      PageTableLevel top = static_cast<T*>(this)->top_level();
      pt_entry_t* table = static_cast<pt_entry_t*>(virt_);
      const uint start = vaddr_to_index(top, base);
      uint end = vaddr_to_index(top, base + size - 1);
      // Check the end if it fills out the table entry.
      if (page_aligned(top, base + size)) {
        end += 1;
      }
      for (uint i = start; i < end; i++) {
        if (IS_PAGE_PRESENT(table[i])) {
          volatile pt_entry_t* next_table = get_next_table_from_entry(table[i]);
          paddr_t ptable_phys = X86_VIRT_TO_PHYS(next_table);
          vm_page_t* page = paddr_to_vm_page(ptable_phys);
          pmm_free_page(page);
          table[i] = 0;
        }
      }
    }

    if constexpr (DEBUG_ASSERT_IMPLEMENTED) {
      PageTableLevel top = static_cast<T*>(this)->top_level();
      if (virt_) {
        pt_entry_t* table = static_cast<pt_entry_t*>(virt_);
        const uint start = vaddr_to_index(top, base);
        uint end = vaddr_to_index(top, base + size - 1);

        // Check the end if it fills out the table entry.
        if (page_aligned(top, base + size)) {
          end += 1;
        }

        for (uint i = start; i < end; ++i) {
          DEBUG_ASSERT_MSG(!IS_PAGE_PRESENT(table[i]),
                           "Destroy() called on page table with entry 0x%" PRIx64
                           " still present at index %u; aspace size: %zu, is_shared_: %d\n",
                           table[i], i, size, IsShared());
        }
      }
    }
    FreeTopLevelPage();
  }

  // Releases the resources exclusively owned by this unified page table, and update the relevant
  // metadata on the associated restricted and shared page tables.
  void DestroyUnified() {
    DEBUG_ASSERT(IsUnified());

    X86PageTableImpl<T>* restricted = nullptr;
    X86PageTableImpl<T>* shared = nullptr;
    {
      // This lock should be uncontended since Destroy is not supposed to be called in parallel with
      // any other operation, but hold it anyway so we can clear virt_ and attempt to surface any
      // bugs. We limit the scope in which we hold this lock when destroying unified page tables
      // because holding it prior to acquiring the shared and restricted page table locks would
      // violate the lock's ordering rules. We do not destroy the unified page table here, as the
      // restricted page table may still reference it.
      Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
      // We can copy these pointers to local variables and use them outside of this critical section
      // because they are notionally const for unified page tables.
      restricted = referenced_pt_;
      shared = shared_pt_;
      shared_pt_ = nullptr;
      referenced_pt_ = nullptr;
    }
    {
      Guard<Mutex> a{AssertOrderedLock, &shared->lock_, shared->LockOrder()};
      // The shared page table should be referenced by at least this page table, and could be
      // referenced by many other unified page tables.
      DEBUG_ASSERT(shared->num_references_ > 0);
      shared->num_references_--;
    }
    {
      Guard<Mutex> a{AssertOrderedLock, &restricted->lock_, restricted->LockOrder()};
      // The restricted page table can only be referenced by a singular unified page table.
      DEBUG_ASSERT(restricted->num_references_ == 1);

      restricted->num_references_--;
      restricted->referenced_pt_ = nullptr;
    }

    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
    FreeTopLevelPage();
  }

  // Frees the top level page in this page table.
  void FreeTopLevelPage() TA_REQ(lock_) {
    if (phys_) {
      pmm_free_page(paddr_to_vm_page(phys_));
      phys_ = 0;
    }

    // Clear virt_ to indicate we are now destroyed, and prevent any misuses of the ArchVmAspace API
    // from performing use-after-free on the PT.
    virt_ = nullptr;
  }

  // Checks that the given page table entries are equal but ignores the accessed and dirty flags.
  bool check_equal_ignore_flags(pt_entry_t left, pt_entry_t right) {
    pt_entry_t no_accessed_dirty_mask = ~X86_MMU_PG_A & ~X86_MMU_PG_D;
    return (left & no_accessed_dirty_mask) == (right & no_accessed_dirty_mask);
  }

  // A reference to another page table that shares entries with this one.
  // If is_restricted_ is set to true, this references the associated unified page table.
  // If is_unified_ is set to true, this references the associated restricted page table.
  // If neither is true, this is set to null.
  X86PageTableImpl<T>* referenced_pt_ TA_GUARDED(lock_) = nullptr;

  // A reference to a shared page table whose mappings are also present in this page table. This is
  // only set for unified page tables.
  X86PageTableImpl<T>* shared_pt_ TA_GUARDED(lock_) = nullptr;
};

#endif  // ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_
