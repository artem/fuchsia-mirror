// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_ZIRCON_TESTING_MAYBE_STANDALONE_TEST_INCLUDE_LIB_MAYBE_STANDALONE_TEST_MAYBE_STANDALONE_H_
#define SRC_ZIRCON_TESTING_MAYBE_STANDALONE_TEST_INCLUDE_LIB_MAYBE_STANDALONE_TEST_MAYBE_STANDALONE_H_

#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <zircon/syscalls/resource.h>

#include <optional>

// Forward declaration for <lib/boot-options/boot-options.h>.
struct BootOptions;

namespace maybe_standalone {

// This returns the invalid handle if not built standalone.
zx::unowned_resource GetMmioResource();
zx::unowned_resource GetSystemResource();

// Creates and returns upon success a specific system resource given a |base|.
zx::result<zx::resource> GetSystemResourceWithBase(zx::unowned_resource& system_resource,
                                                   uint64_t base);

// This returns nullptr if not built standalone.
const BootOptions* GetBootOptions();

}  // namespace maybe_standalone

#endif  // SRC_ZIRCON_TESTING_MAYBE_STANDALONE_TEST_INCLUDE_LIB_MAYBE_STANDALONE_TEST_MAYBE_STANDALONE_H_
