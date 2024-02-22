// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/standalone-test/standalone.h>
#include <lib/zx/resource.h>

// Redeclare the standalone-test function as weak here.
[[gnu::weak]] decltype(standalone::GetRootResource) standalone::GetRootResource;
[[gnu::weak]] decltype(standalone::GetMmioResource) standalone::GetMmioResource;

namespace maybe_standalone {

zx::unowned_resource GetRootResource() {
  zx::unowned_resource root_resource;
  if (standalone::GetRootResource) {
    root_resource = standalone::GetRootResource();
  }
  return root_resource;
}

zx::unowned_resource GetMmioResource() {
  zx::unowned_resource mmio_resource;
  if (standalone::GetMmioResource) {
    mmio_resource = standalone::GetMmioResource();
  }
  return mmio_resource;
}

}  // namespace maybe_standalone
