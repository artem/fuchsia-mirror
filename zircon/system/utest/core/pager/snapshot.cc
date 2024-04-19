// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/zx/bti.h>
#include <lib/zx/iommu.h>
#include <lib/zx/result.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/iommu.h>

#include <zxtest/zxtest.h>

#include "helpers.h"
#include "test_thread.h"
#include "userpager.h"

namespace pager_tests {

// Helper struct that can be used to re run a similar test on different levels of a VMO hierarchy
enum class PageDepth { root, clone, snapshot };

// Smoke test
TEST(Snapshot, Smoke) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);

  // Create first level clone. Should work with either kind of snapshot.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // Fork a page in the clone, supplying the initial content as needed.
  TestThread t([clone = clone.get()]() -> bool {
    *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;
    return true;
  });
  ASSERT_TRUE(t.Start());
  ASSERT_TRUE(pager.WaitForPageRead(vmo, 0, 1, ZX_TIME_INFINITE));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_TRUE(t.Wait());
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);

  // Now snapshot-ish the clone.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // Both should see the same previous modification.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);

  // Modifying clone should not modify the snapshot.
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = clone->key();

  // Page in hidden node should now be attributed to snapshot.
  ASSERT_TRUE(snapshot->PollPopulatedBytes(zx_system_get_page_size()));

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), clone->key());
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);

  // Check attribution.
  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));
}

// Snapshot-at-least-on-write after snapshot-modified should upgrade to snapshot-modified.
TEST(Snapshot, AtLeastOnWriteAfterSnapshotModified) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;

  auto alow = snapshot->Clone(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE);
  ASSERT_NOT_NULL(alow);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(alow->base_addr()), 0xdead1eaf);

  // Write to snapshot & ensure alow doesn't see it
  *reinterpret_cast<uint64_t*>(snapshot->base_addr()) = 0xfff;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xfff);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(alow->base_addr()), 0xdead1eaf);

  // drop snapshot
  snapshot.reset();
}

// Snapshot-modified after multuple snapshot-at-least-on-writes of the root VMO.
TEST(Snapshot, SnapshotModifiedAfterAtLeastOnWrite) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));

  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // Hang two at-least-on-write clones off the root.
  auto alow1 = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE);
  ASSERT_NOT_NULL(alow1);

  auto alow2 = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE);
  ASSERT_NOT_NULL(alow2);

  // Snapshot one of the at-least-on-write clones
  auto alow_snapshot = alow1->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(alow_snapshot);

  // Snapshot-modified the root VMO twice, which should work.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(alow1->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(alow2->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(alow_snapshot->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xc0ffee);
}

// General test that dropping VMOs behaves as expected.
TEST(Snapshot, DropVmos) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;
  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(vmo->PollNumChildren(0));

  // Clone & write different value to pages.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  ASSERT_TRUE(clone->PollPopulatedBytes(0));
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = 0xc0ffee;
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(vmo->PollNumChildren(1));
  ASSERT_TRUE(clone->PollNumChildren(0));

  // Snapshot twice, writing to the first snapshot so they share a page.
  auto full_snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_snapshot);
  *reinterpret_cast<uint64_t*>(full_snapshot->base_addr()) = 0xbee5;
  *reinterpret_cast<uint64_t*>(full_snapshot->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;

  auto partial_snapshot = full_snapshot->Clone(0, 1, ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(partial_snapshot);

  ASSERT_TRUE(full_snapshot->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(partial_snapshot->PollPopulatedBytes(0));

  // drop full snapshot, which will release one of it's pages & give the other to partial snapshot
  full_snapshot.reset();
  ASSERT_TRUE(partial_snapshot->PollPopulatedBytes(zx_system_get_page_size()));
  ASSERT_TRUE(partial_snapshot->PollNumChildren(0));

  // drop partial snapshot, which sholud move the clone into being the single child of the root VMO.
  partial_snapshot.reset();

  ASSERT_TRUE(vmo->PollNumChildren(1));
  ASSERT_TRUE(clone->PollNumChildren(0));
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(2 * zx_system_get_page_size()));
}

// Shrink snapshot that will allow a page in the parent to drop.
TEST(Snapshot, ResizeShrinkSnapshot) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);

  // Write to both pages of root.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;

  // Clone & COW both pages.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED | ZX_VMO_CHILD_RESIZABLE);
  ASSERT_NOT_NULL(clone);
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = 0xc0ffee;

  // snapshot.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED | ZX_VMO_CHILD_RESIZABLE);
  ASSERT_NOT_NULL(snapshot);

  // Pages in hidden node should be attributed to clone.
  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));

  // Shrink clone
  clone->Resize(1);

  // Shrink snapshot, which will drop a page in parent
  snapshot->Resize(1);
  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));
}

// Shrink snapshot that will allow an empty page in the parent to drop.
TEST(Snapshot, ResizeShrinkSnapshotWithEmptyParent) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0x1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0x1eaf;

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(clone->PollPopulatedBytes(0));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));

  // Shrink clone
  clone->Resize(1);

  // Shrink snapshot, which will drop an empty page in parent. This shouldn't cause a panic.
  snapshot->Resize(1);
  ASSERT_NOT_NULL(snapshot);

  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 2));
  ASSERT_TRUE(clone->PollPopulatedBytes(0));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));
}

// Tests that snapshoting a read only VMO should, by default, add write permissions
TEST(Snapshot, SnapshotReadOnlyVmo) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xc0ffee;

  // For checking rights
  zx_info_vmo_t info;

  // For trying writes
  auto kData = 0xdead1eaf;

  // Read only clone of VMO
  auto clone =
      vmo->Clone(0, zx_system_get_page_size(),
                 ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE | ZX_VMO_CHILD_NO_WRITE, ZX_VM_PERM_READ);
  ASSERT_NOT_NULL(clone);

  // Shouldn't have write perms or be able to write
  ASSERT_EQ(clone->vmo().get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr), ZX_OK);
  ASSERT_FALSE(info.handle_rights & ZX_RIGHT_WRITE);

  ASSERT_EQ(clone->vmo().write(&kData, 0, sizeof(kData)), ZX_ERR_ACCESS_DENIED);

  // Snapshot clone
  auto snap = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // By default, the snapshot should have gained write permissions
  ASSERT_EQ(snap->vmo().get_info(ZX_INFO_VMO, &info, sizeof(info), nullptr, nullptr), ZX_OK);
  ASSERT_TRUE(info.handle_rights & ZX_RIGHT_WRITE);

  ASSERT_EQ(snap->vmo().write(&kData, 0, sizeof(kData)), ZX_OK);
}

// Tests that dropping a vmo that results in a call to ReleaseCowParentPages on
// the second page works.
TEST(Snapshot, ReleaseCowParentPagesRight) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);

  auto full_clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_clone);
  auto half_clone = full_clone->Clone(0, zx_system_get_page_size(), ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(half_clone);

  // Drop full clone, which will result in a call to MergeContentWithChild
  // and then ReleaseCowParentPages on right page
  full_clone.reset();

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(half_clone->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(vmo->PollNumChildren(1));

  // Ensure both pages are maintianed in the root VMO.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);
}

// Tests that dropping a vmo that results in a call to ReleaseCowParentPages on
// the first page works.
TEST(Snapshot, ReleaseCowParentPagesLeft) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);

  auto full_clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_clone);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(full_clone->base_addr() + zx_system_get_page_size()),
            0xdead1eaf);
  auto half_clone = full_clone->Clone(zx_system_get_page_size(), zx_system_get_page_size(),
                                      ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(half_clone);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(half_clone->base_addr()), 0xdead1eaf);

  // Drop full clone, which will result in a call to MergeContentWithChild
  // and then ReleaseCowParentPages on left page
  full_clone.reset();

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(half_clone->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(vmo->PollNumChildren(1));

  // Ensure both pages are maintianed in the root VMO.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);
}

// Tests dropping a vmo that results in calls to ReleaseCowParentPages on either side
TEST(Snapshot, ReleaseCowParentPagesLeftAndRight) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  // 3 page vmo
  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(3, &vmo));
  ASSERT_NOT_NULL(vmo);

  // Write to all 3 pages of VMO.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 3));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + (2 * zx_system_get_page_size())) = 0xdead1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + (2 * zx_system_get_page_size())),
            0xdead1eaf);

  auto full_clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_clone);

  // Partial clone which only sees the center page
  auto partial_clone = full_clone->Clone(zx_system_get_page_size(), zx_system_get_page_size(),
                                         ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(partial_clone);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(partial_clone->base_addr()), 0xdead1eaf);

  // Drop full clone, which will result in a call to MergeContentWithChild
  // and ReleaseCowParentPages pages on left and right
  full_clone.reset();

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(partial_clone->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(vmo->PollNumChildren(1));

  // Ensure all pages are maintianed in the root VMO.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + (2 * zx_system_get_page_size())),
            0xdead1eaf);
}

TEST(Snapshot, ReleaseCowParentPagesRightInHiddenNode) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);

  // write to first page in root
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // clone & change value of first page
  auto full_clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_clone);
  *reinterpret_cast<uint64_t*>(full_clone->base_addr()) = 0xc0ffee;

  // snapshot with view of first page only
  auto half_clone = full_clone->Clone(0, 1, ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(half_clone);

  // Drop full clone, which will result in a call to MergeContentWithChild
  // and then ReleaseCowParentPages on second page
  full_clone.reset();

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(half_clone->base_addr()), 0xc0ffee);
}

// Tests zeroing a range at the end of a parent VMO, which results in a call to ReleaseParentPages
// in the hidden node.
TEST(Snapshot, ZeroRangeFromEndOfParent) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(3, &vmo));
  ASSERT_NOT_NULL(vmo);

  // Write to all pages of root
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 3));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + (2 * zx_system_get_page_size())) = 0xdead1eaf;

  ASSERT_TRUE(vmo->PollPopulatedBytes(zx_system_get_page_size() * 3));

  // Clone entire vmo
  auto full_clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(full_clone);

  // Write to second page, so 1 page is in root & 1 is in child.
  *reinterpret_cast<uint64_t*>(full_clone->base_addr() + zx_system_get_page_size()) = 0xc0ffee;

  // Snapshot the first page of the clone
  auto partial_clone = full_clone->Clone(0, 1, ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(partial_clone);

  // Zero last two pages in full_clone, which will cause ReleaseParentPages
  // to be called in the hidden node that owns 1 of the 2 unseen pages
  auto status = full_clone->vmo().op_range(ZX_VMO_OP_ZERO, 1 * zx_system_get_page_size(),
                                           2 * zx_system_get_page_size(), nullptr, 0);

  ASSERT_EQ(status, ZX_OK);

  // Ensure pages 2 & 3 from hidden node have been removed.
  // (If they were present in node, they would be attributed to one of the children).
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(full_clone->base_addr() + zx_system_get_page_size()), 0);
  ASSERT_TRUE(full_clone->PollPopulatedBytes(0));
  ASSERT_TRUE(partial_clone->PollPopulatedBytes(0));

  // Check that original pages can still be read from VMO.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr() + 2 * zx_system_get_page_size()),
            0xdead1eaf);
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent will not leak
// pages from the root to the zeroed range.
TEST(Snapshot, ZeroRangeLeftInSnapshotNoPagesInParent) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Zero range in snapshot.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Supply pages to root & check snapshot doesn't see them.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // SupplyPages should have provided non-zero pages.
  ASSERT_FALSE(check_buffer_data(vmo, 0, 1, kZeroBuffer.data(), false));

  // Clone should see the pages of the root VMO.
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 2, (const void*)vmo->base_addr(), false));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Tests that zeroing a range in a snapshot when there is a page in the parent at the time of the
// zero will not leak pages from the root to the zeroed range.
TEST(Snapshot, ZeroRangeLeftInSnapshotPageInParent) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Supply pages to root before performing OP_ZERO.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  // Zero range in snapshot.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Check that the page isn't leaked from the root VMO.
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // SupplyPages should have provided non-zero pages.
  ASSERT_FALSE(check_buffer_data(vmo, 0, 1, kZeroBuffer.data(), false));

  // Clone should see the pages of the root VMO.
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 2, (const void*)vmo->base_addr(), false));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent, and there is a
// chain of hidden parents, will not cause pages to leak from the root VMO.
TEST(Snapshot, ZeroRangeLeftInSnapshotNoPagesInParentChain) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  // Make a chain of three clones.
  auto clone1 = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone1);
  auto clone2 = clone1->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone2);
  auto clone3 = clone2->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone3);

  // Snapshot will have three hidden parents
  auto snapshot = clone3->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Zero range in snapshot & validate.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Supply pages to root & check snapshot doesn't see.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // SupplyPages should have provided non-zero pages.
  ASSERT_FALSE(check_buffer_data(vmo, 0, 1, kZeroBuffer.data(), false));

  // Clones should see the pages of the root VMO.
  ASSERT_TRUE(check_buffer_data(clone1.get(), 0, 2, (const void*)vmo->base_addr(), false));
  ASSERT_TRUE(check_buffer_data(clone2.get(), 0, 2, (const void*)vmo->base_addr(), false));
  ASSERT_TRUE(check_buffer_data(clone3.get(), 0, 2, (const void*)vmo->base_addr(), false));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent, and there is a
// chain of hidden parents in which one was a page, will not cause any pages to leak to the zeroed
// range.
TEST(Snapshot, ZeroRangeLeftInSnapshotPagesInParentChain) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  // SupplyPages should have provided non-zero pages.
  ASSERT_FALSE(check_buffer_data(vmo, 0, 1, kZeroBuffer.data(), false));

  // Make a chain of three clones & fork a page into clone2.
  auto clone1 = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone1);
  auto clone2 = clone1->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone2);
  *reinterpret_cast<uint64_t*>(clone2->base_addr()) = 0xdead1eaf;
  auto clone3 = clone2->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone3);

  // Snapshot will have three hidden parents, with a page in one of them.
  auto snapshot = clone3->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Zero range in snapshot & validate.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Write to clone3 & check snapshot doesn't see.
  *reinterpret_cast<uint64_t*>(clone3->base_addr()) = 0xc0ffee;
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Clone1 should see the pages of the root VMO.
  ASSERT_TRUE(check_buffer_data(clone1.get(), 0, 2, (const void*)vmo->base_addr(), false));
  // Clones 2 & 3 should see their writes.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone2->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone3->base_addr()), 0xc0ffee);
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent, and there is a
// page committed in the snapshot, will not cause pages to leak from the root VMO.
TEST(Snapshot, ZeroRangeLeftInSnapshotPageInSnapshot) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Write to snapshot
  *reinterpret_cast<uint64_t*>(snapshot->base_addr()) = 0xdead1eaf;

  // Verify snapshot write.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(snapshot->PollPopulatedBytes(zx_system_get_page_size()));

  // Zero range in snapshot & validate.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent, and there is a
// page committed in the clone, will not cause pages to leak from the root VMO.
TEST(Snapshot, ZeroRangeLeftInSnapshotPageInClone) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Write to clone
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;

  // Zero range in snapshot & validate.
  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Verify clone write.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Tests that zeroing a range in a snapshot when there are no pages in the parent, and there is a
// page committed in the hidden parent, will not cause pages to leak from the hidden parent or root
// VMO.
TEST(Snapshot, ZeroRangeLeftInSnapshotPageInHiddenNode) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  std::vector<uint64_t> kZeroBuffer(zx_system_get_page_size(), 0);

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  // Write to clone, which will commit a page in the hidden node.
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));

  // Make snapshot & zero the first page.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  auto status = snapshot->vmo().op_range(ZX_VMO_OP_ZERO, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "zero failed");
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kZeroBuffer.data(), false));

  // Snapshot should see the second page of the root VMO
  ASSERT_TRUE(check_buffer_data(
      snapshot.get(), 1, 1, (const void*)(vmo->base_addr() + zx_system_get_page_size()), false));
}

// Try to snapshot a slice, which should only be allowed on the root VMO.
TEST(Snapshot, AlowSlice) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // Snapshot of a slice of the root should work.
  auto rootslice = vmo->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(rootslice);

  auto slicealow = rootslice->Clone(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE);
  ASSERT_NOT_NULL(slicealow);
}

// Try to snapshot a slice, which should only be allowed on the root VMO.
TEST(Snapshot, SnapshotSlice) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // Snapshot of a slice of the root should work.
  auto rootslice = vmo->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(rootslice);

  auto slicesnapshot = rootslice->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(slicesnapshot);

  // Check reads/writes.
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xc0ffee;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(rootslice->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slicesnapshot->base_addr()), 0xc0ffee);

  // Check that the root-slice snapshot can be extended into a tree.
  auto slicesnapshot2 = slicesnapshot->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(slicesnapshot2);

  // Snapshot of non-root slice should not be allowed.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  auto slice = snapshot->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(rootslice);

  auto slicesnapshotbad = slice->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NULL(slicesnapshotbad);
}

// Tests creating a private pager copy of a slice of a snapshot, which should not be allowed.
TEST(Snapshot, SnapshotSliceAtLeastOnWrite) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // Clone & make slice of snapshot.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  auto snap = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snap);

  auto slice = snap->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(slice);

  // At least on write the slice, should not be allowed.
  auto alow = slice->Clone(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE);
  ASSERT_NULL(alow);
}

// Tests that a slice moves to the correct child after snapshot
TEST(Snapshot, SnapshotVmoWithSlice) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_NOT_NULL(vmo);
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdead1eaf;

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Modify & slice clone, snapshot it again
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;
  auto slice = clone->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(slice);
  auto snapshot2 = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot2);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot2->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr()), 0xc0ffee);

  // Modify clone & check if the slice sees.
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0x1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr()), 0x1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr() + zx_system_get_page_size()),
            0xdead1eaf);
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = 0x1eaf;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr() + zx_system_get_page_size()), 0x1eaf);

  // From the VMO point of view, the clone has 3 children, the two snapshot children & the slice.
  ASSERT_TRUE(clone->PollNumChildren(3));
  ASSERT_TRUE(snapshot->PollNumChildren(0));
  ASSERT_TRUE(snapshot2->PollNumChildren(0));

  // Check that snapshots reads are expected.
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr() + zx_system_get_page_size()),
            0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot2->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot2->base_addr() + zx_system_get_page_size()),
            0xdead1eaf);
}

// Tests creating a snapshot-modified clone of a root VMO that has a slice child, and cloning the
// slice itself.
TEST(Snapshot, CloneAfterSliceRoot) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  // Slice root.
  auto slice = vmo->Clone(ZX_VMO_CHILD_SLICE);
  ASSERT_NOT_NULL(slice);

  // Snapshot root vmo twice.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Snapshot the slice twice
  auto sliceclone = slice->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(sliceclone);

  auto slicesnapshot = sliceclone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(slicesnapshot);

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(sliceclone->base_addr()), 0xdead1eaf);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slicesnapshot->base_addr()), 0xdead1eaf);

  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xc0ffee;

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slice->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(sliceclone->base_addr()), 0xc0ffee);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(slicesnapshot->base_addr()), 0xc0ffee);
}

// Ensures that a snapshot-modified child can not be created on a VMO with pinned pages.
TEST(Snapshot, PinBeforeCreateFailure) {
  auto system_resource = maybe_standalone::GetSystemResource();

  if (!*system_resource) {
    printf("System resource not available, skipping\n");
    return;
  }

  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));

  zx::iommu iommu;
  zx::bti bti;
  zx_iommu_desc_dummy_t desc;
  auto final_bti_check = fit::defer([&bti]() {
    if (bti.is_valid()) {
      zx_info_bti_t info;
      ASSERT_OK(bti.get_info(ZX_INFO_BTI, &info, sizeof(info), nullptr, nullptr));
      EXPECT_EQ(0, info.pmo_count);
      EXPECT_EQ(0, info.quarantine_count);
    }
  });

  zx::result<zx::resource> result =
      maybe_standalone::GetSystemResourceWithBase(system_resource, ZX_RSRC_SYSTEM_IOMMU_BASE);
  ASSERT_OK(result.status_value());
  zx::resource iommu_resource = std::move(result.value());

  ASSERT_OK(zx::iommu::create(iommu_resource, ZX_IOMMU_TYPE_DUMMY, &desc, sizeof(desc), &iommu));
  EXPECT_OK(zx::bti::create(iommu, 0, 0xdead1eaf, &bti));

  auto name = "PinBeforeCreateFailure";
  if (bti.is_valid()) {
    EXPECT_OK(bti.set_property(ZX_PROP_NAME, name, strlen(name)));
  }

  zx::pmt pmt;
  zx_paddr_t addr;
  zx_status_t status =
      bti.pin(ZX_BTI_PERM_READ, vmo->vmo(), 0, zx_system_get_page_size(), &addr, 1, &pmt);
  ASSERT_OK(status, "pin failed");

  // Fail to clone if pages are pinned
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NULL(clone);
  pmt.unpin();

  // Clone successfully after pages are unpinned
  clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
}

// Tests calling op_range with the flag ZX_OP_COMMIT to ensure a panic is not triggered.
TEST(Snapshot, CommitRangeInSnapshot) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdead1eaf;

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);

  // Commit a page in the snapshot.
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));

  auto status =
      snapshot->vmo().op_range(ZX_VMO_OP_COMMIT, 0, zx_system_get_page_size(), nullptr, 0);
  ASSERT_OK(status, "commit failed");

  ASSERT_TRUE(snapshot->PollPopulatedBytes(zx_system_get_page_size()));
}

// Tests that reading from a clone or snapshot gets the correct data. Using VMO read/write
// functions.
TEST(Snapshot, Read) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));

  // Write to root.
  std::vector<uint64_t> kOriginalData(zx_system_get_page_size(), 0);
  vmo->GenerateBufferContents(kOriginalData.data(), 1, 0);
  kOriginalData[0] = 0xdead1eaf;
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_OK(vmo->vmo().write(kOriginalData.data(), 0, zx_system_get_page_size()));

  // Clone root & write to clone.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kOriginalData.data(), false));

  std::vector<uint64_t> kNewData(zx_system_get_page_size(), 0);
  clone->GenerateBufferContents(kNewData.data(), 1, 0);
  kNewData[0] = 0xc0ffee;
  ASSERT_OK(clone.get()->vmo().write(kNewData.data(), 0, zx_system_get_page_size()));

  ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kNewData.data(), false));

  // Snapshot clone & write to snapshot.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kNewData.data(), false));
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kNewData.data(), false));

  std::vector<uint64_t> kNewerData(zx_system_get_page_size(), 0);
  snapshot->GenerateBufferContents(kNewerData.data(), 1, 0);
  kNewerData[0] = 0x1eaf;
  ASSERT_OK(snapshot.get()->vmo().write(kNewerData.data(), 0, zx_system_get_page_size()));

  ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kNewData.data(), false));
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kNewerData.data(), false));
}

// Tests snapshotting a modified clone.
TEST(Snapshot, SnapshotModifiedClone) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  std::vector<uint64_t> kOriginalData(zx_system_get_page_size(), 0);
  std::vector<uint64_t> kNewData(zx_system_get_page_size(), 0);

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));

  // Write to original VMO.
  vmo->GenerateBufferContents(kOriginalData.data(), 1, 0);
  kOriginalData[0] = 0xdead1eaf;
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_OK(vmo->vmo().write(kOriginalData.data(), 0, zx_system_get_page_size()));

  // Clone & modify page in clone.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  clone->GenerateBufferContents(kNewData.data(), 1, 0);
  kNewData[0] = 0xc0ffee;
  ASSERT_OK(clone.get()->vmo().write(kNewData.data(), 0, zx_system_get_page_size()));

  // Snapshot clone.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
  ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kNewData.data(), false));
  ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kNewData.data(), false));
}

// Tests writing to a child of the root VMO after it's sibling is dropped.
TEST(Snapshot, WriteAfterDropSibling) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_TRUE(vmo->PollNumChildren(0));

  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_TRUE(vmo->PollNumChildren(1));

  // Write page into hidden node.
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xdead1eaf;

  // Snapshot.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);
  ASSERT_TRUE(vmo->PollNumChildren(1));

  // Drop snapshot. This shouldn't cause a panic.
  snapshot.reset();
  ASSERT_NULL(snapshot);

  // Write to clone.
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0xc0ffee;
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), 0xc0ffee);
}

// Clone & write down a chain.
TEST(Snapshot, CloneModifyChain) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  static constexpr uint32_t kOriginalData = 0xdead1eaf;
  static constexpr uint32_t kNewData = 0xc0ffee;
  static constexpr uint32_t kNewerData = 0x1eaf;

  // Two page VMO.
  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));

  // Write to page 1 of VMO.
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = kOriginalData;

  // Clone & modify page 2.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = kOriginalData;

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), kOriginalData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), kOriginalData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()),
            kOriginalData);

  // Snapshot & check pages.
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), kOriginalData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()),
            kOriginalData);

  // Modify pages in vmo & clone.
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = kNewData;
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = kNewData;

  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()), kNewData);

  // Snapshot should see modification in unmodified page 1 but snapshot original data in page 2
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr() + zx_system_get_page_size()),
            kOriginalData);

  // Modify snapshot
  *reinterpret_cast<uint64_t*>(snapshot->base_addr()) = kNewerData;
  *reinterpret_cast<uint64_t*>(snapshot->base_addr() + zx_system_get_page_size()) = kNewerData;

  // Modifying clone should not modify snapshot
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(vmo->base_addr()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()), kNewData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr()), kNewerData);
  EXPECT_EQ(*reinterpret_cast<uint64_t*>(snapshot->base_addr() + zx_system_get_page_size()),
            kNewerData);
}

// Basic memory accounting test that checks vmo memory attribution.
TEST(Snapshot, ObjMemAccounting) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  // Create a vmo, write to both pages, and check the committed stats.
  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(2, &vmo));
  ASSERT_TRUE(vmo->PollPopulatedBytes(0));

  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 2));
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  *reinterpret_cast<uint64_t*>(vmo->base_addr()) = 0xdeadbeef;
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0xdeadbeef;

  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));

  // Create a clone & snapshot, check the initialize committed stats.
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  // Write to first page of clone, which will result in a page in the hidden VmCowPages.
  *reinterpret_cast<uint64_t*>(clone->base_addr()) = 0x1eaf5;

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // The page in the hidden VmCowPages will be attributed to clone.
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(zx_system_get_page_size()));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(0));

  // Write to the second page of clone and check that that a page is forked into the clone.
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = 0xc0ffee;
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(2 * zx_system_get_page_size()));

  // Write to the snapshot and check that that forks a page
  *reinterpret_cast<uint64_t*>(snapshot->base_addr()) = 0xcafe;
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(zx_system_get_page_size()));

  // Write to the second page of VMO, which shouldn't affect accounting.
  *reinterpret_cast<uint64_t*>(vmo->base_addr() + zx_system_get_page_size()) = 0x1eaf;
  *reinterpret_cast<uint64_t*>(clone->base_addr() + zx_system_get_page_size()) = 0x1eaf;
  *reinterpret_cast<uint64_t*>(snapshot->base_addr() + zx_system_get_page_size()) = 0x1eaf;
  ASSERT_TRUE(vmo->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(clone->PollPopulatedBytes(2 * zx_system_get_page_size()));
  ASSERT_TRUE(snapshot->PollPopulatedBytes(2 * zx_system_get_page_size()));

  clone.reset();
  snapshot.reset();
}

// Tests that write into the (snapshot|clone|parent) doesn't affect the others.
void VmoWriteTestHelper(PageDepth depth) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  std::vector<uint64_t> kOriginalData(zx_system_get_page_size(), 0);
  std::vector<uint64_t> kRootData(zx_system_get_page_size(), 0);
  std::vector<uint64_t> kCloneData(zx_system_get_page_size(), 0);
  std::vector<uint64_t> kSnapshotData(zx_system_get_page_size(), 0);

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));

  // Write original data to VMO
  vmo->GenerateBufferContents(kOriginalData.data(), 1, 0);
  kOriginalData[0] = 0xdead1eaf;
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));
  ASSERT_OK(vmo->vmo().write(kOriginalData.data(), 0, zx_system_get_page_size()));

  // Snapshot-ish twice
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);

  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // Write to either root, clone or snapshot
  switch (depth) {
    case PageDepth::root:
      clone->GenerateBufferContents(kRootData.data(), 1, 0);
      kRootData[0] = 0xc0ffe;
      ASSERT_OK(vmo->vmo().write(kRootData.data(), 0, zx_system_get_page_size()));
      break;
    case PageDepth::clone:
      clone->GenerateBufferContents(kCloneData.data(), 1, 0);
      kCloneData[0] = 0xc0ffee;
      ASSERT_OK(clone.get()->vmo().write(kCloneData.data(), 0, zx_system_get_page_size()));
      break;
    case PageDepth::snapshot:
      snapshot->GenerateBufferContents(kSnapshotData.data(), 1, 0);
      kSnapshotData[0] = 0xc0ffeee;
      ASSERT_OK(snapshot.get()->vmo().write(kSnapshotData.data(), 0, zx_system_get_page_size()));
      break;
  }

  // Check VMOs have the correct data
  switch (depth) {
    case PageDepth::root:
      ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kRootData.data(), false));
      ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kRootData.data(), false));
      ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kRootData.data(), false));
      break;
    case PageDepth::clone:
      ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kCloneData.data(), false));
      ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kOriginalData.data(), false));
      break;
    case PageDepth::snapshot:
      ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kSnapshotData.data(), false));
      break;
  }

  clone.reset();
  snapshot.reset();
}

TEST(Snapshot, RootVmoWrite) { ASSERT_NO_FATAL_FAILURE(VmoWriteTestHelper(PageDepth::root)); }
TEST(Snapshot, CloneVmoWrite) { ASSERT_NO_FATAL_FAILURE(VmoWriteTestHelper(PageDepth::clone)); }
TEST(Snapshot, SnapshotVmoWrite) {
  ASSERT_NO_FATAL_FAILURE(VmoWriteTestHelper(PageDepth::snapshot));
}

// Tests that closing the (parent|clone|snapshot) doesn't affect the other.
void CloseTestHelper(PageDepth close_depth) {
  UserPager pager;
  ASSERT_TRUE(pager.Init());

  std::vector<uint64_t> kOriginalData(zx_system_get_page_size(), 0);

  Vmo* vmo;
  ASSERT_TRUE(pager.CreateVmo(1, &vmo));
  ASSERT_TRUE(pager.SupplyPages(vmo, 0, 1));

  vmo->GenerateBufferContents(kOriginalData.data(), 1, 0);
  kOriginalData[0] = 0xdead1eaf;
  ASSERT_OK(vmo->vmo().write(kOriginalData.data(), 0, zx_system_get_page_size()));
  auto clone = vmo->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(clone);
  auto snapshot = clone->Clone(ZX_VMO_CHILD_SNAPSHOT_MODIFIED);
  ASSERT_NOT_NULL(snapshot);

  // close either root, clone or snapshot
  switch (close_depth) {
    case PageDepth::root:
      pager.ReleaseVmo(vmo);
      break;
    case PageDepth::clone:
      clone.reset();
      break;
    case PageDepth::snapshot:
      snapshot.reset();
      break;
  }

  // Check data
  switch (close_depth) {
    case PageDepth::root:
      ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kOriginalData.data(), false));
      break;
    case PageDepth::clone:
      ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(snapshot.get(), 0, 1, kOriginalData.data(), false));
      break;
    case PageDepth::snapshot:
      ASSERT_TRUE(check_buffer_data(vmo, 0, 1, kOriginalData.data(), false));
      ASSERT_TRUE(check_buffer_data(clone.get(), 0, 1, kOriginalData.data(), false));
      break;
  }

  clone.reset();
  snapshot.reset();
}

TEST(Snapshot, CloseClone) { ASSERT_NO_FATAL_FAILURE(CloseTestHelper(PageDepth::clone)); }

TEST(Snapshot, CloseSnapshot) { ASSERT_NO_FATAL_FAILURE(CloseTestHelper(PageDepth::snapshot)); }

TEST(Snapshot, CloseRoot) { ASSERT_NO_FATAL_FAILURE(CloseTestHelper(PageDepth::root)); }

}  // namespace pager_tests
