// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fbl/auto_lock.h>
#include <fs/paged_vfs.h>
#include <fs/paged_vnode.h>

namespace fs {

PagedVfs::PagedVfs(int num_pager_threads) : pager_pool_(*this, num_pager_threads) {}

PagedVfs::~PagedVfs() {
  // TODO(fxbug.dev/51111) need to detach from PagedVnodes that have back-references to this class.
  // The vnodes are reference counted and can outlive this class.
}

zx::status<> PagedVfs::Init() {
  if (zx_status_t status = zx::pager::create(0, &pager_); status != ZX_OK)
    return zx::error(status);

  return pager_pool_.Init();
}

zx::status<> PagedVfs::SupplyPages(PagedVnode& node, uint64_t offset, uint64_t length,
                                   zx::vmo& aux_vmo, uint64_t aux_offset) {
  return zx::make_status(pager_.supply_pages(node.vmo(), offset, length, aux_vmo, aux_offset));
}

zx::status<> PagedVfs::ReportPagerError(PagedVnode& node, uint32_t op, uint64_t offset,
                                        uint64_t length, uint64_t data) {
  return zx::make_status(pager_.op_range(op, node.vmo(), offset, length, data));
}

uint64_t PagedVfs::RegisterNode(PagedVnode* node) {
  fbl::AutoLock lock(&vfs_lock_);

  uint64_t id = next_node_id_;
  ++next_node_id_;

  paged_nodes_[id] = node;
  return id;
}

void PagedVfs::UnregisterNode(uint64_t id) {
  fbl::AutoLock lock(&vfs_lock_);

  auto found = paged_nodes_.find(id);
  ZX_ASSERT(found != paged_nodes_.end());
  paged_nodes_.erase(found);
}

zx::status<zx::vmo> PagedVfs::CreatePagedVmo(uint64_t node_id, uint64_t size) {
  zx::vmo vmo;
  if (auto status = pager_.create_vmo(0, pager_pool_.port(), node_id, size, &vmo); status != ZX_OK)
    return zx::error(status);
  return zx::ok(std::move(vmo));
}

void PagedVfs::PagerVmoRead(uint64_t node_id, uint64_t offset, uint64_t length) {
  // Hold a reference to the object to ensure it doesn't go out of scope during processing.
  fbl::RefPtr<PagedVnode> node;

  {
    fbl::AutoLock lock(&vfs_lock_);

    auto found = paged_nodes_.find(node_id);
    if (found == paged_nodes_.end())
      return;  // Possible race with completion message on another thread, ignore.

    node = fbl::RefPtr<PagedVnode>(found->second);
  }

  // Handle the request outside the lock while holding a reference to the node.
  node->VmoRead(offset, length);
}

void PagedVfs::PagerVmoComplete(uint64_t node_id) {
  // Hold a reference to the object to ensure it doesn't go out of scope during processing.
  fbl::RefPtr<PagedVnode> node;

  {
    fbl::AutoLock lock(&vfs_lock_);

    auto found = paged_nodes_.find(node_id);
    if (found == paged_nodes_.end()) {
      ZX_DEBUG_ASSERT(false);  // Should only get one complete message.
      return;
    }

    node = fbl::RefPtr<PagedVnode>(found->second);
  }

  // TODO(fxbug.dev/51111) notify the node (outside of the lock to prevent reentrant locking) that it's
  // complete so it can remove itself.

#ifndef NDEBUG
  // The node should always have removed itself.
  {
    fbl::AutoLock lock(&vfs_lock_);
    ZX_DEBUG_ASSERT(paged_nodes_.find(node_id) == paged_nodes_.end());
  }
#endif
}

}  // namespace fs
