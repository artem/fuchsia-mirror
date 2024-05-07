// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "amlogic-canvas-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>

namespace amlogic_canvas_dt {

zx::result<> AmlogicCanvasVisitor::AddChildNodeSpec(fdf_devicetree::Node& child) {
  std::vector bind_rules = {
      fdf::MakeAcceptBindRule(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                              bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT)};

  std::vector bind_properties = {
      fdf::MakeProperty(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                        bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT)};

  auto amlogic_canvas_node = fuchsia_driver_framework::ParentSpec{{bind_rules, bind_properties}};

  child.AddNodeSpec(amlogic_canvas_node);
  FDF_LOG(DEBUG, "Added amlogic canvas node spec of to '%s'.", child.name().c_str());

  return zx::ok();
}

zx::result<> AmlogicCanvasVisitor::Visit(fdf_devicetree::Node& node,
                                         const devicetree::PropertyDecoder& decoder) {
  if (node.properties().find("amlogic,canvas") != node.properties().end()) {
    return AddChildNodeSpec(node);
  }
  return zx::ok();
}

}  // namespace amlogic_canvas_dt

REGISTER_DEVICETREE_VISITOR(amlogic_canvas_dt::AmlogicCanvasVisitor);
