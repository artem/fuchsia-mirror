// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/compiler.h>

#include <array>

#include "test-data.h"

const int rodata = 5;

extern "C" __CONSTINIT const RelroData relro_data{.relocated = &rodata};
