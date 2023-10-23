// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/machine.h>
#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>
#include <lib/stdcompat/bit.h>

#include "ensure-test-thread-pointer.h"

// The LE access model is the default for things defined within the TU under
// -fPIE, so these attributes should be superfluous.  But since the code below
// is explicitly testing LE access, make doubly sure.
[[gnu::tls_model("local-exec")]] alignas(64) thread_local int tls_data = 23;
[[gnu::tls_model("local-exec")]] thread_local int tls_bss;

using Traits = elfldltl::TlsTraits<>;

constexpr size_t kExpectedAlign = 64;

constexpr size_t kAlignedExecOffset =
    Traits::kTlsLocalExecOffset == 0
        ? 0
        : (Traits::kTlsLocalExecOffset + kExpectedAlign - 1) & -kExpectedAlign;

constexpr size_t kExpectedOffset = Traits::kTlsNegative ? -kExpectedAlign : kAlignedExecOffset;

constexpr size_t kExpectedSize =
    Traits::kTlsNegative ? kExpectedAlign : kAlignedExecOffset + sizeof(int) * 2;

// Since `tls_data` is initialized data and `tls_bss` is zero (bss), we know
// that `tls_data` will be first in the PT_TLS layout, and the checks above
// verified that it's no bigger than we expect to hold just those two so we can
// expect that `tls_data` is at the start and `tls_bss` immediately follows it.
constexpr ptrdiff_t kTpOffsetForData = cpp20::bit_cast<ptrdiff_t>(kExpectedOffset);
constexpr ptrdiff_t kTpOffsetForBss =
    cpp20::bit_cast<ptrdiff_t>(kExpectedOffset + sizeof(tls_data));

extern "C" int64_t TestStart() {
  const auto modules = ld::AbiLoadedModules(ld::abi::_ld_abi);

  const auto& exec_module = *modules.begin();

  if (exec_module.tls_modid != 1) {
    return 1;
  }

  if (ld::abi::_ld_abi.static_tls_modules.size() != 1) {
    return 2;
  }

  const auto& exec_tls = ld::abi::_ld_abi.static_tls_modules.front();

  if (exec_tls.tls_initial_data.size_bytes() != sizeof(tls_data)) {
    return 3;
  }

  if (*reinterpret_cast<const int*>(exec_tls.tls_initial_data.data()) != 23) {
    return 4;
  }

  if (exec_tls.tls_bss_size != sizeof(tls_bss)) {
    return 5;
  }

  if (exec_tls.tls_alignment != kExpectedAlign) {
    return 6;
  }

  if (ld::abi::_ld_abi.static_tls_offsets.size() != 1) {
    return 7;
  }

  if (ld::abi::_ld_abi.static_tls_offsets.front() != kExpectedOffset) {
    return 8;
  }

  if (ld::abi::_ld_abi.static_tls_layout.alignment() != kExpectedAlign) {
    return 9;
  }

  if (ld::abi::_ld_abi.static_tls_layout.size_bytes() != kExpectedSize) {
    return 10;
  }

  if (EnsureTestThreadPointer()) {
    // The compiler will emit LE accesses here and the linker will resolve the
    // offsets statically.  Verify that our runtime calculations match its
    // link-time calculations.

    if (ld::TpRelativeToOffset(&tls_data) != kTpOffsetForData) {
      return 11;
    }

    if (ld::TpRelativeToOffset(&tls_bss) != kTpOffsetForBss) {
      return 12;
    }
  }

  return 17;
}
