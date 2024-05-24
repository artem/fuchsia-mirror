// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "legacy-boot-shim.h"

#ifdef __i386__
#define ARCH "32"
#else
#define ARCH "64"
#endif

// Declared in legacy-boot-shim.h
const char* kLegacyShimName = "linux-x86-boot-shim[" BI ARCH "]";
