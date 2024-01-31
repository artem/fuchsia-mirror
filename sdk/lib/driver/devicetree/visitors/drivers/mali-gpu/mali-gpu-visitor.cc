// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mali-gpu-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <vector>

#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpu/mali/cpp/bind.h>

namespace mali_gpu_dt {

zx::result<> MaliGpuVisitor::AddChildNodeSpec(fdf_devicetree::Node& child) {
  std::vector bind_rules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpu_mali::SERVICE,
                              bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

  std::vector bind_properties = {
      fdf::MakeProperty(bind_fuchsia_hardware_gpu_mali::SERVICE,
                        bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

  auto mali_gpu_node = fuchsia_driver_framework::ParentSpec{{bind_rules, bind_properties}};

  child.AddNodeSpec(mali_gpu_node);
  return zx::ok();
}

zx::result<> MaliGpuVisitor::Visit(fdf_devicetree::Node& node,
                                   const devicetree::PropertyDecoder& decoder) {
  if (node.properties().find("mali-gpu-parent") != node.properties().end()) {
    return AddChildNodeSpec(node);
  }
  return zx::ok();
}

}  // namespace mali_gpu_dt

REGISTER_DEVICETREE_VISITOR(mali_gpu_dt::MaliGpuVisitor);
