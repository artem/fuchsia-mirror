// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/io_buffer.h"

#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <memory>

// Copied from io_buffer lib since we want to remove the dependency of all ddk libs.

// Returns true if a buffer with these parameters was allocated using
// zx_vmo_create_contiguous.  This is primarily important so we know whether we
// need to call COMMIT on it to get the pages to exist.
static bool is_allocated_contiguous(size_t size, uint32_t flags) {
  return (flags & IO_BUFFER_CONTIG) && size > zx_system_get_page_size();
}

static zx_status_t pin_contig_buffer(zx_handle_t bti, zx_handle_t vmo, size_t size,
                                     zx_paddr_t* phys, zx_handle_t* pmt) {
  uint32_t options = ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE;
  if (size > zx_system_get_page_size()) {
    options |= ZX_BTI_CONTIGUOUS;
  }
  return zx_bti_pin(bti, options, vmo, 0, DDK_ROUNDUP(size, zx_system_get_page_size()), phys, 1,
                    pmt);
}

static zx_status_t io_buffer_init_common(io_buffer_t* buffer, zx_handle_t bti_handle,
                                         zx_handle_t vmo_handle, size_t size, zx_off_t offset,
                                         uint32_t flags) {
  zx_vaddr_t virt;

  zx_vm_option_t map_options = ZX_VM_PERM_READ;
  if (flags & IO_BUFFER_RW) {
    map_options = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;
  }

  zx_status_t status = zx_vmar_map(zx_vmar_root_self(), map_options, 0, vmo_handle, 0, size, &virt);
  if (status != ZX_OK) {
    // zxlogf(ERROR, "io_buffer: zx_vmar_map failed %d size: %zu", status, size);
    zx_handle_close(vmo_handle);
    return status;
  }

  // For contiguous buffers, pre-lookup the physical mapping so
  // io_buffer_phys() works.  For non-contiguous buffers, io_buffer_physmap()
  // will need to be called.
  zx_paddr_t phys = IO_BUFFER_INVALID_PHYS;
  zx_handle_t pmt_handle = ZX_HANDLE_INVALID;
  if (flags & IO_BUFFER_CONTIG) {
    ZX_DEBUG_ASSERT(offset == 0);
    status = pin_contig_buffer(bti_handle, vmo_handle, size, &phys, &pmt_handle);
    if (status != ZX_OK) {
      // zxlogf(ERROR, "io_buffer: init pin failed %d size: %zu", status, size);
      zx_vmar_unmap(zx_vmar_root_self(), virt, size);
      zx_handle_close(vmo_handle);
      return status;
    }
  }

  buffer->bti_handle = bti_handle;
  buffer->vmo_handle = vmo_handle;
  buffer->pmt_handle = pmt_handle;
  buffer->size = size;
  buffer->offset = offset;
  buffer->virt = (void*)virt;
  buffer->phys = phys;

  return ZX_OK;
}

zx_status_t io_buffer_init_aligned(io_buffer_t* buffer, zx_handle_t bti, size_t size,
                                   uint32_t alignment_log2, uint32_t flags) {
  memset(buffer, 0, sizeof(*buffer));

  if (size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (flags & ~IO_BUFFER_FLAGS_MASK) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx_handle_t vmo_handle;
  zx_status_t status;

  if (is_allocated_contiguous(size, flags)) {
    status = zx_vmo_create_contiguous(bti, size, alignment_log2, &vmo_handle);
  } else {
    // zx_vmo_create doesn't support passing an alignment.
    if (alignment_log2 != 0)
      return ZX_ERR_INVALID_ARGS;
    status = zx_vmo_create(size, 0, &vmo_handle);
  }
  if (status != ZX_OK) {
    // zxlogf(ERROR, "io_buffer: zx_vmo_create failed %d", status);
    return status;
  }

  if (flags & IO_BUFFER_UNCACHED) {
    status = zx_vmo_set_cache_policy(vmo_handle, ZX_CACHE_POLICY_UNCACHED);
    if (status != ZX_OK) {
      // zxlogf(ERROR, "io_buffer: zx_vmo_set_cache_policy failed %d", status);
      zx_handle_close(vmo_handle);
      return status;
    }
  }

  return io_buffer_init_common(buffer, bti, vmo_handle, size, 0, flags);
}

zx_status_t io_buffer_init(io_buffer_t* buffer, zx_handle_t bti, size_t size, uint32_t flags) {
  // A zero alignment gets interpreted as PAGE_SIZE_SHIFT.
  return io_buffer_init_aligned(buffer, bti, size, 0, flags);
}

// Returns the buffer size available after the given offset, relative to the
// io_buffer vmo offset.
size_t io_buffer_size(const io_buffer_t* buffer, size_t offset) {
  size_t remaining = buffer->size - buffer->offset - offset;
  // May overflow.
  if (remaining > buffer->size) {
    remaining = 0;
  }
  return remaining;
}

void* io_buffer_virt(const io_buffer_t* buffer) {
  return (void*)(((uintptr_t)buffer->virt) + buffer->offset);
}

zx_paddr_t io_buffer_phys(const io_buffer_t* buffer) {
  ZX_DEBUG_ASSERT(buffer->phys != IO_BUFFER_INVALID_PHYS);
  return buffer->phys + buffer->offset;
}

zx_status_t io_buffer_cache_flush(io_buffer_t* buffer, zx_off_t offset, size_t length) {
  if (offset + length < offset || offset + length > buffer->size) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  return zx_cache_flush((void*)((uint8_t*)io_buffer_virt(buffer) + offset), length,
                        ZX_CACHE_FLUSH_DATA);
}

void io_buffer_release(io_buffer_t* buffer) {
  if (buffer->vmo_handle != ZX_HANDLE_INVALID) {
    if (buffer->pmt_handle != ZX_HANDLE_INVALID) {
      zx_status_t status = zx_pmt_unpin(buffer->pmt_handle);
      ZX_DEBUG_ASSERT(status == ZX_OK);
      buffer->pmt_handle = ZX_HANDLE_INVALID;
    }

    zx_vmar_unmap(zx_vmar_root_self(), (uintptr_t)buffer->virt, buffer->size);
    zx_handle_close(buffer->vmo_handle);
    buffer->vmo_handle = ZX_HANDLE_INVALID;
  }
  if (buffer->phys_list && buffer->pmt_handle != ZX_HANDLE_INVALID) {
    zx_status_t status = zx_pmt_unpin(buffer->pmt_handle);
    ZX_DEBUG_ASSERT(status == ZX_OK);
    buffer->pmt_handle = ZX_HANDLE_INVALID;
  }
  free(buffer->phys_list);
  buffer->phys_list = NULL;
  buffer->phys = 0;
  buffer->phys_count = 0;
}
