// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/magma/platform/platform_trace_provider.h>

namespace magma {

std::unique_ptr<PlatformTraceProvider> PlatformTraceProvider::CreateForTesting() { return nullptr; }

PlatformTraceProvider* PlatformTraceProvider::Get() { return nullptr; }

}  // namespace magma
