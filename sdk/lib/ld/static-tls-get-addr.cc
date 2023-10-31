// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/tls.h>
#include <zircon/compiler.h>

namespace ld::abi {

__EXPORT void* __tls_get_addr(const elfldltl::Elf<>::TlsGetAddrGot& got) {
  if (got.tls_modid == 0) [[unlikely]] {
    // Note that glibc doesn't handle this case properly at all, and is liable
    // to crash if it gets called.  It's clearly only intended for undefined
    // weak TLS symbols to be allowed if they aren't actually referenced in
    // live code paths at runtime.
    return nullptr;
  }
  // TODO(mcgrathr): the libc version would also need a hook for libdl here,
  // when modid >= _ld_abi.static_tls_offsets.size(). But it might just want to
  // be replaced with a version that always uses the DTV.  This startup-only
  // implementation has the advantage of not needing a DTV.
  return TpRelative(TlsInitialExecOffset(_ld_abi, got.tls_modid) +
                    static_cast<ptrdiff_t>(got.offset));
}

}  // namespace ld::abi
