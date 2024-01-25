// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/zx/bti.h>
#include <lib/zx/iommu.h>
#include <lib/zx/pager.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/iommu.h>

#include <zxtest/zxtest.h>

#include "helpers.h"

namespace {

void CreateVmoWithCharFill(zx::vmo* vmo, char content, size_t size) {
  char buf[size];
  memset(buf, content, size);
  ASSERT_OK(zx::vmo::create(size, 0, vmo));
  ASSERT_OK(vmo->write(buf, 0, size));
}

TEST(VmoTransferDataTestCase, DestroyedParentWithNonZeroOffset) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kChildSize = kPageSize * 3;
  const uint64_t kChildOffset = kPageSize;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination of the transfer.
  // In this test case, the source VMO is offset from the parent VMO by kChildOffset, and the
  // parent is subsequently deleted. This will allow us to test the case where we have to
  // `TakePages` from a source VMO with non-zero list_skew_.
  zx::vmo parent_vmo;
  CreateVmoWithCharFill(&parent_vmo, 's', kSize);
  zx::vmo src_vmo;
  ASSERT_OK(parent_vmo.create_child(ZX_VMO_CHILD_SNAPSHOT, kChildOffset, kChildSize, &src_vmo));
  parent_vmo.reset();
  zx::vmo dst_vmo;
  CreateVmoWithCharFill(&dst_vmo, 'd', kSize);

  // Verify that the transfer from source still works.
  ASSERT_OK(dst_vmo.transfer_data(0, 0, kChildSize, &src_vmo, 0));

  // Verify that the src VMO has been zeroed out.
  ASSERT_OK(src_vmo.read(got, 0, kChildSize));
  memset(expected, 0, kChildSize);
  EXPECT_BYTES_EQ(got, expected, kChildSize);

  // Verify that the dst VMO correctly received the transferred data.
  ASSERT_OK(dst_vmo.read(got, 0, kSize));
  memset(expected, 's', kChildSize);
  memset(&expected[kChildSize], 'd', kSize - kChildSize);
  EXPECT_BYTES_EQ(got, expected, kSize);
}

TEST(VmoTransferDataTestCase, SnapshotChildSrc) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kChildSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];
  // TODO(https://fxbug.dev/42074633): Add ZX_VMO_CHILD_SNAPSHOT_MODIFIED to this list.
  uint32_t child_types[] = {ZX_VMO_CHILD_SNAPSHOT, ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE};

  for (auto child_type : child_types) {
    // Create VMOs to act as the source and destination VMOs for a transfer.
    zx::vmo parent_vmo;
    if (child_type == ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE) {
      // If the child type is SNAPSHOT_AT_LEAST_ON_WRITE, we need the parent VMO to be pager
      // backed, as children of this type are upgraded to pure snapshots for anonymous VMOs.
      zx::pager pager;
      zx::port port;
      ASSERT_OK(zx::pager::create(0, &pager));
      ASSERT_OK(zx::port::create(0, &port));
      ASSERT_OK(zx_pager_create_vmo(pager.get(), 0, port.get(), 0, kSize,
                                    parent_vmo.reset_and_get_address()));

      // Presupply pages to the pager backed VMO so that we don't need to wait for a request.
      zx::vmo aux_vmo;
      CreateVmoWithCharFill(&aux_vmo, 's', kSize);
      ASSERT_OK(pager.supply_pages(parent_vmo, 0, kSize, aux_vmo, 0));
    } else {
      // If the child type is a pure snapshot, create an anonymous VMO.
      CreateVmoWithCharFill(&parent_vmo, 's', kSize);
    }
    zx::vmo src_vmo;
    ASSERT_OK(parent_vmo.create_child(child_type, 0, kChildSize, &src_vmo));
    zx::vmo dst_vmo;
    CreateVmoWithCharFill(&dst_vmo, 'd', kSize);

    // Verify that transferring data from a snapshot child works.
    ASSERT_OK(dst_vmo.transfer_data(0, 0, kChildSize, &src_vmo, 0));

    // Verify that the src VMO has been zeroed out.
    ASSERT_OK(src_vmo.read(got, 0, kChildSize));
    memset(expected, 0, kChildSize);
    EXPECT_BYTES_EQ(got, expected, kChildSize);

    // Verify that the parent of the src VMO is unaffected.
    ASSERT_OK(parent_vmo.read(got, 0, kSize));
    memset(expected, 's', kSize);
    EXPECT_BYTES_EQ(got, expected, kSize);

    // Verify that the dst VMO correctly received the transferred data.
    ASSERT_OK(dst_vmo.read(got, 0, kSize));
    memset(expected, 's', kChildSize);
    memset(&expected[kChildSize], 'd', kSize - kChildSize);
    EXPECT_BYTES_EQ(got, expected, kSize);
  }
}

TEST(VmoTransferDataTestCase, SnapshotChildDst) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kChildSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];
  // TODO(https://fxbug.dev/42074633): Add ZX_VMO_CHILD_SNAPSHOT_MODIFIED to this list.
  uint32_t child_types[] = {ZX_VMO_CHILD_SNAPSHOT, ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE};

  for (auto child_type : child_types) {
    // Create VMOs to act as the source and destination VMOs for a transfer.
    zx::vmo src_vmo;
    CreateVmoWithCharFill(&src_vmo, 's', kSize);
    zx::vmo parent_vmo;
    if (child_type == ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE) {
      // If the child type is SNAPSHOT_AT_LEAST_ON_WRITE, we need the parent VMO to be pager
      // backed, as children of this type are upgraded to pure snapshots for anonymous VMOs.
      zx::pager pager;
      zx::port port;
      ASSERT_OK(zx::pager::create(0, &pager));
      ASSERT_OK(zx::port::create(0, &port));
      ASSERT_OK(zx_pager_create_vmo(pager.get(), 0, port.get(), 0, kSize,
                                    parent_vmo.reset_and_get_address()));

      // Presupply pages to the pager backed VMO so that we don't need to wait for a request.
      zx::vmo aux_vmo;
      CreateVmoWithCharFill(&aux_vmo, 'd', kSize);
      ASSERT_OK(pager.supply_pages(parent_vmo, 0, kSize, aux_vmo, 0));
    } else {
      // If the child type is a pure snapshot, create an anonymous VMO.
      CreateVmoWithCharFill(&parent_vmo, 'd', kSize);
    }
    zx::vmo dst_vmo;
    ASSERT_OK(parent_vmo.create_child(child_type, 0, kChildSize, &dst_vmo));

    // Verify that transferring data to a snapshot child works.
    ASSERT_OK(dst_vmo.transfer_data(0, 0, kChildSize, &src_vmo, 0));

    // Verify that the destination has the transferred contents.
    ASSERT_OK(dst_vmo.read(got, 0, kChildSize));
    memset(expected, 's', kChildSize);
    EXPECT_BYTES_EQ(got, expected, kChildSize);

    // Verify that the src vmo was zeroed out in the transfer range.
    ASSERT_OK(src_vmo.read(got, 0, kSize));
    memset(expected, 0, kChildSize);
    memset(&expected[kChildSize], 's', kSize - kChildSize);
    EXPECT_BYTES_EQ(got, expected, kSize);

    // Verify that the parent of the destination remained unchanged.
    memset(expected, 'd', kSize);
    ASSERT_OK(parent_vmo.read(got, 0, kSize));
    EXPECT_BYTES_EQ(got, expected, kSize);
  }
}

TEST(VmoTransferDataTestCase, ChildSliceDst) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kChildSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo src_vmo;
  CreateVmoWithCharFill(&src_vmo, 's', kSize);
  zx::vmo parent_vmo;
  CreateVmoWithCharFill(&parent_vmo, 'd', kSize);
  zx::vmo dst_vmo;
  ASSERT_OK(parent_vmo.create_child(ZX_VMO_CHILD_SLICE, 0, kChildSize, &dst_vmo));

  // Verify that transferring data to a VMO that is a child slice works.
  ASSERT_OK(dst_vmo.transfer_data(0, 0, kChildSize, &src_vmo, 0));

  ASSERT_OK(src_vmo.read(got, 0, kSize));
  memset(expected, 0, kChildSize);
  memset(&expected[kChildSize], 's', kSize - kChildSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(parent_vmo.read(got, 0, kSize));
  memset(expected, 's', kChildSize);
  memset(&expected[kChildSize], 'd', kSize - kChildSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(dst_vmo.read(got, 0, kChildSize));
  memset(expected, 's', kChildSize);
  EXPECT_BYTES_EQ(got, expected, kChildSize);
}

TEST(VmoTransferDataTestCase, ChildSliceSrc) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kChildSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo parent_vmo;
  CreateVmoWithCharFill(&parent_vmo, 's', kSize);
  zx::vmo src_vmo;
  ASSERT_OK(parent_vmo.create_child(ZX_VMO_CHILD_SLICE, 0, kChildSize, &src_vmo));
  zx::vmo dst_vmo;
  CreateVmoWithCharFill(&dst_vmo, 'd', kSize);

  // Verify that transferring data from a VMO that is a child slice works.
  ASSERT_OK(dst_vmo.transfer_data(0, 0, kChildSize, &src_vmo, 0));

  ASSERT_OK(src_vmo.read(got, 0, kChildSize));
  memset(expected, 0, kChildSize);
  EXPECT_BYTES_EQ(got, expected, kChildSize);

  ASSERT_OK(parent_vmo.read(got, 0, kSize));
  memset(expected, 0, kChildSize);
  memset(&expected[kChildSize], 's', kSize - kChildSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(dst_vmo.read(got, 0, kSize));
  memset(expected, 's', kChildSize);
  memset(&expected[kChildSize], 'd', kSize - kChildSize);
  EXPECT_BYTES_EQ(got, expected, kSize);
}

TEST(VmoTransferDataTestCase, ReferenceChildSrc) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kTransferSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo parent_vmo;
  CreateVmoWithCharFill(&parent_vmo, 's', kSize);
  zx::vmo src_vmo;
  ASSERT_OK(parent_vmo.create_child(ZX_VMO_CHILD_REFERENCE, 0, 0, &src_vmo));
  zx::vmo dst_vmo;
  CreateVmoWithCharFill(&dst_vmo, 'd', kSize);

  // Verify that transferring data from a VMO that is a reference child works.
  ASSERT_OK(dst_vmo.transfer_data(0, 0, kTransferSize, &src_vmo, 0));

  ASSERT_OK(src_vmo.read(got, 0, kTransferSize));
  memset(expected, 0, kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kTransferSize);

  ASSERT_OK(parent_vmo.read(got, 0, kSize));
  memset(expected, 0, kTransferSize);
  memset(&expected[kTransferSize], 's', kSize - kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(dst_vmo.read(got, 0, kSize));
  memset(expected, 's', kTransferSize);
  memset(&expected[kTransferSize], 'd', kSize - kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);
}

TEST(VmoTransferDataTestCase, ReferenceChildDst) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kTransferSize = kPageSize * 3;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo src_vmo;
  CreateVmoWithCharFill(&src_vmo, 's', kSize);
  zx::vmo parent_vmo;
  CreateVmoWithCharFill(&parent_vmo, 'd', kSize);
  zx::vmo dst_vmo;
  ASSERT_OK(parent_vmo.create_child(ZX_VMO_CHILD_REFERENCE, 0, 0, &dst_vmo));

  // Verify that transferring data to a VMO that is a reference child works.
  ASSERT_OK(dst_vmo.transfer_data(0, 0, kTransferSize, &src_vmo, 0));

  ASSERT_OK(src_vmo.read(got, 0, kSize));
  memset(expected, 0, kTransferSize);
  memset(&expected[kTransferSize], 's', kSize - kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(parent_vmo.read(got, 0, kSize));
  memset(expected, 's', kTransferSize);
  memset(&expected[kTransferSize], 'd', kSize - kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  ASSERT_OK(dst_vmo.read(got, 0, kTransferSize));
  memset(expected, 's', kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kTransferSize);
}

TEST(VmoTransferDataTestCase, SameSrcAndDst) {
  // Verify that passing the same VMO as the source and destination succeeds.
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kTransferSize = kPageSize * 3;
  const uint64_t kTransferOffset = kPageSize * 2;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo vmo;
  CreateVmoWithCharFill(&vmo, 's', kSize);

  ASSERT_OK(vmo.transfer_data(0, kTransferOffset, kTransferSize, &vmo, 0));

  ASSERT_OK(vmo.read(got, 0, kSize));
  memset(expected, 0, kTransferOffset);
  memset(&expected[kTransferOffset], 's', kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);
}

TEST(VmoTransferDataTestCase, Basic) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kSize = kPageSize * 5;
  const uint64_t kTransferSize = kPageSize * 3;
  const uint64_t kTransferOffset = kPageSize * 2;
  char got[kSize];
  char expected[kSize];

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo src_vmo;
  CreateVmoWithCharFill(&src_vmo, 's', kSize);
  zx::vmo dst_vmo;
  CreateVmoWithCharFill(&dst_vmo, 'd', kSize);

  // Verify the happy case works by transfer the first three pages of the
  // original VMO into the last three pages of the destination VMO.
  ASSERT_OK(dst_vmo.transfer_data(0, kTransferOffset, kTransferSize, &src_vmo, 0));

  // Validate that the destination VMO retains its original contents for the first two pages and
  // contains the transferred data for the last three pages.
  ASSERT_OK(dst_vmo.read(got, 0, kSize));
  memset(expected, 'd', kTransferOffset);
  memset(&expected[kTransferOffset], 's', kTransferSize);
  EXPECT_BYTES_EQ(got, expected, kSize);

  // Validate that the transferred pages were zeroed out in the source VMO.
  ASSERT_OK(src_vmo.read(got, 0, kSize));
  memset(expected, 0, kTransferSize);
  memset(&expected[kTransferSize], 's', kTransferOffset);
  EXPECT_BYTES_EQ(got, expected, kSize);
}

TEST(VmoTransferDataTestCase, InvalidInputs) {
  const uint64_t kPageSize = zx_system_get_page_size();
  const uint64_t kPageCount = 5;
  const uint64_t kSize = kPageSize * kPageCount;

  // Create VMOs to act as the source and destination VMOs for a transfer.
  zx::vmo src_vmo;
  ASSERT_OK(zx::vmo::create(kSize, 0, &src_vmo));
  zx::vmo dst_vmo;
  ASSERT_OK(zx::vmo::create(kSize, 0, &dst_vmo));

  // Verify that a bad handle as the source or destination VMO fails.
  zx::vmo invalid_vmo;
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, dst_vmo.transfer_data(0, kPageSize, kPageSize, &invalid_vmo, 0));
  EXPECT_EQ(ZX_ERR_BAD_HANDLE, invalid_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));

  // Verify that passing non-zero options fails.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, dst_vmo.transfer_data(1, kPageSize - 1, kPageSize, &src_vmo, 0));

  // Verify that non-page aligned offsets/lengths fail.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, dst_vmo.transfer_data(0, kPageSize - 1, kPageSize, &src_vmo, 0));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, dst_vmo.transfer_data(0, kPageSize, kPageSize - 1, &src_vmo, 0));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS,
            dst_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, kPageSize - 1));

  // Verify that not having the appropriate rights on the source and destination fails.
  // 1. Src with no rights fails.
  // 2. Src with only read right fails.
  // 3. Src with only write right fails.
  // 4. Dst with no rights fails.
  zx::vmo src_no_rights;
  ASSERT_OK(
      zx_handle_duplicate(src_vmo.get(), ZX_RIGHT_NONE, src_no_rights.reset_and_get_address()));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED,
            dst_vmo.transfer_data(0, kPageSize, kPageSize, &src_no_rights, 0));
  zx::vmo src_no_write;
  ASSERT_OK(
      zx_handle_duplicate(src_vmo.get(), ZX_RIGHT_READ, src_no_write.reset_and_get_address()));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, dst_vmo.transfer_data(0, kPageSize, kPageSize, &src_no_write, 0));
  zx::vmo src_no_read;
  ASSERT_OK(
      zx_handle_duplicate(src_vmo.get(), ZX_RIGHT_WRITE, src_no_read.reset_and_get_address()));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED, dst_vmo.transfer_data(0, kPageSize, kPageSize, &src_no_read, 0));
  zx::vmo dst_no_rights;
  ASSERT_OK(
      zx_handle_duplicate(dst_vmo.get(), ZX_RIGHT_NONE, dst_no_rights.reset_and_get_address()));
  EXPECT_EQ(ZX_ERR_ACCESS_DENIED,
            dst_no_rights.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));

  // Verify that a pager-backed source or destination VMO fails.
  zx::pager pager;
  ASSERT_OK(zx::pager::create(0, &pager));
  zx::port port;
  ASSERT_OK(zx::port::create(0, &port));
  zx::vmo pager_backed_vmo;
  ASSERT_OK(pager.create_vmo(0, port, 1, kSize, &pager_backed_vmo));
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED,
            dst_vmo.transfer_data(0, kPageSize, kPageSize, &pager_backed_vmo, 0));
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED,
            pager_backed_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));

  // Verify that providing an invalid range to the source VMO fails.
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE, dst_vmo.transfer_data(0, kPageSize, 6 * kPageSize, &src_vmo, 0));
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE,
            dst_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 6 * kPageSize));

  // Verify that providing an invalid range to the destination VMO fails.
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE,
            dst_vmo.transfer_data(0, kPageSize * 6, kPageSize, &src_vmo, 3 * kPageSize));

  // Verify that providing a physical, contiguous, or pinned VMO as either the destination or the
  // source fails.
  zx::unowned_resource root_resource = maybe_standalone::GetRootResource();
  if (root_resource->is_valid()) {
    zx::result res = vmo_test::GetTestPhysVmo(kSize);
    ASSERT_OK(res.status_value());
    EXPECT_EQ(ZX_ERR_NOT_SUPPORTED,
              res.value().vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));
    EXPECT_EQ(ZX_ERR_NOT_SUPPORTED,
              dst_vmo.transfer_data(0, kPageSize, kPageSize, &res.value().vmo, 0));

    zx::iommu iommu;
    zx::bti bti;
    zx_iommu_desc_dummy_t desc;
    auto final_bti_check = vmo_test::CreateDeferredBtiCheck(bti);

    ASSERT_OK(zx::iommu::create(*root_resource, ZX_IOMMU_TYPE_DUMMY, &desc, sizeof(desc), &iommu));
    bti = vmo_test::CreateNamedBti(iommu, 0, 0xdeadbeef, "VmoTestCase::TransferData");

    zx::vmo contig_vmo;
    ASSERT_OK(zx::vmo::create_contiguous(bti, kSize, 0, &contig_vmo));
    EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, contig_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));
    EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, dst_vmo.transfer_data(0, kPageSize, kPageSize, &contig_vmo, 0));

    zx::vmo pinned_vmo;
    ASSERT_OK(zx::vmo::create(kSize, 0, &pinned_vmo));
    zx_paddr_t paddrs[kPageCount];
    zx::pmt pmt;
    ASSERT_OK(bti.pin(ZX_BTI_PERM_READ, pinned_vmo, 0, kSize, paddrs, kPageCount, &pmt));
    EXPECT_EQ(ZX_ERR_BAD_STATE, pinned_vmo.transfer_data(0, kPageSize, kPageSize, &src_vmo, 0));
    EXPECT_EQ(ZX_ERR_BAD_STATE, dst_vmo.transfer_data(0, kPageSize, kPageSize, &pinned_vmo, 0));
    ASSERT_OK(pmt.unpin());
  }
}

}  // namespace
