
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_DEFAULT_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_DEFAULT_H_

#include <lib/driver/devicetree/visitors/multivisitor.h>

#include "bind-property.h"
#include "bti.h"
#include "mmio.h"

namespace fdf_devicetree {

// Set of visitors to parse basic devicetree properties like bind property,
// MMIO register properties etc., of each node and publish the properties to
// |fdf_devicetree::Node|. This can be extended to include driver specific
// visitors.
//     Example:
//           DefaultVisitors<MyDriverVisitor> visitors;
//           devicetree_manager.Walk(visitors);
template <typename... AdditionalVisitors>
using DefaultVisitors =
    MultiVisitor<BindPropertyVisitor, MmioVisitor, BtiVisitor, AdditionalVisitors...>;

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_DEFAULT_H_
