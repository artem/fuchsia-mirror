// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_LOAD_VISITORS_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_LOAD_VISITORS_H_

#include <lib/driver/devicetree/visitors/registry.h>
#include <lib/driver/incoming/cpp/namespace.h>

#include <memory>

namespace fdf_devicetree {

// Find all devicetree visitor shared libraries under the incoming namespace's
// `/pkg/lib/visitors` directory. Instantiate all visitor objects and return a
// registry of visitors which includes the default devicetree visitors.
zx::result<std::unique_ptr<VisitorRegistry>> LoadVisitors(fdf::Namespace& incoming);

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_LOAD_VISITORS_H_
