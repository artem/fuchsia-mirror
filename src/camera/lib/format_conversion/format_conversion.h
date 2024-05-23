// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_LIB_FORMAT_CONVERSION_FORMAT_CONVERSION_H_
#define SRC_CAMERA_LIB_FORMAT_CONVERSION_FORMAT_CONVERSION_H_

#ifndef _LIBCPP_ENABLE_HARDENED_MODE
#define _LIBCPP_ENABLE_HARDENED_MODE 1
#endif

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/sysmem/cpp/fidl.h>

namespace camera {

fuchsia_sysmem::wire::ImageFormat2 ConvertToWireType(fuchsia::sysmem::ImageFormat_2 image_format);

fuchsia_sysmem::wire::ImageFormat2 ConvertV2ToV1WireType(
    const fuchsia::images2::ImageFormat& image_format);

fuchsia_sysmem::wire::ImageFormatConstraints ConvertToWireType(
    fuchsia::sysmem::ImageFormatConstraints constraints);

fuchsia_sysmem::wire::ImageFormat2 GetImageFormatFromConstraints(
    fuchsia_sysmem::wire::ImageFormatConstraints constraints, uint32_t coded_width,
    uint32_t coded_height);

fuchsia_sysmem::wire::ImageFormatConstraints ConvertV2ToV1WireType(
    const fuchsia::sysmem2::ImageFormatConstraints& constraints);

}  // namespace camera

#endif  // SRC_CAMERA_LIB_FORMAT_CONVERSION_FORMAT_CONVERSION_H_
