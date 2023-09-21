// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef RINGBUFFER_H
#define RINGBUFFER_H

#include <lib/magma_service/util/ringbuffer.h>

#include "gpu_mapping.h"

using Ringbuffer = magma::Ringbuffer<GpuMapping>;

#endif  // RINGBUFFER_H
