// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef GPU_MAPPING_H
#define GPU_MAPPING_H

#include <lib/magma_service/util/gpu_mapping.h>

#include <memory>

#include "msd_vsi_buffer.h"

using GpuMappingView = magma::GpuMappingView<MsdVsiBuffer>;
using GpuMapping = magma::GpuMapping<MsdVsiBuffer>;

#endif  // GPU_MAPPING_H
