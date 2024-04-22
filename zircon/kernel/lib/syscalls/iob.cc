// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <ktl/move.h>
#include <object/handle.h>
#include <object/io_buffer_dispatcher.h>
#include <object/process_dispatcher.h>

// zx_status_t zx_iob_create
zx_status_t sys_iob_create(uint64_t options, user_in_ptr<const void> regions, uint64_t num_regions,
                           zx_handle_t* ep0_out, zx_handle_t* ep1_out) {
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto up = ProcessDispatcher::GetCurrent();
  zx_status_t res = up->EnforceBasicPolicy(ZX_POL_NEW_IOB);
  if (res != ZX_OK) {
    return res;
  }

  if (num_regions > ZX_IOB_MAX_REGIONS) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AllocChecker ac;
  IoBufferDispatcher::RegionArray copied_regions{&ac, num_regions};

  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  res = regions.reinterpret<const zx_iob_region_t>().copy_array_from_user(copied_regions.get(),
                                                                          num_regions);
  if (res != ZX_OK) {
    return res;
  }

  KernelHandle<IoBufferDispatcher> handle0, handle1;
  zx_rights_t rights;
  zx_status_t result =
      IoBufferDispatcher::Create(options, copied_regions, &handle0, &handle1, &rights);
  if (result != ZX_OK) {
    return result;
  }

  result = up->MakeAndAddHandle(ktl::move(handle0), rights, ep0_out);
  if (result == ZX_OK) {
    result = up->MakeAndAddHandle(ktl::move(handle1), rights, ep1_out);
  }
  return result;
}

// zx_status_t zx_iob_allocate_id
zx_status_t sys_iob_allocate_id(zx_handle_t handle, zx_iob_allocate_id_options_t options,
                                uint32_t region_index, user_in_ptr<const void> blob_ptr,
                                uint64_t blob_size, user_out_ptr<uint32_t> id) {
  // No options are supported at this time.
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto* up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<IoBufferDispatcher> iob;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &iob);
  if (status != ZX_OK) {
    return status;
  }

  zx::result<uint32_t> result =
      iob->AllocateId(region_index, blob_ptr.reinterpret<const std::byte>(), blob_size);
  if (result.is_error()) {
    return result.status_value();
  }
  return id.copy_to_user(result.value());
}
