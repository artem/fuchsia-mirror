// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/vmar.h>

#include "module.h"

namespace dl {

ModuleHandle::~ModuleHandle() {
  if (vaddr_size() > 0) {
    zx::vmar::root_self()->unmap(abi_module_.vaddr_start, vaddr_size());
  }
}

}  // namespace dl
