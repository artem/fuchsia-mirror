// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZX_VMAR_H_
#define LIB_ZX_VMAR_H_

#include <lib/zx/iob.h>
#include <lib/zx/object.h>
#include <lib/zx/vmo.h>
#include <zircon/availability.h>
#include <zircon/process.h>

namespace zx {

// A wrapper for handles to VMARs.  Note that vmar::~vmar() does not execute
// vmar::destroy(), it just closes the handle.
class vmar final : public object<vmar> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_VMAR;

  constexpr vmar() = default;

  explicit vmar(zx_handle_t value) : object(value) {}

  explicit vmar(handle&& h) : object(h.release()) {}

  vmar(vmar&& other) : vmar(other.release()) {}

  vmar& operator=(vmar&& other) {
    reset(other.release());
    return *this;
  }

  zx_status_t map(zx_vm_option_t options, size_t vmar_offset, const vmo& vmo_handle,
                  uint64_t vmo_offset, size_t len, zx_vaddr_t* ptr) const ZX_AVAILABLE_SINCE(7) {
    return zx_vmar_map(get(), options, vmar_offset, vmo_handle.get(), vmo_offset, len, ptr);
  }

  zx_status_t map_iob(zx_vm_option_t options, size_t vmar_offset, const iob& iob_handle,
                      uint32_t region_index, uint64_t region_offset, size_t region_len,
                      zx_vaddr_t* ptr) const ZX_AVAILABLE_SINCE(14) {
    return zx_vmar_map_iob(get(), options, vmar_offset, iob_handle.get(), region_index,
                           region_offset, region_len, ptr);
  }

  zx_status_t unmap(uintptr_t address, size_t len) const ZX_AVAILABLE_SINCE(7) {
    return zx_vmar_unmap(get(), address, len);
  }

  zx_status_t protect(zx_vm_option_t prot, uintptr_t address, size_t len) const
      ZX_AVAILABLE_SINCE(7) {
    return zx_vmar_protect(get(), prot, address, len);
  }

  zx_status_t op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                       size_t buffer_size) const ZX_AVAILABLE_SINCE(7) {
    return zx_vmar_op_range(get(), op, offset, size, buffer, buffer_size);
  }

  zx_status_t destroy() const ZX_AVAILABLE_SINCE(7) { return zx_vmar_destroy(get()); }

  zx_status_t allocate(uint32_t options, size_t offset, size_t size, vmar* child,
                       uintptr_t* child_addr) const ZX_AVAILABLE_SINCE(7);

  static inline unowned<vmar> root_self() ZX_AVAILABLE_SINCE(7) {
    return unowned<vmar>(zx_vmar_root_self());
  }
} ZX_AVAILABLE_SINCE(7);

using unowned_vmar = unowned<vmar> ZX_AVAILABLE_SINCE(7);

}  // namespace zx

#endif  // LIB_ZX_VMAR_H_
