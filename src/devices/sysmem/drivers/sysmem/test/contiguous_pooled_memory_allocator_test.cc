// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "contiguous_pooled_memory_allocator.h"

#include <lib/async-loop/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fake-bti/bti.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmar.h>
#include <zircon/syscalls.h>

#include <vector>

#include <fbl/algorithm.h>
#include <zxtest/zxtest.h>

namespace sysmem_driver {
namespace {

class FakeOwner : public MemoryAllocator::Owner {
 public:
  explicit FakeOwner(inspect::Node* heap_node) : heap_node_(heap_node) {
    EXPECT_OK(fake_bti_create(bti_.reset_and_get_address()));
  }

  ~FakeOwner() {}

  const zx::bti& bti() override { return bti_; }
  zx_status_t CreatePhysicalVmo(uint64_t base, uint64_t size, zx::vmo* vmo_out) override {
    return zx::vmo::create(size, 0u, vmo_out);
  }
  inspect::Node* heap_node() override { return heap_node_; }
  SysmemMetrics& metrics() override { return metrics_; }

 private:
  inspect::Node* heap_node_;
  zx::bti bti_;
  SysmemMetrics metrics_;
};

class ContiguousPooledSystem : public zxtest::Test {
 public:
  ContiguousPooledSystem()
      : allocator_(&fake_owner_, kVmoName, &inspector_.GetRoot(), 0u, kVmoSize * kVmoCount,
                   true,       // is_always_cpu_accessible
                   true,       // is_ever_cpu_accessible
                   false,      // is_ready
                   true,       // can_be_torn_down
                   nullptr) {  // dispatcher
    // nothing else to do here
  }

  zx_status_t PrepareAllocator() {
    zx_status_t status = allocator_.Init();
    if (status != ZX_OK) {
      return status;
    }
    allocator_.SetBtiFakeForUnitTests();
    allocator_.set_ready();
    return ZX_OK;
  }

 protected:
  static constexpr uint32_t kVmoSize = 4096;
  static constexpr uint32_t kVmoCount = 1024;
  static constexpr uint32_t kBigVmoSize = kVmoSize * kVmoCount / 2;
  static constexpr char kVmoName[] = "test-pool";
  static constexpr uint64_t kBufferCollectionId = 12;
  static constexpr uint32_t kBufferIndex = 24;

  inspect::Inspector inspector_;
  FakeOwner fake_owner_{&inspector_.GetRoot()};
  ContiguousPooledMemoryAllocator allocator_;
};

TEST_F(ContiguousPooledSystem, VmoNamesAreSet) {
  EXPECT_OK(PrepareAllocator());

  char name[ZX_MAX_NAME_LEN] = {};
  EXPECT_OK(allocator_.GetPoolVmoForTest().get_property(ZX_PROP_NAME, name, sizeof(name)));
  EXPECT_EQ(0u, strcmp(kVmoName, name));

  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize - 42;
  EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  EXPECT_OK(vmo.get_property(ZX_PROP_NAME, name, sizeof(name)));
  EXPECT_EQ(0u, strcmp("test-pool-child", name));
  allocator_.Delete(std::move(vmo));
}

TEST_F(ContiguousPooledSystem, Full) {
  EXPECT_OK(PrepareAllocator());

  auto hierarchy = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  auto* value = hierarchy.value().GetByPath({"test-pool"});
  ASSERT_TRUE(value);

  EXPECT_LT(
      0u,
      value->node().get_property<inspect::UintPropertyValue>("free_at_high_water_mark")->value());

  std::vector<zx::vmo> vmos;
  for (uint32_t i = 0; i < kVmoCount; ++i) {
    zx::vmo vmo;
    fuchsia_sysmem2::SingleBufferSettings settings;
    settings.buffer_settings().emplace();
    settings.buffer_settings()->size_bytes() = kVmoSize - 100;
    EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
    vmos.push_back(std::move(vmo));
  }

  EXPECT_EQ(0u, value->node()
                    .get_property<inspect::UintPropertyValue>("last_allocation_failed_timestamp_ns")
                    ->value());
  auto before_time = zx::clock::get_monotonic();
  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize;
  EXPECT_NOT_OK(
      allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));

  auto after_time = zx::clock::get_monotonic();

  hierarchy = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  value = hierarchy.value().GetByPath({"test-pool"});
  EXPECT_LE(before_time.get(),
            value->node()
                .get_property<inspect::UintPropertyValue>("last_allocation_failed_timestamp_ns")
                ->value());
  EXPECT_GE(after_time.get(),
            value->node()
                .get_property<inspect::UintPropertyValue>("last_allocation_failed_timestamp_ns")
                ->value());

  allocator_.Delete(std::move(vmos[0]));

  EXPECT_OK(
      allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmos[0]));

  // Destroy half of all vmos.
  for (uint32_t i = 0; i < kVmoCount; i += 2) {
    ZX_DEBUG_ASSERT(vmos[i]);
    allocator_.Delete(std::move(vmos[i]));
  }

  // There shouldn't be enough contiguous address space for even 1 extra byte.
  // This check relies on sequential Allocate() calls to a brand-new allocator
  // being laid out sequentially, so isn't a fundamental check - if the
  // allocator's layout strategy changes this check might start to fail
  // without there necessarily being a real problem.
  settings.buffer_settings()->size_bytes() = kVmoSize + 1;
  EXPECT_NOT_OK(allocator_.Allocate(
      fbl::round_up(*settings.buffer_settings()->size_bytes(), zx_system_get_page_size()), settings,
      "", kBufferCollectionId, kBufferIndex, &vmo));

  // This allocation should fail because there's not enough space in the pool, with or without
  // fragmentation.:
  settings.buffer_settings()->size_bytes() = kVmoSize * kVmoCount - 1;
  EXPECT_NOT_OK(allocator_.Allocate(
      fbl::round_up(*settings.buffer_settings()->size_bytes(), zx_system_get_page_size()), settings,
      "", kBufferCollectionId, kBufferIndex, &vmo));

  hierarchy = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  value = hierarchy.value().GetByPath({"test-pool"});
  EXPECT_EQ(3u,
            value->node().get_property<inspect::UintPropertyValue>("allocations_failed")->value());
  EXPECT_EQ(1u, value->node()
                    .get_property<inspect::UintPropertyValue>("allocations_failed_fragmentation")
                    ->value());
  // All memory was used at high water.
  EXPECT_EQ(
      0u,
      value->node().get_property<inspect::UintPropertyValue>("max_free_at_high_water")->value());
  EXPECT_EQ(
      0u,
      value->node().get_property<inspect::UintPropertyValue>("free_at_high_water_mark")->value());
  for (auto& vmo : vmos) {
    if (vmo)
      allocator_.Delete(std::move(vmo));
  }
}

TEST_F(ContiguousPooledSystem, GetPhysicalMemoryInfo) {
  EXPECT_OK(PrepareAllocator());

  zx_paddr_t base;
  size_t size;
  ASSERT_OK(allocator_.GetPhysicalMemoryInfo(&base, &size));
  EXPECT_EQ(base, FAKE_BTI_PHYS_ADDR);
  EXPECT_EQ(size, kVmoSize * kVmoCount);
}

TEST_F(ContiguousPooledSystem, InitPhysical) {
  // Using fake-bti and the FakeOwner above, it won't be a real physical VMO anyway.
  EXPECT_OK(allocator_.InitPhysical(FAKE_BTI_PHYS_ADDR));
  allocator_.SetBtiFakeForUnitTests();
  allocator_.set_ready();

  zx_paddr_t base;
  size_t size;
  ASSERT_OK(allocator_.GetPhysicalMemoryInfo(&base, &size));
  EXPECT_EQ(base, FAKE_BTI_PHYS_ADDR);
  EXPECT_EQ(size, kVmoSize * kVmoCount);

  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize;
  EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  allocator_.Delete(std::move(vmo));
}

TEST_F(ContiguousPooledSystem, SetReady) {
  EXPECT_OK(allocator_.Init());
  allocator_.SetBtiFakeForUnitTests();
  EXPECT_FALSE(allocator_.is_ready());
  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize;
  EXPECT_EQ(ZX_ERR_BAD_STATE,
            allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  allocator_.set_ready();
  EXPECT_TRUE(allocator_.is_ready());
  EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  allocator_.Delete(std::move(vmo));
}

TEST_F(ContiguousPooledSystem, GuardPages) {
  async::TestLoop loop;
  const uint32_t kGuardRegionSize = zx_system_get_page_size();
  EXPECT_OK(allocator_.Init());
  allocator_.SetBtiFakeForUnitTests();
  const zx::duration kUnusedPageCheckCyclePeriod = zx::sec(600);
  allocator_.InitGuardRegion(kGuardRegionSize, /*unused_pages_guarded=*/false,
                             kUnusedPageCheckCyclePeriod, /*internal_guard_regions=*/true,
                             /*crash_on_guard_failure=*/false, loop.dispatcher());
  allocator_.SetupUnusedPages();
  allocator_.set_ready();

  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize;
  EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());

  // The guard check happens every 5 seconds, so run for 6 seconds to ensure one
  // happens. We're using a test loop, so it's guaranteed that it runs exactly this length of time.
  constexpr uint32_t kLoopTimeSeconds = 6;
  loop.RunFor(zx::sec(kLoopTimeSeconds));

  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());

  uint8_t data_to_write = 1;
  uint64_t guard_offset = allocator_.GetVmoRegionOffsetForTest(vmo) - 1;
  allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));

  guard_offset = allocator_.GetVmoRegionOffsetForTest(vmo) + kVmoSize + kGuardRegionSize - 1;
  allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));

  loop.RunFor(zx::sec(kLoopTimeSeconds));

  // One each for beginning and end.
  EXPECT_EQ(2u, allocator_.failed_guard_region_checks());
  allocator_.Delete(std::move(vmo));
  // Two more.
  EXPECT_EQ(4u, allocator_.failed_guard_region_checks());
}

TEST_F(ContiguousPooledSystem, ExternalGuardPages) {
  async::TestLoop loop;
  const uint32_t kGuardRegionSize = zx_system_get_page_size();
  EXPECT_OK(allocator_.Init());
  allocator_.SetBtiFakeForUnitTests();
  const zx::duration kUnusedPageCheckCyclePeriod = zx::sec(2);
  allocator_.InitGuardRegion(kGuardRegionSize, /*unused_pages_guarded=*/true,
                             kUnusedPageCheckCyclePeriod, /*internal_guard_regions=*/false,
                             /*crash_on_guard_failure=*/false, loop.dispatcher());
  allocator_.SetupUnusedPages();
  allocator_.set_ready();

  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kVmoSize;
  EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());
  // The guard check happens every 5 seconds, so run for 6 seconds to ensure one
  // happens. We're using a test loop, so it's guaranteed that it runs exactly this length of time.
  constexpr uint32_t kLoopTimeSeconds = 6;

  loop.RunFor(zx::sec(kLoopTimeSeconds));

  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());

  uint8_t data_to_write = 1;
  uint64_t guard_offset = 1;
  allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));

  guard_offset = kVmoSize * kVmoCount - 1;
  allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));

  {
    // Write into what would be the internal guard region, to check that it isn't caught.
    uint8_t data_to_write = 1;
    uint64_t guard_offset = allocator_.GetVmoRegionOffsetForTest(vmo) - 1;
    allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));

    guard_offset = allocator_.GetVmoRegionOffsetForTest(vmo) + kVmoSize + kGuardRegionSize - 1;
    allocator_.GetPoolVmoForTest().write(&data_to_write, guard_offset, sizeof(data_to_write));
  }

  loop.RunFor(zx::sec(kLoopTimeSeconds));

  // One each for beginning and end.
  EXPECT_EQ(2u, allocator_.failed_guard_region_checks());
  allocator_.Delete(std::move(vmo));
  // Deleting the allocator won't cause an external guard region check, so the count should be the
  // same.
  EXPECT_EQ(2u, allocator_.failed_guard_region_checks());
}

TEST_F(ContiguousPooledSystem, UnusedGuardPages) {
  async::TestLoop loop;
  const zx::duration unused_page_check_cycle_period = zx::sec(2);
  const uint32_t loop_time_seconds = unused_page_check_cycle_period.to_secs() + 1;

  EXPECT_EQ(0, kBigVmoSize % (ContiguousPooledMemoryAllocator::kUnusedGuardPatternPeriodPages *
                              zx_system_get_page_size()));

  EXPECT_OK(allocator_.Init());
  allocator_.InitGuardRegion(0, true, unused_page_check_cycle_period, false, false,
                             loop.dispatcher());
  allocator_.SetupUnusedPages();
  allocator_.set_ready();

  zx::vmo vmo;
  fuchsia_sysmem2::SingleBufferSettings settings;
  settings.buffer_settings().emplace();
  settings.buffer_settings()->size_bytes() = kBigVmoSize;
  EXPECT_OK(
      allocator_.Allocate(kBigVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());
  // This test is implicitly relying on this allocator behavior for now.
  //
  // TODO(dustingreen): Plumb required alignment down to region allocator so we can eliminate a bit
  // of overhead for a few allocations and so this test can specify
  // ContiguousPooledMemoryAllocator::kUnusedGuardPatternPeriodPages alignment requirement instead
  // of just asserting that alignment.  For now this will be true based on internal behavior of
  // RegionAllocator, but that's not guaranteed by the RegionAllocator interface contract.
  EXPECT_EQ(0, allocator_.GetVmoRegionOffsetForTest(vmo) %
                   (ContiguousPooledMemoryAllocator::kUnusedGuardPatternPeriodPages *
                    zx_system_get_page_size()));

  printf("check for spurious pattern check failure...\n");

  // The guard check happens every 5 seconds, so run for 6 seconds to ensure one
  // happens. We're using a test loop, so it's guaranteed that it runs exactly this length of time.
  loop.RunFor(zx::sec(loop_time_seconds));
  EXPECT_EQ(0u, allocator_.failed_guard_region_checks());

  printf("one write that's inside a vmo, one that's outside of any vmo ever...\n");

  uint8_t data_to_write = 12;
  uint64_t vmo_offset = allocator_.GetVmoRegionOffsetForTest(vmo);
  EXPECT_OK(
      allocator_.GetPoolVmoForTest().write(&data_to_write, vmo_offset, sizeof(data_to_write)));

  uint64_t just_outside_vmo_offset;
  // pick an offset we know is inside the overall allocator space.
  if (vmo_offset) {
    just_outside_vmo_offset =
        vmo_offset -
        ContiguousPooledMemoryAllocator::kUnusedGuardPatternPeriodPages * zx_system_get_page_size();
  } else {
    just_outside_vmo_offset = vmo_offset + kBigVmoSize;
  }
  ZX_ASSERT(just_outside_vmo_offset %
                (ContiguousPooledMemoryAllocator::kUnusedGuardPatternPeriodPages *
                 zx_system_get_page_size()) ==
            0);
  ZX_ASSERT(just_outside_vmo_offset >= 0 && just_outside_vmo_offset < kVmoSize * kVmoCount);
  EXPECT_OK(allocator_.GetPoolVmoForTest().write(&data_to_write, just_outside_vmo_offset,
                                                 sizeof(data_to_write)));

  loop.RunFor(zx::sec(loop_time_seconds));

  // One mismatch in unused pages.  The page is written back with the correct pattern when the
  // mismatch is noticed, partly so we can have this test say EQ instead of LE.
  EXPECT_EQ(1u, allocator_.failed_guard_region_checks());
  allocator_.Delete(std::move(vmo));

  printf("one write to first byte of first page of former vmo...\n");

  // Create another mismatch that's a write-after-free.
  EXPECT_OK(
      allocator_.GetPoolVmoForTest().write(&data_to_write, vmo_offset, sizeof(data_to_write)));
  loop.RunFor(zx::sec(loop_time_seconds));
  EXPECT_EQ(2u, allocator_.failed_guard_region_checks());

  printf("write to 1st byte of 32 pages of former vmo's location...\n");

  for (uint64_t offset = 0; offset < 32ull * zx_system_get_page_size();
       offset += zx_system_get_page_size()) {
    EXPECT_OK(allocator_.GetPoolVmoForTest().write(&data_to_write, vmo_offset + offset,
                                                   sizeof(data_to_write)));
  }
  loop.RunFor(zx::sec(loop_time_seconds));
  // Not strictly required to find the multi-page problem in a single pass - may involve > 1 pass.
  //
  // May also only detect the page at vmo_offset due to kUnusedGuardPatternPeriodPages and
  // kUnusedToPatternPages.
  //
  // If the multi-page problem is found in >= 1 passes, we'll have at least one more failure than
  // previous count which was 2, so we're looking for >= 3 here.
  EXPECT_GE(allocator_.failed_guard_region_checks(), 3u);

  printf("write over first 32 pages of former vmo's location...\n");

  uint64_t count_before = allocator_.failed_guard_region_checks();
  for (uint64_t offset = 0; offset < 32 * zx_system_get_page_size(); ++offset) {
    EXPECT_OK(allocator_.GetPoolVmoForTest().write(&data_to_write, vmo_offset + offset,
                                                   sizeof(data_to_write)));
  }
  loop.RunFor(zx::sec(loop_time_seconds));
  EXPECT_LT(count_before, allocator_.failed_guard_region_checks());

  printf("done\n");
}

TEST_F(ContiguousPooledSystem, FreeRegionReporting) {
  EXPECT_OK(PrepareAllocator());

  std::vector<zx::vmo> vmos;
  for (uint32_t i = 0; i < kVmoCount; ++i) {
    zx::vmo vmo;
    fuchsia_sysmem2::SingleBufferSettings settings;
    settings.buffer_settings().emplace();
    settings.buffer_settings()->size_bytes() = kVmoSize;
    EXPECT_OK(allocator_.Allocate(kVmoSize, settings, "", kBufferCollectionId, kBufferIndex, &vmo));
    vmos.push_back(std::move(vmo));
  }

  // We want this pattern: blank filled blank blank filled ...
  for (uint32_t i = 0; i < kVmoCount - 5; i += 5) {
    allocator_.Delete(std::move(vmos[i]));
    allocator_.Delete(std::move(vmos[i + 2]));
    allocator_.Delete(std::move(vmos[i + 3]));
  }

  auto hierarchy = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  auto* value = hierarchy.value().GetByPath({"test-pool"});
  ASSERT_TRUE(value);

  // There should be at least 10 regions each with 2 adjacent VMOs free.
  EXPECT_EQ(10 * 2u * kVmoSize,
            value->node()
                .get_property<inspect::UintPropertyValue>("large_contiguous_region_sum")
                ->value());
  for (auto& vmo : vmos) {
    if (vmo)
      allocator_.Delete(std::move(vmo));
  }
}

}  // namespace
}  // namespace sysmem_driver
